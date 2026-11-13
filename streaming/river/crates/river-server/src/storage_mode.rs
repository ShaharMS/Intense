//! Select mounted cache storage and bound file-only work in diskless pods.
use anyhow::{Context, Result};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{OnceLock, atomic::{AtomicUsize, Ordering}},
};

struct Mount {
    path: PathBuf,
    writable: bool,
    filesystem: String,
}

fn mounts() -> &'static [Mount] {
    static MOUNTS: OnceLock<Vec<Mount>> = OnceLock::new();
    MOUNTS.get_or_init(|| {
        std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default().lines().filter_map(|line| {
            let (mount, superblock) = line.split_once(" - ")?;
            let mut fields = mount.split_whitespace();
            let path = fields.nth(4)?.replace("\\040", " ").replace("\\011", "\t")
                .replace("\\012", "\n").replace("\\134", "\\");
            let mut superblock = superblock.split_whitespace();
            let filesystem = superblock.next()?.to_owned();
            let mount_writable = fields.next()?.split(',').any(|option| option == "rw");
            let superblock_writable = superblock.nth(1)?.split(',').any(|option| option == "rw");
            Some(Mount { path: path.into(), filesystem, writable: mount_writable && superblock_writable })
        }).collect()
    })
}

fn mount_at(path: &Path) -> Option<&'static Mount> {
    // Resolve existing ancestors so a symlink outside a mount cannot opt into disk writes.
    let mut ancestor = path;
    let resolved = loop {
        if let Ok(resolved) = ancestor.canonicalize() { break resolved; }
        ancestor = ancestor.parent()?;
    };
    mounts().iter().filter(|mount| resolved.starts_with(&mount.path))
        .max_by_key(|mount| mount.path.components().count())
}

fn forced() -> bool {
    crate::adaptation::no_disk()
}

fn attached(path: &Path) -> bool {
    if cfg!(target_os = "linux") {
        mount_at(path).is_some_and(|mount| mount.path != Path::new("/") && mount.writable
            && !matches!(mount.filesystem.as_str(), "tmpfs" | "ramfs" | "proc" | "sysfs"))
    } else {
        // Native development keeps the explicit directory behavior on other platforms.
        path.is_dir()
    }
}

fn disk_at(path: &Path) -> bool { attached(path) }

pub(crate) fn disk_cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    !forced() && *ENABLED.get_or_init(|| std::env::var_os("RIVER_S3_CACHE_ROOT")
        .is_some_and(|path| disk_at(Path::new(&path))))
}

pub(crate) fn scratch_limit(memory: usize) -> usize {
    if disk_cache_enabled() { return 0; }
    std::env::var("RIVER_RAM_SCRATCH_BYTES").ok().and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(memory / 8).min(memory / 2)
}

fn scratch_root() -> PathBuf {
    std::env::var_os("RIVER_RAM_SCRATCH_ROOT").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/dev/shm"))
}

fn verified_root() -> Option<PathBuf> {
    let root = scratch_root().canonicalize().ok()?;
    (root.is_dir() && mount_at(&root).is_some_and(|mount| mount.writable && mount.filesystem == "tmpfs"))
        .then_some(root)
}

static RESERVED: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub(crate) struct Unavailable(String);
impl std::fmt::Display for Unavailable {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { output.write_str(&self.0) }
}
impl std::error::Error for Unavailable {}

fn unavailable(message: impl Into<String>) -> anyhow::Error { Unavailable(message.into()).into() }

#[derive(Debug)]
struct Reservation(usize);
impl Drop for Reservation {
    fn drop(&mut self) { RESERVED.fetch_sub(self.0, Ordering::AcqRel); }
}

fn reserve(bytes: u64) -> Result<Reservation> {
    let limit = scratch_limit(crate::resources::plan().memory_allowance_bytes);
    let bytes = usize::try_from(bytes).context("Decoder scratch length exceeds addressable memory")?;
    let result = RESERVED.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
        used.checked_add(bytes).filter(|total| *total <= limit)
    });
    if let Err(used) = result {
        return Err(unavailable(format!(
            "RAM-only decoding needs {bytes} scratch bytes, but {used} of {limit} bytes are already reserved. Use a smaller view or increase the RAM scratch allowance and tmpfs capacity."
        )));
    }
    Ok(Reservation(bytes))
}

