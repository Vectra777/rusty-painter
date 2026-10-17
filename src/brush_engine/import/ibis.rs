//! ibisPaint brushes, from the QR codes it exports them as (a picture of
//! one or more codes, a brush in each).
//!
//! A code holds `IPBZ`, the oldest app version that reads it (a big-endian
//! int) and a raw-deflated chunk: ibisPaint's brush parameters
//! (`BrushParameterSubChunk`), big-endian, a fixed head followed by fields
//! newer versions added, each there only if the chunk is long enough. The
//! tip and paper texture aren't in the code: it names them by the MD5 of
//! one of the app's own pictures (RGBA, white, the picture's coverage as
//! alpha). The pictures it can name are in `assets/ibis` (`tips/`,
//! `papers/`, and `patterns.tsv` from MD5 to picture), built into the app;
//! they're also offered in the tip list. Read from ibisPaint 10.1.0.

use super::{Imported, Reader};
use crate::brush_engine::brush::{Brush, BrushPreset, BrushType};
use crate::brush_engine::brush_options::{Mixing, PixelBrushShape};
use crate::brush_engine::dual::DualTip;
use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
use crate::brush_engine::texture::{BrushTexture, Pattern};
use crate::brush_engine::tip::{PngSource, TipMask};
use eframe::egui::Color32;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// Pictures the brush QR codes are read from (by the brush import, not
/// when dropped: a dropped picture opens as one).
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// Every picture under `assets/ibis`: (folder, file stem, PNG).
static FILES: &[(&str, &str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/ibis_patterns.rs"));
/// MD5 (hex), folder, file stem: the patterns ibisPaint's brushes name.
const TABLE: &str = include_str!("../../../assets/ibis/patterns.tsv");
/// The 1×1 white dot ibisPaint names for "no texture".
const NO_PATTERN: [u8; 16] = [
    0xa5, 0x4f, 0x00, 0x41, 0xa9, 0xe1, 0x5b, 0x05, 0x0f, 0x25, 0xc4, 0x63, 0xf1, 0xdb, 0x74, 0x49,
];
const MAGIC: &[u8] = b"IPBZ";
/// The chunk type of a brush's parameters.
const BRUSH_CHUNK: u32 = 0x0100_0202;
/// The most a code's brush inflates to (they're a few hundred bytes).
const MAX_INFLATED: usize = 1 << 20;

/// ibisPaint's tips, for the tip list: `ibis <picture name>`, each decoded
/// the first time it's painted or shown.
pub fn tips() -> &'static [(String, Arc<TipMask>)] {
    static TIPS: OnceLock<Vec<(String, Arc<TipMask>)>> = OnceLock::new();
    TIPS.get_or_init(|| {
        FILES
            .iter()
            .filter(|(folder, ..)| *folder == "tips")
            .filter_map(|&(_, stem, png)| {
                let (w, h) = png_size(png)?;
                let tip = TipMask::from_png(w, h, PngSource::built_in(png), false);
                Some((format!("ibis {stem}"), tip))
            })
            .collect()
    })
}

/// A tip's full picture, decoded for the while (for its thumbnail, without
/// keeping the pixels of a tip nobody paints with).
pub fn decode_tip(tip: &TipMask) -> Option<Arc<TipMask>> {
    let img = image::load_from_memory(&tip.png()?).ok()?.to_luma8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some(TipMask::from_mask(w, h, img.into_raw()))
}

fn png_size(png: &[u8]) -> Option<(usize, usize)> {
    let reader =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    let (w, h) = image::ImageDecoder::dimensions(&reader.into_decoder().ok()?);
    Some((w as usize, h as usize))
}

/// The picture ibisPaint names by `md5`: (folder, stem).
fn named(md5: &[u8]) -> Option<(&'static str, &'static str)> {
    static BY_MD5: OnceLock<HashMap<[u8; 16], (&'static str, &'static str)>> = OnceLock::new();
    let table = BY_MD5.get_or_init(|| {
        TABLE
            .lines()
            .filter_map(|line| {
                let mut cols = line.split('\t');
                let (hex, folder, stem) = (cols.next()?, cols.next()?, cols.next()?);
                let mut md5 = [0u8; 16];
                for (i, byte) in md5.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
                }
                Some((md5, (folder, stem)))
            })
            .collect()
    });
    table.get(md5).copied()
}

