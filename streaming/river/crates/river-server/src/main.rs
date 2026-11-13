use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Read,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::ListenerExt,
};
use futures_util::{SinkExt, StreamExt};
use object_store::{aws::{AmazonS3, AmazonS3Builder}, list::{PaginatedListOptions, PaginatedListStore}, path::Path as ObjectPath};
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use river_core::{
    AssetId, AssetManifest, Capability, ClientMessage, ColorPolicy, DecoderDriver, PlannedPass,
    QualityModel, RasterFormat, ServerMessage, ServerTiming, SourceColorProfile, TILE_SIZE,
    TileFrameHeader, Viewport, encode_tile_frame, plan_passes,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use tower_http::services::ServeDir;
use tracing::{Instrument, info_span, instrument};
use tracing_opentelemetry::{OpenTelemetryLayer, OpenTelemetrySpanExt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

mod cors;
mod resources;
mod asset_registry;
mod display;
mod cache_inventory;
mod decoder;
mod console_log;
mod edges;
mod engine;
mod source_admission;
mod requests;
mod api_docs;
mod source_cache;
mod storage_mode;
mod adaptation;
mod shared_cache;
mod remote_source;
mod raster_cache;
mod source_connections;
mod encoding_settings;
mod configuration;
mod tls;
mod http3;
mod read_policy;
mod settings_api;
mod preview_pixels;
mod preview_plan;
mod catalog_search;
mod compatibility;
mod errors;
#[cfg(any(test,all(feature="nvjpeg2000",target_os="linux")))]
mod worker_sizing;
#[cfg(any(test,all(feature="nvjpeg2000",target_os="linux")))]
mod lease_pool;
#[cfg(all(feature = "nvjpeg2000", target_os = "linux"))]
mod cuda_native;
#[cfg(all(feature = "nvjpeg2000", target_os = "linux"))]
mod ht_cache;

use decoder::{Jpeg2000Source, NativeTileLayoutHint, sample_path};
use engine::{DecoderDrivers, TileDecodeRequest};

#[derive(Clone)]
struct AppState {
    configuration_lock: Arc<Mutex<()>>,
    assets: Arc<RwLock<asset_registry::Assets>>,
    s3_sources: Arc<RwLock<HashMap<String, S3Source>>>,
    s3_load_lock: Arc<RwLock<()>>,
    s3_object_loads: Arc<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>,
    s3_range_cache: Arc<remote_source::RangeCache>,
    mosaic_root: PathBuf,
    mosaic_discovery_in_flight: Arc<AtomicBool>,
    catalog_backed: bool,
    /// Shared CPU budget across every socket. Each viewport receives one
    /// bounded ROI batch, so many clients cannot create an unbounded backlog.
    decode_slots: Arc<adaptation::Admission>,
    source_io_slots: Arc<adaptation::Admission>,
    per_viewport_decode_concurrency: usize,
    /// Internal worker count for each native OpenJPEG decode. Combined with
    /// the shared semaphore this prevents multiplicative CPU oversubscription.
    openjpeg_threads_per_decode: usize,
    /// Native OpenJPEG tile traversal is useful only for a small number of
    /// codestream tiles. Larger windows use the bounded ROI decoder instead
    /// of turning a viewport into a serial tile-part scan.
    max_native_tiles_per_job: usize,
    preview_extra_reductions: u8,
    /// Do not begin expensive full-resolution refinement while the user is
    /// still panning. A newer viewport aborts this task during the wait,
    /// preserving the shared decoder budget for fresh first paints.
    refine_idle_delay: Duration,
    /// Source-specific execution belongs behind this interface. The default
    /// direct engine persists no generated raster material or result cache.
    tile_engines: DecoderDrivers,
    /// Generated delivery pyramids are a migration/demo optimization only.
    /// Production direct serving defaults to the source codestream's native
    /// resolution structure and never requires prebuilt raster material.
    delivery_pyramids_enabled: bool,
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


#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetCatalog {
    assets: Vec<CatalogAsset>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogAsset {
    asset_id: String,
    width: u32,
    height: u32,
    bit_depth: u8,
    source: String,
    native_tile_width: Option<u32>,
    native_tile_height: Option<u32>,
    preview_source: Option<String>,
    preview_scale: Option<u32>,
}

#[derive(Clone)]
pub(crate) struct RegisteredAsset {
    local_header: Arc<tokio::sync::OnceCell<remote_source::Header>>,
    local_reader: Arc<tokio::sync::OnceCell<Arc<remote_source::RemoteSource>>>,
    manifest: AssetManifest,
    source: Option<Jpeg2000Source>,
    /// Smallest JP2 pyramid level, used for the immediate first paint.
    preview_source: Option<Jpeg2000Source>,
    preview_scale: u32,
    /// A denser JP2 pyramid level sent once pan/zoom input has gone idle.
    gain_source: Option<Jpeg2000Source>,
    gain_scale: u32,
}

type SocketSender = Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let configuration = configuration::snapshot()?;
    adaptation::initialize(configuration.auto_adaptation_mode.as_deref())?;
    if std::env::args().any(|arg|arg=="--resource-env") {println!("{}",resources::launcher_env());return Ok(());}
    shared_cache::initialize(configuration.shared_cache)?;
    storage_mode::validate()?;
    local_identity_mode()?;
    encoding_settings::restore()?;
    remote_source::validate_serial_transport()?;
    let (tracer_provider, logger_provider) = init_tracing()?;
    storage_mode::print_startup();
    resources::start();
    let default_asset = default_asset();
    let decode_concurrency = decode_concurrency_from_env();
    let per_viewport_decode_concurrency = std::env::var("RIVER_VIEWPORT_DECODE_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|&value| value > 0)
        .unwrap_or(4)
        .min(decode_concurrency);
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
    let preview_extra_reductions = std::env::var("RIVER_PREVIEW_EXTRA_REDUCTIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let refine_idle_delay = Duration::from_millis(
        std::env::var("RIVER_REFINE_IDLE_DELAY_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(150),
    );
    let mosaic_root = std::env::var_os("RIVER_MOSAIC_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/cache/sentinel-date-mosaics"));
    let catalog_path = std::env::var_os("RIVER_ASSET_CATALOG").filter(|value| !value.is_empty()).map(PathBuf::from);
    let delivery_pyramids_enabled = std::env::var("RIVER_ENABLE_DELIVERY_PYRAMIDS")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"));
    let mut assets = BTreeMap::from([(default_asset.manifest.asset_id.clone(), default_asset)]);
    if let Some(path) = catalog_path.as_deref() {
        for asset in load_catalog(path, &mosaic_root, delivery_pyramids_enabled)? {
            assets.insert(asset.manifest.asset_id.clone(), asset);
        }
    }
    let state = AppState {
        configuration_lock: Arc::new(Mutex::new(())),
        assets: asset_registry::Assets::shared(assets),
        s3_sources: Arc::new(RwLock::new(HashMap::new())),
        s3_load_lock: Arc::new(RwLock::new(())),
        s3_object_loads: Arc::new(Mutex::new(HashMap::new())),
        s3_range_cache: remote_source::RangeCache::from_env(),
        mosaic_root,
        catalog_backed: catalog_path.is_some(),
        mosaic_discovery_in_flight: Arc::new(AtomicBool::new(false)),
        decode_slots: Arc::new(adaptation::Admission::new(decode_concurrency, "RIVER_DECODE_CONCURRENCY")),
        source_io_slots: Arc::new(adaptation::Admission::new(resources::plan().source_io_concurrency, "RIVER_SOURCE_IO_CONCURRENCY")),
        per_viewport_decode_concurrency,
        openjpeg_threads_per_decode,
        max_native_tiles_per_job,
        preview_extra_reductions,
        refine_idle_delay,
        tile_engines: DecoderDrivers::new()?,
        delivery_pyramids_enabled,
    };
    tracing::info!(
        decode_concurrency,
        per_viewport_decode_concurrency,
        openjpeg_threads_per_decode,
        max_native_tiles_per_job,
        preview_extra_reductions,
        refine_idle_delay_ms = refine_idle_delay.as_millis(),
        decoder_drivers = ?state.tile_engines.info().iter().map(|driver| (&driver.id, driver.available)).collect::<Vec<_>>(),
        delivery_pyramids_enabled,
        "configured JPEG 2000 decode concurrency"
    );

    source_connections::restore(&state).await?;
    if !state.catalog_backed { start_background_mosaic_discovery(&state); }
    let cors = cors::layer(&std::env::var("RIVER_CORS_ORIGINS").unwrap_or_else(|_| "*".into()))?;
    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(api_docs::spec))
        .route("/docs", get(api_docs::ui))
        .route("/docs/", get(api_docs::ui))
        .route("/swagger", get(|| async { axum::response::Redirect::temporary("/docs/") }))
        .nest_service("/docs/assets", ServeDir::new(std::env::var("RIVER_SWAGGER_ASSETS").unwrap_or_else(|_| "deploy/swagger".into())))
        .route("/api/assets", get(list_assets))
        .route("/api/s3/sources", get(list_s3_sources).post(add_s3_source))
        .route(
            "/api/s3/sources/{source_id}",
            axum::routing::delete(remove_s3_source),
        )
        .route("/api/s3/sources/{source_id}/objects", get(list_s3_objects))
        .route("/api/s3/sources/{source_id}/assets", post(load_s3_asset))
        .route("/api/assets/{asset_id}/manifest", get(manifest))
        .route("/api/drivers", get(list_decoder_drivers))
        .route("/api/cache/ht", get(ht_cache_status))
        .route("/api/formats", get(list_raster_formats))
        .route("/api/settings/configuration", get(settings_api::export).put(settings_api::import))
        .route("/api/diagnostics/resources", get(|| async {Json(resources::diagnostics())}))
        .route("/api/settings/adaptation", get(settings_api::get_adaptation).put(settings_api::put_adaptation).delete(settings_api::reset_adaptation))
        .route("/api/settings/shared-cache", get(settings_api::get_shared_cache).put(settings_api::put_shared_cache).delete(settings_api::reset_shared_cache))
        .route("/api/diagnostics/source-reads", get(settings_api::get_source_reads).put(settings_api::put_source_reads).delete(settings_api::clear_source_reads))
        .route("/api/settings/read-policy", get(settings_api::get_policy).put(settings_api::put_policy))
        .route("/api/settings/drivers", get(settings_api::get_drivers).put(settings_api::put_drivers))
        .route("/api/settings/encoding", get(get_encoding_settings).put(put_encoding_settings))
        .route("/wmts", get(wmts))
        .route("/streamer/v1/{z}/{x}/{y}/{file}", get(compatibility::tile))
        .route("/api/tiles/{image}", get(compatibility::tile_info))
        .route("/ws", get(websocket).connect(websocket))
        .route("/ws/tiles", get(websocket))
        .fallback_service(ServeDir::new("web").append_index_html_on_directories(true))
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
        if let Err(error) = asset_registry::warm_locator_inventory() {
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
    #[serde(rename = "LIMIT", default)]
    limit: Option<usize>,
    #[serde(rename = "CURSOR", default)]
    cursor: Option<String>,
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

#[instrument(name = "river.wmts.request", skip(state, query), fields(river.protocol = "wmts", river.output_format = %query.format, river.decoder_driver = %query.driver))]
async fn wmts(State(state): State<AppState>, Query(query): Query<WmtsQuery>) -> Response {
    render_wmts(state,query,None).await
}
async fn render_wmts(state:AppState,query:WmtsQuery,geometry:Option<compatibility::Geometry>)->Response {
    let edge_mode=match edges::Mode::parse(&query.edge) {Ok(mode)=>mode,Err(error)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_edge_mode",error.to_string(),serde_json::json!({"allowed":["crop","black","transparent"]}))};
    if !query.service.eq_ignore_ascii_case("WMTS")
        || !query.version.is_empty() && query.version != "1.0.0"
    {
        return errors::reply(StatusCode::BAD_REQUEST,"unsupported_wmts_version","SERVICE must be WMTS and VERSION must be 1.0.0 (VERSION may be omitted for capabilities).",serde_json::json!({"service":query.service,"version":query.version}));
    }
    if query.request.eq_ignore_ascii_case("GetCapabilities") {
        let (assets, next) = if !query.layer.is_empty() {
            if let Some((source_id,key)) = remote_asset_key(&query.layer) {
                let response = load_s3_asset(State(state.clone()),Path(source_id),Json(S3AssetInput {
                    key,width: None,height: None,bit_depth: None,
                })).await;
                if !response.status().is_success() { return response; }
            }
            match resolved_asset(&state,&AssetId(query.layer.clone())).await {
                Ok(Some(asset)) => (vec![asset],None),
                Ok(None) => return errors::reply(StatusCode::NOT_FOUND,"asset_not_found","The requested image is not available. List /api/assets or use a locator from a connected S3 source.",serde_json::json!({"assetId":query.layer})),
                Err(error) => return (StatusCode::UNPROCESSABLE_ENTITY,error.to_string()).into_response(),
            }
        } else {
            use std::ops::Bound::{Included,Excluded,Unbounded};
            let registry = state.assets.read().await;
            let boundary = AssetId("s3-".into());
            let local_start = match query.cursor.as_deref() {
                Some(cursor) if cursor < "s3-" => Excluded(AssetId(cursor.into())),
                Some(_) => Included(boundary.clone()),
                None => Unbounded,
            };
            let remote_start = match query.cursor.as_deref() {
                Some(cursor) if cursor >= "s3v-" => Excluded(AssetId(cursor.into())),
                _ => Included(AssetId("s3v-".into())),
            };
            let limit = page_limit(query.limit);
            let mut page = registry.range((local_start,Excluded(boundary)))
                .chain(registry.range((remote_start,Unbounded)))
                .take(limit+1).map(|(_,asset)| asset.clone()).collect::<Vec<_>>();
            let next = (page.len()>limit).then(|| page[limit-1].manifest.asset_id.0.clone());
            page.truncate(limit);
            (page,next)
        };
        // Probe only this bounded page, sharing metadata work with concurrent
        // selectors. Never advertise guessed local resolution levels.
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
        Ok(None) => return errors::reply(StatusCode::NOT_FOUND,"asset_not_found","The requested image is not available. List /api/assets or use a locator from a connected S3 source.",serde_json::json!({"assetId":query.layer})),
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
        return errors::reply(StatusCode::NOT_FOUND,"image_source_unavailable","The image is cataloged but has no readable source. Check the corpus mount or reconnect its S3 source.",serde_json::json!({"assetId":asset_id.0}));
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
    } else if prefer_grok || prefer_indexed_zoom_grok || prefer_single_native_zoom_grok {
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
    let queued_at = Instant::now();
    let (_cancel_guard, cancellation) = remote_source::CancelOnDrop::new();
    let request = TileDecodeRequest {
        progressive_final_reduction:None, output_geometry:geometry.map(|g| edges::OutputGeometry {edge:g.bitmap,
            width:(u64::from((asset.manifest.width-col*tile_edge).min(tile_edge))*u64::from(g.bitmap)).div_ceil(u64::from(tile_edge)) as u32,
            height:(u64::from((asset.manifest.height-row*tile_edge).min(tile_edge))*u64::from(g.bitmap)).div_ceil(u64::from(tile_edge)) as u32}),
        edge_mode,
        capture_pixels:false, webp_method: encoding_settings::snapshot(),
        source_id: asset_id.0.clone(), whole_view: columns == 1 && rows == 1, source, tiles: vec![(col, row)], source_tile: tile_edge,
        reduction: zoom, layers, format, decoder_threads: state.openjpeg_threads_per_decode,
        max_native_tiles: state.max_native_tiles_per_job, cached_tiles: vec![], work_ticket:None, cancellation: Some(cancellation),
    };
    let flight = match engine.flight(&request) {
        Some(gate) => Some(gate.lock_owned().await),
        None => None,
    };
    let decode = if let Some(mut tiles) = engine.cached(&request) {
        Ok(Ok((0.0, elapsed_ms(queued_at), tiles.pop())))
    } else {
        let (request, io_permit, flight) = if request.source.remote.is_some() {
            let source_gate=match request.source.remote.as_ref() {
                Some(remote)=>remote.preparation_admission(&request.tiles,request.source_tile).await,None=>None,
            };
            let Ok(io_permit) = state.source_io_slots.clone().acquire_owned().await else { return errors::reply(StatusCode::SERVICE_UNAVAILABLE,"scheduler_unavailable","The decode or source-I/O scheduler has shut down. Retry after the River instance is ready.",serde_json::json!({})); };
            let preparer = engine.clone();
            let span = tracing::Span::current();
            match tokio::task::spawn_blocking(move || {
                let _entered = span.enter();
                let index_only=request.source.remote.as_ref().is_some_and(|remote|remote.serial_index && !remote.sequential_prefix);
                            let _source_admission=source_admission::Scope::enter(source_gate,index_only);
                engine::prepare_admitted(preparer.as_ref(), request, io_permit, flight, false)
            }).await {
                Ok(Ok((request, io_permit, flight))) => (request, Some(io_permit), flight),
                Ok(Err(error)) => return errors::scratch_reply(&error).unwrap_or_else(|| errors::reply(StatusCode::UNPROCESSABLE_ENTITY,"source_prepare_failed",format!("Cannot prepare this image: {error:#}"),serde_json::json!({"assetId":asset_id.0,"driver":driver.as_str(),"matrix":level,"column":col,"row":row}))),
                Err(error) => return errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"source_prepare_worker_failed",format!("The source-preparation worker stopped unexpectedly: {error}"),serde_json::json!({"assetId":asset_id.0,"driver":driver.as_str()})),
            }
        } else { (request, None, flight) };
        if request.tiles.is_empty() {
            Ok(Ok((0.0, elapsed_ms(queued_at), request.cached_tiles.into_iter().next())))
        } else {
        let cpu_queued_at = Instant::now();
        let Ok(permit) = state.decode_slots.clone().acquire_owned().await else {
            return errors::reply(StatusCode::SERVICE_UNAVAILABLE,"scheduler_unavailable","The decode or source-I/O scheduler has shut down. Retry after the River instance is ready.",serde_json::json!({}));
        };
        drop(io_permit);
        let span = tracing::Span::current();
        tokio::task::spawn_blocking(move || {
            let _flight = flight;
            let _entered = span.enter();
            let queue_ms = elapsed_ms(cpu_queued_at);
            let result = engine.decode(request);
            drop(permit);
            result.map(|mut tiles| (queue_ms, elapsed_ms(queued_at), tiles.pop()))
        }).await
        }
    };
    let (queue_ms, elapsed, decoded) = match decode {
        Ok(Ok((queue_ms, elapsed, Some(decoded)))) => (queue_ms, elapsed, decoded),
        Ok(Err(error)) => {
            if let Some(response) = errors::scratch_reply(&error) { return response; }
            tracing::warn!(%error, "WMTS source decode failed");
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"code":"source_decode_failed","message":format!("{error:#}"),"details":{"assetId":asset_id.0,"driver":driver.as_str(),"matrix":level,"column":col,"row":row}}))).into_response();
        }
        other => {
            tracing::warn!(?other, "WMTS decode worker failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"code":"decode_worker_failed"}))).into_response();
        }
    };
    let trace_id = current_trace_id();
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
    headers.insert(
        "x-river-decode-ms",
        HeaderValue::from_str(&format!("{:.3}", decoded.raster.decode_ms)).unwrap(),
    );
    headers.insert(
        "x-river-encode-ms",
        HeaderValue::from_str(&format!("{:.3}", decoded.raster.encode_ms)).unwrap(),
    );
    headers.insert(
        "x-river-server-ms",
        HeaderValue::from_str(&format!("{elapsed:.3}")).unwrap(),
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
    response
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
        .with(tracing_subscriber::fmt::layer().event_format(console_log::Console { verbose: logging.verbose, pretty: logging.pretty_print }).with_ansi(false))
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
        let response = load_s3_asset(State(state.clone()), Path(source_id), Json(S3AssetInput {
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

async fn resolved_asset(state: &AppState, id: &AssetId) -> anyhow::Result<Option<RegisteredAsset>> {
    if !state.assets.read().await.contains_key(id) {
        if let Some((source,key))=remote_asset_key(&id.0).or_else(||asset_registry::lookup_locator(&id.0)) {
            let response=load_s3_asset(State(state.clone()),Path(source),Json(S3AssetInput{key,width:None,height:None,bit_depth:None})).await;
            anyhow::ensure!(response.status().is_success(),"Cannot reload evicted image metadata: {}",response.status());
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

fn page_limit(limit: Option<usize>) -> usize { limit.unwrap_or(50).clamp(1, 100) }

fn page_ids<'a>(ids: impl Iterator<Item = &'a str>, cursor: Option<&str>, limit: usize) -> (Vec<&'a str>, bool) {
    let mut page = std::collections::BinaryHeap::with_capacity(limit + 1);
    for id in ids.filter(|id| cursor.is_none_or(|cursor| *id > cursor)) {
        if page.len() < limit + 1 { page.push(id); }
        else if page.peek().is_some_and(|largest| id < *largest) { page.pop(); page.push(id); }
    }
    let mut page = page.into_sorted_vec();
    let more = page.len() > limit;
    page.truncate(limit);
    (page, more)
}

#[derive(serde::Deserialize)]
struct AssetListQuery {
    #[serde(rename = "refresh")]
    _refresh: Option<bool>,
    source: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    #[serde(rename = "keyPrefix")]
    key_prefix: Option<String>,
    q: Option<String>,
}

async fn list_assets(State(state): State<AppState>, Query(query): Query<AssetListQuery>) -> Response {
    // Browsing never crawls or hashes the corpus, including legacy refresh requests.
    if query.source.is_some() {
        return catalog_search::search(state,query).await;
    }
    let assets = state.assets.read().await;
    let limit = page_limit(query.limit);
    // Local content/version IDs sort before remote locators/version IDs,
    // after s3-. Seek the registry instead of scanning remote entries.
    let (ids, more) = if query.cursor.as_deref().is_some_and(|cursor| cursor >= "s3-") { (vec![],false) }
    else {
        let lower = query.cursor.as_ref().map(|cursor| std::ops::Bound::Excluded(AssetId(cursor.clone())))
            .unwrap_or(std::ops::Bound::Unbounded);
        page_ids(assets.range((lower,std::ops::Bound::Excluded(AssetId("s3-".into()))))
            .filter(|(_, asset)| asset.source.as_ref().is_none_or(|source| source.remote.as_ref().is_none_or(|reader| reader.local_path.is_some())))
            .map(|(id, _)| id.0.as_str()).take(limit+1),None,limit)
    };
    let next = more.then(|| ids[limit - 1].to_owned());
    let manifests = ids.into_iter().map(|id| {
        let asset = &assets[&AssetId(id.to_owned())];
        let mut entry = serde_json::to_value(&asset.manifest).unwrap();
        entry["name"] = serde_json::Value::String(asset.source.as_ref().and_then(|source| source.remote.as_ref()).map(|remote| remote.key.clone()).unwrap_or_else(|| asset.source.as_ref()
            .and_then(|source| source.path.file_name())
            .map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| id.to_owned())));
        entry
    }).collect::<Vec<_>>();
    let mut response = Json(manifests).into_response();
    if let Some(next) = next {
        response.headers_mut().insert("x-river-next-cursor", HeaderValue::from_str(&next).unwrap());
    }
    response
}

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

#[derive(serde::Deserialize)]
struct S3ObjectQuery {
    prefix: Option<String>,
    #[serde(rename = "keyPrefix")]
    key_prefix: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn list_s3_objects(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    Query(query): Query<S3ObjectQuery>,
) -> Response {
    let Some(source) = state.s3_sources.read().await.get(&source_id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"code":"s3_source_not_found"})),
        )
            .into_response();
    };
    let suffix = query
        .prefix
        .unwrap_or_default()
        .trim_matches('/')
        .to_owned();
    let prefix = [source.prefix.as_str(), suffix.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
    let key_prefix = query.key_prefix.unwrap_or_default();
    let prefix = format!("{prefix}{}", key_prefix.trim_start_matches('/'));
    let prefix = (!prefix.is_empty()).then_some(prefix);
    let search = query.q.unwrap_or_default().trim().to_lowercase();
    // Exactly one provider request. Filtering does not scan extra pages.
    let page = match source.store.list_paginated(prefix.as_deref(), PaginatedListOptions {
        max_keys: Some(page_limit(query.limit)), page_token: query.cursor, ..Default::default()
    }).await {
        Ok(page) => page,
        Err(error) => return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"code":"s3_list_failed","message":format!("{error:#}")}))).into_response(),
    };
    let scanned = page.result.objects.len();
    let objects = page.result.objects.iter().filter(|meta| is_jpeg2000_key(meta.location.as_ref())
        && (search.is_empty() || meta.location.as_ref().to_lowercase().contains(&search))).map(|meta| serde_json::json!({
        "key":meta.location.as_ref(), "name":meta.location.as_ref(),
        "assetId":remote_asset_id(&source.id, meta.location.as_ref()),
        "size":meta.size, "lastModified":meta.last_modified.to_string()
    })).collect::<Vec<_>>();
    Json(serde_json::json!({"sourceId":source.id,"objects":objects,
        "scannedObjects":scanned,"truncated":page.page_token.is_some(),"nextCursor":page.page_token})).into_response()
}

fn is_jpeg2000_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [".jp2", ".j2k", ".j2c", ".jpc", ".jph", ".jhc"].iter().any(|extension| key.ends_with(extension))
}

#[instrument(name = "river.http.s3.asset.load", skip(state, input), fields(source_id = %source_id, key = %input.key))]
async fn load_s3_asset(
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
        Ok(Err(error)) => return errors::reply(StatusCode::UNPROCESSABLE_ENTITY,"s3_header_failed",format!("Cannot read JPEG 2000 header: {error:#}"),remote_source::header_error_details(&error)),
        Err(error) => return errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"s3_header_worker_failed",format!("The image header worker stopped: {error}"),serde_json::json!({})),
    };
    let header = &remote.header;
    if input.width.is_some_and(|w| w != header.width) || input.height.is_some_and(|h| h != header.height)
        || input.bit_depth.is_some_and(|d| d != header.bit_depth) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"code":"metadata_mismatch"}))).into_response();
    }
    let mut source_file = Jpeg2000Source::from_catalog(PathBuf::from(key), header.width, header.height, Some(header.layout));
    source_file.reference_origin = header.image_origin;
    source_file.components=Some(header.components);source_file.display_rgb_only=header.display_rgb_only;
    source_file.remote = Some(remote.clone());
    let asset = RegisteredAsset {
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
        preview_source: None,
        preview_scale: 1,
        gain_source: None,
        gain_scale: 1,
    };
    let locator=(asset_id.clone(),source.id.clone(),key.to_owned());
    tokio::task::spawn_blocking(move||{if let Err(error)=asset_registry::remember_locator(&locator.0,&locator.1,&locator.2){tracing::debug!(%error,"asset locator persistence unavailable; original URLs remain demand-loadable");}}).await.ok();
    state
        .assets
        .write()
        .await
        .insert(AssetId(asset_id.clone()), asset.clone());
    state.assets.write().await.insert(AssetId(remote_id), asset);
    Json(serde_json::json!({"assetId":asset_id,"width":header.width,"height":header.height,"bytes":size,"bytesFetched":stats.bytes,"rangeRequests":stats.requests,"serverRegistrationMs":registration_started.elapsed().as_secs_f64()*1000.0,"headerProbeMs":probe_ms,"identityKind":"s3-version","loadedFrom":{"sourceId":source.id,"bucket":source.bucket,"key":key},"cache":"bounded compressed ranges; no full-file download"})).into_response()
}

