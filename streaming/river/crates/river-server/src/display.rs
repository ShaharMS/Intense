//! Decide whether an original multi-band source has an unambiguous display mapping.
//! Called once during header discovery, before any selected-component decode.
use anyhow::{Context, Result, ensure};

fn boxes(mut data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        let kind = data.get(4..8)?;
        let size = u32::from_be_bytes(data[..4].try_into().ok()?);
        let (header, end) = match size {
            0 => (8, data.len()),
            1 => (
                16,
                usize::try_from(u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)).ok()?,
            ),
            _ => (8, size as usize),
        };
        let payload = data.get(header..end)?;
        data = &data[end..];
        Some((kind, payload))
    })
}

use jpeg2k::BandFallback;
const REQUIRE_LABELS: &str = "RIVER_REQUIRE_CHANNEL_LABELS";

pub fn require_channel_labels() -> Result<bool> {
    match std::env::var(REQUIRE_LABELS).as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("false" | "0") => Ok(false),
        Ok("true" | "1") => Ok(true),
        _ => anyhow::bail!("{REQUIRE_LABELS} must be true or false"),
    }
}

// JP2 channel definitions refer to output channels after component mapping.
fn check_explicit_labels(components: usize, header: &[u8], table: Option<&[u8]>) -> Result<()> {
    let table = table.with_context(|| {
        format!("{REQUIRE_LABELS} requires explicit JP2 channel definitions; none are present")
    })?;
    let channels = if let Some((_, map)) = boxes(header).find(|(kind, _)| *kind == b"cmap") {
        ensure!(
            !map.is_empty() && map.len() % 4 == 0,
            "invalid JP2 component mapping"
        );
        map.len() / 4
    } else {
        components
    };
    let count = usize::from(u16::from_be_bytes(
        table
            .get(..2)
            .context("truncated channel definitions")?
            .try_into()?,
    ));
    ensure!(
        table.len() == 2 + 6 * count,
        "invalid JP2 channel definition table"
    );
    let mut seen = vec![false; channels];
    for entry in table[2..].chunks_exact(6) {
        let index = usize::from(u16::from_be_bytes([entry[0], entry[1]]));
        let role = u16::from_be_bytes([entry[2], entry[3]]);
        let association = u16::from_be_bytes([entry[4], entry[5]]);
        ensure!(
            index < channels && !seen[index],
            "duplicate or out-of-range JP2 channel {index}"
        );
        ensure!(
            matches!((role, association), (0, 1..=65534) | (1 | 2, 0..=65534)),
            "{REQUIRE_LABELS} requires a label for band {} (JP2 type {role}, association {association})",
            index + 1
        );
        seen[index] = true;
    }
    if let Some(index) = seen.iter().position(|labelled| !labelled) {
        anyhow::bail!(
            "{REQUIRE_LABELS} requires a label for band {} (missing JP2 channel definition)",
            index + 1
        );
    }
    Ok(())
}

pub fn four_band_setting() -> Result<Option<BandFallback>> {
    match std::env::var("RIVER_UNDEFINED_FOUR_BAND_LAYOUT").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("rgb") => Ok(Some(BandFallback::Rgb)),
        Ok("bgr") => Ok(Some(BandFallback::Bgr)),
        Ok("rgba") => Ok(Some(BandFallback::Rgba)),
        Ok("bgra") => Ok(Some(BandFallback::Bgra)),
        Ok("reject") => Ok(None),
        _ => {
            anyhow::bail!("RIVER_UNDEFINED_FOUR_BAND_LAYOUT must be rgb, bgr, rgba, bgra or reject")
        }
    }
}
fn fallback(components: usize, space: u32) -> Result<(bool, Option<BandFallback>)> {
    let layout = match components {
        1 | 2 => BandFallback::Gray,
        3 if space == 17 => BandFallback::Gray,
        3 => BandFallback::Rgb,
        4 => {
            let layout = four_band_setting()?
                .context("four-band source lacks channel definitions and fallback is disabled")?;
            if space == 17 {
                BandFallback::Gray
            } else {
                layout
            }
        }
        _ => anyhow::bail!("multiband source lacks JP2 channel definitions"),
    };
    Ok((false, Some(layout)))
}

