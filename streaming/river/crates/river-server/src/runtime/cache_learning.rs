//! Bounded workload learning. No key-level histories or requests on the hot path.
use std::sync::{OnceLock, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
use crate::resources::RAM;

static ENABLED: AtomicBool = AtomicBool::new(true);
static STARTUP: OnceLock<bool> = OnceLock::new();
static TARGETS: [AtomicUsize; 9] = [const { AtomicUsize::new(0) }; 9];

#[derive(Clone, Default, serde::Serialize)]
pub struct Observation {
    pub hits: u64,
    pub saved_ns:u64,
    pub measured_hits:u64,
    pub misses: u64,
    pub evictions: u64,
    pub bytes: usize,
    pub baseline: usize,
    pub explicit: bool,
}
#[derive(Default)]
struct History { previous: Vec<Observation>, last_window:Vec<Observation>, idle_windows: [usize; 9], windows: usize }
fn history() -> &'static Mutex<History> {
    static HISTORY: OnceLock<Mutex<History>> = OnceLock::new();
    HISTORY.get_or_init(Default::default)
}
pub fn initialize(configured: Option<bool>) -> anyhow::Result<()> {
    let enabled = match std::env::var("RIVER_CACHE_LEARNING") {
        Ok(value) => value.parse::<bool>().map_err(|_| anyhow::anyhow!("RIVER_CACHE_LEARNING must be true or false"))?,
        Err(std::env::VarError::NotPresent) => configured.unwrap_or(true),
        Err(_) => anyhow::bail!("RIVER_CACHE_LEARNING must contain valid text"),
    };
    let _ = STARTUP.set(enabled); set(enabled); Ok(())
}
pub fn set(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    for target in &TARGETS { target.store(0, Ordering::Relaxed); }
    *history().lock().unwrap_or_else(|e|e.into_inner()) = History::default();
}
pub fn reset() { set(*STARTUP.get().unwrap_or(&true)); }
pub fn target(name: &str, baseline: usize) -> usize {
    if !ENABLED.load(Ordering::Relaxed) || crate::adaptation::disabled() { return baseline; }
    RAM.iter().position(|(n,_,_)|*n==name).map(|i|TARGETS[i].load(Ordering::Relaxed)).filter(|n|*n>0).unwrap_or(baseline)
}

/// Transfer at most 5% of each baseline per minute; never change explicit limits.
/// Three no-reuse windows precede donation, and half the baseline is protected.
fn transfer(rows: &[Observation], idle: &[usize;9], targets: &mut [usize;9]) {
    let mut donated=[0usize;9];
    let score=|row:&Observation| if row.measured_hits>=8 && row.saved_ns>0 {
        row.saved_ns as f64/row.bytes.max(1) as f64 * (row.measured_hits as f64/row.hits.max(1) as f64).min(1.0)
    }else{0.0};
    let mut receivers=(0..9).collect::<Vec<_>>();receivers.sort_by(|a,b|score(&rows[*b]).total_cmp(&score(&rows[*a])));
    for receiver in receivers {
        let row = &rows[receiver];
        if row.explicit || row.hits < 32 || score(row)==0.0 || row.evictions == 0 || row.bytes < targets[receiver].saturating_mul(3)/4 { continue; }
        let mut wanted = (row.baseline/20).min(row.baseline.saturating_mul(2).saturating_sub(targets[receiver]));
        for donor in 0..9 {
            if wanted == 0 { break; }
            let measured_lower_value=score(&rows[donor])>0.0 && score(&rows[donor])*2.0<score(row)
                && rows[donor].bytes<targets[donor].saturating_mul(3)/4 && rows[donor].evictions==0;
            if donor == receiver || rows[donor].explicit || (idle[donor] < 3 && !measured_lower_value) { continue; }
            let available = targets[donor].saturating_sub(rows[donor].baseline/2).min((rows[donor].baseline/20).saturating_sub(donated[donor]));
            let amount = available.min(wanted);
            targets[donor] -= amount; donated[donor] += amount; targets[receiver] += amount; wanted -= amount;
        }
    }
}

