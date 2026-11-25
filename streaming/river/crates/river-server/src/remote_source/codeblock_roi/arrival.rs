//! Feed complete MQ segments to bounded CPU workers as physical range groups finish.
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

pub(super) struct Word {
    pub span: Range<u64>,
    pub key: [u32; 10],
    pub grid: [u32; 2],
    pub planes: u32,
    pub zeros: u32,
}

// Limit early decode to a single, explicit, unquantized QCD. Overrides, ROI,
// and unknown marker semantics remain the native decoder's responsibility.
pub(in crate::remote_source) fn planes(main: &[u8], tile: &[u8], levels: u8) -> Option<Vec<u32>> {
    let mut planes = None;
    for (tile_header, mut bytes) in [(false, main.strip_prefix(&[0xff, 0x4f])?), (true, tile)] {
        while !bytes.is_empty() {
            let marker = u16::from_be_bytes(bytes.get(..2)?.try_into().ok()?);
            let len = usize::from(u16::from_be_bytes(bytes.get(2..4)?.try_into().ok()?));
            let data = bytes.get(4..len + 2)?;
            match marker {
                0xff5c if !tile_header && planes.is_none() => {
                    let (&style, steps) = data.split_first()?;
                    if style & 31 != 0 || steps.len() != 1 + 3 * usize::from(levels.checked_sub(1)?)
                    {
                        return None;
                    }
                    planes = Some(
                        steps
                            .iter()
                            .map(|step| {
                                (u32::from(style >> 5) + u32::from(step >> 3)).checked_sub(1)
                            })
                            .collect::<Option<Vec<_>>>()?,
                    );
                }
                0xff51 | 0xff52 if !tile_header => (),
                0xff64 => (),
                _ => return None,
            }
            bytes = &bytes[len + 2..];
        }
    }
    planes
}

// Each offset is consumed once, even if a sampled/replanned range is reported twice.
struct Pending {
    size: u64,
    offsets: HashMap<u64, Vec<usize>>,
    words: Vec<(Option<Word>, u64)>,
    blocks: HashMap<u64, Arc<[u8]>>,
}
impl Pending {
    fn new(size: u64, words: Vec<Word>) -> Self {
        let mut offsets = HashMap::<u64, Vec<usize>>::new();
        let words: Vec<_> = words
            .into_iter()
            .enumerate()
            .map(|(i, word)| {
                let first = word.span.start / size;
                let end = word.span.end.div_ceil(size);
                for block in first..end {
                    offsets.entry(block * size).or_default().push(i);
                }
                (Some(word), end - first)
            })
            .collect();
        Self {
            offsets,
            words,
            size,
            blocks: HashMap::new(),
        }
    }
    fn arrive(&mut self, group: Vec<(u64, Arc<[u8]>)>) -> Vec<(Word, Vec<u8>)> {
        let mut ready = Vec::new();
        for (offset, data) in group {
            let Some(waiting) = self.offsets.remove(&offset) else {
                continue;
            };
            self.blocks.insert(offset, data);
            for i in waiting {
                let (word, pending) = &mut self.words[i];
                *pending -= 1;
                if *pending == 0 {
                    let word = word.take().unwrap();
                    let mut bytes = Vec::with_capacity(word.key[9] as usize);
                    if copy_range(&self.blocks, self.size, word.span.clone(), &mut bytes).is_ok() {
                        ready.push((word, bytes));
                    }
                }
            }
        }
        ready
    }
}

#[derive(Default)]
struct Work {
    words: std::collections::VecDeque<(Word, Vec<u8>)>,
    workers: usize,
}

struct Queue {
    work: Mutex<Work>,
    limit: usize,
    source: [u8; 32],
    grok: bool,
    cancel: Option<watch::Receiver<bool>>,
    deadline: Instant,
    count: AtomicUsize,
}

