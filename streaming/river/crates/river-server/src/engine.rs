//! Tile-engine boundary for River's raster-only gateway.
//!
//! An engine owns the source-specific work needed to turn an exact JPEG 2000
//! window into decoded raster tiles. The HTTP/WebSocket layer deliberately
//! knows nothing about local paths, object-store range reads, or JPIP. That
//! lets a future JPIP engine coexist with the direct tiled-JP2 engine without
//! changing the browser protocol.

use anyhow::{Context,Result};
#[cfg(feature = "nvjpeg2000")]
use anyhow::ensure;
use river_core::{DecoderDriver, RasterFormat};
#[cfg(feature = "nvjpeg2000")]
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
#[cfg(feature = "nvjpeg2000")]
use std::time::Duration;
#[cfg(feature = "nvjpeg2000")]
use tracing::info_span;
use tracing::instrument;

use crate::decoder::{DecodedTile, Jpeg2000Source};

#[derive(Clone, Debug)]
pub struct TileDecodeRequest {
    pub progressive_final_reduction: Option<u8>,
    pub output_geometry:Option<crate::edges::OutputGeometry>,
    pub edge_mode: crate::edges::Mode,
    pub source_id: String,
    pub webp_method: u8,
    pub capture_pixels: bool,
    pub whole_view: bool,
    pub source: Jpeg2000Source,
    pub tiles: Vec<(u32, u32)>,
    /// Encoded hits retained while only missing tiles enter source preparation.
    pub cached_tiles: Vec<DecodedTile>,
    pub(crate) work_ticket: Option<std::sync::Arc<crate::decoder::thread_budget::WorkTicket<'static>>>,
    pub source_tile: u32,
    pub reduction: u8,
    pub layers: u32,
    pub format: RasterFormat,
    /// Bound OpenJPEG's internal workers so River's shared decode scheduler
    /// remains the authoritative cross-client CPU budget.
    pub decoder_threads: usize,
    /// Native OpenJPEG tile traversal is selected only when the request
    /// touches a bounded number of codestream tiles.
    pub max_native_tiles: usize,
    pub cancellation: Option<tokio::sync::watch::Receiver<bool>>,
}

impl TileDecodeRequest {
    pub(crate) fn ensure_active(&self) -> Result<()> {
        anyhow::ensure!(self.cancellation.as_ref().is_none_or(|cancel| !*cancel.borrow()), "render request superseded");
        Ok(())
    }
}

