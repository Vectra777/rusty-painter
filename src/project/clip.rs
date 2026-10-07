//! Opening Clip Studio Paint documents (`.clip`): a `CSFCHUNK` container
//! holding an SQLite database (the canvas, the layer tree and where each
//! layer's pixels are) and "external" chunks with the pixels themselves, as
//! zlib-compressed 256×256 tiles.
//!
//! Raster layers (and the pixels Clip Studio keeps for vector and text
//! layers), folders, opacity, visibility, blend modes, clipping, alpha
//! lock, layer masks and the paper colour come across. Filter layers and
//! anything without pixels are left out; when something is, Clip Studio's
//! preview picture is added as a hidden top layer so the look isn't lost.
//! A document that can't be read layer by layer opens as that preview.

use crate::brush_engine::import::sut::{Cell, rows};
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::storage::Depth;
use crate::project::psd::{PsdDocument, PsdKind, PsdLayer, PsdMask};
use eframe::egui::Color32;
use std::collections::HashMap;

type Row = HashMap<String, Cell>;

/// Tiles are square, this many pixels a side.
const TILE: usize = 256;

/// A `.clip` file as a layered document.
pub fn decode_clip(bytes: &[u8]) -> Result<PsdDocument, String> {
    let chunks = Chunks::read(bytes)?;
    let db = chunks
        .sqlite
        .ok_or("The Clip Studio document has no database")?;
    let path = std::env::temp_dir().join(format!(
        "rusty-painter-open-{}-{}.clip.sqlite",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::write(&path, db).map_err(|e| e.to_string())?;
    let result =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("Damaged Clip Studio document ({e})"))
            .and_then(|db| {
                layered(&db, &chunks.external).or_else(|err| {
                    log::warn!("Opening the Clip Studio document as its preview: {err}");
                    flattened(&db).map_err(|_| err)
                })
            });
    let _ = std::fs::remove_file(&path);
    result
}

/// The container's parts: the database and the external chunks by id.
struct Chunks<'a> {
    sqlite: Option<&'a [u8]>,
    external: HashMap<&'a [u8], &'a [u8]>,
}

fn be64(bytes: &[u8], at: usize) -> Option<usize> {
    let b: [u8; 8] = bytes.get(at..at + 8)?.try_into().ok()?;
    usize::try_from(u64::from_be_bytes(b)).ok()
}

fn be32(bytes: &[u8], at: usize) -> Option<usize> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_be_bytes(b) as usize)
}

impl<'a> Chunks<'a> {
    /// `CSFCHUNK`, the file size and the first chunk's offset, then
    /// `CHNK` + a four-letter name + a 64-bit size + the data, repeatedly.
    fn read(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.get(..8) != Some(b"CSFCHUNK") {
            return Err("Not a Clip Studio document".into());
        }
        let bad = || "Damaged Clip Studio document (chunks)".to_string();
        let mut at = be64(bytes, 16).filter(|&a| a >= 24).unwrap_or(24);
        let mut chunks = Chunks {
            sqlite: None,
            external: HashMap::new(),
        };
        while at + 16 <= bytes.len() {
            if &bytes[at..at + 4] != b"CHNK" {
                return Err(bad());
            }
            let name = &bytes[at + 4..at + 8];
            let size = be64(bytes, at + 8).ok_or_else(bad)?;
            let data = bytes.get(at + 16..at + 16 + size).ok_or_else(bad)?;
            match name {
                b"SQLi" => chunks.sqlite = Some(data),
                // An id (its length first), then the body (its length first).
                b"Exta" => {
                    let id_len = be64(data, 0).ok_or_else(bad)?;
                    let id = data.get(8..8 + id_len).ok_or_else(bad)?;
                    let body_len = be64(data, 8 + id_len).ok_or_else(bad)?;
                    let body = data
                        .get(16 + id_len..16 + id_len + body_len)
                        .ok_or_else(bad)?;
                    chunks.external.insert(id, body);
                }
                b"Foot" => break,
                _ => {}
            }
            at += 16 + size;
        }
        Ok(chunks)
    }
}

/// A cell by column name (any case).
fn cell<'r>(row: &'r Row, key: &str) -> Option<&'r Cell> {
    row.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn int(row: &Row, key: &str) -> Option<i64> {
    match cell(row, key)? {
        Cell::Int(v) => Some(*v),
        Cell::Real(v) => Some(*v as i64),
        _ => None,
    }
}

fn text(row: &Row, key: &str) -> Option<String> {
    match cell(row, key)? {
        Cell::Text(t) => Some(t.clone()),
        Cell::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

fn blob<'r>(row: &'r Row, key: &str) -> Option<&'r [u8]> {
    match cell(row, key)? {
        Cell::Blob(b) => Some(b),
        Cell::Text(t) => Some(t.as_bytes()),
        _ => None,
    }
}