#[instrument(name = "river.catalog.start_background_discovery", skip(state))]
fn start_background_mosaic_discovery(state: &AppState) {
    if state
        .mosaic_discovery_in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = register_new_mosaics(&state).await {
            tracing::warn!(%error, "unable to discover date mosaics");
        }
        state
            .mosaic_discovery_in_flight
            .store(false, Ordering::Release);
    });
}

#[instrument(name = "river.catalog.discover", skip(state))]
async fn register_new_mosaics(state: &AppState) -> anyhow::Result<()> {
    let known_paths = state
        .assets
        .read()
        .await
        .values()
        .filter_map(|asset| asset.source.as_ref().map(|source| source.path.clone()))
        .collect::<HashSet<_>>();
    let mosaic_root = state.mosaic_root.clone();
    let delivery_pyramids_enabled = state.delivery_pyramids_enabled;
    let additions = tokio::task::spawn_blocking(move || {
        discover_mosaics(&mosaic_root, &known_paths, delivery_pyramids_enabled)
    })
    .await
    .context("mosaic discovery worker stopped")??;
    if additions.is_empty() {
        return Ok(());
    }
    let mut assets = state.assets.write().await;
    for asset in additions {
        assets.insert(asset.manifest.asset_id.clone(), asset);
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct SocketQuery {#[serde(default,alias="EDGE")] edge:String}

#[instrument(name = "river.ws.upgrade", skip(ws, state, query))]
async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>,Query(query):Query<SocketQuery>) -> Response {
    let mode=match edges::Mode::parse(&query.edge) {Ok(mode)=>mode,Err(error)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_edge_mode",error.to_string(),serde_json::json!({"allowed":["crop","black","transparent"]}))};
    ws.on_upgrade(move |socket| handle_socket(socket, state,mode))
}

#[instrument(name = "river.ws.session", skip(socket, state))]
async fn handle_socket(socket: WebSocket, state: AppState,edge_mode:edges::Mode) {
    let session_id = Uuid::new_v4();
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));
    let mut active_request: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(Ok(message)) = receiver.next().await {
        if matches!(message, Message::Binary(_)) {
            let _ = send_control(&sender,ServerMessage::Error {sequence:None,code:"unsupported_message_type".to_owned(),message:"Client requests must be JSON text frames (viewport or tiles). Binary frames are server image responses only.".to_owned()}).await;
            continue;
        }
        if let Message::Text(text) = message {
            let parsed = serde_json::from_str::<ClientMessage>(&text);
            match parsed {
                Ok(request) => {
                    if let Some(task) = active_request.take() {
                        task.abort();
                    }
                    let span = info_span!("river.request", session_id = %session_id, sequence = request.sequence(), asset_id = %request.asset_id().0);
                    let sender = sender.clone();
                    let state = state.clone();
                    active_request = Some(tokio::spawn(async move {
                        let sequence = request.sequence();
                        if let Err(error) = serve_request(&sender, &state, session_id, request,edge_mode)
                            .instrument(span)
                            .await
                        {
                            tracing::warn!(%error, "tile request failed");
                            let _ = send_control(
                                &sender,
                                ServerMessage::Error {
                                    sequence: Some(sequence),
                                    code: "decode_failed".to_owned(),
                                    message: format!("{error:#}"),
                                },
                            )
                            .await;
                        }
                    }));
                }
                Err(error) => {
                    let _ = send_control(
                        &sender,
                        ServerMessage::Error {
                            sequence: None,
                            code: "invalid_message".to_owned(),
                            message: format!("{error:#}"),
                        },
                    )
                    .await;
                }
            }
        }
    }
    if let Some(task) = active_request {
        task.abort();
    }
}

#[instrument(
    name = "river.request.serve",
    skip(socket, state, request),
    fields(river.asset_id = %request.asset_id().0, river.sequence = request.sequence(), river.output_format = ?request.raster_format(), river.decoder_driver = %request.decoder_driver().as_str())
)]
async fn serve_request(
    socket: &SocketSender,
    state: &AppState,
    session_id: Uuid,
    request: ClientMessage,
    edge_mode:edges::Mode,
) -> anyhow::Result<()> {
    let (_cancel_guard, cancellation) = remote_source::CancelOnDrop::new();
    let viewport_started = Instant::now();
    let sequence = request.sequence();
    let raster_format = request.raster_format();
    let driver = request.decoder_driver();
    let Some(engine) = state.tile_engines.get(driver) else {
        send_control(
            socket,
            ServerMessage::Error {
                sequence: Some(sequence),
                code: "driver_unavailable".to_owned(),
                message: format!(
                    "{} decoder is not available in this River runtime",
                    driver.display_name()
                ),
            },
        )
        .await?;
        return Ok(());
    };
    let asset = resolved_asset(&state,request.asset_id()).await?;
    let Some(asset) = asset else {
        send_control(
            socket,
            ServerMessage::Error {
                sequence: Some(sequence),
                code: "asset_not_found".to_owned(),
                message: format!("Image {} is unavailable. Select an image from /api/assets or use a connected S3 locator.",request.asset_id().0),
            },
        )
        .await?;
        return Ok(());
    };
    let groups = match requests::plan(&request, &asset.manifest) {
        Ok(groups) => groups,
        Err(error) => {
            send_control(
                socket,
                ServerMessage::Error {
                    sequence: Some(sequence),
                    code: match request {
                        ClientMessage::Viewport { .. } => "invalid_viewport",
                        ClientMessage::Tiles { .. } => "invalid_tiles",
                    }
                    .to_owned(),
                    message: format!("{error:#}. Image {}: {}x{} pixels, {} resolution levels; WebSocket z=0 is full resolution, maximum z={}. Requested: {:?}",request.asset_id().0,asset.manifest.width,asset.manifest.height,asset.manifest.resolution_levels,asset.manifest.resolution_levels.saturating_sub(1),request),
                },
            )
            .await?;
            return Ok(());
        }
    };

    let trace_id = current_trace_id();
    send_control(
        socket,
        ServerMessage::Accepted {
            session_id,
            trace_id: trace_id.clone(),
            sequence,
        },
    )
    .await?;
    if let Capability::Limited { reasons } = &asset.manifest.capability {
        send_control(
            socket,
            ServerMessage::Limited {
                sequence,
                reasons: reasons.clone(),
            },
        )
        .await?;
    }

    let Some(source) = asset.source.clone() else {
        return Ok(());
    };
    let mut pass_plan = if let Some(remote) = source.remote.as_ref() {
        let mut passes = vec![PlannedPass { kind: river_core::PassKind::Preview, source_layers: 1, source_bytes: 0 }];
        if remote.header.layers > 2 {
            passes.push(PlannedPass { kind: river_core::PassKind::Gain, source_layers: usize::from(remote.header.layers.div_ceil(2)), source_bytes: 0 });
        }
        passes.push(PlannedPass { kind: river_core::PassKind::Final, source_layers: usize::from(remote.header.layers), source_bytes: remote.meta.size });
        passes
    } else { plan_passes(&asset.manifest.quality) };
    // A one-quality-layer JP2 still benefits from a fast coarse-resolution
    // first paint. Its final pass remains the exact requested reduction.
    if !matches!(pass_plan.first(), Some(pass) if pass.kind == river_core::PassKind::Preview) {
        if let Some(first) = pass_plan.first() {
            pass_plan.insert(
                0,
                PlannedPass {
                    kind: river_core::PassKind::Preview,
                    source_layers: first.source_layers,
                    source_bytes: first.source_bytes,
                },
            );
        }
    }
    if asset.gain_source.is_some()
        && !pass_plan
            .iter()
            .any(|pass| pass.kind == river_core::PassKind::Gain)
    {
        if let Some(final_index) = pass_plan
            .iter()
            .position(|pass| pass.kind == river_core::PassKind::Final)
        {
            let final_pass = pass_plan[final_index].clone();
            pass_plan.insert(
                final_index,
                PlannedPass {
                    kind: river_core::PassKind::Gain,
                    source_layers: final_pass.source_layers,
                    source_bytes: final_pass.source_bytes,
                },
            );
        }
    }
    if driver == DecoderDriver::NvJpeg2000 && source.remote.is_none() {
        // nvJPEG2000 returns full-quality region tiles instead of stopping at
        // individual JPEG 2000 quality layers. Still stream the spatial tiles
        // progressively, but decode each tile only once.
        pass_plan.retain(|pass| pass.kind == river_core::PassKind::Final);
    }
    // A complete image fitting one output tile already gets a coarse first
    // paint. Decode its exact final next instead of blocking it behind another
    // full-grid quality decode. Large/multi-tile and local pyramid plans keep
    // their intermediate gain pass. Explicit opt-in retains the old schedule.
    if source.remote.is_some() && groups.len()==1 && groups[0].tiles.len()==1
        && requests::covers_image(&groups[0],&asset.manifest)
        && !std::env::var("RIVER_PROGRESSIVE_SMALL_PREVIEW_GAIN").is_ok_and(|v|v=="true" || v=="1") {
        pass_plan.retain(|pass|pass.kind!=river_core::PassKind::Gain);
    }
    // User idleness belongs to the viewport, not each quality pass. Once an
    // uncached refinement starts, later passes need no additional pause.
    // Metadata, preview IO and decode already count toward input idleness;
    // only the remaining quiet period may delay refinement.
    let refinement_gate = Arc::new(tokio::sync::OnceCell::new());
    for planned in pass_plan {
        let defer_refinement = planned.kind != river_core::PassKind::Preview;
        for group in &groups {
            let zoom = group.zoom;
            let tiles = &group.tiles;
            let group_whole_view=requests::covers_image(group,&asset.manifest);
            let mut source_tile = TILE_SIZE
                .checked_shl(u32::from(zoom))
                .ok_or_else(|| anyhow::anyhow!("unsupported tile zoom"))?;
            let decode_zoom = if planned.kind == river_core::PassKind::Preview {
                zoom.saturating_add(state.preview_extra_reductions)
                    .min(asset.manifest.resolution_levels.saturating_sub(1))
            } else {
                zoom
            };
            let display_scale = 1u32 << (decode_zoom - zoom);
            let (decoder_source, mut decoder_tile, decoder_reduce, delivery_scale) = match planned.kind
            {
                river_core::PassKind::Preview => asset
                    .preview_source
                    .clone()
                    .map(|preview| {
                        let scale_shift = asset.preview_scale.ilog2() as u8;
                        (
                            preview,
                            source_tile / asset.preview_scale,
                            decode_zoom.saturating_sub(scale_shift),
                            Some(asset.preview_scale),
                        )
                    })
                    .unwrap_or_else(|| (source.clone(), source_tile, decode_zoom, None)),
                river_core::PassKind::Gain => asset
                    .gain_source
                    .clone()
                    .map(|gain| {
                        let scale_shift = asset.gain_scale.ilog2() as u8;
                        (
                            gain,
                            source_tile / asset.gain_scale,
                            zoom.saturating_sub(scale_shift),
                            Some(asset.gain_scale),
                        )
                    })
                    .unwrap_or_else(|| (source.clone(), source_tile, decode_zoom, None)),
                river_core::PassKind::Final => (source.clone(), source_tile, decode_zoom, None),
            };
            let source_layers = planned.source_layers as u32;
            let available_layers = source.remote.as_ref().map(|remote| u32::from(remote.header.layers))
                .unwrap_or(asset.manifest.quality.cumulative_layer_bytes.len() as u32);
            let decode_layers = if decoder_source.path == source.path && source_layers >= available_layers { 0 } else { source_layers };
            let webp_method=encoding_settings::snapshot();
            let canonical_request=TileDecodeRequest {
        progressive_final_reduction:None, output_geometry:None,
                edge_mode,
                source_id:asset.manifest.asset_id.0.clone(),webp_method,capture_pixels:false,whole_view:group_whole_view,source:decoder_source.clone(),
                tiles:group.tiles.clone(),source_tile:decoder_tile,reduction:decoder_reduce,layers:decode_layers,
                format:raster_format,decoder_threads:state.openjpeg_threads_per_decode,max_native_tiles:state.max_native_tiles_per_job,
                cached_tiles:vec![],work_ticket:None,cancellation:Some(cancellation.clone()),
            };
            // Opt-in fine preview patches paint while the unavoidable SOT chain
            // is still being discovered. Final tiles keep the original grid.
            let patch_candidate = std::env::var("RIVER_PREVIEW_PATCH_EDGE").ok().and_then(|v|v.parse::<u32>().ok())
                .filter(|v|matches!(v,1024|2048|4096)).unwrap_or_else(||source.remote.as_ref().map_or(2048,|remote|
                    preview_plan::preferred_edge(asset.manifest.width,asset.manifest.height,
                        remote.header.layout.width,remote.header.layout.height,remote.header.image_origin,remote.minimum_range_latency_us())));
            let patch_edge = if planned.kind == river_core::PassKind::Preview && delivery_scale.is_none() && decode_zoom >= 4
                && source.remote.as_ref().is_some_and(|remote| remote.meta.size>=64*1024*1024 && remote.sequential_prefix
                    && (driver != DecoderDriver::NvJpeg2000 || (remote.header.image_origin==(0,0)
                        && remote.header.layout.width.is_power_of_two() && remote.header.layout.height.is_power_of_two()
                        && remote.header.layout.width >= 1u32 << (remote.header.levels-1)
                        && remote.header.layout.height >= 1u32 << (remote.header.levels-1))))
                && matches!(&request,ClientMessage::Viewport {progressive_patches:true,viewport,..}
                    if viewport.x==0 && viewport.y==0 && viewport.width==asset.manifest.width && viewport.height==asset.manifest.height)
                && asset.manifest.width.div_ceil(patch_candidate) as u64 * asset.manifest.height.div_ceil(patch_candidate) as u64 <= 4096
                && engine.cached(&canonical_request).is_none() {
                Some(patch_candidate)
            } else {None};
            let patch_tiles;
            let tiles=if let Some(edge)=patch_edge {
                source_tile=edge;decoder_tile=edge;
                patch_tiles=(0..asset.manifest.height.div_ceil(edge)).flat_map(|y|
                    (0..asset.manifest.width.div_ceil(edge)).map(move|x|(x,y))).collect::<Vec<_>>();
                &patch_tiles
            } else {tiles};
            let tile_display_scale = if let Some(delivery_scale) = delivery_scale {
                (delivery_scale / (1u32 << zoom))
                    .max(1)
                    .saturating_mul(1u32 << decoder_reduce)
            } else {
                display_scale
            };
            // Decode 2×2 transport-tile unions. That amortizes OpenJPEG codec
            // setup while retaining four CPU-parallel jobs for an HD viewport.
            let mut batches =
                if patch_edge.is_some() {
                    let batch_size=std::env::var("RIVER_PREVIEW_PATCH_BATCH_TILES").ok()
                        .and_then(|value|value.parse::<usize>().ok()).unwrap_or(if patch_edge==Some(1024) {32} else {8});
                    preview_plan::batches(tiles,batch_size.min(if driver==DecoderDriver::NvJpeg2000 {16} else {32}))
                } else if delivery_scale.is_some() && matches!(request, ClientMessage::Viewport { .. }) {
                    vec![tiles.clone()]
                } else {
                    tile_batches(tiles)
                };
            let mut preview_pixels=patch_edge.filter(|_|edge_mode==edges::Mode::Crop).and_then(|edge|preview_pixels::Pixels::new(&canonical_request,edge));
            let capture_pixels=preview_pixels.is_some();
            let shared_prefix = driver == DecoderDriver::OpenJpeg
                || (driver == DecoderDriver::Jp2Grok
                    && !std::env::var("RIVER_GROK_PROGRESSIVE_PREFIX")
                        .is_ok_and(|value| value == "0" || value.eq_ignore_ascii_case("false")));
            let progressive_final_reduction = (planned.kind == river_core::PassKind::Preview
                && group_whole_view && shared_prefix).then_some(zoom);
            if patch_edge.is_none()
                && progressive_final_reduction.is_some()
                && decoder_source.remote.is_some()
                && std::env::var("RIVER_TRIAL_SINGLE_REMOTE_PREVIEW").as_deref() == Ok("1")
                && batches.first().is_some_and(|batch| batch.len() > 1)
            {
                let first_tile = batches[0].remove(0);
                batches.insert(0, vec![first_tile]);
            }
            let mut jobs = futures_util::stream::iter(batches.into_iter().map(|batch| {
                let slots = state.decode_slots.clone();
                let io_slots = state.source_io_slots.clone();
                let engine = engine.clone();
                let source = decoder_source.clone();
                let openjpeg_threads_per_decode = state.openjpeg_threads_per_decode;
                let max_native_tiles_per_job = state.max_native_tiles_per_job;
                let cancellation = cancellation.clone();
                let source_id = asset.manifest.asset_id.0.clone();
                let refinement_gate = refinement_gate.clone();
                let refine_idle_delay = state.refine_idle_delay;
                let whole_view = group_whole_view;
                async move {
                    let request = TileDecodeRequest {
        progressive_final_reduction, output_geometry:None,
                            edge_mode,
                            webp_method,
                            capture_pixels,
                            source_id,
                            whole_view,
                            source,
                            tiles: batch,
                            source_tile: decoder_tile,
                            reduction: decoder_reduce,
                            layers: decode_layers,
                            format: raster_format,
                            decoder_threads: openjpeg_threads_per_decode,
                            max_native_tiles: max_native_tiles_per_job,
                            cached_tiles: vec![], work_ticket:None,
                            cancellation: Some(cancellation),
                    };
                    let flight = match engine.flight(&request) {
                        Some(gate) => Some(gate.lock_owned().await),
                        None => None,
                    };
                    if let Some(tiles) = engine.cached(&request) { return Ok((0.0, tiles)); }
                    let (request, shared_checked) = if defer_refinement && crate::shared_cache::tiles_enabled()
                        && request.source.remote.is_some() && !request.capture_pixels
                    {
                        let probe = engine.clone();
                        let request = tokio::task::spawn_blocking(move || probe.probe_shared(request)).await
                            .context("shared tile cache worker stopped")?;
                        request.ensure_active()?;
                        if request.tiles.is_empty() {
                            if let Some(tiles) = engine.cached(&request) { return Ok((0.0, tiles)); }
                        }
                        (request, true)
                    } else { (request, false) };
                    if defer_refinement {
                        refinement_gate.get_or_init(|| async { tokio::time::sleep(refine_idle_delay.saturating_sub(viewport_started.elapsed())).await }).await;
                    }
                    let (request, io_permit, flight) = if request.source.remote.is_some() {
                        let source_gate=match request.source.remote.as_ref() {
                            Some(remote)=>remote.preparation_admission(&request.tiles,request.source_tile).await,None=>None,
                        };
                        let io_permit = io_slots.acquire_owned().await.map_err(|_| anyhow::anyhow!("source IO scheduler stopped"))?;
                        let preparer = engine.clone();
                        let span = tracing::Span::current();
                        let (request, io_permit, flight) = tokio::task::spawn_blocking(move || {
                            let _entered = span.enter();
                            let index_only=request.source.remote.as_ref().is_some_and(|remote|remote.serial_index && !remote.sequential_prefix);
                            let _source_admission=source_admission::Scope::enter(source_gate,index_only);
                            engine::prepare_admitted(preparer.as_ref(), request, io_permit, flight, shared_checked)
                        }).await
                            .context("source preparation worker stopped")??;
                        (request, Some(io_permit), flight)
                    } else { (request, None, flight) };
                    if let Some(tiles) = engine.cached(&request) { return Ok((0.0, tiles)); }
                    let cpu_queued_at = Instant::now();
                    let permit = slots
                        .acquire_owned()
                        .await
                        .map_err(|_| anyhow::anyhow!("JPEG 2000 decode scheduler stopped"))?;
                    drop(io_permit);
                    let decode_span = tracing::Span::current();
                    tokio::task::spawn_blocking(move || {
                        let _flight = flight;
                        let _entered = decode_span.enter();
                        let worker_queue_ms = elapsed_ms(cpu_queued_at);
                        let result = engine.decode(request);
                        drop(permit);
                        result.map(|decoded_tiles| (worker_queue_ms, decoded_tiles))
                    })
                    .await
                    .context("JPEG 2000 decode worker stopped")?
                }
            }));
            // Finish the first spatial batch before starting jobs which could
            // otherwise acquire the index lock ahead of the first useful pixels.
            let prioritize_remote_preview = progressive_final_reduction.is_some()
                && decoder_source.remote.is_some()
                && std::env::var("RIVER_TRIAL_FIRST_REMOTE_PREVIEW").as_deref() == Ok("1");
            let first=if patch_edge.is_some() || prioritize_remote_preview {jobs.next().await} else {None};
            let jobs=if let Some(first)=first {
                futures_util::stream::once(first).chain(jobs.buffer_unordered(state.per_viewport_decode_concurrency)).boxed()
            } else {jobs.buffer_unordered(state.per_viewport_decode_concurrency).boxed()};
            tokio::pin!(jobs);

            let mut completed=0usize;
            let mut preview_tail=None;
            while let Some(result) = jobs.next().await {
                let (worker_queue_ms, decoded_tiles) = result?;
                for decoded_tile in decoded_tiles {
                    if let Some(pixels)=&mut preview_pixels {pixels.add(&decoded_tile);}
                    let tile_x = decoded_tile.tile_x;
                    let tile_y = decoded_tile.tile_y;
                    let decoded = decoded_tile.raster;
                    let header = TileFrameHeader {
                        source_tile_edge: patch_edge, preview:patch_edge.map(|_|true), preview_patch_count:patch_edge.map(|_|tiles.len()),
                        session_id,
                        trace_id: trace_id.clone(),
                        asset_id: asset.manifest.asset_id.clone(),
                        sequence,
                        zoom,
                        tile_x,
                        tile_y,
                        pass: planned.kind,
                        source_layers: planned.source_layers,
                        source_bytes: planned.source_bytes,
                        // The coarse preview raster is stretched to the exact
                        // final tile extent, then replaced in-place by refinement.
                        output_width: decoded.width.saturating_mul(tile_display_scale).min(
                            if edge_mode==edges::Mode::Crop {display_tile_extent(tile_x, source_tile, asset.manifest.width, zoom)} else {u32::MAX},
                        ),
                        output_height: decoded.height.saturating_mul(tile_display_scale).min(
                            if edge_mode==edges::Mode::Crop {display_tile_extent(tile_y, source_tile, asset.manifest.height, zoom)} else {u32::MAX},
                        ),
                        content_type: decoded.content_type.to_owned(),
                        server_timing: ServerTiming {
                            decoder_route: decoded.decoder_route.to_owned(),
                            decoder_backend: Some(decoded.decoder_backend.to_owned()),
                            worker_queue_ms,
                            decode_ms: decoded.decode_ms,
                            encode_ms: decoded.encode_ms,
                            source_cache_status: decoded.source_cache_status.to_owned(),
                            source_cache_ms: decoded.source_cache_ms,
                            source_read_bytes: decoded.source_read_bytes,
                            source_range_requests: decoded.source_range_requests,
                            source_cache_hit_bytes: decoded.source_cache_hit_bytes,
                            elapsed_ms: elapsed_ms(viewport_started),
                        },
                    };
                    completed+=1;
                    if patch_edge.is_some() && completed==tiles.len() {
                        preview_tail=Some((header,decoded.bytes));continue;
                    }
                    let frame = encode_tile_frame(&header, &decoded.bytes)?;
                    socket
                        .lock()
                        .await
                        .send(Message::Binary(frame.into()))
                        .await?;
                }
            }
            if let Some((mut header,bytes))=preview_tail {
                // Prepare the compact response before announcing complete preview
                // coverage, preserving fast repeats without re-encoding lossy tiles.
                if engine.cached(&canonical_request).is_none() {
                    let preview_pixels=preview_pixels.filter(|pixels|pixels.complete());
                    let io=if preview_pixels.is_some() {None} else {Some(state.source_io_slots.clone().acquire_owned().await?)};
                    let cpu=state.decode_slots.clone().acquire_owned().await?;
                    let engine=engine.clone();
                    let result=tokio::task::spawn_blocking(move|| {
                        let _io=io;let _cpu=cpu;
                        if let Some(pixels)=preview_pixels {
                            pixels.encode(&canonical_request).map(|tiles| {tracing::debug!(tile_count=tiles.len(),"compact preview reused normalized patch pixels");engine.cache_encoded(&canonical_request,&tiles);tiles})
                        } else {engine.prepare(canonical_request).and_then(|request|engine.decode(request))}
                    }).await;
                    if !matches!(result,Ok(Ok(_))) {tracing::debug!("compact preview cache fill skipped");}
                }
                header.server_timing.elapsed_ms=elapsed_ms(viewport_started);
                let frame=encode_tile_frame(&header,&bytes)?;
                socket.lock().await.send(Message::Binary(frame.into())).await?;
            }
        }
    }
    Ok(())
}

fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1_000.0
}

/// The browser receives the same trace identity as the nested Rust spans,
/// making a frame traceable from WebSocket acceptance through worker queue,
/// codec decode, and raster encoding in Jaeger/OTLP.
fn current_trace_id() -> String {
    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
        .to_string()
}

fn display_tile_extent(tile: u32, source_tile: u32, image_extent: u32, zoom: u8) -> u32 {
    let start = tile.saturating_mul(source_tile);
    image_extent
        .saturating_sub(start)
        .min(source_tile)
        .div_ceil(1u32 << zoom)
}

fn decode_concurrency_from_env() -> usize { resources::plan().decode_concurrency }

async fn send_control(socket: &SocketSender, message: ServerMessage) -> anyhow::Result<()> {
    socket
        .lock()
        .await
        .send(Message::Text(serde_json::to_string(&message)?.into()))
        .await?;
    Ok(())
}

fn tiles_for(viewport: &Viewport) -> Vec<(u32, u32)> {
    let scale = 1u32.checked_shl(viewport.zoom as u32).unwrap_or(u32::MAX);
    let source_tile = TILE_SIZE.saturating_mul(scale);
    let start_x = viewport.x / source_tile;
    let start_y = viewport.y / source_tile;
    let end_x = (viewport.x + viewport.width - 1) / source_tile;
    let end_y = (viewport.y + viewport.height - 1) / source_tile;
    let mut tiles = Vec::new();
    for tile_y in start_y..=end_y {
        for tile_x in start_x..=end_x {
            tiles.push((tile_x, tile_y));
        }
    }
    tiles
}

