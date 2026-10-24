//! Photoshop brushes (`.abr`). Adobe doesn't document the format; this
//! follows what GIMP reads (tips) and what brush tools have worked
//! out about the settings (the `desc` block, an "action descriptor").
//!
//! - Versions 1 and 2 (Photoshop 6 and older): a list of brushes, each a
//!   computed round tip (size, hardness, roundness, angle) or a sampled
//!   picture.
//! - Version 6 and later: `8BIM` blocks: `samp` holds the pictures (each
//!   named by an id), `patt` the textures' patterns, `desc` the presets
//!   (name, tip, size, spacing, the shape, scatter, transfer and colour
//!   dynamics, texture, dual brush, wet edges and the tool's flow, opacity
//!   and mode).

use super::{Imported, Reader, unpack_bits};
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::tip::TipMask;
use eframe::egui::Color32;
use std::sync::Arc;

/// Largest tip side read.
const MAX_SIDE: i32 = 8192;

pub(super) fn import(bytes: &[u8], stem: &str) -> Result<Imported, String> {
    let mut r = Reader::new(bytes);
    match r.u16_be()? {
        v @ (1 | 2) => old_versions(&mut r, v, stem),
        6..=10 => new_versions(&mut r, stem),
        v => Err(format!("Photoshop brush version {v} isn't supported")),
    }
}

/// Versions 1 and 2.
fn old_versions(r: &mut Reader<'_>, version: u16, stem: &str) -> Result<Imported, String> {
    let count = r.u16_be()?;
    let mut out = Imported::default();
    for i in 0..count {
        let kind = r.u16_be()?;
        let size = r.u32_be()? as usize;
        let end = r.pos() + size;
        let name = format!("{stem} {}", i + 1);
        match kind {
            1 => {
                let _misc = r.u32_be()?;
                let spacing = r.u16_be()? as f32;
                let diameter = r.u16_be()? as f32;
                let roundness = r.u16_be()? as f32;
                let angle = r.u16_be()? as i16 as f32;
                let hardness = r.u16_be()? as f32;
                let mut b = Brush::new(
                    diameter.max(1.0),
                    hardness,
                    Color32::BLACK,
                    spacing.max(1.0),
                );
                b.dynamics.tip.ratio = (roundness / 100.0).clamp(0.02, 1.0);
                b.dynamics.tip.angle = angle;
                out.presets.push(BrushPreset {
                    name,
                    brush: b,
                    file: None,
                });
            }
            2 => {
                let _misc = r.u32_be()?;
                let spacing = r.u16_be()? as f32;
                let name = if version == 2 {
                    let len = r.u32_be()? as usize;
                    utf16(r.take(len * 2)?)
                } else {
                    name
                };
                let _antialias = r.u8()?;
                r.skip(8)?; // the bounds as shorts
                let tip = read_sample(r)?;
                let mut preset = super::tip_preset(&name, tip, spacing.max(1.0));
                preset.brush.brush_options.tip_colors = false;
                out.presets.push(preset);
            }
            _ => out
                .notes
                .push(format!("Skipped a brush of an unknown kind ({kind})")),
        }
        r.seek(end)?;
    }
    Ok(out)
}

/// A sampled tip: its bounds, depth, compression and pixels.
fn read_sample(r: &mut Reader<'_>) -> Result<Arc<TipMask>, String> {
    let (top, left, bottom, right) = (r.i32_be()?, r.i32_be()?, r.i32_be()?, r.i32_be()?);
    let depth = r.u16_be()?;
    let compression = r.u8()?;
    // In i64: crafted bounds far apart would overflow i32.
    let (w, h) = (
        i64::from(right) - i64::from(left),
        i64::from(bottom) - i64::from(top),
    );
    if w <= 0 || h <= 0 || w > i64::from(MAX_SIDE) || h > i64::from(MAX_SIDE) {
        return Err("A Photoshop brush tip has a bad size".into());
    }
    let (w, h) = (w as usize, h as usize);
    let pixels = match (compression, depth) {
        (0, 8) => r.take(w * h)?.to_vec(),
        // 16-bit: the high bytes.
        (0, 16) => r
            .take(w * h * 2)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| p[0])
            .collect(),
        (1, 8) => {
            let lengths: Vec<usize> = (0..h)
                .map(|_| r.u16_be().map(usize::from))
                .collect::<Result<_, _>>()?;
            let mut out = Vec::with_capacity(w * h);
            for len in lengths {
                let before = out.len();
                unpack_bits(r, len, &mut out)?;
                out.resize(before + w, 0);
            }
            out
        }
        _ => {
            return Err(format!(
                "Photoshop tips of depth {depth} (compression {compression}) aren't supported"
            ));
        }
    };
    Ok(TipMask::from_mask(w, h, pixels))
}

/// The textures made so far for a file's presets: each pattern (by its
/// index), lightened or darkened and its contrast changed.
type Textures = Vec<(
    (usize, i32, i32),
    Arc<crate::brush_engine::texture::Pattern>,
)>;

/// A texture's pattern from the `patt` block: its id, name and grey.
struct AbrPattern {
    id: String,
    name: String,
    grey: image::GrayImage,
}

/// The patterns of a `patt` block (Photoshop's pattern format, each with
/// its length before it and padded to 4 bytes); a pattern that can't be
/// read is left out with a note.
fn read_patterns(block: &[u8], notes: &mut Vec<String>) -> Vec<AbrPattern> {
    let mut r = Reader::new(block);
    let mut out = Vec::new();
    while r.pos() + 4 <= r.len() {
        let Ok(len) = r.u32_be().map(|l| l as usize) else {
            break;
        };
        let Ok(body) = r.take(len) else {
            break;
        };
        match read_pattern(&mut Reader::new(body)) {
            Ok(p) => out.push(p),
            Err(err) => notes.push(err),
        }
        if r.seek(((r.pos() + 3) & !3).min(r.len())).is_err() {
            break;
        }
    }
    out
}

