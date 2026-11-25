//! Process-local checkpoints for a quality-limited OpenJPEG MQ segment.
use super::Jpeg2000Source;
use std::{
    ffi::c_void,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(PartialEq, Eq, Hash)]
struct Key {
    revision: [u8; 32],
    block: [u32; 8],
}

struct Checkpoint {
    key: Key,
    passes: usize,
    prefix: Box<[u8]>,
    state: Box<[u8]>,
    samples: Box<[i32]>,
    flags: Box<[u32]>,
}

type Slot = Mutex<Option<Checkpoint>>;
// Each slot has a byte ceiling as well as one owner. A collision only costs
// a later miss; no decoder waits for another request to finish a checkpoint.
const SLOTS: usize = 2048;
static CACHE: OnceLock<Box<[Slot]>> = OnceLock::new();
fn slot_budget() -> usize {
    (crate::resources::plan().decoder_reserve_bytes / 32).min(64 << 20) / SLOTS
}

pub(crate) fn trim() {
    let Some(slots)=CACHE.get() else {return};
    let limit=crate::resources::pressure_target(slot_budget());
    for slot in slots.iter() {
        let mut slot=slot.lock().unwrap_or_else(|e|e.into_inner());
        if slot.as_ref().is_some_and(|cp|cp.prefix.len()+cp.state.len()+4*(cp.samples.len()+cp.flags.len())+256>limit) {
            *slot=None;
        }
    }
}

impl Key {
    fn slot(&self) -> &'static Slot {
        let mut hash = DefaultHasher::new();
        self.hash(&mut hash);
        let slots = CACHE.get_or_init(|| (0..SLOTS).map(|_| Mutex::new(None)).collect());
        &slots[hash.finish() as usize % SLOTS]
    }
}
struct Session {
    revision: [u8; 32],
    per_slot: usize,
    complete_hits: AtomicUsize,
    resumed: AtomicUsize,
    passes: AtomicUsize,
}
#[repr(C)]
struct Registration {
    session: *const c_void,
    capture: i32,
    transfer: unsafe extern "C" fn(
        *const c_void,
        *const [u32; 8],
        *const u8,
        usize,
        *mut i32,
        usize,
        *mut u32,
        usize,
        *mut u8,
        usize,
        i32,
    ) -> i32,
}
unsafe extern "C" {
    fn river_opj_resume_session(registration: *const Registration);
}
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { river_opj_resume_session(std::ptr::null()) };
    }
}

// Negative pass counts publish a checkpoint. An empty state publishes final
// coefficients. A lookup returns 0 for a miss, 1 to resume MQ, or 2 when the
// entire segment and pass count match an already-finished block.
unsafe extern "C" fn transfer(
    session: *const c_void,
    block: *const [u32; 8],
    input: *const u8,
    len: usize,
    samples: *mut i32,
    n: usize,
    flags: *mut u32,
    nf: usize,
    state: *mut u8,
    size: usize,
    pass_limit: i32,
) -> i32 {
    let session = unsafe { &*session.cast::<Session>() };
    let key = Key {
        revision: session.revision,
        block: unsafe { *block },
    };
    let mut slot = key.slot().lock().unwrap_or_else(|e| e.into_inner());
    let input = unsafe { std::slice::from_raw_parts(input, len) };
    if pass_limit < 0 {
        // The preview and the arrival decoder can finish in either order.
        if size != 0
            && slot.as_ref().is_some_and(|entry| {
                entry.key == key
                    && entry.state.is_empty()
                    && entry.passes >= pass_limit.unsigned_abs() as usize
                    && entry.prefix.starts_with(input)
            })
        {
            return 0;
        }
        if size + len + 4 * (n + nf) + 256 <= crate::resources::pressure_target(session.per_slot) {
            *slot = Some(Checkpoint {
                key,
                passes: pass_limit.unsigned_abs() as usize,
                prefix: input.into(),
                state: if size == 0 {
                    Box::default()
                } else {
                    unsafe { std::slice::from_raw_parts(state, size) }.into()
                },
                samples: unsafe { std::slice::from_raw_parts(samples, n) }.into(),
                flags: if nf == 0 {
                    Box::default()
                } else {
                    unsafe { std::slice::from_raw_parts(flags, nf) }.into()
                },
            });
        }
        return 0;
    }
    let Some(cp) = slot.as_ref() else { return 0 };
    if cp.key == key
        && cp.state.is_empty()
        && cp.samples.len() == n
        && cp.passes == pass_limit as usize
        && cp.prefix.as_ref() == input
    {
        unsafe { std::ptr::copy_nonoverlapping(cp.samples.as_ptr(), samples, n) };
        session.complete_hits.fetch_add(1, Ordering::Relaxed);
        return 2;
    }
    if cp.key != key
        || cp.passes > pass_limit as usize
        || cp.state.len() != size
        || cp.samples.len() != n
        || cp.flags.len() != nf
        || !input.starts_with(&cp.prefix)
    {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(cp.state.as_ptr(), state, size);
        std::ptr::copy_nonoverlapping(cp.samples.as_ptr(), samples, n);
        std::ptr::copy_nonoverlapping(cp.flags.as_ptr(), flags, nf);
    }
    session.resumed.fetch_add(1, Ordering::Relaxed);
    session.passes.fetch_add(cp.passes, Ordering::Relaxed);
    1
}

