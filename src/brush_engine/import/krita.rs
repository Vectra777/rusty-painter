//! Krita brushes: presets (`.kpp`) and bundles (`.bundle`).
//!
//! A preset is a PNG (its icon) carrying the settings as XML in a text
//! chunk named `preset`: which brush engine, its parameters, and the tip
//! (`brush_definition`: Krita's own round or square tip, or a picture,
//! embedded in the preset or kept in the bundle). A bundle is a ZIP of
//! presets with their tips and textures.
//!
//! The pixel brush engine maps onto this app's brushes, the bristle,
//! sketch and hatching ones onto its own, and the colour smudge engine
//! onto a brush with colour mixing; presets of other engines come with
//! their tip only, and the notes say so.

use super::{Imported, Reader, base64_decode, gimp};
use crate::brush_engine::brush::{Brush, BrushPreset, BrushType};
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
use crate::brush_engine::tip::TipMask;
use eframe::egui::Color32;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) fn import_kpp(bytes: &[u8], stem: &str) -> Result<Imported, String> {
    let mut out = Imported::default();
    let preset = read_kpp(bytes, stem, &HashMap::new(), &mut out.notes)?;
    out.presets.push(preset);
    Ok(out)
}

pub(super) fn import_bundle(bytes: &[u8]) -> Result<Imported, String> {
    let entries = crate::project::zip::read_all(bytes)?;
    // The bundle's tips by file name, for presets that refer to them.
    let files: HashMap<String, Vec<u8>> = entries
        .iter()
        .filter(|(name, _)| !name.starts_with("paintoppresets/"))
        .map(|(name, data)| {
            let file = name.rsplit('/').next().unwrap_or(name).to_string();
            (file, data.clone())
        })
        .collect();
    let mut out = Imported::default();
    for (name, data) in entries
        .iter()
        .filter(|(name, _)| name.starts_with("paintoppresets/") && name.ends_with(".kpp"))
    {
        let stem = name
            .rsplit('/')
            .next()
            .unwrap_or(name)
            .trim_end_matches(".kpp")
            .to_string();
        match read_kpp(data, &stem, &files, &mut out.notes) {
            Ok(preset) => out.presets.push(preset),
            Err(err) => out.notes.push(format!("{stem}: {err}")),
        }
    }
    Ok(out)
}

/// The XML in a preset's `preset` text chunk.
fn preset_xml(bytes: &[u8]) -> Result<String, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder
        .read_info()
        .map_err(|e| format!("Not a Krita preset ({e})"))?;
    let info = reader.info();
    for t in &info.uncompressed_latin1_text {
        if t.keyword == "preset" {
            return Ok(t.text.clone());
        }
    }
    for t in &info.compressed_latin1_text {
        if t.keyword == "preset" {
            return t.get_text().map_err(|e| e.to_string());
        }
    }
    for t in &info.utf8_text {
        if t.keyword == "preset" {
            return t.get_text().map_err(|e| e.to_string());
        }
    }
    Err("Not a Krita preset (no settings in it)".into())
}

