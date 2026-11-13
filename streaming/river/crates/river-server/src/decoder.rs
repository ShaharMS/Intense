use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use jpeg2k::{DecodeArea, DecodeParameters, Image, ImagePixelData};
use river_core::RasterFormat;
use tracing::{info_span, instrument};

#[cfg(feature = "grok")]
pub(crate) mod grok;
#[cfg(any(feature = "grok", test))]
pub(crate) mod openht_policy;
mod native;
mod pixels;
#[cfg(test)]
mod empty_precinct_tests;
mod reduced_tiles;
pub(crate) mod thread_budget;

#[derive(Clone, Debug)]
pub struct Jpeg2000Source {
    pub path: PathBuf,
    pub remote: Option<std::sync::Arc<crate::remote_source::RemoteSource>>,
    pub memory: Option<std::sync::Arc<[u8]>>,
    pub components:Option<u8>,
    pub display_rgb_only:bool,
    pub prepared: Option<std::sync::Arc<PreparedIo>>,
    pub width: u32,
    pub height: u32,
    pub reference_origin: (u32,u32),
    /// Ingestion-published codestream layout. This is metadata, not a raster
    /// cache: it lets the request planner avoid opening a source just to rule
    /// out an unsuitable native-tile traversal.
    pub native_tile_layout: Option<NativeTileLayoutHint>,
}

#[derive(Debug)]
pub struct PreparedIo {
    pub file_ranges:bool,
    pub view_reduction:u8,
    pub pcrl_lowest_revision:bool,
    pub technical_origin:Option<(u32,u32)>,
    pub predecoded_origin:(u32,u32),
    /// Work already performed concurrently with source preparation.
    pub overlapped_decode_ms:f64,
    pub predecoded:Option<std::sync::Arc<image::RgbaImage>>,
    pub original_path: PathBuf,
    pub original_tiles: Vec<(u32, u32)>,
    pub tile_offset_x: u32,
    pub tile_offset_y: u32,
    pub route: &'static str,
    pub stats: crate::remote_source::ReadStats,
    pub elapsed_ms: f64,
    pub _file: Option<std::sync::Arc<crate::storage_mode::ScratchFile>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeTileLayoutHint {
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
}

#[derive(Clone, Debug)]
pub struct DecodedRaster {
    pub width: u32,
    pub height: u32,
    pub bytes: axum::body::Bytes,
    pub content_type: &'static str,
    /// Route surfaced in every frame header, paired with the OTLP span.
    pub decoder_backend: &'static str,
    pub decoder_route: &'static str,
    pub decode_ms: f64,
    pub encode_ms: f64,
    pub source_cache_status: &'static str,
    pub source_cache_ms: f64,
    pub compressed_resolution_view: bool,
    pub source_read_bytes: u64,
    pub source_range_requests: u64,
    pub source_cache_hit_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct DecodedTile {
    pub tile_x: u32,
    pub tile_y: u32,
    pub raster: DecodedRaster,
    pub rgba: Option<axum::body::Bytes>,
}

impl Jpeg2000Source {
    #[instrument(
        name = "river.source.local.from_path",
        skip(path),
        fields(width, height)
    )]
    pub fn from_path(path: impl Into<PathBuf>, width: u32, height: u32) -> Self {
        Self {
            path: path.into(),
            remote: None,
            memory: None,
            components:None,display_rgb_only:false,
            prepared: None,
            width,
            height,
            reference_origin: (0,0),
            native_tile_layout: None,
        }
    }