fn by_id(rows: Vec<Row>) -> HashMap<i64, Row> {
    rows.into_iter()
        .filter_map(|r| Some((int(&r, "MainId")?, r)))
        .collect()
}

/// The preview Clip Studio saves (a PNG, often smaller than the canvas).
fn preview(db: &rusqlite::Connection) -> Result<image::RgbaImage, String> {
    let rows = rows(db, "CanvasPreview").map_err(|e| e.to_string())?;
    let png = rows
        .iter()
        .find_map(|r| blob(r, "ImageData"))
        .ok_or("The Clip Studio document has no preview")?;
    Ok(image::load_from_memory(png)
        .map_err(|e| e.to_string())?
        .to_rgba8())
}

fn canvas_size(db: &rusqlite::Connection) -> Option<(usize, usize, Option<i64>)> {
    let canvas = rows(db, "Canvas").ok()?.into_iter().next()?;
    let w = int(&canvas, "CanvasWidth")?;
    let h = int(&canvas, "CanvasHeight")?;
    let (w, h) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    // Before any buffer the size of the canvas is made.
    crate::app::document::validate_canvas_size(w, h).ok()?;
    Some((w, h, int(&canvas, "CanvasRootFolder")))
}

/// The preview, stretched to the canvas, as one full-canvas layer.
fn preview_layer(db: &rusqlite::Connection, w: usize, h: usize, name: &str) -> Option<PsdLayer> {
    let mut img = preview(db).ok()?;
    if (img.width() as usize, img.height() as usize) != (w, h) {
        img = image::imageops::resize(
            &img,
            w as u32,
            h as u32,
            image::imageops::FilterType::Triangle,
        );
    }
    let pixels = img
        .pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    Some(pixel_layer(name, [0, 0, w as i32, h as i32], pixels))
}

fn flattened(db: &rusqlite::Connection) -> Result<PsdDocument, String> {
    let (w, h) = match canvas_size(db) {
        Some((w, h, _)) => (w, h),
        None => {
            let img = preview(db)?;
            (img.width() as usize, img.height() as usize)
        }
    };
    let layer = preview_layer(db, w, h, "Clip Studio picture")
        .ok_or("The Clip Studio document has no preview")?;
    Ok(PsdDocument {
        depth: Depth::U8,
        composite_deep: None,
        width: w,
        height: h,
        composite: layer.pixels.clone(),
        layers: vec![layer],
    })
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
    layers: HashMap<i64, Row>,
    mipmaps: HashMap<i64, Row>,
    mipmap_infos: HashMap<i64, Row>,
    offscreens: HashMap<i64, Row>,
    external: &'a HashMap<&'a [u8], &'a [u8]>,
    size: (usize, usize),
    /// Something was left out.
    skipped: bool,
}

fn layered(
    db: &rusqlite::Connection,
    external: &HashMap<&[u8], &[u8]>,
) -> Result<PsdDocument, String> {
    let (w, h, root) = canvas_size(db).ok_or("The Clip Studio document has no canvas size")?;
    let root = root.ok_or("The Clip Studio document has no layers")?;
    let table = |name: &str| rows(db, name).map(by_id).map_err(|e| e.to_string());
    let mut reader = Reader {
        layers: table("Layer")?,
        mipmaps: table("Mipmap")?,
        mipmap_infos: table("MipmapInfo")?,
        offscreens: table("Offscreen")?,
        external,
        size: (w, h),
        skipped: false,
    };
    let mut layers = Vec::new();
    reader.folder(root, &mut layers, 0)?;
    if reader.skipped
        && let Some(mut flat) =
            preview_layer(db, w, h, "Clip Studio's picture (parts not brought across)")
    {
        flat.visible = false;
        layers.push(flat);
    }
    if layers.is_empty() {
        return Err("The Clip Studio document has no layers this can open".into());
    }
    Ok(PsdDocument {
        depth: Depth::U8,
        composite_deep: None,
        width: w,
        height: h,
        layers,
        composite: Vec::new(),
    })
}

/// A decoded raster: `[left, top, right, bottom)` and its pixels, or the
/// grey levels of a mask with the level outside it.
enum Raster {
    Colour([i32; 4], Vec<Color32>),
    Grey([i32; 4], Vec<u8>, u8),
}

