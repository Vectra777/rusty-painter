//! Procreate documents (`.procreate`): a zip of `Document.archive` (a
//! keyed archive: the canvas's size, tile size, orientation, background
//! and layer tree) and, for each layer, a folder named by its UUID of
//! `col~row.chunk` tiles: LZO1X-compressed premultiplied RGBA, the tiles
//! at the right and bottom cropped to the canvas.
//!
//! The pixels are kept as the iPad's GPU keeps them (rows bottom up, and
//! turned with the canvas), so they're turned upright here. Which way is
//! checked against the file's own thumbnail; the document's orientation
//! decides only when there's none to check with.

use super::psd::{PsdDocument, PsdKind, PsdLayer};
use crate::brush_engine::import::procreate::{Plist, procreate_blend, read_bplist};
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::storage::Depth;
use eframe::egui::Color32;
use std::collections::HashMap;

pub fn decode_procreate(bytes: &[u8]) -> Result<PsdDocument, String> {
    let entries: HashMap<String, Vec<u8>> = crate::project::zip::read_all(bytes)
        .map_err(|_| "Not a Procreate document (damaged archive)".to_string())?
        .into_iter()
        .collect();
    let archive = entries
        .get("Document.archive")
        .ok_or("Not a Procreate document (no Document.archive)")?;
    let a = Archive::read(archive)?;
    let doc = a.root;
    let (w, h) = a
        .text(doc, "size")
        .and_then(parse_size)
        .ok_or("The Procreate document has no size")?;
    crate::app::document::validate_canvas_size(w, h)?;
    let tile = a.num(doc, "tileSize").map_or(256, |t| t as usize);
    if !(16..=4096).contains(&tile) {
        return Err("The Procreate document's tiles are an odd size".into());
    }
    let stored = Stored {
        entries: &entries,
        w,
        h,
        tile,
    };
    // Upright: the way the thumbnail shows it, else the orientation's.
    let composite = a
        .get(doc, "composite")
        .and_then(|l| a.text(l, "UUID"))
        .map(|uuid| stored.layer(uuid));
    let thumbnail = entries
        .get("QuickLook/Thumbnail.png")
        .and_then(|png| image::load_from_memory(png).ok())
        .map(|i| i.to_rgba8());
    let turn = match (&composite, &thumbnail) {
        (Some(c), Some(t)) => Turn::matching(c, w, h, t),
        _ => Turn::for_orientation(
            a.num(doc, "orientation").unwrap_or(1.0) as i64,
            a.flag(doc, "flippedHorizontally"),
            a.flag(doc, "flippedVertically"),
        ),
    };
    let (dw, dh) = turn.size(w, h);

    let mut reader = LayerReader {
        a: &a,
        stored: &stored,
        turn,
        skipped: 0,
    };
    let mut layers = Vec::new();
    // The background, below everything.
    if !a.flag(doc, "backgroundHidden") {
        let colour = a
            .data(doc, "backgroundColor")
            .and_then(|d| {
                let f = |i: usize| -> Option<f32> {
                    Some(f32::from_le_bytes(
                        d.get(i * 4..i * 4 + 4)?.try_into().ok()?,
                    ))
                };
                let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                Some(Color32::from_rgb(c(f(0)?), c(f(1)?), c(f(2)?)))
            })
            .unwrap_or(Color32::WHITE);
        let mut bg = pixel_layer(
            "Background",
            [0, 0, dw as i32, dh as i32],
            vec![colour; dw * dh],
        );
        bg.position_locked = true;
        layers.push(bg);
    }
    if let Some(list) = a.get(doc, "layers") {
        reader.list(list, &mut layers, 0)?;
    }
    if reader.skipped > 0 {
        log::warn!(
            "{} Procreate layer mask(s) left out: their layers open without them",
            reader.skipped
        );
    }
    if layers.is_empty() {
        return Err("The Procreate document has no layers".into());
    }
    Ok(PsdDocument {
        width: dw,
        height: dh,
        layers,
        composite: composite.map(|c| turn.apply(&c, w, h)).unwrap_or_default(),
        depth: Depth::U8,
        composite_deep: None,
        profile: Default::default(),
    })
}

