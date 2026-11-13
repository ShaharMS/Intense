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
                    .is_none_or(|p: &PathBuf| p.components().count() < path.components().count())
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
    pub preview_buffer_bytes: usize,
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
        let large_liberal = mem >= 32 * 1024 * M && crate::adaptation::startup_liberal_ram() && !compact;
        let runtime = (mem / if large_liberal { 20 } else { 8 }).max(128 * M).min(mem / 2);
        let decoder = decode.saturating_mul(
            n("RIVER_DECODE_WORKSPACE_BYTES").unwrap_or((mem / 8).clamp(64 * M, if large_liberal { 128 * M } else { 256 * M })),
        );
        let requests = io.saturating_mul(n("RIVER_S3_REQUEST_BYTES").unwrap_or(64 * M));
        let temporary = (mem / if large_liberal { 32 } else { 16 }).max(32 * M);
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
        Self{detected:d,memory_allowance_bytes:mem,runtime_reserve_bytes:runtime,decoder_reserve_bytes:decoder,request_reserve_bytes:requests,temporary_reserve_bytes:temporary,shared_cache_bytes:shared,explicit_cache_bytes:explicit,overcommitted_explicit_bytes:explicit.saturating_sub(shared),decode_concurrency:decode,source_io_concurrency:io,preview_buffer_bytes:n("RIVER_PREVIEW_PIXEL_REUSE_BYTES").unwrap_or((temporary/4).min(64*M)),caches,decisions:vec!["Automatic cache shares use fixed weights; no speculative benefit-driven redistribution".into(),format!("Automatic decode slots=min(32, max(1, floor(CPU/2)), max(1, RAM/512MiB)); effective slots={decode}; explicit environment/profile values take precedence"),format!("Automatic source IO=min(16, CPU, max(1, RAM/256MiB)); effective IO={io}; persisted per-source limits and learning stay independent"),"More memory increases retention; CPU quotas and source-specific admission remain independent limits".into(),"Cache occupancy counts retained entries; process RSS and active decode reservations are reported separately".into()]}
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
struct Retained {
    bytes: usize,
    live: Box<dyn Fn() -> bool + Send + Sync>,
}
fn retained() -> &'static Mutex<HashMap<(&'static str, usize), Retained>> {
    static R: OnceLock<Mutex<HashMap<(&'static str, usize), Retained>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn retain<T: ?Sized + Send + Sync + 'static>(name: &'static str, bytes: usize, data: &Arc<T>) {
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
pub fn reclaim() {
    let _ = retained_bytes();
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
    fn step(&mut self, used: usize, limit: usize) -> bool {
        let old = self.scale_per_mille.max(100);
        let new = if used >= limit.saturating_mul(85) / 100 {
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
    let inactive = d
        .memory_stat_path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| {
            s.lines().find_map(|l| {
                let mut f = l.split_whitespace();
                if matches!(f.next()?, "inactive_file" | "total_inactive_file") {
                    f.next()?.parse::<usize>().ok()
                } else {
                    None
                }
            })
        })
        .unwrap_or(0);
    (current, current.saturating_sub(inactive))
}
static ACTIVE_MEMORY: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_WORKSPACES: AtomicUsize = AtomicUsize::new(0);
fn memory_wait() -> &'static (Mutex<()>, std::sync::Condvar) {
    static W: OnceLock<(Mutex<()>, std::sync::Condvar)> = OnceLock::new();
    W.get_or_init(|| (Mutex::new(()), std::sync::Condvar::new()))
}
static MEMORY_WAITERS: AtomicUsize = AtomicUsize::new(0);
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
        rows.push(serde_json::json!({"name":name,"allocation":allocation,"effectiveLimitBytes":effective(name,budget(name)),"occupancy":occupancy,"detachedActivePayloadBytes":if name=="raster_results"{None}else{Some(held.get(name.as_str()).copied().unwrap_or(0))},"sharedPayloadBytesEvictedCumulative":counts.map_or(0,|c|c.shared_evicted_bytes.load(Ordering::Relaxed)),"hits":counts.map_or(0,|c|c.hits.load(Ordering::Relaxed)),"misses":counts.map_or(0,|c|c.misses.load(Ordering::Relaxed)),"evictions":counts.map_or(0,|c|c.evictions.load(Ordering::Relaxed)),"bytesOrWorkReused":counts.map_or(0,|c|c.avoided_bytes.load(Ordering::Relaxed)),"initialized":found.is_some(),"storage":if name.ends_with("_disk"){if budget(name)==0{"disabled"}else{"persistent volume"}}else{"process-local RAM"}}));
    }
    serde_json::json!({"plan":plan(),"storage":crate::storage_mode::snapshot(plan().memory_allowance_bytes),"adaptation":crate::adaptation::snapshot(),"externalCache":crate::shared_cache::diagnostics(),"nativeAndAdmissionSettings":native,"caches":rows,"runtimeAssetRegistryFloorBytes":8192*plan().source_io_concurrency,"processRssBytes":rss(),"cgroupCurrentBytes":current,"cgroupWorkingSetBytes":working,"activeDecodeReservationBytes":ACTIVE_MEMORY.load(Ordering::Relaxed),"activeDecodeWorkspaces":ACTIVE_WORKSPACES.load(Ordering::Relaxed),"memoryAdmissionWaiters":MEMORY_WAITERS.load(Ordering::Relaxed),"pressure":*pressure().lock().unwrap_or_else(|e|e.into_inner()),"replicas":"Each process owns independent RAM caches; volumes are separate unless explicitly shared. RAM ceilings are not process memory; active requests can retain Arc payloads after cache eviction. Bytes payloads have no weak-reference API: encoded shared bytes at eviction are cumulative, not current occupancy; active copies and allocator retention remain included in RSS."})
}
pub fn start() {
    let _ = plan();
    tokio::spawn(async {
        let mut timer = tokio::time::interval(Duration::from_secs(2));
        let mut previous_misses = 0;
        loop {
            timer.tick().await;
            if crate::adaptation::disabled() {
                continue;
            }
            let (_, used) = working_set();
            let misses: u64 = counts().values().map(|counter| counter.misses.load(Ordering::Relaxed)).sum();
            let limit = plan().memory_allowance_bytes;
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
                let _ = tokio::task::spawn_blocking(reclaim).await;
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
        ACTIVE_MEMORY.fetch_sub(self.bytes, Ordering::AcqRel);
        ACTIVE_WORKSPACES.fetch_sub(1, Ordering::AcqRel);
        memory_wait().1.notify_all();
    }
}
pub fn admit(request: &crate::engine::TileDecodeRequest) -> anyhow::Result<Lease> {
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
    let batch = request.tiles.len().min(request.max_native_tiles.max(1)) as u64;
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
    let estimate = (pixels
        .saturating_mul(sample_workspace)
        .min(usize::MAX as u64) as usize)
        .saturating_add(32 * M);
    let ceiling = plan().memory_allowance_bytes.saturating_mul(92) / 100;
    anyhow::ensure!(
        estimate <= ceiling.saturating_sub(plan().runtime_reserve_bytes),
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
        let active = ACTIVE_MEMORY.load(Ordering::Acquire);
        let (_, used) = working_set();
        if active.saturating_add(estimate) <= ceiling && used.saturating_add(estimate) <= ceiling {
            if ACTIVE_WORKSPACES.load(Ordering::Acquire) < plan().decode_concurrency
                && ACTIVE_MEMORY
                    .compare_exchange_weak(
                        active,
                        active + estimate,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                if ACTIVE_WORKSPACES
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        (n < plan().decode_concurrency).then_some(n + 1)
                    })
                    .is_ok()
                {
                    return Ok(Lease { bytes: estimate });
                }
                ACTIVE_MEMORY.fetch_sub(estimate, Ordering::AcqRel);
            }
        } else if start.elapsed() < Duration::from_millis(250) {
            let previous = SCALE.swap(100, Ordering::Relaxed);
            reclaim();
            SCALE.store(
                previous.min(
                    pressure()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .scale_per_mille,
                ),
                Ordering::Relaxed,
            );
        }
        anyhow::ensure!(
            start.elapsed() < Duration::from_secs(30),
            "memory admission timed out; cache leases or other processes still retain memory"
        );
        let guard = memory_wait().0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = memory_wait()
            .1
            .wait_timeout(guard, Duration::from_millis(100));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn d(memory: usize, cpus: usize) -> Detected {
        Detected {
            cpu_affinity: cpus,
            cpu_quota: Some(cpus as f64),
            effective_cpus: cpus,
            memory_limit_bytes: memory,
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
        let d = detect(root.path());
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
