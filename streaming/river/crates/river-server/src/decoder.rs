use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jpeg2k::{DecodeArea, DecodeParameters, Image, ImagePixelData};

#[derive(Clone, Debug)]
pub struct Jpeg2000Source {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct DecodedRaster {
    pub width: u32,
    pub height: u32,
    pub webp: Vec<u8>,
}

impl Jpeg2000Source {
    pub fn from_path(path: impl Into<PathBuf>, width: u32, height: u32) -> Self {
        Self {
            path: path.into(),
            width,
            height,
        }
    }

    pub fn is_available(&self) -> bool {
        self.path.is_file()
    }

    /// Decodes only the requested source reference-grid area through OpenJPEG,
    /// then serializes its canonical 8-bit RGBA raster as WebP.
    pub fn decode_region(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        reduce: u8,
        layers: u32,
    ) -> Result<DecodedRaster> {
        if width == 0 || height == 0 || x >= self.width || y >= self.height {
            bail!("requested JPEG 2000 region is empty or out of bounds");
        }
        let end_x = x.saturating_add(width).min(self.width);
        let end_y = y.saturating_add(height).min(self.height);
        let parameters = DecodeParameters::new()
            .reduce(reduce as u32)
            .layers(layers)
            .decode_area(Some(DecodeArea::new(x, y, end_x, end_y)));
        let image = Image::from_file_with(&self.path, parameters)
            .with_context(|| format!("OpenJPEG failed to decode {}", self.path.display()))?;
        let pixels = image
            .get_pixels(Some(255))
            .context("decoded JPEG 2000 components cannot be converted to 8-bit sRGB/RGBA")?;
        let (decoded_width, decoded_height, rgba) = rgba8(pixels)?;
        let encoded = webp::Encoder::from_rgba(&rgba, decoded_width, decoded_height)
            .encode(82.0)
            .to_vec();
        Ok(DecodedRaster {
            width: decoded_width,
            height: decoded_height,
            webp: encoded,
        })
    }
}

fn rgba8(pixels: jpeg2k::ImageData) -> Result<(u32, u32, Vec<u8>)> {
    let width = pixels.width;
    let height = pixels.height;
    let rgba = match pixels.data {
        ImagePixelData::Rgba8(data) => data,
        ImagePixelData::Rgb8(data) => data
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        ImagePixelData::La8(data) => data
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        ImagePixelData::L8(data) => data
            .into_iter()
            .flat_map(|value| [value, value, value, 255])
            .collect(),
        other => bail!("unsupported decoded JPEG 2000 pixel format: {other:?}"),
    };
    Ok((width, height, rgba))
}

pub fn sample_path(corpus_root: &Path) -> PathBuf {
    corpus_root.join("Bretagne2.j2k")
}
