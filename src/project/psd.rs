//! Photoshop documents (`.psd`, 8 or 16-bit RGB or grayscale), read and
//! written: raster layers with their name, opacity, visibility, blend mode,
//! clipping, locked transparency and layer mask, and folders. Pixels are
//! RLE-compressed (PackBits) as Photoshop writes them (ZIP-compressed ones
//! are read too). A 16-bit file opens as a 16-bit document, and a deeper
//! document is written at 16 bits. Our text layers are written as their
//! pixels.
//!
//! Not kept: adjustment, text and smart-object layers come in as the
//! pixels Photoshop stored for them; pass-through folders open as Normal;
//! 32-bit, CMYK and large (`.psb`) documents are refused.

use eframe::egui::Color32;

use crate::canvas::Canvas;
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::storage::{
    CanvasLayerSnapshot, CanvasTileSnapshot, DeepTile, Depth, LayerId, LayerKind,
};

/// Photoshop's limit for `.psd` (bigger documents need `.psb`).
const MAX_EDGE: usize = 30_000;
const GROUP_END_NAME: &str = "</Layer group>";

/// A document as PSD sees it: layers bottom to top, folders as a pair of
/// records around their contents.
pub struct PsdDocument {
    pub width: usize,
    pub height: usize,
    pub layers: Vec<PsdLayer>,
    /// The flattened picture, premultiplied.
    pub composite: Vec<Color32>,
    /// Bits for each channel the layers' pixels came with (see
    /// [`PsdLayer::deep`]).
    pub depth: Depth,
    /// In a deeper document, the flattened picture at full depth
    /// (premultiplied linear light), for writing a 16-bit file.
    pub composite_deep: Option<Vec<[f32; 4]>>,
    /// Which colours the pixels are (the file's embedded ICC profile).
    pub profile: crate::canvas::color_profile::ColorProfile,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PsdKind {
    Pixels,
    /// A folder's own record, above its contents.
    GroupStart,
    /// The marker below a folder's contents.
    GroupEnd,
}

pub struct PsdLayer {
    pub name: String,
    pub kind: PsdKind,
    /// `[left, top, right, bottom)` in canvas pixels.
    pub rect: [i32; 4],
    /// Premultiplied, `rect`-sized.
    pub pixels: Vec<Color32>,
    /// In a deeper document, the same pixels at full depth: premultiplied
    /// linear light.
    pub deep: Option<Vec<[f32; 4]>>,
    pub opacity: f32,
    pub visible: bool,
    pub blend: LayerBlend,
    pub clipped: bool,
    pub alpha_locked: bool,
    /// Photoshop's "lock position" (the `lspf` protection flags).
    pub position_locked: bool,
    pub mask: Option<PsdMask>,
}

/// A layer mask: grey levels (255 shows) over `rect`, `default` elsewhere.
pub struct PsdMask {
    pub rect: [i32; 4],
    pub data: Vec<u8>,
    pub default: u8,
}

fn blend_key(blend: LayerBlend) -> &'static [u8; 4] {
    use LayerBlend::*;
    match blend {
        Normal => b"norm",
        Dissolve => b"diss",
        Darken => b"dark",
        Multiply => b"mul ",
        ColorBurn => b"idiv",
        LinearBurn => b"lbrn",
        DarkerColor => b"dkCl",
        Lighten => b"lite",
        Screen => b"scrn",
        ColorDodge => b"div ",
        LinearDodge => b"lddg",
        LighterColor => b"lgCl",
        Overlay => b"over",
        SoftLight => b"sLit",
        HardLight => b"hLit",
        VividLight => b"vLit",
        LinearLight => b"lLit",
        PinLight => b"pLit",
        HardMix => b"hMix",
        Difference => b"diff",
        Exclusion => b"smud",
        Subtract => b"fsub",
        Divide => b"fdiv",
        Hue => b"hue ",
        Saturation => b"sat ",
        Color => b"colr",
        Luminosity => b"lum ",
        // Photoshop has no Parallel.
        Parallel => b"norm",
    }
}

/// Keys with no counterpart (pass-through) read as Normal.
fn blend_from_key(key: &[u8]) -> LayerBlend {
    LayerBlend::GROUPS
        .iter()
        .flat_map(|g| g.iter())
        .copied()
        .find(|&b| blend_key(b) == key)
        .unwrap_or(LayerBlend::Normal)
}

// ---------------------------------------------------------------- canvas

impl PsdDocument {
    /// Everything PSD can hold of `canvas`.
    pub fn from_canvas(canvas: &Canvas) -> Self {
        let mut layers = Vec::new();
        push_children(canvas, None, &mut layers, 0);
        // As exported: without the draft layers.
        let flat = canvas.flatten_final();
        // PSD keeps 8 or 16 bits: a float document is written at 16.
        let deep = canvas.depth().is_deep();
        PsdDocument {
            depth: if deep { Depth::U16 } else { Depth::U8 },
            composite_deep: deep.then(|| canvas.flatten_final_linear()),
            profile: canvas.profile.clone(),
            width: canvas.width(),
            height: canvas.height(),
            layers,
            composite: flat.pixels,
        }
    }

