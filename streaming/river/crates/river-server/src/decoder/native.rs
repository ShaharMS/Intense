//! Native JPEG 2000 tile access.
//!
//! This is deliberately a small, owned OpenJPEG boundary rather than a
//! shortcut around the generic `jpeg2k` convenience wrapper.  `get_decoded_tile`
//! lets River ask the codestream for just its native tiles; the outer decoder
//! then assembles the requested transport tiles as ordinary RGBA rasters.
//!
//! Decoded native tiles have a bounded cache keyed by the asset snapshot,
//! local file metadata, reduction and quality. Active readers retain leases.

use std::{ffi::CString, path::{Path, PathBuf}, ptr, time::{Instant, SystemTime},
    collections::{HashMap, BTreeMap}, sync::{Arc, Mutex, OnceLock, Weak}};

use anyhow::{Context, Result, bail};
use openjpeg_sys as opj;
use tracing::instrument;

use super::{
    DecodedRaster, DecodedTile, NativeTileLayoutHint, RasterFormat, clamped_tile_bounds,
    elapsed_ms, encode_crop_rgba,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeTileLayout {
    origin_x: u32,
    origin_y: u32,
    width: u32,
    height: u32,
    columns: u32,
    rows: u32,
}

impl From<NativeTileLayoutHint> for NativeTileLayout {
    fn from(layout: NativeTileLayoutHint) -> Self {
        Self {
            origin_x: layout.origin_x,
            origin_y: layout.origin_y,
            width: layout.width,
            height: layout.height,
            columns: layout.columns,
            rows: layout.rows,
        }
    }
}

impl NativeTileLayout {
    fn is_tiled(self) -> bool {
        self.columns.saturating_mul(self.rows) > 1 && self.width > 0 && self.height > 0
    }

    fn bounds(self, index: u32, source_width: u32, source_height: u32) -> (u32, u32, u32, u32) {
        let column = index % self.columns;
        let row = index / self.columns;
        let start_x = self
            .origin_x
            .saturating_add(column.saturating_mul(self.width))
            .min(source_width);
        let start_y = self
            .origin_y
            .saturating_add(row.saturating_mul(self.height))
            .min(source_height);
        (
            start_x,
            start_y,
            start_x.saturating_add(self.width).min(source_width),
            start_y.saturating_add(self.height).min(source_height),
        )
    }

    fn indices_intersecting(
        self,
        source_bounds: (u32, u32, u32, u32),
        source_width: u32,
        source_height: u32,
    ) -> Vec<u32> {
        let (x0, y0, x1, y1) = source_bounds;
        if x0 >= x1 || y0 >= y1 || !self.is_tiled() {
            return Vec::new();
        }
        let first_column = x0.saturating_sub(self.origin_x) / self.width;
        let last_column = x1.saturating_sub(1).saturating_sub(self.origin_x) / self.width;
        let first_row = y0.saturating_sub(self.origin_y) / self.height;
        let last_row = y1.saturating_sub(1).saturating_sub(self.origin_y) / self.height;
        let last_column = last_column.min(self.columns.saturating_sub(1));
        let last_row = last_row.min(self.rows.saturating_sub(1));
        (first_row..=last_row)
            .flat_map(|row| {
                (first_column..=last_column).map(move |column| row * self.columns + column)
            })
            .filter(|&index| {
                let (tile_x0, tile_y0, tile_x1, tile_y1) =
                    self.bounds(index, source_width, source_height);
                tile_x0 < x1 && x0 < tile_x1 && tile_y0 < y1 && y0 < tile_y1
            })
            .collect()
    }
}

struct OpenJpegTileDecoder {
    codec: *mut opj::opj_codec_t,
    stream: *mut opj::opj_stream_t,
    image: *mut opj::opj_image_t,
    layout: NativeTileLayout,
}

struct MemoryStream { bytes: Arc<[u8]>, position: usize }
unsafe extern "C" fn memory_read(output: *mut std::ffi::c_void, length: usize, user: *mut std::ffi::c_void) -> usize {
    let stream = unsafe { &mut *(user as *mut MemoryStream) };
    let count = length.min(stream.bytes.len() - stream.position);
    if count == 0 { return usize::MAX; }
    unsafe { ptr::copy_nonoverlapping(stream.bytes.as_ptr().add(stream.position),output.cast::<u8>(),count); }
    stream.position += count; count
}
unsafe extern "C" fn memory_seek(position: i64, user: *mut std::ffi::c_void) -> i32 {
    let stream = unsafe { &mut *(user as *mut MemoryStream) };
    if position < 0 || position as u64 > stream.bytes.len() as u64 { return 0; }
    stream.position = position as usize; 1
}
unsafe extern "C" fn memory_skip(distance: i64, user: *mut std::ffi::c_void) -> i64 {
    let stream = unsafe { &mut *(user as *mut MemoryStream) };
    let position = stream.position as i128 + distance as i128;
    if position < 0 || position > stream.bytes.len() as i128 { return -1; }
    stream.position = position as usize; distance
}
unsafe extern "C" fn memory_free(user: *mut std::ffi::c_void) {
    unsafe { drop(Box::from_raw(user as *mut MemoryStream)); }
}

impl Drop for OpenJpegTileDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.image.is_null() {
                opj::opj_image_destroy(self.image);
            }
            if !self.codec.is_null() {
                opj::opj_destroy_codec(self.codec);
            }
            if !self.stream.is_null() {
                opj::opj_stream_destroy(self.stream);
            }
        }
    }
}

