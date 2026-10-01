//! Brush preset files (`.rpbrush`): one or more presets with the tips and
//! paper textures they use, so a file shared with someone else paints the
//! same.
//!
//! The file is a ZIP of stored entries:
//!
//! - `presets.json`: the settings (`StoredLibrary`), tips and textures
//!   referred to by number;
//! - `tips/<n>.png`: each tip's mask, 8-bit grey (255 = full paint);
//! - `textures/<n>.bin`: each texture's heights, `f32` little-endian,
//!   zstd-compressed (a PNG would round them to 8 bits).
//!
//! Built-in tips and textures are written by name only (since version 2).
//!
//! Every setting has a default, so files from older versions (or written by
//! hand) load with whatever they leave out at its default.

use crate::brush_engine::brush::{Brush, BrushPreset, BrushType, StabilizerAlgorithm};
use crate::brush_engine::brush_options::{
    BlendMode, PaintingMode, PixelBrushShape, Placement, PressureCurves, TipOrder,
};
use crate::brush_engine::dual::{DualMode, DualTip};
use crate::brush_engine::dynamics::BrushDynamics;
use crate::brush_engine::hardness::{SoftnessCurve, SoftnessSelector};
use crate::brush_engine::texture::{BrushTexture, Pattern, TextureMode};
use crate::brush_engine::tip::TipMask;
use crate::canvas::blend_modes::LayerBlend;
use crate::project::zip;
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The file name extension.
pub const EXTENSION: &str = "rpbrush";
const FORMAT: &str = "rusty-painter-brushes";
const VERSION: u32 = 2;
const PRESETS_ENTRY: &str = "presets.json";
/// Largest tip or texture side accepted from a file.
const MAX_SIDE: usize = 8192;

#[derive(Serialize, Deserialize)]
struct StoredLibrary {
    format: String,
    version: u32,
    presets: Vec<StoredPreset>,
    #[serde(default)]
    tips: Vec<StoredTip>,
    #[serde(default)]
    textures: Vec<StoredTexture>,
}

#[derive(Serialize, Deserialize)]
struct StoredPreset {
    name: String,
    brush: StoredBrush,
}

#[derive(Serialize, Deserialize)]
struct StoredTip {
    width: usize,
    height: usize,
    /// A built-in tip's name: no picture in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    builtin: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredTexture {
    name: String,
    size: usize,
    /// A built-in texture (found by `name`): no heights in the file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    builtin: bool,
}

#[derive(Serialize, Deserialize)]
enum StoredShape {
    Circle,
    Square,
    /// A tip from the file's `tips`.
    Tip(usize),
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct StoredDual {
    shape: StoredShape,
    size: f32,
    hardness: f32,
    spacing: f32,
    scatter: f32,
    count: u32,
    random_angle: bool,
    mode: DualMode,
}

impl Default for StoredDual {
    fn default() -> Self {
        StoredDual::from_dual(&DualTip::default(), &mut Resources::default())
    }
}

impl StoredDual {
    fn from_dual(d: &DualTip, res: &mut Resources) -> Self {
        Self {
            shape: StoredShape::from_shape(&d.shape, res),
            size: d.size,
            hardness: d.hardness,
            spacing: d.spacing,
            scatter: d.scatter,
            count: d.count,
            random_angle: d.random_angle,
            mode: d.mode,
        }
    }

    fn into_dual(self, res: &Resources) -> Result<DualTip, String> {
        Ok(DualTip {
            shape: self.shape.into_shape(res)?,
            size: self.size,
            hardness: self.hardness,
            spacing: self.spacing,
            scatter: self.scatter,
            count: self.count,
            random_angle: self.random_angle,
            mode: self.mode,
        })
    }
}

impl StoredShape {
    fn from_shape(shape: &PixelBrushShape, res: &mut Resources) -> Self {
        match shape {
            PixelBrushShape::Circle => StoredShape::Circle,
            PixelBrushShape::Square => StoredShape::Square,
            PixelBrushShape::Custom(tip) => StoredShape::Tip(res.tip(tip)),
        }
    }