/// One pattern: version, colour mode, size, name, id, (a palette), then
/// its channels in a "virtual memory array list".
fn read_pattern(r: &mut Reader<'_>) -> Result<AbrPattern, String> {
    if r.u32_be()? != 1 {
        return Err("A Photoshop pattern of an unknown version was skipped".into());
    }
    let mode = r.u32_be()?;
    r.skip(4)?; // height, width (the array list has them)
    let name = unicode(r)?;
    let id_len = r.u8()? as usize;
    let id = String::from_utf8_lossy(r.take(id_len)?).into_owned();
    // Indexed colour: the palette (some writers add 4 bytes after it).
    let palette = if mode == 2 {
        let palette = r.take(768)?;
        let at = r.pos();
        if r.u32_be()? != 3 {
            r.seek(at + 4)?;
        } else {
            r.seek(at)?;
        }
        Some(palette)
    } else {
        None
    };
    if r.u32_be()? != 3 {
        return Err(format!("The pattern {name} couldn't be read"));
    }
    let len = r.u32_be()? as usize;
    let mut v = Reader::new(r.take(len)?);
    let (top, left, bottom, right) = (v.i32_be()?, v.i32_be()?, v.i32_be()?, v.i32_be()?);
    let (w, h) = (
        i64::from(right) - i64::from(left),
        i64::from(bottom) - i64::from(top),
    );
    if w <= 0 || h <= 0 || w > i64::from(MAX_SIDE) || h > i64::from(MAX_SIDE) {
        return Err(format!("The pattern {name} has a bad size"));
    }
    let (w, h) = (w as usize, h as usize);
    let count = v.u32_be()?;
    // Grey, indexed: one channel; RGB three (the rest: the first).
    let wanted = if mode == 3 { 3 } else { 1 };
    let mut planes: Vec<Vec<u8>> = Vec::new();
    for _ in 0..count.min(64) + 2 {
        if planes.len() == wanted || v.pos() + 8 > v.len() {
            break;
        }
        if v.u32_be()? == 0 {
            continue;
        }
        let array_len = v.u32_be()? as usize;
        if array_len == 0 {
            continue;
        }
        if array_len < 23 {
            return Err(format!("The pattern {name} is damaged"));
        }
        let _depth = v.u32_be()?;
        let (ct, cl, cb, cr) = (v.i32_be()?, v.i32_be()?, v.i32_be()?, v.i32_be()?);
        let depth = v.u16_be()?;
        let compression = v.u8()?;
        let mut d = Reader::new(v.take(array_len - 23)?);
        if (ct, cl, cb, cr) != (top, left, bottom, right) {
            return Err(format!("The pattern {name} has channels of another size"));
        }
        let plane = match (compression, depth) {
            (0, 8) => d.take(w * h)?.to_vec(),
            // 16-bit: the high bytes.
            (0, 16) => d
                .take(w * h * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| p[0])
                .collect(),
            (1, 8) => {
                let lengths: Vec<usize> = (0..h)
                    .map(|_| d.u16_be().map(usize::from))
                    .collect::<Result<_, _>>()?;
                let mut out = Vec::with_capacity(w * h);
                for len in lengths {
                    let before = out.len();
                    unpack_bits(&mut d, len, &mut out)?;
                    out.resize(before + w, 0);
                }
                out
            }
            _ => {
                return Err(format!(
                    "The pattern {name} is of depth {depth} (compression {compression}), which isn't supported"
                ));
            }
        };
        planes.push(plane);
    }
    if planes.len() < wanted {
        return Err(format!("The pattern {name} is missing channels"));
    }
    let grey: Vec<u8> = match (wanted, palette) {
        (3, _) => (0..w * h)
            .map(|i| {
                let (r, g, b) = (
                    planes[0][i] as u32,
                    planes[1][i] as u32,
                    planes[2][i] as u32,
                );
                ((r * 11 + g * 16 + b * 5) / 32) as u8
            })
            .collect(),
        (_, Some(palette)) => planes[0]
            .iter()
            .map(|&i| {
                let c = &palette[i as usize * 3..i as usize * 3 + 3];
                ((c[0] as u32 * 11 + c[1] as u32 * 16 + c[2] as u32 * 5) / 32) as u8
            })
            .collect(),
        _ => planes.swap_remove(0),
    };
    let grey = image::GrayImage::from_raw(w as u32, h as u32, grey)
        .ok_or_else(|| format!("The pattern {name} is damaged"))?;
    Ok(AbrPattern { id, name, grey })
}

