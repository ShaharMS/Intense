//! Request-local details collected only while a WMTS blocking worker is running.
use std::{cell::RefCell, fmt::Write, time::Instant};

#[derive(Debug, Default)]
struct Stage {
    name: &'static str,
    ms: f64,
    gets: u64,
    bytes: u64,
    cache_bytes: u64,
    curl: u64,
    cpu: Option<(f64, f64)>,
}

#[derive(Debug, Default)]
pub(crate) struct Details {
    stages: Vec<Stage>,
    pub prefix: Option<String>,
    index: Vec<&'static str>,
}
thread_local! {
    static ACTIVE: RefCell<Option<Details>> = const { RefCell::new(None) };
}

impl Details {
    pub fn capture<T>(enabled: bool, work: impl FnOnce() -> T) -> (T, Self) {
        if !enabled {
            return (work(), Self::default());
        }
        // Restore the caller's scope on nested calls and on worker panics.
        struct Restore(Option<Details>);
        impl Drop for Restore {
            fn drop(&mut self) {
                ACTIVE.set(self.0.take());
            }
        }
        let _restore = Restore(ACTIVE.replace(Some(Self::default())));
        let result = work();
        (result, ACTIVE.take().unwrap())
    }

    pub fn append_headers(
        &self,
        stages: &mut String,
        sources: &mut String,
        indexes: &mut Vec<&'static str>,
    ) {
        for stage in &self.stages {
            let Stage {
                name,
                ms,
                gets,
                bytes,
                cache_bytes,
                curl,
                cpu,
            } = stage;
            write!(stages, ", {name};dur={ms:.3}").unwrap();
            if name.starts_with("source_") {
                if !sources.is_empty() {
                    sources.push_str(", ");
                }
                write!(sources, "{name};gets={gets};bytes={bytes};cache_bytes={cache_bytes};curl_requests={curl}").unwrap();
                if let Some((caller, group)) = cpu {
                    write!(
                        sources,
                        ";caller_cpu_ms={caller:.3};cgroup_cpu_ms={group:.3}"
                    )
                    .unwrap();
                }
            }
        }
        for &index in &self.index {
            if !indexes.contains(&index) {
                indexes.push(index);
            }
        }
    }
}

// Store only the latest eligible plan. Phase totals still include all attempts.
pub(super) fn prefix(message: String) {
    ACTIVE.with_borrow_mut(|slot| {
        if let Some(details) = slot {
            details.prefix = Some(message);
        }
    });
}

pub fn active() -> bool {
    ACTIVE.with_borrow(Option::is_some)
}

pub fn source(
    name: &'static str,
    ms: f64,
    io: crate::remote_source::ReadStats,
    curl: u64,
    cpu: Option<(f64, f64)>,
) {
    record(Stage {
        name,
        ms,
        gets: io.requests,
        bytes: io.bytes,
        cache_bytes: io.cache_bytes,
        curl,
        cpu,
    });
}

fn record(sample: Stage) {
    ACTIVE.with_borrow_mut(|slot| {
        if let Some(details) = slot {
            if let Some(stage) = details.stages.iter_mut().find(|s| s.name == sample.name) {
                stage.ms += sample.ms;
                stage.gets += sample.gets;
                stage.bytes += sample.bytes;
                stage.cache_bytes += sample.cache_bytes;
                stage.curl += sample.curl;
                if let (Some((caller, group)), Some((dc, dg))) = (&mut stage.cpu, sample.cpu) {
                    // A missing counter is reported as -1 in the source profiler.
                    *caller = if *caller < 0.0 || dc < 0.0 {
                        -1.0
                    } else {
                        *caller + dc
                    };
                    *group = if *group < 0.0 || dg < 0.0 {
                        -1.0
                    } else {
                        *group + dg
                    };
                }
            } else {
                details.stages.push(sample);
            }
        }
    });
}

pub fn index(name: &'static str) {
    ACTIVE.with_borrow_mut(|slot| {
        if let Some(details) = slot {
            if !details.index.contains(&name) {
                details.index.push(name);
            }
        }
    });
}

pub fn measure<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    if !active() {
        return work();
    }
    let start = Instant::now();
    let result = work();
    record(Stage {
        name,
        ms: start.elapsed().as_secs_f64() * 1000.0,
        ..Stage::default()
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_aggregate_and_do_not_leak_to_the_next_request() {
        let (_, details) = Details::capture(true, || {
            for _ in 0..2 {
                source(
                    "source_index",
                    2.0,
                    crate::remote_source::ReadStats {
                        bytes: 512,
                        requests: 1,
                        cache_bytes: 0,
                    },
                    1,
                    None,
                );
                index("cold-sot");
            }
            let (_, nested) = Details::capture(true, || index("tlm"));
            assert_eq!(nested.index, ["tlm"]);
        });
        assert!(!active());
        assert_eq!(details.index, ["cold-sot"]);
        assert_eq!(details.stages.len(), 1);
        let mut timing = String::new();
        let mut io = String::new();
        details.append_headers(&mut timing, &mut io, &mut Vec::new());
        assert!(timing.contains("source_index;dur=4.000"));
        assert!(io.contains("gets=2;bytes=1024"));
        let (_, quiet) = Details::capture(false, || index("cold-sot"));
        assert!(quiet.index.is_empty());
    }
}
