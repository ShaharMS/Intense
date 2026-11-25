//! Raw Tier-1 coefficients, before ROI adjustment, dequantization or DWT.
//! The immutable object revision identifies blocks across prepared views and source reopenings.
use super::Jpeg2000Source;
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Hash, Eq, PartialEq, Copy, Clone)]
struct Key {
    grok: bool,
    source: [u8; 32],
    block: [u32; 10],
}

struct Entry {
    bitplanes: u32,
    samples: Arc<[i32]>,
    word: Option<Arc<[u8]>>,
}
impl Entry {
    fn cost(&self) -> usize {
        256 + self.samples.len() * 4 + self.word.as_ref().map_or(0, |w| w.len() + 32)
    }
}
#[repr(C)]
struct NativeWord {
    samples: *mut i32,
    bytes: *const u8,
}

#[derive(Default)]
struct Shard {
    blocks: HashMap<Key, Entry>,
    order: VecDeque<Key>,
    bytes: usize,
}

struct Session {
    grok: bool,
    source: [u8; 32],
    shard_limit: usize,
    keep_words: bool,
    hits: AtomicUsize,
    misses: AtomicUsize,
}

const SHARDS: usize = 32;
static CACHE: OnceLock<[Mutex<Shard>; SHARDS]> = OnceLock::new();

impl Key {
    fn shard(&self) -> usize {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hash);
        hash.finish() as usize % SHARDS
    }
}

// Copying the Arc pins the exact original body through serialization, even if
// another request evicts the decoded entry or the decoder later changes path.
pub(crate) fn planned_word(
    source: [u8; 32],
    grok: bool,
    block: [u32; 10],
    grid: [u32; 2],
) -> Option<Arc<[u8]>> {
    let cache = CACHE.get()?;
    let lookup = |block| {
        let key = Key {
            source,
            grok,
            block,
        };
        cache[key.shard()]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .blocks
            .get(&key)?
            .word
            .clone()
    };
    if grok {
        let mut mercury = block;
        mercury[0] |= 1 << 31;
        mercury[3] = grid[0];
        mercury[4] = grid[1];
        if let Some(word) = lookup(mercury) {
            return Some(word);
        }
    }
    lookup(block)
}

pub(super) type Callback =
    unsafe extern "C" fn(*const std::ffi::c_void, *const u32, *mut i32, usize, i32) -> i32;
unsafe extern "C" {
    fn river_opj_block_session_v2(session: *const std::ffi::c_void, callback: Option<Callback>);
}

unsafe extern "C" {
    fn opj_t1_create(encoder: i32) -> *mut std::ffi::c_void;
    fn opj_t1_destroy(t1: *mut std::ffi::c_void);
    fn river_opj_t1_word(
        t1: *mut std::ffi::c_void,
        word: *const u8,
        len: u32,
        passes: u32,
        planes: u32,
        orient: u32,
        w: u32,
        h: u32,
    ) -> *mut i32;
}

// Reuse the aligned MQ, flag and coefficient buffers for all words on a worker.
pub(crate) struct WordDecoder(std::ptr::NonNull<std::ffi::c_void>);
impl Drop for WordDecoder {
    fn drop(&mut self) {
        unsafe { opj_t1_destroy(self.0.as_ptr()) }
    }
}
impl WordDecoder {
    pub(crate) fn new() -> Option<Self> {
        std::ptr::NonNull::new(unsafe { opj_t1_create(0) }).map(Self)
    }
    pub(crate) fn coefficients(
        &mut self,
        key: &[u32; 8],
        passes: u32,
        bytes: &[u8],
    ) -> Option<&mut [i32]> {
        let len = u32::try_from(bytes.len()).ok()?;
        let ptr = unsafe {
            river_opj_t1_word(
                self.0.as_ptr(),
                bytes.as_ptr(),
                len,
                passes,
                key[7],
                key[2],
                key[5],
                key[6],
            )
        };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts_mut(ptr, (key[5] * key[6]) as usize) })
    }
    pub(crate) fn decode(
        &mut self,
        source: [u8; 32],
        grok: bool,
        mut key: [u32; 10],
        grid: [u32; 2],
        band_planes: u32,
        zeros: u32,
        word: &[u8],
    ) -> bool {
        let Some(planes) = band_planes.checked_sub(zeros) else {
            return false;
        };
        let n = u64::from(key[5]) * u64::from(key[6]);
        if !(1..=4096).contains(&n)
            || !(1..=30).contains(&band_planes)
            || word.len() != key[9] as usize
        {
            return false;
        }
        key[7] = planes;
        let Some(samples) = self.coefficients(key.first_chunk::<8>().unwrap(), key[8], word) else {
            return false;
        };
        if grok {
            // Mercury's reversible Tier-1 ABI uses sign-magnitude aligned to Kmax.
            for sample in samples.iter_mut() {
                let sign = (*sample as u32) & 0x8000_0000;
                *sample = (sign | (sample.unsigned_abs() << (30 - band_planes))) as i32;
            }
            key[0] |= 1 << 31;
            key[3..5].copy_from_slice(&grid);
            key[7] = (band_planes << 16) | zeros;
        }
        let session = Session {
            grok,
            source,
            keep_words: true,
            shard_limit: shard_limit(),
            hits: 0.into(),
            misses: 0.into(),
        };
        let mut pair = NativeWord {
            samples: samples.as_mut_ptr(),
            bytes: word.as_ptr(),
        };
        unsafe {
            access(
                (&session as *const Session).cast(),
                key.as_ptr(),
                (&mut pair as *mut NativeWord).cast(),
                n as usize,
                2,
            );
        }
        true
    }
}