pub fn sample(current: Vec<Observation>) {
    if current.len()!=9 || !ENABLED.load(Ordering::Relaxed) || crate::adaptation::disabled() { return; }
    let mut history=history().lock().unwrap_or_else(|e|e.into_inner());
    if history.previous.len()!=9 { history.previous=current; return; }
    let mut delta=current.clone();
    let mut targets=[0;9];
    for i in 0..9 {
        delta[i].hits=current[i].hits.saturating_sub(history.previous[i].hits);
        delta[i].saved_ns=current[i].saved_ns.saturating_sub(history.previous[i].saved_ns);
        delta[i].measured_hits=current[i].measured_hits.saturating_sub(history.previous[i].measured_hits);
        delta[i].misses=current[i].misses.saturating_sub(history.previous[i].misses);
        delta[i].evictions=current[i].evictions.saturating_sub(history.previous[i].evictions);
        history.idle_windows[i]=if delta[i].hits==0 {history.idle_windows[i].saturating_add(1)} else {0};
        targets[i]=target(RAM[i].0,current[i].baseline);
    }
    transfer(&delta,&history.idle_windows,&mut targets);
    // Publish decreases before increases: readers never see an inflated total.
    for i in 0..9 { if targets[i] < target(RAM[i].0,current[i].baseline) { TARGETS[i].store(targets[i],Ordering::Relaxed); } }
    for i in 0..9 { TARGETS[i].store(targets[i],Ordering::Relaxed); }
    history.previous=current;history.last_window=delta;history.windows+=1;
}

pub fn diagnostics() -> serde_json::Value {
    let history=history().lock().unwrap_or_else(|e|e.into_inner());
    serde_json::json!({"enabled":ENABLED.load(Ordering::Relaxed),"pausedByAdaptationMode":crate::adaptation::disabled(),
        "policy":"measured producer-cost estimates saved per resident byte, weighted by coverage, plus reuse and eviction pressure; unknown benefit never treated as zero cost",
        "lastWindow":history.last_window,"minimumMeasuredReuseSamples":8,
        "windowSeconds":60,"sampledWindows":history.windows,"idleWindows":history.idle_windows,
        "protectedBaselineFraction":0.5,"maximumBaselineMultiple":2,"maximumTransferPerWindowFraction":0.05,
        "scope":"process-local deployment workload; learning resets on restart or settings change",
        "targets":RAM.iter().enumerate().map(|(i,(name,_,_))|serde_json::json!({"name":name,"learnedTargetBytes":TARGETS[i].load(Ordering::Relaxed)})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workload_moves_only_bounded_automatic_budget_and_respects_floors() {
        let mut rows=vec![Observation{baseline:10000,..Default::default()};9];
        rows[0].explicit=true; rows[7].hits=100;rows[7].saved_ns=100000000;rows[7].measured_hits=100;rows[7].evictions=20;rows[7].bytes=10000;
        let mut targets=[10000;9];
        transfer(&rows,&[3;9],&mut targets);
        assert_eq!(targets.iter().sum::<usize>(),90000);assert_eq!(targets[0],10000);assert_eq!(targets[7],10500);
        for _ in 0..100 {rows[7].bytes=targets[7];transfer(&rows,&[3;9],&mut targets);}
        assert!(targets.iter().all(|n|*n>=5000 && *n<=20000));assert_eq!(targets.iter().sum::<usize>(),90000);
    }
    #[test]
    fn cold_churn_without_reuse_cannot_take_budget() {
        let mut rows=vec![Observation{baseline:10000,..Default::default()};9];rows[7].misses=1000;rows[7].evictions=900;rows[7].bytes=10000;
        let mut targets=[10000;9];transfer(&rows,&[3;9],&mut targets);assert_eq!(targets,[10000;9]);
    }
}

#[cfg(test)] mod benefit_tests {
 use super::*;
 #[test]fn measured_value_beats_raw_hit_count_and_unknown_is_protected(){
  let mut rows=vec![Observation{baseline:10000,..Default::default()};9];
  rows[7]=Observation{baseline:10000,bytes:10000,hits:1000,measured_hits:1000,saved_ns:1000000,evictions:10,..Default::default()};
  rows[8]=Observation{baseline:10000,bytes:10000,hits:100,measured_hits:100,saved_ns:100000000,evictions:10,..Default::default()};
  let mut targets=[10000;9];let mut idle=[0;9];idle[0]=3;
  transfer(&rows,&idle,&mut targets);assert_eq!(targets[8],10500);assert_eq!(targets[7],10000);
  assert_eq!(targets[1],10000,"unknown benefit alone does not justify donation");
  let mut unknown=rows.clone();unknown[8].measured_hits=0;unknown[8].saved_ns=0;let mut targets=[10000;9];transfer(&unknown,&idle,&mut targets);assert_eq!(targets[8],10000);
 }
}
