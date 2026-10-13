//! GIMP brushes: `.gbr` (one tip, grey or in colour) and `.gih` (an "image
//! hose": several tips the dabs pick from).
//! Both formats are documented by GIMP (developer.gimp.org, "GBR" and
//! "GIH"); this reads them from those descriptions.

use super::{Imported, Reader, tip_preset};
use crate::brush_engine::brush_options::{PixelBrushShape, TipOrder};
use crate::brush_engine::tip::TipMask;
use std::sync::Arc;

/// Largest tip side read.
const MAX_SIDE: u32 = 8192;

/// One `.gbr` brush: its name, tip and spacing (percent of its size).
pub(super) struct Gbr {
    pub name: String,
    pub tip: Arc<TipMask>,
    pub spacing: f32,
}

/// Read one `.gbr` brush from `r` (leaving `r` after it: `.gih` files hold
/// several in a row).
pub(super) fn read_gbr(r: &mut Reader<'_>) -> Result<Gbr, String> {
    let start = r.pos();
    let header_size = r.u32_be()? as usize;
    let version = r.u32_be()?;
    let width = r.u32_be()?;
    let height = r.u32_be()?;
    let bytes = r.u32_be()?;
    let (spacing, fixed) = match version {
        1 => (25.0, 20),
        2 | 3 => {
            if r.take(4)? != b"GIMP" {
                return Err("Not a GIMP brush".into());
            }
            (r.u32_be()? as f32, 28)
        }
        v => return Err(format!("GIMP brush version {v} isn't supported")),
    };
    if version == 3 {
        return Err("16-bit GIMP brushes (CinePaint) aren't supported".into());
    }
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err("GIMP brush has a bad size".into());
    }
    if !(bytes == 1 || bytes == 4) || header_size < fixed {
        return Err("Damaged GIMP brush".into());
    }
    let name_bytes = r.take(header_size - fixed)?;
    let name = String::from_utf8_lossy(name_bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    let _ = start;
    let (w, h) = (width as usize, height as usize);
    let data = r.take(w * h * bytes as usize)?;
    let tip = if bytes == 1 {
        // A mask: 255 paints fully.
        TipMask::from_mask(w, h, data.to_vec())
    } else {
        let px = data.as_chunks::<4>().0;
        let pixels = px.iter().map(|p| p[3]).collect();
        let colors = px.iter().map(|p| [p[0], p[1], p[2]]).collect();
        TipMask::from_colored(w, h, pixels, colors)
    };
    Ok(Gbr {
        name,
        tip,
        spacing: spacing.clamp(1.0, 1000.0),
    })
}

/// A GIMP pattern (`.pat`, as `.kpp` presets' textures are): its picture.
pub(super) fn read_pat(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut r = Reader::new(bytes);
    let header_size = r.u32_be()? as usize;
    let _version = r.u32_be()?;
    let width = r.u32_be()?;
    let height = r.u32_be()?;
    let channels = r.u32_be()? as usize;
    if r.take(4)? != b"GPAT" || header_size < 24 || !(1..=4).contains(&channels) {
        return Err("Not a GIMP pattern".into());
    }
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err("GIMP pattern has a bad size".into());
    }
    r.skip(header_size - 24)?;
    let data = r.take(width as usize * height as usize * channels)?;
    let rgba: Vec<u8> = data
        .chunks_exact(channels)
        .flat_map(|p| match channels {
            1 => [p[0], p[0], p[0], 255],
            2 => [p[0], p[0], p[0], p[1]],
            3 => [p[0], p[1], p[2], 255],
            _ => [p[0], p[1], p[2], p[3]],
        })
        .collect();
    let img = image::RgbaImage::from_raw(width, height, rgba).ok_or("Damaged GIMP pattern")?;
    Ok(img.into())
}

pub(super) fn import_gbr(bytes: &[u8], fallback: &str) -> Result<Imported, String> {
    let gbr = read_gbr(&mut Reader::new(bytes))?;
    let name = if gbr.name.is_empty() {
        fallback.to_string()
    } else {
        gbr.name
    };
    let mut out = Imported::default();
    out.presets.push(tip_preset(&name, gbr.tip, gbr.spacing));
    Ok(out)
}

