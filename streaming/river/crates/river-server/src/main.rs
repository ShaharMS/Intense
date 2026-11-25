use std::{
    collections::{BTreeMap, HashMap},
    fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
    },
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    serve::ListenerExt,
};
use object_store::{aws::{AmazonS3, AmazonS3Builder}, path::Path as ObjectPath};
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use river_core::{
    AssetId, AssetManifest, Capability, ColorPolicy, DecoderDriver,
    QualityModel, RasterFormat, SourceColorProfile, TILE_SIZE,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use tower_http::services::ServeDir;
use tracing::{Instrument, instrument};
use tracing_opentelemetry::{OpenTelemetryLayer, OpenTelemetrySpanExt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use futures_util::StreamExt;

#[path = "hosting/cors.rs"]
mod cors;
#[path = "runtime/resources.rs"]
mod resources;
#[path = "runtime/cache_learning.rs"]
mod cache_learning;
#[path = "caching/image_state.rs"]
mod image_state;
mod display;
#[path = "caching/cache_inventory.rs"]
mod cache_inventory;
mod decoder;
#[path = "hosting/console_log.rs"]
mod console_log;
mod edges;
mod engine;
#[path = "runtime/source_admission.rs"]
mod source_admission;
#[path = "hosting/api_docs.rs"]
mod api_docs;
#[path = "caching/source_cache.rs"]
mod source_cache;
#[path = "runtime/storage_mode.rs"]
mod storage_mode;
#[path = "runtime/adaptation.rs"]
mod adaptation;
#[path = "caching/shared_cache.rs"]
mod shared_cache;
mod remote_source;
mod tile_diagnostics;
#[path = "caching/raster_cache.rs"]
mod raster_cache;
mod source_connections;
#[path = "settings/encoding_settings.rs"]
mod encoding_settings;
#[path = "settings/configuration.rs"]
mod configuration;
#[path = "hosting/tls.rs"]
mod tls;
#[path = "hosting/http3.rs"]
mod http3;
mod read_policy;
#[path = "settings/settings_api.rs"]
mod settings_api;
mod compatibility;
mod maps;
mod errors;
#[cfg(any(test,all(feature="nvjpeg2000",target_os="linux")))]
#[path = "runtime/worker_sizing.rs"]
mod worker_sizing;
#[cfg(any(test,all(feature="nvjpeg2000",target_os="linux")))]
#[path = "runtime/lease_pool.rs"]
mod lease_pool;
#[cfg(all(feature = "nvjpeg2000", target_os = "linux"))]
mod cuda_native;
#[cfg(all(feature = "nvjpeg2000", target_os = "linux"))]
mod ht_cache;

use decoder::Jpeg2000Source;
use engine::{DecoderDrivers, TileDecodeRequest};

#[derive(Clone)]
struct AppState {
    configuration_lock: Arc<Mutex<()>>,
    map_catalog: Arc<maps::Catalog>,
    assets: Arc<RwLock<image_state::Assets>>,
    s3_sources: Arc<RwLock<HashMap<String, S3Source>>>,
    s3_load_lock: Arc<RwLock<()>>,
    s3_object_loads: Arc<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>,
    s3_range_cache: Arc<remote_source::RangeCache>,
    decode_slots: Arc<adaptation::Admission>,
    source_io_slots: Arc<adaptation::Admission>,
    /// Internal worker count for each native OpenJPEG decode. Combined with
    /// the shared semaphore this prevents multiplicative CPU oversubscription.
    openjpeg_threads_per_decode: usize,
    /// Native OpenJPEG tile traversal is useful only for a small number of
    /// codestream tiles. Larger windows use the bounded ROI decoder instead
    /// of turning a viewport into a serial tile-part scan.
    max_native_tiles_per_job: usize,
    /// Source-specific execution belongs behind this interface. The default
    /// direct engine persists no generated raster material or result cache.
    tile_engines: DecoderDrivers,

}

#[derive(Clone)]
struct S3Source {
    id: String,
    name: String,
    endpoint: Option<String>,
    bucket: String,
    region: String,
    prefix: String,
    store: Arc<AmazonS3>,
    connection: S3SourceInput,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase",deny_unknown_fields)]
struct S3SourceInput {
    id: String,
    name: String,
    endpoint: Option<String>,
    bucket: String,
    #[serde(default = "default_s3_region")]
    region: String,
    #[serde(default)]
    prefix: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    #[serde(default)]
    allow_http: bool,
    #[serde(default)]
    virtual_hosted_style: bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    max_retries: Option<u32>,
}

fn s3_retry_limit(input:&S3SourceInput)->anyhow::Result<usize> {
    let retries=match input.max_retries {Some(n)=>n,None=>match std::env::var("RIVER_S3_MAX_RETRIES") {Ok(s)=>s.parse::<u32>().context("RIVER_S3_MAX_RETRIES must be an integer0 through10")?,Err(_)=>2}};
    anyhow::ensure!(retries<=10,"maxRetries must be0 through10 (retries after the initial attempt)");Ok(retries as usize)
}
fn default_s3_region() -> String {
    "us-east-1".to_owned()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct S3AssetInput {
    key: String,
    width: Option<u32>,
    height: Option<u32>,
    bit_depth: Option<u8>,
}


#[derive(Clone)]
pub(crate) struct ImageState {
    local_header: Arc<tokio::sync::OnceCell<remote_source::Header>>,
    local_reader: Arc<tokio::sync::OnceCell<Arc<remote_source::RemoteSource>>>,
    manifest: AssetManifest,
    source: Option<Jpeg2000Source>,

}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let configuration = configuration::snapshot()?;
    adaptation::initialize(configuration.auto_adaptation_mode.as_deref())?;
    cache_learning::initialize(configuration.cache_learning)?;
    if std::env::args().any(|arg|arg=="--resource-env") {println!("{}",resources::launcher_env());return Ok(());}
    shared_cache::initialize(configuration.shared_cache)?;
    storage_mode::validate()?;
    display::require_channel_labels()?;
    display::four_band_setting()?;
    encoding_settings::restore()?;
    remote_source::validate_serial_transport()?;
    let (tracer_provider, logger_provider) = init_tracing()?;
    storage_mode::print_startup();
    resources::start();
    let decode_concurrency = decode_concurrency_from_env();
    let openjpeg_threads_per_decode = std::env::var("RIVER_OPENJPEG_THREADS_PER_DECODE")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|&value| value > 0)
        .unwrap_or_else(decoder::thread_budget::default_max_threads);
    let max_native_tiles_per_job = std::env::var("RIVER_MAX_NATIVE_TILES_PER_JOB")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|&value| value > 0)
        .unwrap_or(4);
    let assets = BTreeMap::new();
    let state = AppState {
        configuration_lock: Arc::new(Mutex::new(())),
        map_catalog: Arc::new(maps::Catalog::load()?),
        assets: image_state::Assets::shared(assets),
        s3_sources: Arc::new(RwLock::new(HashMap::new())),
        s3_load_lock: Arc::new(RwLock::new(())),
        s3_object_loads: Arc::new(Mutex::new(HashMap::new())),
        s3_range_cache: remote_source::RangeCache::from_env(),
        decode_slots: Arc::new(adaptation::Admission::new(decode_concurrency, "RIVER_DECODE_CONCURRENCY")),
        source_io_slots: Arc::new(adaptation::Admission::new(resources::plan().source_io_concurrency, "RIVER_SOURCE_IO_CONCURRENCY")),
        openjpeg_threads_per_decode,
        max_native_tiles_per_job,
        tile_engines: DecoderDrivers::new()?,
    };
    tracing::info!(
        decode_concurrency,
        openjpeg_threads_per_decode,
        max_native_tiles_per_job,
        decoder_drivers = ?state.tile_engines.info().iter().map(|driver| (&driver.id, driver.available)).collect::<Vec<_>>(),
        "configured JPEG 2000 decode concurrency"
    );

    source_connections::restore(&state).await?;
    let cors = cors::layer(&std::env::var("RIVER_CORS_ORIGINS").unwrap_or_else(|_| "*".into()))?;
    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(api_docs::spec))
        .route("/docs", get(api_docs::ui))
        .route("/docs/", get(api_docs::ui))
        .route("/swagger", get(|| async { axum::response::Redirect::temporary("/docs/") }))
        .nest_service("/docs/assets", ServeDir::new(std::env::var("RIVER_SWAGGER_ASSETS").unwrap_or_else(|_| "deploy/swagger".into())))
        .route("/api/s3/sources", get(list_s3_sources).post(add_s3_source))
        .route(
            "/api/s3/sources/{source_id}",
            axum::routing::delete(remove_s3_source),
        )
        .route("/api/assets/{asset_id}/manifest", get(manifest))
        .route("/api/drivers", get(list_decoder_drivers))
        .route("/api/cache/ht", get(ht_cache_status))
        .route("/api/formats", get(list_raster_formats))
        .route("/api/settings/configuration", get(settings_api::export).put(settings_api::import))
        .route("/api/settings/cache-learning", get(settings_api::get_cache_learning).put(settings_api::put_cache_learning).delete(settings_api::reset_cache_learning))
        .route("/api/diagnostics/resources", get(|| async {Json(resources::diagnostics())}))
        .route("/api/settings/adaptation", get(settings_api::get_adaptation).put(settings_api::put_adaptation).delete(settings_api::reset_adaptation))
        .route("/api/settings/shared-cache", get(settings_api::get_shared_cache).put(settings_api::put_shared_cache).delete(settings_api::reset_shared_cache))
        .route("/api/diagnostics/source-reads", get(settings_api::get_source_reads).put(settings_api::put_source_reads).delete(settings_api::clear_source_reads))
        .route("/api/settings/read-policy", get(settings_api::get_policy).put(settings_api::put_policy))
        .route("/api/settings/drivers", get(settings_api::get_drivers).put(settings_api::put_drivers))
        .route("/api/settings/encoding", get(get_encoding_settings).put(put_encoding_settings))
        .route("/wmts", get(wmts))
        .route("/map", get(maps::viewer))
        .route("/map/viewer.js", get(maps::script))
        .route("/api/maps", get(maps::catalog))
        .route("/api/maps/{layer}/{z}/{x}/{y}", get(maps::tile))
        .route("/streamer/v1/{z}/{x}/{y}/{file}", get(compatibility::tile))
        .route("/api/tiles/{image}", get(compatibility::tile_info))
        .route("/", get(|| async { axum::response::Redirect::temporary("/docs/") }))
        .layer(axum::middleware::from_fn(errors::normalize))
        .layer(axum::middleware::from_fn(cors::expose_response_headers))
        .layer(cors)
        .with_state(state);

    let address = std::env::var("RIVER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse::<SocketAddr>()
        .context("RIVER_BIND must be a socket address")?;
    tracing::info!(%address, "River listening");
    tokio::task::spawn_blocking(|| {
        if let Err(error) = image_state::warm_locator_inventory() {
            tracing::debug!(%error, "Asset locator inventory warmup deferred");
        }
    });
    let listener = tokio::net::TcpListener::bind(address).await?.tap_io(|stream| {
        // Small preview/cache-hit frames must not wait for delayed TCP ACKs.
        if let Err(error) = stream.set_nodelay(true) {
            tracing::warn!(%error, "unable to enable TCP_NODELAY");
        }
    });
    tls::serve(listener, app, tls::settings(configuration::snapshot()?.tls)?).await?;
    tracer_provider.shutdown()?;
    if let Some(logger) = logger_provider { logger.shutdown()?; }
    Ok(())
}

#[derive(serde::Deserialize)]
struct WmtsQuery {
    #[serde(rename="EDGE",alias="edge",default)]
    edge: String,
    #[serde(rename = "SERVICE", default)]
    service: String,
    #[serde(rename = "REQUEST", default)]
    request: String,
    #[serde(rename = "VERSION", default)]
    version: String,
    #[serde(rename = "LAYER", default)]
    layer: String,
    #[serde(rename = "STYLE", default)]
    style: String,
    #[serde(rename = "FORMAT", default)]
    format: String,
    #[serde(rename = "DRIVER", default)]
    driver: String,
    #[serde(rename = "LAYERS", default)]
    layers: Option<u32>,
    #[serde(rename = "TILEMATRIXSET", default)]
    matrix_set: String,
    #[serde(rename = "TILEMATRIX", default)]
    matrix: String,
    #[serde(rename = "TILEROW", default)]
    row: String,
    #[serde(rename = "TILECOL", default)]
    col: String,
}

fn schedule_wmts_refinement(
    state: &AppState,
    engine: Arc<dyn engine::TileEngine>,
    mut request: engine::TileDecodeRequest,
    mut predecessor: tokio::sync::oneshot::Receiver<()>,
    overlap_decode: bool,
) -> bool {
    static REFINEMENT: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    request.cancellation = None;
    let Some(gate) = engine.flight(&request) else { return false };
    let Ok(flight) = gate.try_lock_owned() else { return false };
    let Ok(background) = REFINEMENT.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone().try_acquire_owned() else { return false };
    let Some(io) = state.source_io_slots.try_acquire() else { return false };
    let cpu = state.decode_slots.clone();
    tokio::spawn(async move {
        let preparer = engine.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            if preparer.cached(&request).is_some() { return None }
            let source = request.source.remote.as_ref()?;
            let admission = futures_util::FutureExt::now_or_never(
                source.preparation_admission(&request.tiles, request.source_tile))?;
            let index_only = source.serial_index && !source.sequential_prefix;
            let _io = io;
            let request = {
                let _scope = source_admission::Scope::enter(admission, index_only);
                preparer.prepare(request).inspect_err(|error| tracing::debug!(%error, "WMTS background preparation failed")).ok()?
            };
            // Never retain the last source slot while waiting for a foreground
            // request that may itself need I/O. Demand can retry on pressure.
            let bytes = request.source.memory.as_ref().map_or(0, |data| data.len());
            let charge = adaptation::ByteSlots::prepared(bytes)?;
            Some((request, charge))
        }).await;
        let Ok(Some((request, charge))) = prepared else { return };
        // The full tile is independent once prepared. Overlap its decode only
        // when the preview already owns a slot and another remains for demand.
        use tokio::sync::oneshot::error::TryRecvError;
        let spare = match predecessor.try_recv() {
            Err(TryRecvError::Closed) => return,
            Err(TryRecvError::Empty) => {
                let permit = if overlap_decode { cpu.try_acquire_spare() } else { None };
                if permit.is_none() && predecessor.await.is_err() { return; }
                permit
            }
            Ok(()) => None,
        };
        let cpu = match spare {
            Some(permit) => permit,
            None => match cpu.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            },
        };
        drop(charge);
        let _ = tokio::task::spawn_blocking(move || {
            let _guards = (background, flight, cpu);
            if let Err(error) = engine.decode(request) {
                tracing::debug!(%error, "WMTS background decode failed");
            }
        }).await;
    });
    true
}

