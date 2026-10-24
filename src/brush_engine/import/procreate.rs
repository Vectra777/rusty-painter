//! Procreate brushes: a `.brushset` (a zip of brushes, each in a folder of
//! its own) or a single `.brush` (one brush's folder, zipped).
//!
//! A brush's folder holds `Brush.archive`, its settings as a binary
//! property list written by Apple's `NSKeyedArchiver`, and its own shape
//! and grain pictures (`Shape.png`, `Grain.png`: white paints). A `Sub01`
//! folder inside is the second brush of a dual brush. A brush using one of
//! Procreate's built-in shapes or grains names it (`bundledShapePath`)
//! without the picture, which only Procreate has.
//!
//! The settings are fractions with no documented units; the sizes,
//! spacing, jitter and taper lengths here are matched by eye.

use super::{Imported, Reader};
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::brush_options::{PaintingMode, PixelBrushShape};
use crate::brush_engine::tip::TipMask;
use crate::canvas::blend_modes::LayerBlend;
use eframe::egui::Color32;

/// Canvas pixels for a size of 1 (a brush's size is a fraction).
const SIZE_UNIT: f32 = 400.0;

pub(super) fn import(bytes: &[u8]) -> Result<Imported, String> {
    let entries = crate::project::zip::read_all(bytes)?;
    let file = |path: &str| {
        entries
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, data)| &data[..])
    };
    let mut out = Imported::default();
    let (mut no_shape, mut no_grain, mut wet_mix) = (0, 0, 0);
    // Every brush folder (a `.brush` has its archive at the top).
    for (name, data) in &entries {
        let Some(dir) = name.strip_suffix("Brush.archive") else {
            continue;
        };
        if dir.ends_with("Sub01/") {
            continue;
        }
        let main = match Settings::read(data) {
            Ok(s) => s,
            Err(e) => {
                out.notes.push(format!("{name}: {e}"));
                continue;
            }
        };
        // The brush's own picture, else the built-in one it names (when
        // the user has given us Procreate's: see `bundled_image`).
        let picture = |dir: &str, which: &str, s: &Settings, bundled: &str| {
            file(&format!("{dir}{which}"))
                .and_then(|b| image::load_from_memory(b).ok())
                .or_else(|| bundled_image(s.text(bundled)))
        };
        let shape = picture(dir, "Shape.png", &main, "bundledShapePath");
        let mut b = brush_from(&main, shape.as_ref(), &mut no_shape);
        let grain = picture(dir, "Grain.png", &main, "bundledGrainPath");
        b.texture = grain_of(&main, grain.as_ref(), &mut no_grain);
        let sub_dir = format!("{dir}Sub01/");
        if let Some(sub) =
            file(&format!("{sub_dir}Brush.archive")).and_then(|d| Settings::read(d).ok())
        {
            // A dual brush's missing shape counted with its brush's.
            let shape = picture(&sub_dir, "Shape.png", &sub, "bundledShapePath");
            let tip = brush_from(&sub, shape.as_ref(), &mut 0);
            b.dual = Some(dual_of(&main, &sub, &tip, b.brush_options.diameter));
        }
        wet_mix += usize::from(main.num("dynamicsMix") > 0.0 || main.num("dynamicsLoad") > 0.0);
        let mut name = main.text("name").trim().to_string();
        if name.is_empty() {
            name = "Procreate brush".into();
        }
        out.presets.push(BrushPreset {
            name,
            brush: b,
            file: None,
        });
    }
    if no_shape > 0 {
        out.notes.push(format!(
            "{no_shape} brush(es) use one of Procreate's own shapes, which only Procreate has: \
             a round tip stands in"
        ));
    }
    if no_grain > 0 {
        out.notes.push(format!(
            "{no_grain} brush(es) use one of Procreate's own grains, which only Procreate has: \
             they paint without one"
        ));
    }
    if wet_mix > 0 {
        out.notes.push(format!(
            "{wet_mix} brush(es) mix paint with Procreate's Wet Mix (dilution, charge, pull), \
             approximated with this app's colour mixing"
        ));
    }
    Ok(out)
}

/// One of Procreate's own brush images (a shape or grain a brush names
/// but doesn't carry), by its file name, from a folder of them the user
/// supplies: the `ValkyrieResources.bundle/Brushes/Images` folder of their
/// own copy of Procreate, at `$RUSTY_PAINTER_PROCREATE_IMAGES` or as
/// `procreate-images` in the app's data folder. (They're Procreate's, so
/// the app can't ship them.) `None` for the blank and hard-round ones,
/// which this engine has, and when there's no such folder or image.
fn bundled_image(name: &str) -> Option<image::DynamicImage> {
    if name.is_empty() || name.contains("Preset-Blank") || name.contains("Preset-Hard") {
        return None;
    }
    // Just a file name: nothing that could climb out of the folder.
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    let dir = std::env::var_os("RUSTY_PAINTER_PROCREATE_IMAGES")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::app::init::data_dir().join("procreate-images"));
    bundled_image_in(&dir, name)
}