impl Reader<'_> {
    /// A folder's children (bottom first, as Clip Studio links them) into
    /// `out`: sub-folders as their end marker, contents, then their record.
    fn folder(&mut self, id: i64, out: &mut Vec<PsdLayer>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("Folders nested too deep".into());
        }
        let mut next = self
            .layers
            .get(&id)
            .and_then(|l| int(l, "LayerFirstChildIndex"));
        let mut seen = 0;
        while let Some(id) = next.filter(|&i| i != 0) {
            seen += 1;
            if seen > 100_000 {
                return Err("The layer list loops".into());
            }
            let Some(layer) = self.layers.get(&id) else {
                break;
            };
            next = int(layer, "LayerNextIndex");
            let common = |kind: PsdKind| {
                let name = text(layer, "LayerName").unwrap_or_else(|| "Layer".into());
                let mut l = pixel_layer(&name, [0; 4], Vec::new());
                l.kind = kind;
                l.opacity =
                    int(layer, "LayerOpacity").map_or(1.0, |o| (o as f32 / 256.0).clamp(0.0, 1.0));
                // Draft layers aren't part of the picture.
                l.visible = int(layer, "LayerVisibility").is_none_or(|v| v & 1 != 0)
                    && int(layer, "OutputAttribute").is_none_or(|v| v == 0);
                l.blend = blend(int(layer, "LayerComposite").unwrap_or(0));
                l.clipped = int(layer, "LayerClip").is_some_and(|c| c != 0);
                l.alpha_locked = int(layer, "LayerLock").is_some_and(|v| v & 16 != 0);
                l
            };
            if int(layer, "LayerFolder").is_some_and(|f| f != 0) {
                let (end, start) = (common(PsdKind::GroupEnd), common(PsdKind::GroupStart));
                out.push(end);
                self.folder(id, out, depth + 1)?;
                out.push(start);
                continue;
            }
            let mut l = common(PsdKind::Pixels);
            let layer = self.layers.get(&id).expect("looked up above");
            let offset = |a: &str, b: &str| {
                int(layer, a).unwrap_or(0) as i32 + int(layer, b).unwrap_or(0) as i32
            };
            let at = (
                offset("LayerOffsetX", "LayerRenderOffscrOffsetX"),
                offset("LayerOffsetY", "LayerRenderOffscrOffsetY"),
            );
            let filter = cell(layer, "FilterLayerInfo").is_some_and(|c| !matches!(c, Cell::Null));
            let paper = int(layer, "DrawColorEnable").is_some_and(|v| v != 0);
            let pixels = if filter {
                Err("filter layer".to_string())
            } else if paper {
                let c = |k: &str| (int(layer, k).unwrap_or(0) as u64 * 255 / u32::MAX as u64) as u8;
                let colour = Color32::from_rgb(
                    c("DrawColorMainRed"),
                    c("DrawColorMainGreen"),
                    c("DrawColorMainBlue"),
                );
                Ok(self.canvas_fill(colour))
            } else {
                match int(layer, "LayerRenderMipmap") {
                    Some(m) => self.raster(m, at, false),
                    None => Err("no pixels".into()),
                }
            };
            match pixels {
                Ok(Raster::Colour(rect, pixels)) => {
                    l.rect = rect;
                    l.pixels = pixels;
                }
                Ok(Raster::Grey(..)) => unreachable!("colour asked for"),
                Err(err) => {
                    log::warn!("Clip Studio layer {} left out: {err}", l.name);
                    self.skipped = true;
                    continue;
                }
            }
            let layer = self.layers.get(&id).expect("looked up above");
            let mask_on = int(layer, "LayerVisibility").is_none_or(|v| v & 2 != 0);
            if let Some(m) = int(layer, "LayerLayerMaskMipmap").filter(|_| mask_on) {
                let at = (
                    offset("LayerOffsetX", "LayerMaskOffsetX")
                        + int(layer, "LayerMaskOffscrOffsetX").unwrap_or(0) as i32,
                    offset("LayerOffsetY", "LayerMaskOffsetY")
                        + int(layer, "LayerMaskOffscrOffsetY").unwrap_or(0) as i32,
                );
                match self.raster(m, at, true) {
                    Ok(Raster::Grey(rect, data, default)) => {
                        l.mask = Some(PsdMask {
                            rect,
                            data,
                            default,
                        });
                    }
                    Ok(Raster::Colour(..)) => {}
                    Err(err) => log::warn!("Clip Studio mask on {} left out: {err}", l.name),
                }
            }
            out.push(l);
        }
        Ok(())
    }

    /// The paper: one colour over the canvas.
    fn canvas_fill(&self, colour: Color32) -> Raster {
        let (w, h) = self.size;
        Raster::Colour([0, 0, w as i32, h as i32], vec![colour; w * h])
    }

    /// The pixels of a mipmap chain's full-size level, placed at `at`.
    fn raster(&self, mipmap: i64, at: (i32, i32), grey: bool) -> Result<Raster, String> {
        let info = self
            .mipmaps
            .get(&mipmap)
            .and_then(|m| int(m, "BaseMipmapInfo"))
            .ok_or("missing mipmap")?;
        let offscreen = self
            .mipmap_infos
            .get(&info)
            .and_then(|m| int(m, "Offscreen"))
            .and_then(|o| self.offscreens.get(&o))
            .ok_or("missing offscreen")?;
        let attr = Attributes::parse(blob(offscreen, "Attribute").ok_or("no attributes")?)?;
        // No chunk: nothing painted, every tile is the default.
        let tiles = match blob(offscreen, "BlockData").and_then(|id| self.external.get(id)) {
            Some(body) => read_blocks(body, attr.grid.0 * attr.grid.1)?,
            None => vec![None; attr.grid.0 * attr.grid.1],
        };
        attr.decode(&tiles, at, grey)
    }
}

