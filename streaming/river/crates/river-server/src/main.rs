use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use opentelemetry::{
    KeyValue, global,
    trace::{Span as _, Tracer as _, TracerProvider as _},
};
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::trace::SdkTracerProvider;
use river_core::{
    AssetId, AssetManifest, Capability, ClientMessage, ColorPolicy, QualityModel, ServerMessage,
    SourceColorProfile, TILE_SIZE, TileFrameHeader, Viewport, encode_tile_frame, plan_passes,
};
use tokio::sync::{Mutex, RwLock};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};
use tracing::{Instrument, info_span};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

mod decoder;

use decoder::{Jpeg2000Source, sample_path};

#[derive(Clone)]
struct AppState {
    assets: Arc<RwLock<HashMap<AssetId, RegisteredAsset>>>,
}

#[derive(Clone)]
struct RegisteredAsset {
    manifest: AssetManifest,
    source: Option<Jpeg2000Source>,
}

type SocketSender = Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tracer_provider = init_tracing()?;
    let default_asset = default_asset();
    let state = AppState {
        assets: Arc::new(RwLock::new(HashMap::from([(
            default_asset.manifest.asset_id.clone(),
            default_asset,
        )]))),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/assets/{asset_id}/manifest", get(manifest))
        .route("/ws", get(websocket))
        .fallback_service(ServeDir::new("web").append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http())
        // The static viewer is deliberately served from :8765 in Compose;
        // limit browser API access to that loopback origin.
        .layer(
            CorsLayer::new()
                .allow_origin(HeaderValue::from_static("http://127.0.0.1:8765"))
                .allow_methods([Method::GET]),
        )
        .with_state(state);

    let address = std::env::var("RIVER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse::<SocketAddr>()
        .context("RIVER_BIND must be a socket address")?;
    tracing::info!(%address, "River listening");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app).await?;
    tracer_provider.shutdown()?;
    Ok(())
}

fn init_tracing() -> anyhow::Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder().with_tonic().build()?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();
    let tracer = provider.tracer("river-server");
    global::set_tracer_provider(provider.clone());
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "river_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(OpenTelemetryLayer::new(tracer))
        .init();
    Ok(provider)
}

async fn health() -> &'static str {
    "ok"
}

async fn manifest(State(state): State<AppState>, Path(asset_id): Path<String>) -> Response {
    let asset_id = AssetId(asset_id);
    match state.assets.read().await.get(&asset_id) {
        Some(asset) => Json(&asset.manifest).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"code": "asset_not_found"})),
        )
            .into_response(),
    }
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let session_id = Uuid::new_v4();
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));
    let mut active_request: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(Ok(message)) = receiver.next().await {
        if let Message::Text(text) = message {
            let parsed = serde_json::from_str::<ClientMessage>(&text);
            match parsed {
                Ok(ClientMessage::Viewport { viewport }) => {
                    if let Some(task) = active_request.take() {
                        task.abort();
                    }
                    let span = info_span!("river.viewport", session_id = %session_id, sequence = viewport.sequence, asset_id = %viewport.asset_id.0, zoom = viewport.zoom);
                    let sender = sender.clone();
                    let state = state.clone();
                    active_request = Some(tokio::spawn(async move {
                        if let Err(error) = serve_viewport(&sender, &state, session_id, viewport)
                            .instrument(span)
                            .await
                        {
                            tracing::warn!(%error, "viewport request failed");
                        }
                    }));
                }
                Err(error) => {
                    let _ = send_control(
                        &sender,
                        ServerMessage::Error {
                            sequence: None,
                            code: "invalid_message".to_owned(),
                            message: error.to_string(),
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

async fn serve_viewport(
    socket: &SocketSender,
    state: &AppState,
    session_id: Uuid,
    viewport: Viewport,
) -> anyhow::Result<()> {
    let asset = state.assets.read().await.get(&viewport.asset_id).cloned();
    let Some(asset) = asset else {
        send_control(
            socket,
            ServerMessage::Error {
                sequence: Some(viewport.sequence),
                code: "asset_not_found".to_owned(),
                message: "register the SHA-256 asset before requesting it".to_owned(),
            },
        )
        .await?;
        return Ok(());
    };
    if let Err(error) = viewport.validate_against(&asset.manifest) {
        send_control(
            socket,
            ServerMessage::Error {
                sequence: Some(viewport.sequence),
                code: "invalid_viewport".to_owned(),
                message: error.to_string(),
            },
        )
        .await?;
        return Ok(());
    }

    let mut otel_span = global::tracer("river-server.viewport").start("river.viewport");
    otel_span.set_attribute(KeyValue::new(
        "river.asset_id",
        asset.manifest.asset_id.0.clone(),
    ));
    otel_span.set_attribute(KeyValue::new(
        "river.viewport.sequence",
        viewport.sequence as i64,
    ));
    let trace_id = otel_span.span_context().trace_id().to_string();
    send_control(
        socket,
        ServerMessage::Accepted {
            session_id,
            trace_id: trace_id.clone(),
            sequence: viewport.sequence,
        },
    )
    .await?;
    if let Capability::Limited { reasons } = &asset.manifest.capability {
        send_control(
            socket,
            ServerMessage::Limited {
                sequence: viewport.sequence,
                reasons: reasons.clone(),
            },
        )
        .await?;
    }

    let Some(source) = asset.source else {
        otel_span.end();
        return Ok(());
    };
    let pass_plan = plan_passes(&asset.manifest.quality);
    let tiles = tiles_for(&viewport);
    for planned in pass_plan {
        for (tile_x, tile_y) in &tiles {
            let source_tile = TILE_SIZE
                .checked_shl(viewport.zoom as u32)
                .ok_or_else(|| anyhow::anyhow!("unsupported viewport zoom"))?;
            let source_x = tile_x.saturating_mul(source_tile);
            let source_y = tile_y.saturating_mul(source_tile);
            let decoder = source.clone();
            let decoded = tokio::task::spawn_blocking(move || {
                decoder.decode_region(
                    source_x,
                    source_y,
                    source_tile,
                    source_tile,
                    viewport.zoom,
                    planned.source_layers as u32,
                )
            })
            .await
            .context("JPEG 2000 decode worker stopped")??;
            let header = TileFrameHeader {
                session_id,
                trace_id: trace_id.clone(),
                asset_id: asset.manifest.asset_id.clone(),
                sequence: viewport.sequence,
                zoom: viewport.zoom,
                tile_x: *tile_x,
                tile_y: *tile_y,
                pass: planned.kind,
                source_layers: planned.source_layers,
                source_bytes: planned.source_bytes,
                output_width: decoded.width,
                output_height: decoded.height,
                content_type: "image/webp".to_owned(),
            };
            let frame = encode_tile_frame(&header, &decoded.webp)?;
            socket
                .lock()
                .await
                .send(Message::Binary(frame.into()))
                .await?;
        }
    }
    otel_span.end();
    Ok(())
}

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

fn default_asset() -> RegisteredAsset {
    let corpus_root = std::env::var_os("RIVER_CORPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/cache"));
    let source = Jpeg2000Source::from_path(sample_path(&corpus_root), 2_592, 1_944);
    let source_available = source.is_available();
    RegisteredAsset {
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
    }
}