    #[instrument(
        name = "river.source.local.from_catalog",
        skip(path),
        fields(width, height, native_layout = layout.is_some())
    )]
    pub fn from_catalog(
        path: impl Into<PathBuf>,
        width: u32,
        height: u32,
        layout: Option<NativeTileLayoutHint>,
    ) -> Self {
        Self {
            path: path.into(),
            remote: None,
            memory: None,
            components:None,display_rgb_only:false,
            prepared: None,
            width,
            height,
            reference_origin: (0,0),
            native_tile_layout: layout,
        }
    }

    #[instrument(name = "river.source.local.is_available", skip(self), fields(width = self.width, height = self.height))]
    pub fn is_available(&self) -> bool {
        self.remote.is_some() || self.memory.is_some() || self.path.is_file()
    }

    /// Decodes the union of a viewport's tile regions once, then cuts the
    /// canonical reduced raster into independently deliverable encoded tiles.
    ///
    /// Opening an OpenJPEG codec for every 512px tile dominated a full-HD
    /// viewport. This keeps the source operation to one ROI decode while
    /// retaining the tile boundaries the browser uses for progressive display.
    #[instrument(
        name = "river.codec.openjpeg.decode_tiles",
        skip(self, tiles),
        fields(tile_count = tiles.len(), source_tile, reduce, layers, source_width = self.width, source_height = self.height)
    )]
    pub fn decode_tiles(
        &self,
        tiles: &[(u32, u32)],
        source_tile: u32,
        reduce: u8,
        layers: u32,
        format: RasterFormat,
    ) -> Result<Vec<DecodedTile>> {
        self.decode_tiles_with_threads(tiles,source_tile,reduce,layers,format,1)
    }

    pub fn decode_tiles_with_threads(&self,tiles:&[(u32,u32)],source_tile:u32,reduce:u8,layers:u32,format:RasterFormat,decoder_threads:usize)->Result<Vec<DecodedTile>> {
        if reduced_tiles::eligible(self) {
            if let Some(tiles)=reduced_tiles::render(self,tiles,source_tile,reduce,layers,format,decoder_threads)? {return Ok(tiles);}
        }
        self.decode_tiles_standard(tiles,source_tile,reduce,layers,format,decoder_threads)
    }

    fn decode_tiles_standard(
        &self,tiles:&[(u32,u32)],source_tile:u32,reduce:u8,layers:u32,
        format:RasterFormat,decoder_threads:usize,
    )->Result<Vec<DecodedTile>> {
        if tiles.is_empty() {
            return Ok(Vec::new());
        }
        let regions = tiles
            .iter()
            .copied()
            .map(|(tile_x, tile_y)| {
                let (start_x, end_x) = clamped_tile_bounds(tile_x, source_tile, self.width);
                let (start_y, end_y) = clamped_tile_bounds(tile_y, source_tile, self.height);
                (tile_x, tile_y, start_x, start_y, end_x, end_y)
            })
            .collect::<Vec<_>>();
        let origin_x = regions.iter().map(|region| region.2).min().unwrap_or(0);
        let origin_y = regions.iter().map(|region| region.3).min().unwrap_or(0);
        let end_x = regions.iter().map(|region| region.4).max().unwrap_or(0);
        let end_y = regions.iter().map(|region| region.5).max().unwrap_or(0);
        if origin_x >= end_x || origin_y >= end_y {
            bail!("requested JPEG 2000 tile union is empty");
        }

        let (reference_x0,reference_y0,reference_x1,reference_y1) = self.reference_area((origin_x,origin_y,end_x,end_y))?;
        let parameters = DecodeParameters::new()
            .threads(decoder_threads.min(i32::MAX as usize) as u32)
            .reduce(u32::from(self.technical_reduction(reduce)?))
            .layers(layers)
            .decode_area(Some(DecodeArea::new(reference_x0, reference_y0, reference_x1, reference_y1)));
        let decode_started = Instant::now();
        let _decode_span = info_span!(
            "river.codec.openjpeg.decode_area",
            origin_x,
            origin_y,
            end_x,
            end_y
        )
        .entered();
        let image = if let Some(bytes) = &self.memory {
            Image::from_bytes_with(bytes, parameters)
        } else { Image::from_file_with(&self.path, parameters) }
            .with_context(|| format!("OpenJPEG failed to decode {}", self.path.display()))?;
        let codec_ms=elapsed_ms(decode_started);
        let conversion_started=Instant::now();
        let direct=pixels::rgb8(&image);
        let used_direct=direct.is_some();
        let (image_width,image_height,rgba)=if let Some(direct)=direct {direct}else {
            let pixels=image.get_pixels(None)
                .context("decoded JPEG 2000 components cannot be converted to 8-bit sRGB/RGBA")?;
            rgba8(pixels)?
        };
        if std::env::var_os("RIVER_PROFILE_SOURCE").is_some() {
            tracing::info!(codec_ms,conversion_ms=elapsed_ms(conversion_started),used_direct,decoder_threads,image_width,image_height,"OpenJPEG decode stages");
        }
        drop(_decode_span);
        let decode_ms = elapsed_ms(decode_started);
        let reduction = 1u32.checked_shl(reduce as u32).unwrap_or(u32::MAX);
        regions
            .into_iter()
            .map(|(tile_x, tile_y, start_x, start_y, end_x, end_y)| {
                let offset_x = start_x.saturating_sub(origin_x) / reduction;
                let offset_y = start_y.saturating_sub(origin_y) / reduction;
                let width = image_width
                    .saturating_sub(offset_x)
                    .min((end_x - start_x).div_ceil(reduction));
                let height = image_height
                    .saturating_sub(offset_y)
                    .min((end_y - start_y).div_ceil(reduction));
                if width == 0 || height == 0 {
                    bail!("decoded JPEG 2000 tile is outside the decoded union");
                }
                let _encode_span = info_span!(
                    "river.codec.raster.encode",
                    tile_x,
                    tile_y,
                    width,
                    height,
                    format = ?format
                )
                .entered();
                let encode_started = Instant::now();
                let bytes = encode_crop_rgba(
                    &rgba,
                    image_width,
                    offset_x,
                    offset_y,
                    width,
                    height,
                    format,
                )?;
                Ok(DecodedTile { rgba:take_pixel_capture(),
                    tile_x,
                    tile_y,
                    raster: DecodedRaster {
                        width,
                        height,
                        bytes: bytes.into_boxed_slice().into(),
                        content_type: format.content_type(),
                        decoder_backend: "openjpeg", decoder_route: "roi",
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
            .collect()
    }

    /// Uses OpenJPEG's native-tile entrypoint when a codestream really is
    /// tiled. `None` means the source is a one-tile image, for which an ROI
    /// decode is the correct direct operation rather than an emulated tile
    /// path.
    #[instrument(
        name = "river.codec.openjpeg.native_tile_route",
        skip(self, tiles),
        fields(tile_count = tiles.len(), source_tile, reduce, layers, decoder_threads, max_native_tiles)
    )]
    pub fn decode_native_tiled_tiles(
        &self,
        tiles: &[(u32, u32)],
        source_tile: u32,
        reduce: u8,
        layers: u32,
        format: RasterFormat,
        decoder_threads: usize,
        max_native_tiles: usize,
        snapshot: &str,
    ) -> Result<Option<Vec<DecodedTile>>> {
        if self.reference_origin != (0,0) || self.prepared.as_ref().is_some_and(|p|p.technical_origin.is_some() || p.view_reduction>0) { return Ok(None); }
        native::decode_transport_tiles(
            &self.path,
            self.width,
            self.height,
            tiles,
            source_tile,
            reduce,
            layers,
            format,
            decoder_threads,
            max_native_tiles,
            self.native_tile_layout,
            snapshot,
            self.memory.as_ref(),
        )
    }
}

impl Jpeg2000Source {
    pub fn technical_reduction(&self,reduce:u8)->Result<u8> {
        reduce.checked_sub(self.prepared.as_ref().map_or(0,|p|p.view_reduction)).context("requested resolution exceeds prepared view")
    }
    pub fn technical_area(&self,area:(u32,u32,u32,u32))->Result<(u32,u32,u32,u32)> {
        let (x,y)=self.prepared.as_ref().and_then(|p|p.technical_origin).unwrap_or((0,0));
        let scale=1u32<<self.prepared.as_ref().map_or(0,|p|p.view_reduction);
        Ok((area.0.checked_sub(x).context("ROI precedes technical window x")?/scale,
            area.1.checked_sub(y).context("ROI precedes technical window y")?/scale,
            area.2.checked_sub(x).context("ROI precedes technical window x")?.div_ceil(scale),
            area.3.checked_sub(y).context("ROI precedes technical window y")?.div_ceil(scale)))
    }
    pub fn reference_area(&self, area: (u32,u32,u32,u32)) -> Result<(u32,u32,u32,u32)> {
        let (x0,y0,x1,y1) = self.technical_area(area)?;
        Ok((x0.checked_add(self.reference_origin.0).context("reference x overflow")?,
            y0.checked_add(self.reference_origin.1).context("reference y overflow")?,
            x1.checked_add(self.reference_origin.0).context("reference x overflow")?,
            y1.checked_add(self.reference_origin.1).context("reference y overflow")?))
    }
}

/// A delivery pyramid can be rounded down by its encoder. If the final
/// reference-grid tile maps beyond that rounded edge, reuse its final source
/// pixel for the coarse preview rather than dropping the tile and leaving a
/// black strip. The exact source pixel range arrives in the refinement pass.
pub(crate) fn clamped_tile_bounds(tile: u32, tile_size: u32, extent: u32) -> (u32, u32) {
    let start = tile.saturating_mul(tile_size).min(extent.saturating_sub(1));
    let end = tile
        .saturating_add(1)
        .saturating_mul(tile_size)
        .min(extent)
        .max(start.saturating_add(1));
    (start, end)
}

pub(crate) fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1_000.0
}

#[instrument(name = "river.codec.rgba8", skip(pixels))]
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
        // jpeg2k normalizes each component to the full u16 range before it
        // returns these variants, including 10/12-bit Sentinel components.
        // Keep the most-significant byte for River's explicitly 8-bit RGBA
        // raster contract rather than rejecting a valid JP2 source.
        ImagePixelData::L16(data) => data
            .into_iter()
            .map(u16_to_u8)
            .flat_map(|value| [value, value, value, 255])
            .collect(),
        ImagePixelData::La16(data) => data
            .chunks_exact(2)
            .flat_map(|pixel| {
                let value = u16_to_u8(pixel[0]);
                [value, value, value, u16_to_u8(pixel[1])]
            })
            .collect(),
        ImagePixelData::Rgb16(data) => data
            .chunks_exact(3)
            .flat_map(|pixel| {
                [
                    u16_to_u8(pixel[0]),
                    u16_to_u8(pixel[1]),
                    u16_to_u8(pixel[2]),
                    255,
                ]
            })
            .collect(),
        ImagePixelData::Rgba16(data) => {
            // Only source alpha reaches this variant. Missing alpha is added
            // after normalization in the L16/Rgb16 branches above.
            data.chunks_exact(4)
                .flat_map(|pixel| {
                    [
                        u16_to_u8(pixel[0]),
                        u16_to_u8(pixel[1]),
                        u16_to_u8(pixel[2]),
                        u16_to_u8(pixel[3]),
                    ]
                })
                .collect()
        }
    };
    Ok((width, height, rgba))
}