/// Reading big-endian words in turn.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn bad(&self) -> String {
        "damaged attributes".to_string()
    }

    fn u32(&mut self) -> Result<usize, String> {
        let v = be32(self.bytes, self.at).ok_or_else(|| self.bad())?;
        self.at += 4;
        Ok(v)
    }

    /// A UTF-16 name, its length first.
    fn skip_label(&mut self) -> Result<(), String> {
        let n = self.u32()?;
        self.at += 2 * n;
        Ok(())
    }
}

/// An `Offscreen.Attribute` record: sizes, the tile grid, how pixels are
/// packed and what unpainted pixels are.
struct Attributes {
    size: (usize, usize),
    grid: (usize, usize),
    /// Planar alpha channels, then interleaved colour channels.
    channels: (usize, usize),
    monochrome: bool,
    /// Unpainted pixels are opaque white (else transparent / black).
    filled: bool,
}

impl Attributes {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut r = Cursor { bytes, at: 0 };
        if r.u32()? != 16 {
            return Err(r.bad());
        }
        let (_params, _init, _blocks) = (r.u32()?, r.u32()?, r.u32()?);
        // "Parameter" (its length in UTF-16 units first).
        r.skip_label()?;
        let size = (r.u32()?, r.u32()?);
        let grid = (r.u32()?, r.u32()?);
        let mut packing = [0usize; 16];
        for p in &mut packing {
            *p = r.u32()?;
        }
        // "InitColor", the record size, then the default fill.
        r.skip_label()?;
        let _record = r.u32()?;
        let fill = r.u32()?;
        let (tw, th) = (packing[10], packing[11]);
        if (tw, th) != (TILE, TILE) && (tw, th) != (0, 0) {
            return Err(format!("{tw}×{th} tiles"));
        }
        if size.0 > 1 << 16
            || size.1 > 1 << 16
            || size.0 * size.1 > 1 << 28
            || grid.0 != size.0.div_ceil(TILE)
            || grid.1 != size.1.div_ceil(TILE)
        {
            return Err(r.bad());
        }
        Ok(Attributes {
            size,
            grid,
            channels: (packing[1], packing[2]),
            monochrome: packing[14] != 0 || packing[8] >> 5 == 1,
            filled: fill != 0,
        })
    }

    /// Unpack the tiles; `grey` for a mask (one channel).
    fn decode(
        &self,
        tiles: &[Option<Vec<u8>>],
        at: (i32, i32),
        grey: bool,
    ) -> Result<Raster, String> {
        if self.monochrome {
            return Err("1-bit pixels".into());
        }
        let k = TILE * TILE;
        let per_tile = match self.channels {
            (1, 4) if !grey => 5 * k,
            (a, b) if grey && a + b == 1 => k,
            (1, 1) if !grey => 2 * k,
            (a, b) => return Err(format!("{a}+{b} channel pixels")),
        };
        // The painted tiles' extent (all of it when unpainted isn't clear).
        let (w, h) = self.size;
        let whole = self.filled && !grey;
        let mut span = [usize::MAX, usize::MAX, 0, 0];
        for (i, t) in tiles.iter().enumerate() {
            if t.is_some() || whole {
                let (tx, ty) = (i % self.grid.0, i / self.grid.0);
                span = [
                    span[0].min(tx),
                    span[1].min(ty),
                    span[2].max(tx + 1),
                    span[3].max(ty + 1),
                ];
            }
        }
        let (left, top, right, bottom) = if span[0] == usize::MAX {
            (0, 0, 0, 0)
        } else {
            (
                span[0] * TILE,
                span[1] * TILE,
                (span[2] * TILE).min(w),
                (span[3] * TILE).min(h),
            )
        };
        let (rw, rh) = (right.saturating_sub(left), bottom.saturating_sub(top));
        let rect = [
            at.0 + left as i32,
            at.1 + top as i32,
            at.0 + (left + rw) as i32,
            at.1 + (top + rh) as i32,
        ];
        let fill = if self.filled { 255 } else { 0 };
        if grey {
            let mut data = vec![fill; rw * rh];
            for (i, t) in tiles.iter().enumerate() {
                let Some(t) = t else { continue };
                if t.len() != per_tile {
                    return Err("tile of the wrong size".into());
                }
                let (tx, ty) = ((i % self.grid.0) * TILE, (i / self.grid.0) * TILE);
                for y in 0..TILE.min(bottom.saturating_sub(ty)) {
                    let row = (ty + y - top) * rw;
                    let n = TILE.min(right - tx);
                    data[row + tx - left..row + tx - left + n]
                        .copy_from_slice(&t[y * TILE..y * TILE + n]);
                }
            }
            return Ok(Raster::Grey(rect, data, fill));
        }
        let default = if self.filled {
            Color32::WHITE
        } else {
            Color32::TRANSPARENT
        };
        let mut pixels = vec![default; rw * rh];
        for (i, t) in tiles.iter().enumerate() {
            let Some(t) = t else { continue };
            if t.len() != per_tile {
                return Err("tile of the wrong size".into());
            }
            let (tx, ty) = ((i % self.grid.0) * TILE, (i / self.grid.0) * TILE);
            for y in 0..TILE.min(bottom.saturating_sub(ty)) {
                let row = (ty + y - top) * rw;
                for x in 0..TILE.min(right - tx) {
                    let p = y * TILE + x;
                    let a = t[p];
                    pixels[row + tx + x - left] = if per_tile == 5 * k {
                        // Alpha, then B G R x.
                        let c = &t[k + 4 * p..k + 4 * p + 3];
                        Color32::from_rgba_unmultiplied(c[2], c[1], c[0], a)
                    } else {
                        // Alpha, then grey.
                        let v = t[k + p];
                        Color32::from_rgba_unmultiplied(v, v, v, a)
                    };
                }
            }
        }
        Ok(Raster::Colour(rect, pixels))
    }
}

