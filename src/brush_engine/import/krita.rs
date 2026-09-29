//! Krita brushes: presets (`.kpp`) and bundles (`.bundle`).
//!
//! A preset is a PNG (its icon) carrying the settings as XML in a text
//! chunk named `preset`: which brush engine, its parameters, and the tip
//! (`brush_definition`: Krita's own round or square tip, or a picture,
//! embedded in the preset or kept in the bundle). A bundle is a ZIP of
//! presets with their tips and textures.
//!
//! Only the pixel brush engine (and the bristle one, as this app's bristle
//! brush) maps onto this app's brushes; presets of other engines come with
//! their tip only, and the notes say so.

use super::{Imported, Reader, base64_decode, gimp};
use crate::brush_engine::brush::{Brush, BrushPreset, BrushType};
use crate::brush_engine::brush_options::PixelBrushShape;
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
        "colorsmudge" => notes.push(format!(
            "{name}: Krita's colour smudge came across as a plain brush (for mixing, use the \
             Smudge tool's colour setting)"
        )),
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
    // An eraser preset.
    if param("CompositeOp") == Some("erase") || yes("EraserMode") {
        b.brush_options.blend_mode = crate::brush_engine::brush_options::BlendMode::Eraser;
    }
    if let Some(v) = number("OpacityValue") {
        b.brush_options.opacity = v.clamp(0.0, 1.0);
    }
    if let Some(v) = number("FlowValue") {
        b.brush_options.flow = (v * 100.0).clamp(0.0, 100.0);
    }
    let o = &mut b.brush_options;
    o.pressure_size = yes("PressureSize");
    o.pressure_opacity = yes("PressureOpacity");
    o.pressure_flow = yes("PressureFlow");
    o.pressure_curves.size = param("SizeSensor").and_then(sensor_curve);
    o.pressure_curves.opacity = param("OpacitySensor").and_then(sensor_curve);
    o.pressure_curves.flow = param("FlowSensor").and_then(sensor_curve);
    // Scatter (Krita's is a share of the size, either way).
    if yes("PressureScatter")
        && let Some(v) = number("ScatterValue")
    {
        b.jitter = (v * 50.0).clamp(0.0, 500.0);
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
            .map(|img| crate::brush_engine::texture::Pattern::from_image(base, &img));
        match pattern {
            Some(pattern) => {
                use crate::brush_engine::texture::{BrushTexture, TextureMode};
                let mode = match number("Texture/Pattern/TexturingMode").unwrap_or(0.0) as i32 {
                    0 => TextureMode::Multiply,
                    1 => TextureMode::Subtract,
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
                    invert: yes("Texture/Pattern/Invert"),
                    placement: Default::default(),
                });
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
    };
    if brush.attr("type") == Some("auto_brush") {
        if let Some(mask) = brush.find("MaskGenerator") {
            let m = |k: &str| mask.attr(k).and_then(|v| v.parse::<f32>().ok());
            let diameter = m("diameter").or(m("radius").map(|r| r * 2.0));
            tip.diameter = diameter.unwrap_or(40.0).clamp(1.0, 3000.0);
            tip.ratio = m("ratio").unwrap_or(1.0).clamp(0.02, 1.0);
            // Fade 0..1 from the edge in: hardness is what's left.
            let fade = m("hfade").unwrap_or(0.0).max(m("vfade").unwrap_or(0.0));
            tip.hardness = ((1.0 - fade) * 100.0).clamp(0.0, 100.0);
            if mask.attr("type") == Some("rect") {
                tip.shape = PixelBrushShape::Square;
            }
        }
        return Ok(Ok(tip));
    }
    let file = brush.attr("filename").unwrap_or_default();
    let Some((mask, extra)) = picture_tip(file, embedded, bundle) else {
        return Ok(Err(file.to_string()));
    };
    tip.diameter =
        (mask.width.max(mask.height) as f32 * attr("scale").unwrap_or(1.0)).clamp(1.0, 3000.0);
    // Krita paints a colour picture's colours unless it's used as a mask.
    tip.colors = mask.has_colors() && brush.attr("ColorAsMask") != Some("1");
    tip.shape = PixelBrushShape::Custom(mask);
    tip.extra = extra;
    Ok(Ok(tip))
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
    Some((TipMask::from_image(&img), Vec::new()))
}

/// A sensor's curve (`<curve>0,0;0.5,0.2;1,1;</curve>` inside its XML),
/// if it isn't the straight line.
fn sensor_curve(xml: &str) -> Option<SoftnessCurve> {
    let root = parse_xml(xml).ok()?;
    // The pressure sensor's (alone, or among several).
    let pressure = std::iter::once(&root)
        .chain(root.descendants("params"))
        .chain(root.descendants("ChildSensor"))
        .find(|n| n.attr("id") == Some("pressure"))?;
    let text = pressure.descendants("curve").next()?.text.clone();
    let points: Vec<CurvePoint> = text
        .split(';')
        .filter_map(|p| {
            let (x, y) = p.split_once(',')?;
            Some(CurvePoint::new(
                x.trim().parse().ok()?,
                y.trim().parse().ok()?,
            ))
        })
        .collect();
    let straight = points.len() == 2
        && points[0] == CurvePoint::new(0.0, 0.0)
        && points[1] == CurvePoint::new(1.0, 1.0);
    (points.len() >= 2 && !straight).then_some(SoftnessCurve { points })
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
        assert!((o.hardness - 75.0).abs() < 1e-3);
        assert!((o.opacity - 0.7).abs() < 1e-6);
        assert!(o.pressure_size);
        assert_eq!(o.pressure_curves.size.as_ref().unwrap().points.len(), 3);
        assert_eq!(p.brush.dynamics.tip.ratio, 0.5);
        assert!((p.brush.dynamics.tip.angle - 0.5f32.to_degrees()).abs() < 1e-3);
        assert!(imported.notes.is_empty());
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
