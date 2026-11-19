//! Photoshop documents (`.psd`, 8 or 16-bit RGB or grayscale), read and
//! written: raster layers with their name, opacity, visibility, blend mode,
//! clipping, locked transparency and layer mask, and folders. Pixels are
//! RLE-compressed (PackBits) as Photoshop writes them (ZIP-compressed ones
//! are read too). A 16-bit file opens as a 16-bit document, and a deeper
//! document is written at 16 bits. Our text layers are written as their
//! pixels.
//!
//! Text and smart-object layers come in as the pixels Photoshop stored for
//! them, a solid colour fill layer as a colour fill layer. Adjustment and
//! gradient or pattern fill layers, layer effects and vector masks have no
//! form here: when a file has any, its flattened picture is added as a
//! hidden top layer so the look isn't lost. 32-bit, CMYK and large
//! (`.psb`) documents are refused. Our fill layers and layer styles
//! (border, gradient overlay, impasto) are written as the pixels they show.

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
    /// The flattened picture, premultiplied (empty when not read: a
    /// layered file's is only read when its layers don't show everything).
    pub composite: Vec<Color32>,
    /// Bits for each channel the layers' pixels came with (see
    /// [`PsdLayer::deep`]).
    pub depth: Depth,
    /// In a deeper document, the flattened picture at full depth
    /// (premultiplied linear light), for writing a 16-bit file.
    pub composite_deep: Option<Vec<[f32; 4]>>,
    /// Which colours the pixels are (the file's embedded ICC profile).
    pub profile: crate::canvas::color_profile::ColorProfile,
    /// What the user should hear about the import (parts left out).
    pub notes: Vec<String>,
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
    /// A vector layer's lines and shapes (its pixels drawn from them).
    pub vector: Option<Box<crate::canvas::vector::VectorLayer>>,
    /// A fill layer's colour or gradient (it shows everywhere its mask lets it).
    pub fill: Option<crate::canvas::layer_style::LayerFill>,
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
        PassThrough => b"pass",
    }
}

/// Keys with no counterpart read as Normal.
fn blend_from_key(key: &[u8]) -> LayerBlend {
    (LayerBlend::GROUPS.iter().flat_map(|g| g.iter()).copied())
        .chain([LayerBlend::PassThrough])
        .find(|&b| blend_key(b) == key)
        .unwrap_or(LayerBlend::Normal)
}

// ---------------------------------------------------------------- canvas