/// A `.gih`: a name line, a line of settings, then the cells as `.gbr`s.
pub(super) fn import_gih(bytes: &[u8], fallback: &str) -> Result<Imported, String> {
    let mut r = Reader::new(bytes);
    let name = r.line()?.trim().to_string();
    let settings = r.line()?;
    let mut words = settings.split_whitespace();
    let count: usize = words
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or("Damaged GIMP image hose")?;
    let params: Vec<(&str, &str)> = words.filter_map(|w| w.split_once(':')).collect();
    let param = |key: &str| params.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    if count == 0 || count > 1024 {
        return Err("Damaged GIMP image hose".into());
    }
    let mut tips = Vec::with_capacity(count);
    let mut spacing = 25.0;
    for _ in 0..count {
        // Some files hold fewer cells than they say (Krita's own
        // fairy-dust.gih): keep the ones there are, as Krita does.
        let gbr = match read_gbr(&mut r) {
            Ok(gbr) => gbr,
            Err(_) if !tips.is_empty() => break,
            Err(err) => return Err(err),
        };
        spacing = gbr.spacing;
        tips.push(gbr.tip);
    }
    let mut out = Imported::default();
    let dims: usize = param("dim").and_then(|d| d.parse().ok()).unwrap_or(1);
    if dims > 1 {
        out.notes.push(format!(
            "{name}: its {dims} ways of picking tips became one (the first)"
        ));
    }
    let order = match param("sel0").unwrap_or("incremental") {
        "incremental" => TipOrder::Sequence,
        "random" => TipOrder::Random,
        "pressure" => TipOrder::Pressure,
        "angular" => TipOrder::Direction,
        other => {
            out.notes.push(format!(
                "{name}: tips picked by {other} are picked at random here"
            ));
            TipOrder::Random
        }
    };
    let name = if name.is_empty() {
        fallback.to_string()
    } else {
        name
    };
    let first = tips.remove(0);
    let mut preset = tip_preset(&name, first, spacing);
    let o = &mut preset.brush.brush_options;
    o.extra_tips = tips;
    o.tip_order = order;
    if !matches!(o.pixel_shape, PixelBrushShape::Custom(_)) {
        return Err("Damaged GIMP image hose".into());
    }
    out.presets.push(preset);
    Ok(out)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A version 2 `.gbr` of `w`×`h` with `bytes` per pixel.
    pub fn gbr(name: &str, w: u32, h: u32, bytes: u32, spacing: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let name = format!("{name}\0");
        for v in [28 + name.len() as u32, 2, w, h, bytes] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(b"GIMP");
        out.extend_from_slice(&spacing.to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(pixels);
        out
    }

    #[test]
    fn a_grey_gbr_becomes_a_tip_with_its_spacing() {
        let pixels: Vec<u8> = (0..16 * 8)
            .map(|i| if i % 16 < 8 { 255 } else { 0 })
            .collect();
        let bytes = gbr("Half", 16, 8, 1, 40, &pixels);
        let imported = import_gbr(&bytes, "file").unwrap();
        let p = &imported.presets[0];
        assert_eq!(p.name, "Half");
        assert_eq!(p.brush.brush_options.spacing, 40.0);
        let PixelBrushShape::Custom(tip) = &p.brush.brush_options.pixel_shape else {
            panic!("an image tip");
        };
        // The empty half is trimmed off.
        assert_eq!((tip.width, tip.height), (8, 8));
        assert!(!tip.has_colors());
    }

    #[test]
    fn a_colour_gbr_keeps_its_colours() {
        let pixels: Vec<u8> = (0..4 * 4).flat_map(|_| [200, 30, 40, 255]).collect();
        let imported = import_gbr(&gbr("Red", 4, 4, 4, 25, &pixels), "file").unwrap();
        let o = &imported.presets[0].brush.brush_options;
        assert!(o.tip_colors);
        let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
            panic!("an image tip");
        };
        assert_eq!(tip.colors.as_ref().unwrap()[0], [200, 30, 40]);
    }

    #[test]
    fn a_gih_becomes_a_brush_with_several_tips() {
        let mut bytes = b"Hose\n3 ncells:3 dim:1 rank0:3 sel0:random\n".to_vec();
        for k in 0..3u8 {
            let pixels: Vec<u8> = (0..8 * 8)
                .map(|i| if i as u8 % 8 == k { 255 } else { 0 })
                .collect();
            bytes.extend(gbr("cell", 8, 8, 1, 30, &pixels));
        }
        let imported = import_gih(&bytes, "file").unwrap();
        let o = &imported.presets[0].brush.brush_options;
        assert_eq!(imported.presets[0].name, "Hose");
        assert_eq!(o.tip_count(), 3);
        assert_eq!(o.tip_order, TipOrder::Random);
        assert!(imported.notes.is_empty());
    }

    #[test]
    fn a_gih_short_of_cells_keeps_the_ones_it_has() {
        let mut bytes = b"Hose\n4 ncells:4 dim:1 rank0:4 sel0:random\n".to_vec();
        bytes.extend(gbr("cell", 8, 8, 1, 30, &[255; 64]));
        let imported = import_gih(&bytes, "file").unwrap();
        assert_eq!(imported.presets[0].brush.brush_options.tip_count(), 1);
    }

    #[test]
    fn a_gimp_pattern_is_read() {
        let mut bytes = Vec::new();
        for v in [24 + 4u32, 1, 2, 1, 1] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        bytes.extend_from_slice(b"GPAT");
        bytes.extend_from_slice(b"pat\0");
        bytes.extend_from_slice(&[10, 200]);
        let img = read_pat(&bytes).unwrap().to_luma8();
        assert_eq!((img.width(), img.height()), (2, 1));
        assert_eq!(img.get_pixel(1, 0)[0], 200);
        assert!(read_pat(&bytes[..20]).is_err());
    }

    #[test]
    fn damaged_gimp_brushes_are_refused() {
        let pixels = vec![255u8; 64];
        let good = gbr("x", 8, 8, 1, 25, &pixels);
        for cut in [3, 20, 30, good.len() - 1] {
            assert!(import_gbr(&good[..cut], "f").is_err(), "cut at {cut}");
        }
        assert!(import_gih(b"Hose\n2 ncells:2\n", "f").is_err());
        assert!(import_gih(b"", "f").is_err());
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_gimp() {
        let pixels: Vec<u8> = (0..16 * 8 * 4).map(|i| i as u8).collect();
        let seed = gbr("Fuzz", 16, 8, 4, 25, &pixels);
        crate::fuzz::fuzz("gbr", &seed, std::time::Duration::from_secs(2), |b| {
            let _ = import_gbr(b, "fuzz");
        });
        // A pipe of two of them.
        let mut gih = b"Fuzz pipe\n2 ncells:2 cellwidth:16 cellheight:8 step:10 dim:1 cols:1 rows:1 placement:constant rank0:2 sel0:incremental\n".to_vec();
        gih.extend(gbr("a", 16, 8, 4, 25, &pixels));
        gih.extend(gbr("b", 16, 8, 4, 25, &pixels));
        crate::fuzz::fuzz("gih", &gih, std::time::Duration::from_secs(2), |b| {
            let _ = import_gih(b, "fuzz");
        });
    }
}