/// [`bundled_image`] from folder `dir`.
fn bundled_image_in(dir: &std::path::Path, name: &str) -> Option<image::DynamicImage> {
    let bytes = std::fs::read(dir.join(name)).ok()?;
    image::load_from_memory(&bytes).ok()
}

/// The brush's tip, size, spacing and dynamics; `missing` counts a
/// built-in shape that isn't in the file.
fn brush_from(s: &Settings, shape: Option<&image::DynamicImage>, missing: &mut usize) -> Brush {
    let (lo, hi) = (s.num("minSize"), s.num("maxSize"));
    let size = lo + s.num_or("paintSize", 0.5).clamp(0.0, 1.0) * (hi - lo);
    let diameter = (size * SIZE_UNIT).clamp(1.0, 3000.0);
    // A fraction of the size; Procreate's densest is far below 1%.
    let spacing = (s.num_or("plotSpacing", 0.1) * 100.0).clamp(1.0, 1000.0);
    let mut b = Brush::new(diameter, 100.0, Color32::BLACK, spacing);
    match shape {
        Some(img) => {
            let mut grey = img.to_luma8();
            if s.flag("shapeInverted") {
                image::imageops::invert(&mut grey);
            }
            b.brush_options.pixel_shape = PixelBrushShape::Custom(TipMask::from_image_with(
                &image::DynamicImage::ImageLuma8(grey),
                Some(false),
            ));
        }
        None => {
            // A hard round is one of the built-ins this engine has.
            let bundled = s.text("bundledShapePath");
            if !bundled.is_empty() && !bundled.contains("Preset-Hard") {
                *missing += 1;
                b.brush_options.hardness = 50.0;
            }
        }
    }
    b.brush_options.opacity = s.num_or("paintOpacity", 1.0).clamp(0.01, 1.0);
    let pressure_size = s.num("dynamicsPressureSize");
    b.brush_options.pressure_size = pressure_size > 0.0;
    b.brush_options.pressure_min_size = (1.0 - pressure_size).clamp(0.0, 1.0);
    b.brush_options.pressure_opacity = s.num("dynamicsPressureOpacity") > 0.0;
    let tip = &mut b.dynamics.tip;
    tip.angle = s.num("shapeAngle").to_degrees();
    tip.ratio = s.num_or("shapeRoundness", 1.0).clamp(0.02, 1.0);
    // Rotation -1..1: how much the tip turns with the stroke.
    tip.follow_stroke = s.num("shapeRotation").abs() >= 0.5;
    // Scatter: 1 turns each dab any way.
    tip.random_angle = (s.num("shapeScatter") * 180.0).clamp(0.0, 180.0);
    tip.random_flip_x = s.flag("shapeFlipXJitter");
    tip.random_flip_y = s.flag("shapeFlipYJitter");
    b.jitter = (s.num("plotJitter") * 50.0).clamp(0.0, 500.0);
    let r = &mut b.dynamics.random;
    r.count = ((s.num("shapeCount") * 16.0).round() as u32).clamp(1, 16);
    r.size = s.num("dynamicsJitterSize").clamp(0.0, 1.0);
    r.opacity = s.num("dynamicsJitterOpacity").clamp(0.0, 1.0);
    r.hue = (s.num("dynamicsJitterHue") * 180.0).clamp(0.0, 180.0);
    r.saturation = s.num("dynamicsJitterSaturation").clamp(0.0, 1.0);
    r.value = s
        .num("dynamicsJitterLightness")
        .max(s.num("dynamicsJitterDarkness"))
        .clamp(0.0, 1.0);
    let (taper_size, taper_opacity) = (s.num("taperSize"), s.num("taperOpacity"));
    if taper_size > 0.0 || taper_opacity > 0.0 {
        let t = &mut b.dynamics.taper;
        t.start = (s.num("taperStartLength") * SIZE_UNIT).clamp(0.0, 2000.0);
        t.end = (s.num("taperEndLength") * SIZE_UNIT).clamp(0.0, 2000.0);
        t.size = taper_size > 0.0;
        t.opacity = taper_opacity > 0.0;
        t.min = (1.0 - taper_size.max(taper_opacity)).clamp(0.0, 1.0);
    }
    // Procreate's wet edges come from each pixel's coverage (its ring).
    b.wet_edge = s.num("wetEdgesAmount").clamp(0.0, 0.95);
    b.wet_edge_style = crate::brush_engine::wet_edge::WetEdgeStyle::Ring;
    let burnt = s.num("burntEdgesAmount").clamp(0.0, 1.0);
    if burnt > 0.0 {
        b.burnt_edges = Some(crate::brush_engine::wet_edge::BurntEdges {
            amount: burnt,
            // Procreate's default is multiply.
            mode: procreate_blend(s.num_or("burntEdgesBlendMode", 1.0))
                .unwrap_or(LayerBlend::Multiply),
        });
    }
    if let Some(mode) = procreate_blend(s.num("blendMode")) {
        b.paint_blend = mode;
    }
    // How the dabs build up: a glaze adds each at the flow (capped at the
    // stroke's opacity with "max transfer"); pressure can drive the flow.
    let (_recursive, max_transfer, modulated) = rendering(s);
    let o = &mut b.brush_options;
    o.painting_mode = if max_transfer {
        PaintingMode::Wash
    } else {
        PaintingMode::BuildUp
    };
    o.flow = (s.num_or("dynamicsGlazedFlow", 1.0) * 100.0).clamp(1.0, 100.0);
    o.pressure_flow |= modulated;
    b.mixing = wet_mix(s);
    // Wetness jitter: each dab's wetness varies at random, as its smudge
    // length and colour rate.
    let jitter = s.num("dynamicsWetnessJitter").clamp(0.0, 1.0);
    if b.mixing.is_some() && jitter > 0.0 {
        use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
        for setting in [DabSetting::SmudgeLength, DabSetting::ColorRate] {
            b.inputs.push(InputMapping {
                sensor: Sensor::RandomDab,
                setting,
                amount: jitter,
                ..Default::default()
            });
        }
    }
    b
}

