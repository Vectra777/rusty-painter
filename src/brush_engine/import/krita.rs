//! Brush presets (`.kpp`) and bundles (`.bundle`).
//!
//! A preset is a PNG (its icon) carrying the settings as XML in a text
//! chunk named `preset`: which brush engine, its parameters, and the tip
//! (`brush_definition`: a generated round or square tip, or a picture,
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
use crate::brush_engine::hardness::{CurvePoint, Softening, SoftnessCurve, sampled};
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
    // An option's sensors (their ids, by its `…Sensor` key): none when its
    // curve is off (`…UseCurve`,
    // the option's value alone), and each through the option's common
    // curve when it uses the same one for all (`…UseSameCurve`).
    let prefix = |k: &str| k.strip_suffix("Sensor").unwrap_or(k).to_string();
    let sensors = |k: &str| {
        if param(&format!("{}UseCurve", prefix(k))) == Some("false") {
            return Vec::new();
        }
        param(k).map(sensor_ids).unwrap_or_default()
    };
    let by_pressure = |k: &str| sensors(k).iter().any(|id| id == "pressure");
    let curve_of = |k: &str, id: &str| -> Option<SoftnessCurve> {
        let p = prefix(k);
        if param(&format!("{p}UseSameCurve")) != Some("false") {
            // Its common curve, or (not saved: an older preset) the
            // sensor's own.
            match param(&format!("{p}commonCurve")) {
                Some(common) => parse_curve(common).filter(|c| !is_straight(c)),
                None => param(k).and_then(|xml| sensor_curve(xml, id)),
            }
        } else {
            param(k).and_then(|xml| sensor_curve(xml, id))
        }
    };

    // An option's strength (`…Value`): curve options multiply what their
    // sensors read by it.
    let strength = |option: &str| number(&format!("{option}Value")).unwrap_or(1.0).max(0.0);
    // A sensor as an option reads it: its curve, and its length.
    let read = |k: &str, id: &str| SensorRead {
        curve: curve_of(k, id),
        length: param(k).and_then(|xml| sensor_length(xml, id)),
    };

    // A MyPaint-engine preset keeps the MyPaint brush itself inside,
    // with the size, hardness and opacity its own settings show.
    if engine == "mypaintbrush"
        && let Some(json) = param("MyPaint/json")
    {
        let mut imported = super::mypaint::import(&base64_decode(json)?, &name)?;
        notes.append(&mut imported.notes);
        let mut preset = imported.presets.pop().ok_or("An empty MyPaint brush")?;
        let o = &mut preset.brush.brush_options;
        if let Some(d) = number("MyPaint/diameter") {
            o.diameter = d.clamp(1.0, 3000.0);
        }
        if let Some(h) = number("MyPaint/hardness") {
            o.hardness = (h * 100.0).clamp(0.0, 100.0);
        }
        if let Some(a) = number("MyPaint/opcity") {
            o.opacity = a.clamp(0.0, 1.0);
        }
        if matches!(param("MyPaint/eraser"), Some("1" | "true")) {
            o.blend_mode = crate::brush_engine::brush_options::BlendMode::Eraser;
        }
        preset.name = name;
        return Ok(preset);
    }

    let mut b = Brush::new(40.0, 100.0, Color32::BLACK, 10.0);
    b.brush_options.pressure_size = false;
    // How its Softness option softens the tip, if it does.
    let mut softening = None;
    // The tip.
    if let Some(def) = param("brush_definition") {
        match tip_from_definition(def, &embedded, bundle)? {
            Ok(tip) => {
                let o = &mut b.brush_options;
                o.diameter = tip.diameter;
                o.spacing = tip.spacing;
                o.auto_spacing = tip.auto_spacing;
                o.auto_tip = tip.auto_tip;
                o.hardness = tip.hardness;
                if let Some(curve) = tip.softness {
                    o.softness_selector = crate::brush_engine::hardness::SoftnessSelector::Curve;
                    o.softness_curve = curve;
                }
                softening = tip.softening;
                o.tip_colors = tip.colors;
                if let Some(mapping) = tip.mapping {
                    o.tip_mapping = mapping;
                }
                o.pixel_shape = tip.shape;
                o.extra_tips = tip.extra;
                b.anti_aliasing = tip.antialias;
                b.dynamics.tip.angle = tip.angle;
                b.dynamics.tip.ratio = tip.ratio;
            }
            Err(file) => notes.push(format!("{name}: its tip {file} is missing")),
        }
    }
    match engine.as_str() {
        "paintbrush" | "roundmarker" => {}
        "colorsmudge" => {
            // The preset's own smudge: its smudge rate is always on, its
            // colour rate and smudge radius options.
            let rate = |k: &str| number(k).unwrap_or(0.5).clamp(0.0, 1.0);
            b.mixing = Some(crate::brush_engine::brush_options::Mixing {
                smudge_length: rate("SmudgeRateValue"),
                krita: Some(crate::brush_engine::brush_options::KritaSmudge {
                    // The smudge mode: 1 is dulling.
                    dulling: param("SmudgeRateMode") == Some("1"),
                    smear_alpha: param("SmudgeRateSmearAlpha") != Some("false"),
                    radius: if yes("PressureSmudgeRadius") {
                        number("SmudgeRadiusValue").unwrap_or(0.0).clamp(0.0, 1.0)
                    } else {
                        0.0
                    },
                }),
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
        "sketchbrush" => {
            // Sketch lines join points within the tip's radius, each by
            // its probability, as wide and as cut short at both ends as
            // its settings say; drawn solid, or at random strengths.
            b.brush_type = BrushType::Sketch;
            let s = &mut b.sketch;
            s.reach = (b.brush_options.diameter * 0.5).clamp(5.0, 300.0);
            // Each option's strength scales its setting.
            let k = |option: &str| {
                if yes(&format!("Pressure{option}")) {
                    strength(option)
                } else {
                    1.0
                }
            };
            s.density =
                (number("Sketch/probability").unwrap_or(s.density) * k("Density")).clamp(0.0, 1.0);
            s.thickness = (number("Sketch/lineWidth").unwrap_or(s.thickness) * k("Line width"))
                .clamp(0.5, 100.0);
            s.offset = (number("Sketch/offset").map_or(0.0, |v| v / 100.0) * k("Offset scale"))
                .clamp(0.0, 2.0);
            s.opacity = if yes("Sketch/randomOpacity") {
                0.5
            } else {
                1.0
            };
            for (option, setting) in [
                ("Density", DabSetting::SketchDensity),
                ("Line width", DabSetting::SketchWidth),
                ("Offset scale", DabSetting::SketchOffset),
            ] {
                if !yes(&format!("Pressure{option}")) {
                    continue;
                }
                let key = format!("{option}Sensor");
                for id in sensors(&key) {
                    if !map_sensor(&mut b.inputs, &id, setting, false, read(&key, &id)) {
                        notes.push(unmapped(&name, &option.to_lowercase(), &id));
                    }
                }
            }
        }
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
    const READ: [&str; 23] = [
        "Size",
        "Density",
        "Line width",
        "Offset scale",
        "Softness",
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
        "SmudgeRadius",
        "h",
        "s",
        "v",
        "Darken",
        "Gradient",
        "LightnessStrength",
    ];
    let mut skipped: Vec<&str> = params
        .iter()
        .filter(|(_, v)| v.trim() == "true")
        .filter_map(|(k, _)| k.strip_prefix("Pressure"))
        .filter(|o| !READ.contains(o) && !o.is_empty())
        // Mix is read for a plain colour (a gradient's position otherwise);
        // Rate only times the airbrush (`effectiveTiming`).
        .filter(|&o| match o {
            "Mix" => param("ColorSource/Type").is_some_and(|t| t != "plain"),
            "Rate" => yes("PaintOpSettings/isAirbrushing"),
            _ => true,
        })
        .collect();
    skipped.sort_unstable();
    if !skipped.is_empty() {
        let skipped: Vec<&str> = skipped.into_iter().map(option_name).collect();
        notes.push(format!(
            "{name}: Krita's {} option{} didn't come across",
            skipped.join(", "),
            if skipped.len() > 1 { "s" } else { "" }
        ));
    }
    // Several sensors on one option: the preset multiplies what they read, or
    // (its `…curveMode`) adds them, takes the most, the least or the
    // difference; inputs here multiply.
    let mut combined: Vec<(&str, &str)> = params
        .iter()
        .filter_map(|(k, v)| {
            let option = k.strip_suffix("curveMode")?;
            let how = match v.trim() {
                "1" => "adding",
                "2" => "taking the highest of",
                "3" => "taking the lowest of",
                "4" => "taking the difference of",
                _ => return None,
            };
            (yes(&format!("Pressure{option}")) && sensors(&format!("{option}Sensor")).len() > 1)
                .then_some((option, how))
        })
        .collect();
    combined.sort_unstable();
    for (option, how) in combined {
        notes.push(format!(
            "{name}: its {} combines its sensors by {how} them, here by multiplying",
            option_name(option)
        ));
    }
    // The painting mode: 1 build up, 2 wash (the stroke never past its
    // opacity, however its dabs overlap).
    if param("PaintOpAction") == Some("2") {
        b.brush_options.painting_mode = crate::brush_engine::brush_options::PaintingMode::Wash;
    }
    // Size, opacity and flow: pressure through its curve, any other
    // sensor (random, speed…) as an input mapping.
    for (option, setting) in [
        ("Size", DabSetting::Size),
        ("Opacity", DabSetting::Opacity),
        ("Flow", DabSetting::Flow),
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
                *curve = curve_of(&key, "pressure");
            } else if !map_sensor(&mut b.inputs, &id, setting, false, read(&key, &id)) {
                notes.push(unmapped(&name, option, &id));
            }
        }
        // Its strength scales the size (opacity's and flow's are their
        // values, read above).
        if option == "Size" {
            let o = &mut b.brush_options;
            o.diameter = (o.diameter * strength("Size")).clamp(1.0, 3000.0);
        }
    }
    // Squash (the ratio: low values flatten the tip), its strength
    // squashing it to begin with.
    if yes("PressureRatio") {
        let tip = &mut b.dynamics.tip;
        tip.ratio = (tip.ratio * strength("Ratio")).clamp(0.02, 1.0);
        for id in sensors("RatioSensor") {
            let read = read("RatioSensor", &id);
            if !map_sensor(&mut b.inputs, &id, DabSetting::Squash, true, read) {
                notes.push(unmapped(&name, "ratio", &id));
            }
        }
    }
    // Scatter: up to its value in brush widths, either way,
    // like the jitter here (a percentage), times its sensors.
    if yes("PressureScatter") {
        let widths = strength("Scatter").clamp(0.0, 5.0);
        let ids = sensors("ScatterSensor");
        match &ids[..] {
            [] => b.jitter = widths * 100.0,
            // One sensor: as much scatter as it reads (the inputs add, a
            // brush width each at most).
            [id] if krita_sensor(id).is_some() => {
                let mut left = widths;
                while left > 1e-3 {
                    map_sensor(
                        &mut b.inputs,
                        id,
                        DabSetting::Scatter,
                        false,
                        read("ScatterSensor", id),
                    );
                    b.inputs.last_mut().expect("just added").amount = left.min(1.0);
                    left -= 1.0;
                }
            }
            _ => {
                b.jitter = widths * 100.0;
                notes.push(format!(
                    "{name}: its scatter follows {}, here it's always full",
                    ids.join(" and ")
                ));
            }
        }
    }
    // Pressure spacing, its strength spacing the dabs to begin with.
    if yes("PressureSpacing") {
        let o = &mut b.brush_options;
        o.spacing = (o.spacing * strength("Spacing")).clamp(1.0, 1000.0);
        if let Some(coeff) = &mut o.auto_spacing {
            *coeff = (*coeff * strength("Spacing")).clamp(0.01, 10.0);
        }
        // Pressure through its curve, any other sensor as an input.
        for id in sensors("SpacingSensor") {
            if id == "pressure" {
                let o = &mut b.brush_options;
                o.pressure_spacing = true;
                o.pressure_curves.spacing = curve_of("SpacingSensor", "pressure");
            } else {
                let read = read("SpacingSensor", &id);
                if !map_sensor(&mut b.inputs, &id, DabSetting::Spacing, false, read) {
                    notes.push(unmapped(&name, "spacing", &id));
                }
            }
        }
    }
    // Lightness strength (a lightness-mapped tip), by its sensors.
    if yes("PressureLightnessStrength") {
        for id in sensors("LightnessStrengthSensor") {
            let read = read("LightnessStrengthSensor", &id);
            if !map_sensor(&mut b.inputs, &id, DabSetting::Lightness, false, read) {
                notes.push(unmapped(&name, "lightness strength", &id));
            }
        }
    }
    // Rotation: each sensor as this app's counterpart. Its other sensors
    // swing the tip both ways, up to its strength × 180°, as does its
    // randomness.
    if yes("PressureRotation") {
        let swing = strength("Rotation").clamp(0.0, 1.0);
        for id in sensors("RotationSensor") {
            let tip = &mut b.dynamics.tip;
            match id.as_str() {
                "drawingangle" => tip.follow_stroke = true,
                "fuzzy" | "fuzzystroke" => tip.random_angle = 180.0 * swing,
                "ascension" => tip.follow_tilt = true,
                "rotation" => tip.follow_barrel = true,
                other => {
                    let read = read("RotationSensor", other);
                    if map_sensor(&mut b.inputs, other, DabSetting::Angle, false, read) {
                        let m = b.inputs.last_mut().expect("just added");
                        m.both_ways = true;
                        m.amount = swing;
                    } else {
                        notes.push(unmapped(&name, "rotation", other));
                    }
                }
            }
        }
    }
    // Mirror: a dab flips when its sensors (multiplied) reach half;
    // random ones alone flip half the dabs, at random.
    if yes("PressureMirror") {
        b.dynamics.tip.random_flip_x = yes("HorizontalMirrorEnabled");
        b.dynamics.tip.random_flip_y = yes("VerticalMirrorEnabled");
        let ids = sensors("MirrorSensor");
        if ids.is_empty() {
            // Its curve off: its value alone (full), every dab flipped.
            b.inputs.push(InputMapping {
                setting: DabSetting::Mirror,
                curve: SoftnessCurve {
                    points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)],
                },
                ..Default::default()
            });
        } else if ids.iter().any(|id| !id.starts_with("fuzzy")) {
            let first = b.inputs.len();
            for id in &ids {
                let read = read("MirrorSensor", id);
                if !map_sensor(&mut b.inputs, id, DabSetting::Mirror, false, read) {
                    notes.push(unmapped(&name, "mirroring", id));
                }
            }
            // Its strength scales what reaches the half.
            if let Some(m) = b.inputs.get_mut(first) {
                m.curve = scaled(&m.curve, strength("Mirror"));
            }
        }
    }
    // Softness: generated round and square tips get softer (a smaller
    // solid core, or a soft circle's curve lowered); Gaussian and picture
    // tips don't.
    if yes("PressureSoftness")
        && let Some(softening) = softening
    {
        let o = &mut b.brush_options;
        // A hard circle: its falloff as a curve, to soften.
        if o.softness_selector != crate::brush_engine::hardness::SoftnessSelector::Curve {
            o.softness_selector = crate::brush_engine::hardness::SoftnessSelector::Curve;
            o.softness_curve = softening.falloff(&SoftnessCurve::default(), 1.0);
        }
        // Its strength softens it to begin with.
        let k = strength("Softness").clamp(0.1, 1.0);
        o.softening = match softening {
            Softening::Fade(f) => Softening::Fade(f * k),
            Softening::SquaredCurve(c) => Softening::SquaredCurve(c.softened(k)),
            Softening::Curve => Softening::Curve,
        };
        o.softness_curve = o.softening.falloff(&o.softness_curve, 1.0);
        for id in sensors("SoftnessSensor") {
            let read = read("SoftnessSensor", &id);
            if !map_sensor(&mut b.inputs, &id, DabSetting::Softness, false, read) {
                notes.push(unmapped(&name, "softness", &id));
            }
        }
    } else if yes("PressureSoftness") && b.brush_options.auto_tip.has_fade() {
        // Different fades across and down: the fades soften.
        let k = strength("Softness").clamp(0.1, 1.0);
        let fade = &mut b.brush_options.auto_tip.fade;
        *fade = fade.map(|f| f * k);
        for id in sensors("SoftnessSensor") {
            let read = read("SoftnessSensor", &id);
            if !map_sensor(&mut b.inputs, &id, DabSetting::Softness, false, read) {
                notes.push(unmapped(&name, "softness", &id));
            }
        }
    }
    // Sharpness: a dab's pixels above `1 - value` go solid, the rest
    // clear (hard edges).
    if yes("PressureSharpness") {
        b.sharpness = (1.0 - number("SharpnessValue").unwrap_or(1.0)).clamp(0.01, 1.0);
        // Its threshold follows its sensors (pressure, mostly).
        for id in sensors("SharpnessSensor") {
            let read = read("SharpnessSensor", &id);
            if !map_sensor(&mut b.inputs, &id, DabSetting::Sharpness, false, read) {
                notes.push(unmapped(&name, "hard edges", &id));
            }
        }
        // Its soft band, in percent (an older setting as a share).
        b.sharpness_softness = number("Sharpness/softness")
            .map(|v| v / 100.0)
            .or(number("Sharpness/factor"))
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
    }
    // Hue, saturation and value by sensor: either
    // way of the colour, by the option's strength.
    for (option, setting) in [
        ("h", DabSetting::Hue),
        ("s", DabSetting::Saturation),
        ("v", DabSetting::Value),
    ] {
        if !yes(&format!("Pressure{option}")) {
            continue;
        }
        let strength = number(&format!("{option}Value")).unwrap_or(1.0);
        let key = format!("{option}Sensor");
        for id in sensors(&key) {
            if map_sensor(&mut b.inputs, &id, setting, false, read(&key, &id)) {
                let m = b.inputs.last_mut().expect("just added");
                m.amount = strength.clamp(-1.0, 1.0);
                m.both_ways = true;
            } else {
                notes.push(unmapped(&name, option_name(option), &id));
            }
        }
    }
    // Darken: the colour times one less the option's value.
    if yes("PressureDarken") {
        let strength = number("DarkenValue").unwrap_or(1.0);
        for id in sensors("DarkenSensor") {
            let read = read("DarkenSensor", &id);
            if map_sensor(&mut b.inputs, &id, DabSetting::Darken, false, read) {
                b.inputs.last_mut().expect("just added").amount = -strength.clamp(0.0, 1.0);
            } else {
                notes.push(unmapped(&name, "darken", &id));
            }
        }
    }
    // The colour source: a gradient is the brush colour to the secondary
    // (the default foreground-to-background one) by its option.
    match param("ColorSource/Type").unwrap_or("plain") {
        // Mix: the colour is the background to the foreground by the
        // option's value, its strength times its sensors'; here, toward the
        // secondary
        // colour by one less that value.
        "plain" if yes("PressureMix") => {
            let strength = number("MixValue").unwrap_or(1.0).clamp(0.0, 1.0);
            let toward = |curve: SoftnessCurve| SoftnessCurve {
                points: curve
                    .points
                    .iter()
                    .map(|p| CurvePoint::new(p.x, 1.0 - strength * p.y))
                    .collect(),
            };
            match sensors("MixSensor").as_slice() {
                // No sensor: the strength alone, the same whatever the pen.
                [] if strength < 1.0 => b.inputs.push(InputMapping {
                    setting: DabSetting::ColorMix,
                    curve: toward(SoftnessCurve {
                        points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)],
                    }),
                    ..Default::default()
                }),
                [] => {}
                [id] => {
                    let mut curve =
                        curve_of("MixSensor", id).unwrap_or_else(|| InputMapping::default().curve);
                    if id == "speed" {
                        curve = krita_speed(&curve);
                    }
                    let mut read = read("MixSensor", id);
                    read.curve = None;
                    if map_sensor(&mut b.inputs, id, DabSetting::ColorMix, false, read) {
                        b.inputs.last_mut().expect("just added").curve = toward(curve);
                    } else {
                        notes.push(unmapped(&name, "mix", id));
                    }
                }
                _ => notes.push(format!(
                    "{name}: its Mix follows several of Krita's sensors together, which \
                     doesn't come across"
                )),
            }
        }
        "plain" => {}
        "gradient" => {
            if yes("PressureGradient") {
                for id in sensors("GradientSensor") {
                    let read = read("GradientSensor", &id);
                    if !map_sensor(&mut b.inputs, &id, DabSetting::ColorMix, false, read) {
                        notes.push(unmapped(&name, "gradient", &id));
                    }
                }
            }
        }
        "uniform_random" => {
            b.brush_options.color_source =
                crate::brush_engine::brush_options::ColorSource::UniformRandom;
        }
        "total_random" => {
            b.brush_options.color_source =
                crate::brush_engine::brush_options::ColorSource::TotalRandom;
        }
        other => notes.push(format!(
            "{name}: its colour comes from Krita's {other} source, which has no counterpart here"
        )),
    }
    // The paper texture, from the bundle, or else the copy saved in
    // the preset (`Texture/Pattern/Pattern`: a PNG, base64 twice over).
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
            .or_else(|| {
                let once = base64_decode(param("Texture/Pattern/Pattern")?).ok()?;
                let png = std::str::from_utf8(&once)
                    .ok()
                    .and_then(|text| base64_decode(text).ok())
                    .unwrap_or(once);
                image::load_from_memory(&png).ok()
            })
            .map(|img| {
                // The grain as it is (not stretched to the full range), its
                // brightness taken off and contrast about the middle.
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
                let krita_mode = number("Texture/Pattern/TexturingMode").unwrap_or(0.0) as u8;
                // The nearest of this app's own modes (shown, and used if
                // the preset's formula is turned off); its formula paints.
                let mode = match krita_mode {
                    1 => TextureMode::Subtract,
                    6 | 8 => TextureMode::ColorDodge,
                    10 | 11 => TextureMode::HardMix,
                    12..=15 => TextureMode::Height,
                    _ => TextureMode::Multiply,
                };
                if !crate::brush_engine::texture::KritaTexturing::supports(krita_mode) {
                    notes.push(format!(
                        "{name}: its texture colours the dab (Krita's lightness or gradient \
                         texturing), which this app doesn't; multiplied instead"
                    ));
                }
                b.texture = Some(BrushTexture {
                    pattern,
                    mode,
                    scale: number("Texture/Pattern/Scale")
                        .unwrap_or(1.0)
                        .clamp(0.05, 16.0),
                    strength: number("Texture/Strength/Value")
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0),
                    invert: yes("Texture/Pattern/Invert"),
                    placement: crate::brush_engine::texture::GrainPlacement {
                        random_offset: yes("Texture/Pattern/isRandomOffsetX")
                            || yes("Texture/Pattern/isRandomOffsetY"),
                        ..Default::default()
                    },
                    // The preset's own formula for its mode.
                    krita: crate::brush_engine::texture::KritaTexturing::supports(krita_mode)
                        .then_some(crate::brush_engine::texture::KritaTexturing {
                            mode: krita_mode,
                            soft: yes("Texture/Pattern/UseSoftTexturing"),
                        }),
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
                        read("Texture/Strength/Sensor", "pressure"),
                    );
                }
            }
            None => notes.push(format!("{name}: its texture {base} is missing")),
        }
    }
    // The masking brush is this app's dual brush.
    if yes("MaskingBrush/Enabled")
        && let Some(def) = param("MaskingBrush/Preset/brush_definition")
    {
        use crate::brush_engine::dual::{DualMode, DualTip};
        match tip_from_definition(def, &embedded, bundle)? {
            Ok(tip) => {
                let mode = match param("MaskingBrush/MaskingCompositeOp").unwrap_or("multiply") {
                    "multiply" => DualMode::Multiply,
                    "darken" => DualMode::Darken,
                    "subtract" | "linear_burn" => DualMode::Subtract,
                    "burn" => DualMode::Burn,
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
    /// A soft circle: its falloff as a curve (strength by distance
    /// from the centre), rather than a fade.
    softness: Option<SoftnessCurve>,
    /// A lightness or gradient map (painting by the picture's grey).
    mapping: Option<crate::brush_engine::brush_options::TipMapping>,
    /// How the Softness option softens it: none for Gaussian and
    /// picture tips, which it leaves alone.
    softening: Option<Softening>,
    /// A generated tip's Anti-aliasing box (picture tips are always smooth).
    antialias: bool,
    /// Auto spacing's coefficient, when on.
    auto_spacing: Option<f32>,
    /// A generated tip's spikes, fades, density and randomness.
    auto_tip: crate::brush_engine::brush_options::AutoTip,
}

/// The tip a `brush_definition` describes: a generated round or square one,
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
        mapping: None,
        softening: None,
        antialias: true,
        auto_spacing: None,
        auto_tip: Default::default(),
    };
    if brush.attr("type") == Some("auto_brush") {
        if let Some(mask) = brush.find("MaskGenerator") {
            // Krita reads a missing flag as off.
            tip.antialias = mask.attr("antialiasEdges") == Some("1");
            let m = |k: &str| mask.attr(k).and_then(|v| v.parse::<f32>().ok());
            let diameter = m("diameter").or(m("radius").map(|r| r * 2.0));
            tip.diameter = diameter.unwrap_or(40.0).clamp(1.0, 3000.0);
            tip.ratio = m("ratio").unwrap_or(1.0).clamp(0.02, 1.0);
            // The falloff, sampled as a curve (strength by distance
            // from the centre, 0..1), and how its Softness option softens
            // it.
            let (fh, fv) = (m("hfade").unwrap_or(1.0), m("vfade").unwrap_or(1.0));
            tip.hardness = 100.0;
            tip.auto_tip.spikes = m("spikes").map_or(2, |s| s.round().clamp(2.0, 200.0) as u32);
            tip.auto_tip.density = attr("density").unwrap_or(1.0).clamp(0.0, 1.0);
            tip.auto_tip.randomness = attr("randomness").unwrap_or(0.0).clamp(0.0, 1.0);
            match mask.attr("id").unwrap_or("default") {
                // A Gaussian tip ignores softness.
                "gauss" => tip.softness = Some(gauss_falloff(fh, fv)),
                // A soft circle's falloff is its curve, whatever the fade
                // (an airbrush: faint all over, gone at the edge), read at
                // the squared distance from the centre.
                "soft" => {
                    tip.softening = mask
                        .attr("softness_curve")
                        .and_then(parse_curve)
                        .map(Softening::SquaredCurve);
                }
                // Different fades across and down: the tip's own fades.
                _ if (fh - fv).abs() > 1e-3 => {
                    tip.auto_tip.fade = [fh.clamp(0.0, 1.0), fv.clamp(0.0, 1.0)];
                }
                _ => tip.softening = Some(Softening::Fade(fh.min(fv).clamp(0.0, 1.0))),
            }
            // A hard circle (solid to the edge) paints as one, unless
            // softness comes into it.
            if let Some(softening) = &tip.softening
                && !matches!(softening, Softening::Fade(f) if *f >= 0.999)
            {
                tip.softness = Some(softening.falloff(&SoftnessCurve::default(), 1.0));
            }
            if mask.attr("type") == Some("rect") {
                tip.shape = PixelBrushShape::Square;
            }
        }
        auto_spacing(&mut tip, brush);
        return Ok(Ok(tip));
    }
    let file = brush.attr("filename").unwrap_or_default();
    // The brush application: 0 a mask, 1 its colours, 2 a lightness
    // map, 3 a gradient map (the last two paint by the picture's grey).
    let application = brush.attr("brushApplication");
    tip.mapping = match application {
        Some("2") => Some(crate::brush_engine::brush_options::TipMapping::Lightness),
        Some("3") => Some(crate::brush_engine::brush_options::TipMapping::Gradient),
        _ => None,
    };
    let Some((mask, extra)) = picture_tip(file, embedded, bundle, tip.mapping.is_some()) else {
        return Ok(Err(file.to_string()));
    };
    tip.diameter =
        (mask.width.max(mask.height) as f32 * attr("scale").unwrap_or(1.0)).clamp(1.0, 3000.0);
    // A colour picture paints its colours unless it's used as a mask
    // (newer presets say how in `brushApplication`: 1 stamps the picture).
    tip.colors = mask.has_colors()
        && match application {
            Some(application) => application != "0",
            None => brush.attr("ColorAsMask") != Some("1"),
        };
    tip.shape = PixelBrushShape::Custom(mask);
    tip.extra = extra;
    auto_spacing(&mut tip, brush);
    Ok(Ok(tip))
}

/// A Gaussian round tip: its
/// strength by distance from the centre (0 centre, 1 edge).
fn gauss_falloff(fh: f32, fv: f32) -> SoftnessCurve {
    let fade = (1.0 - (fh + fv) as f64 / 2.0).clamp(1e-6, 1.0 - 1e-6);
    let center = 2.5 * (6761.0 * fade - 10000.0) / (std::f64::consts::SQRT_2 * 6761.0 * fade);
    let scale = std::f64::consts::SQRT_2 * 12500.0 / (6761.0 * fade);
    let norm = 2.0 * erf(center);
    sampled(|r: f32| {
        let d = r as f64 * scale;
        ((erf(d + center) - erf(d - center)) / norm) as f32
    })
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

/// Auto spacing: `coeff × √size` pixels apart (for its size), as
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
    tip.auto_spacing = Some(coeff.clamp(0.01, 10.0));
}

/// A tip picture by file name, from the preset or the bundle: the tip,
/// and a `.gih`'s other cells.
fn picture_tip(
    file: &str,
    embedded: &HashMap<String, Vec<u8>>,
    bundle: &HashMap<String, Vec<u8>>,
    keep_colors: bool,
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
    if keep_colors {
        return Some((TipMask::from_image_keeping_colors(&img), Vec::new()));
    }
    // Dark paints, whatever is round the edges.
    Some((TipMask::from_image_with(&img, Some(true)), Vec::new()))
}

/// This app's counterpart of a preset's sensor (by its id).
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
        "pressurein" => Sensor::PressureIn,
        "fade" => Sensor::Fade,
        "perspective" => Sensor::Perspective,
        "xtilt" => Sensor::XTilt,
        "ytilt" => Sensor::YTilt,
        _ => return None,
    })
}