/// Versions 6 and later: the `8BIM` blocks.
fn new_versions(r: &mut Reader<'_>, stem: &str) -> Result<Imported, String> {
    let subversion = r.u16_be()?;
    let mut samples: Vec<(String, Arc<TipMask>)> = Vec::new();
    let mut patterns: Vec<AbrPattern> = Vec::new();
    let mut desc = None;
    let mut out = Imported::default();
    while r.pos() + 12 <= r.len() {
        if r.take(4)? != b"8BIM" {
            break;
        }
        let key = r.take(4)?.to_vec();
        let len = r.u32_be()? as usize;
        let start = r.pos();
        let end = start + len;
        match &key[..] {
            b"samp" => {
                while r.pos() + 4 <= end {
                    let size = r.u32_be()? as usize;
                    let sample_end = r.pos() + size;
                    let id_len = r.u8()? as usize;
                    let id = String::from_utf8_lossy(r.take(id_len)?).into_owned();
                    // What comes between the id and the bounds differs by
                    // subversion (as GIMP reads it).
                    r.skip(if subversion == 1 { 10 } else { 264 })?;
                    match read_sample(r) {
                        Ok(tip) => samples.push((id, tip)),
                        Err(err) => out.notes.push(err),
                    }
                    // Samples are padded to 4 bytes.
                    r.seek(((sample_end + 3) & !3).min(end))?;
                }
            }
            b"patt" => patterns = read_patterns(r.take(len)?, &mut out.notes),
            b"desc" => {
                let mut d = Reader::new(r.take(len)?);
                // A version number, then the descriptor.
                let _ = d.u32_be();
                desc = descriptor(&mut d).ok();
            }
            _ => {}
        }
        // Blocks are padded to an even length (the last one may not be).
        r.seek(((end + 1) & !1).min(r.len()))?;
    }
    // The textures made so far, by pattern and brightness and contrast
    // (presets share them).
    let mut textures = Vec::new();
    let presets = desc
        .as_ref()
        .and_then(|d| d.get("Brsh"))
        .and_then(Value::list)
        .map(|list| {
            list.iter()
                .filter_map(Value::object)
                .filter_map(|p| preset_from(p, &samples, &patterns, &mut textures, &mut out.notes))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if presets.is_empty() {
        // No settings (or unreadable ones): the tips on their own.
        if desc.is_none() && !samples.is_empty() {
            out.notes.push(format!(
                "{stem}: only the tips were imported (no readable settings)"
            ));
        }
        for (i, (_, tip)) in samples.into_iter().enumerate() {
            let mut preset = super::tip_preset(&format!("{stem} {}", i + 1), tip, 25.0);
            preset.brush.brush_options.tip_colors = false;
            out.presets.push(preset);
        }
    } else {
        out.presets = presets;
    }
    Ok(out)
}

/// A sampled tip by its id in `samples`.
fn sample_of(samples: &[(String, Arc<TipMask>)], id: &str) -> Option<Arc<TipMask>> {
    (samples.iter())
        .find(|(s, _)| s.trim_start_matches('$') == id.trim_start_matches('$'))
        .map(|(_, t)| t.clone())
}

/// Which control drives a dynamic `k` of `o` (2: pen pressure, 6: the
/// stroke's direction, 8: the pen's rotation), and its jitter (0..1).
fn dynamic_of(o: &Object, k: &str) -> Option<(i32, f32)> {
    let o = o.get(k).and_then(Value::object)?;
    let control = o.get("bVTy").and_then(Value::number).unwrap_or(0.0) as i32;
    let jitter = o.get("jitter").and_then(Value::number).unwrap_or(0.0) as f32 / 100.0;
    Some((control, jitter))
}

/// A blend mode key: this app's texture mode and the formula for it
/// (Photoshop's own for the height modes; see [`KritaTexturing`]).
///
/// [`KritaTexturing`]: crate::brush_engine::texture::KritaTexturing
fn texture_mode(key: &str) -> (crate::brush_engine::texture::TextureMode, u8) {
    use crate::brush_engine::texture::TextureMode as M;
    match key {
        "Sbtr" => (M::Subtract, 1),
        "Drkn" => (M::Multiply, 4),
        "Ovrl" => (M::Multiply, 5),
        "CDdg" => (M::ColorDodge, 6),
        "CBrn" => (M::Subtract, 7),
        "linearBurn" => (M::Subtract, 9),
        "hardMix" => (M::HardMix, 10),
        "Hght" | "height" => (M::Height, 14),
        "linearHeight" => (M::Height, 15),
        _ => (M::Multiply, 0),
    }
}

/// The preset's texture: its pattern from `patterns`, made lighter or
/// darker and its contrast changed as the preset says, in its blend mode
/// at its depth. `None` when the pattern isn't in the file.
fn texture_of(
    p: &Object,
    patterns: &[AbrPattern],
    textures: &mut Textures,
) -> Option<crate::brush_engine::texture::BrushTexture> {
    use crate::brush_engine::texture::{BrushTexture, GrainPlacement, KritaTexturing};
    let num = |k: &str| p.get(k).and_then(Value::number);
    let flag = |k: &str| p.get(k).and_then(Value::boolean).unwrap_or(false);
    let t = p.get("Txtr").and_then(Value::object)?;
    let (id, name) = (
        t.get("Idnt").and_then(Value::text).unwrap_or(""),
        t.get("Nm  ").and_then(Value::text).unwrap_or(""),
    );
    let index = (patterns.iter().position(|q| !id.is_empty() && q.id == id)).or_else(|| {
        patterns
            .iter()
            .position(|q| !name.is_empty() && q.name == name)
    })?;
    let source = &patterns[index];
    // Photoshop's brightness -150..150 and contrast -50..100 (how far it
    // takes them isn't documented: contrast -50 flattens the pattern, 100
    // takes it to five times).
    let brightness = num("textureBrightness").unwrap_or(0.0).clamp(-150.0, 150.0) as i32;
    let contrast = num("textureContrast").unwrap_or(0.0).clamp(-50.0, 100.0) as i32;
    let key = (index, brightness, contrast);
    let pattern = match textures.iter().find(|(k, _)| *k == key) {
        Some((_, pattern)) => pattern.clone(),
        None => {
            let shift = brightness as f32 / 150.0;
            let gain = if contrast < 0 {
                1.0 + contrast as f32 / 50.0
            } else {
                1.0 + contrast as f32 / 25.0
            };
            let label = if (brightness, contrast) == (0, 0) {
                source.name.clone()
            } else {
                format!("{} ({brightness:+}, {contrast:+})", source.name)
            };
            let img = image::DynamicImage::ImageLuma8(source.grey.clone());
            let pattern =
                crate::brush_engine::texture::Pattern::from_image_with(&label, &img, |v| {
                    ((v + shift - 0.5) * gain + 0.5).clamp(0.0, 1.0)
                });
            textures.push((key, pattern.clone()));
            pattern
        }
    };
    let (mode, formula) = texture_mode(
        p.get("textureBlendMode")
            .and_then(Value::text)
            .unwrap_or(""),
    );
    // The pattern at its own pixels, however it was resampled.
    let side = source.grey.width().max(source.grey.height()) as f32;
    let scale = num("textureScale").unwrap_or(100.0) as f32 / 100.0 * side / pattern.size as f32;
    Some(BrushTexture {
        pattern,
        mode,
        scale: scale.clamp(0.05, 16.0),
        strength: (num("textureDepth").unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0),
        invert: flag("InvT"),
        placement: GrainPlacement {
            per_dab: flag("TxtC"),
            ..Default::default()
        },
        // The soft formulas: like Photoshop's depth, none leaves the paint
        // as it is (the plain ones take it all away at none).
        krita: Some(KritaTexturing {
            mode: formula,
            soft: true,
        }),
    })
}

/// The preset's dual brush, its tip and size taken from the main tip's
/// `diameter`; `None` when it has none.
fn dual_of(
    p: &Object,
    samples: &[(String, Arc<TipMask>)],
    diameter: f32,
    name: &str,
    notes: &mut Vec<String>,
) -> Option<crate::brush_engine::dual::DualTip> {
    use crate::brush_engine::dual::{DualMode, DualTip};
    let db = p.get("dualBrush").and_then(Value::object)?;
    if db.get("useDualBrush").and_then(Value::boolean) != Some(true)
        && p.get("useDualBrush").and_then(Value::boolean) != Some(true)
    {
        return None;
    }
    let num = |o: &Object, k: &str| o.get(k).and_then(Value::number);
    let tip = db.get("Brsh").and_then(Value::object);
    let shape = match tip.and_then(|t| t.get("sampledData")).and_then(Value::text) {
        Some(id) => match sample_of(samples, id) {
            Some(t) => PixelBrushShape::Custom(t),
            None => {
                notes.push(format!("{name}: its dual brush's tip picture is missing"));
                PixelBrushShape::Circle
            }
        },
        None => PixelBrushShape::Circle,
    };
    let own = |k: &str| tip.and_then(|t| num(t, k));
    Some(DualTip {
        shape,
        size: (own("Dmtr").unwrap_or(diameter as f64) as f32 / diameter).clamp(0.05, 20.0),
        hardness: own("Hrdn").unwrap_or(100.0).clamp(0.0, 100.0) as f32,
        // The tip's own spacing is the one Photoshop shows (the one beside
        // it reads 100 whatever the panel says).
        spacing: own("Spcn")
            .or(num(db, "Spcn"))
            .unwrap_or(25.0)
            .clamp(1.0, 1000.0) as f32,
        // A share of the size either way, as the main tip's scatter.
        scatter: dynamic_of(db, "scatterDynamics")
            .map_or(0.0, |(_, j)| (j * 50.0).clamp(0.0, 500.0)),
        count: num(db, "Cnt ").unwrap_or(1.0).clamp(1.0, 16.0) as u32,
        random_angle: false,
        mode: match db.get("BlnM").and_then(Value::text).unwrap_or("") {
            "Drkn" => DualMode::Darken,
            "Sbtr" | "linearBurn" => DualMode::Subtract,
            "Hght" | "height" | "linearHeight" => DualMode::Height,
            "CBrn" => DualMode::Burn,
            _ => DualMode::Multiply,
        },
    })
}

/// The tool's blend mode key as a layer blend; `None` for one with no
/// counterpart here.
fn paint_blend(key: &str) -> Option<crate::canvas::blend_modes::LayerBlend> {
    use crate::canvas::blend_modes::LayerBlend as L;
    Some(match key {
        "Nrml" => L::Normal,
        "Dslv" => L::Dissolve,
        "Drkn" => L::Darken,
        "Mltp" => L::Multiply,
        "CBrn" => L::ColorBurn,
        "linearBurn" => L::LinearBurn,
        "darkerColor" => L::DarkerColor,
        "Lghn" => L::Lighten,
        "Scrn" => L::Screen,
        "CDdg" => L::ColorDodge,
        "linearDodge" => L::LinearDodge,
        "lighterColor" => L::LighterColor,
        "Ovrl" => L::Overlay,
        "SftL" => L::SoftLight,
        "HrdL" => L::HardLight,
        "vividLight" => L::VividLight,
        "linearLight" => L::LinearLight,
        "pinLight" => L::PinLight,
        "hardMix" => L::HardMix,
        "Dfrn" => L::Difference,
        "Xclu" => L::Exclusion,
        "blendSubtraction" => L::Subtract,
        "blendDivide" => L::Divide,
        "H   " => L::Hue,
        "Strt" => L::Saturation,
        "Clr " => L::Color,
        "Lmns" => L::Luminosity,
        _ => return None,
    })
}

/// A preset from one entry of `desc`'s brush list.
fn preset_from(
    p: &Object,
    samples: &[(String, Arc<TipMask>)],
    patterns: &[AbrPattern],
    textures: &mut Textures,
    notes: &mut Vec<String>,
) -> Option<BrushPreset> {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
    let name = p
        .get("Nm  ")
        .and_then(Value::text)
        .unwrap_or("Brush")
        .to_string();
    let tip = p.get("Brsh").and_then(Value::object)?;
    let num = |o: &Object, k: &str| o.get(k).and_then(Value::number);
    let diameter = num(tip, "Dmtr").unwrap_or(25.0).clamp(1.0, 3000.0) as f32;
    let spacing = num(tip, "Spcn").unwrap_or(25.0).clamp(1.0, 1000.0) as f32;
    let hardness = num(tip, "Hrdn").unwrap_or(100.0).clamp(0.0, 100.0) as f32;
    let mut b = Brush::new(diameter, hardness, Color32::BLACK, spacing);
    b.brush_options.pressure_size = false;
    b.dynamics.tip.angle = num(tip, "Angl").unwrap_or(0.0) as f32;
    b.dynamics.tip.ratio = (num(tip, "Rndn").unwrap_or(100.0) as f32 / 100.0).clamp(0.02, 1.0);
    if let Some(id) = tip.get("sampledData").and_then(Value::text) {
        match sample_of(samples, id) {
            Some(t) => b.brush_options.pixel_shape = PixelBrushShape::Custom(t),
            None => notes.push(format!("{name}: its tip picture is missing")),
        }
    }
    let flag = |k: &str| p.get(k).and_then(Value::boolean).unwrap_or(false);
    let dynamic = |k: &str| dynamic_of(p, k);
    // A pen input on a setting: `falling`, a light touch moves it all the
    // way (a heavy one leaves it).
    let input = |sensor: Sensor, setting: DabSetting, amount: f32, falling: bool| InputMapping {
        sensor,
        setting,
        amount,
        curve: SoftnessCurve {
            points: if falling {
                vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 0.0)]
            } else {
                vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
            },
        },
        ..Default::default()
    };
    if flag("useTipDynamics") {
        if let Some((control, jitter)) = dynamic("szVr") {
            b.brush_options.pressure_size = control == 2;
            b.dynamics.random.size = jitter.clamp(0.0, 1.0);
        }
        b.brush_options.pressure_min_size =
            (num(p, "minimumDiameter").unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0);
        if let Some((control, jitter)) = dynamic("angleDynamics") {
            b.dynamics.tip.random_angle = (jitter * 180.0).clamp(0.0, 180.0);
            // The stroke's direction (or its first), the pen's rotation.
            b.dynamics.tip.follow_stroke = matches!(control, 5 | 6);
            b.dynamics.tip.follow_barrel = control == 8;
        }
        // Roundness by pressure: a light touch flattens the tip down to
        // the minimum roundness.
        if let Some((2, _)) = dynamic("roundnessDynamics") {
            let least = (num(p, "minimumRoundness").unwrap_or(1.0) as f32 / 100.0).clamp(0.0, 1.0);
            b.inputs.push(input(
                Sensor::Pressure,
                DabSetting::Squash,
                1.0 - least,
                true,
            ));
        }
    }
    if flag("useScatter") {
        if let Some((_, jitter)) = dynamic("scatterDynamics") {
            // Photoshop's scatter is a share of the size either way.
            b.jitter = (jitter * 50.0).clamp(0.0, 500.0);
        }
        if let Some(count) = num(p, "Cnt ") {
            b.dynamics.random.count = (count as u32).clamp(1, 16);
        }
    }
    if flag("usePaintDynamics") {
        if let Some((control, jitter)) = dynamic("opVr") {
            b.brush_options.pressure_opacity = control == 2;
            b.dynamics.random.opacity = jitter.clamp(0.0, 1.0);
        }
        if let Some((control, jitter)) = dynamic("prVr") {
            b.brush_options.pressure_flow = control == 2;
            if jitter > 0.0 {
                b.inputs.push(input(
                    Sensor::RandomDab,
                    DabSetting::Flow,
                    jitter.min(1.0),
                    false,
                ));
            }
        }
    }
    if flag("useTexture") {
        match texture_of(p, patterns, textures) {
            Some(texture) => b.texture = Some(texture),
            None => notes.push(format!("{name}: its texture's pattern isn't in the file")),
        }
    }
    b.dual = dual_of(p, samples, diameter, &name, notes);
    if flag("useColorDynamics") {
        let r = &mut b.dynamics.random;
        // Hue jitter 100% reaches any hue.
        r.hue = (num(p, "H   ").unwrap_or(0.0) as f32 / 100.0 * 180.0).clamp(0.0, 180.0);
        r.saturation = (num(p, "Strt").unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0);
        r.value = (num(p, "Brgh").unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0);
        r.purity = (num(p, "purity").unwrap_or(0.0) as f32 / 100.0).clamp(-1.0, 1.0);
        // Foreground to background: the secondary colour here, all of it
        // at the lightest touch.
        if let Some((control, jitter)) = dynamic("clVr") {
            if control == 2 {
                b.inputs
                    .push(input(Sensor::Pressure, DabSetting::ColorMix, 1.0, true));
            }
            if jitter > 0.0 {
                b.inputs.push(input(
                    Sensor::RandomDab,
                    DabSetting::ColorMix,
                    jitter.min(1.0),
                    false,
                ));
            }
        }
    }
    if flag("Wtdg") {
        // Photoshop's wet edges leave about half the paint in the middle.
        b.wet_edge = 0.5;
    }
    // The tool's settings saved with the preset.
    if let Some(to) = p.get("toolOptions").and_then(Value::object) {
        if let Some(flow) = num(to, "flow") {
            b.brush_options.flow = (flow as f32).clamp(1.0, 100.0);
        }
        if let Some(opacity) = num(to, "Opct") {
            b.brush_options.opacity = (opacity as f32 / 100.0).clamp(0.01, 1.0);
        }
        if let Some(mode) = to.get("Md  ").and_then(Value::text) {
            match paint_blend(mode) {
                Some(blend) => b.paint_blend = blend,
                None => notes.push(format!("{name}: its blend mode ({mode}) wasn't imported")),
            }
        }
        if to
            .get("usePressureOverridesOpacity")
            .and_then(Value::boolean)
            == Some(true)
        {
            b.brush_options.pressure_opacity = true;
        }
        if to.get("usePressureOverridesSize").and_then(Value::boolean) == Some(true) {
            b.brush_options.pressure_size = true;
        }
    }
    Some(BrushPreset {
        name,
        brush: b,
        file: None,
    })
}