fn release_prepared_io(request:&engine::TileDecodeRequest, io:&mut Option<adaptation::Permit>) -> Option<tokio::sync::SemaphorePermit<'static>> {
    let bytes=request.source.memory.as_ref().map_or(0,|data|data.len());
    let charge=adaptation::ByteSlots::prepared(bytes)?;
    io.take();
    Some(charge)
}


#[instrument(name = "river.wmts.request", skip(state, query), fields(river.protocol = "wmts", river.output_format = %query.format, river.decoder_driver = %query.driver))]
async fn wmts(State(state): State<AppState>, Query(query): Query<WmtsQuery>) -> Response {
    render_wmts(state,query,None,TileTiming::new()).await
}
#[derive(Debug)]
struct TileTiming {
    since: Instant,
    stages: Vec<(&'static str, f64)>,
    debug: bool,
    details: Vec<tile_diagnostics::Details>,
}
impl TileTiming {
    fn new() -> Self { Self { since: Instant::now(), stages: Vec::with_capacity(10), debug: tracing::enabled!(tracing::Level::DEBUG), details: Vec::new() } }
    fn finish(&mut self, stage: &'static str) {
        let now = Instant::now();
        self.stages.push((stage, (now-self.since).as_secs_f64()*1000.0));
        self.since = now;
    }
    fn headers(&self, headers: &mut axum::http::HeaderMap) {
        use std::fmt::Write;
        let mut value = String::with_capacity(self.stages.len()*24);
        for (i, (name, ms)) in self.stages.iter().enumerate() {
            if i > 0 { value.push_str(", "); }
            write!(value, "{name};dur={ms:.3}").unwrap();
        }
        let mut sources = String::new();
        let mut indexes = Vec::new();
        for details in &self.details {
            if let Some(prefix) = &details.prefix {
                headers.insert("x-river-prefix-plan", prefix.parse().unwrap());
            }
            details.append_headers(&mut value, &mut sources, &mut indexes);
        }
        if !sources.is_empty() { headers.insert("x-river-source-stages", sources.parse().unwrap()); }
        if !indexes.is_empty() { headers.insert("x-river-index-source", indexes.join(", ").parse().unwrap()); }
        headers.insert("server-timing", value.parse().unwrap());
    }
}

async fn render_wmts(state:AppState,query:WmtsQuery,geometry:Option<compatibility::Geometry>,mut timing:TileTiming)->Response {
    let edge_mode=match edges::Mode::parse(&query.edge) {Ok(mode)=>mode,Err(error)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_edge_mode",error.to_string(),serde_json::json!({"allowed":["crop","black","transparent"]}))};
    if !query.service.eq_ignore_ascii_case("WMTS")
        || !query.version.is_empty() && query.version != "1.0.0"
    {
        return errors::reply(StatusCode::BAD_REQUEST,"unsupported_wmts_version","SERVICE must be WMTS and VERSION must be 1.0.0 (VERSION may be omitted for capabilities).",serde_json::json!({"service":query.service,"version":query.version}));
    }
    if query.request.eq_ignore_ascii_case("GetCapabilities") {
        let (assets, next) = if !query.layer.is_empty() {
            if let Some((source_id,key)) = remote_asset_key(&query.layer) {
                let response = prepare_s3_image(State(state.clone()),Path(source_id),Json(S3AssetInput {
                    key,width: None,height: None,bit_depth: None,
                })).await;
                if !response.status().is_success() { return response; }
            }
            match resolved_asset(&state,&AssetId(query.layer.clone())).await {
                Ok(Some(asset)) => (vec![asset],None),
                Ok(None) => return errors::reply(StatusCode::NOT_FOUND,"asset_not_found","The requested image is not available. Use the original object URL from a connected source.",serde_json::json!({"assetId":query.layer})),
                Err(error) => return (StatusCode::UNPROCESSABLE_ENTITY,error.to_string()).into_response(),
            }
        } else {
            (Vec::new(), None::<String>)
        };
        // Resolve only the explicitly requested image; never enumerate sources.
        let assets = futures_util::stream::iter(assets.into_iter().map(|asset| {
            let state = state.clone();
            async move {
                match resolved_asset(&state,&asset.manifest.asset_id).await {
                    Ok(asset) => asset,
                    Err(error) => { tracing::warn!(%error,asset_id=%asset.manifest.asset_id.0,"WMTS skipped unavailable image metadata"); None }
                }
            }
        })).buffered(4).filter_map(|asset| async move { asset }).collect::<Vec<_>>().await;
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Capabilities xmlns=\"http://www.opengis.net/wmts/1.0\" xmlns:ows=\"http://www.opengis.net/ows/1.1\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" version=\"1.0.0\"><ows:ServiceIdentification><ows:Title>River raster WMTS</ows:Title></ows:ServiceIdentification><Contents>",
        );
        for asset in &assets {
            let id = &asset.manifest.asset_id.0;
            let matrix_set = format!("RiverPixel-{id}");
            xml.push_str(&format!(
                "<Layer><ows:Title>{id}</ows:Title><ows:Identifier>{id}</ows:Identifier>"
            ));
            for (format, available) in raster_format_options() {
                if available {
                    xml.push_str(&format!("<Format>{}</Format>", format.content_type()));
                }
            }
            xml.push_str(&format!("<Style isDefault=\"true\"><ows:Identifier>default</ows:Identifier></Style><TileMatrixSetLink><TileMatrixSet>{matrix_set}</TileMatrixSet></TileMatrixSetLink>"));
            for driver in state
                .tile_engines
                .info()
                .iter()
                .filter(|driver| driver.available)
            {
                let is_ht = asset.source.as_ref().and_then(|source| source.remote.as_ref()).map(|remote| remote.header.is_ht)
                    .or_else(|| asset.local_header.get().map(|header| header.is_ht)).unwrap_or(false);
                if is_ht && driver.id == "openjpeg" { continue; }
                if driver.id == "nvjpeg2000" && asset.source.as_ref().is_some_and(|source| source.reference_origin != (0,0)) { continue; }
                for (format, available) in raster_format_options() {
                    if !available {
                        continue;
                    }
                    let mime = format.content_type();
                    let mime_query = mime.replace('/', "%2F");
                    xml.push_str(&format!("<ResourceURL format=\"{mime}\" resourceType=\"tile\" template=\"/wmts?SERVICE=WMTS&amp;REQUEST=GetTile&amp;VERSION=1.0.0&amp;LAYER={id}&amp;STYLE=default&amp;FORMAT={mime_query}&amp;DRIVER={}&amp;TILEMATRIXSET={matrix_set}&amp;TILEMATRIX={{TileMatrix}}&amp;TILEROW={{TileRow}}&amp;TILECOL={{TileCol}}\"/>", driver.id));
                }
            }
            xml.push_str("</Layer>");
            let max_level = asset.manifest.resolution_levels.saturating_sub(1).min(15);
            xml.push_str(&format!("<TileMatrixSet><ows:Identifier>{matrix_set}</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:OGC:1.3:CRS84</ows:SupportedCRS><ows:Abstract>River-local pixel matrix for an image without georeferencing; not suitable for geographic overlay.</ows:Abstract>"));
            for level in 0..=max_level {
                let zoom = max_level - level;
                let edge = TILE_SIZE << zoom;
                let matrix_width = asset.manifest.width.div_ceil(edge);
                let matrix_height = asset.manifest.height.div_ceil(edge);
                let scale = 559_082_264.028 / f64::from(1u32 << level);
                xml.push_str(&format!("<TileMatrix><ows:Identifier>{level}</ows:Identifier><ScaleDenominator>{scale}</ScaleDenominator><TopLeftCorner>-180 90</TopLeftCorner><TileWidth>{TILE_SIZE}</TileWidth><TileHeight>{TILE_SIZE}</TileHeight><MatrixWidth>{matrix_width}</MatrixWidth><MatrixHeight>{matrix_height}</MatrixHeight></TileMatrix>"));
            }
            xml.push_str("</TileMatrixSet>");
        }
        xml.push_str("</Contents></Capabilities>");
        let mut response = ([(axum::http::header::CONTENT_TYPE, "application/xml")], xml).into_response();
        if let Some(next) = next { response.headers_mut().insert("x-river-next-cursor",HeaderValue::from_str(&next).unwrap()); }
        return response;
    }
    if !query.request.eq_ignore_ascii_case("GetTile")
        || query.layer.is_empty()
        || query.style != "default"
    {
        return errors::reply(StatusCode::BAD_REQUEST,"invalid_wmts_request","Use REQUEST=GetCapabilities or REQUEST=GetTile. GetTile requires a non-empty LAYER and STYLE=default.",serde_json::json!({"request":query.request,"layer":query.layer,"style":query.style}));
    }
    let Some(format) = raster_format_options()
        .into_iter()
        .find_map(|(candidate, available)| {
            (available && candidate.content_type() == query.format).then_some(candidate)
        })
    else {
        return errors::reply(StatusCode::BAD_REQUEST,"unsupported_image_format","The requested FORMAT is unsupported or unavailable. GET /api/formats lists available MIME types.",serde_json::json!({"format":query.format}));
    };
    let auto_driver = matches!(query.driver.trim(), "" | "auto");
    let Some(mut driver) = parse_decoder_driver(&query.driver) else {
        return errors::reply(StatusCode::BAD_REQUEST,"unknown_driver","Unknown decoder driver. Use openjpeg, jp2grok, openhtj2k or nvjpeg2000; GET /api/drivers lists runtime availability.",serde_json::json!({"driver":query.driver}));
    };
    if auto_driver && state.tile_engines.get(driver).is_none() {
        if let Some(available) = state.tile_engines.info().into_iter()
            .filter(|info| info.available).find_map(|info| parse_decoder_driver(&info.id))
        {
            driver = available;
        }
    }
    let Some(engine) = state.tile_engines.get(driver) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{} decoder is not available in this River runtime",
                driver.display_name()
            ),
        )
            .into_response();
    };
    let asset_id = AssetId(query.layer.clone());
    if let Err(error) = asset_id.validate() {
        return errors::reply(StatusCode::BAD_REQUEST,"invalid_asset_id",error.to_string(),serde_json::json!({"assetId":asset_id.0}));
    }
    let level = match query.matrix.parse::<u8>() {
        Ok(value) if value < 16 || geometry.is_some() => value,
        _ => return errors::reply(StatusCode::BAD_REQUEST,"invalid_tile_matrix","TILEMATRIX must be an integer from 0 through 15.",serde_json::json!({"value":query.matrix})),
    };
    let row = match query.row.parse::<u32>() {
        Ok(value) => value,
        _ => return errors::reply(StatusCode::BAD_REQUEST,"invalid_tile_row","TILEROW must be a non-negative 32-bit integer.",serde_json::json!({"value":query.row})),
    };
    let col = match query.col.parse::<u32>() {
        Ok(value) => value,
        _ => return errors::reply(StatusCode::BAD_REQUEST,"invalid_tile_column","TILECOL must be a non-negative 32-bit integer.",serde_json::json!({"value":query.col})),
    };
    let asset = match resolved_asset(&state,&asset_id).await {
        Ok(Some(asset)) => asset,
        Ok(None) => return errors::reply(StatusCode::NOT_FOUND,"asset_not_found","The requested image is not available. Use the original object URL from a connected source.",serde_json::json!({"assetId":query.layer})),
        Err(error) => return (StatusCode::UNPROCESSABLE_ENTITY,Json(serde_json::json!({"code":"local_header_failed","message":format!("{error:#}")}))).into_response(),
    };
    let max_level = asset.manifest.resolution_levels.saturating_sub(1).min(15);
    if (geometry.is_none() && level > max_level)
        || query.version != "1.0.0"
        || query.matrix_set != format!("RiverPixel-{}", asset_id.0)
    {
        return errors::reply(StatusCode::BAD_REQUEST,"unsupported_tile_matrix",format!("Requested matrix {} / set {} is unavailable. Valid matrices are 0 through {max_level}; expected WMTS version 1.0.0.",query.matrix,query.matrix_set),serde_json::json!({"assetId":asset_id.0,"matrix":level,"minMatrix":0,"maxMatrix":max_level,"expectedMatrixSet":format!("RiverPixel-{}",asset_id.0)}));
    }
    let (zoom,tile_edge)=if let Some(g)=geometry {let scale=g.span/g.bitmap;let reduction=if scale==0 {0}else{scale.ilog2() as u8};(reduction.min(max_level).min(g.span.trailing_zeros() as u8),g.span)} else {(max_level-level,TILE_SIZE.checked_shl(u32::from(max_level-level)).unwrap_or(u32::MAX))};
    let columns = asset.manifest.width.div_ceil(tile_edge);
    let rows = asset.manifest.height.div_ceil(tile_edge);
    if col >= columns || row >= rows {
        return errors::reply(StatusCode::NOT_FOUND,"tile_out_of_bounds",format!("Tile {level}/{col}/{row} is outside this image. Matrix {level} has {columns} column(s) and {rows} row(s): x=0..{}, y=0..{}.",columns.saturating_sub(1),rows.saturating_sub(1)),serde_json::json!({"assetId":asset_id.0,"matrix":level,"column":col,"row":row,"columns":columns,"rows":rows,"maxColumn":columns.saturating_sub(1),"maxRow":rows.saturating_sub(1),"imageWidth":asset.manifest.width,"imageHeight":asset.manifest.height}));
    }
    let Some(source) = asset.source else {
        return errors::reply(StatusCode::NOT_FOUND,"image_source_unavailable","The image has no readable source. Check the corpus mount or reconnect its S3 source.",serde_json::json!({"assetId":asset_id.0}));
    };
    let prefer_grok = auto_driver
        && geometry.is_some() && columns == 1 && rows == 1 && zoom > 0
        && std::env::var("RIVER_GROK_RPCL_PREVIEW").map_or(true, |value| value == "1")
        && source.remote.as_ref().is_some_and(|remote| {
            let h = &remote.header;
            !h.is_ht && h.components == 3 && h.bit_depth == 8
                && u64::from(h.width) * u64::from(h.height) <= 4096 * 4096
                && h.image_origin == (0, 0) && h.progression == 2 // RPCL
                && u64::from(h.layout.columns) * u64::from(h.layout.rows) <= 16
        });
    #[cfg(feature="grok")]
    let prefer_deep_rgb = auto_driver && zoom == 0 && tile_edge == 512
        && source.remote.as_ref().is_some_and(|r| r.deep_rgb_native_tile)
        && crate::decoder::grok::deep_rgb_pool_available();
    #[cfg(not(feature="grok"))]
    let prefer_deep_rgb = false;
    let prefer_indexed_zoom_grok = auto_driver
        && geometry.is_some() && zoom == 0 && tile_edge == 512
        && query.layers.unwrap_or(0) == 0
        && std::env::var("RIVER_TRIAL_GROK_INDEXED_ZOOM").as_deref() == Ok("1")
        && source.remote.as_ref().is_some_and(|remote| {
            let h = &remote.header;
            !remote.serial_index && !h.is_ht && h.components == 3
                && matches!(h.bit_depth, 8 | 16) && (4..=6).contains(&h.levels.saturating_sub(1))
                && h.image_origin == (0, 0) && h.layout.origin_x == 0 && h.layout.origin_y == 0
                && h.layout.width % 512 == 0 && h.layout.height % 512 == 0
        });
    let prefer_single_native_zoom_grok = auto_driver
        && geometry.is_some() && zoom == 0 && tile_edge == 512
        && query.layers.unwrap_or(0) == 0
        && matches!(std::env::var("RIVER_GROK_NATIVE").as_deref(), Ok("1" | "true"))
        && std::env::var("RIVER_GROK_SINGLE_NATIVE_DEEP").map_or(true, |value| value == "1")
        && source.remote.as_ref().is_some_and(|remote| remote.native_grok_single_deep);
    let (driver, engine) = if auto_driver && source.remote.as_ref().is_some_and(|reader| reader.header.is_ht) {
        let selected = [DecoderDriver::Jp2Grok, DecoderDriver::OpenHtj2k, DecoderDriver::NvJpeg2000]
            .into_iter()
            .filter(|driver| *driver != DecoderDriver::NvJpeg2000
                || (query.layers.unwrap_or(0) == 0 && source.reference_origin == (0, 0)))
            .find_map(|driver| state.tile_engines.get(driver).map(|engine| (driver, engine)));
        let Some(selected) = selected else {
            return errors::reply(StatusCode::UNPROCESSABLE_ENTITY, "unsupported_ht_decoder",
                "This HTJ2K image requires an enabled JP2Grok, OpenHTJ2K or compatible nvJPEG2000 decoder.",
                serde_json::json!({}));
        };
        selected
    } else if prefer_deep_rgb || prefer_grok || prefer_indexed_zoom_grok || prefer_single_native_zoom_grok {
        state.tile_engines.get(DecoderDriver::Jp2Grok)
            .map_or((driver, engine), |grok| (DecoderDriver::Jp2Grok, grok))
    } else {
        (driver, engine)
    };
    let layers = query.layers.unwrap_or(0);
    if driver == DecoderDriver::NvJpeg2000 && source.reference_origin != (0,0) {
        return errors::reply(StatusCode::BAD_REQUEST,"driver_origin_unsupported","Non-zero image origins require OpenJPEG or JP2Grok",serde_json::json!({}));
    }
    let available_layers = source.remote.as_ref().map(|remote| u32::from(remote.header.layers))
        .unwrap_or(asset.manifest.quality.cumulative_layer_bytes.len() as u32);
    if layers > available_layers || (layers > 0 && driver == DecoderDriver::NvJpeg2000) {
        return errors::reply(StatusCode::BAD_REQUEST,"unsupported_quality_layers",format!("Requested {layers} quality layers; this source has {available_layers}. Use 0 for all layers. nvjpeg2000 only supports 0."),serde_json::json!({"requestedLayers":layers,"availableLayers":available_layers,"driver":query.driver}));
    }
    // Both 0 and the source's complete layer count mean identical full pixels.
    let layers = if layers == available_layers { 0 } else { layers };
    timing.finish("resolve");
    let mut request = TileDecodeRequest {
        output_geometry:geometry.map(|g| edges::OutputGeometry {edge:g.bitmap,
            width:(u64::from((asset.manifest.width-col*tile_edge).min(tile_edge))*u64::from(g.bitmap)).div_ceil(u64::from(tile_edge)) as u32,
            height:(u64::from((asset.manifest.height-row*tile_edge).min(tile_edge))*u64::from(g.bitmap)).div_ceil(u64::from(tile_edge)) as u32}),
        edge_mode,
         webp_method: encoding_settings::snapshot(),
        source_id: asset_id.0.clone(), whole_view: columns == 1 && rows == 1, source, tiles: vec![(col, row)], source_tile: tile_edge,
        reduction: zoom, layers, format, decoder_threads: state.openjpeg_threads_per_decode,
        max_native_tiles: state.max_native_tiles_per_job, cached_tiles: vec![], work_ticket:None, cancellation: None,
    };
    let requested = (col, row);
    let select_tile = move |tiles: Vec<decoder::DecodedTile>| {
        tiles.into_iter().find(|tile| (tile.tile_x, tile.tile_y) == requested)
    };

    let shared_roi = driver == DecoderDriver::OpenJpeg || (driver == DecoderDriver::Jp2Grok
        && std::env::var("RIVER_GROK_NATIVE").is_ok_and(|value| matches!(value.as_str(), "true" | "1")));
    let index_warmup=request.source.remote.as_ref().filter(|source|
        zoom>0 && shared_roi && request.whole_view && source.header.layers==1 && source.header.layout.columns==1 && source.header.layout.rows==1
    ).cloned();
    let refinement=(layers>0 && request.source.remote.is_some()
        && resources::effective("raster_results",resources::budget("raster_results"))>0).then(||{
        let mut full=request.clone();
        full.layers=0;
        full
    });
    let direct_hit = engine.cached(&request);
    timing.finish("plan_and_lookup");
    let gate = direct_hit.is_none().then(|| engine.flight(&request)).flatten();
    // Only HTTP waiters hold this lease. The producer can outlive its first
    // client while another waiter still needs the same pixels.
    let interest = gate.as_ref().map(|gate| gate.interest());
    let flight = match gate {
        Some(gate) => {
            let guard = gate.lock_owned().await;
            timing.finish("shared_wait");
            Some(guard)
        }
        None => None,
    };
    let hit = direct_hit.or_else(|| engine.cached(&request));
    let shared_miss = flight.is_some() && hit.is_none();
    let local = if hit.is_none() && interest.is_none() {
        Some(remote_source::CancelOnDrop::new().0)
    } else { None };
    let cancellation = interest.as_deref().or(local.as_ref()).map(|lease| lease.subscribe());
    request.cancellation = cancellation.clone();
    let engine_name=engine.name();
    let failure_source=request.source.remote.clone();
    let failure_debug=timing.debug;
    let render = async move {
        let address = |asset| console_log::TileAddress { asset, matrix: level, col, row, layers };
        let failure = |mut response: Response, stage: &'static str, input: Option<&'static str>| {
            let mut details = serde_json::json!({
                "release":option_env!("RIVER_BUILD_VERSION").unwrap_or("development"),
                "replay":{"SERVICE":"WMTS","REQUEST":"GetTile","VERSION":"1.0.0","STYLE":query.style,
                    "LAYER":asset_id.0,"TILEMATRIXSET":query.matrix_set,"TILEMATRIX":level,"TILEROW":row,"TILECOL":col,
                    "LAYERS":query.layers,"FORMAT":query.format,"DRIVER":query.driver,"EDGE":query.edge},
                "geometryOverride":geometry.map(|g|serde_json::json!({"span":g.span,"bitmap":g.bitmap})),
                "availableLayers":available_layers,"sourceTileSpan":tile_edge,"matrixColumns":columns,"matrixRows":rows,
                "stage":stage,"driver":driver.as_str(),"inputRoute":input,"assetId":asset_id.0,
                "matrix":level,"column":col,"row":row,"layers":layers,"reduction":zoom,
                "sourceWidth":asset.manifest.width,"sourceHeight":asset.manifest.height,
                "components":asset.manifest.components,"bitDepth":asset.manifest.bit_depth,
                "resolutionLevels":asset.manifest.resolution_levels
            });
            if failure_debug {
                let plan=resources::plan();
                details["manifest"]=serde_json::json!(&asset.manifest);
                if let Some(remote)=&failure_source {
                    details["structure"]=remote.structure_summary();
                    details["sourceConnection"]=serde_json::json!(&remote.source_id);
                } else if let Some(h)=asset.local_header.get() {
                    details["structure"]=h.structure_summary();
                }
                details["worker"]=serde_json::json!({"engine":engine_name,"openJpegThreadBudget":state.openjpeg_threads_per_decode,
                    "maxNativeTilesPerJob":state.max_native_tiles_per_job,"decodeConcurrency":plan.decode_concurrency,
                    "sourceIoConcurrency":plan.source_io_concurrency,"memoryAllowanceBytes":plan.memory_allowance_bytes});
                details["storage"]=storage_mode::snapshot(plan.memory_allowance_bytes);
                details["adaptation"]=adaptation::snapshot();
                details["readPolicy"]=read_policy::settings_view(&read_policy::settings());
            }
            response.extensions_mut().insert(errors::TileFailure(details));
            response
        };
        let decode = if let Some(tiles) = hit {
            timing.finish("result_lookup");
            Ok(Ok((0.0, timing, select_tile(tiles))))
        } else {
            timing.finish("result_lookup");
            let (request, mut io_permit, flight, mut timing) = if request.source.remote.is_some() {
                let source_gate=match request.source.remote.as_ref() {
                    Some(remote)=>remote.preparation_admission(&request.tiles,request.source_tile).await,None=>None,
                };
                if timing.debug { timing.finish("source_gate_wait"); }
                let Ok(io_permit) = state.source_io_slots.clone().acquire_owned().await else { return errors::reply(StatusCode::SERVICE_UNAVAILABLE,"scheduler_unavailable","The decode or source-I/O scheduler has shut down. Retry after the River instance is ready.",serde_json::json!({})); };
                timing.finish("source_admission");
                let preparer = engine.clone();
                let span = tracing::Span::current();
                match tokio::task::spawn_blocking(move || {
                    let _entered = span.enter();
                    let index_only=request.source.remote.as_ref().is_some_and(|remote|remote.serial_index && !remote.sequential_prefix);
                                let _source_admission=source_admission::Scope::enter(source_gate,index_only);
                    timing.finish("prepare_queue");
                    let (prepared, details) = tile_diagnostics::Details::capture(timing.debug, || engine::prepare_admitted(preparer.as_ref(), request, io_permit, flight, false));
                    if timing.debug { timing.details.push(details); }
                    timing.finish("prepare");
                    (prepared, timing)
                }).await {
                    Ok((Ok((request, io_permit, flight)), mut timing)) => { timing.finish("prepare_return"); (request, Some(io_permit), flight, timing) },
                    Ok((Err(error), timing)) => {
                        let mut response=errors::resource_reply(&error).unwrap_or_else(|| errors::reply(
                            StatusCode::UNPROCESSABLE_ENTITY,"source_prepare_failed",format!("{error:#}"),serde_json::json!({})));
                        if timing.debug { timing.headers(response.headers_mut()); }
                        return failure(response,"prepare",None);
                    }
                    Err(error) => return failure(errors::reply(StatusCode::INTERNAL_SERVER_ERROR,
                        "source_prepare_worker_failed",error.to_string(),serde_json::json!({})),"prepare_worker",None),
                }
            } else { (request, None, flight, timing) };
            if request.tiles.is_empty() {
                Ok(Ok((0.0, timing, select_tile(request.cached_tiles))))
            } else {
            let _prepared=release_prepared_io(&request,&mut io_permit);
            let cpu_queued_at = Instant::now();
            let Ok(permit) = state.decode_slots.clone().acquire_owned().await else {
                return errors::reply(StatusCode::SERVICE_UNAVAILABLE,"scheduler_unavailable","The decode or source-I/O scheduler has shut down. Retry after the River instance is ready.",serde_json::json!({}));
            };
            timing.finish("cpu_admission");
            drop(io_permit);
            drop(_prepared);
            let preview_done=refinement.map(|full|{
                let (tx,rx)=tokio::sync::oneshot::channel();
                schedule_wmts_refinement(&state, engine.clone(), full, rx, driver == DecoderDriver::Jp2Grok);
                tx
            });
            if let Some((source,io))=index_warmup.and_then(|s|state.source_io_slots.try_acquire().map(|io|(s,io))) {
                tokio::task::spawn_blocking(move ||{
                    let _permit=io;
                    if let Err(error)=source.warm_codeblock_index() {tracing::debug!(%error,"Preview packet index warmup ended");}
                });
            }
            let span = tracing::Span::current();
            tokio::task::spawn_blocking(move || {
                let _flight = flight;
                let _entered = span.enter();
                let queue_ms = elapsed_ms(cpu_queued_at);
                timing.finish("render_dispatch");
                let input = request.source.prepared.as_ref().map_or_else(
                    || if request.source.memory.is_some() { "memory" } else { "file" }, |p| p.route);
                let prepared=if timing.debug {request.source.prepared.clone()} else {None};
                let memory_bytes=request.source.memory.as_ref().map(|p|p.len());
                let (result, details) = tile_diagnostics::Details::capture(timing.debug, || jpeg2k::with_codec_diagnostics(timing.debug, || engine.decode(request)));
                if timing.debug { timing.details.push(details); }
                timing.finish("render");
                drop(permit);
                if result.is_ok() { if let Some(done) = preview_done { let _ = done.send(()); } }
                match result { Ok(tiles) => Ok((queue_ms, timing, select_tile(tiles))), Err(error) => {
                    let stats=prepared.as_ref().map(|p|serde_json::json!({"route":p.route,"sourceBytesRead":p.stats.bytes,
                        "rangeRequests":p.stats.requests,"cacheBytes":p.stats.cache_bytes,"prepareMs":p.elapsed_ms,
                        "viewReduction":p.view_reduction,"technicalOrigin":p.technical_origin,"fileRanges":p.file_ranges,
                        "tileOffset":[p.tile_offset_x,p.tile_offset_y],"tileCount":p.original_tiles.len(),
                        "predecoded":p.predecoded.is_some(),"overlappedDecodeMs":p.overlapped_decode_ms,"memoryInputBytes":memory_bytes}));
                    Err((error,timing,input,stats))
                } }
            }).await
            }
        };
        let (queue_ms, mut timing, decoded) = match decode {
            Ok(Ok((queue_ms, timing, Some(decoded)))) => (queue_ms, timing, decoded),
            Ok(Err((error,timing,input,stats))) => {
                let mut response = errors::resource_reply(&error).unwrap_or_else(|| errors::reply(
                    StatusCode::UNPROCESSABLE_ENTITY,"source_decode_failed",format!("{error:#}"),serde_json::json!({})));
                if timing.debug { timing.headers(response.headers_mut()); }
                let mut response=failure(response,"render",Some(input));
                if let Some(stats)=stats {
                    response.extensions_mut().get_mut::<errors::TileFailure>().unwrap().0["preparedInput"]=stats;
                }
                return response;
            }
            Ok(Ok((_,timing,None))) => {
                let mut response=errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"decode_worker_failed",
                    "The decoder returned no raster for the requested tile coordinates.",serde_json::json!({}));
                if timing.debug { timing.headers(response.headers_mut()); }
                return failure(response,"select_tile",None);
            }
            Err(error) => return failure(errors::reply(StatusCode::INTERNAL_SERVER_ERROR,
                "decode_worker_failed",error.to_string(),serde_json::json!({})),"render_worker",None),
        };
        let trace_id = current_trace_id();
        let output_bytes = decoded.raster.bytes.len();
        let mut response = decoded.raster.bytes.into_response();
        let headers = response.headers_mut();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static(decoded.raster.content_type),
        );
        headers.insert(
            "x-river-trace-id",
            HeaderValue::from_str(&trace_id).unwrap_or(HeaderValue::from_static("unknown")),
        );
        headers.insert(
            "x-river-decoder-route",
            HeaderValue::from_static(decoded.raster.decoder_route),
        );
        headers.insert("x-river-selected-driver", HeaderValue::from_static(driver.as_str()));
        headers.insert("x-river-decoder-backend", HeaderValue::from_static(decoded.raster.decoder_backend));
        headers.insert(
            "x-river-queue-ms",
            HeaderValue::from_str(&format!("{queue_ms:.3}")).unwrap(),
        );
        if let Some((hits, misses)) = decoded.raster.block_counts {
            headers.insert("x-river-block-hits", HeaderValue::from(hits as u64));
            headers.insert("x-river-block-misses", HeaderValue::from(misses as u64));
        }
        if let Some(stages) = decoded.raster.grok_stages {
            for (header, duration) in [
                ("x-river-grok-native-ms", Some(stages.native_ms)),
                ("x-river-grok-decompress-ms", stages.decompress_ms),
                ("x-river-grok-conversion-ms", Some(stages.conversion_ms)),
            ] {
                if let Some(ms) = duration {
                    headers.insert(header, HeaderValue::from_str(&format!("{ms:.3}")).unwrap());
                }
            }
        }
        headers.insert(
            "x-river-decode-ms",
            HeaderValue::from_str(&format!("{:.3}", decoded.raster.decode_ms)).unwrap(),
        );
        headers.insert(
            "x-river-encode-ms",
            HeaderValue::from_str(&format!("{:.3}", decoded.raster.encode_ms)).unwrap(),
        );
        headers.insert(
            "x-river-source-cache",
            HeaderValue::from_static(decoded.raster.source_cache_status),
        );
        headers.insert(
            "x-river-source-cache-ms",
            HeaderValue::from_str(&format!("{:.3}", decoded.raster.source_cache_ms)).unwrap(),
        );
        for (name, value) in [
            ("x-river-source-read-bytes", decoded.raster.source_read_bytes),
            ("x-river-source-range-requests", decoded.raster.source_range_requests),
            ("x-river-source-cache-hit-bytes", decoded.raster.source_cache_hit_bytes),
        ] { headers.insert(name, HeaderValue::from_str(&value.to_string()).unwrap()); }
        timing.finish("response");
        if timing.debug && timing.stages.iter().any(|(name, _)| *name == "render") {
            timing.stages.extend([("decoder", decoded.raster.decode_ms), ("encode", decoded.raster.encode_ms)]);
            if let Some(grok) = decoded.raster.grok_stages {
                timing.stages.extend([("grok_native", grok.native_ms), ("grok_conversion", grok.conversion_ms)]);
                if let Some(ms) = grok.decompress_ms { timing.stages.push(("grok_decompress", ms)); }
            }
        }
        timing.headers(headers);
        response.extensions_mut().insert(address(asset_id.0));
        response.extensions_mut().insert(console_log::TileInfo {
            output_bytes, width: decoded.raster.width, height: decoded.raster.height,
        });
        response
    };
    if shared_miss {
        let mut cancelled = cancellation.expect("shared misses have a demand lease");
        let worker = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancelled.wait_for(|gone| *gone) => StatusCode::REQUEST_TIMEOUT.into_response(),
                response = render => response,
            }
        }.in_current_span());
        worker.await.unwrap_or_else(|error| {
            tracing::error!(%error, "WMTS render task failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
    } else {
        render.await
    }
}

fn init_tracing() -> anyhow::Result<(SdkTracerProvider, Option<opentelemetry_sdk::logs::SdkLoggerProvider>)> {
    // Declare service identity on the provider itself.  Relying on an SDK
    // default makes trace exploration needlessly ambiguous in a real shared
    // collector, where "unknown_service" is not a usable operational label.
    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "river-server".to_owned());
    let builder = SdkTracerProvider::builder()
        .with_resource(Resource::builder().with_service_name(service_name.clone()).build());
    let provider = if std::env::var("RIVER_OTEL_ENABLED").as_deref() == Ok("false") {
        builder.build()
    } else {
        builder.with_batch_exporter(SpanExporter::builder().with_tonic().build()?).build()
    };
    let tracer = provider.tracer("river-server");
    let logging = console_log::initialize(configuration::snapshot()?.logging)?;
    let logs = if logging.otlp_logs {
        let exporter = opentelemetry_otlp::LogExporter::builder().with_tonic().build()?;
        Some(opentelemetry_sdk::logs::SdkLoggerProvider::builder()
            .with_resource(Resource::builder().with_service_name(service_name).build())
            .with_batch_exporter(exporter).build())
    } else { None };
    let minimum = tracing_subscriber::EnvFilter::try_new(logging.minimum_level.as_str())?;
    let filter = if std::env::var_os("RIVER_LOG_LEVEL").is_some() || configuration::snapshot()?.logging.is_some() {
        minimum
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or(minimum)
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(console_log::Console::start(&logging)?)
        .with(logs.as_ref().map(|provider| console_log::StructuredLogs::new(provider, logging.verbose)))
        .with(OpenTelemetryLayer::new(tracer))
        .init();
    Ok((provider, logs))
}

#[instrument(name = "river.http.health")]
async fn health() -> &'static str {
    "ok"
}