fn tip_named(md5: &[u8]) -> Option<Arc<TipMask>> {
    let ("tips", stem) = named(md5)? else {
        return None;
    };
    let name = format!("ibis {stem}");
    tips()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.clone())
}

fn paper_named(md5: &[u8]) -> Option<Arc<Pattern>> {
    let ("papers", stem) = named(md5)? else {
        return None;
    };
    let (_, _, png) = FILES
        .iter()
        .find(|(f, s, _)| *f == "papers" && *s == stem)?;
    let img = image::load_from_memory(png).ok()?;
    Some(Pattern::from_image(&format!("ibis {stem}"), &img))
}

pub(super) fn import(bytes: &[u8], stem: &str) -> Result<Imported, String> {
    let codes = qr_codes(bytes)?;
    if codes.is_empty() {
        return Err("No QR code found in the picture".into());
    }
    let name = brush_name(stem);
    let mut out = Imported::default();
    let mut errors = Vec::new();
    for code in codes {
        match read_payload(&code) {
            Ok(brush) => {
                let n = out.presets.len() + 1;
                let name = match &brush.name {
                    Some(own) if !own.is_empty() => own.clone(),
                    _ if n > 1 => format!("{name} {n}"),
                    _ => name.clone(),
                };
                let preset = to_preset(&brush, &name, &mut out.notes);
                out.presets.push(preset);
            }
            Err(err) => errors.push(err),
        }
    }
    if out.presets.is_empty() {
        return Err(errors
            .into_iter()
            .next()
            .unwrap_or_else(|| "Not an ibisPaint brush QR code".into()));
    }
    out.notes.extend(errors);
    Ok(out)
}

/// The brush's name from the exported picture's: `<brush>_<yyyyMMddHHmmss>`.
fn brush_name(stem: &str) -> String {
    match stem.rsplit_once('_') {
        Some((name, stamp))
            if !name.is_empty()
                && stamp.len() == 14
                && stamp.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name.to_string()
        }
        _ => stem.to_string(),
    }
}

/// The data of every QR code in the picture. A small or blurred one (a
/// screenshot of the code) is tried again enlarged, and black and white:
/// the detector misses codes a few pixels a square.
fn qr_codes(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    use image::imageops::{FilterType, resize};
    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("Not a picture ({e})"))?
        .to_luma8();
    if let codes @ [_, ..] = &decode_qr(&img)[..] {
        return Ok(codes.to_vec());
    }
    let (w, h) = (img.width(), img.height());
    if w.max(h) > 1200 {
        return Ok(Vec::new());
    }
    let tries = [
        (2, FilterType::Nearest, false),
        (3, FilterType::Triangle, false),
        (3, FilterType::Triangle, true),
        (4, FilterType::Triangle, false),
    ];
    for (scale, filter, threshold) in tries {
        let mut big = resize(&img, w * scale, h * scale, filter);
        if threshold {
            let mean = big.pixels().map(|p| p[0] as u64).sum::<u64>() / big.len().max(1) as u64;
            big.pixels_mut()
                .for_each(|p| p[0] = if (p[0] as u64) < mean { 0 } else { 255 });
        }
        let codes = decode_qr(&big);
        if !codes.is_empty() {
            return Ok(codes);
        }
    }
    Ok(Vec::new())
}

fn decode_qr(img: &image::GrayImage) -> Vec<Vec<u8>> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
        img.get_pixel(x as u32, y as u32)[0]
    });
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| {
            let mut data = Vec::new();
            grid.decode_to(&mut data).ok().map(|_| data)
        })
        .collect()
}

/// One code's brush.
fn read_payload(data: &[u8]) -> Result<IbisBrush, String> {
    let rest = data
        .strip_prefix(MAGIC)
        .ok_or("A QR code that isn't an ibisPaint brush")?;
    // The oldest app version that reads it: newer fields are read as far
    // as they're understood, so it isn't checked.
    let deflated = rest.get(4..).ok_or_else(super::short)?;
    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(deflated, MAX_INFLATED)
        .or_else(|_| {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(deflated, MAX_INFLATED)
        })
        .map_err(|_| "An ibisPaint brush QR code that's damaged".to_string())?;
    read_chunk(&mut Reader::new(&raw), 0)
}

