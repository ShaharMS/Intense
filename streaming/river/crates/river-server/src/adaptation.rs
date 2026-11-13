//! Live policy for admission and optional cache targets.
use anyhow::{Result, ensure};
use std::sync::{Arc, OnceLock, RwLock, atomic::{AtomicUsize, Ordering}};

const RAM: u8 = 1;
const DISK: u8 = 2;
const COMPUTE: u8 = 4;
const NO_DISK: u8 = 8;
const DISABLED: u8 = 16;
const SET: u8 = 32;

#[derive(Clone, Default)]
struct Mode { label: Option<String>, flags: u8 }

fn parse(value: Option<&str>) -> Result<Mode> {
    let Some(value) = value else { return Ok(Mode::default()); };
    let mut flags = SET;
    let mut presets = 0;
    let mut dimensions = 0;
    let mut labels = Vec::new();
    for part in value.split(',').map(str::trim) {
        ensure!(!labels.contains(&part), "Repeated adaptation mode {part}");
        labels.push(part);
        match part {
            "disabled" => { presets += 1; flags |= DISABLED; }
            "conservative" => { presets += 1; flags |= RAM | DISK | COMPUTE; }
            "liberal" => { presets += 1; }
            "conserve-ram" => { dimensions += 1; flags |= RAM; }
            "conserve-disk" => { dimensions += 1; flags |= DISK; }
            "conserve-compute" => { dimensions += 1; flags |= COMPUTE; }
            "no-disk" => flags |= NO_DISK,
            _ => anyhow::bail!("Unknown adaptation mode {part}; use disabled, conservative, liberal, conserve-ram, conserve-disk, conserve-compute or no-disk"),
        }
    }
    ensure!(presets <= 1 && (presets == 0 || dimensions == 0), "Use one adaptation preset or combine conserve flags; no-disk can accompany either");
    Ok(Mode { label: Some(labels.join(",")), flags })
}

struct State { startup: Mode, current: Mode, startup_source: &'static str, live: bool }
static STATE: OnceLock<RwLock<State>> = OnceLock::new();
static FLAGS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static RAM_SCALE: AtomicUsize = AtomicUsize::new(1000);

pub fn validate(value: Option<&str>) -> Result<()> { parse(value).map(|_| ()) }

pub fn initialize(configured: Option<&str>) -> Result<()> {
    let env = std::env::var("RIVER_AUTO_ADAPTATION_MODE").ok().filter(|value| !value.trim().is_empty());
    let legacy = std::env::var("RIVER_RESOURCE_ADAPTIVE").is_ok_and(|value| value == "false" || value == "0");
    let mode = parse(env.as_deref().or(configured).or(legacy.then_some("disabled")))?;
    let source = if env.is_some() { "environment" } else if configured.is_some() { "configuration" }
        else if legacy { "legacy environment" } else { "default" };
    FLAGS.store(mode.flags, Ordering::Release);
    RAM_SCALE.store(if mode.flags & RAM != 0 { 300 } else { 1000 }, Ordering::Relaxed);
    let _ = STATE.set(RwLock::new(State { startup: mode.clone(), current: mode, startup_source: source, live: false }));
    Ok(())
}

pub fn set_live(value: Option<&str>) -> Result<()> {
    let state = STATE.get().ok_or_else(|| anyhow::anyhow!("Resource adaptation is not initialized"))?;
    let mut state = state.write().unwrap_or_else(|error| error.into_inner());
    let mut mode = if value.is_some() { parse(value)? } else { state.startup.clone() };
    // A deployment's hard storage constraint remains in force for live tuning.
    mode.flags |= state.startup.flags & NO_DISK;
    RAM_SCALE.store(if mode.flags & RAM != 0 { 300 } else { 1000 }, Ordering::Relaxed);
    FLAGS.store(mode.flags, Ordering::Release);
    state.current = mode;
    state.live = value.is_some();
    wake_admissions();
    Ok(())
}

fn has(flag: u8) -> bool { FLAGS.load(Ordering::Acquire) & flag != 0 }
pub fn no_disk() -> bool { has(NO_DISK) }
pub fn conserve_disk() -> bool { has(DISK) || no_disk() }
pub fn conserve_compute() -> bool { has(COMPUTE) }
pub fn disabled() -> bool { has(DISABLED) }
pub fn liberal_ram() -> bool { has(SET) && !has(RAM) && !disabled() }
pub fn startup_liberal_ram() -> bool {
    STATE.get().is_some_and(|state| {
        let flags = state.read().unwrap_or_else(|error| error.into_inner()).startup.flags;
        flags & SET != 0 && flags & (RAM | DISABLED) == 0
    })
}
pub fn ram_scale() -> usize { RAM_SCALE.load(Ordering::Relaxed) }

pub fn sample_ram(demand: bool, pressure: bool) {
    if !has(RAM) { return; }
    if pressure {
        let _ = RAM_SCALE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |scale| Some(scale.saturating_sub(100).max(250)));
    } else if demand {
        let _ = RAM_SCALE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |scale| Some((scale + 50).min(800)));
    }
}

pub fn snapshot() -> serde_json::Value {
    let Some(state) = STATE.get() else { return serde_json::json!({"mode":null,"source":"default"}); };
    let plan = crate::resources::plan();
    let state = state.read().unwrap_or_else(|error| error.into_inner());
    serde_json::json!({"mode":state.current.label, "startupMode":state.startup.label,
        "source":if state.live {"live API"}else{state.startup_source},
        "conservesRam":has(RAM), "conservesDisk":conserve_disk(),
        "conservesCompute":conserve_compute(), "noDisk":no_disk(),
        "rebalancingEnabled":!disabled(), "automaticRamTargetPerMille":ram_scale(),
        "decodeAdmissionTarget":Admission::new(plan.decode_concurrency,"RIVER_DECODE_CONCURRENCY").target(),
        "sourceIoAdmissionTarget":Admission::new(plan.source_io_concurrency,"RIVER_SOURCE_IO_CONCURRENCY").target()})
}

fn notifications() -> &'static tokio::sync::Notify {
    static WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();
    WAKE.get_or_init(Default::default)
}
fn wake_admissions() { notifications().notify_waiters(); }

pub struct Admission { ceiling: usize, explicit: bool, active: AtomicUsize }
pub struct Permit(Arc<Admission>);
impl Drop for Permit {
    fn drop(&mut self) { self.0.active.fetch_sub(1, Ordering::AcqRel); wake_admissions(); }
}

pub fn explicit(key: &str) -> bool {
    let generated = std::env::var("RIVER_RESOURCE_AUTOMATIC_KEYS").unwrap_or_default();
    !generated.split(',').any(|name| name == key) &&
        std::env::var(key).is_ok_and(|value| !value.trim().is_empty() && value != "auto")
}

impl Admission {
    pub fn new(ceiling: usize, key: &str) -> Self {
        Self { ceiling, explicit: explicit(key), active: AtomicUsize::new(0) }
    }
    pub fn target(&self) -> usize {
        if !self.explicit && conserve_compute() { self.ceiling.div_ceil(2).max(1) } else { self.ceiling }
    }
    pub async fn acquire_owned(self: Arc<Self>) -> Result<Permit> {
        loop {
            let ready = notifications().notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if self.active.fetch_update(Ordering::AcqRel, Ordering::Acquire,
                |active| (active < self.target()).then_some(active + 1)).is_ok() {
                return Ok(Permit(self));
            }
            ready.await;
        }
    }
}
