//! Opening `.kra` documents: a ZIP with the layer tree in
//! `maindoc.xml` and each layer's pixels in its own file, as 64×64 tiles
//! (LZF-compressed, one colour channel after another, B G R A).
//!
//! Paint layers (8 or 16 bits per channel or 16/32-bit float, RGBA; the
//! deeper ones open as a 16-bit or float document), folders, opacity,
//! visibility, blend modes, alpha inheritance (as clipping) and
//! transparency masks come across. Layers of other kinds (filters, fills,
//! vectors, files, clones) and other colour spaces are left out; when any
//! is, the file's own flattened picture is added as a hidden top layer so the
//! look isn't lost. A document that can't be read layer by layer opens
//! flattened.

use crate::brush_engine::import::krita::{Node, parse_xml};
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::storage::Depth;
use crate::project::psd::{PsdDocument, PsdKind, PsdLayer, PsdMask};
use eframe::egui::Color32;
use std::collections::HashMap;

/// A `.kra` file as a layered document.
pub fn decode_kra(bytes: &[u8]) -> Result<PsdDocument, String> {
    let entries: HashMap<String, Vec<u8>> = crate::project::zip::read_all(bytes)
        .map_err(|_| "Not a Krita document (damaged archive)".to_string())?
        .into_iter()
        .collect();
    if entries
        .get("mimetype")
        .is_some_and(|m| m.as_slice() != b"application/x-krita")
    {
        return Err("Not a Krita document".into());
    }
    match layered(&entries) {
        Ok(doc) => Ok(doc),
        Err(err) => {
            log::warn!("Opening the Krita document flattened: {err}");
            flattened(&entries).map_err(|_| err)
        }
    }
}

/// The file's flattened picture (`mergedimage.png`) as one layer.
fn flattened(entries: &HashMap<String, Vec<u8>>) -> Result<PsdDocument, String> {
    let (w, h, pixels) = merged_image(entries)?;
    Ok(PsdDocument {
        depth: Depth::U8,
        composite_deep: None,
        profile: Default::default(),
        width: w,
        height: h,
        layers: vec![pixel_layer(
            "Krita picture",
            [0, 0, w as i32, h as i32],
            pixels.clone(),
        )],
        composite: pixels,
    })
}

fn merged_image(
    entries: &HashMap<String, Vec<u8>>,
) -> Result<(usize, usize, Vec<Color32>), String> {
    let png = entries
        .get("mergedimage.png")
        .ok_or("The Krita document has no flattened picture")?;
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let pixels = img
        .pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    Ok((w, h, pixels))
}

fn pixel_layer(name: &str, rect: [i32; 4], pixels: Vec<Color32>) -> PsdLayer {
    PsdLayer {
        deep: None,
        name: name.into(),
        kind: PsdKind::Pixels,
        rect,
        pixels,
        opacity: 1.0,
        visible: true,
        blend: LayerBlend::Normal,
        clipped: false,
        alpha_locked: false,
        position_locked: false,
        mask: None,
    }
}

struct Reader<'a> {
    entries: &'a HashMap<String, Vec<u8>>,
    /// Where the layer files are: `<image name>/layers/`.
    dir: String,
    size: (usize, usize),
    /// Something was left out.
    skipped: bool,
    /// The deepest layer so far.
    depth: Depth,
}