/// An action descriptor's values.
#[derive(Debug)]
pub(super) enum Value {
    Object(Object),
    List(Vec<Value>),
    Number(f64),
    Bool(bool),
    Text(String),
    Other,
}

pub(super) type Object = Vec<(String, Value)>;

pub(super) trait Get {
    fn get(&self, key: &str) -> Option<&Value>;
}

impl Get for Object {
    fn get(&self, key: &str) -> Option<&Value> {
        self.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl Value {
    pub(super) fn object(&self) -> Option<&Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    pub(super) fn list(&self) -> Option<&Vec<Value>> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub(super) fn number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    fn boolean(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

fn unicode(r: &mut Reader<'_>) -> Result<String, String> {
    let len = r.u32_be()? as usize;
    if len > 1 << 20 {
        return Err("Damaged descriptor".into());
    }
    Ok(utf16(r.take(len * 2)?))
}

/// A key: 4 characters, or a longer name.
fn key(r: &mut Reader<'_>) -> Result<String, String> {
    let len = r.u32_be()? as usize;
    let len = if len == 0 { 4 } else { len };
    if len > 1 << 16 {
        return Err("Damaged descriptor".into());
    }
    Ok(String::from_utf8_lossy(r.take(len)?).into_owned())
}

pub(super) fn descriptor(r: &mut Reader<'_>) -> Result<Object, String> {
    let _name = unicode(r)?;
    let _class = key(r)?;
    let count = r.u32_be()?;
    if count > 1 << 16 {
        return Err("Damaged descriptor".into());
    }
    (0..count).map(|_| Ok((key(r)?, value(r)?))).collect()
}

fn value(r: &mut Reader<'_>) -> Result<Value, String> {
    let kind = r.take(4)?;
    Ok(match kind {
        b"Objc" | b"GlbO" => Value::Object(descriptor(r)?),
        b"VlLs" => {
            let count = r.u32_be()?;
            if count > 1 << 16 {
                return Err("Damaged descriptor".into());
            }
            Value::List((0..count).map(|_| value(r)).collect::<Result<_, _>>()?)
        }
        b"doub" => Value::Number(r.f64_be()?),
        b"UntF" => {
            r.skip(4)?;
            Value::Number(r.f64_be()?)
        }
        b"UnFl" => {
            r.skip(4)?;
            let count = r.u32_be()? as usize;
            let first = r.f64_be()?;
            r.skip(count.saturating_sub(1) * 8)?;
            Value::Number(first)
        }
        b"long" => Value::Number(r.i32_be()? as f64),
        b"comp" => {
            let v = i64::from_be_bytes(r.take(8)?.try_into().expect("8 bytes"));
            Value::Number(v as f64)
        }
        b"bool" => Value::Bool(r.u8()? != 0),
        b"TEXT" => Value::Text(unicode(r)?),
        b"enum" => {
            let _type = key(r)?;
            Value::Text(key(r)?)
        }
        b"type" | b"GlbC" => {
            let _name = unicode(r)?;
            let _class = key(r)?;
            Value::Other
        }
        b"tdta" | b"alis" => {
            let len = r.u32_be()? as usize;
            r.skip(len)?;
            Value::Other
        }
        other => {
            return Err(format!(
                "Unsupported descriptor value {}",
                String::from_utf8_lossy(other)
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_key(out: &mut Vec<u8>, k: &str) {
        if k.len() == 4 {
            out.extend_from_slice(&0u32.to_be_bytes());
        } else {
            out.extend_from_slice(&(k.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(k.as_bytes());
    }

    fn put_text(out: &mut Vec<u8>, t: &str) {
        let units: Vec<u16> = t.encode_utf16().chain([0]).collect();
        out.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for u in units {
            out.extend_from_slice(&u.to_be_bytes());
        }
    }

    fn put_untf(out: &mut Vec<u8>, k: &str, v: f64) {
        put_key(out, k);
        out.extend_from_slice(b"UntF#Pxl");
        out.extend_from_slice(&v.to_be_bytes());
    }

    fn sample(id: &str, w: i32, h: i32) -> Vec<u8> {
        let mut s = Vec::new();
        s.push(id.len() as u8);
        s.extend_from_slice(id.as_bytes());
        s.extend_from_slice(&[0; 10]);
        for v in [0, 0, h, w] {
            s.extend_from_slice(&v.to_be_bytes());
        }
        s.extend_from_slice(&8u16.to_be_bytes());
        s.push(0);
        s.extend((0..w * h).map(|i| if i % 2 == 0 { 255 } else { 0 }));
        s
    }

    /// A version 6 file: one 20×10 sample, and a preset using it at size
    /// 60 with pressure on size and 30% scatter.
    pub fn abr6() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&6u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        let s = sample("$abc", 20, 10);
        let mut samp = (s.len() as u32).to_be_bytes().to_vec();
        samp.extend_from_slice(&s);
        while !samp.len().is_multiple_of(4) {
            samp.push(0);
        }
        out.extend_from_slice(b"8BIMsamp");
        out.extend_from_slice(&(samp.len() as u32).to_be_bytes());
        out.extend_from_slice(&samp);
        // desc: { Brsh: [ { Nm: "Chalk", Brsh: {Dmtr 60, Spcn 30,
        // sampledData "$abc"}, useTipDynamics, szVr: {bVTy 2},
        // useScatter, scatterDynamics: {jitter 30} } ] }
        let mut d = 16u32.to_be_bytes().to_vec();
        put_text(&mut d, "");
        put_key(&mut d, "null");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_key(&mut d, "Brsh");
        d.extend_from_slice(b"VlLs");
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brushPreset");
        d.extend_from_slice(&6u32.to_be_bytes());
        put_key(&mut d, "Nm  ");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "Chalk");
        put_key(&mut d, "Brsh");
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "sampledBrush");
        d.extend_from_slice(&3u32.to_be_bytes());
        put_untf(&mut d, "Dmtr", 60.0);
        put_untf(&mut d, "Spcn", 30.0);
        put_key(&mut d, "sampledData");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "$abc");
        put_key(&mut d, "useTipDynamics");
        d.extend_from_slice(b"bool\x01");
        put_key(&mut d, "szVr");
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brVr");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_key(&mut d, "bVTy");
        d.extend_from_slice(b"long");
        d.extend_from_slice(&2i32.to_be_bytes());
        put_key(&mut d, "useScatter");
        d.extend_from_slice(b"bool\x01");
        put_key(&mut d, "scatterDynamics");
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brVr");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_untf(&mut d, "jitter", 30.0);
        out.extend_from_slice(b"8BIMdesc");
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        out.extend_from_slice(&d);
        out
    }

    #[test]
    fn a_version_6_brush_brings_its_tip_and_settings() {
        let imported = import(&abr6(), "set").unwrap();
        assert_eq!(imported.presets.len(), 1, "{:?}", imported.notes);
        let p = &imported.presets[0];
        assert_eq!(p.name, "Chalk");
        let o = &p.brush.brush_options;
        assert_eq!((o.diameter, o.spacing), (60.0, 30.0));
        assert!(o.pressure_size);
        assert!((p.brush.jitter - 15.0).abs() < 1e-3);
        let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
            panic!("the sampled tip");
        };
        assert_eq!((tip.width, tip.height), (19, 10));
    }

    fn put_bool(out: &mut Vec<u8>, k: &str) {
        put_key(out, k);
        out.extend_from_slice(b"bool\x01");
    }

    fn put_enum(out: &mut Vec<u8>, k: &str, v: &str) {
        put_key(out, k);
        out.extend_from_slice(b"enum");
        put_key(out, "BlnM");
        put_key(out, v);
    }

    /// An object value of `count` items, which the caller then writes.
    fn put_object(out: &mut Vec<u8>, k: &str, count: u32) {
        put_key(out, k);
        out.extend_from_slice(b"Objc");
        put_text(out, "");
        put_key(out, "null");
        out.extend_from_slice(&count.to_be_bytes());
    }

    /// An 8BIM block.
    fn put_block(out: &mut Vec<u8>, key: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(b"8BIM");
        out.extend_from_slice(key);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
    }

    /// A version 6 file: the 20×10 sample, a 4×4 grey pattern (a ramp)
    /// and a preset with a texture, a dual brush, colour dynamics and the
    /// tool's flow, opacity and mode.
    fn abr6_textured() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&6u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        let s = sample("$abc", 20, 10);
        let mut samp = (s.len() as u32).to_be_bytes().to_vec();
        samp.extend_from_slice(&s);
        while !samp.len().is_multiple_of(4) {
            samp.push(0);
        }
        put_block(&mut out, b"samp", &samp);
        // The pattern: grey, raw, one channel written of three slots.
        let mut pat = Vec::new();
        pat.extend_from_slice(&1u32.to_be_bytes());
        pat.extend_from_slice(&1u32.to_be_bytes());
        pat.extend_from_slice(&[0, 4, 0, 4]);
        put_text(&mut pat, "Paper");
        pat.push(4);
        pat.extend_from_slice(b"pat1");
        let mut vma = Vec::new();
        for v in [0i32, 0, 4, 4] {
            vma.extend_from_slice(&v.to_be_bytes());
        }
        vma.extend_from_slice(&1u32.to_be_bytes());
        vma.extend_from_slice(&1u32.to_be_bytes());
        vma.extend_from_slice(&(23u32 + 16).to_be_bytes());
        vma.extend_from_slice(&8u32.to_be_bytes());
        for v in [0i32, 0, 4, 4] {
            vma.extend_from_slice(&v.to_be_bytes());
        }
        vma.extend_from_slice(&8u16.to_be_bytes());
        vma.push(0);
        vma.extend((0..16u8).map(|i| i * 16));
        vma.extend_from_slice(&[0; 8]);
        pat.extend_from_slice(&3u32.to_be_bytes());
        pat.extend_from_slice(&(vma.len() as u32).to_be_bytes());
        pat.extend_from_slice(&vma);
        let mut patt = (pat.len() as u32).to_be_bytes().to_vec();
        patt.extend_from_slice(&pat);
        while !patt.len().is_multiple_of(4) {
            patt.push(0);
        }
        put_block(&mut out, b"patt", &patt);
        let mut d = 16u32.to_be_bytes().to_vec();
        put_text(&mut d, "");
        put_key(&mut d, "null");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_key(&mut d, "Brsh");
        d.extend_from_slice(b"VlLs");
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brushPreset");
        d.extend_from_slice(&11u32.to_be_bytes());
        put_key(&mut d, "Nm  ");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "Wash");
        put_object(&mut d, "Brsh", 2);
        put_untf(&mut d, "Dmtr", 60.0);
        put_untf(&mut d, "Hrdn", 0.0);
        put_bool(&mut d, "useTexture");
        put_object(&mut d, "Txtr", 1);
        put_key(&mut d, "Idnt");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "pat1");
        put_untf(&mut d, "textureDepth", 40.0);
        put_enum(&mut d, "textureBlendMode", "Hght");
        put_object(&mut d, "dualBrush", 4);
        put_bool(&mut d, "useDualBrush");
        put_object(&mut d, "Brsh", 3);
        put_untf(&mut d, "Dmtr", 30.0);
        put_untf(&mut d, "Spcn", 50.0);
        put_key(&mut d, "sampledData");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "$abc");
        put_enum(&mut d, "BlnM", "linearHeight");
        put_object(&mut d, "scatterDynamics", 1);
        put_untf(&mut d, "jitter", 20.0);
        put_bool(&mut d, "useColorDynamics");
        put_untf(&mut d, "H   ", 10.0);
        put_object(&mut d, "clVr", 1);
        put_untf(&mut d, "bVTy", 2.0);
        put_object(&mut d, "toolOptions", 3);
        put_untf(&mut d, "flow", 70.0);
        put_untf(&mut d, "Opct", 80.0);
        put_enum(&mut d, "Md  ", "Drkn");
        put_block(&mut out, b"desc", &d);
        out
    }

    #[test]
    fn a_brush_brings_its_texture_dual_brush_colour_dynamics_and_tool_settings() {
        use crate::brush_engine::dual::DualMode;
        use crate::brush_engine::dynamics::{DabSetting, Sensor};
        let imported = import(&abr6_textured(), "set").unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        let b = &imported.presets[0].brush;
        let t = b.texture.as_ref().expect("the texture");
        assert!(t.pattern.name == "Paper" && (t.strength - 0.4).abs() < 1e-6);
        // Photoshop's height formula, the soft way (none leaves the paint).
        assert_eq!(t.krita.map(|k| (k.mode, k.soft)), Some((14, true)));
        // The ramp kept as it is (dark to light), at its own 4 px.
        let d = t.pattern.data();
        assert!(
            d[0] < 0.1 && d[d.len() - 1] > 0.8,
            "{} {}",
            d[0],
            d[d.len() - 1]
        );
        assert!((t.scale - 4.0 / t.pattern.size as f32).abs() < 1e-6);
        let dual = b.dual.as_ref().expect("the dual brush");
        assert!(matches!(dual.shape, PixelBrushShape::Custom(_)));
        assert_eq!(
            (dual.size, dual.spacing, dual.mode),
            (0.5, 50.0, DualMode::Height)
        );
        assert!((dual.scatter - 10.0).abs() < 1e-3);
        assert!((b.dynamics.random.hue - 18.0).abs() < 1e-3);
        // A light touch paints the secondary colour.
        let mix = b
            .inputs
            .iter()
            .find(|m| m.setting == DabSetting::ColorMix)
            .unwrap();
        assert_eq!(mix.sensor, Sensor::Pressure);
        assert!(mix.curve.points[0].y == 1.0 && mix.curve.points[1].y == 0.0);
        let o = &b.brush_options;
        assert_eq!((o.flow, o.opacity), (70.0, 0.8));
        assert_eq!(
            b.paint_blend,
            crate::canvas::blend_modes::LayerBlend::Darken
        );
    }

    #[test]
    fn a_version_1_file_brings_computed_and_sampled_brushes() {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        // Computed: size 40, roundness 50, angle 30, hardness 80, spacing 20.
        let mut c = 0u32.to_be_bytes().to_vec();
        for v in [20u16, 40, 50, 30, 80] {
            c.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(c.len() as u32).to_be_bytes());
        out.extend_from_slice(&c);
        // Sampled, 8×8, run-length encoded rows of full paint.
        let mut s = 0u32.to_be_bytes().to_vec();
        s.extend_from_slice(&25u16.to_be_bytes());
        s.push(1);
        s.extend_from_slice(&[0; 8]);
        for v in [0i32, 0, 8, 8] {
            s.extend_from_slice(&v.to_be_bytes());
        }
        s.extend_from_slice(&8u16.to_be_bytes());
        s.push(1);
        for _ in 0..8 {
            s.extend_from_slice(&2u16.to_be_bytes());
        }
        for _ in 0..8 {
            s.extend_from_slice(&[0xF9, 255]);
        }
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        out.extend_from_slice(&s);
        let imported = import(&out, "old").unwrap();
        assert_eq!(imported.presets.len(), 2);
        let computed = &imported.presets[0].brush;
        assert_eq!(computed.brush_options.diameter, 40.0);
        assert_eq!(computed.dynamics.tip.ratio, 0.5);
        let PixelBrushShape::Custom(tip) = &imported.presets[1].brush.brush_options.pixel_shape
        else {
            panic!("a sampled tip");
        };
        assert_eq!((tip.width, tip.height), (8, 8));
        assert!(tip.pixels().iter().all(|&p| p == 255));
    }

    #[test]
    fn damaged_photoshop_brushes_are_refused_not_panicked_on() {
        for good in [abr6(), abr6_textured()] {
            for cut in (0..good.len()).step_by(7) {
                let _ = import(&good[..cut], "cut");
            }
        }
        assert!(import(&[0, 99], "bad").is_err());
    }

    /// Bounds whose difference overflows i32 (found by `fuzz_abr`).
    #[test]
    fn a_tip_with_bounds_too_far_apart_is_refused() {
        let mut bytes = Vec::new();
        for v in [i32::MIN, i32::MIN, i32::MAX, i32::MAX] {
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.push(0);
        assert!(read_sample(&mut Reader::new(&bytes)).is_err());
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_abr() {
        crate::fuzz::fuzz("abr", &abr6(), std::time::Duration::from_secs(2), |b| {
            let _ = import(b, "fuzz");
        });
    }
}