/// Every driver receives the same on-demand compressed subset. Temporary
/// files remain alive until native/external decoder work has completed.
struct RemoteTileEngine { engine: std::sync::Arc<dyn TileEngine> }
fn oversized_gpu_grid(layout:Option<crate::decoder::NativeTileLayoutHint>)->bool {
    layout.is_some_and(|l|u64::from(l.width).saturating_mul(u64::from(l.height))
        .saturating_mul(u64::from(l.columns)).saturating_mul(u64::from(l.rows))>64*1024*1024)
}
impl TileEngine for RemoteTileEngine {
    fn name(&self) -> &'static str { self.engine.name() }
    fn accepts_memory_source(&self) -> bool { self.engine.accepts_memory_source() }
    fn requires_complete_tile_grid(&self) -> bool { self.engine.requires_complete_tile_grid() }
    fn prepare(&self, mut request: TileDecodeRequest) -> Result<TileDecodeRequest> {
        if request.work_ticket.is_none() && !request.tiles.is_empty() {
            request.work_ticket=Some(crate::decoder::thread_budget::request_ticket());
        }
        let Some(remote) = request.source.remote.take() else { return Ok(request); };
        let file_ranges=remote.local_path.is_some();
        let pcrl_lowest_revision=remote.header.progression==3 && remote.header.layers==1 && !remote.header.is_ht
            && request.reduction.saturating_add(1)==remote.header.levels
            && std::env::var("RIVER_S3_MULTIPART_PLT").is_ok_and(|v|v=="true" || v=="1");
        if remote.header.is_ht && self.engine.name() == "direct-jpeg2000" {
            anyhow::bail!("HTJ2K input needs JP2Grok or nvJPEG2000; OpenJPEG cannot decode its HT blocks");
        }
        let started = std::time::Instant::now();
        let original_path = request.source.path.clone();
        let original_tiles = request.tiles.clone();
        let mut context = crate::remote_source::ReadContext::new(request.cancellation.clone());
        if matches!(self.engine.name(), "direct-jpeg2000" | "jp2grok") && request.whole_view {
            context.progressive_final_reduction = request.progressive_final_reduction;
        }
        if let Some(file) = if pcrl_lowest_revision || file_ranges {None}else{remote.cached_compatibility_file(&mut context)} {
            request.ensure_active()?;
            request.source.path = file.path().to_path_buf();
            request.source.prepared = Some(std::sync::Arc::new(crate::decoder::PreparedIo {file_ranges,
                view_reduction:0,pcrl_lowest_revision,technical_origin:None,predecoded:None,overlapped_decode_ms:0.0,predecoded_origin:(0,0),
                original_path, original_tiles, tile_offset_x: 0, tile_offset_y: 0,
                route: if crate::storage_mode::disk_cache_enabled() { "s3-disk-compatibility" } else { "s3-ram-compatibility" }, stats: context.stats,
                elapsed_ms: crate::decoder::elapsed_ms(started), _file: Some(file),
            }));
            return Ok(request);
        }
        #[cfg(all(feature="grok",unix))]
        if self.engine.name()=="openhtj2k-hybrid" {remote.start_pipeline(&mut context);}
        let compact=if self.engine.requires_complete_tile_grid() && request.whole_view && request.tiles.len()==1 && request.reduction>=3 {
            remote.prepare_gpu_preview(request.reduction,request.layers,&mut context)?
        }else{None};
        // nvJPEG2000's original-grid parser can consume gigabytes even for a
        // tiny ROI. Classical enormous tiles use the bounded CPU ROI path;
        // proven compact LL views still go directly to CUDA.
        let complete_grid=self.engine.requires_complete_tile_grid()
            && (remote.header.is_ht || !oversized_gpu_grid(Some(remote.header.layout)));
        let prepared = match compact.map(Ok).unwrap_or_else(||remote.prepare(&request.tiles, request.source_tile, request.reduction, request.layers, request.whole_view, complete_grid, &mut context)) {
            Ok(prepared) => prepared,
            Err(error) if compatibility_error(&error) => {
                if file_ranges {
                    tracing::debug!(%error,"local selective view unsupported; decoding original file");
                    request.source.prepared=None;
                    return Ok(request);
                }
                tracing::warn!(source = %remote.id, reason = %error, "selective read unavailable; using disk-backed compatibility decode");
                let file = remote.compatibility_file(&mut context).with_context(||format!("Selective S3 preparation failed ({error:#}); disk-backed fallback is unavailable"))?;
                request.source.path = file.path().to_path_buf();
                request.source.prepared = Some(std::sync::Arc::new(crate::decoder::PreparedIo {file_ranges,
                view_reduction:0,pcrl_lowest_revision,technical_origin:None,predecoded:None,overlapped_decode_ms:0.0,predecoded_origin:(0,0),
                    original_path, original_tiles, tile_offset_x: 0, tile_offset_y: 0,
                    route: if crate::storage_mode::disk_cache_enabled() { "s3-disk-compatibility" } else { "s3-ram-compatibility" }, stats: context.stats,
                    elapsed_ms: crate::decoder::elapsed_ms(started), _file: Some(file),
                }));
                return Ok(request);
            }
            Err(error) => return Err(error),
        };
        #[cfg(all(feature="grok",unix))] let predecoded=context.take_pipeline_output();
        #[cfg(not(all(feature="grok",unix)))] let predecoded=None;
        let (predecoded,overlapped_decode_ms,predecoded_origin)=predecoded.map(|(image,ms,origin)|(Some(image),ms,origin)).unwrap_or((None,0.0,(0,0)));
        let route = prepared.route;
        let technical_origin=prepared.technical_origin;
        let view_reduction=prepared.view_reduction;
        let tile_offset_x = prepared.offset_x / request.source_tile;
        let tile_offset_y = prepared.offset_y / request.source_tile;
        request.source.width = prepared.width; request.source.height = prepared.height;
        request.source.native_tile_layout = Some(prepared.layout);
        for (x, y) in &mut request.tiles { *x -= tile_offset_x; *y -= tile_offset_y; }
        let file = if self.engine.accepts_memory_source() {
            request.source.memory = Some(std::sync::Arc::from(prepared.bytes));
            None
        } else {
            let file = prepared.materialize()?;
            request.source.path = file.path().to_path_buf();
            Some(std::sync::Arc::new(file))
        };
        context.check()?;
        let read_ms = crate::decoder::elapsed_ms(started);
        request.source.prepared = Some(std::sync::Arc::new(crate::decoder::PreparedIo {file_ranges,
            view_reduction,pcrl_lowest_revision,technical_origin,predecoded,overlapped_decode_ms,predecoded_origin,
            original_path, original_tiles, tile_offset_x, tile_offset_y, route, stats: context.stats, elapsed_ms: read_ms, _file: file,
        }));
        Ok(request)
    }
    fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
        let request = self.prepare(request)?;
        let info = request.source.prepared.clone();
        let mut tiles = self.engine.decode(request)?;
        let Some(info) = info else { return Ok(tiles); };
        for tile in &mut tiles {
            tile.tile_x += info.tile_offset_x; tile.tile_y += info.tile_offset_y;
            tile.raster.decoder_route = if matches!(tile.raster.decoder_route,"openjpeg-multispectral-fallback"|"openjpeg-declared-alpha-fallback"|"jp2grok-native-multispectral-rgb"|"openht-worker-failure-grok"|"openht-worker-failure-grok-cli") {tile.raster.decoder_route} else if tile.raster.decoder_route=="openjpeg-gpu-large-grid-fallback" {"openjpeg-gpu-large-grid-fallback"} else if tile.raster.decoder_route=="openjpeg-parallel-reduced" {"openjpeg-parallel-reduced-resolution-view"} else if tile.raster.decoder_route=="openht-pipeline" {"openht-pipeline-ranges"} else if tile.raster.decoder_route=="openht-packed" {"openht-packed-ranges"} else if tile.raster.decoder_route=="openjpeg-pcrl-lowest-compatibility" {
                match info.route {"s3-packet-ranges"=>"openjpeg-pcrl-lowest-s3-packet-ranges","s3-disk-compatibility"=>"openjpeg-pcrl-lowest-s3-disk-compatibility",_=>"openjpeg-pcrl-lowest-s3-tile-part-ranges"}
            } else if tile.raster.decoder_route=="openht-fallback-jp2grok" {
                match info.route {
                    "s3-packet-ranges"=>"openht-fallback-s3-packet-ranges",
                    "s3-tile-part-ranges"=>"openht-fallback-s3-tile-part-ranges",
                    "s3-disk-compatibility"=>"openht-fallback-s3-disk-compatibility",
                    route=>route,
                }
            } else {info.route};
            if info.file_ranges {
                tile.raster.decoder_route=match tile.raster.decoder_route {
                    "s3-single-resolution-prefix"=>"file-single-resolution-prefix",
                    "s3-reduced-resolution-view"=>"file-reduced-resolution-view",
                    "openjpeg-parallel-reduced-resolution-view"=>"openjpeg-parallel-file-view",
                    "s3-precinct-window-ranges"=>"file-precinct-window-ranges",
                    "s3-packet-ranges"=>"file-packet-ranges",
                    "s3-tile-part-ranges"=>"file-tile-part-ranges",
                    _=>"file-range-view",
                };
            }
            tile.raster.source_cache_status = if info.file_ranges {
                if info.stats.bytes==0 {"file-range-hit"}else{"file-range-miss"}
            } else if info.stats.bytes == 0 { "s3-range-hit" } else { "s3-range-miss" };
            tile.raster.source_cache_ms = info.elapsed_ms;
            tile.raster.source_read_bytes = info.stats.bytes;
            tile.raster.source_range_requests = info.stats.requests;
            tile.raster.source_cache_hit_bytes = info.stats.cache_bytes;
        }
        Ok(tiles)
    }
}

fn compatibility_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    ["S3 compressed-read budget exceeded", "S3 range request limit exceeded",
        "remote tile subset requires a zero-origin tile grid",
        "GPU range subset needs a rectangular", "GPU range subset is not aligned"]
        .iter().any(|reason| message.starts_with(reason))
}

/// The stable server-side extension boundary. Every implementation returns
/// River's canonical decoded raster, never a browser-visible codestream.
pub trait TileEngine: Send + Sync {
    fn flight(&self, _request: &TileDecodeRequest) -> Option<std::sync::Arc<tokio::sync::Mutex<()>>> { None }
    fn name(&self) -> &'static str;
    fn accepts_memory_source(&self) -> bool { false }
    fn requires_complete_tile_grid(&self) -> bool { false }
    fn cached(&self, _request: &TileDecodeRequest) -> Option<Vec<DecodedTile>> { None }
    fn prepare(&self, request: TileDecodeRequest) -> Result<TileDecodeRequest> { Ok(request) }
    fn probe_shared(&self, request: TileDecodeRequest) -> TileDecodeRequest { request }
    fn prepare_after_shared(&self, request: TileDecodeRequest) -> Result<TileDecodeRequest> { self.prepare(request) }
    fn cache_encoded(&self,_request:&TileDecodeRequest,_tiles:&[DecodedTile]) {}
    fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>>;
}

pub fn prepare_admitted(engine: &dyn TileEngine, request: TileDecodeRequest,
    permit: crate::adaptation::Permit,
    flight: Option<tokio::sync::OwnedMutexGuard<()>>,
    shared_checked: bool) -> Result<(TileDecodeRequest, crate::adaptation::Permit, Option<tokio::sync::OwnedMutexGuard<()>>)> {
    // Keep admission alive inside blocking preparation even if its async
    // caller disappears. Successful buffers retain it until CPU admission.
    request.ensure_active()?;
    let prepared = if shared_checked { engine.prepare_after_shared(request) } else { engine.prepare(request) };
    prepared.map(|request| (request, permit, flight))
}