fn layered(entries: &HashMap<String, Vec<u8>>) -> Result<PsdDocument, String> {
    let xml = entries
        .get("maindoc.xml")
        .ok_or("The Krita document has no maindoc.xml")?;
    let doc = parse_xml(&String::from_utf8_lossy(xml))?;
    let image = doc.find("IMAGE").ok_or("The Krita document has no image")?;
    let num = |k: &str| image.attr(k).and_then(|v| v.parse::<usize>().ok());
    let (w, h) = (
        num("width").ok_or("No width")?,
        num("height").ok_or("No height")?,
    );
    // Before any buffer the size of the canvas is made.
    crate::app::document::validate_canvas_size(w, h)?;
    let name = image.attr("name").unwrap_or_default();
    let mut reader = Reader {
        entries,
        dir: format!("{name}/layers/"),
        size: (w, h),
        skipped: false,
        depth: Depth::U8,
    };
    let mut layers = Vec::new();
    if let Some(list) = child(image, "layers") {
        reader.list(list, &mut layers, 0)?;
    }
    if reader.skipped
        && let Ok((mw, mh, pixels)) = merged_image(entries)
        && (mw, mh) == (w, h)
    {
        let mut flat = pixel_layer(
            "Krita's picture (parts not brought across)",
            [0, 0, w as i32, h as i32],
            pixels,
        );
        flat.visible = false;
        layers.push(flat);
    }
    if layers.is_empty() {
        return Err("The Krita document has no layers this can open".into());
    }
    // The image's colour profile.
    let profile = entries
        .get(&format!("{name}/annotations/icc"))
        .and_then(|icc| {
            let label = image.attr("profile").unwrap_or("Krita profile");
            crate::canvas::color_profile::ColorProfile::from_icc(icc.clone(), label).ok()
        })
        .unwrap_or_default();
    Ok(PsdDocument {
        depth: reader.depth,
        composite_deep: None,
        profile,
        width: w,
        height: h,
        layers,
        composite: Vec::new(),
    })
}

fn child<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    node.children.iter().find(|c| c.name == name)
}