    /// The document as a canvas. A bottom layer that is Photoshop's
    /// background becomes ours; otherwise ours is hidden (transparent).
    pub fn into_canvas(self) -> Result<Canvas, String> {
        crate::app::document::validate_canvas_size(self.width, self.height)?;
        let ts = crate::app::document::TILE_SIZE;
        let mut canvas = Canvas::new(self.width, self.height, Color32::WHITE, ts);
        // (Before the layers come in: their tiles are made at it.)
        canvas.convert_depth(self.depth);
        canvas.profile = self.profile.clone();
        let depth = self.depth;
        let mut snapshots = Vec::new();
        let mut next_id = 1u64;
        let mut layers = self.layers.into_iter().peekable();
        // Photoshop's background has a translated name ("Sfondo",
        // "Hintergrund"...): know it by being opaque and covering the canvas.
        let full = [0, 0, self.width as i32, self.height as i32];
        let background = layers
            .next_if(|l| {
                l.kind == PsdKind::Pixels
                    && (l.name == "Background"
                        || (l.rect == full && l.pixels.iter().all(|p| p.a() == 255)))
            })
            .map(|l| (l.visible, layer_tiles(&l, ts, Color32::TRANSPARENT, depth)));
        let (visible, background) = background.map_or((false, None), |(v, t)| (v, Some(t)));
        snapshots.push(CanvasLayerSnapshot {
            id: LayerId(0),
            name: "Background".into(),
            visible,
            opacity: 1.0,
            locked: true,
            alpha_locked: false,
            kind: LayerKind::Paint,
            parent: None,
            expanded: true,
            blend: LayerBlend::Normal,
            clipped: false,
            adjustment: None,
            style: Default::default(),
            text: None,
            vector: None,
            height: None,
            shader: None,
            position_locked: false,
            draft: false,
            reference: false,
            anim: None,
            rig: None,
            tiles: background.unwrap_or_default(),
        });
        // Folders being read: each GroupEnd opens one, its GroupStart closes it.
        let mut open: Vec<LayerId> = Vec::new();
        for layer in layers {
            let parent = open.last().copied();
            let id = match layer.kind {
                PsdKind::GroupEnd => {
                    open.push(LayerId(next_id));
                    next_id += 1;
                    continue;
                }
                PsdKind::GroupStart => match open.pop() {
                    Some(id) => id,
                    None => continue, // unbalanced; the contents stay at top level
                },
                PsdKind::Pixels => {
                    next_id += 1;
                    LayerId(next_id - 1)
                }
            };
            let parent = if layer.kind == PsdKind::GroupStart {
                open.last().copied()
            } else {
                parent
            };
            let is_group = layer.kind == PsdKind::GroupStart;
            let tiles = if is_group {
                Vec::new()
            } else {
                layer_tiles(&layer, ts, Color32::TRANSPARENT, depth)
            };
            snapshots.push(CanvasLayerSnapshot {
                id,
                name: layer.name,
                visible: layer.visible,
                opacity: layer.opacity,
                locked: false,
                alpha_locked: layer.alpha_locked && !is_group,
                kind: if is_group {
                    LayerKind::Group
                } else {
                    LayerKind::Paint
                },
                parent,
                expanded: true,
                blend: layer.blend,
                clipped: layer.clipped,
                adjustment: None,
                style: Default::default(),
                text: None,
                vector: None,
                height: None,
                shader: None,
                position_locked: layer.position_locked,
                draft: false,
                reference: false,
                anim: None,
                rig: None,
                tiles,
            });
            if let Some(mask) = layer.mask.filter(|_| !is_group) {
                snapshots.push(CanvasLayerSnapshot {
                    id: LayerId(next_id),
                    name: format!("{} mask", snapshots.last().map_or("", |s| s.name.as_str())),
                    visible: true,
                    opacity: 1.0,
                    locked: false,
                    alpha_locked: false,
                    kind: LayerKind::Mask { owner: id },
                    parent: None,
                    expanded: true,
                    blend: LayerBlend::Normal,
                    clipped: false,
                    adjustment: None,
                    style: Default::default(),
                    text: None,
                    vector: None,
                    height: None,
                    shader: None,
                    position_locked: false,
                    draft: false,
                    reference: false,
                    anim: None,
                    rig: None,
                    tiles: mask_tiles(&mask, self.width, self.height, ts),
                });
                next_id += 1;
            }
        }
        let active = snapshots.len() - 1;
        canvas.replace_layers_from_snapshots(snapshots, active);
        // Photoshop blends layers in gamma space.
        canvas.blend_space = crate::canvas::blend_modes::BlendSpace::Gamma;
        Ok(canvas)
    }
}

/// The children of `parent`, bottom first, with folders around theirs.
fn push_children(canvas: &Canvas, parent: Option<LayerId>, out: &mut Vec<PsdLayer>, depth: usize) {
    if depth > canvas.layers.len() {
        return;
    }
    for (i, layer) in canvas.layers.iter().enumerate() {
        // Adjustment and fill layers have no PSD form here: the composite
        // shows them.
        if layer.parent != parent
            || matches!(layer.kind, LayerKind::Mask { .. })
            || layer.adjustment.is_some()
            || layer.style.fill.is_some()
        {
            continue;
        }
        let base = PsdLayer {
            deep: None,
            name: layer.name.clone(),
            kind: PsdKind::Pixels,
            rect: [0; 4],
            pixels: Vec::new(),
            opacity: layer.opacity,
            visible: layer.visible,
            blend: layer.blend,
            clipped: layer.clipped && i != 0,
            alpha_locked: layer.alpha_locked,
            position_locked: layer.position_locked,
            mask: None,
        };
        if layer.kind == LayerKind::Group {
            out.push(PsdLayer {
                name: GROUP_END_NAME.into(),
                kind: PsdKind::GroupEnd,
                pixels: Vec::new(),
                deep: None,
                mask: None,
                blend: LayerBlend::Normal,
                clipped: false,
                visible: true,
                opacity: 1.0,
                ..base
            });
            push_children(canvas, Some(layer.id), out, depth + 1);
            out.push(PsdLayer {
                kind: PsdKind::GroupStart,
                ..base
            });
            continue;
        }
        // The background shows its colour where it's unpainted.
        let fill = if i == 0 {
            canvas.clear_color()
        } else {
            Color32::TRANSPARENT
        };
        let (rect, pixels) = if i == 0 {
            let r = [0, 0, canvas.width() as i32, canvas.height() as i32];
            (r, layer_area(canvas, i, r, fill))
        } else {
            match content_rect(canvas, i) {
                Some(r) => (r, layer_area(canvas, i, r, fill)),
                None => ([0; 4], Vec::new()),
            }
        };
        let mask = canvas.mask_index_of(layer.id).map(|m| {
            let rect = [0, 0, canvas.width() as i32, canvas.height() as i32];
            let data = layer_area(canvas, m, rect, Color32::WHITE)
                .iter()
                .map(|&p| ((p.r() as u32 + p.g() as u32 + p.b() as u32) / 3) as u8)
                .collect();
            PsdMask {
                rect,
                data,
                default: 255,
            }
        });
        let deep = (canvas.depth().is_deep() && rect[2] > rect[0])
            .then(|| layer_area_deep(canvas, i, rect, fill));
        out.push(PsdLayer {
            rect,
            pixels,
            deep,
            mask,
            // A disabled mask is left out rather than kept switched off.
            ..base
        });
        if let Some(PsdLayer { mask, .. }) = out.last_mut()
            && canvas
                .mask_index_of(layer.id)
                .is_some_and(|m| !canvas.layers[m].visible)
        {
            *mask = None;
        }
    }
}

/// The painted tiles' extent, within the canvas.
fn content_rect(canvas: &Canvas, idx: usize) -> Option<[i32; 4]> {
    let ts = canvas.tile_size() as i32;
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let mut r: Option<[i32; 4]> = None;
    for (tx, ty) in canvas.layer_tile_keys(idx) {
        let painted = canvas
            .get_layer_tile_data(idx, tx, ty)
            .is_some_and(|d| d.iter().any(|p| p.a() > 0));
        if !painted {
            continue;
        }
        let t = [tx * ts, ty * ts, (tx + 1) * ts, (ty + 1) * ts];
        r = Some(match r {
            None => t,
            Some(r) => [
                r[0].min(t[0]),
                r[1].min(t[1]),
                r[2].max(t[2]),
                r[3].max(t[3]),
            ],
        });
    }
    let [x0, y0, x1, y1] = r?;
    let r = [x0.max(0), y0.max(0), x1.min(w), y1.min(h)];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

/// [`layer_area`] at full depth: premultiplied linear light.
fn layer_area_deep(canvas: &Canvas, idx: usize, rect: [i32; 4], fill: Color32) -> Vec<[f32; 4]> {
    let ts = canvas.tile_size() as i32;
    let [x0, y0, x1, y1] = rect;
    let w = (x1 - x0) as usize;
    let fill = eframe::egui::Rgba::from(fill).to_array();
    let mut out = vec![fill; w * (y1 - y0) as usize];
    for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let Some(deep) = canvas.get_layer_tile_deep(idx, tx, ty) else {
                continue;
            };
            let (ox, oy) = (tx * ts, ty * ts);
            for y in y0.max(oy)..y1.min(oy + ts) {
                for x in x0.max(ox)..x1.min(ox + ts) {
                    let src = ((y - oy) * ts + (x - ox)) as usize;
                    out[(y - y0) as usize * w + (x - x0) as usize] = deep.linear(src);
                }
            }
        }
    }
    out
}