fn ram_workspace(bytes: u64) -> Result<PathBuf> {
    let root = verified_root().ok_or_else(|| unavailable(
        "RAM-only decoding requires a writable tmpfs workspace. Mount one and set RIVER_RAM_SCRATCH_ROOT, or make /dev/shm available."
    ))?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(root.as_os_str().as_bytes())?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } == 0 {
            let stats = unsafe { stats.assume_init() };
            let available = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
            if available < bytes {
                return Err(unavailable(format!("Decoder needs {bytes} scratch bytes, but the tmpfs mount has {available} bytes free. Increase tmpfs capacity or use a smaller view.")));
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = bytes;
    Ok(root)
}

pub(crate) fn validate() -> Result<()> {
    if let Ok(value) = std::env::var("RIVER_RAM_SCRATCH_BYTES") {
        value.parse::<usize>().context("RIVER_RAM_SCRATCH_BYTES must be an integer byte limit")?;
    }
    if let Ok(value) = std::env::var("RIVER_RESOURCE_DISPLAY") {
        anyhow::ensure!(matches!(value.as_str(), "auto" | "color" | "unicode" | "ascii" | "off"),
            "RIVER_RESOURCE_DISPLAY must be auto, color, unicode, ascii or off");
    }
    Ok(())
}

pub(crate) fn locator_limit(memory: usize) -> usize {
    if disk_cache_enabled() { 0 } else { (memory / 512).min(16 << 20) }
}

#[derive(Default)]
struct Locators {
    entries: std::collections::HashMap<String, (String, String, usize, u64)>,
    order: std::collections::BTreeSet<(u64, String)>,
    bytes: usize,
    tick: u64,
}

fn locators() -> &'static std::sync::Mutex<Locators> {
    static LOCATORS: OnceLock<std::sync::Mutex<Locators>> = OnceLock::new();
    LOCATORS.get_or_init(Default::default)
}

pub(crate) fn remember_locator(id: &str, source: &str, key: &str) {
    let limit = locator_limit(crate::resources::plan().memory_allowance_bytes);
    let bytes = id.len().saturating_mul(2).saturating_add(source.len())
        .saturating_add(key.len()).saturating_add(256);
    if bytes > limit || bytes > 8192 { return; }
    let mut cache = locators().lock().unwrap_or_else(|error| error.into_inner());
    if let Some((_, _, old_bytes, tick)) = cache.entries.remove(id) {
        cache.bytes -= old_bytes;
        cache.order.remove(&(tick, id.to_owned()));
    }
    while cache.bytes.saturating_add(bytes) > limit {
        let Some((_, id)) = cache.order.pop_first() else { break; };
        if let Some((_, _, old_bytes, _)) = cache.entries.remove(&id) { cache.bytes -= old_bytes; }
    }
    cache.tick += 1;
    let tick = cache.tick;
    cache.entries.insert(id.to_owned(), (source.to_owned(), key.to_owned(), bytes, tick));
    cache.order.insert((tick, id.to_owned()));
    cache.bytes += bytes;
}

pub(crate) fn lookup_locator(id: &str) -> Option<(String, String)> {
    let mut cache = locators().lock().unwrap_or_else(|error| error.into_inner());
    let (source, key, _, old) = cache.entries.get(id)?.clone();
    cache.tick += 1;
    let tick = cache.tick;
    cache.entries.get_mut(id).unwrap().3 = tick;
    cache.order.remove(&(old, id.to_owned()));
    cache.order.insert((tick, id.to_owned()));
    Some((source, key))
}

