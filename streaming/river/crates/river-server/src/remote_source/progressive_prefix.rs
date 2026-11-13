//! Bounded read-ahead for a final pass already scheduled by the same viewport.
//! It only changes cached source bytes; current-pass packet selection is intact.
use super::*;

pub(super) fn target(levels:u8,current:u8,future:Option<u8>)->Option<u64> {
    let future=future?;let last=levels.checked_sub(1)?;
    let wanted=last.checked_sub(current)?;let final_wanted=last.checked_sub(future)?;
    if future==0 || future>=current || wanted!=0 || final_wanted>2 {return None;}
    // A bounded header allowance; arbitrary longer headers still use ordinary
    // validated parsing and required reads, never assumed payload offsets.
    Some(prefix_read_ahead(final_wanted)+BLOCK)
}

fn ready(settings:&crate::read_policy::Settings,o:&crate::read_policy::Observation,cap:Option<u64>,cache_fits:bool)->bool {
    cache_fits && !o.prefix_lookahead_backoff && settings.mode==crate::read_policy::Mode::Adaptive
        && u64::from(settings.max_overfetch_ratio).min(cap.unwrap_or(16))>1
        && o.samples>=u64::from(settings.minimum_samples) && o.body_throughput_samples>=3
}

fn affordable(extra:u64,remaining:u64,rate:Option<f64>,latency:u64,lanes:usize)->bool {
    if extra==0 || extra>8*1024*1024 || extra>remaining/4 || !(5000..u64::MAX).contains(&latency) {return false;}
    let Some(rate)=rate.filter(|r|r.is_finite() && *r>0.0) else{return false;};
    let aggregate=rate*lanes.clamp(1,8) as f64;
    aggregate.is_finite() && extra as f64/aggregate*1000.0<=50.0
}

impl RemoteSource {
    pub(super) fn progressive_probe(&self,base:u64,native:u64,context:&mut ReadContext)->u64 {
        let Some(target)=context.progressive_prefix_probe.filter(|p|*p>base) else{return base;};
        if std::env::var("RIVER_S3_PROGRESSIVE_PREFIX_LOOKAHEAD").is_ok_and(|v|v=="false" || v=="0")
            || std::env::var_os("RIVER_S3_COLD_PREFIX_PROBE_BYTES").is_some() {return base;}
        let policy=crate::read_policy::settings();let observation=self.read_profile.snapshot();
        let cache_fits = self.cache.limit as u64 >= context.limit;
        let mut readiness = observation.clone();
        // While backing off, occasionally sample one real lookahead range.
        // Ordinary short probes cannot clear a stall recorded on a larger body.
        let recovery_probe = observation.prefix_lookahead_backoff
            && context.trace_index.is_some_and(|index| index % 16 == 15 && Some(index) != context.last_prefix_recovery_index)
            && !std::env::var("RIVER_PREFIX_RECOVERY_PROBE").is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"));
        if recovery_probe {
            readiness.prefix_lookahead_backoff = false;
        }
        let ready = ready(&policy, &readiness, crate::read_policy::explicit_overfetch_cap(), cache_fits);
        let extra=native.saturating_mul(target.saturating_sub(base).saturating_add(2*self.block_size));
        // Body observations are per range. Share the source's lane allowance
        // across active preparations before estimating aggregate transfer cost.
        let lanes=(policy.effective_source_requests()/observation.active_preparations.max(1) as usize).clamp(1,8);
        let remaining = context.limit.saturating_sub(context.stats.bytes);
        let rate = observation.planning_bytes_per_second();
        let latency = self.discovery_latency_us();
        let accepted = ready && affordable(extra, remaining, rate, latency, lanes);
        if accepted && recovery_probe {
            context.last_prefix_recovery_index = context.trace_index;
        }
        if accepted {target} else {base}
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn only_a_scheduled_finer_low_resolution_final_can_supply_a_hint() {
        assert_eq!(target(7,6,Some(5)),Some(9216));
        assert_eq!(target(7,6,Some(4)),Some(9216));
        for future in [None,Some(6),Some(7),Some(3),Some(0)] {assert_eq!(target(7,6,future),None);}
        assert_eq!(target(7,5,Some(4)),None);assert_eq!(target(0,0,Some(0)),None);
    }
    #[test] fn distant_fast_sources_can_prefetch_but_slow_or_nearby_sources_do_not() {
        let extra=451*9216;
        assert!(affordable(extra,48*1024*1024,Some(80_000_000.0),16000,4));
        assert!(!affordable(extra,48*1024*1024,Some(1_000_000.0),16000,4));
        assert!(!affordable(extra,48*1024*1024,Some(80_000_000.0),300,4));
        assert!(!affordable(extra,48*1024*1024,None,16000,4));
    }
    #[test] fn byte_memory_and_unknown_timing_bounds_are_not_relaxed() {
        assert!(!affordable(9*1024*1024,64*1024*1024,Some(300_000_000.0),16000,8));
        assert!(!affordable(4*1024*1024,8*1024*1024,Some(300_000_000.0),16000,8));
        assert!(!affordable(4*1024*1024,64*1024*1024,Some(300_000_000.0),u64::MAX,8));
        assert!(!affordable(4*1024*1024,64*1024*1024,Some(f64::NAN),16000,8));
    }
    #[test] fn observed_slowdown_and_contended_lane_allowance_back_off() {
        let mut o=crate::read_policy::Observation::default();o.body_throughput_samples=3;o.body_bytes_per_second=Some(80_000_000.0);o.last_body_bytes_per_second=Some(80_000_000.0);
        assert!(affordable(4*1024*1024,48*1024*1024,o.planning_bytes_per_second(),16000,4));
        assert!(!affordable(4*1024*1024,48*1024*1024,o.planning_bytes_per_second(),16000,1));
        o.last_body_bytes_per_second=Some(1_000_000.0);assert!(!affordable(4*1024*1024,48*1024*1024,o.planning_bytes_per_second(),16000,4));
    }
    #[test] fn policy_unknown_measurements_and_cache_limits_preserve_baseline() {
        let mut p=crate::read_policy::Settings::default();p.mode=crate::read_policy::Mode::Adaptive;p.minimum_samples=8;
        let mut o=crate::read_policy::Observation::default();assert!(!ready(&p,&o,None,true));
        o.samples=8;o.body_throughput_samples=3;assert!(ready(&p,&o,None,true));
        o.prefix_lookahead_backoff=true;assert!(!ready(&p,&o,None,true));o.prefix_lookahead_backoff=false;
        assert!(!ready(&p,&o,Some(1),true));assert!(!ready(&p,&o,None,false));
        p.max_overfetch_ratio=1;assert!(!ready(&p,&o,None,true));p.max_overfetch_ratio=2;
        p.mode=crate::read_policy::Mode::Observe;assert!(!ready(&p,&o,None,true));
    }
}