fn read_kpp(
    bytes: &[u8],
    stem: &str,
    bundle: &HashMap<String, Vec<u8>>,
    notes: &mut Vec<String>,
) -> Result<BrushPreset, String> {
    let xml = preset_xml(bytes)?;
    let root = parse_xml(&xml)?;
    let preset = root
        .find("Preset")
        .ok_or("Not a Krita preset (no <Preset>)")?;
    let name = preset.attr("name").unwrap_or(stem).to_string();
    let engine = preset.attr("paintopid").unwrap_or("paintbrush").to_string();
    // Pictures embedded in the preset, by file name.
    let mut embedded: HashMap<String, Vec<u8>> = HashMap::new();
    for res in preset.descendants("resource") {
        if let Some(file) = res.attr("filename")
            && let Ok(data) = base64_decode(&res.text)
        {
            embedded.insert(file.to_string(), data);
        }
    }
    let params: HashMap<&str, &str> = preset
        .children
        .iter()
        .filter(|c| c.name == "param")
        .filter_map(|c| Some((c.attr("name")?, c.text.as_str())))
        .collect();
    let param = |k: &str| params.get(k).map(|v| v.trim());
    let yes = |k: &str| param(k) == Some("true");
    let number = |k: &str| param(k).and_then(|v| v.parse::<f32>().ok());
    // An option's sensors (Krita's ids) and whether pen pressure is one.
    let sensors = |k: &str| param(k).map(sensor_ids).unwrap_or_default();
    let by_pressure = |k: &str| sensors(k).iter().any(|id| id == "pressure");

    let mut b = Brush::new(40.0, 100.0, Color32::BLACK, 10.0);
    b.brush_options.pressure_size = false;
    // The tip.
    if let Some(def) = param("brush_definition") {
        match tip_from_definition(def, &embedded, bundle)? {
            Ok(tip) => {
                let o = &mut b.brush_options;
                o.diameter = tip.diameter;
                o.spacing = tip.spacing;
                o.hardness = tip.hardness;
                if let Some(curve) = tip.softness {
                    o.softness_selector = crate::brush_engine::hardness::SoftnessSelector::Curve;
                    o.softness_curve = curve;
                }
                o.tip_colors = tip.colors;
                o.pixel_shape = tip.shape;
                o.extra_tips = tip.extra;
                b.dynamics.tip.angle = tip.angle;
                b.dynamics.tip.ratio = tip.ratio;
            }
            Err(file) => notes.push(format!("{name}: its tip {file} is missing")),
        }
    }
    match engine.as_str() {
        "paintbrush" | "roundmarker" => {}
        "colorsmudge" => {
            // Its smudge rate is always on, its colour rate an option; a
            // smudge length of 1 would never pick paint up again.
            let rate = |k: &str| number(k).unwrap_or(0.5).clamp(0.0, 1.0);
            b.mixing = Some(crate::brush_engine::brush_options::Mixing {
                smudge_length: rate("SmudgeRateValue").min(0.9),
                color_rate: if yes("PressureColorRate") {
                    rate("ColorRateValue")
                } else {
                    0.0
                },
                pressure_length: yes("PressureSmudgeRate") && by_pressure("SmudgeRateSensor"),
                pressure_color: yes("PressureColorRate") && by_pressure("ColorRateSensor"),
            });
        }
        "hairybrush" => b.brush_type = BrushType::Bristle,
        "sketchbrush" => b.brush_type = BrushType::Sketch,
        "hatchingbrush" => {
            b.brush_type = BrushType::Hatching;
            notes.push(format!(
                "{name}: Krita's hatching came across with this app's hatching settings"
            ));
        }
        "deformbrush" | "filter" | "duplicate" => notes.push(format!(
            "{name}: Krita's {engine} engine is the Smudge and Blur tools' modes here (deform, \
             sharpen and adjust, clone); only its tip came across"
        )),
        other => notes.push(format!(
            "{name}: Krita's {other} engine has no counterpart here; only its tip came across"
        )),
    }
    // An eraser preset, or paint in a blend mode.
    if param("CompositeOp") == Some("erase") || yes("EraserMode") {
        b.brush_options.blend_mode = crate::brush_engine::brush_options::BlendMode::Eraser;
    } else if let Some(op) = param("CompositeOp") {
        b.paint_blend = crate::project::kra::blend(op);
    }
    if let Some(v) = number("OpacityValue") {
        b.brush_options.opacity = v.clamp(0.0, 1.0);
    }
    if let Some(v) = number("FlowValue") {
        b.brush_options.flow = (v * 100.0).clamp(0.0, 100.0);
    }
    // Options switched on that don't come across, for the report.
    const READ: [&str; 12] = [
        "Size",
        "Opacity",
        "Flow",
        "Rotation",
        "Mirror",
        "Scatter",
        "Spacing",
        "Ratio",
        "Sharpness",
        "Texture/Strength/",
        "SmudgeRate",
        "ColorRate",
    ];
    let mut skipped: Vec<&str> = params
        .iter()
        .filter(|(_, v)| v.trim() == "true")
        .filter_map(|(k, _)| k.strip_prefix("Pressure"))
        .filter(|o| !READ.contains(o) && !o.is_empty())
        .collect();
    skipped.sort_unstable();
    if !skipped.is_empty() {
        notes.push(format!(
            "{name}: Krita's {} option{} didn't come across",
            skipped.join(", "),
            if skipped.len() > 1 { "s" } else { "" }
        ));
    }
    // Krita's painting mode: 1 build up, 2 wash (the stroke never past its
    // opacity, however its dabs overlap).
    if param("PaintOpAction") == Some("2") {
        b.brush_options.painting_mode = crate::brush_engine::brush_options::PaintingMode::Wash;
    }
    // Size, opacity and flow: pressure through its curve, any other
    // sensor (random, speed…) as an input mapping.
    for (option, setting) in [
        ("Size", DabSetting::Size),
        ("Opacity", DabSetting::Opacity),
        ("Flow", DabSetting::Opacity),
    ] {
        if !yes(&format!("Pressure{option}")) {
            continue;
        }
        let key = format!("{option}Sensor");
        let o = &mut b.brush_options;
        let (on, curve) = match option {
            "Size" => (&mut o.pressure_size, &mut o.pressure_curves.size),
            "Opacity" => (&mut o.pressure_opacity, &mut o.pressure_curves.opacity),
            _ => (&mut o.pressure_flow, &mut o.pressure_curves.flow),
        };
        for id in sensors(&key) {
            if id == "pressure" {
                *on = true;
                *curve = param(&key).and_then(sensor_curve);
            } else if !map_sensor(&mut b.inputs, &id, setting, false) {
                notes.push(unmapped(&name, option, &id));
            }
        }
    }
    // Squash (Krita's ratio: low values flatten the tip).
    if yes("PressureRatio") {
        for id in sensors("RatioSensor") {
            if !map_sensor(&mut b.inputs, &id, DabSetting::Squash, true) {
                notes.push(unmapped(&name, "ratio", &id));
            }
        }
    }
    // Scatter: Krita's is up to its value in brush widths, either way,
    // like the jitter here (a percentage).
    if yes("PressureScatter")
        && let Some(v) = number("ScatterValue")
    {
        b.jitter = (v * 100.0).clamp(0.0, 500.0);
    }
    // Pressure spacing.
    if yes("PressureSpacing") && by_pressure("SpacingSensor") {
        let o = &mut b.brush_options;
        o.pressure_spacing = true;
        o.pressure_curves.spacing = param("SpacingSensor").and_then(sensor_curve);
    }
    // Rotation: each of Krita's sensors as this app's counterpart.
    if yes("PressureRotation") {
        for id in sensors("RotationSensor") {
            let tip = &mut b.dynamics.tip;
            match id.as_str() {
                "drawingangle" => tip.follow_stroke = true,
                "fuzzy" | "fuzzystroke" => tip.random_angle = 180.0,
                "ascension" => tip.follow_tilt = true,
                "rotation" => tip.follow_barrel = true,
                other => {
                    if !map_sensor(&mut b.inputs, other, DabSetting::Angle, false) {
                        notes.push(unmapped(&name, "rotation", other));
                    }
                }
            }
        }
    }
    // Mirror: here at random, whatever Krita's sensor.
    if yes("PressureMirror") {
        b.dynamics.tip.random_flip_x = yes("HorizontalMirrorEnabled");
        b.dynamics.tip.random_flip_y = yes("VerticalMirrorEnabled");
        let ids = sensors("MirrorSensor");
        if ids.iter().any(|id| !id.starts_with("fuzzy")) {
            notes.push(format!(
                "{name}: its tip flips at random here, rather than by {}",
                ids.join(" and ")
            ));
        }
    }
    // Sharpness: Krita's threshold (out of 100) as hard edges.
    // Sharpness: a dab's pixels above `1 - value` go solid (Krita's
    // `KisSharpnessOption::applyThreshold`), the rest clear.
    if yes("PressureSharpness") {
        b.sharpness = (1.0 - number("SharpnessValue").unwrap_or(1.0)).clamp(0.01, 1.0);
    }
    // The paper texture, from the bundle.
    if yes("Texture/Pattern/Enabled") {
        let file = param("Texture/Pattern/PatternFileName").unwrap_or_default();
        let base = file.rsplit(['/', ':']).next().unwrap_or(file);
        let pattern = bundle
            .get(base)
            .and_then(|d| {
                if base.to_lowercase().ends_with(".pat") {
                    gimp::read_pat(d).ok()
                } else {
                    image::load_from_memory(d).ok()
                }
            })
            .map(|img| {
                // Krita's grain as it is (not stretched to the full range),
                // its brightness taken off and contrast about the middle
                // (`KisTextureMaskInfo::recalculateMask`).
                let brightness = number("Texture/Pattern/Brightness").unwrap_or(0.0);
                let contrast = number("Texture/Pattern/Contrast").unwrap_or(1.0);
                let name = if brightness == 0.0 && contrast == 1.0 {
                    base.to_string()
                } else {
                    format!("{base} ({:+}, ×{contrast})", -brightness)
                };
                crate::brush_engine::texture::Pattern::from_image_with(&name, &img, |v| {
                    (((v - brightness) - 0.5) * contrast + 0.5).clamp(0.0, 1.0)
                })
            });
        match pattern {
            Some(pattern) => {
                use crate::brush_engine::texture::{BrushTexture, TextureMode};
                let mode = match number("Texture/Pattern/TexturingMode").unwrap_or(0.0) as i32 {
                    0 => TextureMode::Multiply,
                    1 => TextureMode::Subtract,
                    6 => TextureMode::ColorDodge,
                    10 | 11 => TextureMode::HardMix,
                    12..=15 => TextureMode::Height,
                    _ => {
                        notes.push(format!(
                            "{name}: its texture blends a way this app doesn't; multiplied instead"
                        ));
                        TextureMode::Multiply
                    }
                };
                b.texture = Some(BrushTexture {
                    pattern,
                    mode,
                    scale: number("Texture/Pattern/Scale")
                        .unwrap_or(1.0)
                        .clamp(0.05, 16.0),
                    strength: number("Texture/Strength/Value")
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0),
                    // Krita's subtract takes paint away where the grain
                    // is light, this app's where it's dark.
                    invert: yes("Texture/Pattern/Invert") != (mode == TextureMode::Subtract),
                    placement: crate::brush_engine::texture::GrainPlacement {
                        random_offset: yes("Texture/Pattern/isRandomOffsetX")
                            || yes("Texture/Pattern/isRandomOffsetY"),
                        ..Default::default()
                    },
                });
                // Its strength by pressure.
                if yes("PressureTexture/Strength/")
                    && sensors("Texture/Strength/Sensor")
                        .iter()
                        .any(|id| id == "pressure")
                {
                    map_sensor(
                        &mut b.inputs,
                        "pressure",
                        DabSetting::TextureStrength,
                        false,
                    );
                }
            }
            None => notes.push(format!("{name}: its texture {base} is missing")),
        }
    }
    // Krita's masking brush is this app's dual brush.
    if yes("MaskingBrush/Enabled")
        && let Some(def) = param("MaskingBrush/Preset/brush_definition")
    {
        use crate::brush_engine::dual::{DualMode, DualTip};
        match tip_from_definition(def, &embedded, bundle)? {
            Ok(tip) => {
                let mode = match param("MaskingBrush/MaskingCompositeOp").unwrap_or("multiply") {
                    "multiply" => DualMode::Multiply,
                    "darken" => DualMode::Darken,
                    "subtract" => DualMode::Subtract,
                    op if op.contains("height") => DualMode::Height,
                    op => {
                        notes.push(format!(
                            "{name}: its masking brush blends by {op}, here by multiply"
                        ));
                        DualMode::Multiply
                    }
                };
                let size = number("MaskingBrush/MasterSizeCoeff").unwrap_or(1.0);
                b.dual = Some(DualTip {
                    shape: tip.shape,
                    size: size.clamp(0.05, 4.0),
                    hardness: tip.hardness,
                    spacing: tip.spacing,
                    scatter: 0.0,
                    count: 1,
                    random_angle: false,
                    mode,
                });
            }
            Err(file) => notes.push(format!("{name}: its masking tip {file} is missing")),
        }
    }
    Ok(BrushPreset {
        name,
        brush: b,
        file: None,
    })
}