fn u16_to_u8(value: u16) -> u8 {
    ((u32::from(value) * 255 + 32_767) / 65_535) as u8
}

thread_local! {
    static CAPTURE_PIXELS:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};
    static CAPTURED_PIXELS:std::cell::RefCell<Option<axum::body::Bytes>>=const {std::cell::RefCell::new(None)};
}
pub(crate) struct PixelCapture(bool);
impl PixelCapture {
    pub fn new(enabled:bool)->Self {
        CAPTURED_PIXELS.with(|slot|slot.borrow_mut().take());
        Self(CAPTURE_PIXELS.with(|slot|slot.replace(enabled)))
    }
}
impl Drop for PixelCapture {
    fn drop(&mut self) {CAPTURE_PIXELS.with(|slot|slot.set(self.0));CAPTURED_PIXELS.with(|slot|slot.borrow_mut().take());}
}
pub(crate) fn take_pixel_capture()->Option<axum::body::Bytes> {CAPTURED_PIXELS.with(|slot|slot.borrow_mut().take())}

#[instrument(
    name = "river.codec.raster.encode_crop",
    skip(rgba),
    fields(image_width, offset_x, offset_y, width, height)
)]
pub(crate) fn encode_crop_rgba(
    rgba: &[u8],
    image_width: u32,
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
    format: RasterFormat,
) -> Result<Vec<u8>> {
    let crop = crop_rgba(rgba,image_width,offset_x,offset_y,width,height)?;
    let (crop,width,height)=crate::edges::pad(crop,width,height)?;
    let result=match format {
        RasterFormat::Webp => {
            let mut config = webp::WebPConfig::new().map_err(|_| anyhow::anyhow!("unable to configure WebP encoder"))?;
            config.quality = 82.0;
            config.alpha_compression = 1;
            config.method = i32::from(crate::encoding_settings::method());
            webp::Encoder::from_rgba(&crop,width,height).encode_advanced(&config)
                .map(|bytes| bytes.to_vec()).map_err(|error| anyhow::anyhow!("WebP encoding failed: {error:?}"))
        },
        RasterFormat::Png => {
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            // Opaque tiles need no alpha channel in the lossless PNG stream.
            // Keep RGBA for any transparency, including transparent edge padding.
            let rgb = crop.chunks_exact(4).all(|pixel| pixel[3] == 255)
                .then(|| crop.chunks_exact(4).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect::<Vec<_>>());
            encoder.set_color(if rgb.is_some() { png::ColorType::Rgb } else { png::ColorType::Rgba });
            encoder.set_depth(png::BitDepth::Eight);
            // Tiles are latency-sensitive; PNG remains exactly lossless.
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(rgb.as_deref().unwrap_or(&crop))?;
            drop(writer);
            Ok(bytes)
        }
        RasterFormat::Avif => {
            use image::ImageEncoder;
            let mut bytes = Vec::new();
            image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut bytes, 10, 80)
                .write_image(&crop, width, height, image::ExtendedColorType::Rgba8)?;
            Ok(bytes)
        }
        RasterFormat::Jxl => encode_jxl_rgba(&crop, width, height),
    };
    if result.is_ok() && CAPTURE_PIXELS.with(|slot|slot.get()) {
        CAPTURED_PIXELS.with(|slot|*slot.borrow_mut()=Some(axum::body::Bytes::copy_from_slice(&crop)));
    }
    result
}