impl Shard {
    fn trim_to(&mut self, target: usize) {
        while self.bytes > target {
            let Some(old) = self.order.pop_front() else {break};
            if let Some(value) = self.blocks.remove(&old) { self.bytes -= value.cost(); }
        }
    }
}

pub(crate) fn trim() {
    let Some(cache)=CACHE.get() else {return};
    let limit=crate::resources::pressure_target(shard_limit());
    for shard in cache {
        shard.lock().unwrap_or_else(|e|e.into_inner()).trim_to(limit);
    }
}

fn shard_limit() -> usize {
    (crate::resources::plan().decoder_reserve_bytes / 16).min(256 << 20) / SHARDS
}

unsafe extern "C" fn access(
    session: *const std::ffi::c_void,
    key: *const u32,
    samples: *mut i32,
    n: usize,
    store: i32,
) -> i32 {
    let session = unsafe { &*session.cast::<Session>() };
    let mut block = unsafe { *(key.cast::<[u32; 10]>()) };
    let bitplanes = std::mem::take(&mut block[7]);
    let key = Key {
        grok: session.grok,
        source: session.source,
        block,
    };
    let word_len = if store == 2 && session.keep_words {
        block[9] as usize
    } else {
        0
    };
    let cost = n * 4 + 256 + if word_len == 0 { 0 } else { word_len + 32 };
    if cost > session.shard_limit {
        return 0;
    }
    let cache = CACHE.get_or_init(|| std::array::from_fn(|_| Mutex::default()));
    let mut shard = cache[key.shard()].lock().unwrap_or_else(|e| e.into_inner());
    if store < 0 {
        let present = shard
            .blocks
            .get(&key)
            .is_some_and(|v| v.bitplanes == bitplanes);
        if !present {
            session.misses.fetch_add(1, Ordering::Relaxed);
        }
        return i32::from(present);
    }
    if store == 0 {
        let found = shard
            .blocks
            .get(&key)
            .filter(|e| e.bitplanes == bitplanes)
            .map(|e| e.samples.clone());
        drop(shard);
        if let Some(value) = found.filter(|v| v.len() == n) {
            unsafe {
                std::ptr::copy_nonoverlapping(value.as_ptr(), samples, n);
            }
            session.hits.fetch_add(1, Ordering::Relaxed);
            return 1;
        }
        session.misses.fetch_add(1, Ordering::Relaxed);
    } else if !shard.blocks.contains_key(&key) {
        let limit=crate::resources::pressure_target(session.shard_limit);
        if cost>limit {return 0}
        shard.trim_to(limit-cost);
        let (planes, word) = if store == 2 {
            let pair = unsafe { &*samples.cast::<NativeWord>() };
            let word = (word_len != 0)
                .then(|| Arc::from(unsafe { std::slice::from_raw_parts(pair.bytes, word_len) }));
            (pair.samples, word)
        } else {
            (samples, None)
        };
        let samples = Arc::from(unsafe { std::slice::from_raw_parts(planes, n) });
        shard.blocks.insert(
            key,
            Entry {
                bitplanes,
                samples,
                word,
            },
        );
        shard.order.push_back(key);
        shard.bytes += cost;
    }
    0
}