// Native decode validates all segment bytes and the pass count on lookup.
pub(crate) fn publish(
    revision: [u8; 32],
    block: [u32; 8],
    passes: u32,
    bytes: Vec<u8>,
    samples: &[i32],
) -> bool {
    let limit = crate::resources::pressure_target(slot_budget());
    if bytes.len() + 4 * samples.len() + 256 > limit {
        return false;
    }
    let key = Key { revision, block };
    let mut slot = key.slot().lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(Checkpoint {
        key,
        passes: passes as usize,
        prefix: bytes.into(),
        state: Box::default(),
        samples: samples.into(),
        flags: Box::default(),
    });
    true
}
pub(super) fn decode<T>(source: &Jpeg2000Source, layers: u32, work: impl FnOnce() -> T) -> T {
    let Some(original) = source.stable_source.as_ref() else {
        return work();
    };
    if original.header.layers < 2
        || original.header.components > 4
        || original.header.is_ht
        || !source.prepared.as_ref().is_some_and(|p| {
            p.route == "s3-packet-ranges"
                && p.technical_origin.is_none()
                && p.tile_offset_x == 0
                && p.tile_offset_y == 0
        })
    {
        return work();
    }
    let capture = layers > 0 && layers < u32::from(original.header.layers);
    if capture {
        original.has_tier1_preview.store(true, Ordering::Relaxed);
    } else if !original.has_tier1_preview.load(Ordering::Relaxed) {
        return work();
    }
    let session = Session {
        revision: original.block_namespace,
        per_slot: slot_budget(),
        complete_hits: 0.into(),
        resumed: 0.into(),
        passes: 0.into(),
    };
    let registration = Registration {
        session: std::ptr::from_ref(&session).cast(),
        capture: i32::from(capture),
        transfer,
    };
    unsafe { river_opj_resume_session(&registration) };
    let _guard = Guard;
    let result = work();
    tracing::debug!(
        complete_hits = session.complete_hits.load(Ordering::Relaxed),
        resumed = session.resumed.load(Ordering::Relaxed),
        skipped_passes = session.passes.load(Ordering::Relaxed),
        capture,
        "Tier-1 continuation"
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_requires_the_consumed_prefix_and_finished_blocks_require_exact_input() {
        let session = Session {
            revision: [0x35; 32],
            per_slot: 2048,
            resumed: 0.into(),
            complete_hits: 0.into(),
            passes: 0.into(),
        };
        let key = [0, 2, 3, 64, 16, 2, 1, 9];
        let call = |bytes: &[u8],
                    samples: &mut [i32; 2],
                    flags: &mut [u32],
                    state: &mut [u8],
                    passes| unsafe {
            transfer(
                std::ptr::from_ref(&session).cast(),
                &key,
                bytes.as_ptr(),
                bytes.len(),
                samples.as_mut_ptr(),
                2,
                flags.as_mut_ptr(),
                flags.len(),
                state.as_mut_ptr(),
                state.len(),
                passes,
            )
        };
        call(&[0xa3, 0x44], &mut [7, -3], &mut [5], &mut [9], -3);
        let mut output = [0, 0];
        for (bytes, passes) in [(&[0xa3, 0x45][..], 4), (&[0xa3], 4), (&[0xa3, 0x44], 2)] {
            assert_eq!(call(bytes, &mut output, &mut [0], &mut [0], passes), 0);
            assert_eq!(output, [0, 0]);
        }
        assert_eq!(
            call(&[0xa3, 0x44, 0xff], &mut output, &mut [0], &mut [0], 8),
            1
        );
        assert_eq!(output, [7, -3]);

        call(&[0xff, 0x80], &mut [51, -39], &mut [], &mut [], -7);
        assert_eq!(
            call(&[0xff, 0x80, 0], &mut output, &mut [0], &mut [0], 8),
            0
        );
        assert_eq!(call(&[0xff, 0x80], &mut output, &mut [0], &mut [0], 8), 0);
        assert_eq!(call(&[0xff, 0x80], &mut output, &mut [0], &mut [0], 7), 2);
        // A concurrent preview must not displace the newly finished segment.
        call(&[0xff], &mut [1, 2], &mut [3], &mut [4], -1);
        assert_eq!(call(&[0xff, 0x80], &mut output, &mut [0], &mut [0], 7), 2);
        assert_eq!(output, [51, -39]);
    }
}
