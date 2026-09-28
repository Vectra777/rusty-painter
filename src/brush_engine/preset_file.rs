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
//! Every setting has a default, so files from older versions (or written by
//! hand) load with whatever they leave out at its default.

use crate::brush_engine::brush::{Brush, BrushPreset, BrushType, StabilizerAlgorithm};
use crate::brush_engine::brush_options::{
    BlendMode, PaintingMode, PixelBrushShape, PressureCurves, TipOrder,
};
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
const VERSION: u32 = 1;
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
}

#[derive(Serialize, Deserialize)]
struct StoredTexture {
    name: String,
    size: usize,
}

#[derive(Serialize, Deserialize)]
enum StoredShape {
    Circle,
    Square,
    /// A tip from the file's `tips`.
    Tip(usize),
}

#[derive(Serialize, Deserialize)]
struct StoredBrushTexture {
    /// A texture from the file's `textures`.
    pattern: usize,
    mode: TextureMode,
    scale: f32,
    strength: f32,
    invert: bool,
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
    pressure_curves: PressureCurves,
    pixel_perfect: bool,
    anti_aliasing: bool,
    jitter: f32,
    stabilizer: f32,
    stabilizer_algorithm: StabilizerAlgorithm,
    stabilizer_mass: f32,
    stabilizer_drag: f32,
    dynamics: BrushDynamics,
    texture: Option<StoredBrushTexture>,
    /// A [`LayerBlend::key`].
    paint_blend: String,
    airbrush_rate: f32,
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
            shape: match &o.pixel_shape {
                PixelBrushShape::Circle => StoredShape::Circle,
                PixelBrushShape::Square => StoredShape::Square,
                PixelBrushShape::Custom(tip) => StoredShape::Tip(res.tip(tip)),
            },
            extra_tips: o.extra_tips.iter().map(|t| res.tip(t)).collect(),
            tip_order: o.tip_order,
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
            pressure_curves: o.pressure_curves.clone(),
            pixel_perfect: b.pixel_perfect,
            anti_aliasing: b.anti_aliasing,
            jitter: b.jitter,
            stabilizer: b.stabilizer,
            stabilizer_algorithm: b.stabilizer_algorithm,
            stabilizer_mass: b.stabilizer_mass,
            stabilizer_drag: b.stabilizer_drag,
            dynamics: b.dynamics,
            texture: b.texture.as_ref().map(|t| StoredBrushTexture {
                pattern: res.texture(&t.pattern),
                mode: t.mode,
                scale: t.scale,
                strength: t.strength,
                invert: t.invert,
            }),
            paint_blend: b.paint_blend.key().to_string(),
            airbrush_rate: b.airbrush_rate,
        }
    }

    fn into_brush(self, res: &Resources) -> Result<Brush, String> {
        let mut b = Brush::new(self.diameter, self.hardness, Color32::BLACK, self.spacing);
        b.brush_type = self.brush_type;
        let o = &mut b.brush_options;
        o.softness_selector = self.softness_selector;
        o.softness_curve = self.softness_curve;
        o.pixel_shape = match self.shape {
            StoredShape::Circle => PixelBrushShape::Circle,
            StoredShape::Square => PixelBrushShape::Square,
            StoredShape::Tip(i) => PixelBrushShape::Custom(
                res.tips
                    .get(i)
                    .cloned()
                    .ok_or_else(|| format!("Missing brush tip {i}"))?,
            ),
        };
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
        o.pressure_curves = self.pressure_curves;
        b.pixel_perfect = self.pixel_perfect;
        b.anti_aliasing = self.anti_aliasing;
        b.jitter = self.jitter;
        b.stabilizer = self.stabilizer;
        b.stabilizer_algorithm = self.stabilizer_algorithm;
        b.stabilizer_mass = self.stabilizer_mass;
        b.stabilizer_drag = self.stabilizer_drag;
        b.dynamics = self.dynamics;
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
            }),
        };
        b.paint_blend = LayerBlend::from_key(&self.paint_blend).unwrap_or_default();
        b.airbrush_rate = self.airbrush_rate;
        Ok(b)
    }
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
            })
            .collect(),
        textures: res
            .textures
            .iter()
            .map(|t| StoredTexture {
                name: t.name.clone(),
                size: t.size,
            })
            .collect(),
    };
    let mut zip = zip::ZipWriter::default();
    let json = serde_json::to_vec_pretty(&library).map_err(|e| e.to_string())?;
    zip.add(PRESETS_ENTRY, &json)?;
    for (i, tip) in res.tips.iter().enumerate() {
        let img =
            image::GrayImage::from_raw(tip.width as u32, tip.height as u32, tip.pixels.clone())
                .ok_or("Brush tip has the wrong size")?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        zip.add(&format!("tips/{i}.png"), &png)?;
    }
    for (i, pattern) in res.textures.iter().enumerate() {
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
        if stored.width == 0 || stored.height == 0 || stored.width.max(stored.height) > MAX_SIDE {
            return Err(damaged(format!("tip {i} size")));
        }
        let png = zip::read_entry(bytes, &format!("tips/{i}.png")).map_err(damaged)?;
        let grey = image::load_from_memory(png)
            .map_err(|e| damaged(e.to_string()))?
            .to_luma8();
        if (grey.width() as usize, grey.height() as usize) != (stored.width, stored.height) {
            return Err(damaged(format!("tip {i} size")));
        }
        let pixels = grey.into_raw();
        let builtin = crate::brush_engine::tip::builtin().iter().find(|(_, t)| {
            (t.width, t.height) == (stored.width, stored.height) && t.pixels == pixels
        });
        res.tips.push(match builtin {
            Some((_, t)) => t.clone(),
            None => TipMask::from_mask(stored.width, stored.height, pixels),
        });
    }
    for (i, stored) in library.textures.iter().enumerate() {
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
    fn damaged_files_are_refused_not_panicked_on() {
        let presets = crate::PainterApp::create_default_brush_presets(Color32::BLACK);
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
        let presets = crate::PainterApp::create_default_brush_presets(Color32::BLACK);
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
    fn preset_names_become_safe_file_names() {
        assert_eq!(file_stem("Ink Pen (fine)"), "Ink Pen (fine)");
        assert_eq!(file_stem("a/b\\c:d"), "abcd");
        assert_eq!(file_stem("../"), "Brush");
    }
}
