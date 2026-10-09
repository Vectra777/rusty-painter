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
    BlendMode, ColorSource, PaintingMode, PixelBrushShape, Placement, PressureCurves, TipOrder,
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
/// The newest version read and written: 3 has the spray, chalk, curve,
/// grid, tangent normal and particle types, 4 inputs that combine, drive
/// smudge length or colour rate, or scatter past a brush width (a file is
/// written as the oldest version that has what it holds, so older builds
/// still read it).
const VERSION: u32 = 4;
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
    /// The library's tags for it (since files of version 2; older files and
    /// older builds go without).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    favourite: bool,
}

/// What the brush library keeps about a preset (not part of its settings),
/// carried in exported files so they arrive tagged and starred.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetMeta {
    pub tags: Vec<String>,
    pub favourite: bool,
}

#[derive(Serialize, Deserialize)]
struct StoredTip {
    width: usize,
    height: usize,
    /// A built-in tip's name: no picture in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    builtin: Option<String>,
    /// An SVG tip's picture: no PNG in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    svg: Option<String>,
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
    /// An imported brush's own texturing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    krita: Option<crate::brush_engine::texture::KritaTexturing>,
}

/// Where the colour comes from, a pattern by number.
#[derive(Serialize, Deserialize, Default)]
enum StoredColorSource {
    #[default]
    Plain,
    UniformRandom,
    TotalRandom,
    /// A texture from the file's `textures`.
    Pattern {
        pattern: usize,
        scale: f32,
    },
}

/// A brush's settings, flattened (the tip and texture by number).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct StoredBrush {
    #[serde(deserialize_with = "lenient_type")]
    brush_type: BrushType,
    diameter: f32,
    hardness: f32,
    softness_selector: SoftnessSelector,
    softness_curve: SoftnessCurve,
    softening: crate::brush_engine::hardness::Softening,
    shape: StoredShape,
    /// More tips the dabs alternate with, from the file's `tips`.
    extra_tips: Vec<usize>,
    tip_order: TipOrder,
    tip_colors: bool,
    tip_mapping: crate::brush_engine::brush_options::TipMapping,
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
    antialias_width: f32,
    jitter: f32,
    stabilizer: f32,
    stabilizer_algorithm: StabilizerAlgorithm,
    stabilizer_mass: f32,
    stabilizer_drag: f32,
    stabilizer_modes: crate::brush_engine::stabilizer::StabilizerModes,
    dynamics: BrushDynamics,
    #[serde(default, deserialize_with = "lenient_inputs")]
    inputs: Vec<crate::brush_engine::dynamics::InputMapping>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_inputs"
    )]
    input_combine: Vec<(
        crate::brush_engine::dynamics::DabSetting,
        crate::brush_engine::dynamics::Combine,
    )>,
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
    engines: crate::brush_engine::engines::Engines,
    impasto: Option<crate::canvas::impasto::Impasto>,
    wet: Option<crate::canvas::wet::WetPaint>,
    stay_inside: bool,
    sharpness: f32,
    sharpness_softness: f32,
    mixing: Option<crate::brush_engine::brush_options::Mixing>,
    auto_spacing: Option<f32>,
    auto_tip: crate::brush_engine::brush_options::AutoTip,
    color_source: StoredColorSource,
}

/// A brush type, or Soft for one this version doesn't know (from a newer
/// one, or written by hand).
fn lenient_type<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BrushType, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or(BrushType::Soft))
}

