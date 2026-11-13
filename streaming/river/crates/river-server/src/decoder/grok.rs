use std::{path::PathBuf, process::Command, time::Instant};

use anyhow::{Context, Result, bail};
use image::{ImageFormat, ImageReader};
use tracing::instrument;
mod native;
mod packed;
#[cfg(unix)] mod openht_worker;
mod cancellation;
pub(crate) use cancellation::enter as cancellation_scope;
fn cancelled()->bool {cancellation::cancelled()}
pub(crate) fn openht_available()->bool {
 #[cfg(unix)] {openht_worker::available()}
 #[cfg(not(unix))] {false}
}

pub(crate) fn native_enabled() -> bool {
    std::env::var("RIVER_GROK_NATIVE").is_ok_and(|value| value == "1" || value == "true")
}

use super::{
    DecodedRaster, DecodedTile, Jpeg2000Source, RasterFormat, clamped_tile_bounds, elapsed_ms,
    encode_crop_rgba,
};

pub(crate) fn is_available() -> bool {
    Command::new(executable())
        .arg("-h")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn executable() -> PathBuf {
    std::env::var_os("RIVER_GROK_DECOMPRESS").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("grk_decompress"))
}

#[instrument(
    name = "river.codec.grok.decode_tiles",
    skip(source, tiles),
    fields(tile_count = tiles.len(), source_tile, reduction, layers, source_width = source.width, source_height = source.height)
)]
pub(crate) fn decode_tiles(
    source: &Jpeg2000Source,
    tiles: &[(u32, u32)],
    source_tile: u32,
    reduction: u8,
    layers: u32,
    format: RasterFormat,
    prefer_openht:bool,
) -> Result<Vec<DecodedTile>> {
    if tiles.is_empty() {
        return Ok(Vec::new());
    }
    if source.components == Some(4) && !source.display_rgb_only {
        if let Ok(mut output) = source.decode_tiles(tiles, source_tile, reduction, layers, format) {
            for tile in &mut output {
                tile.raster.decoder_route = "openjpeg-declared-alpha-fallback";
            }
            return Ok(output);
        }
    }
    if source.prepared.as_ref().is_some_and(|p|p.pcrl_lowest_revision && p.route!="s3-multipart-lowest-rpcl-ranges") {
        let mut decoded=source.decode_tiles(tiles,source_tile,reduction,layers,format)?;
        for tile in &mut decoded {tile.raster.decoder_route="openjpeg-pcrl-lowest-compatibility";}
        return Ok(decoded);
    }
    let regions = tiles
        .iter()
        .copied()
        .map(|(tile_x, tile_y)| {
            let (x0, x1) = clamped_tile_bounds(tile_x, source_tile, source.width);
            let (y0, y1) = clamped_tile_bounds(tile_y, source_tile, source.height);
            (tile_x, tile_y, x0, y0, x1, y1)
        })
        .collect::<Vec<_>>();
    let origin_x = regions.iter().map(|region| region.2).min().unwrap_or(0);
    let origin_y = regions.iter().map(|region| region.3).min().unwrap_or(0);
    let end_x = regions.iter().map(|region| region.4).max().unwrap_or(0);
    let end_y = regions.iter().map(|region| region.5).max().unwrap_or(0);
    // Grok's decompression window is image-relative; OpenJPEG's ROI is
    // reference-grid absolute. Do not add the SIZ origin twice here.
    let technical_area=source.technical_area((origin_x,origin_y,end_x,end_y))?;
    let effective_reduction=source.technical_reduction(reduction)?;
    let area = format!("{},{},{},{}",technical_area.0,technical_area.1,technical_area.2,technical_area.3);
    let started = Instant::now();
    let (image,backend) = if source.display_rgb_only {
        match native::decode(source, technical_area, effective_reduction, layers, false) {
            Ok((image, _)) => (image, "jp2grok-native-multispectral-rgb"),
            Err(error) => {
                if cancelled() { return Err(error); }
                let mut tiles = source.decode_tiles(tiles, source_tile, reduction, layers, format)?;
                for tile in &mut tiles {
                    tile.raster.decoder_route = "openjpeg-multispectral-fallback";
                }
                return Ok(tiles);
            }
        }
    } else if prefer_openht || native_enabled() {
        match native::decode(source, technical_area, effective_reduction, layers, prefer_openht) {
            Ok(image) => image,
            Err(error) => {
                if cancelled(){return Err(error);}
                #[cfg(unix)]
                let worker_failed = error.is::<openht_worker::Failure>();
                #[cfg(not(unix))]
                let worker_failed = false;
                if worker_failed && std::env::var("RIVER_OPENHT_STRICT").as_deref() == Ok("true") {
                    return Err(error);
                }
                if worker_failed {
                    tracing::warn!(%error, "OpenHT worker failed; retrying the prepared region with Grok");
                }
                let recovered = if worker_failed && native_enabled() {
                    native::decode(source, technical_area, effective_reduction, layers, false).ok()
                } else {
                    None
                };
                if cancelled() { bail!("render request superseded"); }
                match recovered {
                    Some((image, _)) => (image, "openht-worker-failure-grok"),
                    None => {
                        tracing::debug!(%error, "using compatible Grok CLI");
                        let image = decode_cli(source, &area, effective_reduction, layers)?;
                        let route = if worker_failed { "openht-worker-failure-grok-cli" } else { "jp2grok-cli" };
                        (image, route)
                    }
                }
            }
        }
    } else {
        (decode_cli(source, &area, effective_reduction, layers)?,"jp2grok-cli")
    };
    if cancelled(){bail!("render request superseded");}
    let decoder_backend = match backend {
        "openht-worker-failure-grok" => "jp2grok-native",
        "openht-worker-failure-grok-cli" => "jp2grok-cli",
        other => other,
    };
    let backend = match (prefer_openht, backend) {
        (true, "jp2grok-native" | "jp2grok-cli") => "openht-fallback-jp2grok",
        (false, "jp2grok-native" | "jp2grok-cli") => "jp2grok",
        (_, other) => other,
    };
    let image_width = image.width();
    let image_height = image.height();
    let rgba = image.into_raw();
    let decode_ms = elapsed_ms(started)+source.prepared.as_ref().map_or(0.0,|info|info.overlapped_decode_ms);
    let reduction_scale = 1_u32.checked_shl(u32::from(reduction)).unwrap_or(u32::MAX);

    regions
        .into_iter()
        .map(|(tile_x, tile_y, x0, y0, x1, y1)| {
            if cancelled(){bail!("render request superseded");}
            let offset_x = x0.saturating_sub(origin_x) / reduction_scale;
            let offset_y = y0.saturating_sub(origin_y) / reduction_scale;
            let width = image_width
                .saturating_sub(offset_x)
                .min((x1 - x0).div_ceil(reduction_scale));
            let height = image_height
                .saturating_sub(offset_y)
                .min((y1 - y0).div_ceil(reduction_scale));
            if width == 0 || height == 0 {
                bail!("JP2Grok returned a tile outside the decoded region");
            }
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
            Ok(DecodedTile { rgba:crate::decoder::take_pixel_capture(),
                tile_x,
                tile_y,
                raster: DecodedRaster {
                    width,
                    height,
                    bytes: bytes.into_boxed_slice().into(),
                    content_type: format.content_type(),
                    decoder_backend,
                    decoder_route: backend,
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

/// Grok 20.4.15 checks JP2 channel definitions against the selected
/// component count during ROI decode. For an identity sRGB display mapping,
/// unspecified extra bands can be omitted from the temporary JP2 header.
fn write_rgb_view(bytes: &[u8], out: &mut impl std::io::Write) -> std::io::Result<bool> {
    fn boxes(data: &[u8]) -> Option<Vec<(usize, usize, &[u8])>> {
        let mut result = Vec::new();
        let mut offset = 0usize;
        while offset < data.len() {
            let header = data.get(offset..offset.checked_add(8)?)?;
            let length = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
            let end = offset.checked_add(length)?;
            if length < 8 || end > data.len() { return None; }
            result.push((offset, end, &header[4..]));
            offset = end;
        }
        Some(result)
    }
    let Some(top) = boxes(bytes) else { return Ok(false); };
    let mut headers = top.iter().filter(|(_, _, kind)| *kind == b"jp2h");
    let Some(&(start, end, _)) = headers.next() else { return Ok(false); };
    if headers.next().is_some() { return Ok(false); }
    let body = &bytes[start + 8..end];
    let Some(children) = boxes(body) else { return Ok(false); };
    let mut color = children.iter().filter(|(_, _, kind)| *kind == b"colr");
    let Some(&(a, b, _)) = color.next() else { return Ok(false); };
    if color.next().is_some() || body[a + 8..b] != [1, 0, 0, 0, 0, 0, 16]
        || children.iter().any(|(_, _, kind)| *kind == b"pclr" || *kind == b"cmap") {
        return Ok(false);
    }
    let mut definitions = children.iter().filter(|(_, _, kind)| *kind == b"cdef");
    let Some(&(a, b, _)) = definitions.next() else { return Ok(false); };
    if definitions.next().is_some() { return Ok(false); }
    let data = &body[a + 8..b];
    let Some(count) = data.get(..2) else { return Ok(false); };
    let count = u16::from_be_bytes([count[0], count[1]]) as usize;
    if !(4..=16).contains(&count) || data.len() != 2 + 6 * count { return Ok(false); }
    for (index, entry) in data[2..].chunks_exact(6).enumerate() {
        let expected = if index < 3 { [0, index as u8, 0, 0, 0, index as u8 + 1] }
            else { [0, index as u8, 255, 255, 255, 255] };
        if entry != expected { return Ok(false); }
    }
    let length = (end - start - (b - a)) as u32;
    out.write_all(&bytes[..start])?;
    out.write_all(&length.to_be_bytes())?;
    out.write_all(&bytes[start + 4..start + 8 + a])?;
    out.write_all(&bytes[start + 8 + b..])?;
    Ok(true)
}

fn decode_cli(source: &Jpeg2000Source, area: &str, reduction: u8, layers: u32) -> Result<image::RgbaImage> {
    use std::io::Write;
    let mut memory_file = None;
    if let Some(bytes) = &source.memory {
        let mut file = crate::storage_mode::ScratchFile::create("river-grok-", ".jp2", bytes.len() as u64, None)?;
        file.write_all(bytes)?;
        memory_file = Some(file);
    }
    let input_path = memory_file.as_ref().map(|file| file.path()).unwrap_or(&source.path);
    let output_bytes = if crate::storage_mode::disk_cache_enabled() { 0 } else {
        let coordinates = area.split(',').map(str::parse::<u64>).collect::<std::result::Result<Vec<_>, _>>()?;
        let [x0,y0,x1,y1]: [u64;4] = coordinates.try_into().map_err(|_| anyhow::anyhow!("Invalid Grok decode region"))?;
        let scale = 1u64.checked_shl(u32::from(reduction)).unwrap_or(u64::MAX);
        (x1.div_ceil(scale) - x0 / scale).saturating_mul(y1.div_ceil(scale) - y0 / scale)
            .saturating_mul(8).saturating_add(4096)
    };
    let output_dir = crate::storage_mode::ScratchDirectory::create(output_bytes)?;
    let output_path = output_dir.path().join("region.pam");
    let executable = executable();
    let threads = std::env::var("RIVER_GROK_THREADS_PER_DECODE").ok()
        .and_then(|value| value.parse::<usize>().ok()).unwrap_or(2).clamp(1, 16);
    let mut command = Command::new(&executable);
    output_dir.configure_child(&mut command);
    command
        .arg("-H")
        .arg(threads.to_string())
        .arg("-i")
        .arg(input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("-d")
        .arg(area)
        .arg("-r")
        .arg(reduction.to_string());
    // Grok's CLI only accepts layer counts starting at 1. Omitting the option
    // leaves it at zero, which means decode all quality layers.
    if source.display_rgb_only {command.arg("-n").arg("0,1,2");}
    if layers > 0 {
        command.arg("-l").arg(layers.to_string());
    }
    let output = command
        .output()
        .with_context(|| format!("unable to run JP2Grok decoder {}", executable.display()))?;
    if !output.status.success() {
        bail!(
            "JP2Grok failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let bytes = std::fs::read(&output_path).context("JP2Grok produced no raster output")?;
    Ok(ImageReader::with_format(std::io::Cursor::new(bytes), ImageFormat::Pnm)
        .decode()
        .context("JP2Grok output could not be decoded as PAM/PNM")?
        .to_rgba8())

}

#[cfg(unix)] pub(crate) struct PipelineView(packed::View);
#[cfg(unix)] pub(crate) fn pipeline_view(bytes:&[u8],reduction:u8)->Option<PipelineView>{
 let view=packed::build(bytes,reduction)?;view.direct_compatible.then_some(PipelineView(view))
}
#[cfg(unix)] pub(crate) fn decode_pipeline_view(view:PipelineView)->Result<image::RgbaImage>{
 openht_worker::decode_preparing(&view.0.data,view.0.width,view.0.height)
}