/// What a brush code says (the fields this import uses).
#[derive(Default, Debug)]
struct IbisBrush {
    /// Single, Water, Double or Color.
    algorithm: i16,
    thickness: f32,
    thickness_of_start: f32,
    thickness_of_end: f32,
    alpha_of_start: f32,
    alpha_of_end: f32,
    initial_angle: f32,
    /// A share of the thickness.
    spacing: f32,
    /// The dabs' own opacity (it builds up within a stroke).
    pattern_alpha: f32,
    flags: u32,
    /// 0..1, -1 for the app's default.
    thickness_to_pressure: f32,
    opacity_to_pressure: f32,
    jitter_rotation: f32,
    jitter_opacity: f32,
    jitter_thickness: f32,
    /// A Double brush's second brush.
    sub_brushes: Vec<IbisBrush>,
    opacity: f32,
    jitter_position: f32,
    jitter_spacing: f32,
    jitter_hue: f32,
    jitter_saturation: f32,
    jitter_brightness: f32,
    color_mix: f32,
    wetness: f32,
    dragging: f32,
    thickness_to_speed: f32,
    opacity_to_speed: f32,
    texture_scale: f32,
    texture_angle: f32,
    texture_opacity: f32,
    texture_moving: f32,
    tip_md5: Option<Vec<u8>>,
    texture_md5: Option<Vec<u8>>,
    name: Option<String>,
    /// An outline brush's edge: canvas pixels with the absolute flag, a
    /// share of the thickness without.
    frame_size: f32,
    /// Particle brushes (the scatter flag): each particle's size (a share
    /// of the thickness), how many, and how far they spread.
    particle_size: f32,
    particle_density: f32,
    particle_deviation: f32,
    blurring: [f32; 3],
    aspect: f32,
}

// Flag bits (the field at +0x58 of ibisPaint's brush).
const ANTIALIAS: u32 = 1 << 0;
const ROTATION: u32 = 1 << 1;
const SETTING_BRUSH_NAME: u32 = 1 << 12;
const SCATTER: u32 = 1 << 16;
const TEXTURE_INVERT: u32 = 1 << 22;
const ABSOLUTE_FRAME_SIZE: u32 = 1 << 24;

/// A brush chunk, `depth` brushes deep (a Double brush's second brush is
/// one deep; deeper is damage, refused before it overflows the stack).
fn read_chunk(r: &mut Reader<'_>, depth: u32) -> Result<IbisBrush, String> {
    if depth > 2 {
        return Err("An ibisPaint brush QR code that's damaged".into());
    }
    let kind = r.u32_be()?;
    let len = usize::try_from(r.i32_be()?).map_err(|_| super::short())?;
    let body = r.take(len)?;
    if kind != BRUSH_CHUNK {
        return Err(format!("Not an ibisPaint brush (chunk {kind:#x})"));
    }
    read_brush(&mut Reader::new(body), depth)
}

impl Reader<'_> {
    fn left(&self) -> usize {
        self.len() - self.pos()
    }

    fn f32_be(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32_be()?))
    }

    /// A field newer versions added: `default` if the chunk ends first.
    fn opt_f32(&mut self, default: f32) -> Result<f32, String> {
        if self.left() >= 4 {
            self.f32_be()
        } else {
            Ok(default)
        }
    }

    fn opt_i32(&mut self) -> Result<i32, String> {
        if self.left() >= 4 {
            self.i32_be()
        } else {
            Ok(0)
        }
    }

    /// Length-prefixed bytes, or `None` past the chunk's end.
    fn opt_binary(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.left() < 4 {
            return Ok(None);
        }
        let len = usize::try_from(self.i32_be()?).map_err(|_| super::short())?;
        Ok(Some(self.take(len)?.to_vec()))
    }
}