/// A tip as a preset's `brush_definition` describes it.
struct TipDef {
    shape: PixelBrushShape,
    extra: Vec<Arc<TipMask>>,
    diameter: f32,
    spacing: f32,
    hardness: f32,
    angle: f32,
    ratio: f32,
    /// A colour picture meant to paint its colours.
    colors: bool,
    /// Krita's soft circle: its falloff as a curve (strength by distance
    /// from the centre), rather than a fade.
    softness: Option<SoftnessCurve>,
}

/// The tip a `brush_definition` describes: Krita's round or square one,
/// or a picture from the preset or the bundle (`Err` names a picture that
/// can't be found).
fn tip_from_definition(
    def: &str,
    embedded: &HashMap<String, Vec<u8>>,
    bundle: &HashMap<String, Vec<u8>>,
) -> Result<Result<TipDef, String>, String> {
    let def = parse_xml(def)?;
    let Some(brush) = def.find("Brush") else {
        return Ok(Err("(none)".into()));
    };
    let attr = |k: &str| brush.attr(k).and_then(|v| v.parse::<f32>().ok());
    let mut tip = TipDef {
        shape: PixelBrushShape::Circle,
        extra: Vec::new(),
        diameter: 40.0,
        spacing: attr("spacing").map_or(10.0, |s| (s * 100.0).clamp(1.0, 1000.0)),
        hardness: 100.0,
        angle: attr("angle").unwrap_or(0.0).to_degrees(),
        ratio: 1.0,
        colors: false,
        softness: None,
    };
    if brush.attr("type") == Some("auto_brush") {
        if let Some(mask) = brush.find("MaskGenerator") {
            let m = |k: &str| mask.attr(k).and_then(|v| v.parse::<f32>().ok());
            let diameter = m("diameter").or(m("radius").map(|r| r * 2.0));
            tip.diameter = diameter.unwrap_or(40.0).clamp(1.0, 3000.0);
            tip.ratio = m("ratio").unwrap_or(1.0).clamp(0.02, 1.0);
            // Krita's falloff, sampled as a curve (strength by distance
            // from the centre, 0..1): see `circle_falloff`.
            let (fh, fv) = (m("hfade").unwrap_or(1.0), m("vfade").unwrap_or(1.0));
            let id = mask.attr("id").unwrap_or("default");
            tip.hardness = 100.0;
            tip.softness = circle_falloff(id, fh, fv).map(|f| SoftnessCurve {
                points: (0..=FALLOFF_SAMPLES)
                    .map(|i| {
                        let r = i as f32 / FALLOFF_SAMPLES as f32;
                        CurvePoint::new(r, f(r).clamp(0.0, 1.0))
                    })
                    .collect(),
            });
            // A soft circle's falloff is its curve, whatever the fade (an
            // airbrush: faint all over, gone at the edge). Krita reads it
            // at the squared distance from the centre, this app at the
            // distance: resampled to match.
            if mask.attr("id") == Some("soft") {
                tip.softness = mask
                    .attr("softness_curve")
                    .and_then(parse_curve)
                    .map(|krita| SoftnessCurve {
                        points: (0..=24)
                            .map(|i| {
                                let r = i as f32 / 24.0;
                                CurvePoint::new(r, krita.eval(r * r).clamp(0.0, 1.0))
                            })
                            .collect(),
                    });
            }
            if mask.attr("type") == Some("rect") {
                tip.shape = PixelBrushShape::Square;
            }
        }
        auto_spacing(&mut tip, brush);
        return Ok(Ok(tip));
    }
    let file = brush.attr("filename").unwrap_or_default();
    let Some((mask, extra)) = picture_tip(file, embedded, bundle) else {
        return Ok(Err(file.to_string()));
    };
    tip.diameter =
        (mask.width.max(mask.height) as f32 * attr("scale").unwrap_or(1.0)).clamp(1.0, 3000.0);
    // Krita paints a colour picture's colours unless it's used as a mask
    // (Krita 5 says how in `brushApplication`: 1 stamps the picture).
    tip.colors = mask.has_colors()
        && match brush.attr("brushApplication") {
            Some(application) => application == "1",
            None => brush.attr("ColorAsMask") != Some("1"),
        };
    tip.shape = PixelBrushShape::Custom(mask);
    tip.extra = extra;
    auto_spacing(&mut tip, brush);
    Ok(Ok(tip))
}