/// A Procreate blend-mode number (as layers and brushes keep it) as this
/// app's blend mode; `None` for the grain-only height modes and the
/// others with no counterpart. (13–16 are the non-separable modes, named
/// from what their maths does.)
pub(crate) fn procreate_blend(n: f32) -> Option<LayerBlend> {
    use LayerBlend::*;
    Some(match n as i32 {
        0 => Normal,
        1 => Multiply,
        2 => Screen,
        3 => LinearDodge,
        4 => Lighten,
        5 => Exclusion,
        6 => Difference,
        7 => Subtract,
        8 => LinearBurn,
        9 => ColorDodge,
        10 => ColorBurn,
        11 => Overlay,
        12 => HardLight,
        13 => Color,
        14 => Luminosity,
        15 => Hue,
        16 => Saturation,
        17 => SoftLight,
        19 => Darken,
        20 => HardMix,
        21 => VividLight,
        22 => LinearLight,
        23 => PinLight,
        24 => LighterColor,
        25 => DarkerColor,
        26 => Divide,
        _ => return None,
    })
}

/// A brush's rendering flags (recursive mixing, max transfer, modulated
/// transfer): as stored, or from the rendering mode an older brush keeps
/// (`dynamicsRenderType`, before it `dynamicsGlazeType`, before that the
/// `dynamicsGlaze` switch).
fn rendering(s: &Settings) -> (bool, bool, bool) {
    if !s.num_or("renderingRecursiveMixing", f32::NAN).is_nan() {
        return (
            s.flag("renderingRecursiveMixing"),
            s.flag("renderingMaxTransfer"),
            s.flag("renderingModulatedTransfer"),
        );
    }
    let kind = ["dynamicsRenderType", "dynamicsGlazeType"]
        .iter()
        .map(|k| s.num_or(k, f32::NAN))
        .find(|v| !v.is_nan())
        .or_else(|| {
            let glaze = s.num_or("dynamicsGlaze", f32::NAN);
            (!glaze.is_nan()).then_some(if glaze != 0.0 { 1.0 } else { 3.0 })
        });
    match kind.map(|k| k as i32) {
        Some(1) => (false, false, false),
        Some(2) => (false, true, false),
        Some(0 | 3) => (true, false, false),
        _ => (true, false, false),
    }
}