/// An external chunk's tiles, in order (`None` for unpainted ones),
/// inflated.
///
/// Each block starts with its length and a UTF-16 name:
/// `BlockDataBeginChunk` (index, tile shape, whether there's data, then its
/// lengths and zlib data, ending with `BlockDataEndChunk`), or the
/// `BlockStatus` / `BlockCheckSum` tables after the tiles.
fn read_blocks(body: &[u8], count: usize) -> Result<Vec<Option<Vec<u8>>>, String> {
    const BEGIN: &str = "BlockDataBeginChunk";
    let bad = |what: &str| format!("damaged tiles ({what})");
    let utf16 = |s: &str| {
        s.encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<u8>>()
    };
    let begin = utf16(BEGIN);
    let mut tiles: Vec<Option<Vec<u8>>> = vec![None; count];
    let mut at = 0;
    while at + 8 <= body.len() {
        if body.get(at + 8..at + 8 + begin.len()) == Some(&begin[..]) {
            let len = be32(body, at).ok_or_else(|| bad("length"))?;
            let block = body.get(at..at + len).ok_or_else(|| bad("length"))?;
            let head = 8 + begin.len();
            let index = be32(block, head).ok_or_else(|| bad("index"))?;
            let has = be32(block, head + 16).ok_or_else(|| bad("flag"))?;
            if has == 1 {
                let outer = be32(block, head + 20).ok_or_else(|| bad("size"))?;
                let data = block
                    .get(head + 28..head + 24 + outer)
                    .ok_or_else(|| bad("size"))?;
                let pixels =
                    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, 8 * TILE * TILE)
                        .map_err(|_| bad("zlib"))?;
                *tiles.get_mut(index).ok_or_else(|| bad("index"))? = Some(pixels);
            }
            at += len;
        } else {
            // A status or checksum table: name, then 12, count, 4 and the
            // values.
            let name = be32(body, at).ok_or_else(|| bad("table"))?;
            let n = be32(body, at + 4 + 2 * name + 4).ok_or_else(|| bad("table"))?;
            at += 4 + 2 * name + 12 + 4 * n;
        }
    }
    Ok(tiles)
}

/// Clip Studio's blend modes (`Layer.LayerComposite`).
fn blend(mode: i64) -> LayerBlend {
    use LayerBlend::*;
    match mode {
        1 => Darken,
        2 => Multiply,
        3 => ColorBurn,
        4 => LinearBurn,
        5 => Subtract,
        6 => DarkerColor,
        7 => Lighten,
        8 => Screen,
        // Colour dodge and glow dodge.
        9 | 10 => ColorDodge,
        // Add and add (glow).
        11 | 12 => LinearDodge,
        13 => LighterColor,
        14 => Overlay,
        15 => SoftLight,
        16 => HardLight,
        17 => VividLight,
        18 => LinearLight,
        19 => PinLight,
        20 => HardMix,
        21 => Difference,
        22 => Exclusion,
        23 => Hue,
        24 => Saturation,
        25 => Color,
        26 => Luminosity,
        36 => Divide,
        _ => Normal,
    }
}

#[cfg(test)]
mod tests {
    //! No Clip Studio sample can be shipped, so these build documents to
    //! the published layout (the same one clip_to_psd and the clipfile
    //! crate read) and check every part comes back.