fn crop_rgba(rgba: &[u8], image_width: u32, offset_x: u32, offset_y: u32,
    width: u32, height: u32) -> Result<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    let right = offset_x.checked_add(width).context("raster crop overflow")?;
    let bottom = offset_y.checked_add(height).context("raster crop overflow")?;
    anyhow::ensure!(width > 0 && height > 0 && right <= image_width, "invalid raster crop dimensions");
    let stride = (image_width as usize).checked_mul(4).context("raster stride overflow")?;
    let row_bytes = (width as usize).checked_mul(4).context("raster row overflow")?;
    let start = (offset_y as usize).checked_mul(stride).and_then(|n| n.checked_add(offset_x as usize*4)).context("raster crop overflow")?;
    let end = ((bottom-1) as usize).checked_mul(stride).and_then(|n| n.checked_add(right as usize*4)).context("raster crop overflow")?;
    anyhow::ensure!(end <= rgba.len(), "raster crop exceeds source buffer");
    if height == 1 || (offset_x == 0 && width == image_width) {
        return Ok(Cow::Borrowed(&rgba[start..end]));
    }
    let mut crop = Vec::with_capacity(row_bytes.checked_mul(height as usize).context("raster crop allocation overflow")?);
    for row in offset_y..bottom {
        let start = row as usize*stride+offset_x as usize*4;
        crop.extend_from_slice(&rgba[start..start+row_bytes]);
    }
    Ok(Cow::Owned(crop))
}