struct RasterCachedEngine { engine: std::sync::Arc<dyn TileEngine>, cache: std::sync::Arc<crate::raster_cache::RasterCache> }
impl RasterCachedEngine {
    fn retain_cached(&self, mut request: TileDecodeRequest) -> TileDecodeRequest {
        // GPU subset assembly may need the complete rectangle. Punching cache
        // holes into it can turn a selective read into a whole-source fallback.
        if self.engine.requires_complete_tile_grid() || request.source.prepared.is_some() || !request.cached_tiles.is_empty() { return request; }
        let original_count = request.tiles.len();
        let mut missing = Vec::with_capacity(original_count);
        let mut single = request.clone();
        for &position in &request.tiles {
            single.tiles = vec![position];
            if let Some(mut hits) = self.cache.get(self.name(), &single) { request.cached_tiles.append(&mut hits); }
            else { missing.push(position); }
        }
        request.whole_view &= missing.len() == original_count;
        request.tiles = missing;
        request
    }

    fn prepare_inner(&self, request: TileDecodeRequest, probe: bool) -> Result<TileDecodeRequest> {
        request.ensure_active()?;
        anyhow::ensure!(!self.engine.requires_complete_tile_grid() || request.source.reference_origin == (0,0),
            "non-zero image reference origins require OpenJPEG or JP2Grok");
        let request = self.retain_cached(request);
        let request = if probe { self.probe_shared(request) } else { request };
        if request.tiles.is_empty() { return Ok(request); }
        let _memory=crate::resources::admit(&request)?;
        self.engine.prepare(request)
    }
}
impl TileEngine for RasterCachedEngine {
    fn flight(&self, request: &TileDecodeRequest) -> Option<std::sync::Arc<tokio::sync::Mutex<()>>> { self.cache.flight(self.name(), request) }
    fn cache_encoded(&self,request:&TileDecodeRequest,tiles:&[DecodedTile]) {self.cache.put(self.name(),request,tiles);}
    fn name(&self) -> &'static str { self.engine.name() }
    fn accepts_memory_source(&self) -> bool { self.engine.accepts_memory_source() }
    fn cached(&self, request: &TileDecodeRequest) -> Option<Vec<DecodedTile>> {
        if request.tiles.is_empty() && !request.cached_tiles.is_empty() {
            let mut tiles = request.cached_tiles.clone();
            tiles.sort_unstable_by_key(|tile| (tile.tile_y, tile.tile_x));
            return Some(tiles);
        }
        let mut tiles = self.cache.get(self.name(), request)?;
        if !request.cached_tiles.is_empty() {
            tiles.extend(request.cached_tiles.iter().cloned());
            tiles.sort_unstable_by_key(|tile| (tile.tile_y, tile.tile_x));
        }
        Some(tiles)
    }
    fn prepare(&self, request: TileDecodeRequest) -> Result<TileDecodeRequest> {
        self.prepare_inner(request, true)
    }
    fn prepare_after_shared(&self, request: TileDecodeRequest) -> Result<TileDecodeRequest> {
        self.prepare_inner(request, false)
    }
    fn probe_shared(&self, mut request: TileDecodeRequest) -> TileDecodeRequest {
        if request.tiles.is_empty() || request.ensure_active().is_err() ||
            (self.engine.requires_complete_tile_grid() && request.source.reference_origin != (0,0)) {
            return request;
        }
        if let Some(mut tiles) = self.cache.get_shared(self.name(), &request) {
            if !self.engine.requires_complete_tile_grid() || tiles.len() == request.tiles.len() {
                request.tiles.retain(|&(x, y)| !tiles.iter().any(|tile| tile.tile_x == x && tile.tile_y == y));
                request.whole_view = false;
                request.cached_tiles.append(&mut tiles);
            }
        }
        request
    }
    fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
        let _encoding = crate::encoding_settings::Scope::new(request.webp_method);
        let _capture = crate::decoder::PixelCapture::new(request.capture_pixels);
        let _edges=crate::edges::Scope::with_output(request.edge_mode,request.source_tile,request.reduction,request.output_geometry);
        request.ensure_active()?;
        anyhow::ensure!(!self.engine.requires_complete_tile_grid() || request.source.reference_origin == (0,0),
            "non-zero image reference origins require OpenJPEG or JP2Grok");
        if let Some(tiles) = self.cached(&request) {
            return Ok(tiles);
        }
        let request = self.retain_cached(request);
        if request.tiles.is_empty() { return Ok(request.cached_tiles); }
        let _memory=crate::resources::admit(&request)?;
        let key_request = request.clone();
        let cached_tiles = request.cached_tiles.clone();
        let nominal_edge=request.source_tile.div_ceil(1u32.checked_shl(u32::from(request.reduction)).unwrap_or(u32::MAX));
        let padded=request.edge_mode!=crate::edges::Mode::Crop;
        let mut tiles = self.engine.decode(request)?;
        if let Some(g)=key_request.output_geometry {
            for tile in &mut tiles {tile.raster.width=if padded {g.edge}else{g.width};tile.raster.height=if padded {g.edge}else{g.height};}
            self.cache.put(self.name(),&key_request,&tiles);
        } else if padded {
            let mut full=Vec::new();let mut edges=Vec::new();
            for tile in &mut tiles {
                if tile.raster.width==nominal_edge && tile.raster.height==nominal_edge
                    && crate::raster_cache::full_tile(&key_request,(tile.tile_x,tile.tile_y)) {full.push(tile.clone());}
                else {tile.raster.width=nominal_edge;tile.raster.height=nominal_edge;edges.push(tile.clone());}
            }
            self.cache.put(self.name(),&key_request,&edges);
            let mut natural=key_request;natural.edge_mode=crate::edges::Mode::Crop;
            self.cache.put(self.name(),&natural,&full);
        } else {self.cache.put(self.name(), &key_request, &tiles);}
        tiles.extend(cached_tiles);
        tiles.sort_unstable_by_key(|tile| (tile.tile_y,tile.tile_x));
        Ok(tiles)
    }
}

struct CachedTileEngine {
    engine: std::sync::Arc<dyn TileEngine>,
    cache: std::sync::Arc<crate::source_cache::SourceCache>,
}
impl TileEngine for CachedTileEngine {
    fn name(&self) -> &'static str {
        self.engine.name()
    }
    fn accepts_memory_source(&self) -> bool { self.engine.accepts_memory_source() }
    fn requires_complete_tile_grid(&self) -> bool { self.engine.requires_complete_tile_grid() }
    fn decode(&self, mut request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
        if request.source.memory.is_some() || request.source.prepared.as_ref().is_some_and(|p|p.file_ranges) { return self.engine.decode(request); }
        let acquire_started = std::time::Instant::now();
        let source_path = request.source.path.clone();
        let (lease, can_stage) = match self.cache.ready(&source_path) {
            Ok(lookup) => lookup,
            Err(error) => {
                tracing::warn!(%error, "local source cache unavailable; decoding original source");
                (None, false)
            }
        };
        if let Some(file) = &lease {
            request.source.path = file.path.clone();
        }
        let acquire_ms = crate::decoder::elapsed_ms(acquire_started);
        let status = if lease.is_some() {
            "ready"
        } else if can_stage {
            "warming"
        } else {
            "bypass"
        };
        let mut result = self.engine.decode(request);
        if let Ok(tiles) = &mut result {
            let compact_only = !tiles.is_empty()
                && tiles
                    .iter()
                    .all(|tile| tile.raster.compressed_resolution_view);
            for tile in tiles {
                tile.raster.source_cache_status = if tile.raster.compressed_resolution_view {
                    "compressed-view"
                } else {
                    status
                };
                tile.raster.source_cache_ms = acquire_ms;
            }
            if can_stage && lease.is_none() && !compact_only {
                self.cache.schedule(source_path);
            }
        }
        drop(lease);
        result
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecoderDriverInfo {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub progressive_quality: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone)]
pub struct DecoderDrivers {
    engines: HashMap<DecoderDriver, std::sync::Arc<dyn TileEngine>>,
    info: Vec<DecoderDriverInfo>,
    enabled: std::sync::Arc<std::sync::RwLock<Option<Vec<DecoderDriver>>>>,
}