/// How an option reads one of its sensors.
struct SensorRead {
    /// Its curve, if it isn't the straight line.
    curve: Option<SoftnessCurve>,
    /// Its length (this app's units) and whether it repeats, for fade,
    /// distance and time.
    length: Option<(f32, bool)>,
}

/// A preset's full speed: 30 view pixels a millisecond, in screen points
/// a second.
const KRITA_FULL_SPEED: f32 = 30_000.0;

/// A preset's tilt elevation reads 0 from this lean (60°) on.
const KRITA_MAX_TILT: f32 = std::f32::consts::FRAC_PI_3;

/// A preset's speed curve against this app's speed input: the preset's
/// reads 1 at its full speed (above), this app's at `FAST_SPEED`, so the
/// same speed is a smaller share there.
fn krita_speed(curve: &SoftnessCurve) -> SoftnessCurve {
    let share = crate::brush_engine::dynamics::FAST_SPEED / KRITA_FULL_SPEED;
    resampled(|x| curve.eval(x * share))
}

/// `f` (this app's input → the curve's value) as a curve, finely: preset
/// curves often turn in a small part of the range, and some readings
/// wrap round.
fn resampled(f: impl Fn(f32) -> f32) -> SoftnessCurve {
    SoftnessCurve {
        points: (0..=128)
            .map(|i| {
                let x = i as f32 / 128.0;
                CurvePoint::new(x, f(x).clamp(0.0, 1.0))
            })
            .collect(),
    }
}

