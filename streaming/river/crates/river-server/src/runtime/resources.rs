//! Container-scoped budgets. Cache ceilings are not RSS or a promise about native allocations.
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const M: usize = 1024 * 1024;
pub const RAM: [(&str, &str, usize); 9] = [
    ("ranges", "RIVER_S3_RANGE_CACHE_BYTES", 27),
    ("headers", "RIVER_S3_HEADER_CACHE_BYTES", 6),
    ("indexes", "RIVER_S3_INDEX_MEMORY_BYTES", 9),
    ("parsed_headers", "RIVER_S3_PARSED_HEADER_CACHE_BYTES", 6),
    ("packet_plans", "RIVER_S3_PACKET_PLAN_CACHE_BYTES", 9),
    (
        "prepared_prefixes",
        "RIVER_S3_PREPARED_PREFIX_CACHE_BYTES",
        5,
    ),
    ("native_tiles", "RIVER_NATIVE_TILE_CACHE_BYTES", 14),
    ("raster_results", "RIVER_RASTER_CACHE_BYTES", 14),
    ("asset_metadata", "RIVER_ASSET_METADATA_CACHE_BYTES", 10),
];
#[derive(Clone, Debug, Serialize)]
pub struct Detected {
    pub cpu_affinity: usize,
    pub cpu_quota: Option<f64>,
    pub effective_cpus: usize,
    pub memory_limit_bytes: usize,
    pub memory_current_path: Option<PathBuf>,
    pub memory_stat_path: Option<PathBuf>,
    // Every finite ancestor has its own usage and competes for its own headroom.
    memory_scopes: Vec<(PathBuf, usize)>,
    pub reasons: Vec<String>,
}
fn read(root: &Path, p: &str) -> Option<String> {
    fs::read_to_string(root.join(p.trim_start_matches('/'))).ok()
}
fn count_cpus(s: &str) -> Option<usize> {
    let mut n = 0usize;
    for part in s.trim().split(',') {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let (a, b) = (a.parse::<usize>().ok()?, b.parse::<usize>().ok()?);
        if b < a {
            return None;
        }
        n = n.checked_add(b - a + 1)?;
    }
    Some(n.max(1))
}
fn number(root: &Path, p: &Path) -> Option<usize> {
    fs::read_to_string(root.join(p.strip_prefix("/").unwrap_or(p)))
        .ok()?
        .trim()
        .parse()
        .ok()
}
fn detect(root: &Path) -> Detected {
    let mut reasons = vec![];
    let affinity = read(root, "/proc/self/status")
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:").and_then(count_cpus))
        })
        .unwrap_or_else(|| {
            reasons.push("CPU affinity unavailable: available_parallelism fallback".into());
            std::thread::available_parallelism().map_or(1, usize::from)
        });
    let physical = read(root, "/proc/meminfo")
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<usize>()
                    .ok()?
                    .checked_mul(1024)
            })
        })
        .unwrap_or_else(|| {
            reasons.push("Physical memory unavailable: conservative 2GiB fallback".into());
            2 * 1024 * M
        });
    let mut dirs = vec![
        PathBuf::from("/sys/fs/cgroup"),
        PathBuf::from("/sys/fs/cgroup/memory"),
        PathBuf::from("/sys/fs/cgroup/cpu"),
        PathBuf::from("/sys/fs/cgroup/cpu,cpuacct"),
    ];
    // Resolve namespace-relative v1/v2 mount roots, then inspect ancestors too.
    if let (Some(groups), Some(mounts)) = (
        read(root, "/proc/self/cgroup"),
        read(root, "/proc/self/mountinfo"),
    ) {
        for line in mounts.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let l = left.split_whitespace().collect::<Vec<_>>();
            let r = right.split_whitespace().collect::<Vec<_>>();
            if l.len() < 5 || r.len() < 3 || !matches!(r[0], "cgroup" | "cgroup2") {
                continue;
            }
            for group in groups.lines() {
                let parts = group.splitn(3, ':').collect::<Vec<_>>();
                if parts.len() != 3 {
                    continue;
                }
                if r[0] == "cgroup" && !parts[1].split(',').any(|c| r[2].split(',').any(|x| x == c))
                {
                    continue;
                }
                let mount = PathBuf::from(l[4]);
                let relative = Path::new(parts[2])
                    .strip_prefix(l[3])
                    .unwrap_or(Path::new(parts[2]));
                let mut path = mount.join(relative.strip_prefix("/").unwrap_or(relative));
                for _ in 0..128 {
                    if !path.starts_with(&mount) {
                        break;
                    }
                    dirs.push(path.clone());
                    if path == mount || !path.pop() {
                        break;
                    }
                }
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    let mut quota: Option<f64> = None;
    let mut memory = physical;
    let mut memory_scopes = Vec::new();
    let mut current = None;
    let mut stat = None;
    for dir in dirs {
        let pair = read(root, &dir.join("cpu.max").to_string_lossy())
            .and_then(|s| {
                let v = s.split_whitespace().collect::<Vec<_>>();
                if v.len() != 2 {
                    return None;
                }
                Some((v[0].parse::<f64>().ok()?, v[1].parse::<f64>().ok()?))
            })
            .or_else(|| {
                Some((
                    read(root, &dir.join("cpu.cfs_quota_us").to_string_lossy())?
                        .trim()
                        .parse::<f64>()
                        .ok()?,
                    read(root, &dir.join("cpu.cfs_period_us").to_string_lossy())?
                        .trim()
                        .parse::<f64>()
                        .ok()?,
                ))
            });
        if let Some((q, p)) = pair {
            if q > 0.0 && p > 0.0 {
                let q = q / p;
                quota = Some(quota.map_or(q, |old| old.min(q)));
            }
        }
        for file in ["memory.max", "memory.limit_in_bytes"] {
            if let Some(n) = number(root, &dir.join(file)) {
                if n > 0 && n <= physical {
                    let usage = dir.join(if file == "memory.max" { "memory.current" } else { "memory.usage_in_bytes" });
                    if number(root, &usage).is_some() {
                        memory_scopes.push((root.join(usage.strip_prefix("/").unwrap_or(&usage)), n));
                    }
                }
                if n > 0 && n < memory {
                    memory = n;
                    reasons.push(format!("memory capped by {}", dir.join(file).display()));
                }
            }
        }
        // Prefer the process cgroup's most specific usage counter, not its parent.
        for (usage, stats) in [
            ("memory.current", "memory.stat"),
            ("memory.usage_in_bytes", "memory.stat"),
        ] {
            let path = dir.join(usage);
            if number(root, &path).is_some()
                && current
                    .as_ref()
                    .is_none_or(|p: &PathBuf| p.components().count() < root.join(path.strip_prefix("/").unwrap_or(&path)).components().count())
            {
                current = Some(root.join(path.strip_prefix("/").unwrap_or(&path)));
                stat = Some(
                    root.join(
                        dir.join(stats)
                            .strip_prefix("/")
                            .unwrap_or(&dir.join(stats)),
                    ),
                );
            }
        }
    }
    let cpus = quota
        .map_or(affinity, |q| affinity.min(q.floor().max(1.0) as usize))
        .max(1);
    reasons.push(format!(
        "CPU workers use min(affinity={affinity}, floor(quota={quota:?})); minimum one"
    ));
    Detected {
        cpu_affinity: affinity,
        cpu_quota: quota,
        effective_cpus: cpus,
        memory_limit_bytes: memory,
        memory_scopes,
        memory_current_path: current,
        memory_stat_path: stat,
        reasons,
    }
}
#[derive(Clone, Serialize)]
pub struct Allocation {
    pub configured_bytes: usize,
    pub effective_target_bytes: usize,
    pub explicit: bool,
    pub reason: String,
}
#[derive(Clone, Serialize)]
pub struct Plan {
    pub detected: Detected,
    pub memory_allowance_bytes: usize,
    pub runtime_reserve_bytes: usize,
    pub decoder_reserve_bytes: usize,
    pub request_reserve_bytes: usize,
    pub temporary_reserve_bytes: usize,
    pub shared_cache_bytes: usize,
    pub explicit_cache_bytes: usize,
    pub overcommitted_explicit_bytes: usize,
    pub decode_concurrency: usize,
    pub source_io_concurrency: usize,
    pub caches: BTreeMap<String, Allocation>,
    pub decisions: Vec<String>,
}
impl Plan {
    fn build(d: Detected, get: impl Fn(&str) -> Option<String>) -> Self {
        let n = |key: &str| {
            get(key)
                .filter(|v| !v.trim().is_empty())
                .and_then(|v| v.parse::<usize>().ok())
        };
        let mem = n("RIVER_MEMORY_BUDGET_BYTES")
            .unwrap_or(d.memory_limit_bytes)
            .min(d.memory_limit_bytes);
        let compact = get("RIVER_PROFILE").is_some_and(|v| v == "compact");
        let cpus = d.effective_cpus;
        let decode = n("RIVER_DECODE_CONCURRENCY")
            .filter(|v| *v > 0)
            .unwrap_or(if compact {
                1
            } else {
                (cpus / 2).max(1).min(32).min((mem / (512 * M)).max(1))
            });
        let io = n("RIVER_SOURCE_IO_CONCURRENCY")
            .filter(|v| *v > 0)
            .unwrap_or(if compact {
                1
            } else {
                cpus.min(16).min((mem / (256 * M)).max(1))
            })
            .clamp(1, 16);
        // Retention policy must not shrink the allowance for native and runtime allocations.
        let runtime = (mem / 8).max(128 * M).min(mem / 2);
        let decoder = decode.saturating_mul(
            n("RIVER_DECODE_WORKSPACE_BYTES").unwrap_or((mem / 8).clamp(64 * M, 256 * M)),
        );
        // Ordinary selective reads start at 64 MiB. On large pods reserve up to
        // 128 MiB per lane for complete parts of compatibility-sized objects.
        let input_per_lane = (mem / io / 8).clamp(64 * M, 128 * M);
        let requests = io.saturating_mul(n("RIVER_S3_REQUEST_BYTES").unwrap_or(input_per_lane));
        let temporary = (mem / 16).max(32 * M);
        let remaining = mem.saturating_sub(
            runtime
                .saturating_add(decoder)
                .saturating_add(requests)
                .saturating_add(temporary).saturating_add(crate::storage_mode::scratch_limit(mem)).saturating_add(crate::storage_mode::locator_limit(mem)),
        );
        let explicit = RAM
            .iter()
            .map(|(_, env, _)| n(env).unwrap_or(0))
            .fold(0usize, usize::saturating_add);
        let shared = n("RIVER_CACHE_MEMORY_BYTES")
            .unwrap_or(if compact {
                (remaining.saturating_mul(7) / 10).min(64 * M)
            } else if crate::adaptation::startup_liberal_ram() {
                remaining.min(mem.saturating_mul(7) / 8)
            } else {
                remaining.saturating_mul(7) / 10
            })
            .min(remaining);
        let available = shared.saturating_sub(explicit);
        let weight: usize = RAM
            .iter()
            .filter(|(_, env, _)| n(env).is_none())
            .map(|(_, _, w)| w)
            .sum();
        let mut caches = BTreeMap::new();
        for (name, env, w) in RAM {
            let value = n(env);
            let bytes = value.unwrap_or_else(|| available.saturating_mul(w) / weight.max(1));
            caches.insert(name.into(),Allocation{configured_bytes:bytes,effective_target_bytes:if value.is_some()&&explicit>shared{bytes.saturating_mul(shared)/explicit.max(1)}else{bytes},explicit:value.is_some(),reason:if value.is_some(){format!("explicit {env} ceiling preserved; target may be smaller to fit the shared memory envelope or reclaim pressure")}else{format!("{w}/{weight} of remaining coordinated RAM after explicit cache ceilings and headroom")}});
        }
        for (name, env, default) in [
            (
                "index_disk",
                "RIVER_S3_INDEX_CACHE_BYTES",
                mem.clamp(256 * M, 8 * 1024 * M) / 5,
            ),
            (
                "raster_disk",
                "RIVER_RASTER_DISK_CACHE_BYTES",
                mem.clamp(256 * M, 8 * 1024 * M) * 3 / 10,
            ),
            (
                "asset_locator_disk",
                "RIVER_ASSET_LOCATOR_CACHE_BYTES",
                (mem.clamp(256 * M, 8 * 1024 * M) / 40).min(64 * M),
            ),
        ] {
            let v = n(env);
            caches.insert(name.into(),Allocation{configured_bytes:v.unwrap_or(default),effective_target_bytes:v.unwrap_or(default),explicit:v.is_some(),reason:if v.is_some(){format!("explicit {env}; persistent bytes outside RAM allowance")}else{"separate persistent-disk ceiling, derived conservatively from instance size; not reserved RAM".into()}});
        }
        Self{detected:d,memory_allowance_bytes:mem,runtime_reserve_bytes:runtime,decoder_reserve_bytes:decoder,request_reserve_bytes:requests,temporary_reserve_bytes:temporary,shared_cache_bytes:shared,explicit_cache_bytes:explicit,overcommitted_explicit_bytes:explicit.saturating_sub(shared),decode_concurrency:decode,source_io_concurrency:io,caches,decisions:vec!["Initial cache shares use fixed weights; bounded workload learning can transfer unused automatic shares to repeatedly reused caches under eviction pressure".into(),format!("Automatic decode slots=min(32, max(1, floor(CPU/2)), max(1, RAM/512MiB)); effective slots={decode}; explicit environment/profile values take precedence"),format!("Automatic source IO=min(16, CPU, max(1, RAM/256MiB)); effective IO={io}; persisted per-source limits and learning stay independent"),"More memory increases retention; CPU quotas and source-specific admission remain independent limits".into(),"Cache occupancy counts retained entries; process RSS and active decode reservations are reported separately".into()]}
    }
}
pub fn launcher_env() -> serde_json::Value {
    let p = plan();
    let workers = p.decode_concurrency.min(8);
    let per_worker = (p.detected.effective_cpus / workers.max(1)).clamp(1, 16);
    let mut values = serde_json::json!({"RIVER_DECODE_CONCURRENCY":p.decode_concurrency.to_string(),"RIVER_SOURCE_IO_CONCURRENCY":p.source_io_concurrency.to_string(),"RIVER_NVJPEG2000_WORKERS":p.decode_concurrency.min(32).to_string(),"RIVER_NVJPEG2000_NATIVE_WORKERS":p.detected.effective_cpus.min(32).to_string(),"RIVER_GROK_NATIVE_THREADS":p.detected.effective_cpus.min(64).to_string(),"RIVER_GROK_THREADS_PER_DECODE":(p.detected.effective_cpus/p.decode_concurrency.max(1)).clamp(1,16).to_string(),"RIVER_OPENHT_WORKERS":workers.to_string(),"RIVER_OPENHT_WORKER_THREADS":per_worker.to_string(),"RIVER_OPENHT_WORKER_MEMORY_MIB":(p.decoder_reserve_bytes/workers.max(1)/M).clamp(256,4096).to_string()});
    let auto = values
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| {
            std::env::var(k)
                .ok()
                .is_none_or(|v| (v.trim().is_empty() || v.trim().eq_ignore_ascii_case("auto")))
        })
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    values["RIVER_RESOURCE_AUTOMATIC_KEYS"] = serde_json::json!(auto);
    values
}
pub fn plan() -> &'static Plan {
    static PLAN: OnceLock<Plan> = OnceLock::new();
    PLAN.get_or_init(|| Plan::build(detect(Path::new("/")), |k| std::env::var(k).ok()))
}
pub fn budget(name: &str) -> usize {
    if name.ends_with("_disk") && (!crate::storage_mode::disk_cache_enabled() || crate::adaptation::conserve_disk()) { return 0; }

    plan().caches[name].effective_target_bytes
}
static SCALE: AtomicUsize = AtomicUsize::new(1000);
pub fn effective(name: &str, ceiling: usize) -> usize {
    let ceiling = if plan().caches.get(name).is_some_and(|a| !a.explicit && a.effective_target_bytes==ceiling) {
        crate::cache_learning::target(name,ceiling)
    } else {ceiling};
    if name.ends_with("_disk") {
        if crate::adaptation::conserve_disk() || !crate::storage_mode::disk_cache_enabled() { 0 } else { ceiling }
    } else {
        let pressure = SCALE.load(Ordering::Relaxed);
        if !crate::adaptation::liberal_ram() && crate::adaptation::ram_scale() == 1000 {
            return ceiling.saturating_mul(pressure) / 1000;
        }
        let explicit = plan().caches.get(name).is_some_and(|value| value.explicit)
            || crate::adaptation::explicit("RIVER_CACHE_MEMORY_BYTES");
        let target = if explicit { ceiling } else if crate::adaptation::liberal_ram()
            && !crate::adaptation::startup_liberal_ram()
            && std::env::var("RIVER_PROFILE").as_deref() != Ok("compact")
        {
            ceiling.saturating_mul(10) / 7
        } else { ceiling.saturating_mul(crate::adaptation::ram_scale()) / 1000 };
        target.saturating_mul(pressure) / 1000
    }
}
pub fn entry_limit(env: &str, bytes: usize) -> usize {
    std::env::var(env)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or_else(|| (bytes / 256).max(64))
}
#[derive(Clone, Default, Serialize)]
pub struct Occupancy {
    pub bytes: usize,
    pub entries: usize,
    pub externally_held_bytes: usize,
    pub persistence_location: Option<String>,
}
#[derive(Default)]
struct Counts {
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    avoided_bytes: AtomicU64,
    estimated_avoided_ns: AtomicU64,
    measured_reuse_samples: AtomicU64,
    shared_evicted_bytes: AtomicU64,
}
struct Cache {
    counts: Arc<Counts>,
    sample: Arc<dyn Fn(usize) -> Occupancy + Send + Sync>,
}
fn caches() -> &'static Mutex<HashMap<&'static str, Cache>> {
    static C: OnceLock<Mutex<HashMap<&'static str, Cache>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}
fn counts() -> &'static HashMap<&'static str, Arc<Counts>> {
    static C: OnceLock<HashMap<&'static str, Arc<Counts>>> = OnceLock::new();
    C.get_or_init(|| {
        RAM.iter()
            .map(|(n, _, _)| (*n, Arc::new(Counts::default())))
            .chain([
                ("index_disk", Arc::new(Counts::default())),
                ("raster_disk", Arc::new(Counts::default())),
                ("asset_locator_disk", Arc::new(Counts::default())),
            ])
            .collect()
    })
}
pub fn register(name: &'static str, sample: impl Fn(usize) -> Occupancy + Send + Sync + 'static) {
    caches().lock().unwrap_or_else(|e| e.into_inner()).insert(
        name,
        Cache {
            counts: counts()[name].clone(),
            sample: Arc::new(sample),
        },
    );
}
pub fn hit(name: &str, bytes: usize) {
    let c = &counts()[name];
    c.hits.fetch_add(1, Ordering::Relaxed);
    c.avoided_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
}
pub fn miss(name: &str) {
    counts()[name].misses.fetch_add(1, Ordering::Relaxed);
}
/// Sum of prior measured rebuild costs reused, not measured end-to-end speedup.
pub fn reused_cost(name: &str, nanoseconds: u64) {
    if nanoseconds==0{return;}
    counts()[name].estimated_avoided_ns.fetch_add(nanoseconds, Ordering::Relaxed);
    counts()[name].measured_reuse_samples.fetch_add(1,Ordering::Relaxed);
}
pub fn evict(name: &str, n: usize) {
    counts()[name]
        .evictions
        .fetch_add(n as u64, Ordering::Relaxed);
}
pub fn retain_encoded(name: &str, bytes: usize, data: &axum::body::Bytes) {
    if !data.is_unique() {
        counts()[name]
            .shared_evicted_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }
}
// Weak<[u8]> would keep the inline allocation alive after the last reader
// dropped its Arc. Count shared evictions without extending their lifetime.
pub fn range_evicted(data: &Arc<[u8]>) {
    if Arc::strong_count(data) > 1 {
        counts()["ranges"].shared_evicted_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
    }
}

struct Retained {
    bytes: usize,
    live: Box<dyn Fn() -> bool + Send + Sync>,
}
fn retained() -> &'static Mutex<HashMap<(&'static str, usize), Retained>> {
    static R: OnceLock<Mutex<HashMap<(&'static str, usize), Retained>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn retain<T: Send + Sync + 'static>(name: &'static str, bytes: usize, data: &Arc<T>) {
    if Arc::strong_count(data) > 1 {
        let key = (name, Arc::as_ptr(data) as *const () as usize);
        let weak = Arc::downgrade(data);
        retained().lock().unwrap_or_else(|e| e.into_inner()).insert(
            key,
            Retained {
                bytes,
                live: Box::new(move || weak.strong_count() > 0),
            },
        );
    }
}
fn retained_bytes() -> BTreeMap<&'static str, usize> {
    let mut r = retained().lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|_, v| (v.live)());
    let mut totals = BTreeMap::new();
    for ((name, _), v) in r.iter() {
        *totals.entry(*name).or_insert(0usize) += v.bytes;
    }
    totals
}
fn samples() -> Vec<(
    &'static str,
    Arc<Counts>,
    Arc<dyn Fn(usize) -> Occupancy + Send + Sync>,
)> {
    caches()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(&n, c)| (n, c.counts.clone(), c.sample.clone()))
        .collect()
}
pub(crate) fn pressure_target(bytes: usize) -> usize {
    bytes.saturating_mul(SCALE.load(Ordering::Relaxed)) / 1000
}
pub fn reclaim() {
    let _ = retained_bytes();
    if SCALE.load(Ordering::Relaxed)<1000 {
        crate::decoder::block_reuse::trim();
        crate::decoder::tier1_resume::trim();
    }
    for (name, _, sample) in samples() {
        sample(effective(name, budget(name)));
    }
}
#[derive(Default, Serialize)]
pub struct Pressure {
    pub scale_per_mille: usize,
    pub low_samples: usize,
    pub reason: String,
    pub adjustments: u64,
}
impl Pressure {
    fn admission_pressure(&mut self) {
        if self.scale_per_mille != 100 {
            self.adjustments += 1;
        }
        self.low_samples = 0;
        self.scale_per_mille = 100;
        self.reason = "memory pressure; hold reduced cache targets until recovery".into();
    }
    fn step(&mut self, used: usize, limit: usize) -> bool {
        let old = self.scale_per_mille.max(100);
        let new = if used >= limit.saturating_mul(92) / 100 {
            self.low_samples = 0;
            self.reason = "working set at admission ceiling; immediately reclaim optional caches".into();
            100
        } else if used >= limit.saturating_mul(85) / 100 {
            self.low_samples = 0;
            self.reason = "working set >=85%; reclaim at most25% per sample".into();
            old.saturating_mul(3) / 4
        } else if used < limit.saturating_mul(65) / 100 {
            self.low_samples += 1;
            if self.low_samples >= 5 {
                self.low_samples = 0;
                self.reason = "five low-pressure samples; grow by5% of original ceiling".into();
                (old + 50).min(1000)
            } else {
                old
            }
        } else {
            self.low_samples = 0;
            old
        }
        .clamp(100, 1000);
        self.scale_per_mille = new;
        if new != old {
            self.adjustments += 1;
            true
        } else {
            false
        }
    }
}
fn pressure() -> &'static Mutex<Pressure> {
    static P: OnceLock<Mutex<Pressure>> = OnceLock::new();
    P.get_or_init(|| {
        Mutex::new(Pressure {
            scale_per_mille: 1000,
            reason: "startup allocation".into(),
            ..Default::default()
        })
    })
}
fn rss() -> usize {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse::<usize>()
                    .ok()?
                    .checked_mul(1024)
            })
        })
        .unwrap_or(0)
}
pub fn working_set() -> (usize, usize) {
    let d = &plan().detected;
    let current = d
        .memory_current_path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok()?.trim().parse::<usize>().ok())
        .unwrap_or_else(rss);
    (current, current.saturating_sub(inactive_file(d.memory_stat_path.as_deref())))
}