    use super::*;

    fn label(out: &mut Vec<u8>, s: &str) {
        out.extend((s.encode_utf16().count() as u32).to_be_bytes());
        out.extend(s.encode_utf16().flat_map(u16::to_be_bytes));
    }

    /// An `Offscreen.Attribute` record.
    fn attributes(size: (u32, u32), channels: (u32, u32), filled: bool) -> Vec<u8> {
        let grid = (size.0.div_ceil(256), size.1.div_ceil(256));
        let mut params = Vec::new();
        label(&mut params, "Parameter");
        for v in [size.0, size.1, grid.0, grid.1] {
            params.extend(v.to_be_bytes());
        }
        let mut packing = [0u32; 16];
        packing[1] = channels.0;
        packing[2] = channels.1;
        packing[3] = channels.0 + channels.1;
        packing[6] = (channels.1 * 8) << 5;
        packing[8] = 8 << 5;
        packing[10] = 256;
        packing[11] = 256;
        params.extend(packing.iter().flat_map(|v| v.to_be_bytes()));
        let mut init = Vec::new();
        label(&mut init, "InitColor");
        for v in [20, filled as u32, 0, 0, 4] {
            init.extend(v.to_be_bytes());
        }
        let mut blocks = Vec::new();
        label(&mut blocks, "BlockSize");
        for v in [12, grid.0 * grid.1, 4] {
            blocks.extend(v.to_be_bytes());
        }
        for _ in 0..grid.0 * grid.1 {
            blocks.extend(0u32.to_be_bytes());
        }
        let mut out = Vec::new();
        for v in [16, params.len(), init.len(), blocks.len()] {
            out.extend((v as u32).to_be_bytes());
        }
        out.extend(params);
        out.extend(init);
        out.extend(blocks);
        out
    }

    /// An external chunk body holding `tiles` (raw, uncompressed).
    fn block_data(tiles: &[Option<Vec<u8>>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, t) in tiles.iter().enumerate() {
            let mut b = Vec::new();
            label(&mut b, "BlockDataBeginChunk");
            b.extend((i as u32).to_be_bytes());
            b.extend([0, 5, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0]);
            match t {
                Some(t) => {
                    let z = miniz_oxide::deflate::compress_to_vec_zlib(t, 6);
                    b.extend(1u32.to_be_bytes());
                    b.extend((z.len() as u32 + 4).to_be_bytes());
                    b.extend((z.len() as u32).to_le_bytes());
                    b.extend(z);
                }
                None => b.extend(0u32.to_be_bytes()),
            }
            label(&mut b, "BlockDataEndChunk");
            out.extend((b.len() as u32 + 4).to_be_bytes());
            out.extend(b);
        }
        for name in ["BlockStatus", "BlockCheckSum"] {
            label(&mut out, name);
            for v in [12, tiles.len() as u32, 4] {
                out.extend(v.to_be_bytes());
            }
            for _ in tiles {
                out.extend(1u32.to_be_bytes());
            }
        }
        out
    }

    /// A 5-channel tile: alpha plane, then B G R x.
    fn colour_tile(paint: impl Fn(usize, usize) -> Option<[u8; 4]>) -> Vec<u8> {
        let k = 256 * 256;
        let mut t = vec![0u8; 5 * k];
        for y in 0..256 {
            for x in 0..256 {
                if let Some([r, g, b, a]) = paint(x, y) {
                    let p = y * 256 + x;
                    t[p] = a;
                    t[k + 4 * p..k + 4 * p + 3].copy_from_slice(&[b, g, r]);
                }
            }
        }
        t
    }