/// `"{w, h}"`.
fn parse_size(s: &str) -> Option<(usize, usize)> {
    let (w, h) = s
        .trim()
        .strip_prefix('{')?
        .strip_suffix('}')?
        .split_once(',')?;
    let n = |v: &str| {
        v.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| *v >= 1.0)
            .map(|v| v as usize)
    };
    Some((n(w)?, n(h)?))
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
        vector: None,
    }
}

/// A keyed archive's objects, `root` its root object.
struct Archive {
    objects: Vec<Plist>,
    /// `$objects`: what uids point at.
    archived: Vec<usize>,
    root: usize,
}

impl Archive {
    fn read(bytes: &[u8]) -> Result<Self, String> {
        let not = || "The Procreate document is damaged".to_string();
        let objects = read_bplist(bytes).map_err(|_| not())?;
        let Some(Plist::Dict(top)) = objects.first() else {
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
        let Some(&Plist::Uid(root)) = entry(t, "root").map(|i| &objects[i]) else {
            return Err(not());
        };
        let archived = archived.clone();
        let root = *archived.get(root).ok_or_else(not)?;
        Ok(Self {
            objects,
            archived,
            root,
        })
    }

    /// Object `i`, following a uid to what it points at.
    fn resolve(&self, i: usize) -> usize {
        match self.objects.get(i) {
            Some(&Plist::Uid(u)) => self.archived.get(u).copied().unwrap_or(i),
            _ => i,
        }
    }

    /// Field `key` of dictionary `dict`.
    fn get(&self, dict: usize, key: &str) -> Option<usize> {
        let Some(Plist::Dict(fields)) = self.objects.get(dict) else {
            return None;
        };
        fields
            .iter()
            .find(|(k, _)| matches!(self.objects.get(*k), Some(Plist::Str(s)) if s == key))
            .map(|&(_, v)| self.resolve(v))
            .filter(|&v| !matches!(self.objects.get(v), Some(Plist::Str(s)) if s == "$null"))
    }

    fn num(&self, dict: usize, key: &str) -> Option<f64> {
        match self.objects.get(self.get(dict, key)?)? {
            Plist::Real(v) if v.is_finite() => Some(*v),
            Plist::Int(v) => Some(*v as f64),
            Plist::Bool(v) => Some(f64::from(u8::from(*v))),
            _ => None,
        }
    }

    fn flag(&self, dict: usize, key: &str) -> bool {
        self.num(dict, key).is_some_and(|v| v != 0.0)
    }

    fn text(&self, dict: usize, key: &str) -> Option<&str> {
        match self.objects.get(self.get(dict, key)?)? {
            Plist::Str(s) => Some(s),
            _ => None,
        }
    }

    fn data(&self, dict: usize, key: &str) -> Option<&[u8]> {
        match self.objects.get(self.get(dict, key)?)? {
            Plist::Data(d) => Some(d),
            _ => None,
        }
    }

    /// An `NSArray`'s items.
    fn items(&self, array: usize) -> Vec<usize> {
        let list = self.get(array, "NS.objects").unwrap_or(array);
        match self.objects.get(list) {
            Some(Plist::Array(items)) => items.iter().map(|&i| self.resolve(i)).collect(),
            _ => Vec::new(),
        }
    }

    /// The class an archived object was.
    fn class(&self, dict: usize) -> Option<&str> {
        self.text(self.get(dict, "$class")?, "$classname")
    }
}

/// The layers' tiles as the file keeps them.
struct Stored<'a> {
    entries: &'a HashMap<String, Vec<u8>>,
    w: usize,
    h: usize,
    tile: usize,
}

impl Stored<'_> {
    /// Layer `uuid`'s pixels, `w` × `h` as stored (premultiplied).
    fn layer(&self, uuid: &str) -> Vec<Color32> {
        let (w, h, t) = (self.w, self.h, self.tile);
        let mut out = vec![Color32::TRANSPARENT; w * h];
        let prefix = format!("{uuid}/");
        for (name, chunk) in self.entries {
            let Some((col, row)) = name
                .strip_prefix(&prefix)
                .and_then(|n| n.strip_suffix(".chunk"))
                .and_then(|n| n.split_once('~'))
                .and_then(|(c, r)| Some((c.parse::<usize>().ok()?, r.parse::<usize>().ok()?)))
            else {
                continue;
            };
            let (x0, y0) = (col.saturating_mul(t), row.saturating_mul(t));
            if x0 >= w || y0 >= h {
                continue;
            }
            let (tw, th) = (t.min(w - x0), t.min(h - y0));
            let Some(px) = lzo1x_decompress(chunk, tw * th * 4) else {
                log::warn!("Procreate tile {name} is damaged: left empty");
                continue;
            };
            for (y, row) in px.chunks_exact(tw * 4).enumerate() {
                let line = &mut out[(y0 + y) * w + x0..][..tw];
                for (o, p) in line.iter_mut().zip(row.as_chunks::<4>().0) {
                    *o = Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]);
                }
            }
        }
        out
    }
}