    fn into_shape(self, res: &Resources) -> Result<PixelBrushShape, String> {
        Ok(match self {
            StoredShape::Circle => PixelBrushShape::Circle,
            StoredShape::Square => PixelBrushShape::Square,
            StoredShape::Tip(i) => PixelBrushShape::Custom(
                res.tips
                    .get(i)
                    .cloned()
                    .ok_or_else(|| format!("Missing brush tip {i}"))?,
            ),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredBrushTexture {
    /// A texture from the file's `textures`.
    pattern: usize,
    mode: TextureMode,
    scale: f32,
    strength: f32,
    invert: bool,
    /// Where the grain sits (moved with the stroke, turned...).
    #[serde(default)]
    placement: crate::brush_engine::texture::GrainPlacement,
}

/// A brush's settings, flattened (the tip and texture by number).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct StoredBrush {
    brush_type: BrushType,
    diameter: f32,
    hardness: f32,
    softness_selector: SoftnessSelector,
    softness_curve: SoftnessCurve,
    shape: StoredShape,
    /// More tips the dabs alternate with, from the file's `tips`.
    extra_tips: Vec<usize>,
    tip_order: TipOrder,
    tip_colors: bool,
    placement: Placement,
    /// Premultiplied RGBA, as the brush holds it.
    color: [u8; 4],
    spacing: f32,
    flow: f32,
    opacity: f32,
    blend_mode: BlendMode,
    painting_mode: PaintingMode,
    pressure_size: bool,
    pressure_min_size: f32,
    pressure_opacity: bool,
    pressure_flow: bool,
    pressure_spacing: bool,
    pressure_curves: PressureCurves,
    pixel_perfect: bool,
    anti_aliasing: bool,
    jitter: f32,
    stabilizer: f32,
    stabilizer_algorithm: StabilizerAlgorithm,
    stabilizer_mass: f32,
    stabilizer_drag: f32,
    stabilizer_modes: crate::brush_engine::stabilizer::StabilizerModes,
    dynamics: BrushDynamics,
    #[serde(default)]
    inputs: Vec<crate::brush_engine::dynamics::InputMapping>,
    texture: Option<StoredBrushTexture>,
    /// A [`LayerBlend::key`].
    paint_blend: String,
    airbrush_rate: f32,
    dual: Option<StoredDual>,
    wet_edge: f32,
    wet_edge_width: f32,
    bristles: crate::brush_engine::bristle::Bristles,
    sketch: crate::brush_engine::sketch::Sketch,
    hatching: crate::brush_engine::hatching::Hatching,
    sharpness: f32,
    mixing: Option<crate::brush_engine::brush_options::Mixing>,
}

impl Default for StoredBrush {
    fn default() -> Self {
        let mut unused = Resources::default();
        StoredBrush::from_brush(&Brush::new(24.0, 20.0, Color32::BLACK, 25.0), &mut unused)
    }
}

/// The tips and textures a file's presets share, each written once.
#[derive(Default)]
struct Resources {
    tips: Vec<Arc<TipMask>>,
    textures: Vec<Arc<Pattern>>,
}

impl Resources {
    fn tip(&mut self, tip: &Arc<TipMask>) -> usize {
        index_of(&mut self.tips, tip)
    }

    fn texture(&mut self, pattern: &Arc<Pattern>) -> usize {
        index_of(&mut self.textures, pattern)
    }
}

fn index_of<T: PartialEq>(list: &mut Vec<Arc<T>>, item: &Arc<T>) -> usize {
    list.iter()
        .position(|t| Arc::ptr_eq(t, item) || **t == **item)
        .unwrap_or_else(|| {
            list.push(item.clone());
            list.len() - 1
        })
}

impl StoredBrush {
    fn from_brush(b: &Brush, res: &mut Resources) -> Self {
        let o = &b.brush_options;
        Self {
            brush_type: b.brush_type,
            diameter: o.diameter,
            hardness: o.hardness,
            softness_selector: o.softness_selector,
            softness_curve: o.softness_curve.clone(),
            shape: StoredShape::from_shape(&o.pixel_shape, res),
            extra_tips: o.extra_tips.iter().map(|t| res.tip(t)).collect(),
            tip_order: o.tip_order,
            tip_colors: o.tip_colors,
            placement: o.placement,
            color: o.color.to_array(),
            spacing: o.spacing,
            flow: o.flow,
            opacity: o.opacity,
            blend_mode: o.blend_mode,
            painting_mode: o.painting_mode,
            pressure_size: o.pressure_size,
            pressure_min_size: o.pressure_min_size,
            pressure_opacity: o.pressure_opacity,
            pressure_flow: o.pressure_flow,
            pressure_spacing: o.pressure_spacing,
            pressure_curves: o.pressure_curves.clone(),
            pixel_perfect: b.pixel_perfect,
            anti_aliasing: b.anti_aliasing,
            jitter: b.jitter,
            stabilizer: b.stabilizer,
            stabilizer_algorithm: b.stabilizer_algorithm,
            stabilizer_mass: b.stabilizer_mass,
            stabilizer_drag: b.stabilizer_drag,
            stabilizer_modes: b.stabilizer_modes,
            dynamics: b.dynamics,
            inputs: b.inputs.clone(),
            texture: b.texture.as_ref().map(|t| StoredBrushTexture {
                pattern: res.texture(&t.pattern),
                mode: t.mode,
                scale: t.scale,
                strength: t.strength,
                invert: t.invert,
                placement: t.placement,
            }),
            paint_blend: b.paint_blend.key().to_string(),
            airbrush_rate: b.airbrush_rate,
            dual: b.dual.as_ref().map(|d| StoredDual::from_dual(d, res)),
            wet_edge: b.wet_edge,
            wet_edge_width: b.wet_edge_width,
            bristles: b.bristles,
            sketch: b.sketch,
            hatching: b.hatching,
            sharpness: b.sharpness,
            mixing: b.mixing,
        }
    }

    fn into_brush(self, res: &Resources) -> Result<Brush, String> {
        let mut b = Brush::new(self.diameter, self.hardness, Color32::BLACK, self.spacing);
        b.brush_type = self.brush_type;
        let o = &mut b.brush_options;
        o.softness_selector = self.softness_selector;
        o.softness_curve = self.softness_curve;
        o.pixel_shape = self.shape.into_shape(res)?;
        o.extra_tips = self
            .extra_tips
            .iter()
            .map(|&i| {
                res.tips
                    .get(i)
                    .cloned()
                    .ok_or_else(|| format!("Missing brush tip {i}"))
            })
            .collect::<Result<_, _>>()?;
        o.tip_order = self.tip_order;
        o.tip_colors = self.tip_colors;
        o.placement = self.placement;
        let [r, g, bl, a] = self.color;
        o.color = Color32::from_rgba_premultiplied(r, g, bl, a);
        o.flow = self.flow;
        o.opacity = self.opacity;
        o.blend_mode = self.blend_mode;
        o.painting_mode = self.painting_mode;
        o.pressure_size = self.pressure_size;
        o.pressure_min_size = self.pressure_min_size;
        o.pressure_opacity = self.pressure_opacity;
        o.pressure_flow = self.pressure_flow;
        o.pressure_spacing = self.pressure_spacing;
        o.pressure_curves = self.pressure_curves;
        b.pixel_perfect = self.pixel_perfect;
        b.anti_aliasing = self.anti_aliasing;
        b.jitter = self.jitter;
        b.stabilizer = self.stabilizer;
        b.stabilizer_algorithm = self.stabilizer_algorithm;
        b.stabilizer_mass = self.stabilizer_mass;
        b.stabilizer_drag = self.stabilizer_drag;
        b.stabilizer_modes = self.stabilizer_modes;
        b.dynamics = self.dynamics;
        b.inputs = self.inputs;
        b.texture = match self.texture {
            None => None,
            Some(t) => Some(BrushTexture {
                pattern: res
                    .textures
                    .get(t.pattern)
                    .cloned()
                    .ok_or_else(|| format!("Missing texture {}", t.pattern))?,
                mode: t.mode,
                scale: t.scale,
                strength: t.strength,
                invert: t.invert,
                placement: t.placement,
            }),
        };
        b.paint_blend = LayerBlend::from_key(&self.paint_blend).unwrap_or_default();
        b.airbrush_rate = self.airbrush_rate;
        b.dual = self.dual.map(|d| d.into_dual(res)).transpose()?;
        b.wet_edge = self.wet_edge;
        b.wet_edge_width = self.wet_edge_width;
        b.bristles = self.bristles;
        b.sketch = self.sketch;
        b.hatching = self.hatching;
        b.sharpness = self.sharpness.clamp(0.0, 1.0);
        b.mixing = self.mixing;
        Ok(b)
    }
}

fn builtin_tip_name(tip: &Arc<TipMask>) -> Option<&'static str> {
    let builtin = crate::brush_engine::tip::builtin().iter();
    let mut builtin = builtin.filter(|(_, t)| Arc::ptr_eq(t, tip) || **t == **tip);
    builtin.next().map(|(n, _)| *n)
}

fn is_builtin_texture(pattern: &Arc<Pattern>) -> bool {
    let mut builtin = crate::brush_engine::texture::builtin().iter();
    builtin.any(|p| Arc::ptr_eq(p, pattern) || **p == **pattern)
}

/// Whether `a` and `b` hold the same settings (everything a preset file
/// keeps; tips and textures compared by content).
pub fn same_settings(a: &Brush, b: &Brush) -> bool {
    let mut res = Resources::default();
    let a = serde_json::to_value(StoredBrush::from_brush(a, &mut res));
    let b = serde_json::to_value(StoredBrush::from_brush(b, &mut res));
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}

/// `presets` as the bytes of a `.rpbrush` file.
pub fn encode(presets: &[BrushPreset]) -> Result<Vec<u8>, String> {
    let mut res = Resources::default();
    let stored = presets
        .iter()
        .map(|p| StoredPreset {
            name: p.name.clone(),
            brush: StoredBrush::from_brush(&p.brush, &mut res),
        })
        .collect();
    let library = StoredLibrary {
        format: FORMAT.to_string(),
        version: VERSION,
        presets: stored,
        tips: res
            .tips
            .iter()
            .map(|t| StoredTip {
                width: t.width,
                height: t.height,
                builtin: builtin_tip_name(t).map(str::to_string),
            })
            .collect(),
        textures: res
            .textures
            .iter()
            .map(|t| StoredTexture {
                name: t.name.clone(),
                size: t.size,
                builtin: is_builtin_texture(t),
            })
            .collect(),
    };
    let mut zip = zip::ZipWriter::default();
    let json = serde_json::to_vec_pretty(&library).map_err(|e| e.to_string())?;
    zip.add(PRESETS_ENTRY, &json)?;
    for (i, tip) in res.tips.iter().enumerate() {
        if builtin_tip_name(tip).is_some() {
            continue;
        }
        let (w, h) = (tip.width as u32, tip.height as u32);
        let img: image::DynamicImage = match &tip.colors {
            // A colour tip: its colours, with the mask as alpha.
            Some(colors) => image::RgbaImage::from_raw(
                w,
                h,
                colors
                    .iter()
                    .zip(&tip.pixels)
                    .flat_map(|(c, &a)| [c[0], c[1], c[2], a])
                    .collect(),
            )
            .ok_or("Brush tip has the wrong size")?
            .into(),
            None => image::GrayImage::from_raw(w, h, tip.pixels.clone())
                .ok_or("Brush tip has the wrong size")?
                .into(),
        };
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        zip.add(&format!("tips/{i}.png"), &png)?;
    }
    for (i, pattern) in res.textures.iter().enumerate() {
        if is_builtin_texture(pattern) {
            continue;
        }
        let raw: Vec<u8> = pattern.data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let packed = zstd::encode_all(&raw[..], 9).map_err(|e| e.to_string())?;
        zip.add(&format!("textures/{i}.bin"), &packed)?;
    }
    zip.finish()
}

/// The presets in a `.rpbrush` file. Tips and textures identical to the
/// built-in ones are shared with them.
pub fn decode(bytes: &[u8]) -> Result<Vec<BrushPreset>, String> {
    if !bytes.starts_with(zip::SIGNATURE) {
        return Err("Not a brush preset file".into());
    }
    let damaged = |e: String| format!("Damaged brush preset file ({e})");
    let json = zip::read_entry(bytes, PRESETS_ENTRY).map_err(damaged)?;
    let library: StoredLibrary =
        serde_json::from_slice(json).map_err(|e| damaged(e.to_string()))?;
    if library.format != FORMAT {
        return Err("Not a brush preset file".into());
    }
    if library.version > VERSION {
        return Err("This brush preset file is from a newer version".into());
    }
    let mut res = Resources::default();
    for (i, stored) in library.tips.iter().enumerate() {
        if let Some(name) = &stored.builtin {
            let tip = crate::brush_engine::tip::builtin()
                .iter()
                .find(|(n, _)| n == name);
            res.tips.push(
                tip.ok_or_else(|| format!("Unknown built-in brush tip {name}"))?
                    .1
                    .clone(),
            );
            continue;
        }
        if stored.width == 0 || stored.height == 0 || stored.width.max(stored.height) > MAX_SIDE {
            return Err(damaged(format!("tip {i} size")));
        }
        let png = zip::read_entry(bytes, &format!("tips/{i}.png")).map_err(damaged)?;
        let img = image::load_from_memory(png).map_err(|e| damaged(e.to_string()))?;
        if (img.width() as usize, img.height() as usize) != (stored.width, stored.height) {
            return Err(damaged(format!("tip {i} size")));
        }
        if img.color().has_alpha() {
            let rgba = img.to_rgba8();
            let pixels: Vec<u8> = rgba.pixels().map(|p| p[3]).collect();
            let colors: Vec<[u8; 3]> = rgba.pixels().map(|p| [p[0], p[1], p[2]]).collect();
            let builtin = crate::brush_engine::tip::builtin()
                .iter()
                .find(|(_, t)| t.pixels == pixels && t.colors.as_ref() == Some(&colors));
            res.tips.push(match builtin {
                Some((_, t)) => t.clone(),
                None => TipMask::from_colored(stored.width, stored.height, pixels, colors),
            });
            continue;
        }
        let pixels = img.to_luma8().into_raw();
        let builtin = crate::brush_engine::tip::builtin().iter().find(|(_, t)| {
            (t.width, t.height) == (stored.width, stored.height) && t.pixels == pixels
        });
        res.tips.push(match builtin {
            Some((_, t)) => t.clone(),
            None => TipMask::from_mask(stored.width, stored.height, pixels),
        });
    }
    for (i, stored) in library.textures.iter().enumerate() {
        if stored.builtin {
            let pattern = crate::brush_engine::texture::builtin()
                .iter()
                .find(|p| p.name == stored.name);
            res.textures.push(
                pattern
                    .ok_or_else(|| format!("Unknown built-in texture {}", stored.name))?
                    .clone(),
            );
            continue;
        }
        if !stored.size.is_power_of_two() || stored.size > MAX_SIDE {
            return Err(damaged(format!("texture {i} size")));
        }
        let packed = zip::read_entry(bytes, &format!("textures/{i}.bin")).map_err(damaged)?;
        let raw = zstd::decode_all(packed).map_err(|e| damaged(e.to_string()))?;
        if raw.len() != stored.size * stored.size * 4 {
            return Err(damaged(format!("texture {i} size")));
        }
        let data: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let pattern = Pattern {
            name: stored.name.clone(),
            size: stored.size,
            data,
        };
        let builtin = crate::brush_engine::texture::builtin()
            .iter()
            .find(|p| ***p == pattern);
        res.textures
            .push(builtin.cloned().unwrap_or_else(|| Arc::new(pattern)));
    }
    library
        .presets
        .into_iter()
        .map(|p| {
            Ok(BrushPreset {
                name: p.name,
                brush: p.brush.into_brush(&res)?,
                file: None,
            })
        })
        .collect()
}

/// A file name for a preset called `name`: letters, digits, spaces and
/// `-_()` kept, the rest dropped.
pub fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || " -_()".contains(*c))
        .collect();
    match stem.trim() {
        "" => "Brush".to_string(),
        s => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_only_a_name_loads_with_defaults() {
        let json = br#"{"format":"rusty-painter-brushes","version":1,
            "presets":[{"name":"Bare","brush":{"diameter":40.0}}]}"#;
        let mut zip = zip::ZipWriter::default();
        zip.add(PRESETS_ENTRY, json).unwrap();
        let presets = decode(&zip.finish().unwrap()).unwrap();
        assert_eq!(presets.len(), 1);
        let b = &presets[0].brush;
        assert_eq!(presets[0].name, "Bare");
        assert_eq!(b.brush_options.diameter, 40.0);
        assert_eq!(b.brush_options.spacing, 25.0);
        assert_eq!(b.dynamics, BrushDynamics::default());
        assert!(b.texture.is_none());
    }