/// Layer `idx`'s pixels over `rect`; unpainted areas read as `fill`.
fn layer_area(canvas: &Canvas, idx: usize, rect: [i32; 4], fill: Color32) -> Vec<Color32> {
    let ts = canvas.tile_size() as i32;
    let [x0, y0, x1, y1] = rect;
    let w = (x1 - x0) as usize;
    let mut out = vec![fill; w * (y1 - y0) as usize];
    for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let Some(data) = canvas.get_layer_tile_data(idx, tx, ty) else {
                continue;
            };
            let (ox, oy) = (tx * ts, ty * ts);
            let (cx0, cx1) = (x0.max(ox), x1.min(ox + ts));
            for y in y0.max(oy)..y1.min(oy + ts) {
                let src = ((y - oy) * ts + (cx0 - ox)) as usize;
                let dst = (y - y0) as usize * w + (cx0 - x0) as usize;
                let n = (cx1 - cx0) as usize;
                out[dst..dst + n].copy_from_slice(&data[src..src + n]);
            }
        }
    }
    out
}

/// A layer's pixels as tiles (at full depth, if it has them), leaving out
/// those all `skip`.
fn layer_tiles(
    layer: &PsdLayer,
    ts: usize,
    skip: Color32,
    depth: Depth,
) -> Vec<CanvasTileSnapshot> {
    let mut tiles = tiles_of(&layer.pixels, layer.rect, ts, skip);
    let (Some(deep), true) = (&layer.deep, depth.is_deep()) else {
        return tiles;
    };
    let [x0, y0, x1, y1] = layer.rect;
    if deep.len() != layer.pixels.len() {
        return tiles;
    }
    let w = (x1 - x0) as usize;
    let t = ts as i32;
    for tile in &mut tiles {
        let Some(mut out) = DeepTile::transparent(depth, ts * ts) else {
            continue;
        };
        let (ox, oy) = (tile.tx * t, tile.ty * t);
        for y in y0.max(oy)..y1.min(oy + t) {
            for x in x0.max(ox)..x1.min(ox + t) {
                let src = (y - y0) as usize * w + (x - x0) as usize;
                out.set_linear(((y - oy) * t + (x - ox)) as usize, deep[src]);
            }
        }
        tile.data = out.narrow_all();
        tile.deep = Some(out);
    }
    tiles
}

/// A `rect`-sized buffer as tiles, leaving out those all `skip`.
fn tiles_of(
    pixels: &[Color32],
    rect: [i32; 4],
    ts: usize,
    skip: Color32,
) -> Vec<CanvasTileSnapshot> {
    let [x0, y0, x1, y1] = rect;
    if x1 <= x0 || y1 <= y0 || pixels.len() != ((x1 - x0) * (y1 - y0)) as usize {
        return Vec::new();
    }
    let w = (x1 - x0) as usize;
    let t = ts as i32;
    let mut tiles = Vec::new();
    for ty in y0.div_euclid(t)..=(y1 - 1).div_euclid(t) {
        for tx in x0.div_euclid(t)..=(x1 - 1).div_euclid(t) {
            if tx < 0 || ty < 0 {
                continue;
            }
            let mut data = vec![Color32::TRANSPARENT; ts * ts];
            let (ox, oy) = (tx * t, ty * t);
            let (cx0, cx1) = (x0.max(ox), x1.min(ox + t));
            for y in y0.max(oy)..y1.min(oy + t) {
                let src = (y - y0) as usize * w + (cx0 - x0) as usize;
                let dst = ((y - oy) * t + (cx0 - ox)) as usize;
                let n = (cx1 - cx0) as usize;
                data[dst..dst + n].copy_from_slice(&pixels[src..src + n]);
            }
            if data.iter().any(|&p| p != skip) {
                tiles.push(CanvasTileSnapshot {
                    tx,
                    ty,
                    data,
                    deep: None,
                });
            }
        }
    }
    tiles
}

/// A mask as whole-canvas tiles (white = shows, the default outside its
/// rect), leaving out all-white tiles (a missing mask tile shows).
fn mask_tiles(mask: &PsdMask, w: usize, h: usize, ts: usize) -> Vec<CanvasTileSnapshot> {
    let [x0, y0, x1, y1] = mask.rect;
    let mw = (x1 - x0).max(0) as usize;
    let grey = |v: u8| Color32::from_rgb(v, v, v);
    let full: Vec<Color32> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            if x >= x0 && x < x1 && y >= y0 && y < y1 {
                grey(mask.data[(y - y0) as usize * mw + (x - x0) as usize])
            } else {
                grey(mask.default)
            }
        })
        .collect();
    tiles_of(&full, [0, 0, w as i32, h as i32], ts, Color32::WHITE)
}

// ---------------------------------------------------------------- writing

struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    /// Reserve a u32 length, returning where it goes.
    fn len_slot(&mut self) -> usize {
        self.u32(0);
        self.0.len()
    }
    /// Fill a length slot with the bytes written since, padded to `align`.
    fn close_len(&mut self, start: usize, align: usize) {
        while !(self.0.len() - start).is_multiple_of(align) {
            self.0.push(0);
        }
        let len = (self.0.len() - start) as u32;
        self.0[start - 4..start].copy_from_slice(&len.to_be_bytes());
    }
}

/// PackBits, as Photoshop's RLE rows are.
fn packbits(row: &[u8], out: &mut Vec<u8>) {
    let n = row.len();
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && row[j] == row[i] && j - i < 128 {
            j += 1;
        }
        if j - i >= 2 {
            out.push((1 - (j - i) as i32) as i8 as u8);
            out.push(row[i]);
            i = j;
            continue;
        }
        let start = i;
        i += 1;
        while i < n && i - start < 128 && !(i + 1 < n && row[i] == row[i + 1]) {
            i += 1;
        }
        out.push((i - start - 1) as u8);
        out.extend_from_slice(&row[start..i]);
    }
}

/// One channel's planes RLE-encoded: the row byte counts, then the rows.
fn rle_channel(plane: &[u8], w: usize, h: usize) -> (Vec<u16>, Vec<u8>) {
    let mut counts = Vec::with_capacity(h);
    let mut data = Vec::new();
    for row in plane.chunks(w.max(1)).take(h) {
        let before = data.len();
        packbits(row, &mut data);
        counts.push((data.len() - before) as u16);
    }
    (counts, data)
}