/// `curve` with its values times `k` (its shape kept: the spline scales
/// with its points).
fn scaled(curve: &SoftnessCurve, k: f32) -> SoftnessCurve {
    SoftnessCurve {
        points: curve
            .points
            .iter()
            .map(|p| CurvePoint::new(p.x, (p.y * k).clamp(0.0, 1.0)))
            .collect(),
    }
}

/// A mapping made from a preset's sensor, its curve put on this app's
/// reading of the same thing:
/// - speed: the preset's full speed is 12 times this app's;
/// - tilt elevation: the preset's is 1 upright and 0 from 60° of lean on,
///   this app's tilt 0 upright and 1 flat;
/// - tilt direction: the preset's starts leaning down the screen at a half
///   and turns clockwise (`atan2(-xTilt, yTilt)`), this app's starts
///   leaning right and turns counter-clockwise;
/// - drawing angle: the preset's curve reads the direction clockwise from
///   rightward, this app's counter-clockwise; and its result is half a
///   turn on, which options that scale take as it is and rotation and
///   hue (which swing) take both ways.
fn krita_scale(m: &mut InputMapping) {
    let c = m.curve.clone();
    match m.sensor {
        Sensor::Speed => m.curve = krita_speed(&c),
        Sensor::Tilt => {
            m.curve = resampled(|lean| {
                let tilt = lean.clamp(0.0, 1.0).asin();
                let elevation =
                    (tilt / KRITA_MAX_TILT).min(1.0).acos() / std::f32::consts::FRAC_PI_2;
                c.eval(elevation)
            })
        }
        Sensor::TiltDirection => m.curve = resampled(|a| c.eval((0.25 - a).rem_euclid(1.0))),
        Sensor::Direction => {
            // Rotation and hue swing by it; the rest scale.
            if matches!(m.setting, DabSetting::Angle | DabSetting::Hue) {
                m.both_ways = true;
                m.curve = resampled(|a| c.eval((1.0 - a).rem_euclid(1.0)));
            } else {
                m.curve = resampled(|a| (c.eval((1.0 - a).rem_euclid(1.0)) + 0.5).rem_euclid(1.0));
            }
        }
        _ => {}
    }
}