/// ibisPaint's `BrushParameterSubChunk::deserializeClassSpecifics`.
fn read_brush(r: &mut Reader<'_>, depth: u32) -> Result<IbisBrush, String> {
    let mut b = IbisBrush::default();
    let _version = r.u16_be()?;
    b.algorithm = r.u16_be()? as i16;
    // The built-in tip's two ids: the MD5 below says which picture it is.
    r.skip(8)?;
    b.thickness = r.f32_be()?;
    let _max_thickness = r.f32_be()?;
    b.thickness_of_start = r.f32_be()?;
    b.thickness_of_end = r.f32_be()?;
    b.alpha_of_start = r.f32_be()?;
    b.alpha_of_end = r.f32_be()?;
    b.initial_angle = r.f32_be()?;
    b.spacing = r.f32_be()?;
    b.pattern_alpha = r.f32_be()?;
    b.flags = r.u32_be()?;
    b.thickness_to_pressure = r.opt_f32(-1.0)?;
    b.opacity_to_pressure = r.opt_f32(-1.0)?;
    b.jitter_rotation = r.opt_f32(0.0)?;
    b.jitter_opacity = r.opt_f32(0.0)?;
    b.jitter_thickness = r.opt_f32(0.0)?;
    for _ in 0..r.opt_i32()? {
        b.sub_brushes.push(read_chunk(r, depth + 1)?);
    }
    b.opacity = r.opt_f32(1.0)?;
    b.jitter_position = r.opt_f32(0.0)?;
    b.jitter_spacing = r.opt_f32(0.0)?;
    b.jitter_hue = r.opt_f32(0.0)?;
    b.jitter_saturation = r.opt_f32(0.0)?;
    b.jitter_brightness = r.opt_f32(0.0)?;
    b.color_mix = r.opt_f32(0.0)?;
    b.wetness = r.opt_f32(0.0)?;
    b.dragging = r.opt_f32(0.0)?;
    b.thickness_to_speed = r.opt_f32(0.0)?;
    b.opacity_to_speed = r.opt_f32(0.0)?;
    let _blurring_to_speed = r.opt_f32(0.0)?;
    let _blurring_to_pressure = r.opt_f32(0.0)?;
    b.texture_scale = r.opt_f32(1.0)?;
    b.texture_angle = r.opt_f32(0.0)?;
    b.texture_opacity = r.opt_f32(0.0)?;
    b.texture_moving = r.opt_f32(0.0)?;
    let _min_thickness = r.opt_f32(0.0)?;
    b.tip_md5 = r.opt_binary()?;
    b.texture_md5 = r.opt_binary()?;
    if b.flags & SETTING_BRUSH_NAME != 0 && r.left() >= 2 {
        let len = r.u16_be()? as usize;
        b.name = Some(modified_utf8(r.take(len)?));
    }
    // Then an int, the inner colour, frame shadow and particle settings,
    // the blurring, its texture and a few more, all read where used.
    let _ = r.opt_i32()?;
    if r.left() >= 4 {
        r.skip(4)?;
    }
    b.frame_size = r.opt_f32(0.2)?;
    b.particle_size = r.opt_f32(0.05)?;
    b.particle_density = r.opt_f32(5.0)?;
    b.particle_deviation = r.opt_f32(0.0)?;
    for v in &mut b.blurring {
        *v = r.opt_f32(0.0)?;
    }
    let _blurring_texture = r.opt_binary()?;
    for _ in 0..4 {
        r.opt_f32(0.0)?; // blurring texture degree, jitter, frame shadow angle, distance
    }
    b.aspect = r.opt_f32(0.0)?;
    Ok(b)
}

/// Java's modified UTF-8: each UTF-16 unit in one to three bytes, so an
/// emoji (two units) is two three-byte runs, not UTF-8's four bytes.
fn modified_utf8(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as u16;
        let next = |k: usize| bytes.get(i + k).map_or(0, |&b| b as u16 & 0x3F);
        let (unit, len) = match b0 {
            0..=0x7F => (b0, 1),
            0xC0..=0xDF => (((b0 & 0x1F) << 6) | next(1), 2),
            0xE0..=0xEF => (((b0 & 0x0F) << 12) | (next(1) << 6) | next(2), 3),
            _ => (0xFFFD, 1),
        };
        units.push(unit);
        i += len;
    }
    String::from_utf16_lossy(&units)
}