async fn ht_cache_status() -> Json<serde_json::Value> {
    #[cfg(all(feature = "nvjpeg2000", target_os = "linux"))]
    { return Json(match ht_cache::global() { Ok(cache) => serde_json::to_value(cache.status()).unwrap(), Err(error) => serde_json::json!({"error": error.to_string()}) }); }
    #[cfg(not(all(feature = "nvjpeg2000", target_os = "linux")))]
    Json(serde_json::json!({"enabled": false}))
}

#[instrument(name = "river.api.decoder_drivers", skip(state))]
async fn list_decoder_drivers(
    State(state): State<AppState>,
) -> Json<Vec<engine::DecoderDriverInfo>> {
    Json(state.tile_engines.info())
}

#[instrument(name = "river.api.raster_formats")]
async fn list_raster_formats() -> Json<Vec<serde_json::Value>> {
    Json(
        raster_format_options()
            .into_iter()
            .map(|(format, available)| {
                serde_json::json!({
                    "id": format.as_str(),
                    "mimeType": format.content_type(),
                    "available": available,
                    "reason": if available { serde_json::Value::Null } else { serde_json::json!("River was built without libjxl") }
                })
            })
            .collect(),
    )
}

fn raster_format_options() -> [(RasterFormat, bool); 4] {
    [
        (RasterFormat::Webp, true),
        (RasterFormat::Png, true),
        (RasterFormat::Avif, true),
        (RasterFormat::Jxl, cfg!(feature = "jxl")),
    ]
}