impl Reader<'_> {
    /// A `<layers>` list (top first in the file) into `out`, bottom first,
    /// folders as their end marker, contents, then their own record.
    fn list(&mut self, list: &Node, out: &mut Vec<PsdLayer>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("Folders nested too deep".into());
        }
        for layer in list.children.iter().rev().filter(|c| c.name == "layer") {
            let common = |kind: PsdKind| {
                let mut l = pixel_layer(layer.attr("name").unwrap_or("Layer"), [0; 4], Vec::new());
                l.kind = kind;
                l.opacity = layer
                    .attr("opacity")
                    .and_then(|v| v.parse::<f32>().ok())
                    .map_or(1.0, |v| (v / 255.0).clamp(0.0, 1.0));
                l.visible = layer.attr("visible") != Some("0");
                l.blend = blend(layer.attr("compositeop").unwrap_or("normal"));
                l.clipped = layer.attr("inheritalpha") == Some("1");
                l.position_locked = layer.attr("locked") == Some("1");
                l
            };
            match layer.attr("nodetype") {
                Some("grouplayer") => {
                    out.push(common(PsdKind::GroupEnd));
                    if let Some(inner) = child(layer, "layers") {
                        self.list(inner, out, depth + 1)?;
                    }
                    out.push(common(PsdKind::GroupStart));
                }
                Some("paintlayer") => {
                    let mut l = common(PsdKind::Pixels);
                    match self.paint_layer(layer) {
                        Ok((rect, pixels, deep)) => {
                            l.rect = rect;
                            l.pixels = pixels;
                            l.deep = deep;
                        }
                        Err(err) => {
                            log::warn!("Krita layer {} left out: {err}", l.name);
                            self.skipped = true;
                            continue;
                        }
                    }
                    l.alpha_locked = layer
                        .attr("channellockflags")
                        .is_some_and(|f| f.len() == 4 && f.ends_with('0'));
                    l.mask = self.mask(layer);
                    out.push(l);
                }
                _ => self.skipped = true,
            }
        }
        Ok(())
    }

    fn file(&self, name: &str) -> Option<&Vec<u8>> {
        self.entries.get(&format!("{}{name}", self.dir))
    }

    /// A paint layer's pixels: the rectangle its tiles cover (the whole
    /// canvas too when its empty pixels aren't transparent).
    fn paint_layer(&mut self, layer: &Node) -> Result<LayerPixels, String> {
        let space = layer.attr("colorspacename").unwrap_or("RGBA");
        let channel = match space {
            "RGBA" => Channel::U8,
            "RGBA16" => Channel::U16,
            "RGBAF16" => Channel::F16,
            "RGBAF32" => Channel::F32,
            other => return Err(format!("colour space {other}")),
        };
        let depth = channel.bytes();
        let deep_at = channel.depth();
        self.depth = match (self.depth, deep_at) {
            (Depth::F32, _) | (_, Depth::F32) => Depth::F32,
            (Depth::U16, _) | (_, Depth::U16) => Depth::U16,
            _ => Depth::U8,
        };
        let filename = layer.attr("filename").ok_or("no file")?;
        let data = self.file(filename).ok_or("its pixels are missing")?;
        let offset = (
            layer
                .attr("x")
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0),
            layer
                .attr("y")
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0),
        );
        let tiles = read_tiles(data, 4 * depth)?;
        let default = self
            .file(&format!("{filename}.defaultpixel"))
            .map_or([0.0; 4], |d| channel.linear(d, 0, 1));
        let default8 = to_color32(default);
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let mut rect = if default8.a() > 0 {
            [0, 0, w, h]
        } else {
            [i32::MAX, i32::MAX, i32::MIN, i32::MIN]
        };
        for t in &tiles {
            let (x, y) = (t.x + offset.0, t.y + offset.1);
            rect = [
                rect[0].min(x),
                rect[1].min(y),
                rect[2].max(x + t.w),
                rect[3].max(y + t.h),
            ];
        }
        if rect[0] >= rect[2] {
            return Ok(([0; 4], Vec::new(), None));
        }
        let rw = (rect[2] - rect[0]) as usize;
        let rh = (rect[3] - rect[1]) as usize;
        if rw * rh > 1 << 28 {
            return Err("too large".into());
        }
        // At full depth a pixel takes 16 bytes: a gigabyte at most.
        if channel != Channel::U8 && rw * rh > MAX_DEEP_PIXELS {
            return Err("too large at full depth".into());
        }
        let mut pixels = vec![default8; rw * rh];
        // Deeper than 8 bits: the pixels at full depth too.
        let mut deep = (channel != Channel::U8).then(|| vec![default; rw * rh]);
        for t in &tiles {
            let n = (t.w * t.h) as usize;
            for ty in 0..t.h {
                for tx in 0..t.w {
                    let (x, y) = (t.x + offset.0 + tx - rect[0], t.y + offset.1 + ty - rect[1]);
                    let i = (ty * t.w + tx) as usize;
                    let at = y as usize * rw + x as usize;
                    match &mut deep {
                        Some(deep) => {
                            let px = channel.linear(&t.data, i, n);
                            deep[at] = px;
                            pixels[at] = to_color32(px);
                        }
                        None => pixels[at] = pixel(&t.data, i, n, depth),
                    }
                }
            }
        }
        Ok((rect, pixels, deep))
    }

    /// The first transparency mask under `layer`, as grey levels.
    fn mask(&self, layer: &Node) -> Option<PsdMask> {
        let masks = child(layer, "masks")?;
        let m = masks
            .children
            .iter()
            .find(|m| m.attr("nodetype") == Some("transparencymask"))?;
        let data = self.file(m.attr("filename")?)?;
        let tiles = read_tiles(data, 1).ok()?;
        let (w, h) = (self.size.0, self.size.1);
        // Unpainted mask areas show (masks start white).
        let default = self
            .file(&format!("{}.defaultpixel", m.attr("filename")?))
            .and_then(|d| d.first().copied())
            .unwrap_or(255);
        let mut grey = vec![default; w * h];
        for t in &tiles {
            for ty in 0..t.h {
                for tx in 0..t.w {
                    let (x, y) = (t.x + tx, t.y + ty);
                    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                        continue;
                    }
                    grey[y as usize * w + x as usize] = t.data[(ty * t.w + tx) as usize];
                }
            }
        }
        Some(PsdMask {
            rect: [0, 0, w as i32, h as i32],
            data: grey,
            default,
        })
    }
}

/// The most pixels a layer read at full depth may have (16 bytes each).
pub(crate) const MAX_DEEP_PIXELS: usize = 1 << 26;

/// A paint layer's rectangle, 8-bit pixels and, deeper than 8 bits, the
/// same at full depth (premultiplied linear light).
type LayerPixels = ([i32; 4], Vec<Color32>, Option<Vec<[f32; 4]>>);