pub fn mapping(components: usize, container: &[u8]) -> Result<(bool, Option<BandFallback>)> {
    let header = boxes(container)
        .find(|(kind, _)| *kind == b"jp2h")
        .map_or(&[][..], |(_, v)| v);
    let mut colors = boxes(header).filter(|(kind, _)| *kind == b"colr");
    let color = colors.next().map(|(_, v)| v);
    let simple = colors.next().is_none()
        && color.is_none_or(|v| v.len() == 7 && v[0] == 1)
        && !boxes(header).any(|(kind, _)| kind == b"pclr" || kind == b"cmap");
    let space = color
        .filter(|v| v.len() == 7 && v[0] == 1)
        .map_or(0, |v| u32::from_be_bytes(v[3..7].try_into().unwrap()));
    let mut defs = boxes(header).filter(|(kind, _)| *kind == b"cdef");
    let table = defs.next().map(|(_, v)| v);
    if require_channel_labels()? {
        ensure!(
            boxes(header)
                .filter(|(kind, _)| *kind == b"cdef")
                .nth(1)
                .is_none(),
            "duplicate JP2 channel definition table"
        );
        check_explicit_labels(components, header, table)?;
    }
    if components < 4 {
        // Preserve existing decoders for declared colour spaces and palette transforms.
        if simple && matches!(space, 0 | 16 | 17) {
            if let Some(t) = table {
                if defs.next().is_none()
                    && t.len() == 2 + components * 6
                    && t[..2] == (components as u16).to_be_bytes()
                    && (0..components).all(|i| {
                        t[2..].chunks_exact(6).any(|e| {
                            e[..2] == (i as u16).to_be_bytes()
                                && (e[2..] == [255; 4] || e[2..] == [0, 0, 255, 255])
                        })
                    })
                {
                    return fallback(components, space);
                }
            } else if components == 2 && space == 16 {
                return fallback(components, space);
            }
        }
        return Ok((false, None));
    }
    ensure!(
        simple && matches!(space, 0 | 16 | 17),
        "unsupported multiband JP2 colour specification or component mapping"
    );
    let Some(table) = table else {
        return fallback(components, space);
    };
    let count = usize::from(u16::from_be_bytes(
        table
            .get(..2)
            .context("truncated channel definitions")?
            .try_into()?,
    ));
    ensure!(
        defs.next().is_none() && table.len() == 2 + 6 * count && count <= components,
        "invalid JP2 channel definition table"
    );
    let color_count = if space == 16 { 3 } else { 1 };
    let mut channels = 0u32;
    let mut associations = 0u32;
    let mut opacity = false;
    let mut unassociated = 0;
    for entry in table[2..].chunks_exact(6) {
        let channel = u16::from_be_bytes([entry[0], entry[1]]);
        let role = u16::from_be_bytes([entry[2], entry[3]]);
        let assoc = u16::from_be_bytes([entry[4], entry[5]]);
        ensure!(
            usize::from(channel) < components && channels & (1 << channel) == 0,
            "duplicate or out-of-range JP2 channel {channel}"
        );
        channels |= 1 << channel;
        match (role, assoc) {
            (0, a) if (1..=color_count).contains(&a) => {
                ensure!(
                    associations & (1 << a) == 0,
                    "duplicate JP2 colour association {a}"
                );
                associations |= 1 << a;
            }
            (1 | 2, 0) if !opacity => opacity = true,
            (65535, 65535) => (),
            // Old GDAL writers used type zero with no association for extra bands.
            (0, 65535) => unassociated += 1,
            _ => {
                anyhow::bail!("unsupported JP2 channel {channel}, type {role}, association {assoc}")
            }
        }
    }
    if count == components && !opacity && associations == 0 {
        return fallback(components, space);
    }
    ensure!(
        space != 0,
        "missing JP2 colour specification for declared channel associations"
    );
    let legacy_gray = space == 17
        && components == 4
        && count == 4
        && associations == 0
        && unassociated == 3
        && opacity
        && table[2..].chunks_exact(6).any(|e| e == [0, 3, 0, 1, 0, 0]);
    ensure!(
        legacy_gray || associations == (1 << (color_count + 1)) - 2,
        "missing JP2 display colour association"
    );
    Ok((space == 16 && !opacity, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparse_eight_band_rgb_uses_the_declared_planes() {
        let bytes = include_bytes!("../tests/fixtures/color-layout/sparse8.jp2");
        assert_eq!(mapping(8, bytes).unwrap(), (true, None));
        let header = boxes(bytes).find(|(kind, _)| *kind == b"jp2h").unwrap().1;
        let table = boxes(header).find(|(kind, _)| *kind == b"cdef").unwrap().1;
        assert!(check_explicit_labels(8, header, Some(table)).is_err());
        let image = jpeg2k::Image::from_bytes(bytes).unwrap();
        let jpeg2k::ImagePixelData::Rgb8(rgb) = image.get_pixels(None).unwrap().data else {
            panic!("expected opaque RGB");
        };
        let expected: Vec<_> = (0..=255u8)
            .flat_map(|n| [4, 2, 1].map(|channel| n.wrapping_add(channel * 29)))
            .collect();
        assert_eq!(rgb, expected);
    }
    #[test]
    fn sparse_rgb_allows_partial_auxiliary_labels_and_declared_opacity() {
        for (bytes, alpha) in [
            (include_bytes!("../tests/fixtures/color-layout/sparse8_aux.jp2").as_slice(), false),
            (include_bytes!("../tests/fixtures/color-layout/sparse8_alpha.jp2").as_slice(), true),
        ] {
            assert_eq!(mapping(8, bytes).unwrap(), (!alpha, None));
            let image = jpeg2k::Image::from_bytes(bytes).unwrap();
            let expected: Vec<_> = (0..=255u8).flat_map(|n| {
                let rgb = [4, 2, 1].map(|channel| n.wrapping_add(channel * 29));
                rgb.into_iter().chain(alpha.then_some(n))
            }).collect();
            match image.get_pixels(None).unwrap().data {
                jpeg2k::ImagePixelData::Rgb8(v) if !alpha => assert_eq!(v, expected),
                jpeg2k::ImagePixelData::Rgba8(v) if alpha => assert_eq!(v, expected),
                _ => panic!("unexpected display format"),
            }
        }
    }
    #[test]
    fn strict_mode_checks_every_channel_even_if_rgb_is_complete() {
        for components in 1..=4 {
            assert!(check_explicit_labels(components, &[], None).is_err());
            let mut table = (components as u16).to_be_bytes().to_vec();
            for index in 0..components {
                table.extend_from_slice(&(index as u16).to_be_bytes());
                let (role, assoc) = if index == 3 {
                    (1u16, 0u16)
                } else {
                    (0, (index + 1) as u16)
                };
                table.extend(role.to_be_bytes());
                table.extend(assoc.to_be_bytes());
            }
            assert!(check_explicit_labels(components, &[], Some(&table)).is_ok());
            let last = table.len() - 4;
            table[last..].fill(255);
            assert!(check_explicit_labels(components, &[], Some(&table)).is_err());
            table.truncate(last - 2);
            table[..2].copy_from_slice(&((components - 1) as u16).to_be_bytes());
            assert!(check_explicit_labels(components, &[], Some(&table)).is_err());
        }
    }
    #[test]
    fn legacy_and_current_gdal_gray_keep_declared_opacity() {
        for data in [
            include_bytes!("../tests/fixtures/color-layout/gdal_legacy.jp2").as_slice(),
            include_bytes!("../tests/fixtures/color-layout/gdal_fixed.jp2").as_slice(),
        ] {
            assert!(mapping(4, data).unwrap() == (false, None));
            let image = jpeg2k::Image::from_bytes(data).unwrap();
            match image.get_pixels(None).unwrap().data {
                jpeg2k::ImagePixelData::La8(pixels) => {
                    assert_eq!(pixels, [71, 133].repeat(96 * 64))
                }
                _ => panic!("expected grayscale with opacity"),
            }
        }
    }
    #[test]
    fn all_undefined_roles_use_the_selected_display_order() {
        let bytes = include_bytes!("../tests/fixtures/color-layout/undefined4.jp2");
        assert_eq!(
            mapping(4, bytes).ok().and_then(|m| m.1),
            four_band_setting().unwrap()
        );
        let image = jpeg2k::Image::from_bytes(bytes).unwrap();
        for (layout, pixel) in [
            (BandFallback::Rgb, &[17, 71, 193][..]),
            (BandFallback::Bgr, &[193, 71, 17]),
            (BandFallback::Rgba, &[17, 71, 193, 109]),
            (BandFallback::Bgra, &[193, 71, 17, 109]),
        ] {
            match image.get_display_pixels(None, Some(layout)).unwrap().data {
                jpeg2k::ImagePixelData::Rgb8(v) | jpeg2k::ImagePixelData::Rgba8(v) => {
                    assert_eq!(v, pixel.repeat(96 * 64))
                }
                _ => panic!("unexpected fallback format"),
            }
        }
    }
    #[test]
    fn four_unlabelled_components_use_the_configured_mapping() {
        assert_eq!(
            mapping(4, &[]).ok().and_then(|m| m.1),
            four_band_setting().unwrap()
        );
    }
}