#[cfg(feature = "jxl")]
fn encode_jxl_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    use std::ptr;

    struct Encoder(*mut jxl_sys::JxlEncoder);
    impl Drop for Encoder {
        fn drop(&mut self) {
            unsafe { jxl_sys::JxlEncoderDestroy(self.0) };
        }
    }

    let encoder = Encoder(unsafe { jxl_sys::JxlEncoderCreate(ptr::null()) });
    if encoder.0.is_null() {
        bail!("libjxl could not create an encoder");
    }
    if unsafe { jxl_sys::JxlEncoderUseContainer(encoder.0, 1) }
        != jxl_sys::JxlEncoderStatus::JXL_ENC_SUCCESS
    {
        bail!("libjxl could not enable the JPEG XL container");
    }
    let mut basic_info = std::mem::MaybeUninit::<jxl_sys::JxlBasicInfo>::uninit();
    unsafe { jxl_sys::JxlEncoderInitBasicInfo(basic_info.as_mut_ptr()) };
    let mut basic_info = unsafe { basic_info.assume_init() };
    basic_info.xsize = width;
    basic_info.ysize = height;
    basic_info.bits_per_sample = 8;
    basic_info.num_color_channels = 3;
    basic_info.num_extra_channels = 1;
    basic_info.alpha_bits = 8;
    if unsafe { jxl_sys::JxlEncoderSetBasicInfo(encoder.0, &basic_info) }
        != jxl_sys::JxlEncoderStatus::JXL_ENC_SUCCESS
    {
        bail!("libjxl rejected the tile dimensions");
    }
    let mut color_encoding = std::mem::MaybeUninit::<jxl_sys::JxlColorEncoding>::uninit();
    unsafe { jxl_sys::JxlColorEncodingSetToSRGB(color_encoding.as_mut_ptr(), 0) };
    let color_encoding = unsafe { color_encoding.assume_init() };
    if unsafe { jxl_sys::JxlEncoderSetColorEncoding(encoder.0, &color_encoding) }
        != jxl_sys::JxlEncoderStatus::JXL_ENC_SUCCESS
    {
        bail!("libjxl could not set the tile color encoding");
    }
    let settings = unsafe { jxl_sys::JxlEncoderFrameSettingsCreate(encoder.0, ptr::null()) };
    if settings.is_null() {
        bail!("libjxl could not allocate frame settings");
    }
    if unsafe { jxl_sys::JxlEncoderSetFrameDistance(settings, 1.0_f32) }
        != jxl_sys::JxlEncoderStatus::JXL_ENC_SUCCESS
    {
        bail!("libjxl could not set the tile quality");
    }
    let pixel_format = jxl_sys::JxlPixelFormat {
        num_channels: 4,
        data_type: jxl_sys::JxlDataType::JXL_TYPE_UINT8,
        endianness: jxl_sys::JxlEndianness::JXL_NATIVE_ENDIAN,
        align: 0,
    };
    if unsafe {
        jxl_sys::JxlEncoderAddImageFrame(settings, &pixel_format, rgba.as_ptr().cast(), rgba.len())
    } != jxl_sys::JxlEncoderStatus::JXL_ENC_SUCCESS
    {
        bail!("libjxl rejected the RGBA tile input");
    }
    unsafe { jxl_sys::JxlEncoderCloseInput(encoder.0) };

    let mut output = vec![0_u8; (width as usize * height as usize / 2).max(64 * 1024)];
    let mut written = 0_usize;
    loop {
        if written == output.len() {
            if output.len() >= 64 * 1024 * 1024 {
                bail!("JPEG XL tile exceeded River's 64 MiB encoded-size limit");
            }
            output.resize(output.len() * 2, 0);
        }
        let initial_available = output.len() - written;
        let mut next_out = unsafe { output.as_mut_ptr().add(written) };
        let mut available = initial_available;
        let status =
            unsafe { jxl_sys::JxlEncoderProcessOutput(encoder.0, &mut next_out, &mut available) };
        written += initial_available - available;
        match status {
            jxl_sys::JxlEncoderStatus::JXL_ENC_SUCCESS => break,
            jxl_sys::JxlEncoderStatus::JXL_ENC_NEED_MORE_OUTPUT => continue,
            _ => bail!("libjxl failed while encoding the tile"),
        }
    }
    output.truncate(written);
    Ok(output)
}