impl OpenJpegTileDecoder {
    #[instrument(
        name = "river.codec.openjpeg.native_header",
        skip(path),
        fields(reduction, layers, decoder_threads)
    )]
    fn open(path: &Path, reduction: u8, layers: u32, decoder_threads: usize, memory: Option<&Arc<[u8]>>) -> Result<Self> {
        let filename = path
            .to_str()
            .context("JPEG 2000 source path is not valid Unicode")?;
        let filename =
            CString::new(filename).context("JPEG 2000 source path contains a NUL byte")?;
        let format = if let Some(bytes) = memory {
            if bytes.starts_with(&[0,0,0,12,b'j',b'P',b' ',b' ']) { opj::OPJ_CODEC_FORMAT::OPJ_CODEC_JP2 }
            else { opj::OPJ_CODEC_FORMAT::OPJ_CODEC_J2K }
        } else { match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.eq_ignore_ascii_case("jp2"))
        {
            Some(true) => opj::OPJ_CODEC_FORMAT::OPJ_CODEC_JP2,
            _ => opj::OPJ_CODEC_FORMAT::OPJ_CODEC_J2K,
        } };
        unsafe {
            let stream = if let Some(bytes) = memory {
                let stream = opj::opj_stream_create(64*1024,1);
                if !stream.is_null() {
                    let data = Box::new(MemoryStream { bytes: bytes.clone(),position: 0 });
                    opj::opj_stream_set_user_data_length(stream,bytes.len() as u64);
                    opj::opj_stream_set_user_data(stream,Box::into_raw(data).cast(),Some(memory_free));
                    opj::opj_stream_set_read_function(stream,Some(memory_read));
                    opj::opj_stream_set_skip_function(stream,Some(memory_skip));
                    opj::opj_stream_set_seek_function(stream,Some(memory_seek));
                }
                stream
            } else { opj::opj_stream_create_default_file_stream(filename.as_ptr(), 1) };
            if stream.is_null() {
                bail!("OpenJPEG could not open {}", path.display());
            }
            let codec = opj::opj_create_decompress(format);
            if codec.is_null() {
                opj::opj_stream_destroy(stream);
                bail!("OpenJPEG could not create a decoder for {}", path.display());
            }
            let mut parameters = std::mem::zeroed::<opj::opj_dparameters_t>();
            opj::opj_set_default_decoder_parameters(&mut parameters);
            parameters.cp_reduce = u32::from(reduction);
            parameters.cp_layer = layers;
            if opj::opj_setup_decoder(codec, &mut parameters) != 1 {
                opj::opj_destroy_codec(codec);
                opj::opj_stream_destroy(stream);
                bail!(
                    "OpenJPEG could not configure decoder for {}",
                    path.display()
                );
            }
            // Extra workers are leased across active OpenJPEG jobs. A single
            // useful CPU uses the synchronous codec path, without a worker
            // pool for small previews.
            let threads = if decoder_threads<=1 {0}else{decoder_threads.min(i32::MAX as usize) as i32};
            if opj::opj_codec_set_threads(codec, threads) != 1 {
                opj::opj_destroy_codec(codec);
                opj::opj_stream_destroy(stream);
                bail!(
                    "OpenJPEG could not set decoder thread budget for {}",
                    path.display()
                );
            }
            let mut image = ptr::null_mut();
            if opj::opj_read_header(stream, codec, &mut image) != 1 || image.is_null() {
                if !image.is_null() { opj::opj_image_destroy(image); }
                opj::opj_destroy_codec(codec);
                opj::opj_stream_destroy(stream);
                bail!(
                    "OpenJPEG could not read the JPEG 2000 header for {}",
                    path.display()
                );
            }
            let mut info = opj::opj_get_cstr_info(codec);
            if info.is_null() {
                opj::opj_image_destroy(image);
                opj::opj_destroy_codec(codec);
                opj::opj_stream_destroy(stream);
                bail!(
                    "OpenJPEG did not expose codestream tile information for {}",
                    path.display()
                );
            }
            let layout = NativeTileLayout {
                origin_x: (*info).tx0,
                origin_y: (*info).ty0,
                width: (*info).tdx,
                height: (*info).tdy,
                columns: (*info).tw,
                rows: (*info).th,
            };
            opj::opj_destroy_cstr_info(&mut info);
            Ok(Self {
                codec,
                stream,
                image,
                layout,
            })
        }
    }

    #[instrument(name = "river.codec.openjpeg.native_tile", skip(self), fields(native_tile = tile_index))]
    fn decode_tile(&mut self, tile_index: u32) -> Result<RawNativeTile> {
        unsafe {
            if opj::opj_get_decoded_tile(self.codec, self.stream, self.image, tile_index) != 1 {
                bail!("OpenJPEG could not decode native tile {tile_index}");
            }
            if opj::opj_end_decompress(self.codec, self.stream) != 1 {
                bail!("OpenJPEG could not finish native tile {tile_index}");
            }
            let image = &*self.image;
            let components = std::slice::from_raw_parts(image.comps, image.numcomps as usize);
            if components.is_empty() || components.len() > 16 {
                bail!(
                    "native tile {tile_index} has unsupported component count {}",
                    components.len()
                );
            }
            let components=if components.len()>4 || (components.len()==4 && components[3].alpha==0) {&components[..3]}else{components};
            let width = components[0].w;
            let height = components[0].h;
            if width == 0 || height == 0 {
                bail!("native tile {tile_index} decoded to an empty raster");
            }
            if components.iter().any(|component| {
                component.w != width || component.h != height || component.data.is_null()
            }) {
                bail!("native tile {tile_index} has unequal or missing component planes");
            }
            let pixel_count = width as usize * height as usize;
            let planes = components
                .iter()
                .map(|component| std::slice::from_raw_parts(component.data, pixel_count))
                .collect::<Vec<_>>();
            let mut rgba = Vec::with_capacity(pixel_count * 4);
            for pixel in 0..pixel_count {
                let red = normalized_u8(planes[0][pixel], &components[0]);
                let (green, blue, alpha) = match components.len() {
                    1 => (red, red, 255),
                    2 => (red, red, normalized_u8(planes[1][pixel], &components[1])),
                    3 => (
                        normalized_u8(planes[1][pixel], &components[1]),
                        normalized_u8(planes[2][pixel], &components[2]),
                        255,
                    ),
                    4 => (
                        normalized_u8(planes[1][pixel], &components[1]),
                        normalized_u8(planes[2][pixel], &components[2]),
                        normalized_u8(planes[3][pixel], &components[3]),
                    ),
                    _ => unreachable!(),
                };
                rgba.extend_from_slice(&[red, green, blue, alpha]);
            }
            Ok(RawNativeTile {
                width,
                height,
                rgba,
            })
        }
    }
}