/// Points a Krita round tip's falloff is sampled at.
const FALLOFF_SAMPLES: usize = 24;

/// A Krita round tip's strength at distance `r` (0 centre, 1 edge), from
/// its mask generator (`id`) and fades; `None` for a hard circle (all 1).
/// Krita works on the squared distance `n = r²`:
/// - `default`: solid out to the fade `f`, then `1 - (n - f²) / (1 - f²)`
///   (a fade of 0 is the softest, `1 - n`; 1 is hard);
/// - `gauss`: `(erf(d + c) - erf(d - c)) / (2 erf c)`, its width set by
///   the fade (`KisGaussCircleMaskGenerator`).
///
/// The `soft` curve tip is read separately (its own curve).
fn circle_falloff(id: &str, fh: f32, fv: f32) -> Option<Box<dyn Fn(f32) -> f32>> {
    match id {
        "gauss" => {
            let fade = (1.0 - (fh + fv) as f64 / 2.0).clamp(1e-6, 1.0 - 1e-6);
            let center =
                2.5 * (6761.0 * fade - 10000.0) / (std::f64::consts::SQRT_2 * 6761.0 * fade);
            let scale = std::f64::consts::SQRT_2 * 12500.0 / (6761.0 * fade);
            let norm = 2.0 * erf(center);
            Some(Box::new(move |r: f32| {
                let d = r as f64 * scale;
                ((erf(d + center) - erf(d - center)) / norm) as f32
            }))
        }
        "soft" => None,
        _ => {
            let f = fh.min(fv).clamp(0.0, 1.0);
            if f >= 0.999 {
                return None;
            }
            Some(Box::new(move |r: f32| {
                let n = r * r;
                if n <= f * f {
                    1.0
                } else {
                    1.0 - (n - f * f) / (1.0 - f * f)
                }
            }))
        }
    }
}