/// The preset painting like `b`, as near as this engine gets; `notes` gets
/// what didn't come across.
fn to_preset(b: &IbisBrush, name: &str, notes: &mut Vec<String>) -> BrushPreset {
    let diameter = b.thickness.clamp(1.0, 3000.0);
    let spacing = (b.spacing * 100.0).clamp(1.0, 1000.0);
    let mut brush = Brush::new(diameter, 100.0, Color32::BLACK, spacing);
    let o = &mut brush.brush_options;
    match b.tip_md5.as_deref().and_then(tip_named) {
        Some(tip) => o.pixel_shape = PixelBrushShape::Custom(tip),
        None => notes.push(format!(
            "{name}: its tip isn't among the ibisPaint tips this app has (some ibisPaint makes itself), so it's round"
        )),
    }
    o.opacity = b.opacity.clamp(0.0, 1.0);
    o.flow = (b.pattern_alpha * 100.0).clamp(1.0, 100.0);
    // Pressure: how far it thins / fades the dabs (-1: ibisPaint's default,
    // none).
    let size_pressure = b.thickness_to_pressure.clamp(0.0, 1.0);
    o.pressure_size = size_pressure > 0.0;
    o.pressure_min_size = 1.0 - size_pressure;
    let opacity_pressure = b.opacity_to_pressure.clamp(0.0, 1.0);
    o.pressure_opacity = opacity_pressure > 0.0;
    if o.pressure_opacity && opacity_pressure < 1.0 {
        o.pressure_curves.opacity = Some(SoftnessCurve {
            points: vec![
                CurvePoint::new(0.0, 1.0 - opacity_pressure),
                CurvePoint::new(1.0, 1.0),
            ],
        });
    }
    brush.anti_aliasing = b.flags & ANTIALIAS != 0;
    let d = &mut brush.dynamics;
    d.tip.angle = b.initial_angle;
    d.tip.follow_stroke = b.flags & ROTATION != 0;
    d.tip.random_angle = (b.jitter_rotation * 180.0).clamp(0.0, 180.0);
    if b.aspect != 0.0 {
        d.tip.ratio = (1.0 - b.aspect.abs()).clamp(0.02, 1.0);
    }
    d.random.size = b.jitter_thickness.clamp(0.0, 1.0);
    d.random.opacity = b.jitter_opacity.clamp(0.0, 1.0);
    let hsv = [
        (b.jitter_hue * 180.0).clamp(0.0, 180.0),
        b.jitter_saturation.clamp(0.0, 1.0),
        b.jitter_brightness.clamp(0.0, 1.0),
    ];
    let particles = b.flags & SCATTER != 0;
    if !particles {
        [d.random.hue, d.random.saturation, d.random.value] = hsv;
    }
    d.speed.size = b.thickness_to_speed.clamp(-1.0, 1.0);
    d.speed.opacity = b.opacity_to_speed.clamp(-1.0, 1.0);
    brush.jitter = (b.jitter_position * 100.0).clamp(0.0, 500.0);
    // Start and end: the share of the thickness / opacity the stroke has
    // at its ends. ibisPaint's fade length isn't in the code: a share of
    // the stroke, so it holds at any brush size.
    let ends = [
        b.thickness_of_start,
        b.thickness_of_end,
        b.alpha_of_start,
        b.alpha_of_end,
    ]
    .map(|v| v.clamp(0.0, 1.0));
    if ends.iter().any(|&v| v < 1.0) {
        let length = 15.0;
        let taper = &mut d.taper;
        taper.percent = true;
        taper.size = ends[0] < 1.0 || ends[1] < 1.0;
        taper.opacity = ends[2] < 1.0 || ends[3] < 1.0;
        taper.start = if ends[0] < 1.0 || ends[2] < 1.0 {
            length
        } else {
            0.0
        };
        taper.end = if ends[1] < 1.0 || ends[3] < 1.0 {
            length
        } else {
            0.0
        };
        taper.min = ends.into_iter().fold(1.0, f32::min);
    }
    if b.jitter_spacing > 0.0 {
        notes.push(format!("{name}: its spacing jitter wasn't imported"));
    }
    match b.texture_md5.as_deref() {
        None => {}
        Some(md5) if md5 == NO_PATTERN => {}
        Some(md5) => match paper_named(md5) {
            Some(pattern) if b.texture_opacity > 0.0 => {
                let mut texture = BrushTexture::new(pattern);
                texture.scale = b.texture_scale.clamp(0.05, 20.0);
                texture.strength = b.texture_opacity.clamp(0.0, 1.0);
                texture.invert = b.flags & TEXTURE_INVERT != 0;
                texture.placement.angle = b.texture_angle;
                texture.placement.follow_stroke = b.texture_moving > 0.0;
                brush.texture = Some(texture);
            }
            Some(_) => {}
            None => notes.push(format!("{name}: its texture isn't one of ibisPaint's own")),
        },
    }
    match b.algorithm {
        // Water: the brush picks up and drags the paint under it.
        1 => {
            brush.mixing = Some(Mixing {
                smudge_length: b.dragging.clamp(0.0, 1.0),
                color_rate: (1.0 - b.color_mix).clamp(0.0, 1.0),
                ..Mixing::default()
            });
            // Mixing samples the canvas under each dab: at ibisPaint's usual
            // 1% a big brush falls seconds behind the pen.
            const MIXING_SPACING: f32 = 10.0;
            if brush.brush_options.spacing < MIXING_SPACING {
                brush.brush_options.spacing = MIXING_SPACING;
                notes.push(format!(
                    "{name}: spacing raised to {MIXING_SPACING}% (colour mixing at ibisPaint's {:.0}% lags)",
                    b.spacing * 100.0
                ));
            }
            if b.wetness > 0.0 {
                notes.push(format!(
                    "{name}: its wetness is approximated by colour mixing"
                ));
            }
        }
        // Double: with a second brush, its tip masks this one.
        2 => match b.sub_brushes.first() {
            Some(second) => {
                let shape = second
                    .tip_md5
                    .as_deref()
                    .and_then(tip_named)
                    .map_or(PixelBrushShape::Circle, PixelBrushShape::Custom);
                brush.dual = Some(DualTip {
                    shape,
                    size: (second.thickness / b.thickness.max(1e-3)).clamp(0.01, 10.0),
                    spacing: (second.spacing * 100.0).clamp(1.0, 1000.0),
                    scatter: (second.jitter_position * 100.0).clamp(0.0, 500.0),
                    random_angle: second.jitter_rotation > 0.0,
                    ..DualTip::default()
                });
            }
            // Without one, the outline brush: an edge in the brush's colour
            // around the stroke (ibisPaint's frame), its inside in another.
            // The stroke's rim kept and its middle thinned comes nearest.
            None => {
                let width = if b.flags & ABSOLUTE_FRAME_SIZE != 0 {
                    b.frame_size
                } else {
                    b.frame_size * diameter
                };
                brush.wet_edge = 0.95;
                brush.wet_edge_width = width.clamp(1.0, 200.0);
                notes.push(format!(
                    "{name}: an outline brush, drawn as its stroke's edge; its inside colour isn't painted"
                ));
            }
        },
        // Color: tips in colour, from pictures the app doesn't share.
        3 => notes.push(format!(
            "{name}: ibisPaint's colour brushes' tips aren't available, so it paints in one colour"
        )),
        _ => {}
    }
    // Particles: each dab a cloud of small copies of the tip over the
    // brush's area, their colour jittered one by one (ibisPaint's grainy
    // pencils and crayons).
    if particles {
        brush.brush_type = BrushType::Spray;
        let spray = &mut brush.engines.spray;
        spray.amount = (b.particle_density.round() as u32).clamp(1, 500);
        spray.particle_size = b.particle_size.clamp(0.005, 1.0);
        spray.size_random = b.particle_deviation.clamp(0.0, 1.0);
        spray.random_rotation = b.jitter_rotation > 0.0;
        spray.jitter = b.jitter_position.clamp(0.0, 1.0);
        spray.random_hsv = hsv;
        spray.random_opacity = b.jitter_opacity > 0.0;
        brush.jitter = 0.0;
    }
    if b.blurring.iter().any(|&v| v > 0.0) {
        notes.push(format!("{name}: its blurring wasn't imported"));
    }
    BrushPreset {
        name: name.to_string(),
        brush,
        file: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/testdata/ibis/{name}.ipbz", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(path).unwrap()
    }

    #[test]
    fn exported_brushes_read_to_the_end() {
        for name in ["airbrush-normal", "pen-soft", "test1"] {
            let b = read_payload(&fixture(name)).unwrap();
            assert_eq!(b.algorithm, 0, "{name}: Single");
            assert!(b.tip_md5.is_some(), "{name}: read as far as the tip");
            assert_eq!(b.texture_md5.as_deref(), Some(&NO_PATTERN[..]), "{name}");
        }
        let air = read_payload(&fixture("airbrush-normal")).unwrap();
        assert_eq!((air.thickness, air.opacity), (269.0, 0.51));
        assert!((air.spacing - 0.2).abs() < 1e-6);
        let test1 = read_payload(&fixture("test1")).unwrap();
        assert_eq!(test1.name.as_deref(), Some("test1"));
    }

    #[test]
    fn exported_brushes_get_their_own_tips() {
        let mut notes = Vec::new();
        for (name, tip) in [
            ("airbrush-normal", "brush_air01"),
            ("pen-soft", "brush_air02"),
            ("test1", "brush_pen_fade"),
        ] {
            let preset = to_preset(&read_payload(&fixture(name)).unwrap(), name, &mut notes);
            let PixelBrushShape::Custom(got) = &preset.brush.brush_options.pixel_shape else {
                panic!("{name}: a picture tip");
            };
            let want = tip_named(&read_payload(&fixture(name)).unwrap().tip_md5.unwrap());
            assert!(Arc::ptr_eq(got, &want.unwrap()), "{name}: {tip}");
            assert!(
                tips()
                    .iter()
                    .any(|(n, t)| *n == format!("ibis {tip}") && Arc::ptr_eq(t, got))
            );
        }
        assert!(notes.is_empty(), "{notes:?}");
        let pen = to_preset(
            &read_payload(&fixture("pen-soft")).unwrap(),
            "p",
            &mut notes,
        );
        let taper = &pen.brush.dynamics.taper;
        assert!(taper.size && taper.start > 0.0 && taper.end > 0.0 && taper.min == 0.0);
    }

    #[test]
    fn every_table_entry_has_its_picture() {
        for line in TABLE.lines() {
            let mut cols = line.split('\t');
            let (_, folder, stem) = (cols.next(), cols.next().unwrap(), cols.next().unwrap());
            assert!(
                FILES.iter().any(|(f, s, _)| *f == folder && *s == stem),
                "{line}"
            );
        }
        assert!(tips().iter().all(|(_, t)| t.width > 1 && t.height > 1));
    }

    #[test]
    fn a_tip_paints_from_its_png() {
        let (_, tip) = tips()
            .iter()
            .find(|(n, _)| n == "ibis brush_air01")
            .unwrap();
        let r = tip.width as f32 / 2.0;
        assert!(
            tip.sample(0.0, 0.0, r) > 0.5,
            "the airbrush's middle paints"
        );
        assert!(decode_tip(tip).is_some());
    }

    #[test]
    fn names_lose_the_export_time_and_damage_is_an_error() {
        assert_eq!(brush_name("Plume (Douce)_20261016232843"), "Plume (Douce)");
        assert_eq!(brush_name("my_brush"), "my_brush");
        assert!(read_payload(b"IPBZ\0\0\0\x01garbage").is_err());
        assert!(read_payload(b"something else").is_err());
        let mut cut = fixture("test1");
        cut.truncate(40);
        assert!(read_payload(&cut).is_err());
    }

    #[test]
    fn a_particle_brush_sprays_its_tip_with_each_speck_s_colour_jittered() {
        let b = read_payload(&fixture("hard-sketch")).unwrap();
        assert_eq!(b.name.as_deref(), Some("hard sketch/lineart"));
        let preset = to_preset(&b, "hard sketch", &mut Vec::new());
        let brush = &preset.brush;
        assert_eq!(brush.brush_type, BrushType::Spray);
        let spray = &brush.engines.spray;
        assert_eq!(spray.amount, 5);
        assert!((spray.particle_size - 0.109).abs() < 1e-6);
        assert!(spray.random_hsv[1] > 0.5, "saturation per speck");
        let random = &brush.dynamics.random;
        assert_eq!((random.hue, random.saturation), (0.0, 0.0), "not per cloud");
        assert!(brush.dynamics.taper.size && brush.dynamics.taper.percent);
    }

    #[test]
    fn an_outline_brush_keeps_its_stroke_s_edge() {
        let b = read_payload(&fixture("fluffy-outline")).unwrap();
        assert_eq!(b.name.as_deref(), Some("もこもこペン(線)"));
        let mut notes = Vec::new();
        let preset = to_preset(&b, "fluffy", &mut notes);
        let brush = &preset.brush;
        assert_eq!(brush.wet_edge, 0.95);
        assert!(
            (brush.wet_edge_width - 2.8).abs() < 1e-3,
            "its frame, in pixels"
        );
        assert!(brush.dual.is_none());
        assert!(brush.jitter > 30.0, "the bumps: dabs scattered");
        assert_eq!(notes.len(), 2, "its tip and its inside: {notes:?}");
    }

    #[test]
    fn a_small_blurred_screenshot_of_a_code_is_read() {
        // 240 px, about 4 a square: found only enlarged.
        let path = format!(
            "{}/testdata/ibis/small-screenshot.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let picture = std::fs::read(path).unwrap();
        let codes = qr_codes(&picture).unwrap();
        assert_eq!(codes.len(), 1);
        let brush = read_payload(&codes[0]).unwrap();
        assert!(brush.name.is_some_and(|n| n.starts_with('Ⓢ')));
    }

    #[test]
    fn a_water_brush_mixes_at_a_spacing_that_keeps_up() {
        let b = read_payload(&fixture("watercolor")).unwrap();
        assert_eq!(b.algorithm, 1, "Water");
        let mut notes = Vec::new();
        let preset = to_preset(&b, "watercolor", &mut notes);
        assert!(preset.brush.mixing.is_some());
        assert_eq!(
            preset.brush.brush_options.spacing, 10.0,
            "not ibisPaint's 1%"
        );
        assert!(notes.iter().any(|n| n.contains("spacing raised")));
    }

    #[test]
    fn names_with_emoji_read_as_java_wrote_them() {
        // "a🎨" as Java's modified UTF-8: the emoji's two halves apart.
        let bytes = [b'a', 0xED, 0xA0, 0xBC, 0xED, 0xBE, 0xA8];
        assert_eq!(modified_utf8(&bytes), "a🎨");
        assert_eq!(modified_utf8("もこもこ".as_bytes()), "もこもこ");
    }

    #[test]
    fn brushes_nested_past_a_double_brush_are_refused() {
        // A brush whose fields end with one sub-brush, `depth` times over.
        fn nested(depth: u32) -> Vec<u8> {
            let mut body = vec![0u8; 2 + 2 + 8 + 9 * 4 + 4 + 5 * 4];
            if depth > 0 {
                let inner = nested(depth - 1);
                body.extend(1i32.to_be_bytes());
                body.extend(inner);
            }
            let mut chunk = BRUSH_CHUNK.to_be_bytes().to_vec();
            chunk.extend((body.len() as i32).to_be_bytes());
            chunk.extend(body);
            chunk
        }
        assert!(
            read_chunk(&mut Reader::new(&nested(1)), 0).is_ok(),
            "a Double brush"
        );
        assert!(read_chunk(&mut Reader::new(&nested(10)), 0).is_err());
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_ibis() {
        let seed = fixture("test1");
        crate::fuzz::fuzz("ibis", &seed, std::time::Duration::from_secs(2), |b| {
            let _ = read_payload(b);
        });
        // The brush itself, past the deflate most mutations stop at.
        let chunk = miniz_oxide::inflate::decompress_to_vec(&seed[8..]).unwrap();
        crate::fuzz::fuzz(
            "ibis chunk",
            &chunk,
            std::time::Duration::from_secs(2),
            |b| {
                if let Ok(brush) = read_chunk(&mut Reader::new(b), 0) {
                    let _ = to_preset(&brush, "fuzz", &mut Vec::new());
                }
            },
        );
    }
}