#[cfg(not(feature = "jxl"))]
fn encode_jxl_rgba(_rgba: &[u8], _width: u32, _height: u32) -> Result<Vec<u8>> {
    bail!("JPEG XL output is unavailable because this River build omits libjxl")
}

pub fn sample_path(corpus_root: &Path) -> PathBuf {
    corpus_root.join("Bretagne2.j2k")
}

#[cfg(test)]
mod tests {
    use super::{clamped_tile_bounds, u16_to_u8};

    #[test]
    fn contiguous_rasters_are_borrowed_and_strided_png_crops_preserve_alpha() {
        let rgba: Vec<u8> = (0..48).collect();
        let contiguous = super::crop_rgba(&rgba,4,0,1,4,2).unwrap();
        assert_eq!(contiguous.as_ptr(),rgba[16..].as_ptr());
        assert_eq!(contiguous.len(),32);
        let png=super::encode_crop_rgba(&rgba,4,1,1,2,2,super::RasterFormat::Png).unwrap();
        let decoded=image::load_from_memory(&png).unwrap().to_rgba8();
        let expected=[rgba[20..28].to_vec(),rgba[36..44].to_vec()].concat();
        assert_eq!(decoded.into_raw(),expected);
        assert!(super::crop_rgba(&rgba,4,3,0,2,1).is_err());
        assert!(super::crop_rgba(&rgba,4,0,2,4,2).is_err());
        assert!(super::crop_rgba(&rgba,4,u32::MAX,0,2,1).is_err());
    }