#[cfg(feature = "grok")]
#[derive(Default)]
pub struct GrokJpeg2000TileEngine {pub openht:bool}

#[cfg(feature = "grok")]
impl TileEngine for GrokJpeg2000TileEngine {
    fn accepts_memory_source(&self) -> bool { self.openht || crate::decoder::grok::native_enabled() }
    fn name(&self) -> &'static str {
        if self.openht {"openhtj2k-hybrid"} else {"jp2grok"}
    }

    #[instrument(
        name = "river.engine.grok.decode",
        skip(self, request),
        fields(tile_count = request.tiles.len(), source_width = request.source.width, source_height = request.source.height)
    )]
    fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
        let _cancel=crate::decoder::grok::cancellation_scope(request.cancellation.clone());
        crate::decoder::grok::decode_tiles(
            &request.source,
            &request.tiles,
            request.source_tile,
            request.reduction,
            request.layers,
            request.format,
            self.openht,
        )
    }
}

#[cfg(feature = "nvjpeg2000")]
#[derive(Clone)]
pub struct NvJpeg2000TileEngine {
    endpoint: String,
    client: reqwest::blocking::Client,
    rpc_codec: String,
    #[cfg(target_os = "linux")]
    native_fine: Option<std::sync::Arc<crate::cuda_native::NativeFine>>,
}

#[cfg(feature = "nvjpeg2000")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NvDecodeRequest {
    source_path: String,
    source_width: u32,
    source_height: u32,
    source_tile: u32,
    reduction: u8,
    tiles: Vec<(u32, u32)>,
    rpc_codec: String,
}

#[cfg(feature = "nvjpeg2000")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NvDecodeResponse {
    decode_ms: f64,
    queue_ms: f64,
    timing: NvPipelineTiming,
    tiles: Vec<NvDecodedTile>,
}

#[cfg(feature = "nvjpeg2000")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NvPipelineTiming {
    parse_ms: f64,
    native_decode_ms: f64,
    raster_ms: f64,
    native_tile_count: usize,
    #[serde(default)]
    resolution_view_ms: f64,
    #[serde(default)]
    view_bytes: usize,
    #[serde(default)]
    read_bytes: usize,
    #[serde(default)]
    resolution_view: bool,
    #[serde(default)]
    batch_requests: usize,
    #[serde(default)]
    batch_outputs: usize,
    #[serde(default)]
    native_submit_ms: f64,
    #[serde(default)]
    native_wait_ms: f64,
    #[serde(default)]
    serialize_ms: f64,
}

#[cfg(feature = "nvjpeg2000")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NvDecodedTile {
    tile_x: u32,
    tile_y: u32,
    width: u32,
    height: u32,
    byte_length: usize,
    #[serde(default)]
    pixel_encoding: String,
}

#[cfg(feature = "nvjpeg2000")]
impl NvJpeg2000TileEngine {
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("RIVER_NVJPEG2000_URL")
            .context("RIVER_NVJPEG2000_URL must point to the nvJPEG2000 sidecar")?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        client
            .get(format!("{}/health", base_url.trim_end_matches('/')))
            .send()
            .context("nvJPEG2000 sidecar health check failed")?
            .error_for_status()
            .context("nvJPEG2000 sidecar is unhealthy")?;
        Ok(Self {
            endpoint: format!("{}/decode", base_url.trim_end_matches('/')),
            client,
            rpc_codec: std::env::var("RIVER_NVJPEG2000_RPC_CODEC").unwrap_or_else(|_| "lz4".into()),
            #[cfg(target_os = "linux")]
            native_fine: if std::env::var("RIVER_NVJPEG2000_NATIVE_FINE").as_deref() == Ok("false") { None } else {
                match crate::cuda_native::NativeFine::new() {
                    Ok(engine) => Some(std::sync::Arc::new(engine)),
                    Err(error) => { tracing::warn!(%error,"direct CUDA fine decoder unavailable; retaining sidecar"); None },
                }
            },
        })
    }
}

#[cfg(any(feature="nvjpeg2000",test))]
fn bounded_gpu_cpu_view(source:&Jpeg2000Source)->bool {
    // Only views produced by validated classical ROI/prefix planners can use
    // OpenJPEG here. Do not redirect arbitrary HTJ2K sources to that decoder.
    source.prepared.as_ref().is_some_and(|p|
        // Sparse compact views deliberately omit unrelated native tiles.
        // The GPU parser requires a complete grid, even after projection.
        p.route=="s3-multipart-partial-compact-prefix" ||
        (oversized_gpu_grid(source.native_tile_layout) &&
        (p.technical_origin.is_some() || p.route=="s3-streamed-plt-roi" || p.route=="s3-multipart-prefix-ranges"
            || p.route=="s3-multipart-compact-lowest-prefix")))
}

#[cfg(any(feature="nvjpeg2000",test))]
fn compact_gpu_request(mut request:TileDecodeRequest)->Result<TileDecodeRequest> {
    let reduction=request.source.prepared.as_ref().map_or(0,|p|p.view_reduction);
    if reduction==0 {return Ok(request);}
    let scale=1u32.checked_shl(u32::from(reduction)).context("invalid GPU view reduction")?;
    anyhow::ensure!(request.source.reference_origin==(0,0)
        && request.source.prepared.as_ref().is_none_or(|p|p.technical_origin.is_none())
        && request.source_tile%scale==0,"GPU compact view requires aligned zero-origin geometry");
    request.reduction=request.reduction.checked_sub(reduction).context("GPU view exceeds requested reduction")?;
    request.source_tile/=scale;
    request.source.width=request.source.width.div_ceil(scale);
    request.source.height=request.source.height.div_ceil(scale);
    if let Some(layout)=&mut request.source.native_tile_layout {
        layout.width=layout.width.div_ceil(scale);layout.height=layout.height.div_ceil(scale);
    }
    Ok(request)
}