fn parse_decoder_driver(value: &str) -> Option<DecoderDriver> {
    match value {
        "" | "auto" | "openjpeg" => Some(DecoderDriver::OpenJpeg),
        "jp2grok" => Some(DecoderDriver::Jp2Grok),
        "openhtj2k" => Some(DecoderDriver::OpenHtj2k),
        "nvjpeg2000" => Some(DecoderDriver::NvJpeg2000),
        _ => None,
    }
}

#[derive(Default,serde::Deserialize)]
#[serde(rename_all="camelCase")]
struct ManifestQuery {#[serde(default)] include_structure:bool}

#[instrument(name = "river.http.manifest", skip(state,query), fields(river.asset_id = %asset_id))]
async fn manifest(State(state): State<AppState>, Path(asset_id): Path<String>,query:Result<Query<ManifestQuery>,axum::extract::rejection::QueryRejection>) -> Response {
    let query=match query {Ok(Query(query))=>query,Err(_)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_manifest_query",
        "includeStructure must be true or false.",serde_json::json!({"parameter":"includeStructure"}))};
    if let Some((source_id, key)) = remote_asset_key(&asset_id) {
        let response = prepare_s3_image(State(state.clone()), Path(source_id), Json(S3AssetInput {
            key, width: None, height: None, bit_depth: None,
        })).await;
        if !response.status().is_success() { return response; }
    }
    let asset_id = AssetId(asset_id);
    match resolved_asset(&state,&asset_id).await {
        Ok(Some(asset)) => {
            let mut manifest = serde_json::to_value(&asset.manifest).unwrap();
            if asset.manifest.asset_id.0.starts_with("filev-") { manifest["identityKind"] = serde_json::json!("file-version"); }
            manifest["tilesAvailable"] = serde_json::json!(asset.source.as_ref().is_some_and(Jpeg2000Source::is_available));
            if query.include_structure {manifest["codestreamStructure"]=serde_json::json!({"state":"headerUnavailable","inspectionDoesNotFetchSource":true});}
            if let Some(header) = asset.source.as_ref().and_then(|source| source.remote.as_ref()).map(|remote| &remote.header)
                .or_else(|| asset.local_header.get()) {
                if query.include_structure {
                    manifest["codestreamStructure"]=asset.source.as_ref().and_then(|s|s.remote.as_ref())
                        .map_or_else(||header.structure_summary(),|remote|remote.structure_summary());
                }
                manifest["codestreamCoding"] = serde_json::json!(if header.is_ht { "htj2k" } else { "jpeg2000" });
                manifest["referenceOrigin"] = serde_json::json!({"x":header.image_origin.0,"y":header.image_origin.1});
                manifest["supportedDecoderDrivers"] = if header.image_origin != (0,0) {
                    if header.is_ht { serde_json::json!(["jp2grok","openhtj2k"]) } else { serde_json::json!(["openjpeg","jp2grok","openhtj2k"]) }
                } else if header.is_ht {
                    serde_json::json!(["jp2grok", "openhtj2k", "nvjpeg2000"])
                } else { serde_json::json!(["openjpeg", "jp2grok", "openhtj2k", "nvjpeg2000"]) };
            }
            Json(manifest).into_response()
        },
        Err(error) => errors::reply(StatusCode::UNPROCESSABLE_ENTITY,"local_header_failed",format!("Cannot read the image header: {error:#}"),serde_json::json!({"assetId":asset_id.0})),
        Ok(None) => errors::reply(StatusCode::NOT_FOUND,"asset_not_found","The requested image is unavailable. Open it from a connected source before inspecting its manifest.",serde_json::json!({"assetId":asset_id.0})),
    }
}