/// How a Krita colour space stores a channel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    U8,
    /// 16-bit integers, sRGB encoded.
    U16,
    /// Half floats, linear light.
    F16,
    /// Floats, linear light.
    F32,
}

impl Channel {
    fn bytes(self) -> usize {
        match self {
            Channel::U8 => 1,
            Channel::U16 | Channel::F16 => 2,
            Channel::F32 => 4,
        }
    }

    /// The document depth this keeps.
    fn depth(self) -> Depth {
        match self {
            Channel::U8 => Depth::U8,
            Channel::U16 => Depth::U16,
            Channel::F16 | Channel::F32 => Depth::F32,
        }
    }

    /// Pixel `i` of `n` from planar B, G, R, A data (unmultiplied, little
    /// endian), premultiplied in linear light.
    fn linear(self, data: &[u8], i: usize, n: usize) -> [f32; 4] {
        let size = self.bytes();
        let channel = |c: usize| -> f32 {
            let at = (c * n + i) * size;
            let Some(b) = data.get(at..at + size) else {
                return 0.0;
            };
            let v = match self {
                Channel::U8 => b[0] as f32 / 255.0,
                Channel::U16 => u16::from_le_bytes([b[0], b[1]]) as f32 / 65535.0,
                Channel::F16 => f16_to_f32(u16::from_le_bytes([b[0], b[1]])),
                Channel::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            };
            if v.is_finite() { v } else { 0.0 }
        };
        let a = channel(3).clamp(0.0, 1.0);
        let rgb = [channel(2), channel(1), channel(0)];
        let lin = |v: f32| match self {
            Channel::U8 | Channel::U16 => {
                eframe::egui::ecolor::linear_from_gamma(v.clamp(0.0, 1.0))
            }
            Channel::F16 | Channel::F32 => v.max(0.0),
        };
        [lin(rgb[0]) * a, lin(rgb[1]) * a, lin(rgb[2]) * a, a]
    }
}

/// A half float's value.
fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::NAN,
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// A premultiplied linear-light pixel rounded to 8 bits.
fn to_color32(px: [f32; 4]) -> Color32 {
    let px = px.map(|v| v.max(0.0));
    Color32::from(eframe::egui::Rgba::from_rgba_premultiplied(
        px[0],
        px[1],
        px[2],
        px[3].min(1.0),
    ))
}

/// Pixel `i` of `n` from planar B, G, R, A data, each channel `depth`
/// bytes (16-bit little-endian: the high byte is kept).
fn pixel(data: &[u8], i: usize, n: usize, depth: usize) -> Color32 {
    let channel = |c: usize| {
        let at = (c * n + i) * depth + depth - 1;
        data.get(at).copied().unwrap_or(0)
    };
    Color32::from_rgba_unmultiplied(channel(2), channel(1), channel(0), channel(3))
}

struct Tile {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// Planar channels, `pixel_size` bytes a pixel in all.
    data: Vec<u8>,
}