struct RawNativeTile {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct NativeKey { snapshot: String, path: PathBuf, size: u64, modified: SystemTime,
    tile: u32, reduction: u8, layers: u32 }
struct NativeEntry { tile: Arc<RawNativeTile>, bytes: usize, touched: u64 }
#[derive(Default)]
struct NativeCacheState { entries: HashMap<NativeKey, NativeEntry>, lru: BTreeMap<u64, NativeKey>, bytes: usize, clock: u64 }
struct NativeCache { limit: usize, state: Mutex<NativeCacheState>, flights: Mutex<HashMap<NativeKey, Weak<Mutex<()>>>> }
impl NativeCache {
    fn shared()->&'static Self {static C:OnceLock<NativeCache>=OnceLock::new();static REGISTER:std::sync::Once=std::sync::Once::new();let cache=C.get_or_init(||Self{limit:crate::resources::budget("native_tiles"),state:Mutex::new(NativeCacheState::default()),flights:Mutex::new(HashMap::new())});REGISTER.call_once(||crate::resources::register("native_tiles",move|limit|cache.trim(limit)));cache}
    fn trim(&self,limit:usize)->crate::resources::Occupancy {let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());while s.bytes>limit||s.entries.len()>crate::resources::entry_limit("RIVER_NATIVE_TILE_CACHE_ENTRIES",limit){let Some((_,key))=s.lru.pop_first()else{break;};if let Some(old)=s.entries.remove(&key){s.bytes-=old.bytes;crate::resources::retain("native_tiles",old.bytes,&old.tile);crate::resources::evict("native_tiles",1);}}crate::resources::Occupancy{bytes:s.bytes,entries:s.entries.len(),..Default::default()}}
    fn get(&self, key: &NativeKey) -> Option<Arc<RawNativeTile>> {
        let mut state = self.state.lock().unwrap();
        let Some(entry)=state.entries.get(key)else{crate::resources::miss("native_tiles");return None;};crate::resources::hit("native_tiles",entry.bytes);
        let tile = entry.tile.clone(); let old = entry.touched;
        state.lru.remove(&old); state.clock += 1; let clock = state.clock;
        state.lru.insert(clock,key.clone()); state.entries.get_mut(key).unwrap().touched = clock;
        Some(tile)
    }
    fn decode(&self, key: Option<NativeKey>, decode: impl FnOnce() -> Result<RawNativeTile>) -> Result<Arc<RawNativeTile>> {
        let Some(key) = key.filter(|_| crate::resources::effective("native_tiles",self.limit) > 0) else { return decode().map(Arc::new); };
        if let Some(tile) = self.get(&key) { return Ok(tile); }
        let gate = {
            let mut flights = self.flights.lock().unwrap();
            flights.retain(|_, gate| gate.strong_count() > 0);
            if let Some(gate) = flights.get(&key).and_then(Weak::upgrade) { gate }
            else { let gate = Arc::new(Mutex::new(())); flights.insert(key.clone(),Arc::downgrade(&gate)); gate }
        };
        let _guard = gate.lock().unwrap();
        if let Some(tile) = self.get(&key) { return Ok(tile); }
        let tile = Arc::new(decode()?);
        let bytes = tile.rgba.capacity();
        if bytes <= crate::resources::effective("native_tiles",self.limit) {
            let mut state = self.state.lock().unwrap();
            while state.bytes + bytes > crate::resources::effective("native_tiles",self.limit) || state.entries.len() >= crate::resources::entry_limit("RIVER_NATIVE_TILE_CACHE_ENTRIES",self.limit) {
                let Some((_, oldest)) = state.lru.pop_first() else { break; };
                if let Some(entry) = state.entries.remove(&oldest) { state.bytes -= entry.bytes;crate::resources::retain("native_tiles",entry.bytes,&entry.tile);crate::resources::evict("native_tiles",1); }
            }
            state.clock += 1; let touched = state.clock;
            state.bytes += bytes; state.lru.insert(touched,key.clone());
            state.entries.insert(key,NativeEntry { tile: tile.clone(),bytes,touched });
        }
        Ok(tile)
    }
}