/// How the stored pixels turn upright: shown pixel (x, y) is stored pixel
/// (`transpose` ? (y, x) : (x, y)), each mirrored where flipped.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Turn {
    transpose: bool,
    flip_x: bool,
    flip_y: bool,
}

impl Turn {
    /// Every way: each of the three switches on or off.
    fn all() -> impl Iterator<Item = Turn> {
        (0..8u8).map(|bits| Turn {
            transpose: bits & 4 != 0,
            flip_x: bits & 2 != 0,
            flip_y: bits & 1 != 0,
        })
    }

    /// From the document's orientation (the stored rows are bottom up, then
    /// turned with the canvas) and its flips. Only orientation 3 is checked
    /// against a real file; the rest follow the same pattern.
    fn for_orientation(orientation: i64, flipped_x: bool, flipped_y: bool) -> Self {
        let mut t = match orientation {
            2 => Turn {
                transpose: false,
                flip_x: true,
                flip_y: false,
            },
            3 => Turn {
                transpose: true,
                flip_x: true,
                flip_y: true,
            },
            4 => Turn {
                transpose: true,
                flip_x: false,
                flip_y: false,
            },
            _ => Turn {
                transpose: false,
                flip_x: false,
                flip_y: true,
            },
        };
        // Mirroring the shown picture mirrors the stored axis it comes from.
        let (sx, sy) = if t.transpose {
            (flipped_y, flipped_x)
        } else {
            (flipped_x, flipped_y)
        };
        t.flip_x ^= sx;
        t.flip_y ^= sy;
        t
    }

    /// The way that shows `stored` (`w` × `h`) most like `thumbnail`.
    fn matching(stored: &[Color32], w: usize, h: usize, thumbnail: &image::RgbaImage) -> Self {
        let (tw, th) = (thumbnail.width() as usize, thumbnail.height() as usize);
        let error = |turn: &Turn| {
            let (dw, dh) = turn.size(w, h);
            let mut sum = 0.0f64;
            // A grid of points is plenty to tell 8 ways apart.
            let n = 48;
            for gy in 0..n {
                for gx in 0..n {
                    let (fx, fy) = ((gx as f64 + 0.5) / n as f64, (gy as f64 + 0.5) / n as f64);
                    let (x, y) = ((fx * dw as f64) as usize, (fy * dh as f64) as usize);
                    let (sx, sy) = turn.stored_at(x, y, w, h);
                    let p = stored[sy * w + sx];
                    // Premultiplied, over white, as the thumbnail shows it.
                    let over = |c: u8| f64::from(c) + 255.0 - f64::from(p.a());
                    let q = thumbnail.get_pixel(
                        ((fx * tw as f64) as u32).min(tw as u32 - 1),
                        ((fy * th as f64) as u32).min(th as u32 - 1),
                    );
                    let qa = f64::from(q[3]) / 255.0;
                    let thumb = |c: u8| f64::from(c) * qa + 255.0 * (1.0 - qa);
                    sum += (over(p.r()) - thumb(q[0])).powi(2)
                        + (over(p.g()) - thumb(q[1])).powi(2)
                        + (over(p.b()) - thumb(q[2])).powi(2);
                }
            }
            // A thumbnail the wrong shape for this way counts against it.
            let aspect = (dw as f64 / dh as f64 - tw as f64 / th as f64).abs();
            sum * (1.0 + aspect * 10.0)
        };
        Self::all()
            .min_by(|a, b| error(a).total_cmp(&error(b)))
            .unwrap_or(Self::for_orientation(1, false, false))
    }

    /// The shown size of a `w` × `h` stored picture.
    fn size(self, w: usize, h: usize) -> (usize, usize) {
        if self.transpose { (h, w) } else { (w, h) }
    }