    fn chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
        out.extend(b"CHNK");
        out.extend(name);
        out.extend((data.len() as u64).to_be_bytes());
        out.extend(data);
    }

    /// A container around `db` and the external chunks.
    fn container(db: &[u8], external: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        chunk(&mut body, b"Head", &[0; 40]);
        for (id, data) in external {
            let mut e = Vec::new();
            e.extend((id.len() as u64).to_be_bytes());
            e.extend(id.as_bytes());
            e.extend((data.len() as u64).to_be_bytes());
            e.extend(data);
            chunk(&mut body, b"Exta", &e);
        }
        chunk(&mut body, b"SQLi", db);
        chunk(&mut body, b"Foot", &[]);
        let mut out = b"CSFCHUNK".to_vec();
        out.extend((body.len() as u64 + 24).to_be_bytes());
        out.extend(24u64.to_be_bytes());
        out.extend(body);
        out
    }

    fn id(n: u32) -> String {
        format!("extrnlid{n:032X}")
    }

    fn preview_png(w: u32, h: u32, colour: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(colour));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    /// The test document (300×200, bottom to top): white paper; "Red",
    /// a red square moved 5 right; folder "Folder" at half opacity holding
    /// "Blue" (multiply, in the second tile column, masked on its left
    /// half) and invisible "Hidden"; a filter layer, which can't come
    /// across.
    fn document() -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!(
            "rusty-painter-clip-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let db = rusqlite::Connection::open(&dir).unwrap();
        db.execute_batch(
            "CREATE TABLE Canvas (MainId INTEGER, CanvasWidth REAL, CanvasHeight REAL, CanvasRootFolder INTEGER);
             CREATE TABLE Layer (MainId INTEGER, LayerName TEXT, LayerFolder INTEGER,
                LayerFirstChildIndex INTEGER, LayerNextIndex INTEGER, LayerOpacity INTEGER,
                LayerVisibility INTEGER, LayerComposite INTEGER, LayerClip INTEGER, LayerLock INTEGER,
                LayerOffsetX INTEGER, LayerOffsetY INTEGER,
                LayerRenderOffscrOffsetX INTEGER, LayerRenderOffscrOffsetY INTEGER,
                LayerMaskOffsetX INTEGER, LayerMaskOffsetY INTEGER,
                LayerMaskOffscrOffsetX INTEGER, LayerMaskOffscrOffsetY INTEGER,
                LayerRenderMipmap INTEGER, LayerLayerMaskMipmap INTEGER,
                DrawColorEnable INTEGER, DrawColorMainRed INTEGER, DrawColorMainGreen INTEGER,
                DrawColorMainBlue INTEGER, FilterLayerInfo BLOB);
             CREATE TABLE Mipmap (MainId INTEGER, BaseMipmapInfo INTEGER);
             CREATE TABLE MipmapInfo (MainId INTEGER, Offscreen INTEGER);
             CREATE TABLE Offscreen (MainId INTEGER, LayerId INTEGER, Attribute BLOB, BlockData BLOB);
             CREATE TABLE CanvasPreview (MainId INTEGER, ImageData BLOB);
             INSERT INTO Canvas VALUES (1, 300.0, 200.0, 1);",
        )
        .unwrap();
        #[allow(clippy::type_complexity)]
        let layers: [(
            i64,
            &str,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            i64,
            bool,
        ); 7] = [
            // id, name, folder, first child, next, opacity, visibility,
            // blend, render mipmap, mask mipmap, x offset, paper
            (1, "Root", 1, 2, 0, 256, 1, 0, None, None, 0, false),
            (2, "Paper", 0, 0, 3, 256, 1, 0, None, None, 0, true),
            (3, "Red", 0, 0, 4, 256, 1, 0, Some(10), None, 5, false),
            (4, "Folder", 1, 5, 7, 128, 1, 0, None, None, 0, false),
            (5, "Blue", 0, 0, 6, 256, 3, 2, Some(11), Some(12), 0, false),
            (6, "Hidden", 0, 0, 0, 256, 0, 0, Some(13), None, 0, false),
            (7, "Levels", 0, 0, 0, 256, 1, 0, None, None, 0, false),
        ];
        for (id, name, folder, first, next, opacity, vis, blend, render, mask, x, paper) in layers {
            db.execute(
                "INSERT INTO Layer VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0, ?9, 0, 0, 0, 0, 0, 0, 0,
                    ?10, ?11, ?12, ?13, ?13, ?13, ?14)",
                rusqlite::params![
                    id, name, folder, first, next, opacity, vis, blend, x, render, mask,
                    paper as i64, u32::MAX as i64,
                    (id == 7).then_some(vec![0u8, 0, 0, 2]),
                ],
            )
            .unwrap();
        }
        // Mipmap n → info n → offscreen n.
        let mut external = Vec::new();
        let mut offscreen = |n: i64, attr: Vec<u8>, tiles: Option<Vec<Option<Vec<u8>>>>| {
            db.execute("INSERT INTO Mipmap VALUES (?1, ?1)", [n])
                .unwrap();
            db.execute("INSERT INTO MipmapInfo VALUES (?1, ?1)", [n])
                .unwrap();
            db.execute(
                "INSERT INTO Offscreen VALUES (?1, 0, ?2, ?3)",
                rusqlite::params![n, attr, id(n as u32).into_bytes()],
            )
            .unwrap();
            if let Some(tiles) = tiles {
                external.push((id(n as u32), block_data(&tiles)));
            }
        };
        let red = colour_tile(|x, y| {
            ((10..90).contains(&x) && (10..70).contains(&y)).then_some([255, 0, 0, 255])
        });
        offscreen(
            10,
            attributes((300, 200), (1, 4), false),
            Some(vec![Some(red), None]),
        );
        let blue = colour_tile(|x, y| (x < 40 && y < 30).then_some([0, 0, 255, 200]));
        offscreen(
            11,
            attributes((300, 200), (1, 4), false),
            Some(vec![None, Some(blue)]),
        );
        let mut mask = vec![255u8; 256 * 256];
        for row in mask.chunks_mut(256) {
            row[..20].fill(0);
        }
        offscreen(
            12,
            attributes((300, 200), (0, 1), true),
            Some(vec![None, Some(mask)]),
        );
        // No chunk at all: an unpainted layer.
        offscreen(13, attributes((300, 200), (1, 4), false), None);
        db.execute(
            "INSERT INTO CanvasPreview VALUES (1, ?1)",
            [preview_png(150, 100, [9, 8, 7, 255])],
        )
        .unwrap();
        drop(db);
        let bytes = std::fs::read(&dir).unwrap();
        let _ = std::fs::remove_file(&dir);
        container(&bytes, &external)
    }

    fn at(l: &PsdLayer, x: i32, y: i32) -> Color32 {
        let w = l.rect[2] - l.rect[0];
        l.pixels[((y - l.rect[1]) * w + x - l.rect[0]) as usize]
    }

    #[test]
    fn a_clip_studio_document_opens_layer_by_layer() {
        let doc = decode_clip(&document()).unwrap();
        assert_eq!((doc.width, doc.height), (300, 200));
        let names: Vec<(&str, &PsdKind)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), &l.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("Paper", &PsdKind::Pixels),
                ("Red", &PsdKind::Pixels),
                ("Folder", &PsdKind::GroupEnd),
                ("Blue", &PsdKind::Pixels),
                ("Hidden", &PsdKind::Pixels),
                ("Folder", &PsdKind::GroupStart),
                (
                    "Clip Studio's picture (parts not brought across)",
                    &PsdKind::Pixels
                ),
            ]
        );
        let paper = &doc.layers[0];
        assert_eq!(paper.rect, [0, 0, 300, 200]);
        assert_eq!(at(paper, 150, 100), Color32::WHITE);

        let red = &doc.layers[1];
        assert_eq!(red.rect, [5, 0, 5 + 256, 200], "the painted tile, moved");
        assert_eq!(at(red, 15, 10), Color32::RED);
        assert_eq!(at(red, 14, 10), Color32::TRANSPARENT);
        assert_eq!(at(red, 5 + 89, 69), Color32::RED);
        assert_eq!(at(red, 5 + 90, 69), Color32::TRANSPARENT);

        let folder = &doc.layers[5];
        assert!((folder.opacity - 0.5).abs() < 1e-6);

        let blue = &doc.layers[3];
        assert_eq!(blue.rect, [256, 0, 300, 200], "cut to the canvas");
        assert_eq!(blue.blend, LayerBlend::Multiply);
        assert_eq!(
            at(blue, 256, 0),
            Color32::from_rgba_unmultiplied(0, 0, 255, 200)
        );
        let mask = blue.mask.as_ref().expect("mask");
        assert_eq!(mask.default, 255);
        let mw = (mask.rect[2] - mask.rect[0]) as usize;
        let level = |x: i32| mask.data[(x - mask.rect[0]) as usize + 5 * mw];
        assert_eq!((level(256 + 19), level(256 + 20)), (0, 255));

        let hidden = &doc.layers[4];
        assert!(!hidden.visible && hidden.pixels.is_empty());

        let preview = doc.layers.last().unwrap();
        assert!(!preview.visible, "the preview is there to compare, hidden");
        assert_eq!(preview.rect, [0, 0, 300, 200], "stretched to the canvas");
        assert_eq!(at(preview, 299, 199), Color32::from_rgb(9, 8, 7));

        let canvas = doc.into_canvas().unwrap();
        assert_eq!((canvas.width(), canvas.height()), (300, 200));
    }

    #[test]
    fn damaged_or_foreign_files_are_refused_not_panicked_on() {
        assert!(decode_clip(b"PK\x03\x04 not this").is_err());
        let good = document();
        for cut in [30, 100, good.len() / 2, good.len() - 20] {
            let _ = decode_clip(&good[..cut]);
        }
        let mut flipped = good.clone();
        for i in (24..flipped.len()).step_by(997) {
            flipped[i] ^= 0x5a;
        }
        let _ = decode_clip(&flipped);
    }

    #[test]
    fn blend_modes_map_across() {
        assert_eq!(blend(0), LayerBlend::Normal);
        assert_eq!(blend(2), LayerBlend::Multiply);
        assert_eq!(blend(14), LayerBlend::Overlay);
        assert_eq!(blend(26), LayerBlend::Luminosity);
        assert_eq!(blend(30), LayerBlend::Normal, "pass-through");
        assert_eq!(blend(999), LayerBlend::Normal);
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_clip() {
        crate::fuzz::fuzz(
            "clip",
            &document(),
            std::time::Duration::from_secs(2),
            |b| {
                if let Ok(doc) = decode_clip(b)
                    && let Ok(canvas) = doc.into_canvas()
                {
                    canvas.flatten();
                }
            },
        );
    }
}
