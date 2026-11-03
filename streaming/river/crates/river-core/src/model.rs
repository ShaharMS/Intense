use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Raster edge length at the requested reduction level. A 512-pixel tile keeps
/// full-HD viewports to roughly a dozen OpenJPEG regions rather than forty;
/// decoder setup is a larger cost than the additional raster payload locally.
pub const TILE_SIZE: u32 = 512;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AssetId(pub String);

impl AssetId {
    pub fn validate(&self) -> Result<(), ModelError> {
        let is_sha256 = self.0.len() == 64 && self.0.bytes().all(|byte| byte.is_ascii_hexdigit());
        let is_remote_version = self.0.strip_prefix("s3v-").is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
        let is_file_version = self.0.strip_prefix("filev-").is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
        if is_sha256 || is_remote_version || is_file_version {
            Ok(())
        } else {
            Err(ModelError::InvalidAssetId)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManifest {
    pub asset_id: AssetId,
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub bit_depth: u8,
    pub resolution_levels: u8,
    pub quality: QualityModel,
    pub color: ColorPolicy,
    pub capability: Capability,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityModel {
    /// Cumulative codestream bytes available after each quality layer.
    pub cumulative_layer_bytes: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorPolicy {
    pub output: String,
    pub source_profile: SourceColorProfile,
    pub assumption_recorded: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceColorProfile {
    Embedded,
    AssumedSrgb,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum Capability {
    FastPath,
    Limited { reasons: Vec<String> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub asset_id: AssetId,
    pub sequence: u64,
    /// Coordinates are in the full-resolution source reference grid.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// A discrete power-of-two divisor of source resolution.
    pub zoom: u8,
}

impl Viewport {
    pub fn validate_against(&self, manifest: &AssetManifest) -> Result<(), ModelError> {
        self.asset_id.validate()?;
        if self.width == 0 || self.height == 0 {
            return Err(ModelError::EmptyViewport);
        }
        if self.zoom >= manifest.resolution_levels {
            return Err(ModelError::UnsupportedZoom);
        }
        let end_x = self
            .x
            .checked_add(self.width)
            .ok_or(ModelError::OutOfBounds)?;
        let end_y = self
            .y
            .checked_add(self.height)
            .ok_or(ModelError::OutOfBounds)?;
        if end_x > manifest.width || end_y > manifest.height {
            return Err(ModelError::OutOfBounds);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ClientMessage {
    Viewport {
        #[serde(default,rename="progressivePatches")]
        progressive_patches: bool,
        viewport: Viewport,
        #[serde(default)]
        format: RasterFormat,
        #[serde(default)]
        driver: DecoderDriver,
    },
    Tiles {
        batch: TileBatch,
        #[serde(default)]
        format: RasterFormat,
        #[serde(default)]
        driver: DecoderDriver,
    },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RasterFormat {
    #[default]
    Webp,
    Png,
    Avif,
    Jxl,
}

impl RasterFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Webp => "image/webp",
            Self::Png => "image/png",
            Self::Avif => "image/avif",
            Self::Jxl => "image/jxl",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Png => "png",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecoderDriver {
    #[default]
    #[serde(rename = "openjpeg")]
    OpenJpeg,
    #[serde(rename = "jp2grok")]
    Jp2Grok,
    #[serde(rename = "openhtj2k")]
    OpenHtj2k,
    #[serde(rename = "nvjpeg2000")]
    NvJpeg2000,
}

impl DecoderDriver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenJpeg => "openjpeg",
            Self::Jp2Grok => "jp2grok",
            Self::OpenHtj2k => "openhtj2k",
            Self::NvJpeg2000 => "nvjpeg2000",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::OpenJpeg => "OpenJPEG",
            Self::Jp2Grok => "JP2Grok",
            Self::OpenHtj2k => "OpenHTJ2K + Grok fallback",
            Self::NvJpeg2000 => "nvJPEG2000",
        }
    }
}

/// Source raster tile indices. `z` is River's power-of-two reduction, with
/// zero at full resolution; these are not geographic slippy-map coordinates.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TileXyz {
    pub x: u32,
    pub y: u32,
    pub z: u8,
}

pub const MAX_BATCH_TILES: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TileBatch {
    pub asset_id: AssetId,
    pub sequence: u64,
    pub tiles: Vec<TileXyz>,
}

impl TileBatch {
    pub fn validate_against(&self, manifest: &AssetManifest) -> Result<(), ModelError> {
        self.asset_id.validate()?;
        if self.tiles.is_empty() || self.tiles.len() > MAX_BATCH_TILES {
            return Err(ModelError::InvalidBatchSize);
        }
        for tile in &self.tiles {
            if tile.z >= manifest.resolution_levels {
                return Err(ModelError::UnsupportedZoom);
            }
            let edge = TILE_SIZE
                .checked_shl(u32::from(tile.z))
                .filter(|edge| *edge != 0)
                .ok_or(ModelError::UnsupportedZoom)?;
            if tile.x >= manifest.width.div_ceil(edge) || tile.y >= manifest.height.div_ceil(edge) {
                return Err(ModelError::OutOfBounds);
            }
        }
        Ok(())
    }
}

impl ClientMessage {
    pub fn asset_id(&self) -> &AssetId {
        match self {
            Self::Viewport { viewport, .. } => &viewport.asset_id,
            Self::Tiles { batch, .. } => &batch.asset_id,
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Self::Viewport { viewport, .. } => viewport.sequence,
            Self::Tiles { batch, .. } => batch.sequence,
        }
    }

    pub fn raster_format(&self) -> RasterFormat {
        match self {
            Self::Viewport { format, .. } | Self::Tiles { format, .. } => *format,
        }
    }

    pub fn decoder_driver(&self) -> DecoderDriver {
        match self {
            Self::Viewport { driver, .. } | Self::Tiles { driver, .. } => *driver,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum ServerMessage {
    Accepted {
        session_id: Uuid,
        trace_id: String,
        sequence: u64,
    },
    Limited {
        sequence: u64,
        reasons: Vec<String>,
    },
    Error {
        sequence: Option<u64>,
        code: String,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PassKind {
    Preview,
    Gain,
    Final,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TileFrameHeader {
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub source_tile_edge: Option<u32>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub preview: Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub preview_patch_count: Option<usize>,
    pub session_id: Uuid,
    pub trace_id: String,
    pub asset_id: AssetId,
    pub sequence: u64,
    pub zoom: u8,
    pub tile_x: u32,
    pub tile_y: u32,
    pub pass: PassKind,
    pub source_layers: usize,
    pub source_bytes: u64,
    pub output_width: u32,
    pub output_height: u32,
    pub content_type: String,
    /// Durations measured in River immediately before this frame is sent.
    pub server_timing: ServerTiming,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTiming {
    /// The source-decoding route selected for this frame. This mirrors the
    /// route attribute on the OpenTelemetry codec span.
    pub decoder_route: String,
    /// Time waiting for a Tokio blocking-decode worker.
    pub worker_queue_ms: f64,
    /// JPEG 2000 source decode plus conversion to River's canonical RGBA raster.
    pub decode_ms: f64,
    /// Requested output format encoding time for the canonical raster.
    pub encode_ms: f64,
    #[serde(default)]
    pub source_cache_status: String,
    #[serde(default)]
    pub source_cache_ms: f64,
    #[serde(default)]
    pub source_read_bytes: u64,
    #[serde(default)]
    pub source_range_requests: u64,
    #[serde(default)]
    pub source_cache_hit_bytes: u64,
    /// Whole elapsed server time from receiving this viewport through frame preparation.
    pub elapsed_ms: f64,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("tile batch must contain between 1 and 256 XYZ entries")]
    InvalidBatchSize,
    #[error("asset id must be a content SHA-256 or an s3v/filev-prefixed version identity")]
    InvalidAssetId,
    #[error("viewport must have non-zero width and height")]
    EmptyViewport,
    #[error("viewport exceeds the source reference grid")]
    OutOfBounds,
    #[error("viewport zoom is not available for this asset")]
    UnsupportedZoom,
}