pub(super) type SetSession = unsafe extern "C" fn(*const std::ffi::c_void, Option<Callback>);
pub(super) struct Scope {
    session: Box<Session>,
    setter: SetSession,
    // Registration belongs to this submitting thread.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Scope {
    pub(super) fn counts(&self) -> (usize, usize) {
        (
            self.session.hits.load(Ordering::Relaxed),
            self.session.misses.load(Ordering::Relaxed),
        )
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        unsafe { (self.setter)(std::ptr::null(), None) };
    }
}
pub(super) fn enter(
    source: &Jpeg2000Source,
    layers: u32,
    grok: bool,
    setter: SetSession,
) -> Option<Scope> {
    let original = source.stable_source.as_ref()?;
    let h = &original.header;
    if !source.prepared.as_ref().is_some_and(|p| {
        matches!(
            p.route,
            "s3-codeblock-ranges"
                | "s3-packet-ranges"
                | "s3-reduced-resolution-view"
                | "s3-original-ranges"
        ) && p.tile_offset_x == 0
            && p.tile_offset_y == 0
            && p.technical_origin.is_none()
    }) || h.layout.columns != 1
        || h.layout.rows != 1
        || h.image_origin != (0, 0)
        || h.is_ht
        || (layers > 0 && layers < u32::from(h.layers))
    {
        return None;
    }
    let session = Box::new(Session {
        grok,
        source: original.block_namespace,
        hits: 0.into(),
        misses: 0.into(),
        keep_words: h.single_codeword_layout(),
        shard_limit: shard_limit(),
    });
    unsafe {
        setter((&*session as *const Session).cast(), Some(access));
    }
    Some(Scope {
        session,
        setter,
        _thread: std::marker::PhantomData,
    })
}

pub(super) fn decode<T>(
    source: &Jpeg2000Source,
    layers: u32,
    work: impl FnOnce() -> T,
) -> (T, Option<(usize, usize)>) {
    let scope = enter(source, layers, false, river_opj_block_session_v2);
    let result = super::tier1_resume::decode(source, layers, work);
    (result, scope.as_ref().map(Scope::counts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_coefficients_are_scoped_to_revision_driver_and_coding_passes() {
        let session = |source, grok| Session {
            source: [source; 32],
            grok,
            shard_limit: 16 * 1024,
            keep_words: true,
            hits: 0.into(),
            misses: 0.into(),
        };
        let first = session(11, false);
        let key = [0, 1, 3, 23, 47, 2, 2, 8, 21, 77];
        let transfer = |s: &Session, key: &[u32; 10], samples: &mut [i32; 4], store| unsafe {
            access(
                (s as *const Session).cast(),
                key.as_ptr(),
                samples.as_mut_ptr(),
                4,
                store,
            )
        };
        let values = [i32::MIN, -1, 0, i32::MAX];
        transfer(&first, &key, &mut values.clone(), 1);
        let mut out = [0; 4];
        assert_eq!(transfer(&first, &key, &mut out, 0), 1);
        assert_eq!(out, values);
        for different in [session(12, false), session(11, true)] {
            assert_eq!(transfer(&different, &key, &mut out, 0), 0);
        }
        let mut refinement = key;
        refinement[8] += 1;
        assert_eq!(transfer(&first, &refinement, &mut out, 0), 0);
    }
    #[test]
    fn planner_pins_original_bytes_and_decoder_checks_bitplanes() {
        let s = Session {
            grok: false,
            source: [38; 32],
            shard_limit: 16384,
            keep_words: true,
            hits: 0.into(),
            misses: 0.into(),
        };
        let block = [0, 1, 3, 23, 47, 2, 2, 8, 21, 3];
        let mut coeffs = [17i32; 4];
        let bytes = [0x8a, 0, 0xff];
        let mut pair = NativeWord {
            samples: coeffs.as_mut_ptr(),
            bytes: bytes.as_ptr(),
        };
        let ptr = (&s as *const Session).cast();
        unsafe {
            access(
                ptr,
                block.as_ptr(),
                (&mut pair as *mut NativeWord).cast(),
                4,
                2,
            );
        }
        let mut spatial = block;
        spatial[7] = 0;
        let word = planned_word(s.source, false, spatial, [0, 0]).unwrap();
        assert!(planned_word(s.source, true, spatial, [0, 0]).is_none());
        assert!(planned_word([39; 32], false, spatial, [0, 0]).is_none());
        assert_eq!(
            unsafe { access(ptr, spatial.as_ptr(), coeffs.as_mut_ptr(), 4, 0) },
            0
        );
        let key = Key {
            grok: false,
            source: s.source,
            block: spatial,
        };
        let mut shard = CACHE.get().unwrap()[key.shard()].lock().unwrap();
        shard.bytes -= shard.blocks.remove(&key).unwrap().cost();
        shard.order.retain(|k| *k != key);
        drop(shard);
        assert!(planned_word(s.source, false, spatial, [0, 0]).is_none());
        assert_eq!(&*word, &bytes);
    }
}