    /// The stored pixel shown at (`x`, `y`).
    fn stored_at(self, x: usize, y: usize, w: usize, h: usize) -> (usize, usize) {
        let (u, v) = if self.transpose { (y, x) } else { (x, y) };
        (
            if self.flip_x { w - 1 - u } else { u },
            if self.flip_y { h - 1 - v } else { v },
        )
    }

    /// `stored` (`w` × `h`) turned upright.
    fn apply(self, stored: &[Color32], w: usize, h: usize) -> Vec<Color32> {
        let (dw, dh) = self.size(w, h);
        let mut out = Vec::with_capacity(dw * dh);
        for y in 0..dh {
            for x in 0..dw {
                let (sx, sy) = self.stored_at(x, y, w, h);
                out.push(stored[sy * w + sx]);
            }
        }
        out
    }
}

struct LayerReader<'a> {
    a: &'a Archive,
    stored: &'a Stored<'a>,
    turn: Turn,
    /// Layer masks left out.
    skipped: usize,
}

impl LayerReader<'_> {
    /// A layer list (top first in the file) into `out`, bottom first,
    /// folders as their end marker, contents, then their own record.
    fn list(&mut self, list: usize, out: &mut Vec<PsdLayer>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("Folders nested too deep".into());
        }
        let a = self.a;
        for layer in a.items(list).into_iter().rev() {
            let common = |kind: PsdKind| {
                let mut l =
                    pixel_layer(a.text(layer, "name").unwrap_or("Layer"), [0; 4], Vec::new());
                l.kind = kind;
                l.opacity = a
                    .num(layer, "opacity")
                    .map_or(1.0, |v| v.clamp(0.0, 1.0) as f32);
                l.visible = !a.flag(layer, "hidden");
                l.blend = procreate_blend(a.num(layer, "blend").unwrap_or(0.0) as f32)
                    .unwrap_or(LayerBlend::Normal);
                l.clipped = a.flag(layer, "clipped");
                l.position_locked = a.flag(layer, "locked");
                l
            };
            if a.class(layer) == Some("SilicaGroup") {
                out.push(common(PsdKind::GroupEnd));
                if let Some(children) = a.get(layer, "children") {
                    self.list(children, out, depth + 1)?;
                }
                out.push(common(PsdKind::GroupStart));
                continue;
            }
            let mut l = common(PsdKind::Pixels);
            l.alpha_locked = a.flag(layer, "preserve");
            if a.get(layer, "mask").is_some() {
                self.skipped += 1;
            }
            let Some(uuid) = a.text(layer, "UUID") else {
                continue;
            };
            let (w, h) = (self.stored.w, self.stored.h);
            let pixels = self.turn.apply(&self.stored.layer(uuid), w, h);
            let (dw, dh) = self.turn.size(w, h);
            (l.rect, l.pixels) = cropped(pixels, dw, dh);
            out.push(l);
        }
        Ok(())
    }
}

/// `pixels` (`w` × `h`) cut to what isn't transparent, with its rect.
fn cropped(pixels: Vec<Color32>, w: usize, h: usize) -> ([i32; 4], Vec<Color32>) {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (y, row) in pixels.chunks_exact(w).enumerate() {
        if let (Some(a), Some(b)) = (
            row.iter().position(|p| p.a() > 0),
            row.iter().rposition(|p| p.a() > 0),
        ) {
            (x0, x1, y0, y1) = (x0.min(a), x1.max(b + 1), y0.min(y), y + 1);
        }
    }
    if x0 >= x1 {
        return ([0; 4], Vec::new());
    }
    if (x0, y0, x1, y1) == (0, 0, w, h) {
        return ([0, 0, w as i32, h as i32], pixels);
    }
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        out.extend_from_slice(&pixels[y * w + x0..y * w + x1]);
    }
    ([x0 as i32, y0 as i32, x1 as i32, y1 as i32], out)
}