impl PsdDocument {
    /// Everything PSD can hold of `canvas`.
    pub fn from_canvas(canvas: &Canvas) -> Self {
        // Moved layers as they show (PSD has no keys).
        let shown = canvas.shown_copy();
        let canvas = shown.as_ref().unwrap_or(canvas);
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
            notes: Vec::new(),
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
            figure: None,
            position_locked: false,
            draft: false,
            reference: false,
            anim: None,
            rig: None,
            warps: Vec::new(),
            motion: None,
            colorize: None,
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
                // Only folders pass through.
                blend: match layer.blend {
                    LayerBlend::PassThrough if !is_group => LayerBlend::Normal,
                    blend => blend,
                },
                clipped: layer.clipped,
                adjustment: None,
                style: crate::canvas::layer_style::LayerStyle {
                    fill: layer.fill.filter(|_| !is_group),
                    ..Default::default()
                },
                text: None,
                vector: layer.vector.filter(|_| !is_group),
                height: None,
                shader: None,
                figure: None,
                position_locked: layer.position_locked,
                draft: false,
                reference: false,
                anim: None,
                rig: None,
                warps: Vec::new(),
                motion: None,
                colorize: None,
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
                    figure: None,
                    position_locked: false,
                    draft: false,
                    reference: false,
                    anim: None,
                    rig: None,
                    warps: Vec::new(),
                    motion: None,
                    colorize: None,
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
        // Adjustment layers have no PSD form here: the composite shows them.
        if layer.parent != parent
            || matches!(layer.kind, LayerKind::Mask { .. })
            || layer.adjustment.is_some()
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
            vector: None,
            fill: None,
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
                vector: None,
                fill: None,
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
        // A fill layer, or a border, gradient overlay or impasto: written as
        // the pixels it shows (Photoshop's own effects aren't written).
        let styled = !layer.shown_style().is_plain();
        let (rect, pixels) = if i == 0 {
            let r = [0, 0, canvas.width() as i32, canvas.height() as i32];
            (r, layer_area(canvas, i, r, fill, styled))
        } else {
            match content_rect(canvas, i, styled) {
                Some(r) => (r, layer_area(canvas, i, r, fill, styled)),
                None => ([0; 4], Vec::new()),
            }
        };
        let mask = canvas.mask_index_of(layer.id).map(|m| {
            let rect = [0, 0, canvas.width() as i32, canvas.height() as i32];
            let data = layer_area(canvas, m, rect, Color32::WHITE, false)
                .iter()
                .map(|&p| ((p.r() as u32 + p.g() as u32 + p.b() as u32) / 3) as u8)
                .collect();
            PsdMask {
                rect,
                data,
                default: 255,
            }
        });
        // (A styled layer's look is made at 8 bits.)
        let deep = (canvas.depth().is_deep() && rect[2] > rect[0] && !styled)
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

/// Layer `idx`'s tile `(tx, ty)`: as painted, or `styled`, as it shows
/// (its fill, border, overlay or impasto made into pixels).
fn tile_pixels(
    canvas: &Canvas,
    idx: usize,
    tx: i32,
    ty: i32,
    styled: bool,
) -> Option<Vec<Color32>> {
    if styled {
        canvas.styled_tile(idx, tx, ty).map(|p| p.to_vec())
    } else {
        canvas.get_layer_tile_data(idx, tx, ty)
    }
}

/// The painted tiles' extent (`styled`: the tiles that show anything),
/// within the canvas.
fn content_rect(canvas: &Canvas, idx: usize, styled: bool) -> Option<[i32; 4]> {
    let ts = canvas.tile_size() as i32;
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let mut r: Option<[i32; 4]> = None;
    // A fill or a border shows where nothing is painted: every tile is asked.
    let keys = if styled {
        let (tw, th) = ((w + ts - 1) / ts, (h + ts - 1) / ts);
        (0..th)
            .flat_map(|ty| (0..tw).map(move |tx| (tx, ty)))
            .collect()
    } else {
        canvas.layer_tile_keys(idx)
    };
    for (tx, ty) in keys {
        let painted =
            tile_pixels(canvas, idx, tx, ty, styled).is_some_and(|d| d.iter().any(|p| p.a() > 0));
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

/// Layer `idx`'s pixels over `rect` (`styled`: as it shows, see
/// [`tile_pixels`]); unpainted areas read as `fill`.
fn layer_area(
    canvas: &Canvas,
    idx: usize,
    rect: [i32; 4],
    fill: Color32,
    styled: bool,
) -> Vec<Color32> {
    let ts = canvas.tile_size() as i32;
    let [x0, y0, x1, y1] = rect;
    let w = (x1 - x0) as usize;
    let mut out = vec![fill; w * (y1 - y0) as usize];
    for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let Some(data) = tile_pixels(canvas, idx, tx, ty, styled) else {
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
        tile.data = std::sync::Arc::new(out.narrow_all());
        tile.deep = Some(std::sync::Arc::new(out));
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
                    data: data.into(),
                    deep: None,
                });
            }
        }
    }
    tiles
}

