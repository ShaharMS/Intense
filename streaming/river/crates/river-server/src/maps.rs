use super::*;
use axum::response::Html;
use serde::{Deserialize, Serialize};

const TILE_EDGE: u32 = 256;

#[derive(Clone, Copy, Deserialize, Serialize)]
enum Grid {
    #[serde(rename = "EPSG:4326")]
    Geographic,
    #[serde(rename = "EPSG:3857")]
    Mercator,
}

impl Grid {
    fn contains(self, z: u8, x: u32, y: u32) -> bool {
        let rows = 1 << z;
        let columns = match self {
            Self::Geographic => 2 * rows,
            Self::Mercator => rows,
        };
        x < columns && y < rows
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Layer {
    title: String,
    attribution: String,
    grid: Grid,
    bounds: [f64; 4],
    min_zoom: u8,
    max_zoom: u8,
    #[serde(skip_serializing)]
    chunks: BTreeMap<String, String>,
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Catalog {
    layers: BTreeMap<String, Layer>,
}

impl Catalog {
    pub(super) fn load() -> anyhow::Result<Self> {
        let configured = std::env::var_os("RIVER_MAP_CATALOG");
        let path = configured.as_ref().map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("RIVER_CORPUS_ROOT").unwrap_or_else(|| "corpus".into()))
                .join("maps/catalog.json")
        });
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) if configured.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("Reading {}", path.display())),
        };
        let catalog: Self =
            serde_json::from_slice(&data).with_context(|| format!("Parsing {}", path.display()))?;
        for (id, layer) in &catalog.layers {
            anyhow::ensure!(
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "Invalid map layer ID {id}"
            );
            let [west, south, east, north] = layer.bounds;
            anyhow::ensure!(
                -180.0 <= west
                    && west < east
                    && east <= 180.0
                    && -90.0 <= south
                    && south < north
                    && north <= 90.0,
                "Invalid bounds for map layer {id}"
            );
            anyhow::ensure!(
                layer.min_zoom <= layer.max_zoom
                    && layer.max_zoom <= 22
                    && layer.max_zoom - layer.min_zoom <= 7,
                "Invalid zoom range for map layer {id}"
            );
            for (coordinate, key) in &layer.chunks {
                let valid_coordinate = coordinate
                    .split_once('/')
                    .and_then(|(x, y)| Some((x.parse::<u32>().ok()?, y.parse::<u32>().ok()?)))
                    .is_some_and(|(x, y)| {
                        *coordinate == format!("{x}/{y}")
                            && layer.grid.contains(layer.min_zoom, x, y)
                    });
                anyhow::ensure!(
                    valid_coordinate
                        && key.ends_with(".jp2")
                        && FsPath::new(key)
                            .components()
                            .all(|part| matches!(part, std::path::Component::Normal(_))),
                    "Invalid chunk {coordinate} for map layer {id}"
                );
            }
            anyhow::ensure!(!layer.chunks.is_empty(), "Map layer {id} has no chunks");
        }
        Ok(catalog)
    }
}

pub(super) async fn viewer() -> Html<&'static str> {
    Html(include_str!("maps/index.html"))
}

pub(super) async fn script() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        include_str!("maps/viewer.js"),
    )
}

pub(super) async fn catalog(State(state): State<AppState>) -> Response {
    no_store(Json(state.map_catalog.as_ref()).into_response())
}

pub(super) async fn tile(
    State(state): State<AppState>,
    Path((id, z, x, y)): Path<(String, u8, u32, u32)>,
) -> Response {
    let Some(layer) = state.map_catalog.layers.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if z > layer.max_zoom || !layer.grid.contains(z, x, y) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if z < layer.min_zoom {
        return empty_tile();
    }
    let size = 1 << (z - layer.min_zoom);
    let Some(key) = layer.chunks.get(&format!("{}/{}", x / size, y / size)) else {
        return empty_tile();
    };
    let query = compatibility::TileQuery::map_tile(key, TILE_EDGE, layer.min_zoom, layer.max_zoom);
    let response = compatibility::tile(
        State(state),
        Path((z, x % size, y % size, "tile.png".into())),
        Query(query),
    )
    .await;
    no_store(response)
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

fn empty_tile() -> Response {
    no_store(
        (
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            include_bytes!("maps/transparent.png").as_slice(),
        )
            .into_response(),
    )
}