#[derive(Debug)]
pub(crate) struct ScratchFile {
    file: tempfile::NamedTempFile,
    reservation: Option<Reservation>,
}
impl ScratchFile {
    pub fn create(prefix: &str, suffix: &str, bytes: u64, disk_directory: Option<&Path>) -> Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix(prefix).suffix(suffix);
        if disk_cache_enabled() {
            let file = if let Some(root) = disk_directory {
                std::fs::create_dir_all(root)?;
                builder.tempfile_in(root)?
            } else { builder.tempfile()? };
            return Ok(Self { file, reservation: None });
        }
        let reservation = reserve(bytes)?;
        let root = ram_workspace(bytes)?;
        let file = builder.tempfile_in(root).map_err(|error|
            unavailable(format!("Cannot create a decoder scratch file on tmpfs: {error}")))?;
        Ok(Self { file, reservation: Some(reservation) })
    }
    pub fn path(&self) -> &Path { self.file.path() }
}
impl Write for ScratchFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> { self.file.write(data) }
    fn flush(&mut self) -> io::Result<()> { self.file.flush() }
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data).map_err(|error| {
            if self.reservation.is_some() && error.kind() == io::ErrorKind::StorageFull {
                io::Error::new(error.kind(), Unavailable(
                    "Decoder scratch tmpfs is full. Increase the mounted tmpfs capacity or use a smaller view.".into()))
            } else { error }
        })
    }
}

pub(crate) struct ScratchDirectory {
    directory: tempfile::TempDir,
    reservation: Option<Reservation>,
}
impl ScratchDirectory {
    pub fn create(bytes: u64) -> Result<Self> {
        if disk_cache_enabled() {
            return Ok(Self { directory: tempfile::tempdir()?, reservation: None });
        }
        let reservation = reserve(bytes)?;
        let root = ram_workspace(bytes)?;
        let directory = tempfile::tempdir_in(root).map_err(|error|
            unavailable(format!("Cannot create the decoder output directory on tmpfs: {error}")))?;
        Ok(Self { directory, reservation: Some(reservation) })
    }
    pub fn path(&self) -> &Path { self.directory.path() }
    #[cfg(feature = "grok")]
    pub fn configure_child(&self, command: &mut std::process::Command) {
        #[cfg(target_os = "linux")]
        if let Some(reservation) = &self.reservation {
            use std::os::unix::process::CommandExt;
            let limit = reservation.0 as libc::rlim_t;
            command.env("TMPDIR", self.path());
            // setrlimit is async-signal-safe and applies only to the decoder child.
            unsafe { command.pre_exec(move || {
                let limits = libc::rlimit { rlim_cur: limit, rlim_max: limit };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limits) != 0 { return Err(io::Error::last_os_error()); }
                Ok(())
            }); }
        }
    }
}

pub(crate) fn snapshot(memory: usize) -> serde_json::Value {
    let disk = disk_cache_enabled();
    let locator = locators().lock().unwrap_or_else(|error| error.into_inner());
    serde_json::json!({
        "mode": if disk { "mounted-cache-volume" } else { "ram-only" },
        "reason": if forced() { "explicit no-disk constraint" } else if disk {
            "configured cache root is on a writable attached volume"
        } else { "no writable attached cache volume" },
        "scratchRoot": if disk { None } else { verified_root() },
        "scratchLimitBytes": scratch_limit(memory),
        "scratchReservedBytes": RESERVED.load(Ordering::Relaxed),
        "ramLocatorLimitBytes": locator_limit(memory),
        "ramLocatorBytes": locator.bytes,
        "ramLocatorEntries": locator.entries.len(),
        "diskFallbackAllowed": disk,
    })
}

fn human(bytes: usize) -> String {
    for (scale, unit) in [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")] {
        if bytes >= scale { return format!("{:.1} {unit}", bytes as f64 / scale as f64); }
    }
    format!("{bytes} B")
}