/// A `.kra` layer file: a text header, then `x,y,LZF,size` and the data of
/// each tile.
fn read_tiles(bytes: &[u8], pixel_size: usize) -> Result<Vec<Tile>, String> {
    let bad = |what: &str| format!("damaged pixels ({what})");
    let mut at = 0;
    let (mut tw, mut th, mut size) = (64, 64, pixel_size);
    let count = loop {
        let l = next_line(bytes, &mut at).ok_or_else(|| bad("header"))?;
        let mut parts = l.split_whitespace();
        match (
            parts.next(),
            parts.next().and_then(|v| v.parse::<usize>().ok()),
        ) {
            (Some("VERSION"), Some(v)) if v != 2 => return Err(bad("version")),
            (Some("TILEWIDTH"), Some(v)) => tw = v,
            (Some("TILEHEIGHT"), Some(v)) => th = v,
            (Some("PIXELSIZE"), Some(v)) => size = v,
            (Some("DATA"), Some(v)) => break v,
            _ => {}
        }
    };
    if size != pixel_size || tw == 0 || th == 0 || tw > 1024 || th > 1024 {
        return Err(bad("tile size"));
    }
    let tile_bytes = tw * th * size;
    let mut tiles = Vec::with_capacity(count);
    for _ in 0..count {
        let l = next_line(bytes, &mut at).ok_or_else(|| bad("tile header"))?;
        let f: Vec<&str> = l.split(',').collect();
        if f.len() != 4 || f[2] != "LZF" {
            return Err(bad("tile header"));
        }
        let (x, y, len) = (
            f[0].parse::<i32>().map_err(|_| bad("tile x"))?,
            f[1].parse::<i32>().map_err(|_| bad("tile y"))?,
            f[3].parse::<usize>().map_err(|_| bad("tile size"))?,
        );
        let blob = bytes.get(at..at + len).ok_or_else(|| bad("tile data"))?;
        at += len;
        let data = match blob.first() {
            Some(1) => lzf_decompress(&blob[1..], tile_bytes).ok_or_else(|| bad("LZF"))?,
            Some(0) => blob[1..].to_vec(),
            _ => return Err(bad("tile data")),
        };
        if data.len() != tile_bytes {
            return Err(bad("tile length"));
        }
        tiles.push(Tile {
            x,
            y,
            w: tw as i32,
            h: th as i32,
            data,
        });
    }
    Ok(tiles)
}

/// The text line starting at `at`, moving `at` past it.
fn next_line<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a str> {
    let start = *at;
    let end = bytes.get(start..)?.iter().position(|&b| b == b'\n')? + start;
    *at = end + 1;
    std::str::from_utf8(&bytes[start..end]).ok()
}

/// LZF (as `.kra` files hold it): literal runs and back references.
pub(crate) fn lzf_decompress(src: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < src.len() {
        let c = src[i] as usize;
        i += 1;
        if c < 32 {
            let run = c + 1;
            out.extend_from_slice(src.get(i..i + run)?);
            i += run;
        } else {
            let mut len = c >> 5;
            if len == 7 {
                len += *src.get(i)? as usize;
                i += 1;
            }
            let back = ((c & 0x1f) << 8) + *src.get(i)? as usize + 1;
            i += 1;
            let from = out.len().checked_sub(back)?;
            for k in 0..len + 2 {
                let b = out[from + k];
                out.push(b);
            }
        }
        if out.len() > expected {
            return None;
        }
    }
    Some(out)
}