/// Unpremultiplied R, G, B, A planes of 16-bit samples (sRGB encoded, big
/// endian) from premultiplied linear-light pixels.
fn planes16(pixels: &[[f32; 4]]) -> [Vec<u8>; 4] {
    use eframe::egui::ecolor::gamma_from_linear;
    let to = |v: f32| ((v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).to_be_bytes();
    let mut p: [Vec<u8>; 4] = std::array::from_fn(|_| Vec::with_capacity(pixels.len() * 2));
    for &[r, g, b, a] in pixels {
        let a = a.clamp(0.0, 1.0);
        let c = |v: f32| {
            if a <= 0.0 {
                0.0
            } else {
                gamma_from_linear((v / a).clamp(0.0, 1.0))
            }
        };
        for (plane, v) in p.iter_mut().zip([c(r), c(g), c(b), a]) {
            plane.extend(to(v));
        }
    }
    p
}

/// Unpremultiplied R, G, B, A planes.
fn planes(pixels: &[Color32]) -> [Vec<u8>; 4] {
    let mut p: [Vec<u8>; 4] = std::array::from_fn(|_| Vec::with_capacity(pixels.len()));
    for &c in pixels {
        for (plane, v) in p.iter_mut().zip(crate::canvas::blend::unmultiply(c)) {
            plane.push(v);
        }
    }
    p
}

/// An 8-bit plane as 16-bit samples (big endian, 257 steps each).
fn widen_plane(plane: Vec<u8>) -> Vec<u8> {
    plane
        .into_iter()
        .flat_map(|v| (v as u16 * 257).to_be_bytes())
        .collect()
}

fn pascal_name(name: &str, out: &mut Out) {
    let bytes: Vec<u8> = name
        .chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .take(255)
        .collect();
    let start = out.0.len();
    out.u8(bytes.len() as u8);
    out.bytes(&bytes);
    while !(out.0.len() - start).is_multiple_of(4) {
        out.u8(0);
    }
}

pub fn encode_psd(doc: &PsdDocument) -> Result<Vec<u8>, String> {
    use rayon::prelude::*;
    if doc.width > MAX_EDGE || doc.height > MAX_EDGE {
        return Err(format!(
            "PSD files are limited to {MAX_EDGE} px a side (this is {} × {})",
            doc.width, doc.height
        ));
    }
    let mut out = Out(Vec::new());
    out.bytes(b"8BPS");
    out.u16(1);
    out.bytes(&[0; 6]);
    out.u16(4);
    out.u32(doc.height as u32);
    out.u32(doc.width as u32);
    // 16 bits when the layers have them (written like the 8-bit file, as
    // Krita and GIMP do: each sample two bytes, rows twice as long).
    let sixteen = doc.depth.is_deep();
    let bytes = if sixteen { 2 } else { 1 };
    out.u16(8 * bytes as u16);
    out.u16(3); // RGB
    out.u32(0); // colour mode data
    // Image resources: the colour profile (1039), unless sRGB, which a file
    // without one means.
    let resources = out.len_slot();
    if doc.profile != crate::canvas::color_profile::ColorProfile::Srgb {
        let icc = doc.profile.icc();
        out.bytes(b"8BIM");
        out.u16(1039);
        out.u16(0); // no name (padded to even)
        out.u32(icc.len() as u32);
        out.bytes(&icc);
        if icc.len() % 2 == 1 {
            out.u8(0);
        }
    }
    out.close_len(resources, 1);

    // Each layer's channels, encoded in parallel: (id, compressed bytes).
    let channels: Vec<Vec<(i16, Vec<u8>)>> = doc
        .layers
        .par_iter()
        .map(|l| {
            let [x0, y0, x1, y1] = l.rect;
            let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
            let encode = |plane: &[u8], w: usize, h: usize| -> Vec<u8> {
                if w == 0 || h == 0 {
                    return vec![0, 0]; // raw, no pixels
                }
                let (counts, data) = rle_channel(plane, w * bytes, h);
                let mut o = Out(Vec::with_capacity(2 + counts.len() * 2 + data.len()));
                o.u16(1);
                for c in counts {
                    o.u16(c);
                }
                o.bytes(&data);
                o.0
            };
            let [r, g, b, a] = match (&l.deep, sixteen) {
                (Some(deep), true) if deep.len() == l.pixels.len() => planes16(deep),
                (_, true) => planes(&l.pixels).map(widen_plane),
                _ => planes(&l.pixels),
            };
            let mut ch = vec![
                (-1, encode(&a, w, h)),
                (0, encode(&r, w, h)),
                (1, encode(&g, w, h)),
                (2, encode(&b, w, h)),
            ];
            if let Some(m) = &l.mask {
                let mw = (m.rect[2] - m.rect[0]).max(0) as usize;
                let mh = (m.rect[3] - m.rect[1]).max(0) as usize;
                let data = if sixteen {
                    widen_plane(m.data.clone())
                } else {
                    m.data.clone()
                };
                ch.push((-2, encode(&data, mw, mh)));
            }
            ch
        })
        .collect();

    let section = out.len_slot();
    let info = out.len_slot();
    // Negative: the first alpha channel is the composite's transparency.
    out.i16(-(doc.layers.len() as i16));
    for (l, ch) in doc.layers.iter().zip(&channels) {
        let [x0, y0, x1, y1] = l.rect;
        out.i32(y0);
        out.i32(x0);
        out.i32(y1);
        out.i32(x1);
        out.u16(ch.len() as u16);
        for (id, data) in ch {
            out.i16(*id);
            out.u32(data.len() as u32);
        }
        out.bytes(b"8BIM");
        out.bytes(blend_key(l.blend));
        out.u8((l.opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
        out.u8(l.clipped as u8);
        let mut flags = 0u8;
        if l.alpha_locked {
            flags |= 1;
        }
        if !l.visible {
            flags |= 2;
        }
        if l.kind != PsdKind::Pixels {
            flags |= 0x18; // "pixel data irrelevant" (and that bit is valid)
        }
        out.u8(flags);
        out.u8(0);
        let extra = out.len_slot();
        match &l.mask {
            Some(m) => {
                out.u32(20);
                out.i32(m.rect[1]);
                out.i32(m.rect[0]);
                out.i32(m.rect[3]);
                out.i32(m.rect[2]);
                out.u8(m.default);
                out.u8(0);
                out.u16(0);
            }
            None => out.u32(0),
        }
        out.u32(0); // blending ranges
        pascal_name(&l.name, &mut out);
        // The full name, in UTF-16.
        out.bytes(b"8BIMluni");
        let luni = out.len_slot();
        let units: Vec<u16> = l.name.encode_utf16().collect();
        out.u32(units.len() as u32);
        for u in units {
            out.u16(u);
        }
        out.close_len(luni, 2);
        if l.kind != PsdKind::Pixels {
            out.bytes(b"8BIMlsct");
            out.u32(12);
            out.u32(if l.kind == PsdKind::GroupEnd { 3 } else { 1 });
            out.bytes(b"8BIM");
            out.bytes(blend_key(l.blend));
        }
        if l.position_locked {
            // Protection flags: bit 0 transparency, bit 2 position.
            out.bytes(b"8BIMlspf");
            out.u32(4);
            out.u32(4 | l.alpha_locked as u32);
        }
        out.close_len(extra, 1);
    }
    for ch in &channels {
        for (_, data) in ch {
            out.bytes(data);
        }
    }
    out.close_len(info, 2);
    out.u32(0); // global layer mask info
    out.close_len(section, 1);

    // The flattened picture: RLE, every channel's row counts first.
    let (w, h) = (doc.width, doc.height);
    let composite = match (&doc.composite_deep, sixteen) {
        (Some(deep), true) => planes16(deep),
        (_, true) => planes(&doc.composite).map(widen_plane),
        _ => planes(&doc.composite),
    };
    let encoded: Vec<(Vec<u16>, Vec<u8>)> = composite
        .par_iter()
        .map(|p| rle_channel(p, w * bytes, h))
        .collect();
    out.u16(1);
    for (counts, _) in &encoded {
        for &c in counts {
            out.u16(c);
        }
    }
    for (_, data) in &encoded {
        out.bytes(data);
    }
    Ok(out.0)
}

// ---------------------------------------------------------------- reading

struct In<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or("The PSD file is cut short")?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    /// A u32-length-prefixed block.
    fn block(&mut self) -> Result<In<'a>, String> {
        let len = self.u32()? as usize;
        Ok(In {
            b: self.take(len)?,
            pos: 0,
        })
    }
}

fn unpackbits(src: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while out.len() < len && i < src.len() {
        let h = src[i] as i8;
        i += 1;
        if h >= 0 {
            let n = (h as usize + 1).min(src.len() - i);
            out.extend_from_slice(&src[i..i + n]);
            i += n;
        } else if h != -128 {
            if let Some(&v) = src.get(i) {
                out.extend(std::iter::repeat_n(v, (1 - h as isize) as usize));
            }
            i += 1;
        }
    }
    out.resize(len, 0);
    out
}

/// A channel's `w`×`h` plane of `bytes`-byte samples (big endian): its
/// compression, then raw, RLE or ZIP rows.
fn read_plane(data: &[u8], w: usize, h: usize, bytes: usize) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Ok(Vec::new());
    }
    let row = w * bytes;
    let len = row * h;
    let mut r = In { b: data, pos: 0 };
    match r.u16()? {
        0 => {
            let mut p = r.take(len.min(data.len().saturating_sub(2)))?.to_vec();
            p.resize(len, 0);
            Ok(p)
        }
        1 => {
            let counts: Vec<usize> = (0..h)
                .map(|_| r.u16().map(|c| c as usize))
                .collect::<Result<_, _>>()?;
            let mut plane = Vec::with_capacity(len);
            for c in counts {
                plane.extend(unpackbits(r.take(c)?, row));
            }
            Ok(plane)
        }
        // ZIP, and ZIP with each row stored as differences.
        compression @ (2 | 3) => {
            let mut plane =
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data[2..], len)
                    .map_err(|_| "Damaged ZIP data in the PSD")?;
            plane.resize(len, 0);
            if compression == 3 {
                undo_prediction(&mut plane, w, bytes);
            }
            Ok(plane)
        }
        _ => Err("Unknown PSD compression".into()),
    }
}