pub(crate) fn print_startup() {
    use std::io::IsTerminal;
    let format = std::env::var("RIVER_RESOURCE_DISPLAY").unwrap_or_else(|_| "auto".into());
    if format == "off" { return; }
    let terminal = io::stderr().is_terminal()
        && std::env::var("TERM").as_deref() != Ok("dumb");
    let color = format == "color" || (format == "auto" && terminal && std::env::var_os("NO_COLOR").is_none());
    let unicode = color || format == "unicode" || (format == "auto" && terminal);
    let plan = crate::resources::plan();
    let memory = plan.memory_allowance_bytes;
    let scratch = scratch_limit(memory);
    let cache_target = crate::resources::RAM.iter().map(|(name, _, _)| {
        crate::resources::effective(name, crate::resources::budget(name))
    }).fold(0usize, usize::saturating_add);
    let allocated = [cache_target, plan.decoder_reserve_bytes, plan.request_reserve_bytes,
        scratch, plan.runtime_reserve_bytes, plan.temporary_reserve_bytes].into_iter().fold(0usize, usize::saturating_add);
    let groups = [
        ("RAM cache target", cache_target, '█', '#', 36),
        ("Decoder reserve", plan.decoder_reserve_bytes, '▓', '=', 35),
        ("Request reserve", plan.request_reserve_bytes, '▒', '+', 34),
        ("RAM-backed scratch", scratch, '▌', '*', 33),
        ("Runtime and temporary", plan.runtime_reserve_bytes.saturating_add(plan.temporary_reserve_bytes), '░', ':', 32),
        ("Unassigned headroom", memory.saturating_sub(allocated), '·', '.', 90),
    ];
    let mut chart = format!("\nRiver resource plan | {} RAM | {} effective CPUs\n", human(memory), plan.detected.effective_cpus);
    let mut used = 0usize;
    let mut drawn = 0usize;
    if !unicode { chart.push_str("+------------------------------------------------+\n"); }
    chart.push(if unicode { '[' } else { '|' });
    for (_, bytes, texture, plain, ansi) in groups {
        used = used.saturating_add(bytes);
        let end = ((used.min(memory) as u128 * 48) / memory.max(1) as u128) as usize;
        if end > drawn {
            if color { chart.push_str(&format!("\x1b[{ansi}m")); }
            chart.extend(std::iter::repeat_n(if unicode { texture } else { plain }, end - drawn));
            if color { chart.push_str("\x1b[0m"); }
        }
        drawn = end;
    }
    chart.push_str(if unicode { "]\n" } else { "|\n+------------------------------------------------+\n" });
    for (label, bytes, texture, plain, ansi) in groups {
        chart.push_str("  ");
        if color { chart.push_str(&format!("\x1b[{ansi}m")); }
        chart.push(if unicode { texture } else { plain });
        if color { chart.push_str("\x1b[0m"); }
        chart.push_str(&format!("  {label:<24} {:>10}\n", human(bytes)));
    }
    let storage = snapshot(memory);
    chart.push_str(&format!("  Cache storage              {}\n", storage["mode"].as_str().unwrap_or_default()));
    chart.push_str(&format!("  Storage decision           {}\n", storage["reason"].as_str().unwrap_or_default()));
    let policy = crate::adaptation::snapshot();
    chart.push_str(&format!("  Adaptation                 {} ({})\n",
        policy["mode"].as_str().unwrap_or("automatic defaults"), policy["source"].as_str().unwrap_or_default()));
    if !disk_cache_enabled() {
        chart.push_str(&format!("  Scratch workspace          {}\n", storage["scratchRoot"].as_str().unwrap_or("unavailable (requires tmpfs)")));
    }
    chart.push_str(&format!("  Decode / source IO slots   {} / {}\n", plan.decode_concurrency, plan.source_io_concurrency));
    let external = crate::shared_cache::diagnostics();
    if external["configured"] == true {
        let backend = if external["server"]["valkey_version"].is_string() { "Valkey" }
            else if external["server"]["redis_version"].is_string() { "Redis" } else { "Redis/Valkey" };
        chart.push_str(&format!("  External cache             {backend} ({})\n", external["namespace"].as_str().unwrap_or_default()));
        if let Some(bytes) = external["server"]["used_memory"].as_u64() {
            chart.push_str(&format!("  External memory (server)   {}\n", human(bytes as usize)));
        } else { chart.push_str("  External memory (server)   metrics pending or unavailable\n"); }
        chart.push_str("  External footprint is separate from the pod budget.\n");
    } else { chart.push_str("  External cache             not configured\n"); }
    chart.push_str("  Local RAM cache targets\n");
    for (name, _, _) in crate::resources::RAM {
        chart.push_str(&format!("    {name:<22} {:>10}\n",
            human(crate::resources::effective(name, crate::resources::budget(name)))));
    }
    if allocated > memory {
        chart.push_str(&format!("  Warning: configured reserves exceed detected memory by {}\n", human(allocated - memory)));
    }
    chart.push_str("  Targets are ceilings. Current occupancy is available at /api/diagnostics/resources.\n");
    eprint!("{chart}");
}