    #[test]
    fn grayscale_16_bit_rasters_are_opaque_and_real_alpha_is_preserved() {
        let (_, _, rgba) = super::rgba8(jpeg2k::ImageData {
            width: 1, height: 1, format: jpeg2k::ImageFormat::L16,
            data: jpeg2k::ImagePixelData::L16(vec![32768]),
        }).unwrap();
        assert_eq!(rgba, [128, 128, 128, 255]);
        let (_, _, rgba) = super::rgba8(jpeg2k::ImageData {
            width: 1, height: 1, format: jpeg2k::ImageFormat::Rgba16,
            data: jpeg2k::ImagePixelData::Rgba16(vec![65535, 65535, 65535, 255]),
        }).unwrap();
        assert_eq!(rgba, [255, 255, 255, 1]);
    }

    #[test]
    fn normalizes_full_range_u16_components_to_u8() {
        assert_eq!(u16_to_u8(0), 0);
        assert_eq!(u16_to_u8(u16::MAX), 255);
        assert_eq!(u16_to_u8(32_768), 128);
    }

    #[test]
    fn keeps_a_preview_pixel_for_a_rounded_down_pyramid_edge() {
        assert_eq!(clamped_tile_bounds(9, 64, 640), (576, 640));
        assert_eq!(clamped_tile_bounds(10, 64, 640), (639, 640));
    }
}

#[cfg(test)]
mod opaque_png_tests {
    use super::*;
    #[test]
    fn opaque_crop_omits_alpha_and_preserves_exact_pixels() {
        let rgba = [1,2,3,255, 7,11,13,255, 17,19,23,255, 29,31,37,255];
        let bytes = encode_crop_rgba(&rgba,2,1,0,1,2,RasterFormat::Png).unwrap();
        assert_eq!(bytes[25],2); // IHDR RGB, not an alpha-bearing stream.
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(image.dimensions(),(1,2));
        assert_eq!(image.as_raw(), &[7,11,13,255,29,31,37,255]);
    }
    #[test]
    fn partial_and_zero_alpha_are_preserved() {
        for alpha in [0,1,127,254] {
            let rgba=[1,2,3,255,7,11,13,alpha];
            let bytes=encode_crop_rgba(&rgba,2,0,0,2,1,RasterFormat::Png).unwrap();
            assert_eq!(bytes[25],6); // IHDR RGBA.
            assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8().as_raw(),&rgba);
        }
    }
    #[test]
    fn transparent_padding_stays_rgba_and_black_padding_can_be_rgb() {
        for (mode,color,alpha) in [(crate::edges::Mode::Transparent,6,0),(crate::edges::Mode::Black,2,255)] {
            let _scope=crate::edges::Scope::new(mode,2,0);
            let bytes=encode_crop_rgba(&[1,2,3,255],1,0,0,1,1,RasterFormat::Png).unwrap();
            assert_eq!(bytes[25],color);
            let image=image::load_from_memory(&bytes).unwrap().to_rgba8();
            assert_eq!(image.dimensions(),(2,2));assert_eq!(image.get_pixel(0,0).0,[1,2,3,255]);
            assert_eq!(image.get_pixel(1,1).0,[0,0,0,alpha]);
        }
    }
}