/// The error function (Abramowitz & Stegun 7.1.26, within 1.5e-7).
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let poly = t
        * (0.254829592
            + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let y = 1.0 - poly * (-x * x).exp();
    if x < 0.0 { -y } else { y }
}

/// Krita's auto spacing: `coeff × √size` pixels apart (for its size), as
/// this app's share of the size.
fn auto_spacing(tip: &mut TipDef, brush: &Node) {
    if brush.attr("useAutoSpacing") != Some("1") {
        return;
    }
    let coeff = brush
        .attr("autoSpacingCoeff")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(1.0);
    let d = tip.diameter.max(1.0);
    tip.spacing = (100.0 * coeff * d.sqrt() / d).clamp(1.0, 1000.0);
}

/// A tip picture by file name, from the preset or the bundle: the tip,
/// and a `.gih`'s other cells.
fn picture_tip(
    file: &str,
    embedded: &HashMap<String, Vec<u8>>,
    bundle: &HashMap<String, Vec<u8>>,
) -> Option<(Arc<TipMask>, Vec<Arc<TipMask>>)> {
    let base = file.rsplit('/').next().unwrap_or(file);
    let data = embedded
        .get(file)
        .or(embedded.get(base))
        .or(bundle.get(base))?;
    let lower = base.to_lowercase();
    if lower.ends_with(".gbr") {
        return gimp::read_gbr(&mut Reader::new(data))
            .ok()
            .map(|g| (g.tip, Vec::new()));
    }
    if lower.ends_with(".gih") {
        let imported = gimp::import_gih(data, base).ok()?;
        let o = &imported.presets.first()?.brush.brush_options;
        let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
            return None;
        };
        return Some((tip.clone(), o.extra_tips.clone()));
    }
    let img = image::load_from_memory(data).ok()?;
    // Krita: dark paints, whatever is round the edges.
    Some((TipMask::from_image_with(&img, Some(true)), Vec::new()))
}

/// A sensor's curve (`<curve>0,0;0.5,0.2;1,1;</curve>` inside its XML),
/// if it isn't the straight line.
/// This app's counterpart of one of Krita's sensors (by its id).
fn krita_sensor(id: &str) -> Option<Sensor> {
    Some(match id {
        "pressure" => Sensor::Pressure,
        "speed" => Sensor::Speed,
        "declination" => Sensor::Tilt,
        "ascension" => Sensor::TiltDirection,
        "drawingangle" => Sensor::Direction,
        "distance" => Sensor::Distance,
        "time" => Sensor::Time,
        "fuzzy" => Sensor::RandomDab,
        "fuzzystroke" => Sensor::RandomStroke,
        "rotation" => Sensor::Rotation,
        "tangentialpressure" => Sensor::Wheel,
        _ => return None,
    })
}

/// Krita's `id` sensor driving `setting`, as an input mapping (full
/// amount; `inverted`: a high input reduces it). Whether it has one here.
fn map_sensor(
    inputs: &mut Vec<InputMapping>,
    id: &str,
    setting: DabSetting,
    inverted: bool,
) -> bool {
    let Some(sensor) = krita_sensor(id) else {
        return false;
    };
    let mut mapping = InputMapping {
        sensor,
        setting,
        amount: 1.0,
        ..Default::default()
    };
    if inverted {
        mapping.curve = SoftnessCurve {
            points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 0.0)],
        };
    }
    inputs.push(mapping);
    true
}

/// The note for a sensor with no counterpart.
fn unmapped(name: &str, option: &str, id: &str) -> String {
    format!("{name}: its {option} follows Krita's {id} sensor, which has no counterpart here")
}

/// The ids of a curve option's sensors: one, or a `sensorslist`'s.
fn sensor_ids(xml: &str) -> Vec<String> {
    let Ok(root) = parse_xml(xml) else {
        return Vec::new();
    };
    let Some(params) = root.find("params") else {
        return Vec::new();
    };
    match params.attr("id") {
        Some("sensorslist") => params
            .descendants("ChildSensor")
            .filter_map(|c| c.attr("id").map(str::to_string))
            .collect(),
        Some(id) => vec![id.to_string()],
        None => Vec::new(),
    }
}

fn sensor_curve(xml: &str) -> Option<SoftnessCurve> {
    let root = parse_xml(xml).ok()?;
    // The pressure sensor's (alone, or among several).
    let pressure = std::iter::once(&root)
        .chain(root.descendants("params"))
        .chain(root.descendants("ChildSensor"))
        .find(|n| n.attr("id") == Some("pressure"))?;
    let curve = parse_curve(&pressure.descendants("curve").next()?.text)?;
    let straight = curve.points.len() == 2
        && curve.points[0] == CurvePoint::new(0.0, 0.0)
        && curve.points[1] == CurvePoint::new(1.0, 1.0);
    (!straight).then_some(curve)
}