/// Composite op names in `.kra` and `.kpp` files (layers' and brushes').
pub(crate) fn blend(op: &str) -> LayerBlend {
    use LayerBlend::*;
    match op {
        "dissolve" => Dissolve,
        "darken" => Darken,
        "multiply" => Multiply,
        "burn" => ColorBurn,
        "linear_burn" => LinearBurn,
        "darker color" => DarkerColor,
        "lighten" => Lighten,
        "screen" => Screen,
        "dodge" => ColorDodge,
        "add" | "linear_dodge" => LinearDodge,
        "lighter color" => LighterColor,
        "overlay" => Overlay,
        "soft_light" | "soft_light_photoshop" | "soft_light_svg" => SoftLight,
        "hard_light" => HardLight,
        "vivid_light" => VividLight,
        "linear light" | "linear_light" => LinearLight,
        "pin_light" => PinLight,
        "hard mix" | "hard_mix" | "hard_mix_photoshop" => HardMix,
        "diff" => Difference,
        "exclusion" => Exclusion,
        "subtract" => Subtract,
        "divide" => Divide,
        "parallel" => Parallel,
        "hue" => Hue,
        "saturation" => Saturation,
        "color" => Color,
        "luminize" => Luminosity,
        _ => Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::blend::Unmultiply;

    #[test]
    fn lzf_undoes_runs_and_back_references() {
        // "abcabcabc": three literals, then a copy of 6 from 3 back.
        let src = [2, b'a', b'b', b'c', (4 << 5) as u8, 2];
        assert_eq!(lzf_decompress(&src, 9).unwrap(), b"abcabcabc");
        // A reference before the start is refused, not a panic.
        assert!(lzf_decompress(&[(1 << 5) as u8, 5], 9).is_none());
        assert!(lzf_decompress(&[5, 1], 9).is_none(), "a run past the end");
    }

    #[test]
    fn composite_ops_map_to_blend_modes() {
        assert_eq!(blend("multiply"), LayerBlend::Multiply);
        assert_eq!(blend("luminize"), LayerBlend::Luminosity);
        assert_eq!(blend("something new"), LayerBlend::Normal);
    }

    /// Sample `.kra` files (see `testdata/README.md`).
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn check_document(doc: PsdDocument) {
        assert_eq!((doc.width, doc.height), (200, 120));
        let names: Vec<(&str, &PsdKind)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), &l.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("Background", &PsdKind::Pixels),
                ("Red square", &PsdKind::Pixels),
                ("Folder", &PsdKind::GroupEnd),
                ("Blue multiply", &PsdKind::Pixels),
                ("Folder", &PsdKind::GroupStart),
                ("Hidden", &PsdKind::Pixels),
            ]
        );
        let canvas = doc.into_canvas().unwrap();
        let find = |n: &str| canvas.layers.iter().position(|l| l.name == n).unwrap();
        let px = |i: usize, x: i32, y: i32| {
            canvas
                .get_layer_tile_data(i, x / 64, y / 64)
                .map_or(Color32::TRANSPARENT, |t| {
                    t[((y % 64) * 64 + x % 64) as usize]
                })
        };
        let red = find("Red square");
        assert_eq!(px(red, 20, 20), Color32::RED);
        assert_eq!(px(red, 95, 20), Color32::TRANSPARENT);
        let blue = find("Blue multiply");
        assert_eq!(canvas.layers[blue].blend, LayerBlend::Multiply);
        let b = px(blue, 100, 60).unmultiplied();
        assert_eq!((b[2], b[3]), (255, 200), "{b:?}");
        let folder = canvas.layers.iter().find(|l| l.name == "Folder").unwrap();
        assert!((folder.opacity - 191.0 / 255.0).abs() < 1e-3);
        assert_eq!(canvas.layers[blue].parent, Some(folder.id));
        assert!(!canvas.layers[find("Hidden")].visible);
        // The white background became the canvas background.
        assert_eq!(px(0, 150, 100), Color32::WHITE);
    }

    #[test]
    fn a_krita_document_opens_layer_by_layer() {
        check_document(decode_kra(&fixture("krita-layers-8bit.kra")).unwrap());
    }

    #[test]
    fn the_app_opens_it_and_it_looks_as_it_did_in_krita() {
        let bytes = fixture("krita-layers-8bit.kra");
        let path = std::env::temp_dir().join(format!("rp-open-{}.KRA", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
            64,
            64,
            Color32::WHITE,
            64,
        ));
        app.load_project_from_path(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let entries: HashMap<String, Vec<u8>> = crate::project::zip::read_all(&bytes)
            .unwrap()
            .into_iter()
            .collect();
        let (w, h, krita) = merged_image(&entries).unwrap();
        let ours = app.canvas.flatten_final();
        assert_eq!(ours.size, [w, h]);
        // The file's own picture against ours: the same to within rounding.
        let worst = ours
            .pixels
            .iter()
            .zip(&krita)
            .map(|(a, b)| {
                (0..4)
                    .map(|c| (a.to_array()[c] as i32 - b.to_array()[c] as i32).abs())
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst <= 3, "differs from Krita by up to {worst}");
    }

    #[test]
    fn a_16_bit_krita_document_opens_too() {
        check_document(decode_kra(&fixture("krita-layers-16bit.kra")).unwrap());
    }

    #[test]
    fn a_16_bit_krita_document_opens_at_16_bits() {
        let doc = decode_kra(&fixture("krita-layers-16bit.kra")).unwrap();
        assert_eq!(doc.depth, Depth::U16);
        let canvas = doc.into_canvas().unwrap();
        assert_eq!(canvas.depth(), Depth::U16);
        let red = canvas
            .layers
            .iter()
            .position(|l| l.name == "Red square")
            .unwrap();
        let deep = canvas.get_layer_tile_deep(red, 0, 0).unwrap();
        assert_eq!(deep.linear(20 * 64 + 20), [1.0, 0.0, 0.0, 1.0]);
        // The 8-bit document stays 8-bit.
        let eight = decode_kra(&fixture("krita-layers-8bit.kra")).unwrap();
        assert_eq!(eight.depth, Depth::U8);
    }

    #[test]
    fn deep_channels_are_read_as_krita_stores_them() {
        // One pixel, planar B, G, R, A.
        let u16s = |v: [u16; 4]| v.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
        let f32s = |v: [f32; 4]| v.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
        // Half grey at half alpha, sRGB encoded: linear 0.214, premultiplied.
        let px = Channel::U16.linear(&u16s([32768, 32768, 32768, 32768]), 0, 1);
        assert!((px[0] - 0.214 * 0.5).abs() < 1e-3 && (px[3] - 0.5).abs() < 1e-4);
        // Floats are linear already, and keep values past white.
        let px = Channel::F32.linear(&f32s([0.25, 0.5, 2.0, 0.5]), 0, 1);
        assert_eq!(px, [1.0, 0.25, 0.125, 0.5]);
        // Halves: 1.0 = 0x3C00, 0.5 = 0x3800.
        let px = Channel::F16.linear(&u16s([0x3C00, 0x3800, 0x3800, 0x3C00]), 0, 1);
        assert_eq!(px, [0.5, 0.5, 1.0, 1.0]);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        // Nothing to read: transparent, not a panic.
        assert_eq!(Channel::F32.linear(&[1, 2], 0, 1), [0.0; 4]);
    }

    #[test]
    fn damaged_or_foreign_files_are_refused_not_panicked_on() {
        let good = fixture("krita-layers-8bit.kra");
        assert!(decode_kra(b"not a zip").is_err());
        for cut in [10, good.len() / 3, good.len() - 30] {
            let _ = decode_kra(&good[..cut]);
        }
        let mut flipped = good.clone();
        for i in (100..flipped.len()).step_by(997) {
            flipped[i] ^= 0x5a;
        }
        let _ = decode_kra(&flipped);
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_kra() {
        let open = |b: &[u8]| {
            if let Ok(doc) = decode_kra(b)
                && let Ok(canvas) = doc.into_canvas()
            {
                canvas.flatten();
            }
        };
        let seed = fixture("krita-layers-8bit.kra");
        crate::fuzz::fuzz("kra", &seed, std::time::Duration::from_secs(2), open);
        // Each file inside, damaged on its own (the archive stays sound).
        for depth in ["8bit", "16bit"] {
            let entries: HashMap<String, Vec<u8>> =
                crate::project::zip::read_all(&fixture(&format!("krita-layers-{depth}.kra")))
                    .unwrap()
                    .into_iter()
                    .collect();
            for name in entries.keys().filter(|n| !n.ends_with(".png")) {
                crate::fuzz::fuzz(
                    &format!("kra-{depth}-{}", name.replace('/', "_")),
                    &entries[name],
                    std::time::Duration::from_secs(2),
                    |b| {
                        let mut damaged = entries.clone();
                        damaged.insert(name.clone(), b.to_vec());
                        if let Ok(doc) = layered(&damaged)
                            && let Ok(canvas) = doc.into_canvas()
                        {
                            canvas.flatten();
                        }
                    },
                );
            }
        }
    }

    #[test]
    fn a_huge_canvas_claim_is_refused_before_anything_is_made_that_size() {
        let mut entries: HashMap<String, Vec<u8>> =
            crate::project::zip::read_all(&fixture("krita-layers-8bit.kra"))
                .unwrap()
                .into_iter()
                .collect();
        let xml = String::from_utf8(entries["maindoc.xml"].clone()).unwrap();
        let xml = xml
            .replacen(r#"width="200""#, r#"width="65536""#, 1)
            .replacen(r#"height="120""#, r#"height="65536""#, 1);
        entries.insert("maindoc.xml".into(), xml.into_bytes());
        let err = layered(&entries).err().expect("refused");
        assert!(err.contains("too large"), "{err}");
    }
}