/// Procreate's Wet Mix as this engine's colour mixing, `None` without it:
/// dilution (how much water) is the inverse of the colour rate, charge how
/// long the brush's colour lasts, pull how far it drags the paint under it.
fn wet_mix(s: &Settings) -> Option<crate::brush_engine::brush_options::Mixing> {
    let (dilution, charge) = (s.num("dynamicsMix"), s.num("dynamicsLoad"));
    if dilution <= 0.0 && charge <= 0.0 {
        return None;
    }
    let pull = s.num_or(
        "dynamicsWetAccumulation",
        s.num("dynamicsSmudgeAccumulation"),
    );
    Some(crate::brush_engine::brush_options::Mixing {
        smudge_length: pull.clamp(0.0, 1.0),
        color_rate: (1.0 - dilution).clamp(0.0, 1.0),
        pressure_color: s.num("dynamicsPressureMix") > 0.0,
        gamma_correct: s.flag("blendGammaCorrect"),
        // Blur: the paint picked up is softened. Krita's dulling (one
        // colour averaged over that much of the dab) is the nearest; it
        // flattens the smear, so only for a clear blur.
        krita: (s.num("dynamicsBlur") >= 0.25).then(|| {
            crate::brush_engine::brush_options::KritaSmudge {
                dulling: true,
                radius: s.num("dynamicsBlur").clamp(0.0, 1.0),
                ..Default::default()
            }
        }),
        // Charge 1 never runs dry; less runs out after that many tens of
        // brush widths.
        load: if charge >= 1.0 {
            0.0
        } else {
            (charge * 40.0).max(1.0)
        },
        ..Default::default()
    })
}

/// The brush's grain; `missing` counts a built-in one that isn't in the
/// file.
fn grain_of(
    s: &Settings,
    grain: Option<&image::DynamicImage>,
    missing: &mut usize,
) -> Option<crate::brush_engine::texture::BrushTexture> {
    use crate::brush_engine::texture::{BrushTexture, GrainPlacement, Pattern, TextureMode};
    let Some(img) = grain else {
        let bundled = s.text("bundledGrainPath");
        // The blank grain is no grain.
        if !bundled.is_empty() && !bundled.contains("Preset-Blank") {
            *missing += 1;
        }
        return None;
    };
    let (shift, contrast) = (s.num("textureBrightness"), s.num("textureContrast"));
    let gain = 1.0 + contrast.clamp(-1.0, 1.0);
    let name = format!("{} grain", s.text("name").trim());
    let pattern = Pattern::from_image_with(&name, img, |v| {
        ((v + shift - 0.5) * gain + 0.5).clamp(0.0, 1.0)
    });
    let side = img.width().max(img.height()) as f32;
    Some(BrushTexture {
        scale: (s.num_or("textureScale", 1.0).clamp(0.1, 4.0) * side / pattern.size as f32)
            .clamp(0.05, 16.0),
        pattern,
        // Procreate's numbering (see `procreate_blend`; 27 and 28 are its
        // grain-only height modes), as the nearest of this engine's grain
        // modes; multiply (Procreate's default) for the rest.
        mode: match s.num_or("grainBlendMode", 1.0) as i32 {
            27 | 28 => TextureMode::Height,
            7 | 8 => TextureMode::Subtract,
            9 => TextureMode::ColorDodge,
            20 => TextureMode::HardMix,
            _ => TextureMode::Multiply,
        },
        strength: s.num_or("grainDepth", 1.0).clamp(0.0, 1.0),
        invert: s.flag("textureInverted"),
        placement: GrainPlacement {
            // Movement: rolling (1) is paper the stroke passes over, glide
            // (0) the grain held in the dab.
            per_dab: s.num_or("textureMovement", 1.0) < 0.5,
            angle: s.num("textureRotation").to_degrees(),
            random_offset: s.flag("textureOffsetJitter"),
            ..Default::default()
        },
        krita: None,
    })
}

/// The second brush of a dual brush, `tip` made from its settings `sub`.
fn dual_of(
    main: &Settings,
    sub: &Settings,
    tip: &Brush,
    diameter: f32,
) -> crate::brush_engine::dual::DualTip {
    use crate::brush_engine::dual::{DualMode, DualTip};
    let o = &tip.brush_options;
    DualTip {
        shape: o.pixel_shape.clone(),
        size: (o.diameter / diameter).clamp(0.05, 20.0),
        hardness: o.hardness,
        spacing: o.spacing,
        scatter: tip.jitter,
        count: tip.dynamics.random.count,
        random_angle: tip.dynamics.tip.random_angle > 0.0 || sub.flag("shapeRandomise"),
        // Procreate's numbering (see `procreate_blend`); the nearest of
        // this engine's dual modes.
        mode: match main.num("dualBlendMode") as i32 {
            19 => DualMode::Darken,
            7 | 8 => DualMode::Subtract,
            10 => DualMode::Burn,
            27 | 28 => DualMode::Height,
            _ => DualMode::Multiply,
        },
    }
}