    #[test]
    fn the_stabiliser_modes_survive_and_default_when_missing() {
        use crate::brush_engine::stabilizer::StabilizerModes;
        let mut brush = Brush::new(24.0, 20.0, Color32::BLACK, 25.0);
        brush.stabilizer_algorithm = StabilizerAlgorithm::String;
        brush.stabilizer_modes = StabilizerModes {
            string_length: 75.0,
            catch_up: false,
            correction: 0.2,
            filter_strength: 0.9,
            filter_speed: 0.1,
        };
        let preset = BrushPreset {
            name: "Lazy".into(),
            brush: brush.clone(),
            file: None,
        };
        let back = &decode(&encode(&[preset]).unwrap()).unwrap()[0].brush;
        assert_eq!(back.stabilizer_algorithm, StabilizerAlgorithm::String);
        assert_eq!(back.stabilizer_modes, brush.stabilizer_modes);
        // Older files have neither: no new mode, default settings.
        let json = br#"{"format":"rusty-painter-brushes","version":1,
            "presets":[{"name":"Old","brush":{"stabilizer_algorithm":"Simple",
            "stabilizer":0.4,"stabilizer_modes":{"string_length":12.0}}}]}"#;
        let mut zip = zip::ZipWriter::default();
        zip.add(PRESETS_ENTRY, json).unwrap();
        let old = &decode(&zip.finish().unwrap()).unwrap()[0].brush;
        assert_eq!(old.stabilizer_algorithm, StabilizerAlgorithm::Simple);
        assert_eq!(old.stabilizer, 0.4);
        assert_eq!(
            old.stabilizer_modes,
            StabilizerModes {
                string_length: 12.0,
                ..Default::default()
            }
        );
        let bare = &decode(&{
            let mut zip = zip::ZipWriter::default();
            zip.add(PRESETS_ENTRY, br#"{"format":"rusty-painter-brushes","version":1,"presets":[{"name":"B","brush":{}}]}"#)
                .unwrap();
            zip.finish().unwrap()
        })
        .unwrap()[0]
            .brush;
        assert_eq!(bare.stabilizer_algorithm, StabilizerAlgorithm::None);
        assert_eq!(bare.stabilizer_modes, StabilizerModes::default());
    }