impl Queue {
    fn submit(
        self: &Arc<Self>,
        ready: impl Iterator<Item = (Word, Vec<u8>)>,
        runtime: &tokio::runtime::Handle,
    ) {
        let mut work = self.work.lock().unwrap_or_else(|e| e.into_inner());
        let room = (self.limit * 8).saturating_sub(work.words.len());
        work.words.extend(ready.take(room));
        let needed = (self.limit - work.workers).min(work.words.len());
        for _ in 0..needed {
            let Some(lease) = crate::decoder::thread_budget::try_arrival_worker() else {
                break;
            };
            work.workers += 1;
            let queue = self.clone();
            runtime.spawn_blocking(move || {
                let _lease = lease;
                let mut decoder = crate::decoder::block_reuse::WordDecoder::new();
                loop {
                    let mut work = queue.work.lock().unwrap_or_else(|e| e.into_inner());
                    if decoder.is_none()
                        || queue.cancel.as_ref().is_some_and(|c| *c.borrow())
                        || Instant::now() >= queue.deadline
                    {
                        work.words.clear();
                    }
                    let Some((word, bytes)) = work.words.pop_front() else {
                        work.workers -= 1;
                        break;
                    };
                    drop(work);
                    if decoder.as_mut().unwrap().decode(
                        queue.source,
                        queue.grok,
                        word.key,
                        word.grid,
                        word.planes,
                        word.zeros,
                        &bytes,
                    ) {
                        queue.count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    }
}

pub(super) fn prefetch(
    source: &RemoteSource,
    required: &[(bool, Range<u64>)],
    ctx: &mut ReadContext,
    words: Vec<Word>,
) -> Result<HashMap<u64, Arc<[u8]>>> {
    if words.is_empty() || ctx.ready_blocks.is_some() {
        return source.prefetch_codeblocks(required, ctx);
    }
    let lanes = crate::decoder::thread_budget::arrival_limit(ctx.tier1_threads).min(words.len());
    if lanes == 0 {
        return source.prefetch_codeblocks(required, ctx);
    }
    let queue = Arc::new(Queue {
        work: Mutex::default(),
        limit: lanes,
        source: source.block_namespace,
        grok: ctx.planning_driver_grok.unwrap_or(false),
        cancel: ctx.cancel.clone(),
        deadline: ctx.deadline,
        count: 0.into(),
    });
    let size = source.block_size;
    let done = AtomicBool::new(false);
    // The original read runs on its admitted thread. The dispatcher holds no
    // source locks. Late or saturated lanes leave the codeword to normal decode.
    let (events, arrivals) = mpsc::channel();
    let (result, before_end) = std::thread::scope(|scope| {
        let finished = &done;
        let dispatch = queue.clone();
        let dispatcher = std::thread::Builder::new().spawn_scoped(scope, move || {
            let mut pending = Pending::new(size, words);
            for group in arrivals {
                if finished.load(Ordering::Acquire) {
                    break;
                }
                let ready = pending.arrive(group);
                if !ready.is_empty() {
                    dispatch.submit(ready.into_iter(), &source.runtime);
                }
            }
        });
        if dispatcher.is_err() {
            return (source.prefetch_codeblocks(required, ctx), 0);
        }
        // Ensure the channel closes on unwind too; otherwise the scoped
        // dispatcher could wait for a sender retained inside the read context.
        struct ArrivalContext<'a>(&'a mut ReadContext);
        impl Drop for ArrivalContext<'_> {
            fn drop(&mut self) {
                self.0.ready_blocks.take();
            }
        }
        ctx.ready_blocks = Some(events);
        let guard = ArrivalContext(ctx);
        let result = source.prefetch_codeblocks(required, guard.0);
        let n = queue.count.load(Ordering::Relaxed);
        done.store(true, Ordering::Release);
        (result, n)
    });
    if std::env::var_os("RIVER_PROFILE_SOURCE").is_some() {
        tracing::info!(
            workers = lanes,
            completed_before_read_end = before_end,
            "arrival Tier-1"
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn out_of_order_pages_publish_a_complete_word_once() {
        let mut key = [0; 10];
        key[9] = 5;
        let word = Word {
            span: 2..7,
            key,
            grid: [0, 0],
            planes: 5,
            zeros: 1,
        };
        let mut pending = Pending::new(4, vec![word]);
        let high: Arc<[u8]> = Arc::from([4, 5, 6, 7]);
        assert!(pending
            .arrive(vec![(4, high.clone()), (4, high)])
            .is_empty());
        let low: Arc<[u8]> = Arc::from([0, 1, 2, 3]);
        let words = pending.arrive(vec![(0, low.clone())]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].1, [2, 3, 4, 5, 6]);
        assert!(pending.arrive(vec![(0, low)]).is_empty());
    }
    #[test]
    fn derives_only_unambiguous_reversible_band_planes() {
        let main = [255, 79, 255, 92, 0, 7, 64, 64, 72, 72, 80];
        assert_eq!(planes(&main, &[], 2).unwrap(), [9, 10, 10, 11]);
        for tile in [
            [255, 93, 0, 2],
            [255, 94, 0, 2],
            [255, 92, 0, 2],
            [255, 82, 0, 2],
        ] {
            assert!(planes(&main, &tile, 2).is_none());
        }
        assert!(planes(&main[..8], &[], 2).is_none());
        assert!(planes(&main, &[], 3).is_none());
        let mut quantized = main;
        quantized[6] |= 1;
        assert!(planes(&quantized, &[], 2).is_none());
    }
}