async fn resolved_asset(state: &AppState, id: &AssetId) -> anyhow::Result<Option<ImageState>> {
    if !state.assets.read().await.contains_key(id) {
        if let Some((source,key))=remote_asset_key(&id.0).or_else(||image_state::lookup_locator(&id.0)) {
            if source.is_empty() && id.0.starts_with("filev-") {
                let asset = compatibility::resolve_local(state,&key).await?;
                anyhow::ensure!(asset.manifest.asset_id == *id,"Local file version changed; reopen the original local key");
                return Ok(Some(asset));
            }
            let response=prepare_s3_image(State(state.clone()),Path(source),Json(S3AssetInput{key,width:None,height:None,bit_depth:None})).await;
            if !response.status().is_success() {
                let status=response.status();
                let bytes=axum::body::to_bytes(response.into_body(),65536).await?;
                let payload:serde_json::Value=serde_json::from_slice(&bytes).unwrap_or_default();
                anyhow::bail!("Cannot reload image metadata ({status}, {}): {}",
                    payload["code"].as_str().unwrap_or("unknown"),payload["message"].as_str().unwrap_or("no source detail"));
            }
            anyhow::ensure!(state.assets.read().await.contains_key(id),"Source version changed; reopen the original image URL to obtain current metadata");
        }
    }
    let Some(mut asset) = state.assets.read().await.get(id).cloned() else { return Ok(None); };
    let Some(source) = asset.source.as_ref().filter(|source| source.remote.is_none()).cloned()
        else { return Ok(Some(asset)); };
    if asset.local_header.get().is_none() && !source.is_available() { return Ok(Some(asset)); }
    let path = source.path.clone();
    let reader = asset.local_reader.get_or_try_init(|| async {
        let permit = state.source_io_slots.clone().acquire_owned().await?;
        let runtime = tokio::runtime::Handle::current();
        let cache = state.s3_range_cache.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            remote_source::RemoteSource::open_file(&path,runtime,cache).map(|(reader,_)| reader)
        }).await.context("local metadata reader stopped")?
    }).await?.clone();
    let header=reader.header.clone();
    asset.local_header.get_or_init(||async{header.clone()}).await;
    let attach_ranges=reader.local_ranges_supported();
    let changed = attach_ranges || asset.manifest.width != header.width || asset.manifest.height != header.height
        || asset.manifest.bit_depth != header.bit_depth || asset.manifest.components != header.components
        || asset.manifest.resolution_levels != header.levels
        || asset.manifest.quality.cumulative_layer_bytes.len() != usize::from(header.layers)
        || source.native_tile_layout != Some(header.layout) || source.reference_origin != header.image_origin;
    if changed {
        asset.manifest.width = header.width; asset.manifest.height = header.height;
        asset.manifest.components = header.components; asset.manifest.bit_depth = header.bit_depth;
        asset.manifest.resolution_levels = header.levels;
        if asset.manifest.quality.cumulative_layer_bytes.len() != usize::from(header.layers) {
            let bytes = asset.manifest.quality.cumulative_layer_bytes.last().copied().unwrap_or(0);
            asset.manifest.quality.cumulative_layer_bytes = (1..=header.layers).map(|layer| bytes*u64::from(layer)/u64::from(header.layers)).collect();
        }
        if let Some(source) = &mut asset.source {
            source.width = header.width; source.height = header.height; source.native_tile_layout = Some(header.layout);
            source.reference_origin = header.image_origin;
            if attach_ranges {source.remote=Some(reader.clone());}
        }
        if let Some(stored) = state.assets.write().await.get_mut(id) {
            if stored.source.as_ref().is_some_and(|stored| stored.path == source.path && stored.remote.is_none()) {
                stored.manifest = asset.manifest.clone(); stored.source = asset.source.clone();
            }
        }
    }
    Ok(Some(asset))
}