/// ZIP-with-prediction rows: each sample was stored as the difference from
/// the one before it in its row.
fn undo_prediction(plane: &mut [u8], w: usize, bytes: usize) {
    for row in plane.chunks_mut(w * bytes) {
        match bytes {
            2 => {
                let mut prev = 0u16;
                for s in row.chunks_mut(2) {
                    let v = prev.wrapping_add(u16::from_be_bytes([s[0], s[1]]));
                    s.copy_from_slice(&v.to_be_bytes());
                    prev = v;
                }
            }
            _ => {
                for i in 1..row.len() {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
            }
        }
    }
}

/// A 16-bit plane cut to 8 bits (rounded).
fn plane_to_8(plane: &[u8]) -> Vec<u8> {
    plane
        .chunks(2)
        .map(|s| {
            let v = u16::from_be_bytes([s[0], *s.get(1).unwrap_or(&0)]) as u32;
            ((v * 255 + 32767) / 65535) as u8
        })
        .collect()
}

/// Pixels from 16-bit channel planes: rounded to 8 bits, and at full depth
/// (premultiplied linear light).
fn from_planes16(rgb: [&[u8]; 3], alpha: Option<&[u8]>, n: usize) -> (Vec<Color32>, Vec<[f32; 4]>) {
    use eframe::egui::ecolor::linear_from_gamma;
    let mut deep = DeepTile::transparent(Depth::U16, n).expect("a deep depth");
    let mut linear = Vec::with_capacity(n);
    for i in 0..n {
        let at = |p: &[u8]| {
            p.get(2 * i..2 * i + 2)
                .map_or(0, |s| u16::from_be_bytes([s[0], s[1]]))
        };
        let a = alpha.map_or(65535, at) as f32 / 65535.0;
        let c = |p: &[u8]| linear_from_gamma(at(p) as f32 / 65535.0) * a;
        let px = [c(rgb[0]), c(rgb[1]), c(rgb[2]), a];
        deep.set_linear(i, px);
        linear.push(deep.linear(i));
    }
    (deep.narrow_all(), linear)
}

/// Premultiplied pixels from channel planes (gray documents: one plane).
fn from_planes(rgb: [&[u8]; 3], alpha: Option<&[u8]>, n: usize) -> Vec<Color32> {
    (0..n)
        .map(|i| {
            let at = |p: &[u8]| p.get(i).copied().unwrap_or(0);
            let a = alpha.map_or(255, at);
            Color32::from_rgba_unmultiplied(at(rgb[0]), at(rgb[1]), at(rgb[2]), a)
        })
        .collect()
}

pub fn decode_psd(bytes: &[u8]) -> Result<PsdDocument, String> {
    let mut r = In { b: bytes, pos: 0 };
    if r.take(4)? != b"8BPS" {
        return Err("Not a Photoshop file".into());
    }
    match r.u16()? {
        1 => {}
        2 => return Err("Large documents (.psb) aren't supported".into()),
        _ => return Err("Unknown Photoshop file version".into()),
    }
    r.take(6)?;
    let channels = r.u16()? as usize;
    let height = r.u32()? as usize;
    let width = r.u32()? as usize;
    let depth = r.u16()?;
    let mode = r.u16()?;
    let bytes = match depth {
        8 => 1,
        16 => 2,
        _ => {
            return Err(format!(
                "{depth}-bit PSD files aren't supported (8 and 16-bit only)"
            ));
        }
    };
    let gray = match mode {
        3 => false,
        1 => true,
        _ => return Err("Only RGB and grayscale PSD files are supported".into()),
    };
    crate::app::document::validate_canvas_size(width, height)?;
    r.block()?; // colour mode data
    let profile = read_profile(r.block()?);
    let mut section = r.block()?;
    let mut layers = Vec::new();
    if !section.b.is_empty() {
        let mut info = section.block()?;
        if !info.b.is_empty() {
            layers = read_layers(&mut info, gray, bytes)?;
        } else if bytes == 2 {
            // Photoshop keeps a 16-bit file's layers in a tagged block
            // after the global mask instead.
            section.block()?;
            while section.b.len() - section.pos >= 12 {
                let sig = section.take(4)?;
                if sig != b"8BIM" && sig != b"8B64" {
                    break;
                }
                let key = section.take(4)?;
                let mut data = section.block()?;
                if key == b"Lr16" && !data.b.is_empty() {
                    layers = read_layers(&mut data, gray, bytes)?;
                    break;
                }
            }
        }
    }
    // The flattened picture.
    let composite =
        read_composite(&mut r, width, height, channels, gray, bytes).unwrap_or_default();
    let composite = if composite.len() == width * height {
        composite
    } else {
        vec![Color32::TRANSPARENT; width * height]
    };
    if layers.is_empty() {
        // No layers: the picture is the one layer.
        layers.push(PsdLayer {
            deep: None,
            name: "Layer 1".into(),
            kind: PsdKind::Pixels,
            rect: [0, 0, width as i32, height as i32],
            pixels: composite.clone(),
            opacity: 1.0,
            visible: true,
            blend: LayerBlend::Normal,
            clipped: false,
            alpha_locked: false,
            position_locked: false,
            mask: None,
        });
    }
    Ok(PsdDocument {
        depth: if bytes == 2 { Depth::U16 } else { Depth::U8 },
        composite_deep: None,
        profile,
        width,
        height,
        layers,
        composite,
    })
}

/// The colour profile among the image resources (1039, an ICC profile);
/// sRGB if there's none, or it's damaged or not RGB.
fn read_profile(mut r: In<'_>) -> crate::canvas::color_profile::ColorProfile {
    use crate::canvas::color_profile::ColorProfile;
    let mut next = || -> Result<Option<(u16, &[u8])>, String> {
        if r.b.len() - r.pos < 12 || r.take(4)? != b"8BIM" {
            return Ok(None);
        }
        let id = r.u16()?;
        let name_len = r.u8()? as usize;
        // The name and its length byte, padded to even.
        r.take(name_len + (name_len + 1) % 2)?;
        let len = r.u32()? as usize;
        let data = r.take(len)?;
        if len % 2 == 1 {
            r.take(1).ok();
        }
        Ok(Some((id, data)))
    };
    while let Ok(Some((id, data))) = next() {
        if id == 1039 {
            return ColorProfile::from_icc(data.to_vec(), "Photoshop profile").unwrap_or_default();
        }
    }
    ColorProfile::Srgb
}

/// A layer record, before its channel data is read.
struct Record {
    layer: PsdLayer,
    /// Channel id and compressed length, in file order.
    channels: Vec<(i16, usize)>,
}

fn read_layers(r: &mut In<'_>, gray: bool, bytes: usize) -> Result<Vec<PsdLayer>, String> {
    let count = r.i16()?.unsigned_abs() as usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let top = r.i32()?;
        let left = r.i32()?;
        let bottom = r.i32()?;
        let right = r.i32()?;
        let n = r.u16()? as usize;
        let channels = (0..n)
            .map(|_| Ok((r.i16()?, r.u32()? as usize)))
            .collect::<Result<Vec<_>, String>>()?;
        if r.take(4)? != b"8BIM" {
            return Err("Damaged PSD layer record".into());
        }
        let blend = blend_from_key(r.take(4)?);
        let opacity = r.u8()? as f32 / 255.0;
        let clipped = r.u8()? != 0;
        let flags = r.u8()?;
        r.u8()?;
        let mut extra = r.block()?;
        let mut mask_block = extra.block()?;
        let mask = if mask_block.b.len() >= 18 {
            let (t, l, b, rt) = (
                mask_block.i32()?,
                mask_block.i32()?,
                mask_block.i32()?,
                mask_block.i32()?,
            );
            let default = mask_block.u8()?;
            let mask_flags = mask_block.u8()?;
            // Bit 1: the mask is disabled.
            (mask_flags & 2 == 0).then_some(PsdMask {
                rect: [l, t, rt, b],
                data: Vec::new(),
                default,
            })
        } else {
            None
        };
        extra.block()?; // blending ranges
        let name_len = extra.u8()? as usize;
        let mut name = String::from_utf8_lossy(extra.take(name_len)?).into_owned();
        let padded = (name_len + 1).div_ceil(4) * 4;
        extra.take(padded - name_len - 1)?;
        let mut kind = PsdKind::Pixels;
        let mut group_blend = None;
        let mut protection = 0u32;
        while extra.b.len() - extra.pos >= 12 {
            let sig = extra.take(4)?;
            if sig != b"8BIM" && sig != b"8B64" {
                break;
            }
            let key = extra.take(4)?;
            let mut data = extra.block()?;
            match key {
                b"luni" => {
                    let n = data.u32()? as usize;
                    let units = (0..n).map(|_| data.u16()).collect::<Result<Vec<_>, _>>()?;
                    name = String::from_utf16_lossy(&units)
                        .trim_end_matches('\0')
                        .to_string();
                }
                b"lsct" | b"lsdk" => {
                    kind = match data.u32()? {
                        1 | 2 => PsdKind::GroupStart,
                        3 => PsdKind::GroupEnd,
                        _ => PsdKind::Pixels,
                    };
                    if data.b.len() >= 12 {
                        data.take(4)?;
                        group_blend = Some(blend_from_key(data.take(4)?));
                    }
                }
                b"lspf" => protection = data.u32()?,
                _ => {}
            }
        }
        records.push(Record {
            layer: PsdLayer {
                deep: None,
                name,
                kind,
                rect: [left, top, right, bottom],
                pixels: Vec::new(),
                opacity,
                visible: flags & 2 == 0,
                blend: group_blend.unwrap_or(blend),
                clipped,
                alpha_locked: flags & 1 != 0 || protection & 1 != 0,
                position_locked: protection & 4 != 0,
                mask,
            },
            channels,
        });
    }
    // Channel data, layer by layer in the same order.
    let mut layers = Vec::with_capacity(records.len());
    for Record {
        mut layer,
        channels,
    } in records
    {
        let [x0, y0, x1, y1] = layer.rect;
        let (w, h) = (span(x0, x1)?, span(y0, y1)?);
        if w * h > crate::app::document::MAX_CANVAS_PIXELS
            || (bytes == 2 && w * h > crate::project::kra::MAX_DEEP_PIXELS)
        {
            return Err("Damaged PSD layer bounds".into());
        }
        if let Some(m) = &layer.mask {
            span(m.rect[0], m.rect[2])?;
            span(m.rect[1], m.rect[3])?;
        }
        let mut planes: [Option<Vec<u8>>; 4] = Default::default();
        for (id, len) in channels {
            let data = r.take(len)?;
            match id {
                -1 => planes[3] = Some(read_plane(data, w, h, bytes)?),
                0..=2 => planes[id as usize] = Some(read_plane(data, w, h, bytes)?),
                -2 => {
                    if let Some(m) = layer.mask.as_mut() {
                        let mw = (m.rect[2] - m.rect[0]).max(0) as usize;
                        let mh = (m.rect[3] - m.rect[1]).max(0) as usize;
                        m.data = read_plane(data, mw, mh, bytes)?;
                        if bytes == 2 {
                            m.data = plane_to_8(&m.data);
                        }
                        if m.data.len() != mw * mh {
                            m.data = vec![255; mw * mh];
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(m) = &layer.mask
            && m.data.len()
                != ((m.rect[2] - m.rect[0]).max(0) * (m.rect[3] - m.rect[1]).max(0)) as usize
        {
            layer.mask = None;
        }
        let empty = Vec::new();
        let plane = |i: usize| planes[i].as_deref().unwrap_or(&empty);
        let rgb = if gray {
            [plane(0); 3]
        } else {
            [plane(0), plane(1), plane(2)]
        };
        if bytes == 2 {
            let (pixels, deep) = from_planes16(rgb, planes[3].as_deref(), w * h);
            layer.pixels = pixels;
            layer.deep = Some(deep);
        } else {
            layer.pixels = from_planes(rgb, planes[3].as_deref(), w * h);
        }
        layers.push(layer);
    }
    Ok(layers)
}

/// A layer's width or height from its edges, refusing absurd ones (a
/// damaged file must not allocate gigabytes).
fn span(from: i32, to: i32) -> Result<usize, String> {
    let n = (to as i64 - from as i64).max(0) as usize;
    if n > 2 * MAX_EDGE {
        return Err("Damaged PSD layer bounds".into());
    }
    Ok(n)
}

fn read_composite(
    r: &mut In<'_>,
    w: usize,
    h: usize,
    channels: usize,
    gray: bool,
    bytes: usize,
) -> Result<Vec<Color32>, String> {
    let compression = r.u16()?;
    let n = w * h;
    let mut planes = Vec::with_capacity(channels);
    match compression {
        0 => {
            for _ in 0..channels {
                planes.push(r.take(n * bytes)?.to_vec());
            }
        }
        1 => {
            let counts: Vec<usize> = (0..channels * h)
                .map(|_| r.u16().map(|c| c as usize))
                .collect::<Result<_, _>>()?;
            for c in 0..channels {
                let mut plane = Vec::with_capacity(n * bytes);
                for &count in &counts[c * h..(c + 1) * h] {
                    plane.extend(unpackbits(r.take(count)?, w * bytes));
                }
                planes.push(plane);
            }
        }
        _ => return Err("Unsupported composite compression".into()),
    }
    if bytes == 2 {
        planes = planes.iter().map(|p| plane_to_8(p)).collect();
    }
    let color = if gray { 1 } else { 3 };
    if planes.len() < color {
        return Err("The PSD has too few channels".into());
    }
    let rgb = if gray {
        [planes[0].as_slice(); 3]
    } else {
        [
            planes[0].as_slice(),
            planes[1].as_slice(),
            planes[2].as_slice(),
        ]
    };
    Ok(from_planes(rgb, planes.get(color).map(|p| p.as_slice()), n))
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_16_bit_psd_opens_at_16_bits() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/krita-layers-16bit.psd"
        );
        let doc = decode_psd(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(doc.depth, Depth::U16);
        let red = doc.layers.iter().find(|l| l.name == "Red square").unwrap();
        let deep = red.deep.as_ref().unwrap();
        assert_eq!(deep.len(), red.pixels.len());
        let [x0, y0, x1, _] = red.rect;
        let i = ((20 - y0) * (x1 - x0) + (20 - x0)) as usize;
        assert_eq!(red.pixels[i], Color32::RED);
        assert_eq!(deep[i], [1.0, 0.0, 0.0, 1.0]);
        let krita = doc.composite.clone();
        let canvas = doc.into_canvas().unwrap();
        assert_eq!(canvas.depth(), Depth::U16);
        // It looks as Krita flattened it.
        let ours = canvas.flatten_final();
        let worst = (ours.pixels.iter().zip(&krita))
            .flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c])))
            .max()
            .unwrap();
        assert!(worst <= 3, "off by {worst}");
        let idx = canvas
            .layers
            .iter()
            .position(|l| l.name == "Red square")
            .unwrap();
        assert_eq!(
            canvas
                .get_layer_tile_deep(idx, 0, 0)
                .unwrap()
                .linear(20 * 64 + 20),
            [1.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn a_deep_document_writes_a_16_bit_psd_that_reads_back() {
        let mut canvas = Canvas::new(100, 70, Color32::WHITE, 64);
        canvas.convert_depth(Depth::F32);
        // A dark ramp (too fine for 8 bits) at half alpha, and a mask.
        let mut deep = DeepTile::transparent(Depth::F32, 64 * 64).unwrap();
        for i in 0..deep.len() {
            let v = (i % 64) as f32 / 64.0 * 0.02;
            deep.set_linear(i, [v * 0.5, v * 0.25, v * 0.5, 0.5]);
        }
        canvas.set_layer_tile_deep(1, 0, 0, &deep);
        let bytes = encode_psd(&PsdDocument::from_canvas(&canvas)).unwrap();
        assert_eq!(&bytes[22..24], &16u16.to_be_bytes(), "a 16-bit file");
        let back = decode_psd(&bytes).unwrap().into_canvas().unwrap();
        assert_eq!(back.depth(), Depth::U16);
        let idx = back
            .layers
            .iter()
            .position(|l| l.name == "Layer 1")
            .unwrap();
        let got = back.get_layer_tile_deep(idx, 0, 0).unwrap();
        for i in [0, 5, 63, 64 * 10 + 40] {
            let (a, b) = (got.linear(i), deep.linear(i));
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 2e-4, "pixel {i}: {a:?} vs {b:?}");
            }
        }
        // The steps 8 bits would merge are still apart.
        let mut steps: Vec<u32> = (0..64).map(|x| (got.linear(x)[0] * 1e7) as u32).collect();
        steps.dedup();
        assert!(steps.len() > 50, "{} steps", steps.len());
    }

    #[test]
    fn the_colour_profile_travels_through_psd() {
        use crate::canvas::color_profile::ColorProfile;
        let mut canvas = Canvas::new(40, 30, Color32::WHITE, 64);
        canvas.profile = ColorProfile::AdobeRgb;
        let bytes = encode_psd(&PsdDocument::from_canvas(&canvas)).unwrap();
        let doc = decode_psd(&bytes).unwrap();
        assert_eq!(doc.profile.label(), "Adobe RGB (1998)");
        assert_eq!(
            doc.into_canvas().unwrap().profile.label(),
            "Adobe RGB (1998)"
        );
        // sRGB writes no profile, and reads back as sRGB.
        canvas.profile = ColorProfile::Srgb;
        let plain = encode_psd(&PsdDocument::from_canvas(&canvas)).unwrap();
        assert!(plain.len() < bytes.len());
        assert_eq!(decode_psd(&plain).unwrap().profile, ColorProfile::Srgb);
        // Krita's 16-bit documents are linear sRGB (gamma 1.0): kept as such.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/krita-layers-16bit.psd"
        );
        let krita = decode_psd(&std::fs::read(path).unwrap()).unwrap().profile;
        assert_eq!(krita.label(), "sRGB-elle-V2-g10.icc");
    }

    #[test]
    fn zip_rows_with_prediction_come_back() {
        // Two 16-bit samples a row, stored as differences, zlib-packed.
        let raw: Vec<u8> = [100u16, 5, 7, 1]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let mut data = 3u16.to_be_bytes().to_vec();
        data.extend(miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
        let plane = read_plane(&data, 2, 2, 2).unwrap();
        let values: Vec<u16> = plane
            .chunks(2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]))
            .collect();
        assert_eq!(values, [100, 105, 7, 8]);
        // 8-bit ZIP without prediction.
        let mut data = 2u16.to_be_bytes().to_vec();
        data.extend(miniz_oxide::deflate::compress_to_vec_zlib(&[1, 2, 3, 4], 6));
        assert_eq!(read_plane(&data, 2, 2, 1).unwrap(), [1, 2, 3, 4]);
    }
    use super::*;

    #[test]
    fn packbits_round_trips_runs_and_literals() {
        let rows: [Vec<u8>; 4] = [
            vec![],
            vec![7],
            (0..300).map(|i| (i / 7) as u8).collect(),
            (0..500).map(|i| (i * 31 % 251) as u8).collect(),
        ];
        for row in rows {
            let mut enc = Vec::new();
            packbits(&row, &mut enc);
            assert_eq!(unpackbits(&enc, row.len()), row);
        }
    }

    /// A document with a background, a folder holding a clipped multiply
    /// layer over a masked one, and a hidden layer.
    fn document() -> Canvas {
        let mut canvas = Canvas::new(100, 70, Color32::from_rgb(240, 230, 220), 64);
        canvas.layers[1].name = "Base ✓".into();
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        for p in tile.iter_mut().take(64 * 20) {
            *p = Color32::from_rgba_unmultiplied(200, 40, 40, 255);
        }
        canvas.set_layer_tile_data(1, 1, 0, tile);
        let folder = canvas.insert_new_layer(2, "Folder".into(), LayerKind::Group, None);
        canvas.layers[1].parent = Some(folder);
        let shade = canvas.insert_new_layer(2, "Shade".into(), LayerKind::Paint, Some(folder));
        let si = canvas.layer_index_of(shade).unwrap();
        canvas.set_layer_tile_data(si, 1, 0, vec![Color32::from_gray(128); 64 * 64]);
        canvas.layers[si].blend = LayerBlend::Multiply;
        canvas.layers[si].clipped = true;
        canvas.layers[si].opacity = 0.5;
        let base_id = canvas.layers[1].id;
        let m = canvas.insert_new_layer(4, "m".into(), LayerKind::Mask { owner: base_id }, None);
        let mi = canvas.layer_index_of(m).unwrap();
        canvas.set_layer_tile_data(mi, 1, 0, vec![Color32::BLACK; 64 * 64]);
        let hidden = canvas.insert_new_layer(5, "Hidden".into(), LayerKind::Paint, None);
        let hi = canvas.layer_index_of(hidden).unwrap();
        canvas.set_layer_tile_data(hi, 0, 0, vec![Color32::BLUE; 64 * 64]);
        canvas.layers[hi].visible = false;
        canvas.layers[hi].alpha_locked = true;
        canvas.layers[hi].position_locked = true;
        canvas
    }

    #[test]
    fn a_document_round_trips_through_psd() {
        let canvas = document();
        let bytes = encode_psd(&PsdDocument::from_canvas(&canvas)).unwrap();
        let back = decode_psd(&bytes).unwrap().into_canvas().unwrap();
        assert_eq!((back.width(), back.height()), (100, 70));
        let find = |name: &str| back.layers.iter().position(|l| l.name == name).unwrap();
        let folder = &back.layers[find("Folder")];
        assert_eq!(folder.kind, LayerKind::Group);
        let base = &back.layers[find("Base ✓")];
        assert_eq!(base.parent, Some(folder.id), "the unicode name and folder");
        let shade = &back.layers[find("Shade")];
        assert_eq!(shade.parent, Some(folder.id));
        assert!(shade.clipped);
        assert_eq!(shade.blend, LayerBlend::Multiply);
        assert!((shade.opacity - 0.5).abs() < 0.01);
        assert!(find("Base ✓") < find("Shade"), "stacking order kept");
        let hidden = &back.layers[find("Hidden")];
        assert!(!hidden.visible && hidden.alpha_locked && hidden.position_locked);
        assert!(!base.position_locked);
        assert!(back.mask_index_of(base.id).is_some(), "the mask came along");
        // The pictures match.
        let (a, b) = (canvas.flatten(), back.flatten());
        let worst = a
            .pixels
            .iter()
            .zip(&b.pixels)
            .map(|(x, y)| {
                x.to_array()
                    .iter()
                    .zip(y.to_array())
                    .map(|(p, q)| p.abs_diff(q))
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst <= 2, "off by {worst}");
    }

    #[test]
    fn damaged_files_are_refused_not_panicked_on() {
        let bytes = encode_psd(&PsdDocument::from_canvas(&document())).unwrap();
        for cut in [
            0,
            3,
            10,
            30,
            60,
            bytes.len() / 3,
            bytes.len() / 2,
            bytes.len() - 1,
        ] {
            let _ = decode_psd(&bytes[..cut]).and_then(|d| d.into_canvas());
        }
        let mut junk = bytes.clone();
        for i in (26..junk.len()).step_by(97) {
            junk[i] ^= 0x5a;
        }
        let _ = decode_psd(&junk).and_then(|d| d.into_canvas());
    }

    #[test]
    #[ignore = "writes a file; set RUSTY_PAINTER_PSD_OUT"]
    fn write_sample_psd() {
        let path = std::env::var("RUSTY_PAINTER_PSD_OUT").expect("RUSTY_PAINTER_PSD_OUT");
        let bytes = encode_psd(&PsdDocument::from_canvas(&document())).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    #[ignore = "reads files; set RUSTY_PAINTER_PSD_IN to paths separated by ':'"]
    fn read_psd_files() {
        let paths = std::env::var("RUSTY_PAINTER_PSD_IN").expect("RUSTY_PAINTER_PSD_IN");
        for path in paths.split(':') {
            let bytes = std::fs::read(path).unwrap();
            match decode_psd(&bytes).and_then(|d| {
                let names: Vec<String> = d
                    .layers
                    .iter()
                    .map(|l| format!("{:?}:{}", l.kind, l.name))
                    .collect();
                let n = names.len();
                d.into_canvas().map(|c| (c, n, names))
            }) {
                Ok((c, n, names)) => {
                    if let Ok(dir) = std::env::var("RUSTY_PAINTER_PSD_PNG") {
                        let name = std::path::Path::new(path)
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        let img = crate::project::export::to_rgba_image(c.flatten()).unwrap();
                        img.save(format!("{dir}/{name}.png")).unwrap();
                    }
                    println!(
                        "OK {path}: {}x{}, {n} records -> {} layers; {:?}",
                        c.width(),
                        c.height(),
                        c.layers.len(),
                        &names[..names.len().min(8)]
                    )
                }
                Err(e) => println!("ERR {path}: {e}"),
            }
        }
    }

    #[test]
    fn the_app_opens_a_psd_as_the_document() {
        let path = std::env::temp_dir().join(format!("rp-open-{}.psd", std::process::id()));
        std::fs::write(
            &path,
            encode_psd(&PsdDocument::from_canvas(&document())).unwrap(),
        )
        .unwrap();
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.load_project_from_path(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((app.canvas.width(), app.canvas.height()), (100, 70));
        assert!(app.canvas.layers.iter().any(|l| l.name == "Shade"));
        assert_eq!(app.render_cache.tiles_x, 2);
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_psd() {
        let seed = encode_psd(&PsdDocument::from_canvas(&document())).unwrap();
        crate::fuzz::fuzz("psd", &seed, std::time::Duration::from_secs(2), |b| {
            if let Ok(doc) = decode_psd(b)
                && let Ok(canvas) = doc.into_canvas()
            {
                canvas.flatten();
            }
        });
    }
}