/// LZO1X decompression (Procreate's tiles) into exactly `expected` bytes;
/// `None` for a damaged stream.
pub(crate) fn lzo1x_decompress(src: &[u8], expected: usize) -> Option<Vec<u8>> {
    struct In<'a> {
        src: &'a [u8],
        at: usize,
    }
    impl In<'_> {
        fn byte(&mut self) -> Option<usize> {
            let b = *self.src.get(self.at)?;
            self.at += 1;
            Some(b as usize)
        }
        fn take(&mut self, n: usize) -> Option<&[u8]> {
            let s = self.src.get(self.at..self.at.checked_add(n)?)?;
            self.at += n;
            Some(s)
        }
        /// A run length: `t`, or when that's 0, `base` plus what follows
        /// (each zero byte adding 255).
        fn length(&mut self, t: usize, base: usize) -> Option<usize> {
            if t != 0 {
                return Some(t);
            }
            let mut n = 0usize;
            loop {
                match self.byte()? {
                    0 => n = n.checked_add(255)?,
                    b => return n.checked_add(base + b),
                }
            }
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let mut i = In { src, at: 0 };
    let literal = |i: &mut In<'_>, out: &mut Vec<u8>, n: usize| -> Option<()> {
        if out.len() + n > expected {
            return None;
        }
        out.extend_from_slice(i.take(n)?);
        Some(())
    };
    let copy = |out: &mut Vec<u8>, distance: usize, n: usize| -> Option<()> {
        if distance == 0 || distance > out.len() || out.len() + n > expected {
            return None;
        }
        let from = out.len() - distance;
        // (May overlap what it writes: byte by byte.)
        for k in 0..n {
            out.push(out[from + k]);
        }
        Some(())
    };
    // How many literals the last instruction copied (4: a long run).
    let mut state = 0;
    if *src.first()? > 17 {
        let n = i.byte()? - 17;
        literal(&mut i, &mut out, n)?;
        state = n.min(4);
    }
    loop {
        let t = i.byte()?;
        let (len, distance, next) = if t >= 64 {
            let d = ((t >> 2) & 7) + (i.byte()? << 3) + 1;
            ((t >> 5) + 1, d, t & 3)
        } else if t >= 32 {
            let len = i.length(t & 31, 31)? + 2;
            let d = i.byte()? | (i.byte()? << 8);
            (len, (d >> 2) + 1, d & 3)
        } else if t >= 16 {
            let len = i.length(t & 7, 7)? + 2;
            let d = i.byte()? | (i.byte()? << 8);
            let far = ((t & 8) << 11) + (d >> 2);
            if far == 0 {
                break;
            }
            (len, far + 16384, d & 3)
        } else if state == 0 {
            let n = i.length(t, 15)? + 3;
            literal(&mut i, &mut out, n)?;
            state = 4;
            continue;
        } else if state < 4 {
            (2, (t >> 2) + (i.byte()? << 2) + 1, t & 3)
        } else {
            (3, (t >> 2) + (i.byte()? << 2) + 2049, t & 3)
        };
        copy(&mut out, distance, len)?;
        literal(&mut i, &mut out, next)?;
        state = next;
    }
    (out.len() == expected).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzo_literals_matches_and_the_end() {
        // 4 literals "abcd" (first byte 17 + 4), then a match of 3 at
        // distance 4 (a short match after a long run: 3 bytes from 2049
        // back is too far here, so a 64+ instruction: len 3, dist 4),
        // then the end marker.
        let len3_dist4 = ((3 - 1) << 5) | ((3 & 7) << 2);
        let stream = [21, b'a', b'b', b'c', b'd', len3_dist4, 0, 17, 0, 0];
        assert_eq!(
            lzo1x_decompress(&stream, 7).as_deref(),
            Some(&b"abcdabc"[..])
        );
        // The wrong size, or a cut stream: refused.
        assert!(lzo1x_decompress(&stream, 8).is_none());
        for cut in 0..stream.len() - 1 {
            assert!(lzo1x_decompress(&stream[..cut], 7).is_none());
        }
        // Junk never panics.
        let mut x = 0x1234_5678u32;
        for _ in 0..2000 {
            let junk: Vec<u8> = (0..64)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let _ = lzo1x_decompress(&junk, 256);
        }
    }

    #[test]
    fn the_turns_cover_every_way_and_orientation_3_is_the_checked_one() {
        // A 3 × 2 picture: each turn shows it a different way.
        let px: Vec<Color32> = (0..6).map(|i| Color32::from_gray(i * 40)).collect();
        let mut seen: Vec<Vec<Color32>> = Turn::all().map(|t| t.apply(&px, 3, 2)).collect();
        seen.sort_by_key(|v| v.iter().map(|c| c.r()).collect::<Vec<_>>());
        seen.dedup();
        assert_eq!(seen.len(), 8);
        assert_eq!(
            Turn::for_orientation(3, false, false),
            Turn {
                transpose: true,
                flip_x: true,
                flip_y: true
            }
        );
        assert_eq!(parse_size("{2550, 1200}"), Some((2550, 1200)));
        assert_eq!(parse_size("{0, 5}"), None);
    }

    fn fixture() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/procreate-tiny.procreate");
        std::fs::read(path).unwrap()
    }

    #[test]
    fn a_document_opens_with_its_layers_folder_and_background_upright() {
        let doc = decode_procreate(&fixture()).unwrap();
        assert_eq!((doc.width, doc.height), (300, 60));
        let names: Vec<(&str, &PsdKind)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), &l.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("Background", &PsdKind::Pixels),
                ("Folder", &PsdKind::GroupEnd),
                ("Inside", &PsdKind::Pixels),
                ("Folder", &PsdKind::GroupStart),
                ("Top", &PsdKind::Pixels),
            ]
        );
        assert_eq!(doc.layers[2].blend, LayerBlend::Multiply);
        // Where they were drawn, the right way up (one across the edge tile).
        let top = &doc.layers[4];
        assert_eq!(top.rect, [20, 10, 120, 30]);
        assert_eq!(top.pixels[0], Color32::from_rgb(255, 0, 0));
        assert_eq!(doc.layers[2].rect, [150, 35, 280, 55]);
        assert_eq!(
            doc.layers[2].pixels[0],
            Color32::from_rgba_premultiplied(0, 0, 128, 128)
        );
    }

    #[test]
    fn damaged_documents_are_refused_not_panicked_on() {
        let good = fixture();
        for cut in (0..good.len()).step_by(7) {
            let _ = decode_procreate(&good[..cut]);
        }
        let mut bad = good.clone();
        for i in (0..bad.len()).step_by(3) {
            bad[i] ^= 0x5a;
            let _ = decode_procreate(&bad);
            bad[i] ^= 0x5a;
        }
        assert!(decode_procreate(b"not a zip").is_err());
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_procreate() {
        let open = |b: &[u8]| {
            if let Ok(doc) = decode_procreate(b)
                && let Ok(canvas) = doc.into_canvas()
            {
                canvas.flatten();
            }
        };
        crate::fuzz::fuzz(
            "procreate",
            &fixture(),
            std::time::Duration::from_secs(2),
            open,
        );
        // Each file inside, damaged on its own (the archive stays sound).
        let entries = crate::project::zip::read_all(&fixture()).unwrap();
        for (name, data) in &entries {
            crate::fuzz::fuzz(
                &format!("procreate-{}", name.replace('/', "_")),
                data,
                std::time::Duration::from_secs(1),
                |b: &[u8]| {
                    let mut e = entries.clone();
                    e.iter_mut().find(|(n, _)| n == name).unwrap().1 = b.to_vec();
                    let mut zip = crate::project::zip::ZipWriter::default();
                    for (n, d) in &e {
                        zip.add(n, d).unwrap();
                    }
                    let _ = decode_procreate(&zip.finish().unwrap());
                },
            );
        }
    }

    /// A real file, when given (`PROCREATE_SAMPLE`: the free "Watercolor
    /// Test Canvas"; not shipped, it's someone else's): its layers and the
    /// way up.
    #[test]
    fn a_real_document_opens_upright() {
        let Some(bytes) = std::env::var_os("PROCREATE_SAMPLE").and_then(|p| std::fs::read(p).ok())
        else {
            return;
        };
        let doc = decode_procreate(&bytes).unwrap();
        assert_eq!((doc.width, doc.height), (2550, 2550));
        let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Background",
                "Painting",
                "Info",
                "Watercolor Sheet",
                "Paper Texture"
            ]
        );
        let sheet = &doc.layers[3];
        assert_eq!(sheet.blend, LayerBlend::Overlay);
        assert_eq!(doc.layers[4].blend, LayerBlend::Multiply);
        // The title banner is along the top, as the thumbnail shows.
        let top: u32 = (0..doc.width)
            .map(|x| 255 - u32::from(doc.composite[200 * doc.width + x].g()))
            .sum();
        let bottom: u32 = (0..doc.width)
            .map(|x| 255 - u32::from(doc.composite[2300 * doc.width + x].g()))
            .sum();
        assert!(top > bottom * 3, "banner on top: {top} vs {bottom}");
    }
}