#[cfg(test)] mod compact_gpu_tests {
    use super::*;
    fn request(view:u8)->TileDecodeRequest {
        let mut source=Jpeg2000Source::from_path("compact-test.j2k",65536,163840);
        source.prepared=Some(std::sync::Arc::new(crate::decoder::PreparedIo {
            file_ranges:false,view_reduction:view,pcrl_lowest_revision:false,technical_origin:None,
            predecoded:None,overlapped_decode_ms:0.0,predecoded_origin:(0,0),original_path:source.path.clone(),
            original_tiles:vec![(0,0)],tile_offset_x:0,tile_offset_y:0,route:"s3-single-resolution-prefix",
            stats:Default::default(),elapsed_ms:0.0,_file:None,
        }));
        TileDecodeRequest {progressive_final_reduction:None, output_geometry:None,edge_mode:crate::edges::Mode::Crop,source_id:"compact-test".into(),
            webp_method:0,capture_pixels:false,whole_view:true,source,tiles:vec![(0,0)],cached_tiles:vec![],work_ticket:None,
            source_tile:262144,reduction:6,layers:0,format:RasterFormat::Png,decoder_threads:1,max_native_tiles:4,cancellation:None}
    }
    #[test] fn compact_preview_uses_encoded_geometry_and_preserves_output_coordinates() {
        let r=compact_gpu_request(request(6)).unwrap();assert_eq!((r.source.width,r.source.height,r.source_tile,r.reduction),(1024,2560,4096,0));assert_eq!(r.tiles,vec![(0,0)]);
        let r=compact_gpu_request(request(5)).unwrap();assert_eq!((r.source.width,r.source.height,r.source_tile,r.reduction),(2048,5120,8192,1));
    }
    #[test] fn compact_preview_rejects_misalignment_and_excessive_reduction() {
        let mut r=request(6);r.source_tile+=1;assert!(compact_gpu_request(r).is_err());
        let mut r=request(6);r.reduction=5;assert!(compact_gpu_request(r).is_err());
        let mut r=request(6);r.source.reference_origin=(1,0);assert!(compact_gpu_request(r).is_err());
        let r=compact_gpu_request(request(0)).unwrap();assert_eq!(r.source.width,65536);
    }
    #[test] fn gpu_guard_uses_native_grid_and_accepts_compact_geometry() {
        let large=crate::decoder::NativeTileLayoutHint {origin_x:0,origin_y:0,width:65536,height:163840,columns:1,rows:1};
        assert!(oversized_gpu_grid(Some(large)));
        let mut r=request(6);r.source.native_tile_layout=Some(large);
        let r=compact_gpu_request(r).unwrap();assert!(!oversized_gpu_grid(r.source.native_tile_layout));
        assert!(oversized_gpu_grid(Some(crate::decoder::NativeTileLayoutHint {width:1024,height:1024,columns:41,rows:11,..large})));
        assert!(!oversized_gpu_grid(Some(crate::decoder::NativeTileLayoutHint {width:1024,height:1024,columns:8,rows:8,..large})));
        assert!(oversized_gpu_grid(Some(crate::decoder::NativeTileLayoutHint {width:u32::MAX,height:u32::MAX,columns:u32::MAX,rows:u32::MAX,..large})));
    }
    #[test] fn cpu_fallback_requires_a_proven_classical_roi_view() {
        let mut r=request(0);r.source.native_tile_layout=Some(crate::decoder::NativeTileLayoutHint {
            origin_x:0,origin_y:0,width:65536,height:163840,columns:1,rows:1});
        assert!(!bounded_gpu_cpu_view(&r.source));
        std::sync::Arc::get_mut(r.source.prepared.as_mut().unwrap()).unwrap().technical_origin=Some((32768,79872));
        assert!(bounded_gpu_cpu_view(&r.source));
    }
    #[test] fn sparse_compact_gpu_views_use_cpu_even_after_projection() {
        let mut r=request(6);
        r.source.native_tile_layout=Some(crate::decoder::NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:64,rows:64});
        std::sync::Arc::get_mut(r.source.prepared.as_mut().unwrap()).unwrap().route="s3-multipart-partial-compact-prefix";
        let r=compact_gpu_request(r).unwrap();
        assert!(!oversized_gpu_grid(r.source.native_tile_layout));
        assert!(bounded_gpu_cpu_view(&r.source));
    }
    #[test] fn large_complete_compact_grids_use_bounded_cpu_before_gpu_rebasing() {
        let mut r=request(6);
        r.source.native_tile_layout=Some(crate::decoder::NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:64,rows:64});
        std::sync::Arc::get_mut(r.source.prepared.as_mut().unwrap()).unwrap().route="s3-multipart-compact-lowest-prefix";
        assert!(bounded_gpu_cpu_view(&r.source));
        r.source.native_tile_layout.as_mut().unwrap().columns=8;
        r.source.native_tile_layout.as_mut().unwrap().rows=8;
        assert!(!bounded_gpu_cpu_view(&r.source));
    }
}