fn tile_batches(tiles: &[(u32, u32)]) -> Vec<Vec<(u32, u32)>> {
    let start_x = tiles.iter().map(|tile| tile.0).min().unwrap_or(0);
    let start_y = tiles.iter().map(|tile| tile.1).min().unwrap_or(0);
    let mut batches = BTreeMap::<(u32, u32), Vec<(u32, u32)>>::new();
    for &(tile_x, tile_y) in tiles {
        batches
            .entry(((tile_x - start_x) / 2, (tile_y - start_y) / 2))
            .or_default()
            .push((tile_x, tile_y));
    }
    batches.into_values().collect()
}

fn discover_mosaics(
    mosaic_root: &FsPath,
    known_paths: &HashSet<PathBuf>,
    delivery_pyramids_enabled: bool,
) -> anyhow::Result<Vec<RegisteredAsset>> {
    if !mosaic_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut assets = Vec::new();
    for entry in fs::read_dir(mosaic_root)
        .with_context(|| format!("unable to read mosaic root {}", mosaic_root.display()))?
    {
        let path = entry?.path();
        let is_completed_jp2 = path.extension().is_some_and(|extension| extension == "jp2")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".partial.jp2"));
        if !is_completed_jp2 || known_paths.contains(&path) {
            continue;
        }
        let info_path = PathBuf::from(format!("{}.info.json", path.display()));
        let info = fs::read_to_string(&info_path)
            .with_context(|| format!("missing mosaic sidecar {}", info_path.display()))?;
        let info_json = serde_json::from_str::<serde_json::Value>(&info)?;
        let size = info_json
            .get("size")
            .and_then(serde_json::Value::as_array)
            .filter(|size| size.len() == 2)
            .ok_or_else(|| anyhow::anyhow!("invalid GDAL size in {}", info_path.display()))?;
        let width = size[0]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow::anyhow!("invalid mosaic width in {}", info_path.display()))?;
        let height = size[1]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow::anyhow!("invalid mosaic height in {}", info_path.display()))?;
        let source = Jpeg2000Source::from_path(path.clone(), width, height);
        let preview_path = mosaic_root
            .join("river-preview-jp2")
            .join(path.file_name().unwrap());
        let preview_source = (delivery_pyramids_enabled && preview_path.is_file()).then(|| {
            Jpeg2000Source::from_path(preview_path, (width / 16).max(1), (height / 16).max(1))
        });
        assets.push(RegisteredAsset {
            local_header: Arc::new(tokio::sync::OnceCell::new()), local_reader: Arc::new(tokio::sync::OnceCell::new()),
            manifest: AssetManifest {
                asset_id: AssetId(if local_identity_mode()? == "sha256" {
                    sha256_file(&path)?
                } else { file_version_id(&path)? }),
                width,
                height,
                components: 3,
                bit_depth: 16,
                resolution_levels: 7,
                // The generated mosaics are lossless single-layer JP2 files.
                quality: QualityModel {
                    cumulative_layer_bytes: vec![fs::metadata(&path)?.len()],
                },
                color: ColorPolicy {
                    output: "sRGB/RGBA 8-bit WebP".to_owned(),
                    source_profile: SourceColorProfile::Embedded,
                    assumption_recorded: true,
                },
                capability: Capability::FastPath,
            },
            source: Some(source),
            preview_source,
            preview_scale: 16,
            gain_source: None,
            gain_scale: 1,
        });
    }
    Ok(assets)
}