/// A mask as whole-canvas tiles (white = shows, the default outside its
/// rect), leaving out all-white tiles (a missing mask tile shows).
/// Each tile is made by itself: no canvas-sized picture of the mask.
/// (Past the canvas's edge a tile is transparent, as a painted one is.)
fn mask_tiles(mask: &PsdMask, w: usize, h: usize, ts: usize) -> Vec<CanvasTileSnapshot> {
    let [x0, y0, x1, y1] = mask.rect;
    let mw = (x1 - x0).max(0) as usize;
    if mask.data.len() != mw * (y1 - y0).max(0) as usize {
        return Vec::new();
    }
    let grey = |v: u8| Color32::from_rgb(v, v, v);
    let (t, w, h) = (ts as i32, w as i32, h as i32);
    let mut tiles = Vec::new();
    for ty in 0..(h + t - 1) / t {
        for tx in 0..(w + t - 1) / t {
            let (ox, oy) = (tx * t, ty * t);
            let (ex, ey) = ((ox + t).min(w), (oy + t).min(h));
            let whole = ex - ox == t && ey - oy == t;
            let touches = x0 < ex && x1 > ox && y0 < ey && y1 > oy;
            if whole && !touches && mask.default == 255 {
                continue;
            }
            let mut data = vec![Color32::TRANSPARENT; ts * ts];
            for y in oy..ey {
                let row = &mut data[((y - oy) * t) as usize..][..(ex - ox) as usize];
                for (x, p) in (ox..ex).zip(row) {
                    *p = if x >= x0 && x < x1 && y >= y0 && y < y1 {
                        grey(mask.data[(y - y0) as usize * mw + (x - x0) as usize])
                    } else {
                        grey(mask.default)
                    };
                }
            }
            if data.iter().any(|&p| p != Color32::WHITE) {
                tiles.push(CanvasTileSnapshot {
                    tx,
                    ty,
                    data: data.into(),
                    deep: None,
                });
            }
        }
    }
    tiles
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

/// How much bigger than its compressed bytes a channel can honestly come
/// out: PackBits turns 2 bytes into at most 128, zlib 1 into about 1032.
const RLE_RATIO: usize = 128;
const ZIP_RATIO: usize = 1100;

/// Bytes of pixels decoding may still make. A damaged or hostile file must
/// not turn kilobytes into gigabytes: every layer can claim the whole
/// canvas with its channels missing, or as rows that are all padding.
struct Budget(usize);

impl Budget {
    /// What a `len`-byte file could hold at best (pixels as 8-bit planes,
    /// then as our pixels at full depth), with room for small files.
    fn for_file(len: usize) -> Self {
        Budget(len.saturating_mul(3 * ZIP_RATIO).max(256 << 20))
    }

    fn spend(&mut self, n: usize) -> Result<(), String> {
        self.0 = (self.0.checked_sub(n))
            .ok_or("The PSD's layers are bigger than its data could hold (a damaged file?)")?;
        Ok(())
    }
}

/// `rect` cut to a `w`×`h` canvas (empty: all zeros), and the same area in
/// `rect`'s own pixels.
fn clip_to_canvas(rect: [i32; 4], w: usize, h: usize) -> ([i32; 4], [usize; 4]) {
    let r = [
        rect[0].max(0),
        rect[1].max(0),
        rect[2].min(w as i32),
        rect[3].min(h as i32),
    ];
    if r[2] <= r[0] || r[3] <= r[1] {
        return ([0; 4], [0; 4]);
    }
    let local = |v: i32, from: i32| (v as i64 - from as i64) as usize;
    (
        r,
        [
            local(r[0], rect[0]),
            local(r[1], rect[1]),
            local(r[2], rect[0]),
            local(r[3], rect[1]),
        ],
    )
}

/// A channel's plane of `bytes`-byte samples (big endian), `w`×`h` in the
/// file but kept only over `crop` (`[x0, y0, x1, y1]` in its own pixels):
/// its compression, then raw, RLE or ZIP rows. Data too short to hold that
/// many pixels is refused rather than padded out.
fn read_plane(
    data: &[u8],
    w: usize,
    h: usize,
    bytes: usize,
    crop: [usize; 4],
) -> Result<Vec<u8>, String> {
    let [cx0, cy0, cx1, cy1] = crop;
    if w == 0 || h == 0 || cx1 <= cx0 || cy1 <= cy0 || cx1 > w || cy1 > h {
        return Ok(Vec::new());
    }
    let short = || "The PSD's pixel data is cut short".to_string();
    let row = w * bytes;
    let len = row * h;
    let (from, to) = (cx0 * bytes, cx1 * bytes);
    let kept = (to - from) * (cy1 - cy0);
    let crop_rows = |plane: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(kept);
        for y in cy0..cy1 {
            out.extend_from_slice(&plane[y * row + from..y * row + to]);
        }
        out
    };
    let mut r = In { b: data, pos: 0 };
    match r.u16()? {
        0 => Ok(crop_rows(r.take(len).map_err(|_| short())?)),
        1 => {
            if kept > data.len().saturating_mul(RLE_RATIO) {
                return Err(short());
            }
            let counts: Vec<usize> = (0..h)
                .map(|_| r.u16().map(|c| c as usize))
                .collect::<Result<_, _>>()?;
            let mut out = Vec::with_capacity(kept);
            for (y, c) in counts.into_iter().enumerate().take(cy1) {
                let src = r.take(c)?;
                if y >= cy0 {
                    out.extend_from_slice(&unpackbits(src, to)[from..]);
                }
            }
            Ok(out)
        }
        // ZIP, and ZIP with each row stored as differences.
        compression @ (2 | 3) => {
            if len > data.len().saturating_mul(ZIP_RATIO) {
                return Err(short());
            }
            let mut plane =
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data[2..], len)
                    .map_err(|_| "Damaged ZIP data in the PSD")?;
            plane.resize(len, 0);
            if compression == 3 {
                undo_prediction(&mut plane, w, bytes);
            }
            Ok(if kept == len {
                plane
            } else {
                crop_rows(&plane)
            })
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
    decode(bytes, false)
}

/// [`decode_psd`]; `want_composite`: read the flattened picture even when
/// the layers show everything.
fn decode(file: &[u8], want_composite: bool) -> Result<PsdDocument, String> {
    let mut r = In { b: file, pos: 0 };
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
    let mut budget = Budget::for_file(file.len());
    let format = Format {
        gray,
        bytes,
        size: [width, height],
    };
    r.block()?; // colour mode data
    let profile = read_profile(r.block()?);
    let mut section = r.block()?;
    let (mut layers, mut dropped) = (Vec::new(), Dropped::default());
    if !section.b.is_empty() {
        let mut info = section.block()?;
        if !info.b.is_empty() {
            (layers, dropped) = read_layers(&mut info, format, &mut budget)?;
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
                    (layers, dropped) = read_layers(&mut data, format, &mut budget)?;
                    break;
                }
            }
        }
    }
    // The flattened picture: only when the layers don't show it all (it's
    // as big as the canvas, and slow to unpack).
    let mut composite = if layers.is_empty() || dropped.any() || want_composite {
        read_composite(&mut r, format, channels, &mut budget).unwrap_or_default()
    } else {
        Vec::new()
    };
    if composite.len() != width * height {
        composite = Vec::new();
    }
    let full = [0, 0, width as i32, height as i32];
    let mut notes = Vec::new();
    let flat_layer = |name: &str, pixels: Vec<Color32>| PsdLayer {
        name: name.into(),
        rect: if pixels.is_empty() { [0; 4] } else { full },
        pixels,
        ..empty_layer()
    };
    if layers.is_empty() {
        // No layers: the picture is the one layer.
        let pixels = if want_composite {
            composite.clone()
        } else {
            std::mem::take(&mut composite)
        };
        layers.push(flat_layer("Layer 1", pixels));
    } else if dropped.any() {
        notes.push(dropped.message(!composite.is_empty()));
        if !composite.is_empty() {
            let pixels = if want_composite {
                composite.clone()
            } else {
                std::mem::take(&mut composite)
            };
            let mut flat = flat_layer("Photoshop's picture (parts not brought across)", pixels);
            flat.visible = false;
            layers.push(flat);
        }
    }
    Ok(PsdDocument {
        depth: if bytes == 2 { Depth::U16 } else { Depth::U8 },
        composite_deep: None,
        profile,
        width,
        height,
        layers,
        composite,
        notes,
    })
}