/// The preset's `id` sensor driving `setting` through its curve (straight when
/// `None`), as an input mapping (full amount; `inverted`: a high input
/// reduces it). Whether it has one here.
fn map_sensor(
    inputs: &mut Vec<InputMapping>,
    id: &str,
    setting: DabSetting,
    inverted: bool,
    read: SensorRead,
) -> bool {
    let SensorRead { curve, length } = read;
    let Some(sensor) = krita_sensor(id) else {
        return false;
    };
    let mut mapping = InputMapping {
        sensor,
        setting,
        amount: 1.0,
        ..Default::default()
    };
    if let Some((length, periodic)) = length {
        mapping.length = length;
        mapping.periodic = periodic;
    }
    if let Some(curve) = curve {
        mapping.curve = curve;
    }
    krita_scale(&mut mapping);
    if inverted {
        for p in &mut mapping.curve.points {
            p.y = 1.0 - p.y;
        }
    }
    inputs.push(mapping);
    true
}

/// A preset option's name as its settings show it (its key is terse).
fn option_name(key: &str) -> &str {
    match key {
        "h" => "hue",
        "s" => "saturation",
        "v" => "value",
        "SmudgeRadius" => "smudge radius",
        "SmudgeRate" => "smudge length",
        "ColorRate" => "colour rate",
        "LightnessStrength" => "lightness strength",
        "PaintThickness" => "paint thickness",
        "Texture/Strength/" => "texture strength",
        other => other,
    }
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

/// Sensor `id`'s own curve (`<curve>0,0;0.5,0.2;1,1;</curve>` inside the
/// option's sensor XML), if it isn't the straight line.
fn sensor_curve(xml: &str, id: &str) -> Option<SoftnessCurve> {
    let root = parse_xml(xml).ok()?;
    // That sensor's (alone, or among several).
    let sensor = std::iter::once(&root)
        .chain(root.descendants("params"))
        .chain(root.descendants("ChildSensor"))
        .find(|n| n.attr("id") == Some(id))?;
    let curve = parse_curve(&sensor.descendants("curve").next()?.text)?;
    (!is_straight(&curve)).then_some(curve)
}

/// Sensor `id`'s length (`length`, or `duration` for time) and whether it
/// repeats (`periodic`), with the format's defaults; in this app's units
/// (time in seconds, the preset's in milliseconds). `None` for a sensor without one.
fn sensor_length(xml: &str, id: &str) -> Option<(f32, bool)> {
    let (tag, default, per_unit) = match id {
        "fade" => ("length", 1000.0, 1.0),
        "distance" => ("length", 30.0, 1.0),
        "time" => ("duration", 30.0, 1000.0),
        _ => return None,
    };
    let root = parse_xml(xml).ok();
    let sensor = root.as_ref().and_then(|root| {
        std::iter::once(root)
            .chain(root.descendants("params"))
            .chain(root.descendants("ChildSensor"))
            .find(|n| n.attr("id") == Some(id))
    });
    let attr = |k: &str| sensor.and_then(|n| n.attr(k));
    let length = attr(tag)
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(default)
        .max(1.0);
    Some((
        length / per_unit,
        attr("periodic").is_some_and(|v| v == "1"),
    ))
}

/// The straight line, `0,0` to `1,1` (as good as no curve).
fn is_straight(curve: &SoftnessCurve) -> bool {
    curve.points.len() == 2
        && curve.points[0] == CurvePoint::new(0.0, 0.0)
        && curve.points[1] == CurvePoint::new(1.0, 1.0)
}

/// A preset's curve text, `x,y;x,y;…` (0..1 each), as a curve; `None` with
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

/// A small XML reader (elements, attributes, text): enough for `.kpp`
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
        // Fade 0.25: solid to a quarter of the way out, then the
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

    /// Standard base64, for embedded resources.
    fn base64(bytes: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in bytes.chunks(3) {
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
    }

    #[test]
    fn a_texture_missing_from_the_bundle_comes_from_the_preset_s_copy() {
        let mut png = Vec::new();
        image::GrayImage::from_pixel(4, 4, image::Luma([128]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let brush = with_params(
            "paintbrush",
            &[
                ("Texture/Pattern/Enabled", "true"),
                (
                    "Texture/Pattern/PatternFileName",
                    "C:/krita/patterns/paper3.pat",
                ),
                ("Texture/Pattern/Pattern", &base64(base64(&png).as_bytes())),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let texture = imported.presets[0].brush.texture.as_ref();
        assert_eq!(texture.map(|t| t.pattern.name.as_str()), Some("paper3.pat"));
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    }

    #[test]
    fn mix_moves_toward_the_secondary_colour_and_rate_only_times_the_airbrush() {
        let mix = |extra: &[(&str, &str)]| {
            let mut params = vec![("PressureMix", "true"), ("MixValue", "0.8")];
            params.extend_from_slice(extra);
            import_kpp(&with_params("paintbrush", &params), "file").unwrap()
        };
        // By pressure: a full press is the strength's share of the brush
        // colour, none the secondary.
        let imported = mix(&[("MixSensor", PRESSURE)]);
        let m = &imported.presets[0].brush.inputs[0];
        assert_eq!(
            (m.sensor, m.setting),
            (Sensor::Pressure, DabSetting::ColorMix)
        );
        assert!((m.curve.eval(0.0) - 1.0).abs() < 1e-3);
        assert!((m.curve.eval(1.0) - 0.2).abs() < 1e-3);
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        // Its curve off: the strength alone, whatever the pen.
        let imported = mix(&[("MixSensor", PRESSURE), ("MixUseCurve", "false")]);
        let m = &imported.presets[0].brush.inputs[0];
        assert!((m.curve.eval(0.0) - 0.2).abs() < 1e-3);
        assert!((m.curve.eval(1.0) - 0.2).abs() < 1e-3);
        // A gradient source: not read.
        let imported = mix(&[("ColorSource/Type", "gradient")]);
        assert!(imported.notes[0].contains("Mix"), "{:?}", imported.notes);
        // Rate: nothing without the airbrush.
        let rate = |airbrush: &str| {
            let params = [
                ("PressureRate", "true"),
                ("PaintOpSettings/isAirbrushing", airbrush),
            ];
            import_kpp(&with_params("paintbrush", &params), "file")
                .unwrap()
                .notes
        };
        assert!(rate("false").is_empty());
        assert!(rate("true")[0].contains("Rate"));
    }

    #[test]
    fn pressure_in_is_the_highest_pressure_so_far() {
        let brush = with_params(
            "paintbrush",
            &[(
                "SizeSensor",
                r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="pressurein"/></params>"#,
            )],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let b = &imported.presets[0].brush;
        assert_eq!(b.inputs[0].sensor, Sensor::PressureIn);
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
    }

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
                ("hSensor", FUZZY),
                ("hValue", "0.5"),
                ("PressurePaintThickness", "true"),
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
        // Auto spacing: 0.5 × √size (3 px of 36).
        assert_eq!(o.auto_spacing, Some(0.5));
        assert!(
            (o.spacing - 100.0 * 3.0 / 36.0).abs() < 1e-3,
            "{}",
            o.spacing
        );
        // Half a brush width either way.
        assert_eq!(b.jitter, 50.0);
        // Hue by a random input, either way, by its strength.
        let hue = b
            .inputs
            .iter()
            .find(|m| m.setting == DabSetting::Hue)
            .expect("hue");
        assert_eq!(
            (hue.sensor, hue.amount, hue.both_ways),
            (Sensor::RandomDab, 0.5, true)
        );
        // An option with no counterpart is named in the report.
        assert!(
            imported
                .notes
                .iter()
                .any(|n| n.contains("paint thickness option")),
            "{:?}",
            imported.notes
        );
    }

    /// A preset's XML with `params` added.
    #[test]
    fn spikes_fades_grain_tilt_flow_and_random_colour_come_across() {
        const XTILT: &str = r#"<!DOCTYPE params><params id="xtilt"/>"#;
        let xml = AUTO
            .replace(
                r#"spacing="0.08" angle="0.5""#,
                r#"spacing="0.08" angle="0.5" density="0.7" randomness="0.3""#,
            )
            .replace(
                r#"hfade="0.25" vfade="0.25""#,
                r#"hfade="0.25" vfade="0.75" spikes="5""#,
            );
        let brush = with_params_xml(
            &xml,
            &[
                ("PressureFlow", "true"),
                ("FlowSensor", XTILT),
                ("ColorSource/Type", "total_random"),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let b = &imported.presets[0].brush;
        let t = b.brush_options.auto_tip;
        assert_eq!(
            (t.spikes, t.fade, t.density, t.randomness),
            (5, [0.25, 0.75], 0.7, 0.3)
        );
        assert_eq!(
            b.brush_options.color_source,
            crate::brush_engine::brush_options::ColorSource::TotalRandom
        );
        assert!(
            b.inputs
                .iter()
                .any(|m| (m.sensor, m.setting) == (Sensor::XTilt, DabSetting::Flow)),
            "{:?}",
            b.inputs
        );
    }

    fn with_params_xml(xml: &str, params: &[(&str, &str)]) -> Vec<u8> {
        let extra: String = params
            .iter()
            .map(|(k, v)| format!(r#"<param type="string" name="{k}"><![CDATA[{v}]]></param>"#))
            .collect();
        kpp(&xml.replace("</Preset>", &format!("{extra}</Preset>")))
    }

    #[test]
    fn sensors_follow_krita_s_use_curve_and_common_curve() {
        const CURVED: &str =
            r#"<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>"#;
        let base = AUTO.replace(
            r#"<param type="string" name="SizeSensor"><![CDATA[<!DOCTYPE params><params id="pressure"><curve>0,0;0.5,0.2;1,1;</curve></params>]]></param>"#,
            "",
        );
        let import = |params: &[(&str, &str)]| {
            let imported = import_kpp(&with_params_xml(&base, params), "file").unwrap();
            imported.presets[0].brush.brush_options.clone()
        };
        // Its curve off: the option's value alone, no pressure.
        let o = import(&[("SizeSensor", CURVED), ("SizeUseCurve", "false")]);
        assert!(!o.pressure_size);
        // The same curve for all: the common one, not the sensor's.
        let o = import(&[
            ("SizeSensor", CURVED),
            ("SizeUseSameCurve", "true"),
            ("SizecommonCurve", "0,0;0.5,0.8;1,1;"),
        ]);
        let c = o.pressure_curves.size.expect("a curve");
        assert!((c.eval(0.5) - 0.8).abs() < 1e-3, "{}", c.eval(0.5));
        // Not the same: the sensor's own.
        let o = import(&[
            ("SizeSensor", CURVED),
            ("SizeUseSameCurve", "false"),
            ("SizecommonCurve", "0,0;0.5,0.8;1,1;"),
        ]);
        let c = o.pressure_curves.size.expect("a curve");
        assert!((c.eval(0.5) - 0.2).abs() < 1e-3, "{}", c.eval(0.5));
    }

    #[test]
    fn a_lightness_map_tip_comes_across() {
        let png = {
            let mut bytes = Vec::new();
            let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([128, 128, 128, 255]));
            image::DynamicImage::ImageRgba8(img)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        };
        let xml = r#"<Preset paintopid="paintbrush" name="Map" embedded_resources="0">
            <param type="string" name="brush_definition"><![CDATA[<Brush type="png_brush" filename="tip.png" spacing="0.1" brushApplication="2"/>]]></param>
            </Preset>"#;
        let bundle: HashMap<String, Vec<u8>> = [("tip.png".to_string(), png)].into();
        let mut notes = Vec::new();
        let p = read_kpp(&kpp(xml), "file", &bundle, &mut notes).unwrap();
        let o = &p.brush.brush_options;
        assert!(o.tip_colors, "{notes:?}");
        assert_eq!(
            o.tip_mapping,
            crate::brush_engine::brush_options::TipMapping::Lightness
        );
    }

    #[test]
    fn the_anti_aliasing_box_comes_across() {
        let aa = |xml: &str| {
            import_kpp(&kpp(xml), "file").unwrap().presets[0]
                .brush
                .anti_aliasing
        };
        // Krita reads a missing flag as off.
        assert!(!aa(AUTO));
        assert!(aa(&AUTO.replace(
            r#"type="circle""#,
            r#"antialiasEdges="1" type="circle""#
        )));
    }

    #[test]
    fn a_soft_circle_keeps_its_falloff_curve() {
        // An airbrush: faint in the middle, nothing at the edge, and
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
        // Its curve is read at the squared distance: √0.43 of the way
        // out is its point at 0.43 (0.12).
        let at = o.softness_curve.eval(0.43f32.sqrt());
        assert!((at - 0.12).abs() < 0.01, "{at}");
        // A circle with a full fade is a hard tip: hard here too.
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
        // alphafactor·(erf(d + c) − erf(d − c)), fade 0.5.
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
    fn softness_mirror_fade_and_perspective_come_across() {
        let brush = with_params(
            "paintbrush",
            &[
                ("PressureSoftness", "true"),
                ("SoftnessSensor", PRESSURE),
                ("PressureMirror", "true"),
                ("VerticalMirrorEnabled", "true"),
                ("MirrorSensor", PRESSURE),
                ("PressureSize", "true"),
                (
                    "SizeSensor",
                    r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="fade" length="300" periodic="1"/><ChildSensor id="perspective"/><ChildSensor id="time" duration="1500"/></params>"#,
                ),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        let b = &imported.presets[0].brush;
        // Its round tip's fade (a quarter) is what softness shrinks.
        assert_eq!(b.brush_options.softening, Softening::Fade(0.25));
        let by = |sensor: Sensor| b.inputs.iter().find(|m| m.sensor == sensor).unwrap();
        let settings: Vec<DabSetting> = b.inputs.iter().map(|m| m.setting).collect();
        assert!(settings.contains(&DabSetting::Softness) && settings.contains(&DabSetting::Mirror));
        assert!(b.dynamics.tip.random_flip_y && !b.dynamics.tip.random_flip_x);
        let fade = by(Sensor::Fade);
        assert_eq!(
            (fade.setting, fade.length, fade.periodic),
            (DabSetting::Size, 300.0, true)
        );
        assert_eq!(by(Sensor::Perspective).setting, DabSetting::Size);
        // The preset's time is in milliseconds.
        assert_eq!(
            (by(Sensor::Time).length, by(Sensor::Time).periodic),
            (1.5, false)
        );

        // A Gaussian tip ignores softness: nothing to map, nothing
        // missing.
        let gauss = with_params_xml(
            &AUTO.replace(r#"type="circle""#, r#"type="circle" id="gauss""#),
            &[("PressureSoftness", "true"), ("SoftnessSensor", PRESSURE)],
        );
        let imported = import_kpp(&gauss, "file").unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        assert!(imported.presets[0].brush.inputs.is_empty());
    }

    #[test]
    fn speed_reads_on_krita_s_scale() {
        // Size by speed, full when still and gone at a tenth of the
        // preset's full speed (3,000 px/s): here that's past this app's fast.
        let brush = with_params(
            "paintbrush",
            &[(
                "SizeSensor",
                r#"<!DOCTYPE params><params id="speed"><curve>0,1;0.1,0;1,0;</curve></params>"#,
            )],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let m = &imported.presets[0].brush.inputs[0];
        assert_eq!(m.sensor, Sensor::Speed);
        let fast = crate::brush_engine::dynamics::FAST_SPEED;
        let at = |px_per_s: f32| m.curve.eval(px_per_s / fast);
        assert!((at(0.0) - 1.0).abs() < 1e-3);
        // 1,500 px/s reads where the preset's does: a twentieth along its curve.
        let krita = parse_curve("0,1;0.1,0;1,0;").unwrap();
        assert!(
            (at(1500.0) - krita.eval(0.05)).abs() < 0.02,
            "{}",
            at(1500.0)
        );
        assert!(at(2500.0) < 0.2);
    }

    #[test]
    fn a_krita_mypaint_preset_brings_its_mypaint_brush() {
        let myb = r#"{"version": 3, "settings": {
            "radius_logarithmic": {"base_value": 1.61, "inputs": {}},
            "hardness": {"base_value": 0.7, "inputs": {}},
            "dabs_per_actual_radius": {"base_value": 4.0, "inputs": {}}}}"#;
        let xml = format!(
            r#"<Preset paintopid="mypaintbrush" name="Snirkel" embedded_resources="0">
            <param name="MyPaint/json" type="bytearray">{}</param>
            <param name="MyPaint/diameter" type="internal">12</param>
            <param name="MyPaint/opcity" type="internal">0.5</param>
            </Preset>"#,
            base64(myb.as_bytes())
        );
        let imported = import_kpp(&kpp(&xml), "file").unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        let p = &imported.presets[0];
        assert_eq!(p.name, "Snirkel");
        let o = &p.brush.brush_options;
        assert_eq!((o.diameter, o.opacity), (12.0, 0.5));
        assert!((o.hardness - 70.0).abs() < 1e-3);
        // Four dabs a radius: an eighth of the size apart.
        assert!((o.spacing - 12.5).abs() < 1e-3);
    }

    #[test]
    fn krita_sketch_settings_and_options_come_across() {
        let brush = with_params(
            "sketchbrush",
            &[
                ("Sketch/probability", "0.6"),
                ("Sketch/lineWidth", "3"),
                ("Sketch/offset", "30"),
                ("Sketch/randomOpacity", "true"),
                ("PressureDensity", "true"),
                ("DensitySensor", PRESSURE),
                ("PressureLine width", "true"),
                ("Line widthSensor", PRESSURE),
                ("PressureOffset scale", "true"),
                ("Offset scaleSensor", PRESSURE),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        let b = &imported.presets[0].brush;
        let s = b.sketch;
        assert_eq!(
            (s.density, s.thickness, s.offset, s.opacity),
            (0.6, 3.0, 0.3, 0.5)
        );
        assert_eq!(s.reach, 18.0, "the tip's radius");
        let settings: Vec<DabSetting> = b.inputs.iter().map(|m| m.setting).collect();
        assert_eq!(
            settings,
            [
                DabSetting::SketchDensity,
                DabSetting::SketchWidth,
                DabSetting::SketchOffset
            ]
        );
    }

    #[test]
    fn tilt_and_drawing_angle_read_on_krita_s_scales() {
        let one = |id: &str| {
            let mut inputs = Vec::new();
            let read = SensorRead {
                curve: None,
                length: None,
            };
            assert!(map_sensor(&mut inputs, id, DabSetting::Size, false, read));
            inputs.pop().unwrap()
        };
        let at = |m: &InputMapping, x: f32| m.curve.eval(x);
        // Elevation: 1 upright, 0 from 60° of lean (this app's lean is its
        // sine), two thirds at 30°.
        let tilt = one("declination");
        assert!((at(&tilt, 0.0) - 1.0).abs() < 1e-3);
        assert!(at(&tilt, 0.87) < 0.02);
        assert!(
            (at(&tilt, 0.5) - 2.0 / 3.0).abs() < 0.02,
            "{}",
            at(&tilt, 0.5)
        );
        // Tilt direction: leaning right (0 here) is the preset's quarter,
        // leaning down the screen (three quarters here) its half.
        let dir = one("ascension");
        assert!((at(&dir, 0.0) - 0.25).abs() < 0.01);
        assert!((at(&dir, 0.75) - 0.5).abs() < 0.01);
        // Drawing angle on size: going right reads a half, going up (a
        // quarter turn counter-clockwise here) a quarter.
        let angle = one("drawingangle");
        assert!((at(&angle, 0.0) - 0.5).abs() < 0.01);
        assert!((at(&angle, 0.25) - 0.25).abs() < 0.01);
        assert!(!angle.both_ways);
    }

    #[test]
    fn option_strengths_scale_what_their_sensors_read() {
        let brush = with_params(
            "paintbrush",
            &[
                ("SizeValue", "0.5"),
                ("PressureRotation", "true"),
                ("RotationValue", "0.25"),
                ("RotationSensor", PRESSURE),
                ("PressureScatter", "true"),
                ("ScatterValue", "1.5"),
                ("ScatterSensor", PRESSURE),
                ("PressureSpacing", "true"),
                ("SpacingValue", "2"),
                ("SpacingSensor", PRESSURE),
                ("PressureOpacity", "true"),
                ("OpacitycurveMode", "2"),
                (
                    "OpacitySensor",
                    r#"<!DOCTYPE params><params id="sensorslist"><ChildSensor id="pressure"/><ChildSensor id="speed"/></params>"#,
                ),
            ],
        );
        let imported = import_kpp(&brush, "file").unwrap();
        let b = &imported.presets[0].brush;
        // The tip is 36 across; its size option's strength halves it.
        assert_eq!(b.brush_options.diameter, 18.0);
        // Rotation by pressure: either way, up to a quarter of 180°.
        let turn = b
            .inputs
            .iter()
            .find(|m| m.setting == DabSetting::Angle)
            .unwrap();
        assert!(turn.both_ways && turn.amount == 0.25);
        // Scatter by pressure, up to one and a half widths: the inputs add.
        let scatter: f32 = b
            .inputs
            .iter()
            .filter(|m| m.setting == DabSetting::Scatter)
            .map(|m| m.amount)
            .sum();
        assert_eq!((scatter, b.jitter), (1.5, 0.0));
        // Spacing 8% (the tip's), doubled.
        assert_eq!(b.brush_options.spacing, 16.0);
        // Opacity takes the higher of its sensors in the preset: said so.
        assert!(
            imported.notes.iter().any(|n| n.contains("highest")),
            "{:?}",
            imported.notes
        );
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
        let b64 = base64(&gbr);
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