    #[test]
    fn damaged_files_are_refused_not_panicked_on() {
        let presets = crate::PainterApp::default_brush_presets();
        let bytes = encode(&presets).unwrap();
        assert!(decode(b"not a zip").is_err());
        for cut in [10, bytes.len() / 3, bytes.len() / 2, bytes.len() - 5] {
            assert!(decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut flipped = bytes.clone();
        let mid = flipped.len() / 2;
        flipped[mid] ^= 0x55;
        assert!(decode(&flipped).is_err());
    }

    #[test]
    fn shared_tips_and_textures_are_written_once_and_built_ins_come_back_shared() {
        let presets = crate::PainterApp::default_brush_presets();
        let back = decode(&encode(&presets).unwrap()).unwrap();
        assert_eq!(back.len(), presets.len());
        for (a, b) in presets.iter().zip(&back) {
            assert_eq!(a.name, b.name);
            if let (PixelBrushShape::Custom(x), PixelBrushShape::Custom(y)) = (
                &a.brush.brush_options.pixel_shape,
                &b.brush.brush_options.pixel_shape,
            ) {
                assert!(Arc::ptr_eq(x, y), "{}: built-in tip not shared", a.name);
            }
            if let (Some(x), Some(y)) = (&a.brush.texture, &b.brush.texture) {
                assert!(Arc::ptr_eq(&x.pattern, &y.pattern), "{}", a.name);
            }
        }
    }

    #[test]
    fn mixing_sharpness_flips_and_pressure_spacing_survive() {
        use crate::brush_engine::brush_options::Mixing;
        let mut brush = Brush::new(30.0, 50.0, Color32::BLACK, 12.0);
        let mixing = Mixing {
            smudge_length: 0.4,
            color_rate: 0.7,
            pressure_length: true,
            pressure_color: false,
        };
        brush.mixing = Some(mixing);
        brush.sharpness = 0.4;
        brush.brush_options.pressure_spacing = true;
        brush.dynamics.tip.random_flip_x = true;
        brush.dynamics.tip.follow_barrel = true;
        brush.paint_blend = LayerBlend::Parallel;
        let preset = BrushPreset {
            name: "Krita-like".into(),
            brush,
            file: None,
        };
        let back = decode(&encode(&[preset]).unwrap()).unwrap();
        let b = &back[0].brush;
        assert_eq!(b.mixing, Some(mixing));
        assert_eq!(b.sharpness, 0.4);
        assert!(b.brush_options.pressure_spacing);
        assert!(b.dynamics.tip.random_flip_x && !b.dynamics.tip.random_flip_y);
        assert!(b.dynamics.tip.follow_barrel);
        assert_eq!(b.paint_blend, LayerBlend::Parallel);
    }

    #[test]
    fn a_custom_tip_and_texture_survive_exactly() {
        let tip = TipMask::from_mask(3, 2, vec![0, 128, 255, 7, 9, 200]);
        let pattern = Arc::new(Pattern {
            name: "Mine".into(),
            size: 2,
            data: vec![0.0, 0.123_456_7, 0.5, 1.0],
        });
        let mut brush = Brush::new(
            30.0,
            50.0,
            Color32::from_rgba_premultiplied(10, 20, 30, 40),
            12.0,
        );
        brush.brush_options.pixel_shape = PixelBrushShape::Custom(tip.clone());
        brush.texture = Some(BrushTexture::new(pattern.clone()));
        brush.paint_blend = LayerBlend::Multiply;
        let preset = BrushPreset {
            name: "Custom".into(),
            brush,
            file: None,
        };
        let back = decode(&encode(&[preset]).unwrap()).unwrap();
        let b = &back[0].brush;
        assert_eq!(b.brush_options.pixel_shape, PixelBrushShape::Custom(tip));
        assert_eq!(*b.texture.as_ref().unwrap().pattern, *pattern);
        assert_eq!(
            b.brush_options.color,
            Color32::from_rgba_premultiplied(10, 20, 30, 40)
        );
        assert_eq!(b.paint_blend, LayerBlend::Multiply);
    }

    #[test]
    fn a_colour_tip_keeps_its_colours() {
        let tip = TipMask::from_colored(
            2,
            2,
            vec![255, 128, 40, 0],
            vec![[200, 10, 30], [5, 250, 90], [1, 2, 3], [9, 9, 9]],
        );
        let mut brush = Brush::new(30.0, 50.0, Color32::BLACK, 12.0);
        brush.brush_options.pixel_shape = PixelBrushShape::Custom(tip.clone());
        brush.brush_options.tip_colors = true;
        let preset = BrushPreset {
            name: "Colour".into(),
            brush,
            file: None,
        };
        let back = decode(&encode(&[preset]).unwrap()).unwrap();
        let o = &back[0].brush.brush_options;
        assert!(o.tip_colors);
        assert_eq!(o.pixel_shape, PixelBrushShape::Custom(tip));
    }

    #[test]
    fn preset_names_become_safe_file_names() {
        assert_eq!(file_stem("Ink Pen (fine)"), "Ink Pen (fine)");
        assert_eq!(file_stem("a/b\\c:d"), "abcd");
        assert_eq!(file_stem("../"), "Brush");
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_preset_file() {
        let presets = crate::PainterApp::default_brush_presets();
        let seed = encode(&presets[..presets.len().min(6)]).unwrap();
        crate::fuzz::fuzz("rpbrush", &seed, std::time::Duration::from_secs(2), |b| {
            let _ = decode(b);
        });
    }
}