#[cfg(feature = "nvjpeg2000")]
impl TileEngine for NvJpeg2000TileEngine {
    fn name(&self) -> &'static str {
        "nvjpeg2000-cuda"
    }
    fn requires_complete_tile_grid(&self) -> bool { true }

    #[instrument(
        name = "river.engine.nvjpeg2000.decode",
        skip(self, request),
        fields(tile_count = request.tiles.len(), source_width = request.source.width, source_height = request.source.height, reduction = request.reduction)
    )]
    fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
        if bounded_gpu_cpu_view(&request.source) {
            let mut output=DirectJpeg2000TileEngine.decode(request)?;
            for tile in &mut output {tile.raster.decoder_route="openjpeg-gpu-large-grid-fallback";}
            return Ok(output);
        }
        let request=compact_gpu_request(request)?;
        #[cfg(target_os = "linux")]
        if let Some(engine) = &self.native_fine {
            if let Some(output) = engine.decode(&request)? { return Ok(output); }
        }
        let http_span = info_span!(
            "river.sidecar.http",
            output_bytes = tracing::field::Empty,
            sidecar_queue_ms = tracing::field::Empty,
            parse_ms = tracing::field::Empty,
            native_decode_ms = tracing::field::Empty,
            raster_ms = tracing::field::Empty,
            native_tile_count = tracing::field::Empty,
            resolution_view_ms = tracing::field::Empty,
            view_bytes = tracing::field::Empty,
            source_read_bytes = tracing::field::Empty,
            resolution_view = tracing::field::Empty,
            batch_requests = tracing::field::Empty,
            batch_outputs = tracing::field::Empty,
            native_submit_ms = tracing::field::Empty, native_wait_ms = tracing::field::Empty,
            sidecar_serialize_ms = tracing::field::Empty, rpc_headers_ms = tracing::field::Empty,
            rpc_body_ms = tracing::field::Empty
        );
        let http_entered = http_span.enter();
        let headers_started=std::time::Instant::now();
        let response = self
            .client
            .post(&self.endpoint)
            .json(&NvDecodeRequest {
                source_path: request.source.path.to_string_lossy().into_owned(),
                source_width: request.source.width,
                source_height: request.source.height,
                source_tile: request.source_tile,
                reduction: request.reduction,
                tiles: request.tiles.clone(),
                rpc_codec: self.rpc_codec.clone(),
            })
            .send()
            .context("nvJPEG2000 CUDA service request failed")?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().unwrap_or_default();
            anyhow::bail!("nvJPEG2000 CUDA service rejected the decode request ({status}): {}", detail.chars().take(4096).collect::<String>());
        }
        http_span.record("rpc_headers_ms", crate::decoder::elapsed_ms(headers_started));
        ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                == Some("application/x-river-rgba"),
            "nvJPEG2000 CUDA service returned an unexpected content type"
        );
        let body_started=std::time::Instant::now();
        let body = response
            .bytes()
            .context("unable to read CUDA raster response")?;
        http_span.record("rpc_body_ms", crate::decoder::elapsed_ms(body_started));
        http_span.record("output_bytes", body.len());
        ensure!(body.len() >= 4, "CUDA raster header is truncated");
        let header_length = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
        ensure!(
            header_length <= 65536 && header_length <= body.len() - 4,
            "CUDA raster metadata length is invalid"
        );
        let result: NvDecodeResponse = serde_json::from_slice(&body[4..4 + header_length])
            .context("CUDA raster metadata is invalid")?;
        http_span.record("sidecar_queue_ms", result.queue_ms);
        http_span.record("parse_ms", result.timing.parse_ms);
        http_span.record("native_decode_ms", result.timing.native_decode_ms);
        http_span.record("raster_ms", result.timing.raster_ms);
        http_span.record("native_tile_count", result.timing.native_tile_count);
        http_span.record("resolution_view_ms", result.timing.resolution_view_ms);
        http_span.record("view_bytes", result.timing.view_bytes);
        http_span.record("source_read_bytes", result.timing.read_bytes);
        http_span.record("resolution_view", result.timing.resolution_view);
        http_span.record("batch_requests", result.timing.batch_requests);
        http_span.record("batch_outputs", result.timing.batch_outputs);
        http_span.record("native_submit_ms", result.timing.native_submit_ms);
        http_span.record("native_wait_ms", result.timing.native_wait_ms);
        http_span.record("sidecar_serialize_ms", result.timing.serialize_ms);
        drop(http_entered);
        drop(http_span);
        ensure!(
            result.tiles.len() == request.tiles.len(),
            "CUDA service returned an unexpected tile count"
        );
        let mut offset = 4 + header_length;
        let mut output = Vec::with_capacity(result.tiles.len());

        for (tile, expected_position) in result.tiles.into_iter().zip(&request.tiles) {
            ensure!(
                (tile.tile_x, tile.tile_y) == *expected_position,
                "CUDA service returned an unexpected tile position"
            );
            let expected_length = (tile.width as usize)
                .checked_mul(tile.height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .context("CUDA raster dimensions overflow")?;
            ensure!(
                tile.width > 0
                    && tile.height > 0
                    && expected_length <= 134217728
                    && tile.byte_length <= body.len().saturating_sub(offset),
                "nvJPEG2000 CUDA service returned an incomplete raster tile"
            );
            let unpack_span=info_span!("river.sidecar.unpack",codec=tile.pixel_encoding,output_bytes=expected_length);
            let unpack_entered=unpack_span.enter();
            let payload = &body[offset..offset + tile.byte_length];
            let rgba: std::borrow::Cow<'_, [u8]> = match tile.pixel_encoding.as_str() {
                "" | "raw" => { ensure!(payload.len()==expected_length,"raw CUDA raster length mismatch"); std::borrow::Cow::Borrowed(payload) },
                "lz4" => std::borrow::Cow::Owned(lz4_flex::block::decompress(payload,expected_length).context("invalid CUDA LZ4 raster")?),
                _ => anyhow::bail!("unsupported CUDA raster encoding"),
            };
            ensure!(rgba.len()==expected_length,"decompressed CUDA raster length mismatch");
            drop(unpack_entered);
            drop(unpack_span);
            offset += tile.byte_length;
            let encode_started = std::time::Instant::now();
            let bytes = crate::decoder::encode_crop_rgba(
                &rgba,
                tile.width,
                0,
                0,
                tile.width,
                tile.height,
                request.format,
            )?;
            output.push(DecodedTile { rgba:crate::decoder::take_pixel_capture(),
                tile_x: tile.tile_x,
                tile_y: tile.tile_y,
                raster: crate::decoder::DecodedRaster {
                    width: tile.width,
                    height: tile.height,
                    bytes: bytes.into_boxed_slice().into(),
                    content_type: request.format.content_type(),
                    decoder_backend: "nvjpeg2000", decoder_route: "nvjpeg2000",
                    decode_ms: result.decode_ms,
                    encode_ms: crate::decoder::elapsed_ms(encode_started),
                    source_cache_status: "bypass",
                    source_cache_ms: 0.0,
                    compressed_resolution_view: result.timing.resolution_view,
                    source_read_bytes: 0,
                    source_range_requests: 0,
                    source_cache_hit_bytes: 0,
                },
            });
        }
        ensure!(
            offset == body.len(),
            "CUDA response contains unexpected trailing bytes"
        );
        Ok(output)
    }
}