/// A list, without the entries this version doesn't know (an input
/// driving a setting from a newer one).
fn lenient_inputs<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(values
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
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
            softening: o.softening.clone(),
            shape: StoredShape::from_shape(&o.pixel_shape, res),
            extra_tips: o.extra_tips.iter().map(|t| res.tip(t)).collect(),
            tip_order: o.tip_order,
            tip_colors: o.tip_colors,
            tip_mapping: o.tip_mapping,
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
            antialias_width: b.antialias_width,
            jitter: b.jitter,
            stabilizer: b.stabilizer,
            stabilizer_algorithm: b.stabilizer_algorithm,
            stabilizer_mass: b.stabilizer_mass,
            stabilizer_drag: b.stabilizer_drag,
            stabilizer_modes: b.stabilizer_modes,
            dynamics: b.dynamics,
            inputs: b.inputs.clone(),
            input_combine: b.input_combine.clone(),
            texture: b.texture.as_ref().map(|t| StoredBrushTexture {
                pattern: res.texture(&t.pattern),
                mode: t.mode,
                scale: t.scale,
                strength: t.strength,
                invert: t.invert,
                placement: t.placement,
                krita: t.krita,
            }),
            paint_blend: b.paint_blend.key().to_string(),
            airbrush_rate: b.airbrush_rate,
            dual: b.dual.as_ref().map(|d| StoredDual::from_dual(d, res)),
            wet_edge: b.wet_edge,
            wet_edge_width: b.wet_edge_width,
            bristles: b.bristles.clone(),
            sketch: b.sketch,
            hatching: b.hatching,
            engines: b.engines,
            impasto: b.impasto,
            wet: b.wet,
            stay_inside: b.stay_inside,
            sharpness: b.sharpness,
            sharpness_softness: b.sharpness_softness,
            mixing: b.mixing,
            auto_spacing: o.auto_spacing,
            auto_tip: o.auto_tip,
            color_source: match &o.color_source {
                ColorSource::Plain => StoredColorSource::Plain,
                ColorSource::UniformRandom => StoredColorSource::UniformRandom,
                ColorSource::TotalRandom => StoredColorSource::TotalRandom,
                ColorSource::Pattern { pattern, scale } => StoredColorSource::Pattern {
                    pattern: res.texture(pattern),
                    scale: *scale,
                },
            },
        }
    }

    fn into_brush(self, res: &Resources) -> Result<Brush, String> {
        let mut b = Brush::new(self.diameter, self.hardness, Color32::BLACK, self.spacing);
        b.brush_type = self.brush_type;
        let o = &mut b.brush_options;
        o.softness_selector = self.softness_selector;
        o.softness_curve = self.softness_curve;
        o.softening = self.softening;
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
        o.tip_mapping = self.tip_mapping;
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
        o.auto_spacing = self.auto_spacing.map(|c| c.clamp(0.01, 10.0));
        o.auto_tip = self.auto_tip;
        o.color_source = match self.color_source {
            StoredColorSource::Plain => ColorSource::Plain,
            StoredColorSource::UniformRandom => ColorSource::UniformRandom,
            StoredColorSource::TotalRandom => ColorSource::TotalRandom,
            StoredColorSource::Pattern { pattern, scale } => ColorSource::Pattern {
                pattern: res
                    .textures
                    .get(pattern)
                    .cloned()
                    .ok_or_else(|| format!("Missing texture {pattern}"))?,
                scale,
            },
        };
        b.pixel_perfect = self.pixel_perfect;
        b.anti_aliasing = self.anti_aliasing;
        b.antialias_width = self.antialias_width;
        b.jitter = self.jitter;
        b.stabilizer = self.stabilizer;
        b.stabilizer_algorithm = self.stabilizer_algorithm;
        b.stabilizer_mass = self.stabilizer_mass;
        b.stabilizer_drag = self.stabilizer_drag;
        b.stabilizer_modes = self.stabilizer_modes;
        b.dynamics = self.dynamics;
        b.inputs = self.inputs;
        b.input_combine = self.input_combine;
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
                krita: t.krita,
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
        b.engines = self.engines;
        b.impasto = self.impasto;
        b.wet = self.wet;
        b.stay_inside = self.stay_inside;
        b.sharpness = self.sharpness.clamp(0.0, 1.0);
        b.sharpness_softness = self.sharpness_softness.clamp(0.0, 1.0);
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
    encode_with_meta(presets, &[])
}

