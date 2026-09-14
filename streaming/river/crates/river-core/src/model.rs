use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const TILE_SIZE: u32 = 256;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AssetId(pub String);

impl AssetId {
    pub fn validate(&self) -> Result<(), ModelError> {
        let is_sha256 = self.0.len() == 64 && self.0.bytes().all(|byte| byte.is_ascii_hexdigit());
        if is_sha256 {
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
    Viewport { viewport: Viewport },
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
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("asset id must be a SHA-256 hex digest")]
    InvalidAssetId,
    #[error("viewport must have non-zero width and height")]
    EmptyViewport,
    #[error("viewport exceeds the source reference grid")]
    OutOfBounds,
    #[error("viewport zoom is not available for this asset")]
    UnsupportedZoom,
}