fn sha256_file(path: &FsPath) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("unable to hash mosaic {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn file_version_id(path: &FsPath) -> anyhow::Result<String> {
    let absolute = fs::canonicalize(path)?;
    let metadata = fs::metadata(&absolute)?;
    let identity = serde_json::to_vec(&(&absolute,metadata.len(),metadata.modified()?))?;
    Ok(format!("filev-{}",hex::encode(Sha256::digest(identity))))
}

fn local_identity_mode() -> anyhow::Result<String> {
    let mode = match std::env::var("RIVER_LOCAL_IDENTITY") {
        Ok(mode) => mode,
        Err(std::env::VarError::NotPresent) => "version".into(),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(matches!(mode.as_str(),"version"|"sha256"), "RIVER_LOCAL_IDENTITY must be version or sha256");
    Ok(mode)
}

#[instrument(
    name = "river.catalog.load",
    skip(path, root),
    fields(delivery_pyramids_enabled)
)]
fn load_catalog(
    path: &FsPath,
    root: &FsPath,
    delivery_pyramids_enabled: bool,
) -> anyhow::Result<Vec<RegisteredAsset>> {
    let catalog = serde_json::from_slice::<AssetCatalog>(
        &fs::read(path)
            .with_context(|| format!("unable to read asset catalog {}", path.display()))?,
    )?;
    catalog
        .assets
        .into_iter()
        .map(|entry| {
            let source_path = root.join(&entry.source);
            let native_tile_layout = match (entry.native_tile_width, entry.native_tile_height) {
                (Some(width), Some(height)) if width > 0 && height > 0 => {
                    Some(NativeTileLayoutHint {
                        origin_x: 0,
                        origin_y: 0,
                        width,
                        height,
                        columns: entry.width.div_ceil(width),
                        rows: entry.height.div_ceil(height),
                    })
                }
                _ => None,
            };
            let source = Jpeg2000Source::from_catalog(
                source_path,
                entry.width,
                entry.height,
                native_tile_layout,
            );
            let thumbnail = root.join("river-thumbnail-jp2").join(&entry.source);
            let configured_preview = entry.preview_source.map(|preview| root.join(preview));
            let configured_preview_scale = entry.preview_scale.unwrap_or(1);
            let configured_gain = root.join("river-gain-jp2").join(&entry.source);
            let gain_available = delivery_pyramids_enabled && configured_gain.is_file();
            let (preview_path, preview_scale, gain_path, gain_scale) =
                if delivery_pyramids_enabled && thumbnail.is_file() {
                    (
                        Some(thumbnail),
                        64,
                        if gain_available {
                            Some(configured_gain)
                        } else {
                            configured_preview
                        },
                        if gain_available {
                            32
                        } else {
                            configured_preview_scale
                        },
                    )
                } else if delivery_pyramids_enabled {
                    (configured_preview, configured_preview_scale, None, 1)
                } else {
                    (None, 1, None, 1)
                };
            let preview_source = preview_path.map(|preview| {
                Jpeg2000Source::from_path(
                    preview,
                    (entry.width / preview_scale).max(1),
                    (entry.height / preview_scale).max(1),
                )
            });
            let gain_source = gain_path.filter(|gain| gain.is_file()).map(|gain| {
                Jpeg2000Source::from_path(
                    gain,
                    (entry.width / gain_scale).max(1),
                    (entry.height / gain_scale).max(1),
                )
            });
            Ok(RegisteredAsset {
                local_header: Arc::new(tokio::sync::OnceCell::new()), local_reader: Arc::new(tokio::sync::OnceCell::new()),
                manifest: AssetManifest {
                    asset_id: AssetId(entry.asset_id),
                    width: entry.width,
                    height: entry.height,
                    components: 3,
                    bit_depth: entry.bit_depth,
                    resolution_levels: 7,
                    quality: QualityModel {
                        cumulative_layer_bytes: vec![fs::metadata(&source.path)?.len()],
                    },
                    color: ColorPolicy {
                        output: "sRGB/RGBA 8-bit WebP".to_owned(),
                        source_profile: SourceColorProfile::Embedded,
                        assumption_recorded: true,
                    },
                    capability: Capability::FastPath,
                },
                source: Some(source),
                preview_source,
                preview_scale,
                gain_source,
                gain_scale,
            })
        })
        .collect()
}