/// A visible, normal pixel layer with nothing in it.
fn empty_layer() -> PsdLayer {
    PsdLayer {
        deep: None,
        name: String::new(),
        kind: PsdKind::Pixels,
        rect: [0; 4],
        pixels: Vec::new(),
        opacity: 1.0,
        visible: true,
        blend: LayerBlend::Normal,
        clipped: false,
        alpha_locked: false,
        position_locked: false,
        mask: None,
        vector: None,
        fill: None,
    }
}

/// How a file's pixels are stored.
#[derive(Clone, Copy)]
struct Format {
    gray: bool,
    /// Per sample: 1 or 2.
    bytes: usize,
    /// The canvas, `[width, height]`.
    size: [usize; 2],
}

/// What a file had that has no form here.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct Dropped {
    adjustment: bool,
    /// Gradient or pattern fill layers.
    fill: bool,
    effects: bool,
    vector_mask: bool,
}

impl Dropped {
    fn any(self) -> bool {
        self.adjustment || self.fill || self.effects || self.vector_mask
    }

    fn merge(&mut self, o: Dropped) {
        self.adjustment |= o.adjustment;
        self.fill |= o.fill;
        self.effects |= o.effects;
        self.vector_mask |= o.vector_mask;
    }

    /// A line for the user; `flattened`: the file's picture was added.
    fn message(self, flattened: bool) -> String {
        let what: Vec<&str> = [
            (self.adjustment, "adjustment layers"),
            (self.fill, "gradient or pattern fills"),
            (self.effects, "layer effects"),
            (self.vector_mask, "vector masks"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        let tail = if flattened {
            "its flattened picture was added as a hidden top layer"
        } else {
            "the file has no flattened picture to show them"
        };
        format!("The PSD's {} have no form here: {tail}", what.join(", "))
    }
}

/// A solid colour fill's colour (its `SoCo` descriptor's RGB, 0–255).
fn solid_colour(b: &[u8]) -> Option<[u8; 3]> {
    let get = |key: &[u8; 8]| {
        let at = b.windows(8).position(|w| w == key)? + 8;
        let v = f64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?);
        v.is_finite().then(|| v.round().clamp(0.0, 255.0) as u8)
    };
    Some([get(b"Rd  doub")?, get(b"Grn doub")?, get(b"Bl  doub")?])
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

/// A layer whose channel data is found, to be decoded.
struct Pending<'a> {
    layer: PsdLayer,
    /// Its size in the file, and the part of that on the canvas.
    size: [usize; 2],
    crop: [usize; 4],
    /// The same for its mask.
    mask: Option<([usize; 2], [usize; 4])>,
    channels: Vec<(i16, &'a [u8])>,
}

fn read_layers(
    r: &mut In<'_>,
    format: Format,
    budget: &mut Budget,
) -> Result<(Vec<PsdLayer>, Dropped), String> {
    let count = r.i16()?.unsigned_abs() as usize;
    let mut records = Vec::with_capacity(count);
    let mut dropped = Dropped::default();
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
        let mut lost = Dropped::default();
        let (mut colour, mut fill_opacity) = (None, None);
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
                b"SoCo" => match solid_colour(data.b) {
                    Some(c) => colour = Some(c),
                    None => lost.fill = true,
                },
                b"GdFl" | b"PtFl" => lost.fill = true,
                b"levl" | b"curv" | b"hue2" | b"hue " | b"brit" | b"blnc" | b"selc" | b"mixr"
                | b"grdm" | b"phfl" | b"expA" | b"vibA" | b"thrs" | b"post" | b"nvrt" | b"blwh"
                | b"clrL" => lost.adjustment = true,
                b"lfx2" | b"lrFX" | b"lmfx" => lost.effects = true,
                b"vmsk" | b"vsms" => lost.vector_mask = true,
                b"iOpa" => fill_opacity = Some(data.u8()?),
                _ => {}
            }
        }
        // Fill opacity fades the layer's own pixels but not its effects:
        // without effects, it's the same as its opacity.
        let opacity = match fill_opacity {
            Some(f) if !lost.effects => opacity * f as f32 / 255.0,
            _ => opacity,
        };
        // A shape layer is a colour fill cut by a vector mask: without the
        // mask it isn't a fill everywhere, and the flattened picture shows it.
        let fill = colour
            .filter(|_| !lost.vector_mask)
            .map(crate::canvas::layer_style::LayerFill::Colour);
        dropped.merge(lost);
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
                vector: None,
                fill,
            },
            channels,
        });
    }
    // Channel data, layer by layer in the same order: found, sized and
    // budgeted in turn, then decoded together.
    let [cw, ch] = format.size;
    let bytes = format.bytes;
    let mut pending = Vec::with_capacity(records.len());
    for Record {
        mut layer,
        channels,
    } in records
    {
        let [x0, y0, x1, y1] = layer.rect;
        let (w, h) = (span(x0, x1)?, span(y0, y1)?);
        // (A ZIP channel is unpacked whole before it's cut to the canvas.)
        if w * h > crate::app::document::MAX_CANVAS_PIXELS {
            return Err("Damaged PSD layer bounds".into());
        }
        let (rect, crop) = clip_to_canvas(layer.rect, cw, ch);
        layer.rect = rect;
        let area = (crop[2] - crop[0]) * (crop[3] - crop[1]);
        if bytes == 2 && area > crate::project::kra::MAX_DEEP_PIXELS {
            return Err("Damaged PSD layer bounds".into());
        }
        let mut mask = None;
        if let Some(m) = layer.mask.as_mut() {
            let (mw, mh) = (span(m.rect[0], m.rect[2])?, span(m.rect[1], m.rect[3])?);
            if mw * mh > crate::app::document::MAX_CANVAS_PIXELS {
                return Err("Damaged PSD layer mask bounds".into());
            }
            let (rect, crop) = clip_to_canvas(m.rect, cw, ch);
            m.rect = rect;
            mask = Some(([mw, mh], crop));
        }
        let mask_area = mask.map_or(0, |(_, c)| (c[2] - c[0]) * (c[3] - c[1]));
        // The planes, our pixels (and at full depth), the mask.
        let per_pixel = 4 * bytes + 4 + if bytes == 2 { 16 } else { 0 };
        budget.spend(area * per_pixel + mask_area * (bytes + 1))?;
        let channels = (channels.into_iter())
            .map(|(id, len)| Ok((id, r.take(len)?)))
            .collect::<Result<Vec<_>, String>>()?;
        pending.push(Pending {
            layer,
            size: [w, h],
            crop,
            mask,
            channels,
        });
    }
    use rayon::prelude::*;
    let layers = (pending.into_par_iter())
        .map(|p| decode_layer(p, format))
        .collect::<Result<Vec<_>, String>>()?;
    Ok((layers, dropped))
}