fn remote_asset_id(source_id: &str, key: &str) -> String {
    format!("s3-{}", hex::encode(serde_json::to_vec(&(source_id, key)).unwrap()))
}
fn remote_asset_key(id:&str)->Option<(String,String)> {serde_json::from_slice(&hex::decode(id.strip_prefix("s3-")?).ok()?).ok()}

fn valid_s3_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

#[instrument(name = "river.http.s3.source.add", skip(state, input), fields(source_id = %input.id))]
async fn add_s3_source(
    State(state): State<AppState>,
    Json(input): Json<S3SourceInput>,
) -> Response {
    let _configuration=state.configuration_lock.lock().await;
    let source = match source_from_input(&input) {
        Ok(source) => source,
        Err(error) => return errors::reply(StatusCode::BAD_REQUEST,"invalid_s3_source",format!("Invalid S3 connection: {error:#}"),serde_json::json!({"sourceId":input.id})),
    };
    let _load = state.s3_load_lock.write().await;
    let mut sources = state.s3_sources.write().await;
    if sources.len() >= 100 && !sources.contains_key(&input.id) {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"code":"s3_source_limit","message":"At most 100 connections are supported."}))).into_response();
    }
    let mut replacement = sources.clone();
    replacement.insert(input.id.clone(), source.clone());
    if source_connections::save(&replacement).is_err() { return errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"source_settings_persistence_failed","Could not persist S3 connections. Check write access and free space for the River configuration volume.",serde_json::json!({})); }
    *sources = replacement;
    read_policy::reset(&input.id);
    drop(sources);
    state.assets.write().await.retain(|id, asset| remote_asset_key(&id.0).is_none_or(|(source_id, _)| source_id != input.id) && asset.source.as_ref().and_then(|s| s.remote.as_ref()).is_none_or(|remote| remote.local_path.is_some() || remote.source_id != input.id));
    Json(serde_json::json!({
        "id": source.id, "name": source.name, "endpoint": source.endpoint,
        "bucket": source.bucket, "region": source.region, "prefix": source.prefix,
        "credentialsStored": true, "credentialsReturned": false, "maxRetries":source.connection.max_retries.unwrap_or(2)
    }))
    .into_response()
}