fn default_asset() -> RegisteredAsset {
    let corpus_root = std::env::var_os("RIVER_CORPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/cache"));
    let source = Jpeg2000Source::from_path(sample_path(&corpus_root), 2_592, 1_944);
    let source_available = source.is_available();
    RegisteredAsset {
        local_header: Arc::new(tokio::sync::OnceCell::new()), local_reader: Arc::new(tokio::sync::OnceCell::new()),
        manifest: AssetManifest {
            asset_id: AssetId(
                "94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290".to_owned(),
            ),
            width: 2_592,
            height: 1_944,
            components: 3,
            bit_depth: 8,
            resolution_levels: 6,
            quality: QualityModel {
                cumulative_layer_bytes: vec![1_795_544, 3_591_088, 5_386_631],
            },
            color: ColorPolicy {
                output: "sRGB/RGBA 8-bit WebP".to_owned(),
                source_profile: SourceColorProfile::AssumedSrgb,
                assumption_recorded: true,
            },
            capability: if source_available {
                Capability::FastPath
            } else {
                Capability::Limited {
                    reasons: vec![format!(
                        "OpenJPEG decoder is linked, but the sample codestream is not mounted at {}",
                        source.path.display()
                    )],
                }
            },
        },
        source: source_available.then_some(source),
        preview_source: None,
        preview_scale: 1,
        gain_source: None,
        gain_scale: 1,
    }
}

#[cfg(test)]
mod browsing_tests {
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
    fn bounded_page_selection_matches_full_sort_with_cursors() {
        let ids: Vec<_> = (0..10_000).map(|i| format!("{:05}",(i*37)%10_000)).collect();
        for limit in [1,50,100] {
            for cursor in [None,Some("05200"),Some("09998"),Some("05000.1")] {
                let (page, more) = super::page_ids(ids.iter().map(String::as_str),cursor,limit);
                let mut expected: Vec<_> = ids.iter().map(String::as_str).filter(|id| cursor.is_none_or(|c| *id > c)).collect();
                expected.sort_unstable();
                assert_eq!(more,expected.len()>limit);
                expected.truncate(limit);
                assert_eq!(page,expected);
            }
        }
    }
    use super::*;
    #[test]
    fn limits_are_small_even_for_oversized_requests() {
        assert_eq!(page_limit(None), 50);
        assert_eq!(page_limit(Some(5000)), 100);
        assert_eq!(page_limit(Some(0)), 1);
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