/// A layer's pixels (and mask) from its channels, on the canvas.
fn decode_layer(p: Pending<'_>, format: Format) -> Result<PsdLayer, String> {
    let Pending {
        mut layer,
        size: [w, h],
        crop,
        mask,
        channels,
    } = p;
    let bytes = format.bytes;
    let area = (crop[2] - crop[0]) * (crop[3] - crop[1]);
    let mut planes: [Option<Vec<u8>>; 4] = Default::default();
    for (id, data) in channels {
        match id {
            -1 => planes[3] = Some(read_plane(data, w, h, bytes, crop)?),
            0..=2 => planes[id as usize] = Some(read_plane(data, w, h, bytes, crop)?),
            -2 => {
                if let (Some(m), Some(([mw, mh], mcrop))) = (layer.mask.as_mut(), mask) {
                    // A damaged mask is left out; the layer still comes.
                    m.data = read_plane(data, mw, mh, bytes, mcrop).unwrap_or_default();
                    if bytes == 2 {
                        m.data = plane_to_8(&m.data);
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
    let rgb = if format.gray {
        [plane(0); 3]
    } else {
        [plane(0), plane(1), plane(2)]
    };
    if bytes == 2 {
        let (pixels, deep) = from_planes16(rgb, planes[3].as_deref(), area);
        layer.pixels = pixels;
        layer.deep = Some(deep);
    } else {
        layer.pixels = from_planes(rgb, planes[3].as_deref(), area);
    }
    Ok(layer)
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

/// The flattened picture's colour and alpha channels (the others are
/// skipped), premultiplied.
fn read_composite(
    r: &mut In<'_>,
    format: Format,
    channels: usize,
    budget: &mut Budget,
) -> Result<Vec<Color32>, String> {
    let Format { gray, bytes, .. } = format;
    let [w, h] = format.size;
    let compression = r.u16()?;
    let n = w * h;
    let color = if gray { 1 } else { 3 };
    if channels < color {
        return Err("The PSD has too few channels".into());
    }
    let wanted = channels.min(color + 1);
    budget.spend(n * (wanted * bytes + 4))?;
    let mut planes = Vec::with_capacity(wanted);
    match compression {
        0 => {
            for _ in 0..wanted {
                planes.push(r.take(n * bytes)?.to_vec());
            }
        }
        1 => {
            let counts: Vec<usize> = (0..channels * h)
                .map(|_| r.u16().map(|c| c as usize))
                .collect::<Result<_, _>>()?;
            // Rows that are all padding would make a picture from nothing.
            if wanted * n * bytes > (r.b.len() - r.pos).saturating_mul(RLE_RATIO) {
                return Err("The PSD's picture is cut short".into());
            }
            for c in 0..wanted {
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
        let doc = decode(&std::fs::read(path).unwrap(), true).unwrap();
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
        let plane = read_plane(&data, 2, 2, 2, [0, 0, 2, 2]).unwrap();
        let values: Vec<u16> = plane
            .chunks(2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]))
            .collect();
        assert_eq!(values, [100, 105, 7, 8]);
        // 8-bit ZIP without prediction.
        let mut data = 2u16.to_be_bytes().to_vec();
        data.extend(miniz_oxide::deflate::compress_to_vec_zlib(&[1, 2, 3, 4], 6));
        assert_eq!(
            read_plane(&data, 2, 2, 1, [0, 0, 2, 2]).unwrap(),
            [1, 2, 3, 4]
        );
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
    fn a_pass_through_folder_round_trips_through_psd() {
        let mut canvas = document();
        let folder = canvas
            .layers
            .iter()
            .position(|l| l.name == "Folder")
            .unwrap();
        canvas.layers[folder].blend = LayerBlend::PassThrough;
        let bytes = encode_psd(&PsdDocument::from_canvas(&canvas)).unwrap();
        let back = decode_psd(&bytes).unwrap().into_canvas().unwrap();
        let folder = back.layers.iter().find(|l| l.name == "Folder").unwrap();
        assert_eq!(folder.blend, LayerBlend::PassThrough);
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

    /// A layer for [`crafted`]: `[left, top, right, bottom]`, its channels
    /// (id, data with its compression), its mask's rect, tagged blocks.
    type Crafted = (
        [i32; 4],
        Vec<(i16, Vec<u8>)>,
        Option<[i32; 4]>,
        Vec<(&'static [u8; 4], Vec<u8>)>,
    );

    /// An 8-bit RGB file of `w`×`h` with `layers`, and `composite` (raw
    /// R, G, B planes) as its flattened picture.
    fn crafted(w: u32, h: u32, layers: &[Crafted], composite: &[u8]) -> Vec<u8> {
        let mut o = Out(Vec::new());
        o.bytes(b"8BPS");
        o.u16(1);
        o.bytes(&[0; 6]);
        o.u16(3);
        o.u32(h);
        o.u32(w);
        o.u16(8);
        o.u16(3);
        o.u32(0);
        o.u32(0);
        let section = o.len_slot();
        let info = o.len_slot();
        o.i16(layers.len() as i16);
        for (rect, channels, mask, blocks) in layers {
            for v in [rect[1], rect[0], rect[3], rect[2]] {
                o.i32(v);
            }
            o.u16(channels.len() as u16);
            for (id, data) in channels {
                o.i16(*id);
                o.u32(data.len() as u32);
            }
            o.bytes(b"8BIMnorm");
            o.bytes(&[255, 0, 0, 0]);
            let extra = o.len_slot();
            match mask {
                Some(m) => {
                    o.u32(20);
                    for v in [m[1], m[0], m[3], m[2]] {
                        o.i32(v);
                    }
                    o.bytes(&[255, 0, 0, 0]);
                }
                None => o.u32(0),
            }
            o.u32(0);
            pascal_name("L", &mut o);
            for (key, data) in blocks {
                o.bytes(b"8BIM");
                o.bytes(*key);
                o.u32(data.len() as u32);
                o.bytes(data);
            }
            o.close_len(extra, 1);
        }
        for (_, channels, ..) in layers {
            for (_, data) in channels {
                o.bytes(data);
            }
        }
        o.close_len(info, 2);
        o.u32(0);
        o.close_len(section, 1);
        o.u16(0);
        o.bytes(composite);
        o.0
    }

    /// An RLE channel of `h` rows that are all padding (no bytes each).
    fn padding_rows(h: usize) -> Vec<u8> {
        let mut d = vec![0, 1];
        d.extend(vec![0; 2 * h]);
        d
    }

    fn rgba_padding(h: usize) -> Vec<(i16, Vec<u8>)> {
        (-1..3).map(|id| (id, padding_rows(h))).collect()
    }

    #[test]
    fn a_layer_far_past_the_canvas_is_cut_to_it_before_decoding() {
        // 30 000 × 8 000 claimed, 64 × 64 on the canvas.
        let layer = (
            [-100, -100, 29_900, 7_900],
            rgba_padding(8000),
            None,
            vec![],
        );
        let file = crafted(64, 64, &[layer], &[]);
        assert!(file.len() < 70_000);
        let doc = decode_psd(&file).unwrap();
        assert_eq!(doc.layers[0].rect, [0, 0, 64, 64]);
        assert_eq!(doc.layers[0].pixels.len(), 64 * 64);
    }

    #[test]
    fn many_canvas_sized_layers_from_a_small_file_are_refused() {
        // Each layer: 16 384² pixels from 4 × 32 KB of padding rows. Unchecked
        // that's a gigabyte a layer.
        let layers: Vec<Crafted> = (0..8)
            .map(|_| ([0, 0, 16_384, 16_384], rgba_padding(16_384), None, vec![]))
            .collect();
        let file = crafted(16_384, 16_384, &layers, &[]);
        let err = decode_psd(&file).err().expect("refused");
        assert!(err.contains("bigger than its data"), "{err}");
        // Even one has nothing like that in it: RLE can't make that much.
        let one = crafted(16_384, 16_384, &layers[..1], &[]);
        assert!(decode_psd(&one).is_err());
    }

    #[test]
    fn a_huge_mask_is_refused_and_a_big_one_is_cut_to_the_canvas() {
        let mask = |m: [i32; 4], data: Vec<u8>| -> Crafted {
            let mut channels = rgba_padding(16);
            channels.push((-2, data));
            ([0, 0, 16, 16], channels, Some(m), vec![])
        };
        // Each side allowed, the two together not.
        let huge = mask([0, 0, 40_000, 40_000], padding_rows(40_000));
        assert!(decode_psd(&crafted(16, 16, &[huge], &[])).is_err());
        let big = mask([-8, -8, 2_000, 2_000], padding_rows(2_008));
        let doc = decode_psd(&crafted(16, 16, &[big], &[])).unwrap();
        let m = doc.layers[0].mask.as_ref().expect("kept");
        assert_eq!(m.rect, [0, 0, 16, 16]);
        assert_eq!(m.data.len(), 16 * 16);
    }

    #[test]
    fn short_raw_channels_are_refused_not_padded() {
        let mut short = vec![0, 0];
        short.extend(vec![9; 100]);
        let layer = ([0, 0, 16, 16], vec![(0, short)], None, vec![]);
        assert!(decode_psd(&crafted(16, 16, &[layer], &[])).is_err());
        let mut whole = vec![0, 0];
        whole.extend(vec![9; 256]);
        let layer = ([0, 0, 16, 16], vec![(0, whole)], None, vec![]);
        assert!(decode_psd(&crafted(16, 16, &[layer], &[])).is_ok());
    }

    #[test]
    fn photoshop_layers_with_no_form_here_bring_the_flattened_picture() {
        let picture: Vec<u8> = [200u8, 100, 50].iter().flat_map(|&v| vec![v; 64]).collect();
        let plain = ([0, 0, 8, 8], rgba_padding(8), None, vec![]);
        // Without them, the picture isn't even read.
        let doc = decode_psd(&crafted(8, 8, std::slice::from_ref(&plain), &picture)).unwrap();
        assert!(doc.notes.is_empty() && doc.composite.is_empty());
        assert_eq!(doc.layers.len(), 1);
        let levels: Crafted = ([0, 0, 0, 0], vec![], None, vec![(b"levl", vec![0; 8])]);
        let doc = decode_psd(&crafted(8, 8, &[plain.clone(), levels], &picture)).unwrap();
        assert_eq!(doc.notes.len(), 1, "{:?}", doc.notes);
        assert!(doc.notes[0].contains("adjustment layers"));
        let flat = doc.layers.last().unwrap();
        assert!(!flat.visible);
        assert_eq!(flat.rect, [0, 0, 8, 8]);
        assert_eq!(flat.pixels[0], Color32::from_rgb(200, 100, 50));
        // Effects too; a solid colour fill comes as a fill layer.
        let fx: Crafted = (
            [0, 0, 8, 8],
            rgba_padding(8),
            None,
            vec![(b"lfx2", vec![0; 8])],
        );
        let doc = decode_psd(&crafted(8, 8, &[fx], &picture)).unwrap();
        assert!(doc.notes[0].contains("layer effects"));
        let mut soco = vec![0, 0, 0, 16];
        for (key, v) in [(b"Rd  ", 10.0f64), (b"Grn ", 20.0), (b"Bl  ", 30.0)] {
            soco.extend([0, 0, 0, 0]);
            soco.extend(key);
            soco.extend(b"doub");
            soco.extend(v.to_be_bytes());
        }
        let fill: Crafted = ([0, 0, 0, 0], vec![], None, vec![(b"SoCo", soco)]);
        let doc = decode_psd(&crafted(8, 8, &[plain, fill], &picture)).unwrap();
        assert!(doc.notes.is_empty());
        let canvas = doc.into_canvas().unwrap();
        let top = canvas.layers.last().unwrap();
        assert_eq!(
            top.style.fill,
            Some(crate::canvas::layer_style::LayerFill::Colour([10, 20, 30]))
        );
    }

    #[test]
    fn fill_layers_and_borders_are_written_as_they_show() {
        use crate::canvas::layer_style::{Border, LayerFill};
        let mut canvas = Canvas::new(100, 70, Color32::WHITE, 64);
        let fill = canvas.insert_new_layer(2, "Fill".into(), LayerKind::Paint, None);
        let fi = canvas.layer_index_of(fill).unwrap();
        canvas.layers[fi].style.fill = Some(LayerFill::Colour([0, 0, 255]));
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        tile[20 * 64 + 20] = Color32::RED;
        canvas.set_layer_tile_data(1, 0, 0, tile);
        canvas.layers[1].style.border = Some(Border::default());
        let bytes = encode_psd(&PsdDocument::from_canvas(&canvas)).unwrap();
        let back = decode_psd(&bytes).unwrap();
        let fill = back
            .layers
            .iter()
            .find(|l| l.name == "Fill")
            .expect("written");
        assert_eq!(fill.rect, [0, 0, 100, 70]);
        assert!(fill.pixels.iter().all(|&p| p == Color32::BLUE));
        let bordered = back.layers.iter().find(|l| l.name == "Layer 1").unwrap();
        let painted = bordered.pixels.iter().filter(|p| p.a() > 0).count();
        assert!(painted > 1, "the border came along ({painted} pixels)");
    }

    #[test]
    fn mask_tiles_are_as_from_a_whole_canvas_picture() {
        // The old way: the whole canvas's mask, then cut into tiles.
        let whole = |mask: &PsdMask, w: usize, h: usize| {
            let [x0, y0, x1, y1] = mask.rect;
            let mw = (x1 - x0).max(0) as usize;
            let full: Vec<Color32> = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as i32, (i / w) as i32);
                    let v = if x >= x0 && x < x1 && y >= y0 && y < y1 {
                        mask.data[(y - y0) as usize * mw + (x - x0) as usize]
                    } else {
                        mask.default
                    };
                    Color32::from_rgb(v, v, v)
                })
                .collect();
            tiles_of(&full, [0, 0, w as i32, h as i32], 64, Color32::WHITE)
        };
        for (rect, default) in [
            ([10, 70, 150, 90], 255),
            ([0, 0, 0, 0], 0),
            ([64, 64, 128, 128], 255),
        ] {
            let n = ((rect[2] - rect[0]) * (rect[3] - rect[1])) as usize;
            let mask = PsdMask {
                rect,
                data: (0..n).map(|i| (i % 251) as u8).collect(),
                default,
            };
            let (a, b) = (mask_tiles(&mask, 200, 150, 64), whole(&mask, 200, 150));
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(&b) {
                assert_eq!((a.tx, a.ty, &a.data), (b.tx, b.ty, &b.data));
            }
        }
    }
}