/// A brush's settings: the archive's root object.
struct Settings {
    objects: Vec<Plist>,
    root: Vec<(usize, usize)>,
}

impl Settings {
    fn read(bytes: &[u8]) -> Result<Self, String> {
        let objects = read_bplist(bytes)?;
        let not = || "Brush.archive isn't a Procreate brush".to_string();
        // The top: `$objects` (everything) and `$top`.`root`.
        let Plist::Dict(top) = objects.first().ok_or_else(not)? else {
            return Err(not());
        };
        let entry = |dict: &[(usize, usize)], key: &str| {
            dict.iter()
                .find(|(k, _)| matches!(&objects[*k], Plist::Str(s) if s == key))
                .map(|&(_, v)| v)
        };
        let Some(Plist::Array(archived)) = entry(top, "$objects").map(|i| &objects[i]) else {
            return Err(not());
        };
        let Some(Plist::Dict(t)) = entry(top, "$top").map(|i| &objects[i]) else {
            return Err(not());
        };
        let Some(Plist::Uid(root)) = entry(t, "root").map(|i| &objects[i]) else {
            return Err(not());
        };
        let Some(Plist::Dict(root)) = archived.get(*root).map(|&i| &objects[i]) else {
            return Err(not());
        };
        // Uids point into `$objects`; made plain plist indices here.
        let root = root
            .iter()
            .map(|&(k, v)| {
                let v = match objects[v] {
                    Plist::Uid(u) => archived.get(u).copied().unwrap_or(v),
                    _ => v,
                };
                (k, v)
            })
            .collect();
        Ok(Self { objects, root })
    }

    fn get(&self, key: &str) -> Option<&Plist> {
        self.root
            .iter()
            .find(|(k, _)| matches!(&self.objects[*k], Plist::Str(s) if s == key))
            .map(|&(_, v)| &self.objects[v])
    }

    fn num_or(&self, key: &str, default: f32) -> f32 {
        match self.get(key) {
            Some(Plist::Real(v)) if v.is_finite() => *v as f32,
            Some(Plist::Int(v)) => *v as f32,
            Some(Plist::Bool(v)) => f32::from(u8::from(*v)),
            _ => default,
        }
    }

    fn num(&self, key: &str) -> f32 {
        self.num_or(key, 0.0)
    }

    fn flag(&self, key: &str) -> bool {
        self.num(key) != 0.0
    }

    /// A string, empty when there's none (`$null`).
    fn text(&self, key: &str) -> &str {
        match self.get(key) {
            Some(Plist::Str(s)) if s != "$null" => s,
            _ => "",
        }
    }
}

/// A binary property list's object; containers hold other objects'
/// indices.
#[derive(Debug, PartialEq)]
pub(crate) enum Plist {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(String),
    Data(Vec<u8>),
    Uid(usize),
    Array(Vec<usize>),
    Dict(Vec<(usize, usize)>),
}