async fn list_s3_sources(State(state): State<AppState>) -> Response {
    let sources = state
        .s3_sources
        .read()
        .await
        .values()
        .map(|source| {
            serde_json::json!({
                "id": source.id, "name": source.name, "endpoint": source.endpoint,
                "bucket": source.bucket, "region": source.region, "prefix": source.prefix,
                "credentialsStored": true, "credentialsReturned": false, "maxRetries":source.connection.max_retries.unwrap_or(2)
            })
        })
        .collect::<Vec<_>>();
    Json(sources).into_response()
}

async fn remove_s3_source(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
) -> Response {
    let _configuration=state.configuration_lock.lock().await;
    let _load = state.s3_load_lock.write().await;
    let mut sources = state.s3_sources.write().await;
    let mut replacement = sources.clone();
    if replacement.remove(&source_id).is_some() {
        if source_connections::save(&replacement).is_err() { return errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"source_settings_persistence_failed","Could not persist S3 connections. Check write access and free space for the River configuration volume.",serde_json::json!({})); }
        *sources = replacement;
        read_policy::reset(&source_id);
        drop(sources);
        state.assets.write().await.retain(|id, asset| remote_asset_key(&id.0).is_none_or(|(id, _)| id != source_id) && asset.source.as_ref().and_then(|s| s.remote.as_ref()).is_none_or(|remote| remote.local_path.is_some() || remote.source_id != source_id));
        StatusCode::NO_CONTENT.into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"code":"s3_source_not_found"})),
        )
            .into_response()
    }
}

fn is_jpeg2000_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [".jp2", ".j2k", ".j2c", ".jpc", ".jph", ".jhc"].iter().any(|extension| key.ends_with(extension))
}

#[instrument(name = "river.http.s3.asset.load", skip(state, input), fields(source_id = %source_id, key = %input.key))]
async fn prepare_s3_image(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    Json(input): Json<S3AssetInput>,
) -> Response {
    let registration_started = std::time::Instant::now();
    // Configuration mutations exclude opens, while distinct objects may load
    // concurrently. Duplicate requests share only their own object's gate.
    let _load = state.s3_load_lock.read().await;
    let key = input.key.trim_start_matches('/');
    if !is_jpeg2000_key(key) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"code":"unsupported_object","message":"JPEG 2000 .jp2/.j2k/.j2c/.jpc and HTJ2K .jph/.jhc objects are accepted."}))).into_response();
    }
    let Some(source) = state.s3_sources.read().await.get(&source_id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"code":"s3_source_not_found"})),
        )
            .into_response();
    };
    let remote_id = remote_asset_id(&source_id, key);
    let gate = {
        let mut gates = state.s3_object_loads.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&remote_id).and_then(std::sync::Weak::upgrade) { gate }
        else {
            let gate = Arc::new(Mutex::new(()));
            gates.insert(remote_id.clone(), Arc::downgrade(&gate));
            gate
        }
    };
    let _object_load = gate.lock().await;
    let cached=state.assets.read().await.get(&AssetId(remote_id.clone())).cloned();
    let age=state.assets.read().await.age(&AssetId(remote_id.clone()));
    let ttl=std::env::var("RIVER_S3_METADATA_TTL_MS").ok().and_then(|v|v.parse().ok()).unwrap_or(30000u64);
    if let Some(asset)=cached.filter(|_|age.is_some_and(|age|age<Duration::from_millis(ttl))) {
        if input.width.is_some_and(|w| w != asset.manifest.width)
            || input.height.is_some_and(|h| h != asset.manifest.height)
            || input.bit_depth.is_some_and(|d| d != asset.manifest.bit_depth) {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"code":"metadata_mismatch"}))).into_response();
        }
        return Json(serde_json::json!({"assetId":asset.manifest.asset_id.0,
            "width":asset.manifest.width,"height":asset.manifest.height,
            "bytes":asset.manifest.quality.cumulative_layer_bytes.last(),"bytesFetched":0,"rangeRequests":0,"serverRegistrationMs":registration_started.elapsed().as_secs_f64()*1000.0,"headerProbeMs":0.0,"identityKind":"s3-version","cached":true})).into_response();
    }
    let location =
        match ObjectPath::parse(key) {
            Ok(location) => location,
            Err(error) => return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"code":"invalid_object_key","message":format!("{error:#}")})),
            )
                .into_response(),
        };
    if !source.prefix.is_empty()
        && !key.starts_with(&format!("{}/", source.prefix))
        && key != source.prefix
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"code":"object_outside_source_prefix"})),
        )
            .into_response();
    }
    let probe_started = std::time::Instant::now();
    let io_permit = match state.source_io_slots.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return errors::reply(StatusCode::SERVICE_UNAVAILABLE,"scheduler_unavailable","The source-I/O scheduler has shut down.",serde_json::json!({})),
    };
    let probe = match tokio::time::timeout(Duration::from_secs(10), async {
        let profile=read_policy::profile(&source.id);let _source_permit=profile.acquire().await;
        remote_source::InitialRead::fetch(source.store.as_ref(),&location).await
    }).await {
        Ok(Ok(probe)) => probe,
        Ok(Err(error)) if matches!(error.downcast_ref::<object_store::Error>(),Some(object_store::Error::NotFound {..})) => return errors::reply(StatusCode::NOT_FOUND,"s3_object_not_found","The object does not exist in the connected S3 source. Check its key and the source bucket/prefix.",serde_json::json!({"sourceId":source_id,"key":key})),
        Err(_) => return errors::reply(StatusCode::GATEWAY_TIMEOUT,"s3_probe_timeout","S3 did not return its bounded header probe within 10 seconds. Check source connectivity and provider latency.",serde_json::json!({"sourceId":source_id,"key":key})),
        Ok(Err(error)) => return errors::reply(StatusCode::BAD_GATEWAY,"s3_probe_failed",format!("Cannot read the initial S3 header range: {error:#}"),serde_json::json!({"sourceId":source_id,"key":key})),
    };
    let probe_ms = probe_started.elapsed().as_secs_f64() * 1000.0;
    let meta=&probe.meta;
    // This is a version identity, deliberately distinct from a content SHA-256.
    let version_id = format!("s3v-{}", hex::encode(Sha256::digest(serde_json::to_vec(&(
        &source.id, &source.endpoint, &source.bucket, key, meta.size, &meta.e_tag, &meta.version, meta.last_modified
    )).unwrap())));
    let asset_id=version_id.clone();
    let runtime = tokio::runtime::Handle::current();
    let store = source.store.clone();
    let cache = state.s3_range_cache.clone();
    let id = version_id.clone();
    let source_id_for_reader = source.id.clone();
    let size = meta.size;
    #[cfg(all(feature="grok",unix))] let native_region=source.region.clone();
    #[cfg(all(feature="grok",unix))] let native_allow_http=source.connection.allow_http;
    #[cfg(all(feature="grok",unix))] let native_max_retries=source.connection.max_retries.unwrap_or(2) as usize;
    let loaded = tokio::task::spawn_blocking(move || {
        // A disconnected caller must not release admission while its blocking
        // header reader is still running.
        let _io_permit = io_permit;
        #[cfg(all(feature="grok",unix))]
        {remote_source::RemoteSource::open_s3_from_probe(source_id_for_reader,id,store,location,probe,runtime,cache,native_region,native_allow_http,native_max_retries)}
        #[cfg(not(all(feature="grok",unix)))]
        {remote_source::RemoteSource::open_from_probe(source_id_for_reader,id,store,location,probe,runtime,cache)}
    }).await;
    let (remote, stats) = match loaded {
        Ok(Ok(loaded)) => loaded,
        Ok(Err(error)) => {
            let mut details = remote_source::header_error_details(&error);
            details["sourceId"] = serde_json::json!(source.id);
            details["assetVersion"] = serde_json::json!(version_id);
            details["manifestLocator"] = serde_json::json!(remote_asset_id(&source.id,key));
            details["sourceBytes"] = serde_json::json!(size);
            return errors::reply(StatusCode::UNPROCESSABLE_ENTITY,"s3_header_failed",format!("Cannot read JPEG 2000 header: {error:#}"),details);
        }
        Err(error) => return errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"s3_header_worker_failed",format!("The image header worker stopped: {error}"),serde_json::json!({})),
    };
    let header = &remote.header;
    if input.width.is_some_and(|w| w != header.width) || input.height.is_some_and(|h| h != header.height)
        || input.bit_depth.is_some_and(|d| d != header.bit_depth) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"code":"metadata_mismatch"}))).into_response();
    }
    let mut source_file = Jpeg2000Source::from_header_layout(PathBuf::from(key), header.width, header.height, Some(header.layout));
    source_file.reference_origin = header.image_origin;
    source_file.components=Some(header.components);source_file.display_rgb_only=header.display_rgb_only;source_file.band_fallback=header.band_fallback;
    source_file.remote = Some(remote.clone());
    let asset = ImageState {
        local_header: Arc::new(tokio::sync::OnceCell::new()), local_reader: Arc::new(tokio::sync::OnceCell::new()),
        manifest: AssetManifest {
            asset_id: AssetId(asset_id.clone()),
            width: header.width,
            height: header.height,
            components: header.components,
            bit_depth: header.bit_depth,
            resolution_levels: header.levels,
            quality: QualityModel {
                cumulative_layer_bytes: vec![size],
            },
            color: ColorPolicy {
                output: "sRGB/RGBA 8-bit".to_owned(),
                source_profile: SourceColorProfile::Embedded,
                assumption_recorded: true,
            },
            capability: Capability::Limited { reasons: vec!["S3 packet selection depends on codestream indexing; bounded whole-tile reads are used when packet selection is unavailable".into()] },
        },
        source: Some(source_file),
    };
    let locator=(asset_id.clone(),source.id.clone(),key.to_owned());
    tokio::task::spawn_blocking(move||{if let Err(error)=image_state::remember_locator(&locator.0,&locator.1,&locator.2){tracing::debug!(%error,"asset locator persistence unavailable; original URLs remain demand-loadable");}}).await.ok();
    state
        .assets
        .write()
        .await
        .insert(AssetId(asset_id.clone()), asset.clone());
    state.assets.write().await.insert(AssetId(remote_id), asset);
    Json(serde_json::json!({"assetId":asset_id,"width":header.width,"height":header.height,"bytes":size,"bytesFetched":stats.bytes,"rangeRequests":stats.requests,"serverRegistrationMs":registration_started.elapsed().as_secs_f64()*1000.0,"headerProbeMs":probe_ms,"identityKind":"s3-version","loadedFrom":{"sourceId":source.id,"bucket":source.bucket,"key":key},"cache":"bounded compressed ranges; no full-file download"})).into_response()
}

fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1_000.0
}

fn current_trace_id() -> String {
    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
        .to_string()
}

fn decode_concurrency_from_env() -> usize { resources::plan().decode_concurrency }

fn file_version_id(path: &FsPath) -> anyhow::Result<String> {
    let absolute = fs::canonicalize(path)?;
    let metadata = fs::metadata(&absolute)?;
    let identity = serde_json::to_vec(&(&absolute,metadata.len(),metadata.modified()?))?;
    Ok(format!("filev-{}",hex::encode(Sha256::digest(identity))))
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn file_version_identity_is_stable_and_tracks_size_and_path_without_content_hashing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one.jp2");
        std::fs::write(&path,b"first").unwrap();
        let first = super::file_version_id(&path).unwrap();
        assert_eq!(first,super::file_version_id(&path).unwrap());
        assert!(first.starts_with("filev-"));
        assert!(river_core::AssetId(first.clone()).validate().is_ok());
        std::fs::write(&path,b"second-size").unwrap();
        assert_ne!(first,super::file_version_id(&path).unwrap());
        let other = directory.path().join("other.jp2");
        std::fs::copy(&path,&other).unwrap();
        assert_ne!(super::file_version_id(&path).unwrap(),super::file_version_id(&other).unwrap());
    }
    #[test]
    fn remote_identity_preserves_nested_and_unicode_keys() {
        let key = "folder/שלום space.jp2";
        assert_eq!(remote_asset_key(&remote_asset_id("minio", key)), Some(("minio".into(), key.into())));
        assert_eq!(remote_asset_key("s3-invalid"), None);
    }
    #[test]
    fn loading_requires_only_an_object_key() {
        let input: S3AssetInput = serde_json::from_str(r#"{"key":"nested/image.jp2"}"#).unwrap();
        assert!(input.width.is_none() && input.height.is_none() && input.bit_depth.is_none());
    }
    #[test]
    fn discovery_and_loading_accept_raw_codestream_aliases() {
        for key in ["nested/image.j2c", "IMAGE.J2C", "nested/image.jpc", "image.j2k", "image.jp2", "image.jph", "image.jhc"] {
            assert!(is_jpeg2000_key(key), "{key}");
        }
        for key in ["image.j2c.tmp", "image.jpeg", "image.jp2/metadata.json"] {
            assert!(!is_jpeg2000_key(key), "{key}");
        }
    }
}

fn source_from_input(input: &S3SourceInput) -> anyhow::Result<S3Source> {
    let max_retries=s3_retry_limit(input)?;
    anyhow::ensure!(valid_s3_id(&input.id), "id must contain 1..64 ASCII letters, digits, underscores or hyphens");
    anyhow::ensure!(!input.name.trim().is_empty(), "name must not be empty");
    anyhow::ensure!(!input.bucket.trim().is_empty(), "bucket must not be empty");
    anyhow::ensure!(!input.region.trim().is_empty(), "region must not be empty");
    anyhow::ensure!(!input.access_key_id.is_empty(), "accessKeyId must not be empty");
    anyhow::ensure!(!input.secret_access_key.is_empty(), "secretAccessKey must not be empty");
    if let Some(endpoint) = input.endpoint.as_deref() {
        anyhow::ensure!(endpoint.starts_with("http://") || endpoint.starts_with("https://"), "endpoint must use http:// or https://");
        anyhow::ensure!(!endpoint.contains('@') && !endpoint.contains('?') && !endpoint.contains('#'), "endpoint must not contain user credentials, query parameters or fragments");
        anyhow::ensure!(!endpoint.starts_with("http://") || input.allow_http, "HTTP endpoint requires allowHttp=true");
    }

    if !valid_s3_id(&input.id)
        || input.name.trim().is_empty()
        || input.bucket.trim().is_empty()
        || input.access_key_id.is_empty()
        || input.secret_access_key.is_empty()
        || (input
            .endpoint
            .as_deref()
            .is_some_and(|url| url.starts_with("http://"))
            && !input.allow_http)
        || input
            .endpoint
            .as_deref()
            .is_some_and(|url| !(url.starts_with("http://") || url.starts_with("https://")))
        || input
            .endpoint
            .as_deref()
            .is_some_and(|url| url.contains('@') || url.contains('?') || url.contains('#'))
 { anyhow::bail!("Invalid S3 connection configuration"); }
    let (client_options,connector)=source_connections::transport(input.allow_http)?;
    let mut builder = AmazonS3Builder::new()
        .with_client_options(client_options)
        .with_retry(object_store::RetryConfig {max_retries,retry_timeout:Duration::from_secs(10),..Default::default()})
        .with_http_connector(connector)
        .with_region(&input.region)
        .with_bucket_name(&input.bucket)
        .with_access_key_id(&input.access_key_id)
        .with_secret_access_key(&input.secret_access_key)
        .with_allow_http(input.allow_http)
        .with_virtual_hosted_style_request(input.virtual_hosted_style);
    if let Some(endpoint) = &input.endpoint {
        builder = builder.with_endpoint(endpoint);
    }
    if let Some(token) = &input.session_token {
        builder = builder.with_token(token);
    }
    Ok(S3Source {
        id: input.id.clone(),
        name: input.name.clone(),
        endpoint: input.endpoint.clone(),
        bucket: input.bucket.clone(),
        region: input.region.clone(),
        prefix: input.prefix.trim_matches('/').to_owned(),
        store: Arc::new(builder.build()?),
        connection: {let mut saved=input.clone();saved.max_retries=Some(max_retries as u32);saved},
    })
}

async fn get_encoding_settings()->Json<encoding_settings::Settings> {
    Json(encoding_settings::Settings {webp_method:encoding_settings::snapshot()})
}
async fn put_encoding_settings(State(state):State<AppState>,Json(settings):Json<encoding_settings::Settings>)->Response {
    if settings.webp_method>6 {return (StatusCode::BAD_REQUEST,Json(serde_json::json!({"code":"invalid_webp_method","message":"webpMethod must be an integer from 0 through 6"}))).into_response();}
    let _configuration=state.configuration_lock.lock().await;
    let result=tokio::task::spawn_blocking(move||encoding_settings::update(settings)).await;
    if !matches!(result,Ok(Ok(()))) {return (StatusCode::INTERNAL_SERVER_ERROR,Json(serde_json::json!({"code":"encoding_settings_persistence_failed"}))).into_response();}
    Json(settings).into_response()
}

#[cfg(all(test,not(feature="grok")))]
#[path="decoder/grok/packed.rs"]
mod packed_view_parser_tests;

#[cfg(all(test,not(feature="grok")))]
#[path="decoder/grok/cancellation.rs"]
mod grok_cancellation_tests;

#[cfg(test)]mod source_retry_tests {
 use super::*;
 fn input(n:Option<u32>)->S3SourceInput {let mut v=serde_json::json!({"id":"retry-proof","name":"Retry proof","bucket":"fixture","region":"us-east-1","endpoint":"http://127.0.0.1:9","accessKeyId":"fixture","secretAccessKey":"fixture","allowHttp":true});if let Some(n)=n {v["maxRetries"]=n.into();}serde_json::from_value(v).unwrap()}
 #[test]fn explicit_retry_limits_are_validated_and_canonicalized_without_source_io() {
  for n in [0,1,2,10] {let s=source_from_input(&input(Some(n))).unwrap();assert_eq!(s.connection.max_retries,Some(n));}
  assert!(source_from_input(&input(Some(11))).err().unwrap().to_string().contains("maxRetries"));
 }
 #[test]fn legacy_source_json_does_not_require_a_retry_field() {
  let s=input(None);assert_eq!(s.max_retries,None);assert!(serde_json::to_value(&s).unwrap().get("maxRetries").is_none());
 }
}