impl DecoderDrivers {
    pub fn new() -> Result<Self> {
        let enabled = crate::configuration::snapshot()?.enabled_drivers;
        let mut engines: HashMap<DecoderDriver, std::sync::Arc<dyn TileEngine>> = HashMap::new();
        let mut info = Vec::new();
        engines.insert(
            DecoderDriver::OpenJpeg,
            std::sync::Arc::new(DirectJpeg2000TileEngine),
        );
        info.push(DecoderDriverInfo {
            id: DecoderDriver::OpenJpeg.as_str().to_owned(),
            name: DecoderDriver::OpenJpeg.display_name().to_owned(),
            available: true,
            progressive_quality: true,
            reason: None,
        });

        #[cfg(feature = "grok")]
        {
            let available = crate::decoder::grok::is_available();
            if available {
                engines.insert(
                    DecoderDriver::Jp2Grok,
                    std::sync::Arc::new(GrokJpeg2000TileEngine::default()),
                );
            }
            info.push(DecoderDriverInfo {
                id: DecoderDriver::Jp2Grok.as_str().to_owned(),
                name: DecoderDriver::Jp2Grok.display_name().to_owned(),
                available,
                progressive_quality: true,
                reason: (!available)
                    .then(|| "grk_decompress is not available on River PATH".to_owned()),
            });
        }
        #[cfg(not(feature = "grok"))]
        info.push(DecoderDriverInfo {
            id: DecoderDriver::Jp2Grok.as_str().to_owned(),
            name: DecoderDriver::Jp2Grok.display_name().to_owned(),
            available: false,
            progressive_quality: true,
            reason: Some("River was built without the JP2Grok driver".to_owned()),
        });

        #[cfg(feature = "grok")]
        {
            let available=crate::decoder::grok::is_available() && crate::decoder::grok::openht_available();
            if available {engines.insert(DecoderDriver::OpenHtj2k,std::sync::Arc::new(GrokJpeg2000TileEngine {openht:true}));}
            info.push(DecoderDriverInfo {id:DecoderDriver::OpenHtj2k.as_str().into(),name:DecoderDriver::OpenHtj2k.display_name().into(),available,progressive_quality:true,reason:(!available).then(||"The portable OpenHT worker and Grok fallback must both be installed".into())});
        }
        #[cfg(not(feature = "grok"))]
        info.push(DecoderDriverInfo {id:DecoderDriver::OpenHtj2k.as_str().into(),name:DecoderDriver::OpenHtj2k.display_name().into(),available:false,progressive_quality:true,reason:Some("River was built without the Grok/OpenHT hybrid driver".into())});

        #[cfg(feature = "nvjpeg2000")]
        let nvjpeg2000 = std::thread::spawn(NvJpeg2000TileEngine::from_env)
            .join()
            .ok()
            .and_then(Result::ok);
        #[cfg(feature = "nvjpeg2000")]
        if let Some(driver) = nvjpeg2000 {
            engines.insert(DecoderDriver::NvJpeg2000, std::sync::Arc::new(driver));
            info.push(DecoderDriverInfo {
                id: DecoderDriver::NvJpeg2000.as_str().to_owned(),
                name: DecoderDriver::NvJpeg2000.display_name().to_owned(),
                available: true,
                progressive_quality: false,
                reason: Some("Streams final-quality tiles; nvJPEG2000 does not expose quality-layer refinement".to_owned()),
            });
        } else {
            info.push(DecoderDriverInfo {
                id: DecoderDriver::NvJpeg2000.as_str().to_owned(),
                name: DecoderDriver::NvJpeg2000.display_name().to_owned(),
                available: false,
                progressive_quality: false,
                reason: Some(
                    "nvJPEG2000 sidecar is not configured or failed its health check".to_owned(),
                ),
            });
        }
        #[cfg(not(feature = "nvjpeg2000"))]
        info.push(DecoderDriverInfo {
            id: DecoderDriver::NvJpeg2000.as_str().to_owned(),
            name: DecoderDriver::NvJpeg2000.display_name().to_owned(),
            available: false,
            progressive_quality: false,
            reason: Some("River was built without the nvJPEG2000 driver".to_owned()),
        });

        match crate::source_cache::SourceCache::from_env() {
            Ok(Some(cache)) => {
                let cache = std::sync::Arc::new(cache);
                for engine in engines.values_mut() {
                    *engine = std::sync::Arc::new(CachedTileEngine {
                        engine: engine.clone(),
                        cache: cache.clone(),
                    });
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "local source cache disabled"),
        }
        for engine in engines.values_mut() {
            *engine = std::sync::Arc::new(RemoteTileEngine { engine: engine.clone() });
        }
        let raster_cache = crate::raster_cache::RasterCache::from_env();
        for engine in engines.values_mut() {
            *engine = std::sync::Arc::new(RasterCachedEngine { engine: engine.clone(), cache: raster_cache.clone() });
        }
        let drivers = Self { engines, info, enabled: Default::default() };
        drivers.validate_enabled(enabled.as_deref())?;
        drivers.apply_enabled(enabled);
        Ok(drivers)
    }

    pub fn validate_enabled(&self, enabled: Option<&[DecoderDriver]>) -> Result<()> {
        if let Some(enabled) = enabled {
            anyhow::ensure!(enabled.iter().any(|driver| self.engines.contains_key(driver)),
                "enabledDrivers must select at least one installed and available decoder");
        }
        Ok(())
    }

    pub fn apply_enabled(&self, enabled: Option<Vec<DecoderDriver>>) {
        *self.enabled.write().unwrap_or_else(|error| error.into_inner()) = enabled;
    }

    pub fn get(&self, driver: DecoderDriver) -> Option<std::sync::Arc<dyn TileEngine>> {
        let enabled = self.enabled.read().unwrap_or_else(|error| error.into_inner());
        if enabled.as_ref().is_some_and(|drivers| !drivers.contains(&driver)) {
            return None;
        }
        self.engines.get(&driver).cloned()
    }

    pub fn info(&self) -> Vec<DecoderDriverInfo> {
        let enabled = self.enabled.read().unwrap_or_else(|error| error.into_inner());
        let mut info = self.info.clone();
        if let Some(enabled) = &*enabled {
            for driver in &mut info {
                if !enabled.iter().any(|value| value.as_str() == driver.id) {
                    driver.available = false;
                    driver.reason = Some("Disabled by enabledDrivers configuration".into());
                }
            }
        }
        info
    }
}

/// Direct tiled JPEG 2000 decoding. It has no persistent result cache and no
/// derived-raster dependency: source layout determines whether it is fast.
#[derive(Default)]
pub struct DirectJpeg2000TileEngine;

impl TileEngine for DirectJpeg2000TileEngine {
    fn name(&self) -> &'static str {
        "direct-jpeg2000"
    }
    fn accepts_memory_source(&self) -> bool { true }