/// Every object of a `bplist00`, the top one first.
pub(crate) fn read_bplist(bytes: &[u8]) -> Result<Vec<Plist>, String> {
    let bad = || "Brush.archive is damaged".to_string();
    if !bytes.starts_with(b"bplist00") || bytes.len() < 40 {
        return Err(bad());
    }
    let mut t = Reader::new(&bytes[bytes.len() - 26..]);
    let (offset_size, ref_size) = (t.u8()? as usize, t.u8()? as usize);
    let be = |b: &[u8]| b.iter().fold(0u64, |n, &x| n << 8 | u64::from(x));
    let count = be(t.take(8)?) as usize;
    let top = be(t.take(8)?) as usize;
    let table = be(t.take(8)?) as usize;
    if !(1..=8).contains(&offset_size)
        || !(1..=8).contains(&ref_size)
        || count == 0
        || top >= count
        || count > bytes.len()
    {
        return Err(bad());
    }
    let mut r = Reader::new(bytes);
    r.seek(table)?;
    let offsets: Vec<usize> = (0..count)
        .map(|_| r.take(offset_size).map(|b| be(b) as usize))
        .collect::<Result<_, _>>()?;
    let object = |at: usize| -> Result<Plist, String> {
        let mut r = Reader::new(bytes);
        r.seek(at)?;
        let marker = r.u8()?;
        let low = (marker & 0x0f) as usize;
        let reference = |r: &mut Reader<'_>| -> Result<usize, String> {
            let i = be(r.take(ref_size)?) as usize;
            if i < count { Ok(i) } else { Err(bad()) }
        };
        // A length too long for the marker's nibble follows as an int.
        let length = |r: &mut Reader<'_>| -> Result<usize, String> {
            if low != 0x0f {
                return Ok(low);
            }
            let m = r.u8()?;
            if m >> 4 != 1 {
                return Err(bad());
            }
            Ok(be(r.take(1 << (m & 0x0f).min(3))?) as usize)
        };
        Ok(match marker >> 4 {
            0 => match marker {
                0x08 => Plist::Bool(false),
                0x09 => Plist::Bool(true),
                _ => Plist::Null,
            },
            1 => {
                let b = r.take(1 << low.min(3))?;
                // 8 bytes are signed, fewer unsigned.
                Plist::Int(be(b) as i64)
            }
            2 => match low {
                2 => Plist::Real(f64::from(f32::from_bits(be(r.take(4)?) as u32))),
                3 => Plist::Real(f64::from_bits(be(r.take(8)?))),
                _ => return Err(bad()),
            },
            3 => Plist::Real(f64::from_bits(be(r.take(8)?))),
            4 => {
                let n = length(&mut r)?;
                Plist::Data(r.take(n)?.to_vec())
            }
            5 => {
                let n = length(&mut r)?;
                Plist::Str(String::from_utf8_lossy(r.take(n)?).into_owned())
            }
            6 => {
                let n = length(&mut r)?;
                let units: Vec<u16> = r
                    .take(n.checked_mul(2).ok_or_else(bad)?)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                Plist::Str(String::from_utf16_lossy(&units))
            }
            8 => Plist::Uid(be(r.take(low + 1)?) as usize),
            0xa => {
                let n = length(&mut r)?;
                Plist::Array(
                    (0..n)
                        .map(|_| reference(&mut r))
                        .collect::<Result<_, _>>()?,
                )
            }
            0xd => {
                let n = length(&mut r)?;
                let keys: Vec<usize> = (0..n)
                    .map(|_| reference(&mut r))
                    .collect::<Result<_, _>>()?;
                let values: Vec<usize> = (0..n)
                    .map(|_| reference(&mut r))
                    .collect::<Result<_, _>>()?;
                Plist::Dict(keys.into_iter().zip(values).collect())
            }
            _ => Plist::Null,
        })
    };
    // The top object first, the rest at their own indices after it.
    let mut objects: Vec<Plist> = offsets
        .iter()
        .map(|&at| object(at))
        .collect::<Result<_, _>>()?;
    objects.swap(0, top);
    let fix = |i: usize| match i {
        0 => top,
        i if i == top => 0,
        i => i,
    };
    for o in &mut objects {
        match o {
            Plist::Array(items) => items.iter_mut().for_each(|i| *i = fix(*i)),
            Plist::Dict(pairs) => pairs
                .iter_mut()
                .for_each(|(k, v)| (*k, *v) = (fix(*k), fix(*v))),
            _ => {}
        }
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `bplist00` of `objects` (references one byte), `top` its root.
    fn bplist(objects: &[Vec<u8>], top: u8) -> Vec<u8> {
        let mut out = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        for o in objects {
            offsets.push(out.len() as u8);
            out.extend(o);
        }
        let table = out.len() as u64;
        out.extend(&offsets);
        out.extend([0; 6]);
        out.extend([1, 1]);
        out.extend((objects.len() as u64).to_be_bytes());
        out.extend(u64::from(top).to_be_bytes());
        out.extend(table.to_be_bytes());
        out
    }

    fn string(s: &str) -> Vec<u8> {
        // Longer than 14: the length follows as a one-byte int.
        let mut v = match s.len() {
            n @ 0..15 => vec![0x50 | n as u8],
            n => vec![0x5f, 0x10, n as u8],
        };
        v.extend(s.as_bytes());
        v
    }

    fn real(v: f64) -> Vec<u8> {
        let mut out = vec![0x23];
        out.extend(v.to_bits().to_be_bytes());
        out
    }

    /// A keyed archive whose root holds `fields` (a name and numbers).
    fn archive(name: &str, fields: &[(&str, f64)]) -> Vec<u8> {
        // 0: top {$objects: 1, $top: 2}; 1: [$null, root]; 2: {root: uid 1};
        // 3..: strings and values; the root dict last.
        let mut objects: Vec<Vec<u8>> = vec![vec![], vec![], vec![], string("$objects")];
        objects.push(string("$top")); // 4
        objects.push(string("root")); // 5
        objects.push(string("$null")); // 6
        objects.push(vec![0x80, 1]); // 7: uid 1
        objects.push(string("name")); // 8
        objects.push(string(name)); // 9
        let mut pairs = vec![(8u8, 9u8)];
        for (k, v) in fields {
            let at = objects.len() as u8;
            objects.push(string(k));
            objects.push(real(*v));
            pairs.push((at, at + 1));
        }
        let root = objects.len() as u8;
        let mut dict = vec![0xd0 | pairs.len() as u8];
        dict.extend(pairs.iter().map(|p| p.0));
        dict.extend(pairs.iter().map(|p| p.1));
        objects.push(dict);
        objects[0] = vec![0xd2, 3, 4, 1, 2];
        objects[1] = vec![0xa2, 6, root];
        objects[2] = vec![0xd1, 5, 7];
        bplist(&objects, 0)
    }

    #[test]
    fn a_brushset_brings_its_brushes_shape_grain_and_dual_brush() {
        let shape = {
            let img = image::GrayImage::from_fn(32, 32, |x, y| {
                let d = (x as f32 - 16.0).hypot(y as f32 - 16.0);
                image::Luma([if d < 12.0 { 255 } else { 0 }])
            });
            let mut png = Vec::new();
            image::DynamicImage::ImageLuma8(img)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        };
        let mut zip = crate::project::zip::ZipWriter::default();
        let main = archive(
            "Wash",
            &[
                ("maxSize", 1.0),
                ("paintSize", 0.25),
                ("plotSpacing", 0.1),
                ("paintOpacity", 0.5),
                ("dynamicsPressureSize", 0.75),
                ("textureScale", 1.0),
                ("grainDepth", 0.4),
                ("wetEdgesAmount", 1.0),
                ("blendMode", 1.0),
            ],
        );
        zip.add("A/Brush.archive", &main).unwrap();
        zip.add("A/Shape.png", &shape).unwrap();
        zip.add("A/Grain.png", &shape).unwrap();
        zip.add(
            "A/Sub01/Brush.archive",
            &archive("1", &[("maxSize", 0.5), ("paintSize", 1.0)]),
        )
        .unwrap();
        zip.add("A/Sub01/Shape.png", &shape).unwrap();
        zip.add(
            "B/Brush.archive",
            &archive("Plain", &[("maxSize", 0.1), ("paintSize", 1.0)]),
        )
        .unwrap();
        let imported = super::super::import("set.brushset", &zip.finish().unwrap()).unwrap();
        let [wash, plain] = &imported.presets[..] else {
            panic!("{:?}", imported.notes);
        };
        assert_eq!(wash.name, "Wash");
        let b = &wash.brush;
        assert_eq!(b.brush_options.diameter, 100.0);
        assert_eq!(b.brush_options.spacing, 10.0);
        assert_eq!(b.brush_options.opacity, 0.5);
        // Pressure on size, folded into the inputs (down to a quarter).
        use crate::brush_engine::dynamics::{DabSetting, Sensor};
        let size = b
            .inputs
            .iter()
            .find(|m| m.sensor == Sensor::Pressure && m.setting == DabSetting::Size);
        assert!(
            size.is_some_and(|m| (m.amount - 0.75).abs() < 1e-6),
            "{:?}",
            b.inputs
        );
        assert!(matches!(
            b.brush_options.pixel_shape,
            PixelBrushShape::Custom(_)
        ));
        let t = b.texture.as_ref().expect("grain");
        assert!((t.strength - 0.4).abs() < 1e-6);
        assert_eq!(b.wet_edge, 0.95);
        assert_eq!(
            b.wet_edge_style,
            crate::brush_engine::wet_edge::WetEdgeStyle::Ring
        );
        assert_eq!(
            b.paint_blend,
            crate::canvas::blend_modes::LayerBlend::Multiply
        );
        let dual = b.dual.as_ref().expect("dual brush");
        assert!((dual.size - 2.0).abs() < 1e-6);
        assert!(matches!(dual.shape, PixelBrushShape::Custom(_)));
        assert_eq!(plain.brush.brush_options.diameter, 40.0);
        assert!(plain.brush.texture.is_none() && plain.brush.dual.is_none());
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    }

    #[test]
    fn watercolour_settings_come_across() {
        let brush = |fields: &[(&str, f64)]| {
            let data = archive("W", fields);
            brush_from(&Settings::read(&data).unwrap(), None, &mut 0)
        };
        // (The test archives keep offsets in a byte: a few fields each.)
        // A glaze capped at the stroke's opacity, its flow driven by pressure.
        let glaze = brush(&[
            ("renderingMaxTransfer", 1.0),
            ("renderingRecursiveMixing", 0.0),
            ("renderingModulatedTransfer", 1.0),
            ("dynamicsGlazedFlow", 0.4),
        ]);
        let o = &glaze.brush_options;
        assert_eq!(o.painting_mode, PaintingMode::Wash);
        assert!((o.flow - 40.0).abs() < 1e-4 && o.pressure_flow);
        // Burnt edges in colour burn, and soft light.
        let burnt = brush(&[
            ("burntEdgesAmount", 0.7),
            ("burntEdgesBlendMode", 10.0),
            ("blendMode", 17.0),
        ]);
        assert_eq!(burnt.paint_blend, LayerBlend::SoftLight);
        let edges = burnt.burnt_edges.expect("burnt edges");
        assert!((edges.amount - 0.7).abs() < 1e-6);
        assert_eq!(edges.mode, LayerBlend::ColorBurn);
        // Wet Mix.
        let wet = brush(&[
            ("dynamicsMix", 0.6),
            ("dynamicsLoad", 0.5),
            ("dynamicsWetAccumulation", 0.75),
        ]);
        let mix = wet.mixing.expect("wet mix");
        assert!((mix.color_rate - 0.4).abs() < 1e-6);
        assert!((mix.smudge_length - 0.75).abs() < 1e-6);
        assert!(mix.load > 0.0, "charge below 1 runs dry");
        assert!(mix.krita.is_none(), "no blur: smeared");
        let blurred = brush(&[
            ("dynamicsMix", 0.5),
            ("dynamicsBlur", 0.5),
            ("blendGammaCorrect", 1.0),
        ]);
        let mix = blurred.mixing.expect("wet mix");
        assert!(
            mix.krita
                .is_some_and(|k| k.dulling && (k.radius - 0.5).abs() < 1e-6)
        );
        assert!(mix.gamma_correct);
        // An older brush keeps only its rendering mode: 2 is max transfer.
        let old = brush(&[("maxSize", 0.5), ("dynamicsRenderType", 2.0)]);
        assert_eq!(old.brush_options.painting_mode, PaintingMode::Wash);
        let plain = brush(&[("maxSize", 0.5)]);
        assert_eq!(plain.brush_options.painting_mode, PaintingMode::BuildUp);
        assert!(plain.mixing.is_none() && plain.burnt_edges.is_none());
        // The blend table: the grain-only height modes have no counterpart.
        assert_eq!(procreate_blend(0.0), Some(LayerBlend::Normal));
        assert_eq!(procreate_blend(19.0), Some(LayerBlend::Darken));
        assert_eq!(procreate_blend(26.0), Some(LayerBlend::Divide));
        assert_eq!(procreate_blend(27.0), None);
    }

    #[test]
    fn built_in_images_come_from_the_users_folder_and_grain_modes_map() {
        let dir = std::env::temp_dir().join(format!("rp-procreate-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img =
            image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(8, 8, image::Luma([200])));
        // Procreate keeps one without an extension: read by its content.
        img.save_with_format(dir.join("Dry-Wash"), image::ImageFormat::Png)
            .unwrap();
        assert!(bundled_image_in(&dir, "Dry-Wash").is_some());
        assert!(bundled_image_in(&dir, "Missing.jpg").is_none());
        // Names that aren't plain file names, and the ones this engine has.
        assert!(bundled_image("../secret.png").is_none());
        assert!(bundled_image("Brush-Preset-Hard.png").is_none());
        std::fs::remove_dir_all(&dir).ok();
        // Grain: height mode, and wetness jitter on a wet brush.
        let data = archive(
            "G",
            &[
                ("grainBlendMode", 27.0),
                ("dynamicsLoad", 0.5),
                ("dynamicsWetnessJitter", 0.3),
            ],
        );
        let s = Settings::read(&data).unwrap();
        let grain = grain_of(&s, Some(&img), &mut 0).unwrap();
        assert_eq!(
            grain.mode,
            crate::brush_engine::texture::TextureMode::Height
        );
        let b = brush_from(&s, None, &mut 0);
        use crate::brush_engine::dynamics::{DabSetting, Sensor};
        assert!(b.inputs.iter().any(|m| m.sensor == Sensor::RandomDab
            && m.setting == DabSetting::SmudgeLength
            && (m.amount - 0.3).abs() < 1e-6));
    }

    #[test]
    fn a_damaged_archive_is_an_error_not_a_panic() {
        let good = archive("B", &[("maxSize", 0.5)]);
        assert!(Settings::read(&good).is_ok());
        for cut in 0..good.len() {
            let _ = Settings::read(&good[..cut]);
        }
        let mut bad = good.clone();
        for i in 8..bad.len() {
            bad[i] ^= 0xff;
            let _ = Settings::read(&bad);
            bad[i] ^= 0xff;
        }
    }
}