/// Krita's curve text, `x,y;x,y;…` (0..1 each), as a curve; `None` with
/// fewer than two points.
fn parse_curve(text: &str) -> Option<SoftnessCurve> {
    let mut points: Vec<CurvePoint> = text
        .split(';')
        .filter_map(|p| {
            let (x, y) = p.split_once(',')?;
            let (x, y) = (x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?);
            (x.is_finite() && y.is_finite())
                .then(|| CurvePoint::new(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
        })
        .collect();
    points.sort_by(|a, b| a.x.total_cmp(&b.x));
    (points.len() >= 2).then_some(SoftnessCurve { points })
}

/// An XML element: its name, attributes, text (and CDATA) and children.
#[derive(Debug, Default)]
pub(crate) struct Node {
    pub(crate) name: String,
    attrs: Vec<(String, String)>,
    text: String,
    pub(crate) children: Vec<Node>,
}

impl Node {
    pub(crate) fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The first element called `name`, this one or below.
    pub(crate) fn find(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }

    /// Every element called `name` below this one.
    fn descendants<'a>(&'a self, name: &'a str) -> Box<dyn Iterator<Item = &'a Node> + 'a> {
        Box::new(self.children.iter().flat_map(move |c| {
            let own = (c.name == name).then_some(c);
            own.into_iter().chain(c.descendants(name))
        }))
    }
}

/// A small XML reader (elements, attributes, text): enough for Krita's
/// presets. The result is a nameless root holding the document's elements.
pub(crate) fn parse_xml(xml: &str) -> Result<Node, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut stack = vec![Node::default()];
    let bad = |e: &dyn std::fmt::Display| format!("Damaged preset settings ({e})");
    let element = |e: &quick_xml::events::BytesStart<'_>| -> Result<Node, String> {
        let mut node = Node {
            name: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
            ..Default::default()
        };
        for a in e.attributes().flatten() {
            let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let value = a.unescape_value().map_err(|e| bad(&e))?.into_owned();
            node.attrs.push((key, value));
        }
        Ok(node)
    };
    loop {
        match reader.read_event().map_err(|e| bad(&e))? {
            Event::Start(e) => stack.push(element(&e)?),
            Event::Empty(e) => {
                let node = element(&e)?;
                stack.last_mut().expect("the root").children.push(node);
            }
            Event::End(_) => {
                let node = stack.pop().expect("an open element");
                let Some(parent) = stack.last_mut() else {
                    return Err(bad(&"an unmatched closing tag"));
                };
                parent.children.push(node);
            }
            Event::Text(t) => {
                let text = t.unescape().map_err(|e| bad(&e))?;
                stack.last_mut().expect("the root").text.push_str(&text);
            }
            Event::CData(t) => {
                let text = String::from_utf8_lossy(&t.into_inner()).into_owned();
                stack.last_mut().expect("the root").text.push_str(&text);
            }
            Event::Eof => break,
            _ => {}
        }
        if stack.is_empty() {
            return Err(bad(&"an unmatched closing tag"));
        }
    }
    if stack.len() != 1 {
        return Err(bad(&"an element left open"));
    }
    Ok(stack.pop().expect("the root"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::blend_modes::LayerBlend;

    /// A preset PNG with `xml` in its `preset` chunk.
    pub fn kpp(xml: &str) -> Vec<u8> {
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder
                .add_ztxt_chunk("version".into(), "5.0".into())
                .unwrap();
            encoder.add_ztxt_chunk("preset".into(), xml.into()).unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0; 16]).unwrap();
        }
        png_bytes
    }

    const AUTO: &str = r#"<Preset paintopid="paintbrush" name="Soft Ink" embedded_resources="0">
        <param type="string" name="brush_definition"><![CDATA[<Brush type="auto_brush" spacing="0.08" angle="0.5">
            <MaskGenerator diameter="36" ratio="0.5" hfade="0.25" vfade="0.25" type="circle"/></Brush>]]></param>
        <param type="string" name="PressureSize"><![CDATA[true]]></param>
        <param type="string" name="SizeSensor"><![CDATA[<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>]]></param>
        <param type="string" name="OpacityValue"><![CDATA[0.7]]></param>
        </Preset>"#;

    #[test]
    fn a_round_krita_preset_brings_its_settings() {
        let imported = import_kpp(&kpp(AUTO), "file").unwrap();
        let p = &imported.presets[0];
        assert_eq!(p.name, "Soft Ink");
        let o = &p.brush.brush_options;
        assert_eq!(o.diameter, 36.0);
        assert!((o.spacing - 8.0).abs() < 1e-3);
        // Fade 0.25: solid to a quarter of the way out, then Krita's
        // falloff on the squared distance, `1 - (n - f²) / (1 - f²)`.
        assert_eq!(
            o.softness_selector,
            crate::brush_engine::hardness::SoftnessSelector::Curve
        );
        let c = &o.softness_curve;
        assert!((c.eval(0.2) - 1.0).abs() < 1e-3);
        assert!((c.eval(0.5) - 0.8).abs() < 0.01, "{}", c.eval(0.5));
        assert!(c.eval(1.0).abs() < 1e-3);
        assert!((o.opacity - 0.7).abs() < 1e-6);
        assert!(o.pressure_size);
        assert_eq!(o.pressure_curves.size.as_ref().unwrap().points.len(), 3);
        assert_eq!(p.brush.dynamics.tip.ratio, 0.5);
        assert!((p.brush.dynamics.tip.angle - 0.5f32.to_degrees()).abs() < 1e-3);
        assert!(imported.notes.is_empty());
    }

    /// `AUTO` with `params` added (name, value pairs) and its engine `engine`.
    fn with_params(engine: &str, params: &[(&str, &str)]) -> Vec<u8> {
        let extra: String = params
            .iter()
            .map(|(k, v)| format!(r#"<param type="string" name="{k}"><![CDATA[{v}]]></param>"#))
            .collect();
        kpp(&AUTO
            .replace("paintbrush", engine)
            .replace("</Preset>", &format!("{extra}</Preset>")))
    }

    const PRESSURE: &str = r#"<!DOCTYPE params><params id="pressure"/>"#;

    #[test]
    fn wash_random_sensors_auto_spacing_scatter_and_texture_options_come_across() {
        const FUZZY: &str = r#"<!DOCTYPE params><params id="fuzzy"/>"#;
        let xml = AUTO
            .replace(r#"spacing="0.08""#, r#"spacing="0.08" useAutoSpacing="1" autoSpacingCoeff="0.5""#)
            .replace(
                r#"<param type="string" name="SizeSensor"><![CDATA[<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>]]></param>"#,
                &format!(r#"<param type="string" name="SizeSensor"><![CDATA[{FUZZY}]]></param>"#),
            );
        let brush = with_params_xml(
            &xml,
            &[
                ("PaintOpAction", "2"),
                ("PressureScatter", "true"),
                ("ScatterValue", "0.5"),
                ("PressureRatio", "true"),
                ("RatioSensor", FUZZY),
                ("Pressureh", "true"),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let b = &imported.presets[0].brush;
        let o = &b.brush_options;
        assert_eq!(
            o.painting_mode,
            crate::brush_engine::brush_options::PaintingMode::Wash
        );
        // Random size, not pressure.
        assert!(!o.pressure_size);
        let mapped: Vec<_> = b.inputs.iter().map(|m| (m.sensor, m.setting)).collect();
        assert!(
            mapped.contains(&(Sensor::RandomDab, DabSetting::Size)),
            "{mapped:?}"
        );
        assert!(
            mapped.contains(&(Sensor::RandomDab, DabSetting::Squash)),
            "{mapped:?}"
        );
        // Auto spacing: 0.5 × √36 = 3 px of 36.
        assert!(
            (o.spacing - 100.0 * 3.0 / 36.0).abs() < 1e-3,
            "{}",
            o.spacing
        );
        // Half a brush width either way.
        assert_eq!(b.jitter, 50.0);
        // The hue option has no counterpart: the report says so.
        assert!(
            imported.notes.iter().any(|n| n.contains("h option")),
            "{:?}",
            imported.notes
        );
    }

    /// A preset's XML with `params` added.
    fn with_params_xml(xml: &str, params: &[(&str, &str)]) -> Vec<u8> {
        let extra: String = params
            .iter()
            .map(|(k, v)| format!(r#"<param type="string" name="{k}"><![CDATA[{v}]]></param>"#))
            .collect();
        kpp(&xml.replace("</Preset>", &format!("{extra}</Preset>")))
    }

    #[test]
    fn a_soft_circle_keeps_its_falloff_curve() {
        // Krita's airbrush: faint in the middle, nothing at the edge, and
        // no fade (which alone would read as a hard tip).
        let xml = AUTO.replace(
            r#"hfade="0.25" vfade="0.25" type="circle""#,
            r#"hfade="0" vfade="0" id="soft" softness_curve="0,0.4;0.43,0.12;1,0;" type="circle""#,
        );
        let imported = import_kpp(&kpp(&xml), "file").unwrap();
        let o = &imported.presets[0].brush.brush_options;
        assert_eq!(
            o.softness_selector,
            crate::brush_engine::hardness::SoftnessSelector::Curve
        );
        assert!(
            (o.softness_curve.eval(0.0) - 0.4).abs() < 1e-3,
            "faint centre"
        );
        assert!(o.softness_curve.eval(1.0).abs() < 1e-3, "gone at the edge");
        // Krita reads its curve at the squared distance: √0.43 of the way
        // out is its point at 0.43 (0.12).
        let at = o.softness_curve.eval(0.43f32.sqrt());
        assert!((at - 0.12).abs() < 0.01, "{at}");
        // A circle with a full fade is Krita's hard tip: hard here too.
        let hard = AUTO.replace(r#"hfade="0.25" vfade="0.25""#, r#"hfade="1" vfade="1""#);
        let hard = import_kpp(&kpp(&hard), "file").unwrap();
        let o = &hard.presets[0].brush.brush_options;
        assert_eq!(
            o.softness_selector,
            crate::brush_engine::hardness::SoftnessSelector::Gaussian
        );
        assert_eq!(o.hardness, 100.0);
    }

    #[test]
    fn a_gaussian_circle_falls_off_like_krita_s() {
        let xml = AUTO.replace(
            r#"hfade="0.25" vfade="0.25" type="circle""#,
            r#"hfade="0.5" vfade="0.5" id="gauss" type="circle""#,
        );
        let imported = import_kpp(&kpp(&xml), "file").unwrap();
        let c = &imported.presets[0].brush.brush_options.softness_curve;
        // Krita: alphafactor·(erf(d + c) − erf(d − c)), fade 0.5.
        let (fade, sqrt2) = (0.5f64, std::f64::consts::SQRT_2);
        let center = 2.5 * (6761.0 * fade - 10000.0) / (sqrt2 * 6761.0 * fade);
        let krita = |r: f64| {
            let d = r * sqrt2 * 12500.0 / (6761.0 * fade);
            (erf(d + center) - erf(d - center)) / (2.0 * erf(center))
        };
        for r in [0.0, 0.3, 0.6, 0.9] {
            let got = c.eval(r as f32) as f64;
            assert!((got - krita(r)).abs() < 0.02, "{r}: {got} vs {}", krita(r));
        }
        assert!(c.eval(0.0) > c.eval(0.5) && c.eval(0.5) > c.eval(0.9));
    }

    #[test]
    fn erf_matches_known_values() {
        for (x, want) in [
            (0.0, 0.0),
            (0.5, 0.5204999),
            (1.0, 0.8427008),
            (-2.0, -0.9953223),
        ] {
            assert!((erf(x) - want).abs() < 1e-6, "{x}");
        }
    }

    #[test]
    fn colour_smudge_mirror_rotation_spacing_and_sharpness_come_across() {
        let smudge = with_params(
            "colorsmudge",
            &[
                ("SmudgeRateValue", "0.6"),
                ("PressureSmudgeRate", "true"),
                ("SmudgeRateSensor", PRESSURE),
                ("PressureColorRate", "true"),
                ("ColorRateValue", "0.5"),
                (
                    "ColorRateSensor",
                    r#"<!DOCTYPE params><params id="fuzzy"/>"#,
                ),
                ("CompositeOp", "parallel"),
            ],
        );
        let imported = import_kpp(&smudge, "file").unwrap();
        let b = &imported.presets[0].brush;
        let m = b.mixing.expect("colour mixing");
        assert_eq!((m.smudge_length, m.color_rate), (0.6, 0.5));
        assert!(m.pressure_length && !m.pressure_color);
        assert_eq!(b.paint_blend, LayerBlend::Parallel);
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);

        let brush = with_params(
            "paintbrush",
            &[
                ("PressureMirror", "true"),
                ("HorizontalMirrorEnabled", "true"),
                ("MirrorSensor", r#"<!DOCTYPE params><params id="fuzzy"/>"#),
                ("PressureRotation", "true"),
                (
                    "RotationSensor",
                    r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="drawingangle"/><ChildSensor id="tangentialpressure"/></params>"#,
                ),
                ("PressureSpacing", "true"),
                ("SpacingSensor", PRESSURE),
                ("PressureSharpness", "true"),
                ("SharpnessValue", "0.6"),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let b = &imported.presets[0].brush;
        let tip = &b.dynamics.tip;
        assert!(tip.random_flip_x && !tip.random_flip_y);
        assert!(tip.follow_stroke);
        assert_eq!(b.inputs.len(), 1);
        assert_eq!(
            (b.inputs[0].sensor, b.inputs[0].setting),
            (Sensor::Wheel, DabSetting::Angle)
        );
        assert!(b.brush_options.pressure_spacing);
        assert!((b.sharpness - 0.4).abs() < 1e-6);
        assert!(b.mixing.is_none());
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    }

    #[test]
    fn a_picture_tip_comes_from_the_preset_or_its_bundle() {
        let pixels = vec![255u8; 12 * 6];
        let gbr = gimp::tests::gbr("t", 12, 6, 1, 25, &pixels);
        let b64 = {
            // Standard base64, for the embedded resource.
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in gbr.chunks(3) {
                let n = (c[0] as u32) << 16
                    | (*c.get(1).unwrap_or(&0) as u32) << 8
                    | *c.get(2).unwrap_or(&0) as u32;
                for k in 0..4 {
                    if k <= c.len() {
                        s.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
                    } else {
                        s.push('=');
                    }
                }
            }
            s
        };
        let xml = format!(
            r#"<Preset paintopid="paintbrush" name="Pic" embedded_resources="1">
            <resources><resource type="brushes" filename="tip.gbr" name="tip">{b64}</resource></resources>
            <param type="string" name="brush_definition"><![CDATA[<Brush type="gbr_brush" filename="tip.gbr" spacing="0.2" scale="2"/>]]></param>
            </Preset>"#
        );
        let imported = import_kpp(&kpp(&xml), "file").unwrap();
        let o = &imported.presets[0].brush.brush_options;
        let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
            panic!("the picture: {:?}", imported.notes);
        };
        assert_eq!((tip.width, tip.height), (12, 6));
        assert_eq!(o.diameter, 24.0);
        // Not embedded and not in a bundle: a note, still a preset.
        let lonely = xml.replace("tip.gbr\" name", "other.gbr\" name");
        let imported = import_kpp(&kpp(&lonely), "file").unwrap();
        assert_eq!(imported.notes.len(), 1);
    }

    #[test]
    fn other_engines_come_with_a_note_and_damage_is_refused() {
        let spray = AUTO.replace("paintbrush", "spraybrush");
        let imported = import_kpp(&kpp(&spray), "file").unwrap();
        assert!(imported.notes[0].contains("spraybrush"));
        assert!(import_kpp(b"not a png", "f").is_err());
        assert!(import_kpp(&kpp("<Preset><param>"), "f").is_err());
    }

    #[test]
    fn a_bundle_brings_its_presets() {
        let mut zip = crate::project::zip::ZipWriter::default();
        zip.add("mimetype", b"application/x-krita-resourcebundle")
            .unwrap();
        zip.add("paintoppresets/a.kpp", &kpp(AUTO)).unwrap();
        zip.add(
            "paintoppresets/b.kpp",
            &kpp(&AUTO.replace("Soft Ink", "Second")),
        )
        .unwrap();
        let imported = import_bundle(&zip.finish().unwrap()).unwrap();
        let names: Vec<&str> = imported.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Soft Ink", "Second"]);
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_krita_presets() {
        // The preset XML (the PNG around it has checksums).
        crate::fuzz::fuzz(
            "kpp-xml",
            AUTO.as_bytes(),
            std::time::Duration::from_secs(2),
            |b| {
                let mut notes = Vec::new();
                // Latin-1, as PNG text must be.
                let xml: String = b.iter().map(|&c| c as char).collect();
                let _ = read_kpp(&kpp(&xml), "fuzz", &HashMap::new(), &mut notes);
            },
        );
    }
}