    #[instrument(
        name = "river.engine.direct.decode",
        skip(self, request),
        fields(
            engine = self.name(),
            tile_count = request.tiles.len(),
            source_tile = request.source_tile,
            reduction = request.reduction,
            layers = request.layers,
            max_native_tiles = request.max_native_tiles,
            decoder_threads = tracing::field::Empty,
            source_width = request.source.width,
            source_height = request.source.height,
        )
    )]
    fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
        let thread_budget=crate::decoder::thread_budget::acquire(crate::decoder::thread_budget::request_maximum(&request),crate::decoder::thread_budget::large_request(&request));
        let decoder_threads=thread_budget.threads();
        tracing::Span::current().record("decoder_threads",decoder_threads);
        let native_memory = std::env::var("RIVER_S3_NATIVE_TILES").is_ok_and(|value| value == "1" || value == "true");
        if request.source.memory.is_none() || native_memory { if let Some(tiles) = request.source.decode_native_tiled_tiles(
            &request.tiles,
            request.source_tile,
            request.reduction,
            request.layers,
            request.format,
            decoder_threads,
            request.max_native_tiles,
            &request.source_id,
        )? {
            return Ok(tiles);
        } }
        request.source.decode_tiles_with_threads(
            &request.tiles,
            request.source_tile,
            request.reduction,
            request.layers,
            request.format,
            decoder_threads,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    #[tokio::test]
    async fn abandoned_preparation_keeps_io_admission_until_worker_exits() {
        struct HeldPreparation(std::sync::mpsc::Sender<()>, std::sync::Mutex<std::sync::mpsc::Receiver<()>>);
        impl TileEngine for HeldPreparation {
            fn name(&self) -> &'static str { "held" }
            fn prepare(&self, request: TileDecodeRequest) -> Result<TileDecodeRequest> {
                self.0.send(()).unwrap();
                self.1.lock().unwrap().recv().unwrap();
                Ok(request)
            }
            fn decode(&self, _: TileDecodeRequest) -> Result<Vec<DecodedTile>> { unreachable!() }
        }
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = slots.clone().acquire_owned().await.unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let engine = HeldPreparation(entered_tx, std::sync::Mutex::new(release_rx));
        let request = TileDecodeRequest { progressive_final_reduction:None, output_geometry:None, edge_mode:crate::edges::Mode::Crop, capture_pixels:false, webp_method: crate::encoding_settings::snapshot(), source_id: "cancelled".into(), whole_view: false,
            source: Jpeg2000Source::from_path("cancelled.jp2", 512, 512), tiles: vec![(0, 0)], source_tile: 512,
            reduction: 0, layers: 1, format: RasterFormat::Png, decoder_threads: 1, max_native_tiles: 4, cached_tiles: vec![], work_ticket:None, cancellation: None };
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let flight = gate.clone().lock_owned().await;
        let worker = tokio::task::spawn_blocking(move || prepare_admitted(&engine, request, permit, Some(flight), false));
        entered_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        worker.abort(); drop(worker);
        assert!(slots.clone().try_acquire_owned().is_err());
        assert!(gate.try_lock().is_err(), "abandoned worker allowed duplicate preparation");
        release_tx.send(()).unwrap();
        let released = tokio::time::timeout(std::time::Duration::from_secs(2), slots.acquire()).await.unwrap().unwrap();
        drop(released);
        let unlocked = tokio::time::timeout(std::time::Duration::from_secs(2), gate.lock()).await.unwrap();
        drop(unlocked);
    }

    struct CountedEngine(Arc<AtomicUsize>);
    impl TileEngine for CountedEngine {
        fn name(&self) -> &'static str { "counted" }
        fn decode(&self, request: TileDecodeRequest) -> Result<Vec<DecodedTile>> {
            self.0.fetch_add(request.tiles.len(), Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(40));
            Ok(request.tiles.iter().map(|&(tile_x,tile_y)| DecodedTile { rgba:None, tile_x, tile_y, raster: crate::decoder::DecodedRaster {
                width: 1, height: 1, bytes: vec![request.layers as u8; 16].into(), content_type: "image/png", decoder_backend: "nvjpeg2000", decoder_route: "counted",
                decode_ms: 40.0, encode_ms: 0.0, source_cache_status: "bypass", source_cache_ms: 0.0,
                compressed_resolution_view: false, source_read_bytes: 0, source_range_requests: 0, source_cache_hit_bytes: 0 } }).collect())
        }
    }
    async fn render(engine: Arc<dyn TileEngine>, request: TileDecodeRequest) -> Vec<DecodedTile> {
        let gate = engine.flight(&request).unwrap().lock_owned().await;
        if let Some(tiles) = engine.cached(&request) { return tiles; }
        tokio::task::spawn_blocking(move || { let _gate = gate; engine.decode(request).unwrap() }).await.unwrap()
    }
    #[test]
    fn mixed_batches_prepare_and_decode_only_missing_tiles() {
        let count = Arc::new(AtomicUsize::new(0));
        let engine = RasterCachedEngine { engine: Arc::new(CountedEngine(count.clone())), cache: crate::raster_cache::RasterCache::from_env() };
        let mut request = TileDecodeRequest { progressive_final_reduction:None, output_geometry:None, edge_mode:crate::edges::Mode::Crop, capture_pixels:false, webp_method: crate::encoding_settings::snapshot(), source_id: "partial-batch-snapshot".into(), whole_view: true,
            source: Jpeg2000Source::from_path("partial.jp2",1536,512), tiles: vec![(0,0)], cached_tiles: vec![], work_ticket:None,
            source_tile: 512, reduction: 0, layers: 0, format: RasterFormat::Png, decoder_threads: 1, max_native_tiles: 4, cancellation: None };
        let initial = engine.decode(request.clone()).unwrap();
        request.tiles = vec![(2,0),(0,0),(1,0)];
        let prepared = engine.prepare(request).unwrap();
        assert_eq!(prepared.tiles,vec![(2,0),(1,0)]);
        assert_eq!(prepared.cached_tiles.len(),1);
        assert!(!prepared.whole_view,"partial miss cannot trigger whole-object preparation");
        let tiles = engine.decode(prepared).unwrap();
        assert_eq!(tiles.iter().map(|tile| tile.tile_x).collect::<Vec<_>>(),vec![0,1,2]);
        assert_eq!(count.load(Ordering::SeqCst),3,"cached tile entered the decoder again");
        assert_eq!(tiles[0].raster.decoder_route,"encoded-raster-cache");
        assert_eq!(tiles[0].raster.bytes.as_ptr(),initial[0].raster.bytes.as_ptr());
        assert_eq!(tiles[1].raster.decoder_route,"counted");
    }
    #[tokio::test]
    async fn concurrent_identical_renders_decode_once_and_quality_stays_independent() {
        let count = Arc::new(AtomicUsize::new(0));
        let engine: Arc<dyn TileEngine> = Arc::new(RasterCachedEngine {
            engine: Arc::new(CountedEngine(count.clone())), cache: crate::raster_cache::RasterCache::from_env() });
        let request = TileDecodeRequest { progressive_final_reduction:None, output_geometry:None, edge_mode:crate::edges::Mode::Crop, capture_pixels:false, webp_method: crate::encoding_settings::snapshot(), source_id: "singleflight-snapshot".into(), whole_view: false,
            source: Jpeg2000Source::from_path("singleflight.jp2", 512, 512), tiles: vec![(0, 0)], source_tile: 512,
            reduction: 0, layers: 1, format: RasterFormat::Png, decoder_threads: 1, max_native_tiles: 4, cached_tiles: vec![], work_ticket:None, cancellation: None };
        let (one, two) = tokio::join!(render(engine.clone(), request.clone()), render(engine.clone(), request.clone()));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(one[0].raster.bytes, two[0].raster.bytes);
        let mut next = request; next.layers = 2;
        assert_eq!(render(engine.clone(), next.clone()).await[0].raster.bytes, vec![2; 16]);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let (cancel, cancelled) = tokio::sync::watch::channel(false);
        let mut obsolete = next.clone();
        obsolete.layers = 3;
        obsolete.cancellation = Some(cancelled);
        cancel.send(true).unwrap();
        assert!(engine.decode(obsolete).unwrap_err().to_string().contains("superseded"));
        assert_eq!(count.load(Ordering::SeqCst), 2, "superseded request entered decoder");
    }

    #[test]
    fn direct_engine_has_a_stable_operational_identity() {
        assert_eq!(DirectJpeg2000TileEngine.name(), "direct-jpeg2000");
    }
    #[test]
    fn an_interior_tile_rendered_black_first_is_reused_by_crop_and_transparent() {
        struct Full(Arc<AtomicUsize>);
        impl TileEngine for Full {
            fn name(&self)->&'static str {"full-edge-fixture"}
            fn decode(&self,request:TileDecodeRequest)->Result<Vec<DecodedTile>> {
                let mut tiles=CountedEngine(self.0.clone()).decode(request)?;
                for tile in &mut tiles {tile.raster.width=512;tile.raster.height=512;}Ok(tiles)
            }
        }
        let count=Arc::new(AtomicUsize::new(0));
        let engine=RasterCachedEngine {engine:Arc::new(Full(count.clone())),cache:crate::raster_cache::RasterCache::from_env()};
        let mut request=TileDecodeRequest { progressive_final_reduction:None, output_geometry:None,edge_mode:crate::edges::Mode::Black,capture_pixels:false,webp_method:crate::encoding_settings::snapshot(),source_id:uuid::Uuid::new_v4().to_string(),whole_view:false,
            source:Jpeg2000Source::from_path("full-edge-fixture.jp2",512,512),tiles:vec![(0,0)],source_tile:512,reduction:0,layers:1,format:RasterFormat::Png,decoder_threads:1,max_native_tiles:4,cached_tiles:vec![],work_ticket:None,cancellation:None};
        let first=engine.decode(request.clone()).unwrap();
        for mode in [crate::edges::Mode::Crop,crate::edges::Mode::Transparent] {
            request.edge_mode=mode;let result=engine.decode(request.clone()).unwrap();
            assert_eq!(result[0].raster.bytes.as_ptr(),first[0].raster.bytes.as_ptr());
            assert_eq!(result[0].raster.decoder_route,"encoded-raster-cache");
        }
        assert_eq!(count.load(Ordering::SeqCst),1);
    }
}