fn inactive_file(path: Option<&Path>) -> usize {
    let Some(stats)=path.and_then(|p| fs::read_to_string(p).ok()) else {return 0};
    let mut local=0;
    for line in stats.lines() {
        let Some((key,value))=line.split_once(' ') else {continue};
        match key {
            "total_inactive_file" => return value.trim().parse().unwrap_or(local),
            "inactive_file" => local=value.trim().parse().unwrap_or(0),
            _ => (),
        }
    }
    local
}
// An explicit River allowance is not the entire parent cgroup's allowance.
// Charge other processes only when they consume the headroom outside River's share.
fn pressure_usage(group_working: usize, process_rss: usize, group_limit: usize, river_allowance: usize) -> usize {
    process_rss.max(group_working.saturating_sub(group_limit.saturating_sub(river_allowance)))
}
fn effective_pressure_usage(group_working: usize) -> usize {
    let plan=plan();
    let detected=&plan.detected;
    let base=pressure_usage(group_working,rss(),detected.memory_limit_bytes,plan.memory_allowance_bytes);
    ancestor_pressure(detected, base, plan.memory_allowance_bytes)
}

fn ancestor_pressure(detected: &Detected, mut used: usize, allowance: usize) -> usize {
    for (path,limit) in &detected.memory_scopes {
        if detected.memory_current_path.as_ref()==Some(path) && *limit==detected.memory_limit_bytes {continue}
        if let Some(current)=fs::read_to_string(path).ok().and_then(|s|s.trim().parse::<usize>().ok()) {
            let working=current.saturating_sub(inactive_file(Some(&path.with_file_name("memory.stat"))));
            used=pressure_usage(working,used,*limit,allowance);
        }
    }
    used
}
static ACTIVE_MEMORY: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_WORKSPACES: AtomicUsize = AtomicUsize::new(0);
fn memory_wait() -> &'static (Mutex<()>, std::sync::Condvar) {
    static W: OnceLock<(Mutex<()>, std::sync::Condvar)> = OnceLock::new();
    W.get_or_init(|| (Mutex::new(()), std::sync::Condvar::new()))
}
static MEMORY_WAITERS: AtomicUsize = AtomicUsize::new(0);
fn cache_purpose(name: &str) -> &'static str {
    match name {
        "ranges" => "Compressed source blocks; avoids fetching retained byte ranges",
        "headers" => "Raw main headers and marker tables; avoids source header reads",
        "indexes" => "Validated tile-part offsets; avoids rediscovering file positions",
        "parsed_headers" => "Parsed codestream structures; avoids repeated parsing",
        "packet_plans" => "Packet selection plans; avoids rebuilding byte selection",
        "prepared_prefixes" => "Reusable decoder input prefixes; avoids reconstruction",
        "native_tiles" => "Decoded pixel planes; avoids native decode work",
        "raster_results" => "Encoded output tiles; avoids preparation, decode and encoding",
        "asset_metadata" => "Demand-loaded image handles; preserves cross-request preparation state",
        _ => "Persistent disposable cache; separate from process RAM",
    }
}
fn benefit_measurement(name:&str,hits:u64,measured:u64)->serde_json::Value {
    let method=match name {
        "ranges"=>Some("actual fetch elapsed time apportioned by bytes to cached blocks"),
        "headers"=>Some("elapsed validated main-header rebuild cost"),
        "parsed_headers"=>Some("elapsed tile-header parse and required-read rebuild cost"),
        "packet_plans"=>Some("elapsed packet-geometry construction cost"),
        "prepared_prefixes"=>Some("elapsed reusable prefix reconstruction cost"),
        "native_tiles"=>Some("elapsed native-tile decode cost"),
        "raster_results"=>Some("attributed decode plus encoding cost per output tile"),
        _=>None,
    };
    serde_json::json!({"status":if method.is_none(){"unavailable"}else if measured==0{"not-yet-observed"}else if measured<hits{"partial"}else{"measured"},
        "method":method,"measuredReuseSamples":measured,"reuseSamples":hits,
        "coverage":if hits>0{Some((measured as f64/hits as f64).min(1.0))}else{None},
        "reason":if method.is_none(){"Incremental/shared metadata ownership does not yet provide a reliable per-entry counterfactual rebuild cost; no invented timing feeds allocation"}else{"Producer elapsed time is measured; saved work on a later hit is an estimate, not a benchmarked latency gain. Inclusive costs from nested caches must not be summed."}})
}
pub fn diagnostics() -> serde_json::Value {
    let (current, working) = working_set();
    let automatic = std::env::var("RIVER_RESOURCE_AUTOMATIC_KEYS").unwrap_or_default();
    let native=launcher_env().as_object().unwrap().keys().filter(|k|k.as_str()!="RIVER_RESOURCE_AUTOMATIC_KEYS").map(|key|(key.clone(),serde_json::json!({"value":std::env::var(key).ok(),"source":if automatic.split(',').any(|s|s==key){"automatic container CPU/memory plan"}else if std::env::var(key).is_ok_and(|v|!v.trim().is_empty()){"explicit environment/profile"}else{"automatic Rust fallback"}}))).collect::<BTreeMap<_,_>>();
    let mut rows = vec![];
    let snapshots = samples();
    let held = retained_bytes();
    for (name, allocation) in &plan().caches {
        let found = snapshots.iter().find(|(n, _, _)| *n == name);
        let occupancy = found
            .map(|(_, _, s)| s(effective(name, budget(name))))
            .unwrap_or_default();
        let counts = found.map(|(_, c, _)| c);
        rows.push(serde_json::json!({"name":name,"purpose":cache_purpose(name),"averageRetainedEntryBytes":if occupancy.entries>0 {Some(occupancy.bytes/occupancy.entries)} else {None},"estimatedWorkAvoidedNanoseconds":counts.map_or(0,|c|c.estimated_avoided_ns.load(Ordering::Relaxed)),"benefitMeasurement":benefit_measurement(name,counts.map_or(0,|c|c.hits.load(Ordering::Relaxed)),counts.map_or(0,|c|c.measured_reuse_samples.load(Ordering::Relaxed))),"workAvoidanceCaveat":"Prior observed rebuild cost, not a measured speedup; nested cache estimates must not be summed", "allocation":allocation,"effectiveLimitBytes":effective(name,budget(name)),"occupancy":occupancy,"detachedActivePayloadBytes":if matches!(name.as_str(),"raster_results" | "ranges"){None}else{Some(held.get(name.as_str()).copied().unwrap_or(0))},"sharedPayloadBytesEvictedCumulative":counts.map_or(0,|c|c.shared_evicted_bytes.load(Ordering::Relaxed)),"hits":counts.map_or(0,|c|c.hits.load(Ordering::Relaxed)),"misses":counts.map_or(0,|c|c.misses.load(Ordering::Relaxed)),"evictions":counts.map_or(0,|c|c.evictions.load(Ordering::Relaxed)),"bytesOrWorkReused":counts.map_or(0,|c|c.avoided_bytes.load(Ordering::Relaxed)),"initialized":found.is_some(),"storage":if name.ends_with("_disk"){if budget(name)==0{"disabled"}else{"persistent volume"}}else{"process-local RAM"}}));
    }
    serde_json::json!({"cacheLearning":crate::cache_learning::diagnostics(),"plan":plan(),"storage":crate::storage_mode::snapshot(plan().memory_allowance_bytes),"adaptation":crate::adaptation::snapshot(),"externalCache":crate::shared_cache::diagnostics(),"nativeAndAdmissionSettings":native,"caches":rows,"runtimeAssetRegistryFloorBytes":8192*plan().source_io_concurrency,"processRssBytes":rss(),"cgroupCurrentBytes":current,"cgroupWorkingSetBytes":working,"effectivePressureWorkingSetBytes":effective_pressure_usage(working),"pressureAccounting":"process RSS or cgroup working set exceeding headroom outside River allowance, whichever is larger","activeDecodeReservationBytes":ACTIVE_MEMORY.load(Ordering::Relaxed),"activeDecodeWorkspaces":ACTIVE_WORKSPACES.load(Ordering::Relaxed),"memoryAdmissionWaiters":MEMORY_WAITERS.load(Ordering::Relaxed),"pressure":*pressure().lock().unwrap_or_else(|e|e.into_inner()),"replicas":"Each process owns independent RAM caches; volumes are separate unless explicitly shared. RAM ceilings are not process memory; active requests can retain Arc payloads after cache eviction. For ranges and encoded results, shared bytes at eviction are cumulative; active copies and allocator retention remain included in RSS."})
}
pub fn start() {
    let _ = plan();
    tokio::spawn(async {
        let mut timer = tokio::time::interval(Duration::from_millis(500));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut next_regular=Instant::now();
        let mut previous_misses = 0;
        let mut learning_tick = 0;
        loop {
            timer.tick().await;
            if crate::adaptation::disabled() {
                continue;
            }
            let used=effective_pressure_usage(working_set().1);
            let limit=plan().memory_allowance_bytes;
            let emergency=used>=limit.saturating_mul(92)/100;
            let now=Instant::now();
            if now<next_regular {
                if emergency && SCALE.load(Ordering::Relaxed)>100 {
                    let _=tokio::task::spawn_blocking(reclaim_for_admission).await;
                }
                continue;
            }
            next_regular+=Duration::from_secs(2);
            if next_regular<=now {next_regular=now+Duration::from_secs(2);}
            learning_tick += 1;
            if learning_tick >= 30 {
                learning_tick = 0;
                let snapshots=samples();
                let rows=RAM.iter().map(|(name,_,_)| {
                    let allocation=&plan().caches[*name];let c=&counts()[*name];
                    let bytes=snapshots.iter().find(|(n,_,_)|n==name).map(|(_,_,sample)|sample(effective(name,budget(name))).bytes).unwrap_or(0);
                    crate::cache_learning::Observation {saved_ns:c.estimated_avoided_ns.load(Ordering::Relaxed),measured_hits:c.measured_reuse_samples.load(Ordering::Relaxed),hits:c.hits.load(Ordering::Relaxed),misses:c.misses.load(Ordering::Relaxed),evictions:c.evictions.load(Ordering::Relaxed),bytes,baseline:allocation.effective_target_bytes,explicit:allocation.explicit}
                }).collect();
                crate::cache_learning::sample(rows);
                reclaim();
            }
            let misses: u64 = counts().values().map(|counter| counter.misses.load(Ordering::Relaxed)).sum();
            let old_scale = crate::adaptation::ram_scale();
            crate::adaptation::sample_ram(misses.saturating_sub(previous_misses) >= 16 && used < limit.saturating_mul(65) / 100,
                used >= limit.saturating_mul(85) / 100);
            previous_misses = misses;
            let changed = {
                let mut p = pressure().lock().unwrap_or_else(|e| e.into_inner());
                let changed = p.step(used, plan().memory_allowance_bytes);
                SCALE.store(p.scale_per_mille, Ordering::Relaxed);
                changed
            };
            if changed || old_scale != crate::adaptation::ram_scale() || SCALE.load(Ordering::Relaxed) < 1000 {
                if emergency {
                    let _=tokio::task::spawn_blocking(reclaim_for_admission).await;
                } else {
                    let _=tokio::task::spawn_blocking(reclaim).await;
                }
            }
        }
    });
}
// Reservation protects simultaneous native work; it does not claim to measure
// an opaque codec allocator exactly. Current cgroup usage is checked as well.
pub struct Lease {
    bytes: usize,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _guard = memory_wait().0.lock().unwrap_or_else(|e| e.into_inner());
        ACTIVE_MEMORY.fetch_sub(self.bytes, Ordering::AcqRel);
        ACTIVE_WORKSPACES.fetch_sub(1, Ordering::AcqRel);
        memory_wait().1.notify_all();
    }
}
fn workspace_estimate(request: &crate::engine::TileDecodeRequest, tile_count: usize) -> usize {
    let scale = 1u64
        .checked_shl(u32::from(request.reduction))
        .unwrap_or(u64::MAX);
    let width = u64::from(request.source.width).div_ceil(scale);
    let height = u64::from(request.source.height).div_ceil(scale);
    let tile = request
        .source
        .native_tile_layout
        .map(|l| {
            u64::from(l.width)
                .div_ceil(scale)
                .saturating_mul(u64::from(l.height).div_ceil(scale))
        })
        .unwrap_or_else(|| u64::from(request.source_tile).div_ceil(scale).pow(2));
    // Huge native tiles use bounded ROI preparation, so their full canvas
    // is not an appropriate workspace reservation for a small transport tile.
    let batch = tile_count.min(request.max_native_tiles.max(1)) as u64;
    let region = u64::from(request.source_tile)
        .div_ceil(scale)
        .saturating_pow(2);
    let per_tile = if tile > 16 * 1024 * 1024 {
        region.max(4 * 1024 * 1024)
    } else {
        tile.max(region)
    };
    let pixels = per_tile
        .saturating_mul(batch)
        .min(width.saturating_mul(height));
    let sample_workspace = u64::from(request.source.components.unwrap_or(4).max(4)) * 8;
    (pixels
        .saturating_mul(sample_workspace)
        .min(usize::MAX as u64) as usize)
        .saturating_add(32 * M)
}
fn workspace_ceiling() -> usize {
    (plan().memory_allowance_bytes.saturating_mul(92) / 100).saturating_sub(plan().runtime_reserve_bytes)
}
// Only one waiter runs a sweep. Retry as readers release their leases,
// including when the working set stays high after the initial eviction.
fn reclaim_for_admission() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let Ok(mut last) = LAST.try_lock() else { return };
    if last.is_some_and(|when| when.elapsed() < Duration::from_secs(2)) {
        return;
    }
    if crate::adaptation::disabled() {
        // A one-off trim preserves the operator's fixed cache targets.
        for (name, _, sample) in samples() {
            if !name.ends_with("_disk") {
                sample(effective(name, budget(name)) / 10);
            }
        }
    } else {
        {
            let mut p = pressure().lock().unwrap_or_else(|e| e.into_inner());
            p.admission_pressure();
            SCALE.store(p.scale_per_mille, Ordering::Relaxed);
        }
        reclaim();
    }
    // The release image uses glibc. Freed pages in allocator arenas still
    // count toward RSS and cgroup limits until returned to the kernel.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe { libc::malloc_trim(0); }
    *last = Some(Instant::now());
}