struct TransportTile {
    tile_x: u32,
    tile_y: u32,
    source_x0: u32,
    source_y0: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[instrument(
    name = "river.codec.openjpeg.native_tiles",
    skip(path, tiles),
    fields(tile_count = tiles.len(), source_tile, reduction, layers, decoder_threads, max_native_tiles, native_tile_count = tracing::field::Empty, route = tracing::field::Empty)
)]
pub(super) fn decode_transport_tiles(
    path: &Path,
    source_width: u32,
    source_height: u32,
    tiles: &[(u32, u32)],
    source_tile: u32,
    reduction: u8,
    layers: u32,
    format: RasterFormat,
    decoder_threads: usize,
    max_native_tiles: usize,
    layout_hint: Option<NativeTileLayoutHint>,
    snapshot: &str,
    memory: Option<&Arc<[u8]>>,
) -> Result<Option<Vec<DecodedTile>>> {
    if tiles.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let scale = 1u32
        .checked_shl(u32::from(reduction))
        .context("unsupported JPEG 2000 reduction")?;
    // Catalog-backed sources carry this tiny layout record. It allows a
    // large request to reject native traversal before even opening the JP2.
    // Development/discovered sources retain the header-probe fallback.
    let mut first_decoder = None;
    let layout = if let Some(hint) = layout_hint {
        NativeTileLayout::from(hint)
    } else {
        let decoder = OpenJpegTileDecoder::open(path, reduction, layers, decoder_threads, memory)?;
        let layout = decoder.layout;
        first_decoder = Some(decoder);
        layout
    };
    if !layout.is_tiled() {
        return Ok(None);
    }
    let mut outputs = tiles
        .iter()
        .map(|&(tile_x, tile_y)| {
            let (source_x0, source_x1) = clamped_tile_bounds(tile_x, source_tile, source_width);
            let (source_y0, source_y1) = clamped_tile_bounds(tile_y, source_tile, source_height);
            let width = (source_x1 - source_x0).div_ceil(scale);
            let height = (source_y1 - source_y0).div_ceil(scale);
            TransportTile {
                tile_x,
                tile_y,
                source_x0,
                source_y0,
                width,
                height,
                rgba: vec![0; width as usize * height as usize * 4],
            }
        })
        .collect::<Vec<_>>();
    let union = outputs.iter().fold(
        (u32::MAX, u32::MAX, 0_u32, 0_u32),
        |(x0, y0, x1, y1), tile| {
            (
                x0.min(tile.source_x0),
                y0.min(tile.source_y0),
                x1.max(
                    tile.source_x0
                        .saturating_add(tile.width.saturating_mul(scale)),
                ),
                y1.max(
                    tile.source_y0
                        .saturating_add(tile.height.saturating_mul(scale)),
                ),
            )
        },
    );
    let native_tiles = layout.indices_intersecting(union, source_width, source_height);
    let native_pixels: u64 = native_tiles.iter().map(|&tile| {
        let (x0,y0,x1,y1) = layout.bounds(tile,source_width,source_height);
        u64::from((x1-x0).div_ceil(scale))*u64::from((y1-y0).div_ceil(scale))
    }).sum();
    let requested_pixels: u64 = outputs.iter().map(|tile| u64::from(tile.width)*u64::from(tile.height)).sum();
    let span = tracing::Span::current();
    span.record("native_tile_count", native_tiles.len());
    if native_tiles.len() > max_native_tiles || native_pixels > 8*1024*1024
        || native_pixels > requested_pixels.saturating_mul(16) {
        span.record("route", "roi_fallback");
        tracing::info!(
            native_tile_count = native_tiles.len(),
            max_native_tiles,
            native_pixels,
            requested_pixels,
            tile_width = layout.width,
            tile_height = layout.height,
            "native JPEG 2000 traversal is unsuitable for this viewport; using bounded ROI decode"
        );
        return Ok(None);
    }
    span.record("route", "native_tile");
    // Memory subsets are pinned by River's version snapshot; their lengths
    // vary by viewport and must not become distinct native-tile identities.
    let version = if memory.is_some() { Some((0,SystemTime::UNIX_EPOCH)) }
        else { std::fs::metadata(path).ok().and_then(|meta| meta.modified().ok().map(|mtime| (meta.len(),mtime))) };
    let decode_started = Instant::now();
    for native_tile in native_tiles {
        let key = version.map(|(size,modified)| NativeKey { snapshot: snapshot.into(), path: path.into(),
            size,modified,tile: native_tile,reduction,layers });
        let raw = NativeCache::shared().decode(key, || {
            let mut decoder = match first_decoder.take() {
                Some(decoder) => decoder,
                None => OpenJpegTileDecoder::open(path, reduction, layers, decoder_threads, memory)?,
            };
            if decoder.layout != layout { bail!("catalog native-tile layout does not match immutable source {}",path.display()); }
            decoder.decode_tile(native_tile)
        })?;
        let native_bounds = layout.bounds(native_tile, source_width, source_height);
        composite_native_tile(&mut outputs, &raw, native_bounds, scale)?;
    }
    let decode_ms = elapsed_ms(decode_started);
    outputs
        .into_iter()
        .map(|tile| {
            let encode_started = Instant::now();
            let bytes = encode_crop_rgba(
                &tile.rgba,
                tile.width,
                0,
                0,
                tile.width,
                tile.height,
                format,
            )?;
            Ok(DecodedTile { rgba:crate::decoder::take_pixel_capture(),
                tile_x: tile.tile_x,
                tile_y: tile.tile_y,
                raster: DecodedRaster {
                    width: tile.width,
                    height: tile.height,
                    bytes: bytes.into_boxed_slice().into(),
                    content_type: format.content_type(),
                    decoder_backend: "openjpeg", decoder_route: "native-tile",
                    decode_ms,
                    encode_ms: elapsed_ms(encode_started),
                    source_cache_status: "bypass",
                    source_cache_ms: 0.0,
                    compressed_resolution_view: false,
                    source_read_bytes: 0,
                    source_range_requests: 0,
                    source_cache_hit_bytes: 0,
                },
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

#[instrument(
    name = "river.codec.openjpeg.native_composite",
    skip(outputs, raw),
    fields(native_x0 = native_bounds.0, native_y0 = native_bounds.1, native_x1 = native_bounds.2, native_y1 = native_bounds.3, scale)
)]
fn composite_native_tile(
    outputs: &mut [TransportTile],
    raw: &RawNativeTile,
    native_bounds: (u32, u32, u32, u32),
    scale: u32,
) -> Result<()> {
    let (native_x0, native_y0, native_x1, native_y1) = native_bounds;
    let expected_width = (native_x1 - native_x0).div_ceil(scale);
    let expected_height = (native_y1 - native_y0).div_ceil(scale);
    if raw.width < expected_width || raw.height < expected_height {
        bail!(
            "native JPEG 2000 tile is smaller than its declared source bounds: decoded {}x{}, expected at least {}x{}",
            raw.width,
            raw.height,
            expected_width,
            expected_height
        );
    }
    for output in outputs {
        let output_x1 = output
            .source_x0
            .saturating_add(output.width.saturating_mul(scale));
        let output_y1 = output
            .source_y0
            .saturating_add(output.height.saturating_mul(scale));
        let x0 = output.source_x0.max(native_x0);
        let y0 = output.source_y0.max(native_y0);
        let x1 = output_x1.min(native_x1);
        let y1 = output_y1.min(native_y1);
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let copy_width = (x1 - x0)
            .div_ceil(scale)
            .min(raw.width.saturating_sub((x0 - native_x0) / scale))
            .min(output.width.saturating_sub((x0 - output.source_x0) / scale));
        let copy_height = (y1 - y0)
            .div_ceil(scale)
            .min(raw.height.saturating_sub((y0 - native_y0) / scale))
            .min(
                output
                    .height
                    .saturating_sub((y0 - output.source_y0) / scale),
            );
        for row in 0..copy_height {
            let source_offset = (((y0 - native_y0) / scale + row) * raw.width
                + (x0 - native_x0) / scale) as usize
                * 4;
            let destination_offset = (((y0 - output.source_y0) / scale + row) * output.width
                + (x0 - output.source_x0) / scale) as usize
                * 4;
            let bytes = copy_width as usize * 4;
            output.rgba[destination_offset..destination_offset + bytes]
                .copy_from_slice(&raw.rgba[source_offset..source_offset + bytes]);
        }
    }
    Ok(())
}

fn normalized_u8(value: i32, component: &opj::opj_image_comp_t) -> u8 {
    let precision = component.prec.clamp(1, 31);
    let maximum = (1_i64 << precision) - 1;
    let value = if component.sgnd != 0 {
        i64::from(value).saturating_add(1_i64 << (precision - 1))
    } else {
        i64::from(value)
    }
    .clamp(0, maximum);
    ((value * 255 + maximum / 2) / maximum) as u8
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_native_tiles_choose_roi_before_opening_the_source() {
        let result = super::decode_transport_tiles(std::path::Path::new("absent-huge-native.jp2"),
            8192,16384,&[(0,0)],512,0,0,super::RasterFormat::Png,1,4,
            Some(super::NativeTileLayoutHint { origin_x: 0,origin_y: 0,width: 8192,height: 8192,
                columns: 1,rows: 2 }),"snapshot",None).unwrap();
        assert!(result.is_none());
        // This case stays below the pixel cap but exceeds 16x requested work.
        let result = super::decode_transport_tiles(std::path::Path::new("absent-wide-native.jp2"),
            4096,4096,&[(0,0)],256,0,0,super::RasterFormat::Png,1,4,
            Some(super::NativeTileLayoutHint { origin_x: 0,origin_y: 0,width: 2048,height: 2048,
                columns: 2,rows: 2 }),"snapshot",None).unwrap();
        assert!(result.is_none());
    }
    #[test]
    fn memory_stream_checks_bounds_and_releases_its_buffer_lease() {
        let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from([1u8,2,3]);
        let data = Box::into_raw(Box::new(super::MemoryStream { bytes: bytes.clone(),position: 0 })).cast();
        let mut output = [0u8;8];
        unsafe {
            assert_eq!(super::memory_read(output.as_mut_ptr().cast(),8,data),3);
            assert_eq!(&output[..3],&[1,2,3]);
            assert_eq!(super::memory_read(output.as_mut_ptr().cast(),8,data),usize::MAX);
            assert_eq!(super::memory_skip(-2,data),-2);
            assert_eq!(super::memory_read(output.as_mut_ptr().cast(),8,data),2);
            assert_eq!(&output[..2],&[2,3]);
            assert_eq!(super::memory_seek(4,data),0);
            assert_eq!(super::memory_seek(-1,data),0);
            assert_eq!(super::memory_skip(-4,data),-1);
            assert_eq!(super::memory_seek(0,data),1);
            assert_eq!(std::sync::Arc::strong_count(&bytes),2);
            super::memory_free(data);
            assert_eq!(std::sync::Arc::strong_count(&bytes),1);
        }
    }
    #[test]
    fn native_cache_coalesces_misses_and_preserves_active_evicted_tiles() {
        use std::sync::atomic::{AtomicUsize,Ordering};
        let cache = super::NativeCache { limit: 4, state: std::sync::Mutex::new(super::NativeCacheState::default()),
            flights: std::sync::Mutex::new(std::collections::HashMap::new()) };
        let key = super::NativeKey { snapshot: "one".into(), path: "one.jp2".into(), size: 100,
            modified: std::time::SystemTime::UNIX_EPOCH, tile: 0, reduction: 0, layers: 1 };
        let count = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 { let cache=&cache; let key=key.clone(); let count=&count;
                scope.spawn(move || {
                    let tile=cache.decode(Some(key),|| { count.fetch_add(1,Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        Ok(super::RawNativeTile { width: 1,height: 1,rgba: vec![7;4] }) }).unwrap();
                    assert_eq!(tile.rgba,vec![7;4]);
                });
            }
        });
        assert_eq!(count.load(Ordering::SeqCst),1);
        let active=cache.get(&key).unwrap();
        for changed in 0..4 {
            let mut next=key.clone();
            match changed { 0=>next.layers=2, 1=>next.reduction=1, 2=>next.snapshot="two".into(),
                _=>next.modified+=std::time::Duration::from_secs(1) };
            assert!(cache.get(&next).is_none());
            cache.decode(Some(next),|| Ok(super::RawNativeTile { width: 1,height: 1,rgba: vec![9;4] })).unwrap();
        }
        assert!(cache.get(&key).is_none());
        assert_eq!(active.rgba,vec![7;4]);
        assert_eq!(cache.state.lock().unwrap().bytes,4);
    }
    use super::{NativeTileLayout, normalized_u8};
    use openjpeg_sys as opj;

    #[test]
    fn locates_only_the_native_tiles_touched_by_a_window() {
        let layout = NativeTileLayout {
            origin_x: 0,
            origin_y: 0,
            width: 1_024,
            height: 1_024,
            columns: 4,
            rows: 3,
        };
        assert_eq!(
            layout.indices_intersecting((900, 900, 2_100, 1_100), 4_000, 3_000),
            vec![0, 1, 2, 4, 5, 6]
        );
    }

    #[test]
    fn normalizes_unsigned_16_bit_native_samples() {
        let component = opj::opj_image_comp_t {
            prec: 16,
            sgnd: 0,
            ..unsafe { std::mem::zeroed() }
        };
        assert_eq!(normalized_u8(0, &component), 0);
        assert_eq!(normalized_u8(65_535, &component), 255);
    }
}