/// `presets` as the bytes of a `.rpbrush` file, each with its tags and star
/// from `meta` (by position; missing ones have none).
pub fn encode_with_meta(presets: &[BrushPreset], meta: &[PresetMeta]) -> Result<Vec<u8>, String> {
    let mut res = Resources::default();
    let stored = presets
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let meta = meta.get(i).cloned().unwrap_or_default();
            StoredPreset {
                name: p.name.clone(),
                brush: StoredBrush::from_brush(&p.brush, &mut res),
                tags: meta.tags,
                favourite: meta.favourite,
            }
        })
        .collect();
    let engines = presets.iter().any(|p| {
        !matches!(
            p.brush.brush_type,
            BrushType::Soft
                | BrushType::Pixel
                | BrushType::Bristle
                | BrushType::Sketch
                | BrushType::Hatching
        )
    });
    let combining = presets.iter().any(|p| {
        use crate::brush_engine::dynamics::DabSetting;
        let b = &p.brush;
        !b.input_combine.is_empty()
            || b.inputs.iter().any(|m| {
                matches!(
                    m.setting,
                    DabSetting::SmudgeLength | DabSetting::ColorRate | DabSetting::PaintThickness
                ) || m.amount.abs() > 1.0
            })
    });
    let library = StoredLibrary {
        format: FORMAT.to_string(),
        version: if combining {
            4
        } else if engines {
            3
        } else {
            2
        },
        presets: stored,
        tips: res
            .tips
            .iter()
            .map(|t| StoredTip {
                width: t.width,
                height: t.height,
                builtin: builtin_tip_name(t).map(str::to_string),
                svg: t.svg.as_deref().map(str::to_string),
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
        if builtin_tip_name(tip).is_some() || tip.svg.is_some() {
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
    Ok(decode_with_meta(bytes)?
        .into_iter()
        .map(|(p, _)| p)
        .collect())
}

/// [`decode`], with each preset's tags and star.
pub fn decode_with_meta(bytes: &[u8]) -> Result<Vec<(BrushPreset, PresetMeta)>, String> {
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
        if let Some(svg) = &stored.svg {
            let side = crate::brush_engine::tip::svg_side(stored.width.max(stored.height) as f32);
            res.tips
                .push(TipMask::from_svg(svg, side).ok_or_else(|| damaged(format!("tip {i}")))?);
            continue;
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
            let preset = BrushPreset {
                name: p.name,
                brush: p.brush.into_brush(&res)?,
                file: None,
            };
            let meta = PresetMeta {
                tags: p.tags,
                favourite: p.favourite,
            };
            Ok((preset, meta))
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
    fn combined_inputs_survive_as_version_4_and_unknown_inputs_are_skipped() {
        use crate::brush_engine::dynamics::{Combine, DabSetting, InputMapping, Sensor};
        let written = |brush: Brush| {
            let preset = BrushPreset {
                name: "P".into(),
                brush,
                file: None,
            };
            let bytes = encode(&[preset]).unwrap();
            let json = zip::read_entry(&bytes, PRESETS_ENTRY).unwrap().to_vec();
            let library: StoredLibrary = serde_json::from_slice(&json).unwrap();
            (library.version, decode(&bytes).unwrap()[0].brush.clone())
        };
        let mut brush = Brush::new(24.0, 20.0, Color32::BLACK, 25.0);
        let input = |sensor, setting| InputMapping {
            sensor,
            setting,
            ..Default::default()
        };
        brush.inputs = vec![
            input(Sensor::Pressure, DabSetting::SmudgeLength),
            input(Sensor::Speed, DabSetting::SmudgeLength),
        ];
        assert_eq!(written(brush.clone()).0, 4, "a new setting");
        brush.inputs[0].setting = DabSetting::Size;
        brush.inputs[1].setting = DabSetting::Size;
        assert_eq!(written(brush.clone()).0, 2, "nothing new");
        brush.input_combine = vec![(DabSetting::Size, Combine::Highest)];
        let (v, back) = written(brush.clone());
        assert_eq!(v, 4);
        assert_eq!(back.input_combine, brush.input_combine);
        assert_eq!(back.inputs, brush.inputs);
        // From a newer version: an input on a setting this one doesn't
        // know is left out, the rest kept.
        let json = br#"{"format":"rusty-painter-brushes","version":4,
            "presets":[{"name":"Future","brush":{"diameter":30.0,"inputs":[
                {"sensor":"Pressure","setting":"Size"},
                {"sensor":"Pressure","setting":"Teleport"}],
                "input_combine":[["Size","Add"],["Size","Sideways"]]}}]}"#;
        let mut zip = zip::ZipWriter::default();
        zip.add(PRESETS_ENTRY, json).unwrap();
        let future = &decode(&zip.finish().unwrap()).unwrap()[0].brush;
        assert_eq!(future.inputs.len(), 1);
        assert_eq!(future.input_combine, [(DabSetting::Size, Combine::Add)]);
    }

    #[test]
    fn new_engines_are_written_as_version_3_and_unknown_types_read_as_soft() {
        let version = |t: BrushType| {
            let mut brush = Brush::new(24.0, 20.0, Color32::BLACK, 25.0);
            brush.brush_type = t;
            brush.engines.spray.amount = 77;
            let preset = BrushPreset {
                name: "P".into(),
                brush,
                file: None,
            };
            let bytes = encode(&[preset]).unwrap();
            let json = zip::read_entry(&bytes, PRESETS_ENTRY).unwrap().to_vec();
            let library: StoredLibrary = serde_json::from_slice(&json).unwrap();
            (library.version, decode(&bytes).unwrap()[0].brush.clone())
        };
        assert_eq!(
            version(BrushType::Sketch).0,
            2,
            "older builds still read it"
        );
        let (v, back) = version(BrushType::Spray);
        assert_eq!(v, 3);
        assert_eq!(
            (back.brush_type, back.engines.spray.amount),
            (BrushType::Spray, 77)
        );
        let json = br#"{"format":"rusty-painter-brushes","version":3,
            "presets":[{"name":"Future","brush":{"brush_type":"Watercolour9000","diameter":30.0}}]}"#;
        let mut zip = zip::ZipWriter::default();
        zip.add(PRESETS_ENTRY, json).unwrap();
        let future = &decode(&zip.finish().unwrap()).unwrap()[0].brush;
        assert_eq!(
            (future.brush_type, future.brush_options.diameter),
            (BrushType::Soft, 30.0)
        );
    }

    #[test]
    fn tags_and_stars_travel_with_the_presets() {
        let preset = |name: &str| BrushPreset {
            name: name.into(),
            brush: Brush::new(24.0, 20.0, Color32::BLACK, 25.0),
            file: None,
        };
        let meta = PresetMeta {
            tags: vec!["Comics".into(), "Ink".into()],
            favourite: true,
        };
        let bytes =
            encode_with_meta(&[preset("A"), preset("B")], std::slice::from_ref(&meta)).unwrap();
        let back = decode_with_meta(&bytes).unwrap();
        assert_eq!(back[0].1, meta);
        assert_eq!(back[1].1, PresetMeta::default(), "missing meta is none");
        // Plain encode writes neither (older builds see the file they knew).
        let json = zip::read_entry(&encode(&[preset("A")]).unwrap(), PRESETS_ENTRY)
            .unwrap()
            .to_vec();
        let json = String::from_utf8(json).unwrap();
        assert!(!json.contains("\"tags\"") && !json.contains("favourite"));
        // Tags aren't settings: the brush compares equal either way.
        assert!(same_settings(&back[0].0.brush, &back[1].0.brush));
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
            correction_live: true,
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
            ) && builtin_tip_name(x).is_some()
            {
                assert!(Arc::ptr_eq(x, y), "{}: built-in tip not shared", a.name);
            }
            if let (Some(x), Some(y)) = (&a.brush.texture, &b.brush.texture)
                && is_builtin_texture(&x.pattern)
            {
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
            krita: Some(crate::brush_engine::brush_options::KritaSmudge {
                dulling: true,
                smear_alpha: false,
                radius: 0.3,
                ..Default::default()
            }),
            load: 12.0,
            keep_dirty: true,
            sample_all: true,
        };
        brush.mixing = Some(mixing);
        brush.antialias_width = 3.0;
        brush.brush_options.blend_mode = BlendMode::Behind;
        brush.dynamics.taper.percent = true;
        brush.dynamics.random.purity = -0.5;
        brush.stay_inside = true;
        brush.sharpness = 0.4;
        brush.brush_options.pressure_spacing = true;
        brush.dynamics.tip.random_flip_x = true;
        brush.dynamics.tip.follow_barrel = true;
        brush.paint_blend = LayerBlend::Parallel;
        let mut texture = BrushTexture::new(crate::brush_engine::texture::builtin()[0].clone());
        texture.krita = Some(crate::brush_engine::texture::KritaTexturing {
            mode: 12,
            soft: false,
        });
        brush.texture = Some(texture);
        let preset = BrushPreset {
            name: "Krita-like".into(),
            brush,
            file: None,
        };
        let back = decode(&encode(&[preset]).unwrap()).unwrap();
        let b = &back[0].brush;
        assert_eq!(b.mixing, Some(mixing));
        assert_eq!(b.antialias_width, 3.0);
        assert_eq!(b.brush_options.blend_mode, BlendMode::Behind);
        assert!(b.dynamics.taper.percent);
        assert_eq!(b.dynamics.random.purity, -0.5);
        assert!(b.stay_inside);
        assert_eq!(b.sharpness, 0.4);
        assert!(b.brush_options.pressure_spacing);
        assert!(b.dynamics.tip.random_flip_x && !b.dynamics.tip.random_flip_y);
        assert!(b.dynamics.tip.follow_barrel);
        assert_eq!(b.paint_blend, LayerBlend::Parallel);
        assert_eq!(
            b.texture.as_ref().unwrap().krita,
            Some(crate::brush_engine::texture::KritaTexturing {
                mode: 12,
                soft: false
            })
        );
    }

    #[test]
    fn the_auto_tip_auto_spacing_and_colour_source_survive() {
        use crate::brush_engine::brush_options::{AutoTip, ColorSource};
        let pattern = crate::brush_engine::texture::builtin()[0].clone();
        let mut brush = Brush::new(30.0, 50.0, Color32::BLACK, 12.0);
        let o = &mut brush.brush_options;
        o.auto_spacing = Some(0.7);
        o.auto_tip = AutoTip {
            spikes: 5,
            fade: [0.3, 0.8],
            density: 0.6,
            randomness: 0.2,
        };
        o.color_source = ColorSource::Pattern {
            pattern: pattern.clone(),
            scale: 2.0,
        };
        let preset = BrushPreset {
            name: "Star".into(),
            brush: brush.clone(),
            file: None,
        };
        let back = decode(&encode(&[preset]).unwrap()).unwrap();
        let b = &back[0].brush.brush_options;
        assert_eq!(b.auto_spacing, Some(0.7));
        assert_eq!(b.auto_tip, brush.brush_options.auto_tip);
        assert_eq!(
            b.color_source,
            ColorSource::Pattern {
                pattern,
                scale: 2.0
            }
        );
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