#[derive(Debug)]
pub struct AdmissionTimeout {
    used: usize,
    estimate: usize,
    active: usize,
    ceiling: usize,
}
impl std::fmt::Display for AdmissionTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "memory admission timed out (working set {} bytes, workspace estimate {} bytes, active reservations {} bytes, admission ceiling {} bytes)",
            self.used, self.estimate, self.active, self.ceiling)
    }
}
impl std::error::Error for AdmissionTimeout {}

// A newly admitted worker can hold a reservation before its native allocator
// touches any pages. Charge outstanding reservations conservatively. Some
// are already resident; lease release wakes waiters to recheck actual usage.
fn workspace_fits(used: usize, active: usize, estimate: usize, ceiling: usize) -> bool {
    used.saturating_add(active).saturating_add(estimate) <= ceiling
}

pub fn admit(request: &crate::engine::TileDecodeRequest) -> anyhow::Result<Lease> {
    let estimate = workspace_estimate(request, request.tiles.len());
    let ceiling = plan().memory_allowance_bytes.saturating_mul(92) / 100;
    anyhow::ensure!(
        estimate <= workspace_ceiling(),
        "decode workspace estimate {estimate} exceeds available memory allowance; use a coarser view or increase instance memory"
    );
    let start = Instant::now();
    MEMORY_WAITERS.fetch_add(1, Ordering::Relaxed);
    struct Waiting;
    impl Drop for Waiting {
        fn drop(&mut self) {
            MEMORY_WAITERS.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let _waiting = Waiting;
    loop {
        anyhow::ensure!(
            request
                .cancellation
                .as_ref()
                .is_none_or(|c| !c.has_changed().is_err() && !*c.borrow()),
            "render request superseded while waiting for memory"
        );
        let (_, group_used) = working_set();
        let mut used = effective_pressure_usage(group_used);
        if !workspace_fits(used, ACTIVE_MEMORY.load(Ordering::Acquire), estimate, ceiling) {
            reclaim_for_admission();
            used = effective_pressure_usage(working_set().1);
        }
        // Commit the slot and bytes together. The releaser holds the same mutex,
        // so capacity cannot be freed between this check and the condition wait.
        let guard = memory_wait().0.lock().unwrap_or_else(|e| e.into_inner());
        let active = ACTIVE_MEMORY.load(Ordering::Acquire);
        if workspace_fits(used, active, estimate, ceiling)
            && ACTIVE_WORKSPACES.load(Ordering::Acquire) < plan().decode_concurrency
        {
            ACTIVE_MEMORY.fetch_add(estimate, Ordering::AcqRel);
            ACTIVE_WORKSPACES.fetch_add(1, Ordering::AcqRel);
            return Ok(Lease { bytes: estimate });
        }
        if start.elapsed() >= Duration::from_secs(30) {
            return Err(AdmissionTimeout { used, estimate, active, ceiling }.into());
        }
        let _ = memory_wait()
            .1
            .wait_timeout(guard, Duration::from_millis(100));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_process_allowance_does_not_charge_unrelated_group_memory() {
        assert_eq!(pressure_usage(1200*M,20*M,24000*M,512*M),20*M);
        assert_eq!(pressure_usage(1800*M,20*M,2048*M,1024*M),776*M);
        assert_eq!(pressure_usage(1800*M,20*M,2048*M,2048*M),1800*M);
        assert_eq!(pressure_usage(10*M,20*M,2048*M,1024*M),20*M);
    }
    fn d(memory: usize, cpus: usize) -> Detected {
        Detected {
            cpu_affinity: cpus,
            cpu_quota: Some(cpus as f64),
            effective_cpus: cpus,
            memory_limit_bytes: memory,
            memory_scopes: Vec::new(),
            memory_current_path: None,
            memory_stat_path: None,
            reasons: vec![],
        }
    }
    #[test]
    fn allocations_reserve_headroom_and_preserve_explicit_caps() {
        for memory in [256 * M, 512 * M, 2 * 1024 * M, 64 * 1024 * M] {
            let p = Plan::build(d(memory, 4), |_| None);
            let sum: usize = RAM
                .iter()
                .map(|(n, _, _)| p.caches[*n].configured_bytes)
                .sum();
            assert!(sum <= p.shared_cache_bytes);
            assert!(p.decode_concurrency <= 2);
            assert!(p.source_io_concurrency <= 4);
        }
        let p = Plan::build(d(2 * 1024 * M, 4), |k| {
            if k == "RIVER_S3_RANGE_CACHE_BYTES" {
                Some("123456".into())
            } else {
                None
            }
        });
        assert_eq!(p.caches["ranges"].configured_bytes, 123456);
        assert!(p.caches["ranges"].explicit);
    }
    #[test]
    fn concurrent_admission_accounts_for_pages_not_yet_touched() {
        assert!(workspace_fits(700, 0, 128, 900));
        assert!(!workspace_fits(700, 128, 128, 900));
    }

    #[test]
    fn blocked_admission_holds_targets_through_pressure_and_recovers() {
        let mut p = Pressure { scale_per_mille: 1000, ..Default::default() };
        p.admission_pressure();
        assert!(!p.step(95, 100));
        assert!(!p.step(75, 100));
        for _ in 0..4 { assert!(!p.step(60, 100)); }
        assert!(p.step(60, 100));
        assert_eq!(p.scale_per_mille, 150);
    }

    #[test]
    fn pressure_hysteresis_and_growth_are_bounded() {
        let mut p = Pressure {
            scale_per_mille: 1000,
            ..Default::default()
        };
        assert!(p.step(90, 100));
        assert_eq!(p.scale_per_mille, 750);
        for _ in 0..4 {
            assert!(!p.step(50, 100));
        }
        assert!(p.step(50, 100));
        assert_eq!(p.scale_per_mille, 800);
        for _ in 0..20 {
            p.step(100, 100);
        }
        assert_eq!(p.scale_per_mille, 100);
    }
    #[test]
    fn nested_v2_limits_and_fractional_cpu_quota() {
        let root = tempfile::tempdir().unwrap();
        let write = |p: &str, s: &str| {
            let p = root.path().join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        };
        write("proc/self/status", "Cpus_allowed_list:\t0-7\n");
        write("proc/meminfo", "MemTotal: 16777216 kB\n");
        write("proc/self/cgroup", "0::/group/child\n");
        write(
            "proc/self/mountinfo",
            "1 2 0:1 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
        );
        write("sys/fs/cgroup/group/cpu.max", "150000 100000");
        write("sys/fs/cgroup/group/memory.max", "536870912");
        write("sys/fs/cgroup/group/child/memory.max", "max");
        write("sys/fs/cgroup/group/memory.current", &(500*M).to_string());
        write("sys/fs/cgroup/group/memory.stat", &format!("inactive_file {}\n", 4*M));
        write("sys/fs/cgroup/group/child/memory.current", &(40*M).to_string());
        let d = detect(root.path());
        assert_eq!(ancestor_pressure(&d,40*M,512*M),496*M);
        assert_eq!(ancestor_pressure(&d,40*M,256*M),240*M);
        assert_eq!(d.effective_cpus, 1);
        assert_eq!(d.memory_limit_bytes, 512 * M);
    }
    #[test]
    fn v1_cpu_memory_and_missing_cgroup_fallback() {
        let root = tempfile::tempdir().unwrap();
        let write = |p: &str, s: &str| {
            let p = root.path().join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        };
        write("proc/self/status", "Cpus_allowed_list:\t2-3,7\n");
        write("proc/meminfo", "MemTotal: 8388608 kB\n");
        write("sys/fs/cgroup/cpu/cpu.cfs_quota_us", "200000");
        write("sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000");
        write("sys/fs/cgroup/memory/memory.limit_in_bytes", "1073741824");
        let d = detect(root.path());
        assert_eq!(d.effective_cpus, 2);
        assert_eq!(d.memory_limit_bytes, 1024 * M);
    }
}
