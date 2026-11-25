use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("asset id must be a content SHA-256 or an s3v/filev-prefixed version identity")]
    InvalidAssetId,
}
