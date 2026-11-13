//! Fine ROI CUDA decoding on persistent Rust threads. Compressed native tile
//! views retain original entropy bytes; no decoded raster cache is used.
use crate::{
    decoder::{DecodedRaster, DecodedTile, elapsed_ms, encode_crop_rgba},
    engine::TileDecodeRequest,
};
use anyhow::{Context as _, Result, bail, ensure};
use libloading::Library;
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    ffi::c_void,
    fs::File,
    os::unix::fs::{FileExt, MetadataExt},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Instant,
};
use tracing::info_span;
type P = *mut c_void;
#[repr(C)]
struct Image {
    pixels: *mut P,
    pitches: *mut usize,
    pixel_type: i32,
    components: u32,
}
#[repr(C)]
struct AllocFns {
    allocate: unsafe extern "C" fn(P, *mut P, usize, P) -> i32,
    free: unsafe extern "C" fn(P, P, usize, P) -> i32,
    context: P,
}
struct Api {
    _nv: Library,
    _cu: Library,
    create: unsafe extern "C" fn(*mut P) -> i32,
    destroy: unsafe extern "C" fn(P) -> i32,
    create_v2: unsafe extern "C" fn(i32, *mut AllocFns, *mut AllocFns, *mut P) -> i32,
    state_create: unsafe extern "C" fn(P, *mut P) -> i32,
    state_destroy: unsafe extern "C" fn(P) -> i32,
    stream_create: unsafe extern "C" fn(*mut P) -> i32,
    stream_destroy: unsafe extern "C" fn(P) -> i32,
    parse: unsafe extern "C" fn(P, *const u8, usize, i32, i32, P) -> i32,
    params_create: unsafe extern "C" fn(*mut P) -> i32,
    params_destroy: unsafe extern "C" fn(P) -> i32,
    rgb: unsafe extern "C" fn(P, i32) -> i32,
    area: unsafe extern "C" fn(P, u32, u32, u32, u32) -> i32,
    tile: unsafe extern "C" fn(P, P, P, P, u32, u32, *mut Image, P) -> i32,
    device: unsafe extern "C" fn(i32) -> i32,
    cuda_create: unsafe extern "C" fn(*mut P, u32) -> i32,
    flags: unsafe extern "C" fn(u32) -> i32,
    cuda_destroy: unsafe extern "C" fn(P) -> i32,
    sync: unsafe extern "C" fn(P) -> i32,
    malloc: unsafe extern "C" fn(*mut P, usize) -> i32,
    free: unsafe extern "C" fn(P) -> i32,
    host_alloc: unsafe extern "C" fn(*mut P, usize, u32) -> i32,
    host_free: unsafe extern "C" fn(P) -> i32,
    copy: unsafe extern "C" fn(P, P, usize, i32, P) -> i32,
    malloc_async: unsafe extern "C" fn(*mut P, usize, P) -> i32,
    free_async: unsafe extern "C" fn(P, P) -> i32,
    pool: unsafe extern "C" fn(*mut P, i32) -> i32,
    pool_attribute: unsafe extern "C" fn(P, i32, P) -> i32,
    event_create: unsafe extern "C" fn(*mut P, u32) -> i32,
    event_record: unsafe extern "C" fn(P, P) -> i32,
    event_query: unsafe extern "C" fn(P) -> i32,
    event_sync: unsafe extern "C" fn(P) -> i32,
    event_destroy: unsafe extern "C" fn(P) -> i32,
}
fn check(code: i32, what: &str) -> Result<()> {
    ensure!(code == 0, "{what}: CUDA/nvJPEG2000 status {code}");
    Ok(())
}
impl Api {
    fn load() -> Result<Arc<Self>> {
        let nv = unsafe { Library::new("/usr/local/lib/libnvjpeg2k.so.0")? };
        let cu = unsafe { Library::new("/usr/local/lib/libcudart.so.13")? };
        macro_rules! sym {
            ($lib:ident,$name:literal) => {
                unsafe { *$lib.get(concat!($name, "\0").as_bytes())? }
            };
        }
        let api = Arc::new(Self {
            create: sym!(nv, "nvjpeg2kCreateSimple"),
            create_v2: sym!(nv, "nvjpeg2kCreateV2"),
            destroy: sym!(nv, "nvjpeg2kDestroy"),
            state_create: sym!(nv, "nvjpeg2kDecodeStateCreate"),
            state_destroy: sym!(nv, "nvjpeg2kDecodeStateDestroy"),
            stream_create: sym!(nv, "nvjpeg2kStreamCreate"),
            stream_destroy: sym!(nv, "nvjpeg2kStreamDestroy"),
            parse: sym!(nv, "nvjpeg2kStreamParse"),
            params_create: sym!(nv, "nvjpeg2kDecodeParamsCreate"),
            params_destroy: sym!(nv, "nvjpeg2kDecodeParamsDestroy"),
            rgb: sym!(nv, "nvjpeg2kDecodeParamsSetRGBOutput"),
            area: sym!(nv, "nvjpeg2kDecodeParamsSetDecodeArea"),
            tile: sym!(nv, "nvjpeg2kDecodeTile"),
            device: sym!(cu, "cudaSetDevice"),
            flags: sym!(cu, "cudaSetDeviceFlags"),
            cuda_create: sym!(cu, "cudaStreamCreateWithFlags"),
            cuda_destroy: sym!(cu, "cudaStreamDestroy"),
            sync: sym!(cu, "cudaStreamSynchronize"),
            malloc: sym!(cu, "cudaMalloc"),
            free: sym!(cu, "cudaFree"),
            host_alloc: sym!(cu, "cudaHostAlloc"),
            host_free: sym!(cu, "cudaFreeHost"),
            copy: sym!(cu, "cudaMemcpyAsync"),
            malloc_async: sym!(cu, "cudaMallocAsync"),
            free_async: sym!(cu, "cudaFreeAsync"),
            pool: sym!(cu, "cudaDeviceGetDefaultMemPool"),
            pool_attribute: sym!(cu, "cudaMemPoolSetAttribute"),
            event_create: sym!(cu, "cudaEventCreateWithFlags"),
            event_record: sym!(cu, "cudaEventRecord"),
            event_query: sym!(cu, "cudaEventQuery"),
            event_sync: sym!(cu, "cudaEventSynchronize"),
            event_destroy: sym!(cu, "cudaEventDestroy"),
            _nv: nv,
            _cu: cu,
        });
        if std::env::var("RIVER_NVJPEG2000_NATIVE_BLOCKING_SYNC").as_deref() != Ok("false") {
            unsafe {
                check((api.flags)(4), "blocking GPU synchronization")?;
            }
        }
        if std::env::var("RIVER_NVJPEG2000_NATIVE_ASYNC_ALLOCATOR").as_deref() != Ok("false") {
            let mut pool = std::ptr::null_mut();
            let mut threshold = 536870912u64;
            unsafe {
                check((api.pool)(&mut pool, 0), "CUDA memory pool")?;
                check(
                    (api.pool_attribute)(pool, 4, &mut threshold as *mut u64 as P),
                    "retain CUDA allocation pool",
                )?;
            }
        }
        Ok(api)
    }
}
struct Pinned {
    pointer: usize,
    size: usize,
    event: usize,
}
struct AllocCtx {
    api: Arc<Api>,
    pinned: Mutex<Vec<Pinned>>,
    sizes: Mutex<HashMap<usize, usize>>,
}
unsafe extern "C" fn device_allocate(ctx: P, pointer: *mut P, size: usize, stream: P) -> i32 {
    unsafe {
        let ctx = &*(ctx as *const AllocCtx);
        (ctx.api.malloc_async)(pointer, size, stream)
    }
}
unsafe extern "C" fn device_free(ctx: P, pointer: P, _size: usize, stream: P) -> i32 {
    unsafe {
        let ctx = &*(ctx as *const AllocCtx);
        (ctx.api.free_async)(pointer, stream)
    }
}
unsafe extern "C" fn pinned_allocate(ctx: P, pointer: *mut P, size: usize, _stream: P) -> i32 {
    unsafe {
        let ctx = &*(ctx as *const AllocCtx);
        if size == 0 {
            *pointer = std::ptr::null_mut();
            return 0;
        }
        let reused = {
            let mut cache = match ctx.pinned.lock() {
                Ok(cache) => cache,
                Err(_) => return 1,
            };
            let found = cache
                .iter()
                .position(|item| item.size >= size && (ctx.api.event_query)(item.event as P) == 0);
            found.map(|i| cache.swap_remove(i))
        };
        if let Some(item) = reused {
            (ctx.api.event_destroy)(item.event as P);
            *pointer = item.pointer as P;
            return 0;
        }
        let capacity = match size.checked_next_power_of_two() {
            Some(capacity) => capacity.max(4096),
            None => return 1,
        };
        if capacity > 134217728 {
            return 1;
        }
        let code = (ctx.api.host_alloc)(pointer, capacity, 0);
        if code == 0 {
            match ctx.sizes.lock() {
                Ok(mut sizes) => {
                    sizes.insert(*pointer as usize, capacity);
                }
                Err(_) => return 1,
            }
        }
        code
    }
}
unsafe extern "C" fn pinned_free(ctx: P, pointer: P, _size: usize, stream: P) -> i32 {
    unsafe {
        let ctx = &*(ctx as *const AllocCtx);
        if pointer.is_null() {
            return 0;
        }
        let mut event = std::ptr::null_mut();
        let code = (ctx.api.event_create)(&mut event, 2);
        if code != 0 {
            return code;
        }
        let code = (ctx.api.event_record)(event, stream);
        if code != 0 {
            (ctx.api.event_destroy)(event);
            return code;
        }
        let capacity = match ctx.sizes.lock() {
            Ok(sizes) => match sizes.get(&(pointer as usize)) {
                Some(size) => *size,
                None => return 1,
            },
            Err(_) => return 1,
        };
        {
            let mut cache = match ctx.pinned.lock() {
                Ok(cache) => cache,
                Err(_) => return 1,
            };
            if cache.iter().map(|p| p.size).sum::<usize>() + capacity <= 16777216 {
                cache.push(Pinned {
                    pointer: pointer as usize,
                    size: capacity,
                    event: event as usize,
                });
                return 0;
            }
        }
        let code = (ctx.api.event_sync)(event);
        (ctx.api.event_destroy)(event);
        if code != 0 {
            return code;
        }
        let code = (ctx.api.host_free)(pointer);
        if code == 0 {
            if let Ok(mut sizes) = ctx.sizes.lock() {
                sizes.remove(&(pointer as usize));
            }
        }
        code
    }
}
impl AllocCtx {
    fn drain(&self) {
        let entries = std::mem::take(&mut *self.pinned.lock().unwrap());
        for item in entries {
            unsafe {
                (self.api.event_sync)(item.event as P);
                (self.api.event_destroy)(item.event as P);
                (self.api.host_free)(item.pointer as P);
            }
        }
        self.sizes.lock().unwrap().clear();
    }
}
#[derive(Clone, PartialEq, Eq, Debug)]
struct Key(PathBuf, u64, u64, u64, i64, i64, i64, i64);
fn key(path: &PathBuf) -> Result<Key> {
    let m = path.metadata()?;
    Ok(Key(
        path.clone(),
        m.len(),
        m.dev(),
        m.ino(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    ))
}
fn be16(b: &[u8], i: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        b.get(i..i + 2)
            .context("short JPEG2000 header")?
            .try_into()?,
    ))
}
fn be32(b: &[u8], i: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        b.get(i..i + 4)
            .context("short JPEG2000 header")?
            .try_into()?,
    ))
}
struct Indexed {
    key: Key,
    file: File,
    header: Vec<u8>,
    siz: usize,
    width: u32,
    height: u32,
    tw: u32,
    th: u32,
    columns: u32,
    levels: u32,
    precision: Vec<u8>,
    lut: Vec<Vec<u8>>,
    parts: Vec<(u64, usize)>,
    compressed: Mutex<VecDeque<(u32, Arc<Vec<u8>>)>>,
    ht_eligible: bool,
    ht_keys: Mutex<HashMap<u32, String>>,
}
impl Indexed {
    fn open(k: Key) -> Result<Self> {
        let file = File::open(&k.0)?;
        let mut prefix = vec![0; 65536.min(k.1 as usize)];
        file.read_exact_at(&mut prefix, 0)?;
        let mut start = 0;
        let mut colour = None;
        let boxed = prefix.get(..2) != Some(&[0xff, 0x4f]);
        if prefix.get(..2) != Some(&[0xff, 0x4f]) {
            loop {
                let size = be32(&prefix, start)? as usize;
                ensure!(size >= 8, "unsupported JP2 box");
                if prefix.get(start + 4..start + 8) == Some(b"jp2h") {
                    let end = start.checked_add(size).context("JP2 header overflow")?;
                    ensure!(end <= prefix.len(), "large JP2 header");
                    let mut child = start + 8;
                    while child < end {
                        let length = be32(&prefix, child)? as usize;
                        ensure!(
                            length >= 8 && child + length <= end,
                            "invalid JP2 child box"
                        );
                        if prefix.get(child + 4..child + 8) == Some(b"colr") {
                            ensure!(
                                length >= 15 && prefix[child + 8] == 1,
                                "unsupported colour profile"
                            );
                            colour = Some(be32(&prefix, child + 11)?);
                        }
                        child += length;
                    }
                }
                if prefix.get(start + 4..start + 8) == Some(b"jp2c") {
                    start += 8;
                    break;
                }
                start = start.checked_add(size).context("JP2 box overflow")?;
                ensure!(start < prefix.len(), "large JP2 prefix");
            }
        }
        ensure!(
            prefix.get(start..start + 2) == Some(&[0xff, 0x4f]),
            "missing codestream start"
        );
        let mut pos = start + 2;
        let mut siz = None;
        let mut cod = None;
        while be16(&prefix, pos)? != 0xff90 {
            let marker = be16(&prefix, pos)?;
            let length = be16(&prefix, pos + 2)? as usize;
            ensure!(
                length >= 2 && pos + 2 + length <= prefix.len(),
                "bad main marker"
            );
            match marker {
                0xff51 => siz = Some(pos - start + 4),
                0xff52 => cod = Some(prefix[pos + 4..pos + 2 + length].to_vec()),
                0xff5c | 0xff64 => {}
                _ => bail!("unsupported main marker {marker:x}"),
            }
            pos += length + 2;
        }
        let siz = siz.context("no SIZ")?;
        let cod = cod.context("no COD")?;
        ensure!(cod.len() >= 10 && cod[5] <= 15, "invalid COD");
        let mut header = prefix[start..pos].to_vec();
        let width = be32(&header, siz + 2)?;
        let height = be32(&header, siz + 6)?;
        let tw = be32(&header, siz + 18)?;
        let th = be32(&header, siz + 22)?;
        let components = be16(&header, siz + 34)? as usize;
        ensure!(
            width > 0
                && height > 0
                && (1..=4096).contains(&tw)
                && (1..=4096).contains(&th)
                && (1..=4).contains(&components),
            "unsupported SIZ"
        );
        ensure!(be16(&cod, 2)? == 1, "multiple quality layers");
        if boxed {
            ensure!(
                matches!((colour, components), (Some(16), 3 | 4) | (Some(17), 1 | 2)),
                "unsupported colour space"
            );
        }
        ensure!(
            [10, 14, 26, 30]
                .iter()
                .all(|i| be32(&header, siz + i).ok() == Some(0)),
            "nonzero source origin"
        );
        let scale = 1u32 << cod[5];
        ensure!(tw % scale == 0 && th % scale == 0, "unaligned native tiles");
        let mut precision = Vec::new();
        let mut lut = Vec::new();
        for i in 0..components {
            let p = *header.get(siz + 36 + 3 * i).context("missing component")?;
            ensure!(
                p & 128 == 0
                    && p <= 15
                    && header.get(siz + 37 + 3 * i..siz + 39 + 3 * i) == Some(&[1, 1]),
                "unsupported component"
            );
            let bits = p + 1;
            let maximum = (1u32 << bits) - 1;
            precision.push(bits);
            lut.push(
                (0..=maximum)
                    .map(|v| ((v * 255 + maximum / 2) / maximum) as u8)
                    .collect(),
            );
        }
        header[siz..siz + 2].copy_from_slice(&0u16.to_be_bytes());
        let columns = width.div_ceil(tw);
        let rows = height.div_ceil(th);
        let count = columns.checked_mul(rows).context("tile count overflow")?;
        ensure!(count <= 4096, "too many native tiles");
        let mut parts = vec![(0, 0); count as usize];
        let mut offset = pos as u64;
        for _ in 0..count {
            let mut sot = [0u8; 14];
            file.read_exact_at(&mut sot, offset)?;
            ensure!(
                sot[..4] == [0xff, 0x90, 0, 10]
                    && sot[12..] == [0xff, 0x93]
                    && sot[10] == 0
                    && sot[11] <= 1,
                "unsupported tile header"
            );
            let tid = be16(&sot, 4)? as usize;
            let length = be32(&sot, 6)? as usize;
            ensure!(
                tid < parts.len()
                    && parts[tid].1 == 0
                    && (14..=16777216).contains(&length)
                    && offset + length as u64 <= k.1,
                "invalid tile part"
            );
            parts[tid] = (offset, length);
            offset += length as u64;
        }
        let mut end = [0; 2];
        file.read_exact_at(&mut end, offset)?;
        ensure!(end == [0xff, 0xd9], "unexpected codestream tail");
        ensure!(key(&k.0)? == k, "source changed while indexing");
        let ht_eligible =
            components == 3 && precision.iter().all(|p| *p == 16) && cod[4] == 0 && cod[9] == 1;
        Ok(Self {
            key: k,
            file,
            header,
            siz,
            width,
            height,
            tw,
            th,
            columns,
            levels: cod[5] as u32 + 1,
            precision,
            lut,
            parts,
            compressed: Mutex::new(VecDeque::new()),
            ht_eligible,
            ht_keys: Mutex::new(HashMap::new()),
        })
    }
    fn tile(&self, tid: u32) -> Result<Arc<Vec<u8>>> {
        {
            let mut cache = self.compressed.lock().unwrap();
            if let Some(i) = cache.iter().position(|(id, _)| *id == tid) {
                let entry = cache.remove(i).unwrap();
                let result = entry.1.clone();
                cache.push_back(entry);
                return Ok(result);
            }
        }
        let (offset, length) = self.parts[tid as usize];
        let mut data = self.header.clone();
        let w = self.tw.min(self.width - (tid % self.columns) * self.tw);
        let h = self.th.min(self.height - (tid / self.columns) * self.th);
        data[self.siz + 2..self.siz + 6].copy_from_slice(&w.to_be_bytes());
        data[self.siz + 6..self.siz + 10].copy_from_slice(&h.to_be_bytes());
        let at = data.len();
        data.resize(at + length, 0);
        self.file.read_exact_at(&mut data[at..], offset)?;
        data[at + 4..at + 6].copy_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&[0xff, 0xd9]);
        let data = Arc::new(data);
        let mut cache = self.compressed.lock().unwrap();
        cache.push_back((tid, data.clone()));
        while cache.iter().map(|(_, b)| b.len()).sum::<usize>() > 67108864 && cache.len() > 1 {
            cache.pop_front();
        }
        Ok(data)
    }
    fn cached_tile(&self, tid: u32) -> Result<(Arc<Vec<u8>>, bool)> {
        let original = self.tile(tid)?;
        let w = self.tw.min(self.width - (tid % self.columns) * self.tw);
        let h = self.th.min(self.height - (tid / self.columns) * self.th);
        if self.ht_eligible && w >= (1 << (self.levels - 1)) && h >= (1 << (self.levels - 1)) {
            if let Ok(cache) = crate::ht_cache::global() {
                let key = self
                    .ht_keys
                    .lock()
                    .unwrap()
                    .entry(tid)
                    .or_insert_with(|| crate::ht_cache::digest(&original))
                    .clone();
                if let Some(data) = cache.get(&key) {
                    return Ok((data, true));
                }
                cache.schedule(
                    key,
                    original.clone(),
                    crate::ht_cache::Params {
                        width: w,
                        height: h,
                        tile_width: self.tw,
                        tile_height: self.th,
                        resolutions: self.levels,
                    },
                );
            }
        }
        Ok((original, false))
    }
}
struct State {
    state: usize,
    stream: usize,
    params: usize,
    identity: Option<(Key, u32, bool)>,
    data: Option<Arc<Vec<u8>>>,
}
struct Workspace {
    api:Arc<Api>, device:i32, gpu:usize, host:usize, capacity:usize,
}
impl Workspace {
    fn new(api:Arc<Api>,device:i32)->Result<Self> {
        unsafe {check((api.device)(device),"select workspace device")?;}
        let mut workspace=Self {api,device,gpu:0,host:0,capacity:0};
        workspace.reserve(8388608)?;
        Ok(workspace)
    }
    fn reserve(&mut self,size:usize)->Result<()> {
        ensure!(size<=134217728,"GPU workspace limit");
        if size<=self.capacity {return Ok(());}
        unsafe {
            check((self.api.device)(self.device),"select workspace device")?;
            self.capacity=0;
            if self.gpu!=0 {check((self.api.free)(self.gpu as P),"free output")?;self.gpu=0;}
            if self.host!=0 {check((self.api.host_free)(self.host as P),"free pinned output")?;self.host=0;}
            check((self.api.malloc)(&mut self.gpu as *mut usize as *mut P,size),"allocate output")?;
            check((self.api.host_alloc)(&mut self.host as *mut usize as *mut P,size,0),"allocate pinned output")?;
            self.capacity=size;
        }
        Ok(())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        unsafe {
            let _=(self.api.device)(self.device);
            if self.gpu!=0 {(self.api.free)(self.gpu as P);}
            if self.host!=0 {(self.api.host_free)(self.host as P);}
        }
    }
}
struct Gpu {
    api: Arc<Api>,
    device: i32,
    handle: usize,
    cuda: usize,
    workspaces:Arc<crate::lease_pool::Pool<Workspace>>,
    states: Vec<State>,
    allocator: Option<Box<AllocCtx>>,
    device_functions: Option<Box<AllocFns>>,
    pinned_functions: Option<Box<AllocFns>>,
}
struct Finish(Arc<Api>, usize, bool);
impl Drop for Finish {
    fn drop(&mut self) {
        if self.2 {
            unsafe {
                (self.0.sync)(self.1 as P);
            }
        }
    }
}
impl Gpu {
    fn new(api: Arc<Api>,workspaces:Arc<crate::lease_pool::Pool<Workspace>>) -> Result<Self> {
        let device = std::env::var("RIVER_NVJPEG2000_DEVICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut result = Self {
            api,
            device,
            handle: 0,
            cuda: 0,
            workspaces,
            states: Vec::new(),
            allocator: None,
            device_functions: None,
            pinned_functions: None,
        };
        unsafe {
            check((result.api.device)(device), "select GPU")?;
            if std::env::var("RIVER_NVJPEG2000_NATIVE_ASYNC_ALLOCATOR").as_deref() != Ok("false") {
                let mut context = Box::new(AllocCtx {
                    api: result.api.clone(),
                    pinned: Mutex::new(Vec::new()),
                    sizes: Mutex::new(HashMap::new()),
                });
                let pointer = context.as_mut() as *mut AllocCtx as P;
                result.allocator = Some(context);
                result.device_functions = Some(Box::new(AllocFns {
                    allocate: device_allocate,
                    free: device_free,
                    context: pointer,
                }));
                result.pinned_functions = Some(Box::new(AllocFns {
                    allocate: pinned_allocate,
                    free: pinned_free,
                    context: pointer,
                }));
                check(
                    (result.api.create_v2)(
                        0,
                        result.device_functions.as_mut().unwrap().as_mut(),
                        result.pinned_functions.as_mut().unwrap().as_mut(),
                        &mut result.handle as *mut usize as *mut P,
                    ),
                    "create pooled decoder",
                )?;
            } else {
                check(
                    (result.api.create)(&mut result.handle as *mut usize as *mut P),
                    "create decoder",
                )?;
            }
            check(
                (result.api.cuda_create)(&mut result.cuda as *mut usize as *mut P, 1),
                "create CUDA stream",
            )?;
            for _ in 0..8 {
                result.states.push(State {
                    state: 0,
                    stream: 0,
                    params: 0,
                    identity: None,
                    data: None,
                });
                let state = result.states.last_mut().unwrap();
                check(
                    (result.api.state_create)(
                        result.handle as P,
                        &mut state.state as *mut usize as *mut P,
                    ),
                    "create state",
                )?;
                check(
                    (result.api.stream_create)(&mut state.stream as *mut usize as *mut P),
                    "create parsed stream",
                )?;
                check(
                    (result.api.params_create)(&mut state.params as *mut usize as *mut P),
                    "create parameters",
                )?;
                check((result.api.rgb)(state.params as P, 1), "RGB output")?;
            }
        }
        Ok(result)
    }
    fn decode(
        &mut self,
        source: &Indexed,
        bounds: &[[u32; 4]],
        reduction: u8,
    ) -> Result<(Vec<(u32, u32, Vec<u8>)>, &'static str)> {
        let prepare_span = info_span!("river.cuda.native.prepare");
        let prepare_entered = prepare_span.enter();
        ensure!((reduction as u32) < source.levels, "invalid reduction");
        let scale = 1u32 << reduction;
        let mut ids = BTreeSet::new();
        for [a, b, c, d] in bounds {
            for y in b / source.th..=(d - 1) / source.th {
                for x in a / source.tw..=(c - 1) / source.tw {
                    ids.insert(y * source.columns + x);
                }
            }
        }
        struct Plan {
            tid: u32,
            x: u32,
            y: u32,
            w: u32,
            h: u32,
            offset: usize,
            area: [u32; 4],
            rgba: Vec<u8>,
        }
        let components = source.precision.len();
        let word = if source.precision.iter().any(|p| *p > 8) {
            2
        } else {
            1
        };
        let mut plans = Vec::new();
        let mut total = 0;
        for tid in ids {
            let nx = tid % source.columns * source.tw;
            let ny = tid / source.columns * source.th;
            let ex = (nx + source.tw).min(source.width);
            let ey = (ny + source.th).min(source.height);
            let hits: Vec<_> = bounds
                .iter()
                .filter(|[a, b, c, d]| *a < ex && *c > nx && *b < ey && *d > ny)
                .collect();
            let x = hits
                .iter()
                .map(|r| r[0].max(nx))
                .min()
                .unwrap()
                .div_ceil(scale);
            let y = hits
                .iter()
                .map(|r| r[1].max(ny))
                .min()
                .unwrap()
                .div_ceil(scale);
            let right = hits
                .iter()
                .map(|r| r[2].min(ex))
                .max()
                .unwrap()
                .div_ceil(scale);
            let bottom = hits
                .iter()
                .map(|r| r[3].min(ey))
                .max()
                .unwrap()
                .div_ceil(scale);
            let w = right - x;
            let h = bottom - y;
            let area = [
                x - nx / scale,
                right - nx / scale,
                y - ny / scale,
                bottom - ny / scale,
            ];
            plans.push(Plan {
                tid,
                x,
                y,
                w,
                h,
                area,
                offset: total,
                rgba: Vec::new(),
            });
            total += w as usize * h as usize * components * word;
        }
        // The lease outlives the synchronization guard, including error paths.
        let mut workspace=self.workspaces.take();
        workspace.reserve(total)?;
        drop(prepare_entered);
        drop(prepare_span);
        let decode_span = info_span!(
            "river.cuda.native.decode_tiles",
            native_tiles = plans.len(),
            output_bytes = total,
            reduction,
            ht_cache_tiles = tracing::field::Empty,
            parse_ms = tracing::field::Empty,
            submit_ms = tracing::field::Empty,
            wait_ms = tracing::field::Empty
        );
        let _decode = decode_span.enter();
        let mut ht_tiles = 0usize;
        let mut descriptors = Vec::new();
        let mut finish = Finish(self.api.clone(), self.cuda, true);
        let mut submit_ms = 0.0;
        let mut parse_ms = 0.0;
        let started = Instant::now();
        for (i, plan) in plans.iter().enumerate() {
            let slot = i % self.states.len();
            if crate::worker_sizing::state_reuse_barrier(i,self.states.len()) {
                unsafe {
                    check((self.api.sync)(self.cuda as P), "wait for state reuse")?;
                }
            }
            let (data, ht) = source.cached_tile(plan.tid)?;
            ht_tiles += usize::from(ht);
            decode_span.record("ht_cache_tiles", ht_tiles);
            let state = &mut self.states[slot];
            let identity = (source.key.clone(), plan.tid, ht);
            if state.identity.as_ref() != Some(&identity) {
                let parsed = Instant::now();
                state.identity = None;
                unsafe {
                    check(
                        (self.api.stream_destroy)(state.stream as P),
                        "reset parsed stream",
                    )?;
                    state.stream = 0;
                    check(
                        (self.api.stream_create)(&mut state.stream as *mut usize as *mut P),
                        "create parsed stream",
                    )?;
                    check(
                        (self.api.parse)(
                            self.handle as P,
                            data.as_ptr(),
                            data.len(),
                            0,
                            0,
                            state.stream as P,
                        ),
                        "parse compressed native tile",
                    )?;
                }
                state.data = Some(data);
                state.identity = Some(identity);
                parse_ms += elapsed_ms(parsed);
            }
            let plane = plan.w as usize * plan.h as usize * word;
            let mut pointers: Vec<usize> = (0..components)
                .map(|c| workspace.gpu + plan.offset + c * plane)
                .collect();
            let mut pitches = vec![plan.w as usize * word; components];
            let output = Box::new(Image {
                pixels: pointers.as_mut_ptr() as *mut P,
                pitches: pitches.as_mut_ptr(),
                pixel_type: if word == 2 { 1 } else { 0 },
                components: components as u32,
            });
            descriptors.push((pointers, pitches, output));
            let submit = Instant::now();
            unsafe {
                check(
                    (self.api.area)(
                        state.params as P,
                        plan.area[0],
                        plan.area[1],
                        plan.area[2],
                        plan.area[3],
                    ),
                    "set tile ROI",
                )?;
                check(
                    (self.api.tile)(
                        self.handle as P,
                        state.state as P,
                        state.stream as P,
                        state.params as P,
                        0,
                        source.levels - reduction as u32,
                        descriptors.last_mut().unwrap().2.as_mut(),
                        self.cuda as P,
                    ),
                    "decode native tile",
                )?;
            }
            submit_ms += elapsed_ms(submit);
        }
        let waiting = Instant::now();
        unsafe {
            check(
                (self.api.copy)(workspace.host as P, workspace.gpu as P, total, 2, self.cuda as P),
                "download raster",
            )?;
            check((self.api.sync)(self.cuda as P), "finish raster")?;
        }
        finish.2 = false;
        let wait_ms = elapsed_ms(waiting);
        decode_span.record("native_tiles", plans.len());
        decode_span.record("parse_ms", parse_ms);
        decode_span.record("submit_ms", submit_ms);
        decode_span.record("wait_ms", wait_ms);
        tracing::debug!(
            parse_ms,
            submit_ms,
            wait_ms,
            elapsed_ms = elapsed_ms(started),
            "direct CUDA stages"
        );
        drop(_decode);
        drop(decode_span);
        let normalize = info_span!("river.cuda.native.normalize");
        let _normalize = normalize.enter();
        for plan in &mut plans {
            let pixels = plan.w as usize * plan.h as usize;
            plan.rgba = vec![0; pixels * 4];
            let values = unsafe {
                std::slice::from_raw_parts(
                    (workspace.host + plan.offset) as *const u8,
                    pixels * components * word,
                )
            };
            for i in 0..pixels {
                let mut channels = [0u8; 4];
                for c in 0..components {
                    let at = (c * pixels + i) * word;
                    let v = if word == 2 {
                        u16::from_ne_bytes([values[at], values[at + 1]]) as usize
                    } else {
                        values[at] as usize
                    };
                    channels[c] = *source.lut[c]
                        .get(v)
                        .context("GPU sample exceeds component precision")?;
                }
                plan.rgba[i * 4..i * 4 + 4].copy_from_slice(&[
                    channels[0],
                    if components < 3 {
                        channels[0]
                    } else {
                        channels[1]
                    },
                    if components < 3 {
                        channels[0]
                    } else {
                        channels[2]
                    },
                    if components == 2 {
                        channels[1]
                    } else if components == 4 {
                        channels[3]
                    } else {
                        255
                    },
                ]);
            }
        }
        let mut output = Vec::new();
        for [a, b, c, d] in bounds {
            let x = a.div_ceil(scale);
            let y = b.div_ceil(scale);
            let w = c.div_ceil(scale) - x;
            let h = d.div_ceil(scale) - y;
            let mut rgba = vec![0; w as usize * h as usize * 4];
            for plan in &plans {
                let l = x.max(plan.x);
                let t = y.max(plan.y);
                let r = (x + w).min(plan.x + plan.w);
                let bottom = (y + h).min(plan.y + plan.h);
                if l >= r || t >= bottom {
                    continue;
                }
                for row in t..bottom {
                    let dst = ((row - y) * w + l - x) as usize * 4;
                    let src = ((row - plan.y) * plan.w + l - plan.x) as usize * 4;
                    let count = (r - l) as usize * 4;
                    rgba[dst..dst + count].copy_from_slice(&plan.rgba[src..src + count]);
                }
            }
            output.push((w, h, rgba));
        }
        let route = if ht_tiles == plans.len() {
            "nvjpeg2000-ht-native"
        } else if ht_tiles > 0 {
            "nvjpeg2000-mixed-native"
        } else {
            "nvjpeg2000-native"
        };
        Ok((output, route))
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.api.device)(self.device);
            if self.cuda != 0 {
                let _ = (self.api.sync)(self.cuda as P);
            }
            for state in &self.states {
                if state.state != 0 {
                    (self.api.state_destroy)(state.state as P);
                }
                if state.stream != 0 {
                    (self.api.stream_destroy)(state.stream as P);
                }
                if state.params != 0 {
                    (self.api.params_destroy)(state.params as P);
                }
            }
            if self.handle != 0 {
                (self.api.destroy)(self.handle as P);
            }
            if let Some(allocator) = &self.allocator {
                allocator.drain();
            }
            if self.cuda != 0 {
                (self.api.cuda_destroy)(self.cuda as P);
            }
        }
    }
}
type Cell = Arc<OnceLock<std::result::Result<Arc<Indexed>, String>>>;
struct Job {
    source: Arc<Indexed>,
    bounds: Vec<[u32; 4]>,
    reduction: u8,
    queued: Instant,
    span: tracing::Span,
    reply: mpsc::SyncSender<Result<(Vec<(u32, u32, Vec<u8>)>, f64, Instant, &'static str)>>,
}
pub struct NativeFine {
    sources: Mutex<VecDeque<(Key, Cell)>>,
    sender: mpsc::Sender<Job>,
}
impl NativeFine {
    pub fn new() -> Result<Self> {
        let api = Api::load()?;
        let workers = std::env::var("RIVER_NVJPEG2000_NATIVE_WORKERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok()).unwrap_or(32).clamp(1,32);
        let count=crate::worker_sizing::workspaces(workers,crate::decode_concurrency_from_env());
        let device=std::env::var("RIVER_NVJPEG2000_DEVICE").ok().and_then(|v|v.parse().ok()).unwrap_or(0);
        let workspaces=crate::lease_pool::Pool::new((0..count).map(|_|Workspace::new(api.clone(),device)).collect::<Result<Vec<_>>>()?);
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let (ready_tx, ready_rx) = mpsc::channel();
        for index in 0..workers {
            let api = api.clone();
            let rx = receiver.clone();
            let ready = ready_tx.clone();
            let workspaces=workspaces.clone();
            std::thread::Builder::new()
                .name(format!("river-cuda-{index}"))
                .spawn(move || {
                    let mut gpu = match Gpu::new(api,workspaces) {
                        Ok(gpu) => {
                            let _ = ready.send(Ok(()));
                            gpu
                        }
                        Err(error) => {
                            let _ = ready.send(Err(error.to_string()));
                            return;
                        }
                    };
                    loop {
                        let job = match rx.lock().unwrap().recv() {
                            Ok(job) => job,
                            Err(_) => break,
                        };
                        let _entered = job.span.enter();
                        let queue_ms = elapsed_ms(job.queued);
                        let result = gpu
                            .decode(&job.source, &job.bounds, job.reduction)
                            .map(|(v, route)| (v, queue_ms, Instant::now(), route));
                        let _ = job.reply.send(result);
                    }
                })?;
        }
        drop(ready_tx);
        for _ in 0..workers {
            ready_rx
                .recv()
                .context("GPU initializer stopped")?
                .map_err(anyhow::Error::msg)?;
        }
        tracing::info!(workers,workspaces=count, "direct CUDA fine workers initialized");
        Ok(Self {
            sources: Mutex::new(VecDeque::new()),
            sender,
        })
    }
    pub fn decode(&self, request: &TileDecodeRequest) -> Result<Option<Vec<DecodedTile>>> {
        let _encoding = crate::encoding_settings::Scope::new(request.webp_method);
        if request.reduction >= 3 {
            return Ok(None);
        }
        let started = Instant::now();
        let k = key(&request.source.path)?;
        let cell = {
            let mut sources = self.sources.lock().unwrap();
            if let Some(i) = sources.iter().position(|(key, _)| key == &k) {
                let entry = sources.remove(i).unwrap();
                let cell = entry.1.clone();
                sources.push_back(entry);
                cell
            } else {
                let cell = Arc::new(OnceLock::new());
                sources.push_back((k.clone(), cell.clone()));
                while sources.len() > 4 {
                    sources.pop_front();
                }
                cell
            }
        };
        let source =
            match cell.get_or_init(|| Indexed::open(k).map(Arc::new).map_err(|e| e.to_string())) {
                Ok(source) => source.clone(),
                Err(reason) => {
                    tracing::debug!(reason, "direct fine source unsupported; using sidecar");
                    return Ok(None);
                }
            };
        ensure!(
            (source.width, source.height) == (request.source.width, request.source.height),
            "source dimensions mismatch"
        );
        let mut bounds = Vec::new();
        for (x, y) in &request.tiles {
            let a = x
                .checked_mul(request.source_tile)
                .context("tile x overflow")?;
            let b = y
                .checked_mul(request.source_tile)
                .context("tile y overflow")?;
            ensure!(a < source.width && b < source.height, "tile outside source");
            bounds.push([
                a,
                b,
                a.saturating_add(request.source_tile).min(source.width),
                b.saturating_add(request.source_tile).min(source.height),
            ]);
        }
        let pool_span = info_span!(
            "river.cuda.native.pool",
            prepare_ms = elapsed_ms(started),
            queue_ms = tracing::field::Empty,
            dispatch_ms = tracing::field::Empty
        );
        let pool_entered = pool_span.enter();
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .send(Job {
                source,
                bounds,
                reduction: request.reduction,
                queued: Instant::now(),
                span: tracing::Span::current(),
                reply: tx,
            })
            .context("CUDA worker pool stopped")?;
        let (rasters, queue_ms, finished, route) = rx.recv().context("CUDA worker stopped")??;
        let decode_ms = elapsed_ms(started);
        pool_span.record("queue_ms", queue_ms);
        pool_span.record("dispatch_ms", elapsed_ms(finished));
        drop(pool_entered);
        drop(pool_span);
        tracing::debug!(queue_ms, decode_ms, "direct CUDA response");
        let mut output = Vec::new();
        for ((tile_x, tile_y), (w, h, rgba)) in request.tiles.iter().zip(rasters) {
            let encode = Instant::now();
            let bytes = encode_crop_rgba(&rgba, w, 0, 0, w, h, request.format)?;
            output.push(DecodedTile { rgba:crate::decoder::take_pixel_capture(),
                tile_x: *tile_x,
                tile_y: *tile_y,
                raster: DecodedRaster {
                    width: w,
                    height: h,
                    bytes: bytes.into_boxed_slice().into(),
                    content_type: request.format.content_type(),
                    decoder_backend: route, decoder_route: route,
                    decode_ms,
                    encode_ms: elapsed_ms(encode),
                    source_cache_status: "bypass",
                    source_cache_ms: 0.0,
                    compressed_resolution_view: false,
                    source_read_bytes: 0,
                    source_range_requests: 0,
                    source_cache_hit_bytes: 0,
                },
            });
        }
        Ok(Some(output))
    }
}
