use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use eframe::egui::{Color32, ColorImage, Rgba};

use crate::canvas::blend::{
    apply_opacity_scale, average_over, color32_to_linear, color32s_to_linear,
    gamma_color32_to_rgba, gamma_rgba_to_color32, premultiply, rgba_to_color32_fast,
};
use crate::canvas::blend_modes::{
    BlendSpace, LayerBlend, composite as blend_composite, pixel_noise,
};
use crate::canvas::history::{LayerMeta, TileSnapshot, UndoAction};
use crate::selection::SelectionManager;
use crate::utils::color::{Color, ColorManipulation};
use eframe::egui::Vec2;

const MAX_TRANSFORM_SOURCE_PIXELS: usize = 67_108_864;

/// Stable identity for a layer, independent of its current position in
/// `Canvas::layers`. Undo history and other data that outlives a single
/// frame must key off this instead of a raw index, since reordering,
/// inserting or removing layers changes every index after the edit point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u64);

// Tile lookups happen per dab per tile and per layer per composited tile; FxHash
// is several times cheaper than the default SipHash for small integer keys.
type TileMap = FxHashMap<(i32, i32), Arc<Mutex<TileCell>>>;
type RowTileCache = Vec<Option<(i32, Arc<Mutex<TileCell>>, Option<Vec<Rgba>>, bool)>>;

pub use crate::canvas::blend::alpha_over_batch;

/// Transform operation parameters: an affine move/rotate/scale about
/// `center`, or (with `distort`) a free four-corner (perspective) mapping.
#[derive(Clone, Copy, Debug)]
pub struct TransformParams {
    pub offset: Vec2,
    pub rotation: f32,
    pub scale: Vec2,
    pub center: Vec2,
    pub distort: Option<Distort>,
    /// Quick preview: nearest-pixel sampling instead of bilinear.
    pub draft: bool,
}

/// Four-corner distortion: the source rectangle's corners (top-left,
/// top-right, bottom-right, bottom-left) go to `dst`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Distort {
    pub src: eframe::egui::Rect,
    pub dst: [Vec2; 4],
}

impl TransformParams {
    pub fn new(offset: Vec2, rotation: f32, scale: Vec2, center: Vec2) -> Self {
        Self {
            offset,
            rotation,
            scale,
            center,
            distort: None,
            draft: false,
        }
    }

    pub fn distorted(distort: Distort) -> Self {
        Self {
            distort: Some(distort),
            ..Self::new(Vec2::ZERO, 0.0, Vec2::new(1.0, 1.0), Vec2::ZERO)
        }
    }

    /// Canvas position a source point moves to.
    pub fn forward(&self, p: Vec2) -> Vec2 {
        if let Some(d) = &self.distort
            && let Some(h) = homography(rect_corners(d.src), d.dst)
        {
            return apply_homography(&h, p);
        }
        let (sin_r, cos_r) = self.rotation.sin_cos();
        let (dx, dy) = (
            (p.x - self.center.x) * self.scale.x,
            (p.y - self.center.y) * self.scale.y,
        );
        Vec2::new(
            dx * cos_r - dy * sin_r + self.center.x + self.offset.x,
            dx * sin_r + dy * cos_r + self.center.y + self.offset.y,
        )
    }

    /// Moves every pixel by the same whole number of pixels (so pixels can be
    /// copied exactly, with no resampling).
    fn is_whole_pixel_move(&self) -> bool {
        self.distort.is_none()
            && self.rotation == 0.0
            && self.scale == Vec2::new(1.0, 1.0)
            && self.offset.x.fract() == 0.0
            && self.offset.y.fract() == 0.0
    }
}

pub(crate) fn rect_corners(r: eframe::egui::Rect) -> [Vec2; 4] {
    [
        Vec2::new(r.min.x, r.min.y),
        Vec2::new(r.max.x, r.min.y),
        Vec2::new(r.max.x, r.max.y),
        Vec2::new(r.min.x, r.max.y),
    ]
}

/// The 3×3 projective map taking the four `src` points to `dst`
/// (row-major, last entry 1), or `None` if degenerate.
pub(crate) fn homography(src: [Vec2; 4], dst: [Vec2; 4]) -> Option<[f64; 9]> {
    // 8 equations in the 8 unknowns h0..h7 (h8 = 1), Gaussian elimination.
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (src[i].x as f64, src[i].y as f64);
        let (u, v) = (dst[i].x as f64, dst[i].y as f64);
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for col in 0..8 {
        let pivot = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        let pivot_row = m[col];
        for (row, r) in m.iter_mut().enumerate() {
            if row != col {
                let f = r[col] / pivot_row[col];
                for (v, p) in r[col..].iter_mut().zip(&pivot_row[col..]) {
                    *v -= f * p;
                }
            }
        }
    }
    let mut h = [0.0; 9];
    for i in 0..8 {
        h[i] = m[i][8] / m[i][i];
    }
    h[8] = 1.0;
    Some(h)
}

pub(crate) fn invert3(h: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, hh, i] = *h;
    let det = a * (e * i - f * hh) - b * (d * i - f * g) + c * (d * hh - e * g);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        (e * i - f * hh) * inv,
        (c * hh - b * i) * inv,
        (b * f - c * e) * inv,
        (f * g - d * i) * inv,
        (a * i - c * g) * inv,
        (c * d - a * f) * inv,
        (d * hh - e * g) * inv,
        (b * g - a * hh) * inv,
        (a * e - b * d) * inv,
    ])
}

pub(crate) fn apply_homography(h: &[f64; 9], p: Vec2) -> Vec2 {
    let (x, y) = (p.x as f64, p.y as f64);
    let w = h[6] * x + h[7] * y + h[8];
    let w = if w.abs() < 1e-12 { 1e-12 } else { w };
    Vec2::new(
        ((h[0] * x + h[1] * y + h[2]) / w) as f32,
        ((h[3] * x + h[4] * y + h[5]) / w) as f32,
    )
}

fn source_bounds_and_tiles(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    tile_size: usize,
    selection: Option<&SelectionManager>,
) -> Option<(eframe::egui::Rect, HashSet<(i32, i32)>)> {
    // Each tile's content bounds in parallel, then merged.
    let per_tile: Vec<((i32, i32), eframe::egui::Rect)> = src_tiles
        .par_iter()
        .filter_map(|(&(tx, ty), data)| {
            let base_x = tx * tile_size as i32;
            let base_y = ty * tile_size as i32;
            let mut bounds: Option<eframe::egui::Rect> = None;
            for py in 0..tile_size {
                for px in 0..tile_size {
                    if data[py * tile_size + px].a() == 0 {
                        continue;
                    }
                    let gx = base_x + px as i32;
                    let gy = base_y + py as i32;
                    if let Some(sel) = selection
                        && !sel.contains_coords(gx as f32, gy as f32)
                    {
                        continue;
                    }
                    let pos = eframe::egui::pos2(gx as f32, gy as f32);
                    match &mut bounds {
                        Some(b) => b.extend_with(pos),
                        None => bounds = Some(eframe::egui::Rect::from_min_max(pos, pos)),
                    }
                }
            }
            bounds.map(|b| ((tx, ty), b))
        })
        .collect();
    let mut iter = per_tile.iter();
    let (_, first) = iter.next()?;
    let bounds = iter.fold(*first, |acc, (_, b)| acc.union(*b));
    Some((bounds, per_tile.iter().map(|(key, _)| *key).collect()))
}

#[cfg(test)] // used by the reference transform in tests
fn sample_source_tile(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    x: i32,
    y: i32,
    tile_size: usize,
    selection: Option<&SelectionManager>,
) -> Color32 {
    if let Some(sel) = selection
        && !sel.contains_coords(x as f32, y as f32)
    {
        return Color32::TRANSPARENT;
    }

    let tile_size_i32 = tile_size as i32;
    let tx = x.div_euclid(tile_size_i32);
    let ty = y.div_euclid(tile_size_i32);
    let px = (x - tx * tile_size_i32) as usize;
    let py = (y - ty * tile_size_i32) as usize;
    src_tiles
        .get(&(tx, ty))
        .map_or(Color32::TRANSPARENT, |data| data[py * tile_size + px])
}

fn transform_tiles(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    src_bounds: eframe::egui::Rect,
    params: TransformParams,
    tile_size: usize,
    canvas_width: usize,
    canvas_height: usize,
    selection: Option<&SelectionManager>,
) -> HashMap<(i32, i32), Vec<Color32>> {
    if params.scale.x.abs() < f32::EPSILON
        || params.scale.y.abs() < f32::EPSILON
        || !params.offset.is_finite()
        || !params.scale.is_finite()
        || !params.center.is_finite()
        || !params.rotation.is_finite()
    {
        return HashMap::new();
    }
    // Source pixel area (pixel edges), and where its corners land.
    let src_area = eframe::egui::Rect::from_min_max(
        src_bounds.min,
        src_bounds.max + eframe::egui::vec2(1.0, 1.0),
    );
    let dst_corners = rect_corners(src_area).map(|c| params.forward(c));
    if dst_corners.iter().any(|c| !c.is_finite()) {
        return HashMap::new();
    }
    let min = dst_corners.iter().fold(Vec2::splat(f32::MAX), |a, c| {
        Vec2::new(a.x.min(c.x), a.y.min(c.y))
    });
    let max = dst_corners.iter().fold(Vec2::splat(f32::MIN), |a, c| {
        Vec2::new(a.x.max(c.x), a.y.max(c.y))
    });
    let tile_size_i32 = tile_size as i32;
    // Pixels may land off the canvas: they're kept (in tiles beyond its
    // edge, not drawn), so an image moved out and back comes back whole.
    // Bounded to one canvas size around it, so a wild drag can't allocate
    // without limit.
    let (margin_x, margin_y) = (canvas_width as i32, canvas_height as i32);
    let (x0, x1) = (
        (min.x.floor() as i32).max(-margin_x),
        (max.x.ceil() as i32).min(canvas_width as i32 + margin_x),
    );
    let (y0, y1) = (
        (min.y.floor() as i32).max(-margin_y),
        (max.y.ceil() as i32).min(canvas_height as i32 + margin_y),
    );
    if x0 >= x1 || y0 >= y1 {
        return HashMap::new();
    }

    // Destination pixel centre -> source point.
    let inverse_h = params
        .distort
        .and_then(|d| homography(rect_corners(d.src), d.dst))
        .and_then(|h| invert3(&h));
    if params.distort.is_some() && inverse_h.is_none() {
        return HashMap::new();
    }
    let (sin_r, cos_r) = params.rotation.sin_cos();
    let (inv_sx, inv_sy) = (1.0 / params.scale.x, 1.0 / params.scale.y);
    let inverse = |p: Vec2| -> Vec2 {
        if let Some(h) = &inverse_h {
            return apply_homography(h, p);
        }
        let (dx, dy) = (
            p.x - params.center.x - params.offset.x,
            p.y - params.center.y - params.offset.y,
        );
        let (rx, ry) = (dx * cos_r + dy * sin_r, -dx * sin_r + dy * cos_r);
        Vec2::new(rx * inv_sx + params.center.x, ry * inv_sy + params.center.y)
    };
    // Whole-pixel moves copy pixels exactly; anything else is resampled
    // bilinearly so rotated or scaled art stays smooth.
    let exact = params.is_whole_pixel_move();
    let (src_min_x, src_max_x) = (src_area.min.x.floor() as i32, src_area.max.x.ceil() as i32);
    let (src_min_y, src_max_y) = (src_area.min.y.floor() as i32, src_area.max.y.ceil() as i32);

    let dst_tiles: Vec<(i32, i32)> = (y0.div_euclid(tile_size_i32)
        ..=(y1 - 1).div_euclid(tile_size_i32))
        .flat_map(|ty| {
            (x0.div_euclid(tile_size_i32)..=(x1 - 1).div_euclid(tile_size_i32))
                .map(move |tx| (tx, ty))
        })
        .collect();

    dst_tiles
        .par_iter()
        .filter_map(|&(ntx, nty)| {
            let mut data: Option<Vec<Color32>> = None;
            // Source pixel lookup with a one-tile cache (neighbouring
            // destination pixels read neighbouring source pixels).
            let mut cached: Option<(TileKey, Option<&Vec<Color32>>)> = None;
            let mut source = |sx: i32, sy: i32| -> Color32 {
                if sx < src_min_x || sx >= src_max_x || sy < src_min_y || sy >= src_max_y {
                    return Color32::TRANSPARENT;
                }
                if let Some(sel) = selection
                    && !sel.contains_coords(sx as f32, sy as f32)
                {
                    return Color32::TRANSPARENT;
                }
                let key = (sx.div_euclid(tile_size_i32), sy.div_euclid(tile_size_i32));
                let tile = match cached {
                    Some((k, tile)) if k == key => tile,
                    _ => {
                        let tile = src_tiles.get(&key);
                        cached = Some((key, tile));
                        tile
                    }
                };
                tile.map_or(Color32::TRANSPARENT, |t| {
                    t[(sy - key.1 * tile_size_i32) as usize * tile_size
                        + (sx - key.0 * tile_size_i32) as usize]
                })
            };
            let mut quad_cache: Option<(TileKey, Option<&Vec<Color32>>)> = None;
            let tile_y0 = (nty * tile_size_i32).max(y0);
            let tile_y1 = ((nty + 1) * tile_size_i32).min(y1);
            let tile_x0 = (ntx * tile_size_i32).max(x0);
            let tile_x1 = ((ntx + 1) * tile_size_i32).min(x1);
            for y in tile_y0..tile_y1 {
                for x in tile_x0..tile_x1 {
                    let pixel = if exact {
                        source(x - params.offset.x as i32, y - params.offset.y as i32)
                    } else if params.draft {
                        let p = inverse(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                        source(p.x.floor() as i32, p.y.floor() as i32)
                    } else {
                        let p = inverse(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                        let (u, v) = (p.x - 0.5, p.y - 0.5);
                        let (fx0, fy0) = (u.floor(), v.floor());
                        let (ix, iy) = (fx0 as i32, fy0 as i32);
                        let (fx, fy) = (u - fx0, v - fy0);
                        let (lx, ly) = (ix.rem_euclid(tile_size_i32), iy.rem_euclid(tile_size_i32));
                        // Usually all four samples sit in one source tile:
                        // one lookup instead of four.
                        let inside = selection.is_none()
                            && lx + 1 < tile_size_i32
                            && ly + 1 < tile_size_i32
                            && ix >= src_min_x
                            && ix + 1 < src_max_x
                            && iy >= src_min_y
                            && iy + 1 < src_max_y;
                        if inside {
                            let key = (ix.div_euclid(tile_size_i32), iy.div_euclid(tile_size_i32));
                            let tile = match quad_cache {
                                Some((k, tile)) if k == key => tile,
                                _ => {
                                    let tile = src_tiles.get(&key);
                                    quad_cache = Some((key, tile));
                                    tile
                                }
                            };
                            match tile {
                                Some(t) => {
                                    let i = ly as usize * tile_size + lx as usize;
                                    bilinear(
                                        t[i],
                                        t[i + 1],
                                        t[i + tile_size],
                                        t[i + tile_size + 1],
                                        fx,
                                        fy,
                                    )
                                }
                                None => Color32::TRANSPARENT,
                            }
                        } else {
                            bilinear(
                                source(ix, iy),
                                source(ix + 1, iy),
                                source(ix, iy + 1),
                                source(ix + 1, iy + 1),
                                fx,
                                fy,
                            )
                        }
                    };
                    if pixel == Color32::TRANSPARENT {
                        continue;
                    }
                    let npx = (x - ntx * tile_size_i32) as usize;
                    let npy = (y - nty * tile_size_i32) as usize;
                    data.get_or_insert_with(|| vec![Color32::TRANSPARENT; tile_size * tile_size])
                        [npy * tile_size + npx] = pixel;
                }
            }
            data.map(|d| ((ntx, nty), d))
        })
        .collect()
}

/// Bilinear blend of four premultiplied pixels.
#[inline]
fn bilinear(p00: Color32, p10: Color32, p01: Color32, p11: Color32, fx: f32, fy: f32) -> Color32 {
    if fx == 0.0 && fy == 0.0 {
        return p00;
    }
    let (w00, w10, w01, w11) = (
        (1.0 - fx) * (1.0 - fy),
        fx * (1.0 - fy),
        (1.0 - fx) * fy,
        fx * fy,
    );
    let mix = |a: u8, b: u8, c: u8, d: u8| {
        (a as f32 * w00 + b as f32 * w10 + c as f32 * w01 + d as f32 * w11 + 0.5) as u8
    };
    Color32::from_rgba_premultiplied(
        mix(p00.r(), p10.r(), p01.r(), p11.r()),
        mix(p00.g(), p10.g(), p01.g(), p11.g()),
        mix(p00.b(), p10.b(), p01.b(), p11.b()),
        mix(p00.a(), p10.a(), p01.a(), p11.a()),
    )
}

fn write_transformed_tiles(
    tiles: &mut TileMap,
    dst_tiles: HashMap<(i32, i32), Vec<Color32>>,
    tile_size: usize,
) {
    for ((tx, ty), data) in dst_tiles {
        let tile_arc = tiles.entry((tx, ty)).or_insert_with(|| {
            Arc::new(Mutex::new(TileCell {
                data: Some(vec![Color32::TRANSPARENT; tile_size * tile_size]),
                is_empty: true,
            }))
        });
        let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
        if guard.data.is_none() {
            guard.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
        }

        let mut has_content = false;
        if let Some(target_data) = &mut guard.data {
            for i in 0..data.len() {
                if data[i].a() > 0 {
                    target_data[i] = data[i];
                    has_content = true;
                }
            }
        }
        guard.is_empty = !has_content;
    }
}

/// What a layer entry is. Folders and masks are entries in the same flat
/// list as paint layers, so painting, undo and saving handle them all alike;
/// the tree comes from `Layer::parent` and mask `owner` links, not from
/// positions (among siblings, list order is stacking order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerKind {
    #[default]
    Paint,
    /// A folder: composites its children (layers whose `parent` is this
    /// folder) on their own, then applies its opacity/visibility.
    Group,
    /// Mask of `owner`: white shows the owner, black or transparent hides
    /// it. Missing tiles count as white, so a new mask shows everything.
    /// The entry's `visible` flag enables/disables the mask.
    Mask { owner: LayerId },
}

#[derive(Debug)]
/// Single painting layer with its own opacity, visibility and tile storage.
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32, // 0..1
    pub locked: bool,
    /// Painting keeps the existing transparency.
    pub alpha_locked: bool,
    pub kind: LayerKind,
    /// Folder containing this layer (`None` = top level).
    pub parent: Option<LayerId>,
    /// Folder shown open in the layers panel.
    pub expanded: bool,
    /// How this layer (or folder) combines with what's below it.
    pub blend: LayerBlend,
    tiles: Mutex<TileMap>,
}

#[derive(Clone)]
pub struct CanvasTileSnapshot {
    pub tx: i32,
    pub ty: i32,
    pub data: Vec<Color32>,
}

#[derive(Clone)]
pub struct CanvasLayerSnapshot {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub locked: bool,
    /// Painting keeps the existing transparency.
    pub alpha_locked: bool,
    pub kind: LayerKind,
    pub parent: Option<LayerId>,
    pub expanded: bool,
    pub blend: LayerBlend,
    pub tiles: Vec<CanvasTileSnapshot>,
}

impl Layer {
    /// Allocate a new layer backing store but keep tile data lazy.
    fn new(id: LayerId, name: String, _width: usize, _height: usize, _tile_size: usize) -> Self {
        Self {
            id,
            name,
            visible: true,
            opacity: 1.0,
            locked: false,
            alpha_locked: false,
            kind: LayerKind::Paint,
            parent: None,
            expanded: true,
            blend: LayerBlend::Normal,
            tiles: Mutex::new(TileMap::default()),
        }
    }

    fn from_snapshot(snapshot: CanvasLayerSnapshot) -> Self {
        let mut tiles = TileMap::default();
        for tile in snapshot.tiles {
            tiles.insert(
                (tile.tx, tile.ty),
                Arc::new(Mutex::new(TileCell {
                    is_empty: tile.data.iter().all(|&p| p == Color32::TRANSPARENT),
                    data: Some(tile.data),
                })),
            );
        }
        Self {
            id: snapshot.id,
            name: snapshot.name,
            visible: snapshot.visible,
            opacity: snapshot.opacity.clamp(0.0, 1.0),
            locked: snapshot.locked,
            alpha_locked: snapshot.alpha_locked,
            kind: snapshot.kind,
            parent: snapshot.parent,
            expanded: snapshot.expanded,
            blend: snapshot.blend,
            tiles: Mutex::new(tiles),
        }
    }
}

/// Main drawing surface that owns tile grids and blending rules across layers.
pub struct Canvas {
    width: usize,
    height: usize,
    tile_size: usize,
    clear_color: Color32,

    pub layers: Vec<Layer>,
    pub active_layer_idx: usize,
    next_layer_id: u64,
    /// Colour space layers and strokes blend in (per document).
    pub blend_space: BlendSpace,
}

/// A tile's `(tx, ty)` position.
type TileKey = (i32, i32);
/// A tile's shared, lockable storage.
type SharedCell = Arc<Mutex<TileCell>>;

#[derive(Debug)]
/// Tile container that is lazily filled with pixel data.
pub(crate) struct TileCell {
    pub data: Option<Vec<Color32>>,
    /// True if the tile contains only transparent pixels
    pub is_empty: bool,
}

/// A precomputed composite of the layers below `first_layer` for one tile.
#[derive(Clone, Copy)]
pub struct BelowComposite<'a> {
    pub first_layer: usize,
    pub pixels: &'a [Rgba],
}

/// One visible layer's contribution to a tile, already in linear light.
struct LayerInput {
    opacity: f32,
    /// Used where the tile has no pixel data (background clear color, else transparent).
    fill: Rgba,
    linear: Option<Vec<Rgba>>,
}

/// A node of the per-tile composite tree used when folders or masks exist.
enum CompositeNode {
    Layer {
        input: LayerInput,
        mask: Option<MaskInput>,
        blend: LayerBlend,
    },
    /// Children are composited on their own first, then blended as one
    /// layer with the folder's opacity and blend mode.
    Group {
        opacity: f32,
        children: Vec<CompositeNode>,
        blend: LayerBlend,
    },
}

/// A mask's coverage (0..=1) per pixel of one tile; `None` means the tile
/// has no mask data, which shows everything.
struct MaskInput {
    values: Option<Vec<f32>>,
}

/// Normal-mode "over" of stored (gamma) values: the same operations as the
/// tree compositor's gamma path, so both give identical pixels.
#[inline]
fn gamma_over(src: Color32, dst: Color32) -> Color32 {
    let s = gamma_color32_to_rgba(src);
    if s.a() <= 0.0 {
        return dst;
    }
    gamma_rgba_to_color32(s + gamma_color32_to_rgba(dst) * (1.0 - s.a()))
}

/// Mask coverage of a (premultiplied) mask pixel: brightness times alpha,
/// so white shows, and black or transparent hides.
#[inline]
fn mask_coverage(px: Color32) -> f32 {
    (px.r() as f32 + px.g() as f32 + px.b() as f32) / (3.0 * 255.0)
}

/// Composite a tree onto `composite` for one pixel, each node with its
/// blend mode. `noise` is the pixel's Dissolve threshold.
fn composite_nodes(nodes: &[CompositeNode], idx: usize, mut composite: Rgba, noise: f32) -> Rgba {
    for node in nodes {
        let (src, blend) = match node {
            CompositeNode::Layer { input, mask, blend } => {
                let mut src = input.linear.as_ref().map_or(input.fill, |data| data[idx]);
                if src.a() == 0.0 {
                    continue;
                }
                if input.opacity < 1.0 {
                    src = src * input.opacity;
                }
                if let Some(mask) = mask {
                    let coverage = mask.values.as_ref().map_or(1.0, |v| v[idx]);
                    if coverage <= 0.0 {
                        continue;
                    }
                    if coverage < 1.0 {
                        src = src * coverage;
                    }
                }
                (src, *blend)
            }
            CompositeNode::Group {
                opacity,
                children,
                blend,
            } => {
                let inner = composite_nodes(children, idx, Rgba::TRANSPARENT, noise);
                if inner.a() == 0.0 {
                    continue;
                }
                let inner = if *opacity < 1.0 {
                    inner * *opacity
                } else {
                    inner
                };
                (inner, *blend)
            }
        };
        composite = blend_composite(blend, src, composite, noise);
    }
    composite
}

/// Source-over composite of `layers` (bottom to top) onto `composite` for one pixel.
#[inline]
fn composite_pixel(layers: &[LayerInput], idx: usize, mut composite: Rgba) -> Rgba {
    for layer in layers {
        let src = layer.linear.as_ref().map_or(layer.fill, |data| data[idx]);
        if src.a() == 0.0 {
            continue;
        }
        let src = if layer.opacity < 1.0 {
            src * layer.opacity
        } else {
            src
        };
        composite = src + composite * (1.0 - src.a());
    }
    composite
}

fn layer_tile(layer: &Layer, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
    layer
        .tiles
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(tx, ty))
        .cloned()
}

impl Canvas {
    /// Create a new canvas with a single background layer and configured tile size.
    pub fn new(width: usize, height: usize, clear_color: Color32, tile_size: usize) -> Self {
        let mut bg_layer = Layer::new(
            LayerId(0),
            "Background".to_string(),
            width,
            height,
            tile_size,
        );
        bg_layer.locked = true;

        let layer1 = Layer::new(LayerId(1), "Layer 1".to_string(), width, height, tile_size);

        // Initialize background layer with clear color
        // We can't easily pre-fill all tiles without allocating massive memory.
        // The original code lazily allocated.
        // But if it's the background, it should probably be white (or clear_color).
        // The original code handled `None` as `clear_color` in `ensure_tile`.
        // We should preserve that behavior.

        Self {
            width,
            height,
            tile_size,
            clear_color: premultiply(clear_color),
            layers: vec![bg_layer, layer1],
            active_layer_idx: 1,
            next_layer_id: 2,
            blend_space: BlendSpace::Linear,
        }
    }

    /// Look up a layer's current position by its stable id. O(layer count);
    /// layer counts are small, and this is never called from a pixel-stamp
    /// hot path.
    pub fn layer_index_of(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    /// The stable id of the layer currently at `idx`, if any.
    pub fn layer_id_at(&self, idx: usize) -> Option<LayerId> {
        self.layers.get(idx).map(|layer| layer.id)
    }

    /// Whether compositing needs the general tree compositor: any folder,
    /// mask or non-Normal blend mode, or gamma-space blending. Otherwise it's
    /// a plain Normal stack in linear light and the fast paths apply.
    pub fn needs_tree_compositing(&self) -> bool {
        self.blend_space != BlendSpace::Linear || !self.is_plain_stack()
    }

    /// No folders, masks or blend modes: a plain stack of Normal layers
    /// (in either blend space).
    fn is_plain_stack(&self) -> bool {
        self.layers.iter().all(|l| {
            l.kind == LayerKind::Paint && l.parent.is_none() && l.blend == LayerBlend::Normal
        })
    }

    /// Final 8-bit value of a composite computed by the tree compositor.
    fn encode_tree(&self, c: Rgba) -> Color32 {
        match self.blend_space {
            BlendSpace::Linear => rgba_to_color32_fast(c),
            BlendSpace::Gamma => gamma_rgba_to_color32(c),
        }
    }

    /// Position of the mask entry belonging to layer `owner`, if any.
    pub fn mask_index_of(&self, owner: LayerId) -> Option<usize> {
        self.layers
            .iter()
            .position(|l| l.kind == LayerKind::Mask { owner })
    }

    /// Whether `id` is `ancestor` or nested (at any depth) inside it.
    pub fn is_within(&self, id: LayerId, ancestor: LayerId) -> bool {
        let mut current = Some(id);
        // Bounded by the layer count, in case of a (never expected) cycle.
        for _ in 0..=self.layers.len() {
            match current {
                Some(c) if c == ancestor => return true,
                Some(c) => {
                    current = self.layer_index_of(c).and_then(|i| self.layers[i].parent);
                }
                None => return false,
            }
        }
        false
    }

    /// Insert a new empty entry at `index` and return its id.
    pub fn insert_new_layer(
        &mut self,
        index: usize,
        name: String,
        kind: LayerKind,
        parent: Option<LayerId>,
    ) -> LayerId {
        let id = self.allocate_layer_id();
        let mut layer = Layer::new(id, name, self.width, self.height, self.tile_size);
        layer.kind = kind;
        layer.parent = parent;
        self.layers.insert(index.min(self.layers.len()), layer);
        id
    }

    fn allocate_layer_id(&mut self) -> LayerId {
        let id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        id
    }

    pub fn add_layer(&mut self) -> LayerId {
        let name = format!("Layer {}", self.layers.len() + 1);
        let id = self.allocate_layer_id();
        let layer = Layer::new(id, name, self.width, self.height, self.tile_size);
        self.layers.push(layer);
        self.active_layer_idx = self.layers.len() - 1;
        id
    }

    /// Insert an empty layer with a specific (already-allocated) id and
    /// metadata at `index`, clamped to the current layer count. Used to
    /// reconstruct a layer shell for undo/redo of a layer add/remove; the
    /// caller is responsible for restoring pixel content separately (via
    /// `TileSnapshot`s resolved by the same id).
    pub fn insert_layer_with_meta(&mut self, index: usize, id: LayerId, meta: &LayerMeta) {
        let idx = index.min(self.layers.len());
        let mut layer = Layer::new(
            id,
            meta.name.clone(),
            self.width,
            self.height,
            self.tile_size,
        );
        layer.visible = meta.visible;
        layer.opacity = meta.opacity;
        layer.locked = meta.locked;
        layer.alpha_locked = meta.alpha_locked;
        layer.kind = meta.kind;
        layer.parent = meta.parent;
        layer.blend = meta.blend;
        self.layers.insert(idx, layer);
        // `id` is a reused (previously-allocated) id, not a new one, but
        // guard against ever handing out a colliding id afterward.
        self.next_layer_id = self.next_layer_id.max(id.0 + 1);
    }

    /// Snapshot every non-empty tile of a layer as full-tile `TileSnapshot`s,
    /// keyed by the layer's current stable id. Used to preserve a layer's
    /// pixel content across undo/redo of an operation that removes it
    /// (layer removal, merge-down).
    pub fn snapshot_layer_tiles(&self, layer_idx: usize) -> Vec<TileSnapshot> {
        let (Some(layer), Some(id)) = (self.layers.get(layer_idx), self.layer_id_at(layer_idx))
        else {
            return Vec::new();
        };
        let tile_size = self.tile_size;
        let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
        tiles
            .iter()
            .filter_map(|(&(tx, ty), cell)| {
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty {
                    return None;
                }
                guard.data.clone().map(|data| TileSnapshot {
                    tx,
                    ty,
                    layer_id: id,
                    x0: 0,
                    y0: 0,
                    width: tile_size,
                    height: tile_size,
                    data: data.into(),
                })
            })
            .collect()
    }

    /// Metadata (name/visible/opacity/locked) for a layer, without its tile
    /// content. Used to build an undo record before removing a layer.
    pub fn layer_meta_at(&self, layer_idx: usize) -> Option<LayerMeta> {
        self.layers.get(layer_idx).map(|layer| LayerMeta {
            name: layer.name.clone(),
            visible: layer.visible,
            opacity: layer.opacity,
            locked: layer.locked,
            alpha_locked: layer.alpha_locked,
            kind: layer.kind,
            parent: layer.parent,
            blend: layer.blend,
        })
    }

    /// Current canvas width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Current canvas height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn clear_color(&self) -> Color32 {
        self.clear_color
    }

    pub fn layer_snapshots(&self) -> Vec<CanvasLayerSnapshot> {
        self.layers
            .iter()
            .map(|layer| {
                let mut tiles: Vec<_> = layer
                    .tiles
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .filter_map(|(&(tx, ty), cell)| {
                        let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                        if guard.is_empty {
                            return None;
                        }
                        guard
                            .data
                            .clone()
                            .map(|data| CanvasTileSnapshot { tx, ty, data })
                    })
                    .collect();
                tiles.sort_by_key(|tile| (tile.ty, tile.tx));
                CanvasLayerSnapshot {
                    id: layer.id,
                    name: layer.name.clone(),
                    visible: layer.visible,
                    opacity: layer.opacity,
                    locked: layer.locked,
                    alpha_locked: layer.alpha_locked,
                    kind: layer.kind,
                    parent: layer.parent,
                    expanded: layer.expanded,
                    blend: layer.blend,
                    tiles,
                }
            })
            .collect()
    }

    pub fn replace_layers_from_snapshots(
        &mut self,
        layers: Vec<CanvasLayerSnapshot>,
        active_layer_idx: usize,
    ) {
        self.layers = layers.into_iter().map(Layer::from_snapshot).collect();
        if self.layers.is_empty() {
            let id = self.allocate_layer_id();
            self.layers.push(Layer::new(
                id,
                "Background".to_string(),
                self.width,
                self.height,
                self.tile_size,
            ));
        }
        self.active_layer_idx = active_layer_idx.min(self.layers.len().saturating_sub(1));
        // Loaded layers may carry ids from the saved file (or position-based
        // fallback ids for files predating LayerId); make sure new layers
        // added after this never collide with them.
        self.next_layer_id = self
            .layers
            .iter()
            .map(|layer| layer.id.0)
            .max()
            .map_or(0, |max_id| max_id + 1);
    }

    /// Size of a tile edge in pixels.
    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    /// Access a specific layer's tile by index (used for compositing).
    fn layer_tile_cell(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
        tiles.get(&(tx, ty)).cloned()
    }

    /// Ensure the tile exists on a specific layer, initializing it if needed.
    pub(crate) fn ensure_layer_tile(
        &self,
        layer_idx: usize,
        tx: i32,
        ty: i32,
    ) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];

        let tile_arc = {
            let mut tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            tiles
                .entry((tx, ty))
                .or_insert_with(|| {
                    Arc::new(Mutex::new(TileCell {
                        data: None,
                        is_empty: true,
                    }))
                })
                .clone()
        };

        {
            let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
            if guard.data.is_none() {
                let fill_color = if layer_idx == 0 {
                    self.clear_color
                } else if matches!(layer.kind, LayerKind::Mask { .. }) {
                    // A mask starts out showing everything.
                    Color32::WHITE
                } else {
                    Color32::TRANSPARENT
                };

                let data = vec![fill_color; self.tile_size * self.tile_size];
                guard.is_empty = fill_color == Color32::TRANSPARENT;
                guard.data = Some(data);
            }
        }
        Some(tile_arc)
    }

    /// Ensure the active layer has storage for the given tile.
    fn ensure_tile(&self, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(self.active_layer_idx, tx, ty)
    }

    /// Guarantee a tile exists on the active layer.
    pub fn ensure_tile_exists(&self, tx: usize, ty: usize) {
        let _ = self.ensure_tile(tx as i32, ty as i32);
    }

    /// Guarantee a tile exists on the specified layer.
    /// Guarantee a tile exists on the specified layer.
    pub fn ensure_layer_tile_exists(&self, layer_idx: usize, tx: usize, ty: usize) {
        let _ = self.ensure_layer_tile(layer_idx, tx as i32, ty as i32);
    }

    /// Lock a tile in the active layer, initializing it if absent.
    pub(crate) fn lock_tile(&self, tx: usize, ty: usize) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_tile(tx as i32, ty as i32)
    }

    /// Lock a tile in a specific layer, initializing it if absent.
    pub(crate) fn lock_layer_tile(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(layer_idx, tx as i32, ty as i32)
    }

    /// Lock a tile in a specific layer only if it already exists; avoids allocating new data.
    pub(crate) fn lock_layer_tile_if_exists(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.layer_tile_cell(layer_idx, tx as i32, ty as i32)
    }

    /// Clone the raw pixel buffer for a tile in a given layer.
    pub fn get_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Vec<Color32>> {
        let cell = self.layer_tile_cell(layer_idx, tx, ty)?;
        let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
        guard.data.clone()
    }

    /// Overwrite a tile's pixel buffer for a given layer.
    pub fn set_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32, data: Vec<Color32>) {
        // Ensure tile exists
        if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
            let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
            let is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
            guard.is_empty = is_empty;
            guard.data = Some(data);
        }
    }

    /// Composite a canvas region into a `ColorImage`, optionally downsampled by `step`.
    pub fn write_region_to_color_image(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        out: &mut ColorImage,
        step: usize,
    ) {
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }

        let step = step.max(1);
        let dst_w = w.div_ceil(step);
        let dst_h = h.div_ceil(step);

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        // Optimization: Check if the region is within a single tile
        let start_tx = x / self.tile_size;
        let start_ty = y / self.tile_size;
        let end_tx = (x.saturating_add(w).saturating_sub(1)) / self.tile_size;
        let end_ty = (y.saturating_add(h).saturating_sub(1)) / self.tile_size;

        if start_tx == end_tx
            && start_ty == end_ty
            && step == 1
            && self.try_write_single_tile_fast(
                start_tx as i32,
                start_ty as i32,
                x..x + w,
                y..y + h,
                out,
            )
        {
            return;
        }

        if start_tx == end_tx && start_ty == end_ty {
            self.write_single_tile_region(
                x,
                y,
                w,
                h,
                start_tx as i32,
                start_ty as i32,
                dst_w,
                dst_h,
                step,
                out,
                None,
            );
            return;
        }

        self.write_multi_tile_region(x, y, dst_w, dst_h, step, out);
    }

    /// The whole flattened picture, composited tile by tile in parallel
    /// (the same per-tile path the screen uses).
    pub fn flatten(&self) -> ColorImage {
        let (w, h, ts) = (self.width, self.height, self.tile_size);
        let mut img = ColorImage::new([w, h], Color32::TRANSPARENT);
        let tiles_w = w.div_ceil(ts);
        img.pixels
            .par_chunks_mut(w * ts)
            .enumerate()
            .for_each(|(ty, strip)| {
                let strip_h = strip.len() / w;
                let mut part = ColorImage::new([0, 0], Color32::TRANSPARENT);
                for tx in 0..tiles_w {
                    self.write_tile_to_color_image(tx, ty, &mut part, 1, None);
                    let [pw, ph] = part.size;
                    for row in 0..ph.min(strip_h) {
                        let dst = row * w + tx * ts;
                        strip[dst..dst + pw]
                            .copy_from_slice(&part.pixels[row * pw..(row + 1) * pw]);
                    }
                }
            });
        img
    }

    /// Composite one whole tile into `out` (downsampled by `step`), optionally
    /// starting from a precomputed composite of the layers below
    /// `below.first_layer` instead of compositing them again.
    pub fn write_tile_to_color_image(
        &self,
        tx: usize,
        ty: usize,
        out: &mut ColorImage,
        step: usize,
        below: Option<BelowComposite<'_>>,
    ) {
        let x = tx * self.tile_size;
        let y = ty * self.tile_size;
        let w = self.tile_size.min(self.width.saturating_sub(x));
        let h = self.tile_size.min(self.height.saturating_sub(y));
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }
        let step = step.max(1);
        let dst_w = w.div_ceil(step);
        let dst_h = h.div_ceil(step);
        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }
        if step == 1
            && self.try_write_single_tile_fast(tx as i32, ty as i32, x..x + w, y..y + h, out)
        {
            return;
        }
        self.write_single_tile_region(
            x, y, w, h, tx as i32, ty as i32, dst_w, dst_h, step, out, below,
        );
    }

    /// Composite the tile-local rectangle `rect` (`[x0, y0, x1, y1)`, clipped
    /// to the canvas) of tile `(tx, ty)` into `out`, sized to the rectangle.
    /// Same as [`Self::write_tile_to_color_image`] at step 1, for the part of
    /// a tile a stroke actually changed.
    pub fn write_tile_rect_to_color_image(
        &self,
        tx: usize,
        ty: usize,
        rect: [usize; 4],
        out: &mut ColorImage,
        below: Option<BelowComposite<'_>>,
    ) {
        let x = tx * self.tile_size + rect[0];
        let y = ty * self.tile_size + rect[1];
        let w = (rect[2] - rect[0]).min(self.width.saturating_sub(x));
        let h = (rect[3] - rect[1]).min(self.height.saturating_sub(y));
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }
        if out.size != [w, h] {
            out.size = [w, h];
            out.pixels.resize(w * h, Color32::TRANSPARENT);
        }
        if self.try_write_single_tile_fast(tx as i32, ty as i32, x..x + w, y..y + h, out) {
            return;
        }
        self.write_single_tile_region(x, y, w, h, tx as i32, ty as i32, w, h, 1, out, below);
    }

    /// [`Self::write_tile_rect_to_color_image`] shrunk by `block` (a power of
    /// two, `rect` aligned to it): each output pixel is the linear-light
    /// average of its block's composites, computed directly instead of
    /// compositing at full resolution, rounding to 8 bits, then decoding
    /// again to average (the zoomed-out stroke preview's main cost).
    pub fn write_tile_rect_downsampled(
        &self,
        tx: usize,
        ty: usize,
        rect: [usize; 4],
        block: usize,
        out: &mut ColorImage,
        below: Option<BelowComposite<'_>>,
    ) {
        let x = tx * self.tile_size + rect[0];
        let y = ty * self.tile_size + rect[1];
        let w = (rect[2] - rect[0]).min(self.width.saturating_sub(x));
        let h = (rect[3] - rect[1]).min(self.height.saturating_sub(y));
        let block = block.max(1);
        let (dst_w, dst_h) = (w.div_ceil(block), h.div_ceil(block));
        if dst_w == 0 || dst_h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }
        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }
        if below.is_none()
            && self.try_write_downsampled_fast(tx as i32, ty as i32, x..x + w, y..y + h, block, out)
        {
            return;
        }
        // Any stack: the general compositor already averages composites in
        // linear light when stepping.
        self.write_single_tile_region(
            x, y, w, h, tx as i32, ty as i32, dst_w, dst_h, block, out, below,
        );
    }

    /// Downsampled counterpart of [`Self::try_write_single_tile_fast`]: the
    /// same background + one opaque paint layer case, blended per pixel and
    /// averaged per block in one pass.
    fn try_write_downsampled_fast(
        &self,
        tx: i32,
        ty: i32,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
        block: usize,
        out: &mut ColorImage,
    ) -> bool {
        // Averages in linear light: linear documents only.
        if self.needs_tree_compositing() {
            return false;
        }
        let mut bg_visible = false;
        let mut paint_layer = None;
        for (idx, layer) in self.layers.iter().enumerate() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if idx == 0 {
                bg_visible = true;
            } else if paint_layer.is_none() && layer.opacity >= 1.0 {
                paint_layer = Some(idx);
            } else {
                return false;
            }
        }
        let bg_arc = bg_visible
            .then(|| self.layer_tile_cell(0, tx, ty))
            .flatten();
        let bg_guard = bg_arc
            .as_ref()
            .map(|a| a.lock().unwrap_or_else(|e| e.into_inner()));
        let bg_data = bg_guard.as_ref().and_then(|g| g.data.as_ref());
        let paint_arc = paint_layer.and_then(|idx| self.layer_tile_cell(idx, tx, ty));
        let paint_guard = paint_arc
            .as_ref()
            .map(|a| a.lock().unwrap_or_else(|e| e.into_inner()));
        let paint_data = paint_guard
            .as_ref()
            .filter(|g| !g.is_empty)
            .and_then(|g| g.data.as_ref());

        let ts = self.tile_size;
        let (x0, y0) = (x_range.start % ts, y_range.start % ts);
        let (w, h) = (x_range.len(), y_range.len());
        let dst_w = out.size[0];
        for (oy, row) in out.pixels.chunks_mut(dst_w).enumerate() {
            let rows = oy * block..((oy + 1) * block).min(h);
            for (ox, px) in row.iter_mut().enumerate() {
                let cols = ox * block..((ox + 1) * block).min(w);
                let pixels = rows.clone().flat_map(|r| {
                    let base = (y0 + r) * ts + x0;
                    cols.clone().map(move |c| base + c)
                });
                *px = average_over(pixels.map(|i| {
                    let dst = if bg_visible {
                        bg_data.map_or(self.clear_color, |d| d[i])
                    } else {
                        Color32::TRANSPARENT
                    };
                    let src = paint_data.map_or(Color32::TRANSPARENT, |d| d[i]);
                    (src, dst)
                }));
            }
        }
        true
    }

    /// Linear, premultiplied composite of the visible layers below
    /// `layer_idx` for every pixel of one tile: exactly the value the
    /// compositor has accumulated just before reaching `layer_idx`.
    pub fn composite_below(&self, layer_idx: usize, tx: i32, ty: i32) -> Vec<Rgba> {
        let layers = self.tile_layer_inputs(tx, ty, 0..layer_idx.min(self.layers.len()));
        (0..self.tile_size * self.tile_size)
            .map(|idx| composite_pixel(&layers, idx, Rgba::TRANSPARENT))
            .collect()
    }

    /// Cache key for [`Self::composite_below`]: everything besides pixel
    /// content (which only the active layer's strokes change) it depends on.
    pub fn composite_below_key(&self, layer_idx: usize) -> (Color32, Vec<(LayerId, bool, u32)>) {
        let layers = self.layers[..layer_idx.min(self.layers.len())]
            .iter()
            .map(|l| (l.id, l.visible, l.opacity.to_bits()))
            .collect();
        (self.clear_color, layers)
    }

    /// The visible, non-empty layers in `range` for one tile, pre-converted to
    /// linear light.
    fn tile_layer_inputs(
        &self,
        tx: i32,
        ty: i32,
        range: std::ops::Range<usize>,
    ) -> Vec<LayerInput> {
        range
            .filter(|&i| self.layers[i].kind == LayerKind::Paint)
            .filter_map(|i| self.layer_input(i, tx, ty, BlendSpace::Linear))
            .collect()
    }

    /// Layer `i`'s contribution to one tile, in `space` (linear light, or
    /// the stored sRGB values), or `None` if it contributes nothing (hidden,
    /// fully transparent or no content there).
    fn layer_input(&self, i: usize, tx: i32, ty: i32, space: BlendSpace) -> Option<LayerInput> {
        let layer = &self.layers[i];
        if !(layer.visible && layer.opacity > 0.0) {
            return None;
        }
        let cell = layer_tile(layer, tx, ty);
        let guard = cell
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        // A missing background tile shows the clear color; any other missing tile is empty.
        if guard.as_ref().map_or(i != 0, |g| g.is_empty) {
            return None;
        }
        let convert = |c: Color32| match space {
            BlendSpace::Linear => color32_to_linear(c),
            BlendSpace::Gamma => gamma_color32_to_rgba(c),
        };
        Some(LayerInput {
            opacity: layer.opacity,
            fill: if i == 0 {
                convert(self.clear_color)
            } else {
                Rgba::TRANSPARENT
            },
            linear: guard
                .as_ref()
                .and_then(|g| g.data.as_deref())
                .map(|data| match space {
                    BlendSpace::Linear => color32s_to_linear(data),
                    BlendSpace::Gamma => data.iter().copied().map(gamma_color32_to_rgba).collect(),
                }),
        })
    }

    /// The composite tree (folders and masks) for one tile.
    fn tile_nodes(&self, tx: i32, ty: i32) -> Vec<CompositeNode> {
        let masks: HashMap<LayerId, usize> = self
            .layers
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l.kind {
                LayerKind::Mask { owner } => Some((owner, i)),
                _ => None,
            })
            .collect();
        self.child_nodes(tx, ty, None, &masks, 0)
    }

    /// Nodes for the children of `parent`, bottom to top.
    fn child_nodes(
        &self,
        tx: i32,
        ty: i32,
        parent: Option<LayerId>,
        masks: &HashMap<LayerId, usize>,
        depth: usize,
    ) -> Vec<CompositeNode> {
        let mut nodes = Vec::new();
        if depth > self.layers.len() {
            return nodes; // a parent cycle; never expected
        }
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.parent != parent || !(layer.visible && layer.opacity > 0.0) {
                continue;
            }
            match layer.kind {
                LayerKind::Mask { .. } => {}
                LayerKind::Group => {
                    let children = self.child_nodes(tx, ty, Some(layer.id), masks, depth + 1);
                    if !children.is_empty() {
                        nodes.push(CompositeNode::Group {
                            opacity: layer.opacity,
                            children,
                            blend: layer.blend,
                        });
                    }
                }
                LayerKind::Paint => {
                    let Some(input) = self.layer_input(i, tx, ty, self.blend_space) else {
                        continue;
                    };
                    let mask = masks
                        .get(&layer.id)
                        .and_then(|&m| self.mask_input(m, tx, ty));
                    nodes.push(CompositeNode::Layer {
                        input,
                        mask,
                        blend: layer.blend,
                    });
                }
            }
        }
        nodes
    }

    /// Mask coverage for one tile, or `None` when the mask is disabled.
    fn mask_input(&self, mask_idx: usize, tx: i32, ty: i32) -> Option<MaskInput> {
        let layer = &self.layers[mask_idx];
        if !layer.visible {
            return None;
        }
        let cell = layer_tile(layer, tx, ty);
        let guard = cell
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let values = guard
            .as_ref()
            .and_then(|g| g.data.as_deref())
            .map(|data| data.iter().copied().map(mask_coverage).collect());
        Some(MaskInput { values })
    }

    /// Composite a region that lies entirely within one tile. Used when
    /// `try_write_single_tile_fast`'s stricter fast path doesn't apply
    /// (several layers, partial opacity, or `step != 1` downsampling).
    #[allow(clippy::too_many_arguments)]
    fn write_single_tile_region(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        tx: i32,
        ty: i32,
        dst_w: usize,
        dst_h: usize,
        step: usize,
        out: &mut ColorImage,
        below: Option<BelowComposite<'_>>,
    ) {
        let first_layer = below.map_or(0, |b| b.first_layer);
        // Folders/masks need the tree compositor (never with a below-cache,
        // which assumes a plain stack).
        let nodes =
            (below.is_none() && self.needs_tree_compositing()).then(|| self.tile_nodes(tx, ty));
        let layers = if nodes.is_some() {
            Vec::new()
        } else {
            self.tile_layer_inputs(tx, ty, first_layer..self.layers.len())
        };
        let start = |idx: usize| below.map_or(Rgba::TRANSPARENT, |b| b.pixels[idx]);
        let composite_at = |idx: usize, gx: usize, gy: usize| match &nodes {
            Some(nodes) => composite_nodes(
                nodes,
                idx,
                Rgba::TRANSPARENT,
                pixel_noise(gx as u32, gy as u32),
            ),
            None => composite_pixel(&layers, idx, start(idx)),
        };
        let encode = |c: Rgba| {
            if nodes.is_some() {
                self.encode_tree(c)
            } else {
                rgba_to_color32_fast(c)
            }
        };

        for dst_y in 0..dst_h {
            let global_y_start = y + dst_y * step;
            let row_start = dst_y * dst_w;

            for dst_x in 0..dst_w {
                let global_x_start = x + dst_x * step;

                if step == 1 {
                    let local_y = global_y_start % self.tile_size;
                    let local_x = global_x_start % self.tile_size;
                    let src_idx = local_y * self.tile_size + local_x;
                    let composite = composite_at(src_idx, global_x_start, global_y_start);
                    out.pixels[row_start + dst_x] = encode(composite);
                } else {
                    // Downsample: average the linear composites of the covered pixels.
                    let mut r_acc = 0.0;
                    let mut g_acc = 0.0;
                    let mut b_acc = 0.0;
                    let mut a_acc = 0.0;
                    let mut count = 0.0;

                    for sy in 0..step {
                        let global_y = global_y_start + sy;
                        if global_y >= y + h {
                            continue;
                        }
                        let local_y = global_y % self.tile_size;

                        for sx in 0..step {
                            let global_x = global_x_start + sx;
                            if global_x >= x + w {
                                continue;
                            }
                            let local_x = global_x % self.tile_size;
                            let src_idx = local_y * self.tile_size + local_x;
                            let sub_composite = composite_at(src_idx, global_x, global_y);

                            r_acc += sub_composite.r();
                            g_acc += sub_composite.g();
                            b_acc += sub_composite.b();
                            a_acc += sub_composite.a();
                            count += 1.0;
                        }
                    }

                    if count > 0.0 {
                        let inv = 1.0 / count;
                        out.pixels[row_start + dst_x] = encode(Rgba::from_rgba_premultiplied(
                            r_acc * inv,
                            g_acc * inv,
                            b_acc * inv,
                            a_acc * inv,
                        ));
                    }
                }
            }
        }
    }

    /// Composite a region that spans multiple tiles, caching one decoded
    /// tile per layer per output row.
    fn write_multi_tile_region(
        &self,
        x: usize,
        y: usize,
        dst_w: usize,
        dst_h: usize,
        step: usize,
        out: &mut ColorImage,
    ) {
        if self.needs_tree_compositing() {
            self.write_multi_tile_region_tree(x, y, dst_w, dst_h, step, out);
            return;
        }
        let clear_color_linear = color32_to_linear(self.clear_color);
        // Optimization: Cache tiles and pre-convert to linear space
        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let ty = (global_y / self.tile_size) as i32;
            let local_y = global_y % self.tile_size;

            // Cache tile Arc and converted linear data for this row
            // Tuple: (cached_tx, tile_arc, linear_tile_data, is_empty)
            let mut row_tile_cache: RowTileCache = Vec::with_capacity(self.layers.len());

            // Initialize cache with None for each layer
            for _ in 0..self.layers.len() {
                row_tile_cache.push(None);
            }

            let mut dst_x = 0;
            while dst_x < dst_w {
                let global_x = x + dst_x * step;
                let tx = (global_x / self.tile_size) as i32;
                let local_x = global_x % self.tile_size;

                let dst_start = dst_y * dst_w + dst_x;

                let mut composite = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                for (layer_idx, layer) in self.layers.iter().enumerate() {
                    if !layer.visible || layer.opacity <= 0.0 {
                        continue;
                    }

                    // Check if we need to fetch a different tile
                    let needs_lookup = row_tile_cache[layer_idx]
                        .as_ref()
                        .is_none_or(|(cached_tx, _, _, _)| *cached_tx != tx);

                    if needs_lookup {
                        // Drop old cache entry
                        row_tile_cache[layer_idx] = None;

                        // Fetch new tile and pre-convert to linear
                        if let Some(tile_arc) = self.layer_tile_cell(layer_idx, tx, ty) {
                            // Lock temporarily to read data
                            let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                            let is_empty = guard.is_empty;

                            // Pre-convert entire tile to linear space for efficiency
                            let linear_data = guard.data.as_deref().map(color32s_to_linear);

                            // Release lock and cache the Arc with converted data
                            drop(guard);
                            row_tile_cache[layer_idx] = Some((tx, tile_arc, linear_data, is_empty));
                        }
                    }

                    // Skip if tile is empty or missing
                    let (is_empty, linear_data) =
                        if let Some((_, _, linear_data, is_empty)) = &row_tile_cache[layer_idx] {
                            (*is_empty, linear_data.as_ref())
                        } else if layer_idx == 0 {
                            (false, None) // Background uses clear_color
                        } else {
                            continue; // Non-background layer with no tile
                        };

                    if is_empty {
                        continue;
                    }

                    // Resolve Pixel in linear space
                    let src = if let Some(linear_tile) = linear_data {
                        let src_idx = local_y * self.tile_size + local_x;
                        linear_tile[src_idx]
                    } else if layer_idx == 0 {
                        clear_color_linear
                    } else {
                        Rgba::TRANSPARENT
                    };

                    if src.a() == 0.0 {
                        continue;
                    }

                    // Apply opacity and blend (already in linear space)
                    let src = if layer.opacity < 1.0 {
                        src * layer.opacity
                    } else {
                        src
                    };
                    composite = src + composite * (1.0 - src.a());
                }

                out.pixels[dst_start] = rgba_to_color32_fast(composite);
                dst_x += 1;
            }
        }
    }

    /// [`Self::write_multi_tile_region`] for canvases with folders or masks:
    /// same sampling, through the composite tree. Trees are cached for one
    /// row of tiles at a time to bound memory on large exports.
    fn write_multi_tile_region_tree(
        &self,
        x: usize,
        y: usize,
        dst_w: usize,
        dst_h: usize,
        step: usize,
        out: &mut ColorImage,
    ) {
        let mut cache: HashMap<i32, Vec<CompositeNode>> = HashMap::new();
        let mut cached_ty = None;
        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let ty = (global_y / self.tile_size) as i32;
            if cached_ty != Some(ty) {
                cache.clear();
                cached_ty = Some(ty);
            }
            let local_y = global_y % self.tile_size;
            for dst_x in 0..dst_w {
                let global_x = x + dst_x * step;
                let tx = (global_x / self.tile_size) as i32;
                let local_x = global_x % self.tile_size;
                let nodes = cache.entry(tx).or_insert_with(|| self.tile_nodes(tx, ty));
                let idx = local_y * self.tile_size + local_x;
                let noise = pixel_noise(global_x as u32, global_y as u32);
                let composite = composite_nodes(nodes, idx, Rgba::TRANSPARENT, noise);
                out.pixels[dst_y * dst_w + dst_x] = self.encode_tree(composite);
            }
        }
    }

    pub fn write_thumbnail_nearest(&self, max_edge: usize, out: &mut ColorImage) {
        let max_edge = max_edge.max(1);
        let longest = self.width.max(self.height).max(1);
        let scale = max_edge as f32 / longest as f32;
        let dst_w = ((self.width as f32 * scale).round() as usize).max(1);
        let dst_h = ((self.height as f32 * scale).round() as usize).max(1);

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        let mut pixel = ColorImage::new([1, 1], Color32::TRANSPARENT);
        for y in 0..dst_h {
            let src_y = (y * self.height / dst_h).min(self.height.saturating_sub(1));
            for x in 0..dst_w {
                let src_x = (x * self.width / dst_w).min(self.width.saturating_sub(1));
                self.write_region_to_color_image(src_x, src_y, 1, 1, &mut pixel, 1);
                out.pixels[y * dst_w + x] = pixel.pixels[0];
            }
        }
    }

    fn try_write_single_tile_fast(
        &self,
        tx: i32,
        ty: i32,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
        out: &mut ColorImage,
    ) -> bool {
        // Gamma documents with plain Normal layers get this path too.
        if !self.is_plain_stack() {
            return false;
        }
        let gamma = self.blend_space == BlendSpace::Gamma;
        let mut bg_visible = false;
        let mut paint_layer = None;
        for (idx, layer) in self.layers.iter().enumerate() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if idx == 0 {
                bg_visible = true;
            } else if paint_layer.is_none() && layer.opacity >= 1.0 {
                paint_layer = Some(idx);
            } else {
                return false;
            }
        }

        let bg_arc = bg_visible
            .then(|| self.layer_tile_cell(0, tx, ty))
            .flatten();
        let bg_guard = bg_arc
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let bg_data = bg_guard.as_ref().and_then(|guard| guard.data.as_ref());

        let paint_arc = paint_layer.and_then(|idx| self.layer_tile_cell(idx, tx, ty));
        let paint_guard = paint_arc
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let paint_data = paint_guard
            .as_ref()
            .filter(|guard| !guard.is_empty)
            .and_then(|guard| guard.data.as_ref());

        let w = x_range.len();
        let local_x = x_range.start % self.tile_size;
        let local_y = y_range.start % self.tile_size;

        // Blend in fixed-size chunks via the SIMD-batched alpha_over instead of one
        // call per pixel; alpha_over already returns `src`/`dst` exactly for the
        // alpha==255/0 cases internally, so no separate fast-path branch is needed.
        const CHUNK: usize = 64;
        let mut bg_buf = [Color32::TRANSPARENT; CHUNK];

        for row in 0..y_range.len() {
            let src_row = (local_y + row) * self.tile_size + local_x;
            let dst_row = row * w;

            let mut col = 0;
            while col < w {
                let n = CHUNK.min(w - col);
                for (k, slot) in bg_buf[..n].iter_mut().enumerate() {
                    *slot = if bg_visible {
                        bg_data.map_or(self.clear_color, |data| data[src_row + col + k])
                    } else {
                        Color32::TRANSPARENT
                    };
                }
                match paint_data {
                    Some(data) if gamma => {
                        let src = &data[src_row + col..src_row + col + n];
                        let dst = &mut out.pixels[dst_row + col..dst_row + col + n];
                        for ((o, &s), &b) in dst.iter_mut().zip(src).zip(&bg_buf[..n]) {
                            *o = gamma_over(s, b);
                        }
                    }
                    Some(data) => {
                        alpha_over_batch(
                            &data[src_row + col..src_row + col + n],
                            &bg_buf[..n],
                            &mut out.pixels[dst_row + col..dst_row + col + n],
                        );
                    }
                    None => {
                        out.pixels[dst_row + col..dst_row + col + n].copy_from_slice(&bg_buf[..n]);
                    }
                }
                col += n;
            }
        }
        true
    }

    /// Clear the active layer to the provided color (or transparent for non-background).
    pub fn clear(&mut self, color: Color) {
        self.clear_color = premultiply(color.to_color32());
        if let Some(layer) = self.layers.get(self.active_layer_idx) {
            let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                cell.data = None;
                cell.is_empty = true;
            }
        }
    }

    pub fn capture_layer_pixels(&self, layer_idx: usize) -> HashMap<(i32, i32), Vec<Color32>> {
        let Some(layer) = self.layers.get(layer_idx) else {
            return HashMap::new();
        };
        let cells: Vec<(TileKey, SharedCell)> = layer
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, c)| (k, c.clone()))
            .collect();
        // Copy the tiles in parallel (a whole 4K layer is ~64 MB).
        cells
            .par_iter()
            .filter_map(|(key, cell)| {
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                guard.data.clone().map(|d| (*key, d))
            })
            .collect()
    }

    /// Redraw floating layer `layer_idx` as `src_tiles` (whose content
    /// bounds are `src_bounds`, worked out once per session) transformed.
    pub fn preview_transform(
        &mut self,
        layer_idx: usize,
        src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
        src_bounds: eframe::egui::Rect,
        params: TransformParams,
    ) {
        let tile_size = self.tile_size;
        if src_tiles.len().saturating_mul(tile_size * tile_size) > MAX_TRANSFORM_SOURCE_PIXELS {
            log::warn!("Skipping transform preview: source is too large");
            return;
        }
        let dst_tiles = transform_tiles(
            src_tiles,
            src_bounds,
            params,
            tile_size,
            self.width,
            self.height,
            None,
        );
        // The preview replaces the layer's content: swap in fresh tiles
        // (built in parallel) rather than clearing and copying into the old.
        let fresh: TileMap = dst_tiles
            .into_par_iter()
            .map(|(key, data)| {
                let is_empty = data.iter().all(|p| p.a() == 0);
                (
                    key,
                    Arc::new(Mutex::new(TileCell {
                        data: Some(data),
                        is_empty,
                    })),
                )
            })
            .collect();
        if let Some(layer) = self.layers.get(layer_idx) {
            *layer.tiles.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
        }
    }

    /// Content bounds of detached tiles (a floating layer's source).
    pub fn tiles_content_bounds(
        &self,
        tiles: &HashMap<(i32, i32), Vec<Color32>>,
    ) -> Option<eframe::egui::Rect> {
        source_bounds_and_tiles(tiles, self.tile_size, None).map(|(b, _)| b)
    }

    pub fn apply_transform(
        &mut self,
        params: TransformParams,
        selection: Option<&crate::selection::SelectionManager>,
        history: Option<&mut UndoAction>,
    ) {
        let layer_idx = self.active_layer_idx;
        let tile_size = self.tile_size;
        let src_tiles = self.capture_layer_pixels(layer_idx);

        if src_tiles.len().saturating_mul(tile_size * tile_size) > MAX_TRANSFORM_SOURCE_PIXELS {
            log::warn!("Skipping transform: source layer is too large");
            return;
        }

        let Some((src_bounds, affected_src_tiles)) =
            source_bounds_and_tiles(&src_tiles, tile_size, selection)
        else {
            return;
        };

        let dst_tiles = transform_tiles(
            &src_tiles,
            src_bounds,
            params,
            tile_size,
            self.width,
            self.height,
            selection,
        );
        let tile_size_i32 = tile_size as i32;

        if let Some(layer) = self.layers.get(layer_idx) {
            let mut tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(action) = history {
                let mut affected_tiles = affected_src_tiles.clone();
                affected_tiles.extend(dst_tiles.keys().copied());

                for (tx, ty) in affected_tiles {
                    let data = if let Some(tile_arc) = tiles.get(&(tx, ty)) {
                        let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                        guard
                            .data
                            .clone()
                            .unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_size * tile_size])
                    } else {
                        vec![Color32::TRANSPARENT; tile_size * tile_size]
                    };

                    action.tiles.push(crate::canvas::history::TileSnapshot {
                        tx,
                        ty,
                        layer_id: layer.id,
                        x0: 0,
                        y0: 0,
                        width: tile_size,
                        height: tile_size,
                        data: data.into(),
                    });
                }
            }

            // Clear the moved pixels from their source tiles, in parallel.
            let sources: Vec<(i32, i32, &Vec<Color32>, SharedCell)> = src_tiles
                .iter()
                .filter(|(key, _)| affected_src_tiles.contains(key))
                .filter_map(|(&(tx, ty), data)| Some((tx, ty, data, tiles.get(&(tx, ty))?.clone())))
                .collect();
            sources
                .par_iter()
                .for_each(|(tx, ty, source_data, tile_arc)| {
                    let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(data) = &mut guard.data {
                        let base_x = *tx * tile_size_i32;
                        let base_y = *ty * tile_size_i32;
                        for py in 0..tile_size {
                            for px in 0..tile_size {
                                let idx = py * tile_size + px;
                                if source_data[idx].a() == 0 {
                                    continue;
                                }

                                let gx = base_x + px as i32;
                                let gy = base_y + py as i32;
                                if selection
                                    .is_none_or(|sel| sel.contains_coords(gx as f32, gy as f32))
                                {
                                    data[idx] = Color32::TRANSPARENT;
                                }
                            }
                        }
                        guard.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                    }
                });

            write_transformed_tiles(&mut tiles, dst_tiles, tile_size);
        }
    }

    pub fn get_content_bounds(
        &self,
        layer_idx: usize,
        selection: Option<&crate::selection::SelectionManager>,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let cells: Vec<(TileKey, SharedCell)> = layer
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&key, cell)| (key, cell.clone()))
            .collect();
        let ts = self.tile_size;
        // Each tile's [min_x, min_y, max_x, max_y] in parallel, then merged.
        cells
            .par_iter()
            .filter_map(|((tx, ty), cell)| {
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty {
                    return None;
                }
                let data = guard.data.as_ref()?;
                let mut b: Option<[i32; 4]> = None;
                for py in 0..ts {
                    for px in 0..ts {
                        if data[py * ts + px].a() == 0 {
                            continue;
                        }
                        let gx = tx * ts as i32 + px as i32;
                        let gy = ty * ts as i32 + py as i32;
                        if let Some(sel) = selection
                            && !sel.contains_coords(gx as f32, gy as f32)
                        {
                            continue;
                        }
                        let r = b.get_or_insert([gx, gy, gx, gy]);
                        *r = [r[0].min(gx), r[1].min(gy), r[2].max(gx), r[3].max(gy)];
                    }
                }
                b
            })
            .reduce_with(|a, b| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            })
            .map(|[min_x, min_y, max_x, max_y]| {
                eframe::egui::Rect::from_min_max(
                    eframe::egui::pos2(min_x as f32, min_y as f32),
                    eframe::egui::pos2(max_x as f32 + 1.0, max_y as f32 + 1.0),
                )
            })
    }

    /// Merge the specified layer down into the layer below it.
    /// This combines their tile data according to the visible pixels and opacity.
    /// The upper layer (source) is removed after the merge.
    pub fn float_selection(&mut self, selection: &SelectionManager) -> Option<usize> {
        if !selection.has_selection() {
            return None;
        }
        self.float_pixels(Some(selection))
    }

    /// Move the active layer's pixels inside `selection` (all of them for
    /// `None`) onto a new "floating" layer directly above it, so they can be
    /// transformed live; committing merges it back down. Returns its index,
    /// or `None` if there was nothing to float.
    pub fn float_pixels(&mut self, selection: Option<&SelectionManager>) -> Option<usize> {
        let active_idx = self.active_layer_idx;
        if active_idx >= self.layers.len() {
            return None;
        }
        let ts = self.tile_size;
        let cells: Vec<(TileKey, SharedCell)> = self.layers[active_idx]
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&key, cell)| (key, cell.clone()))
            .collect();
        // Split each tile into its floated and remaining pixels, in parallel.
        let floated: Vec<(TileKey, Vec<Color32>)> = cells
            .par_iter()
            .filter_map(|((tx, ty), cell)| {
                let mut tile = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = tile.data.as_mut()?;
                let mut lifted = vec![Color32::TRANSPARENT; ts * ts];
                let mut has_content = false;
                for y in 0..ts {
                    for x in 0..ts {
                        let idx = y * ts + x;
                        if data[idx] == Color32::TRANSPARENT {
                            continue;
                        }
                        let (px, py) = (tx * ts as i32 + x as i32, ty * ts as i32 + y as i32);
                        if selection.is_none_or(|sel| sel.contains(Vec2::new(px as f32, py as f32)))
                        {
                            lifted[idx] = data[idx];
                            data[idx] = Color32::TRANSPARENT;
                            has_content = true;
                        }
                    }
                }
                if has_content {
                    tile.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                }
                has_content.then_some(((*tx, *ty), lifted))
            })
            .collect();
        if floated.is_empty() {
            return None;
        }

        let new_layer_id = self.allocate_layer_id();
        let mut new_layer = Layer::new(
            new_layer_id,
            "Floating Selection".to_string(),
            self.width,
            self.height,
            ts,
        );
        new_layer.parent = self.layers[active_idx].parent;
        {
            let mut tiles = new_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for (key, data) in floated {
                tiles.insert(
                    key,
                    Arc::new(Mutex::new(TileCell {
                        data: Some(data),
                        is_empty: false,
                    })),
                );
            }
        }
        // Directly above the source, so committing merges back into it.
        self.layers.insert(active_idx + 1, new_layer);
        self.active_layer_idx = active_idx + 1;
        Some(self.active_layer_idx)
    }

    /// Pixels of layer `source` (everything visible when `None`) over the
    /// canvas rectangle `(x, y, w, h)`, row-major. Outside the canvas and
    /// unpainted tiles read as transparent.
    pub fn render_reference(
        &self,
        source: Option<usize>,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
    ) -> Vec<Color32> {
        let ts = self.tile_size as i32;
        let mut out = vec![Color32::TRANSPARENT; w * h];
        if w == 0 || h == 0 {
            return out;
        }
        // Every visible paint layer, bottom first, with its effective opacity
        // (folders included). Blend modes and masks are ignored: this only
        // has to show where the lines are, and plain "over" is fast.
        let layers: Vec<(usize, f32)> = match source {
            Some(idx) => vec![(idx, 1.0)],
            None => self.reference_layers(),
        };
        let (x1, y1) = (x + w as i32, y + h as i32);
        let mut tile = vec![Color32::TRANSPARENT; (ts * ts) as usize];
        for ty in y.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
            for tx in x.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
                tile.fill(Color32::TRANSPARENT);
                let mut any = false;
                for &(idx, opacity) in &layers {
                    let Some(cell) = self.layer_tile_cell(idx, tx, ty) else {
                        continue;
                    };
                    let cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(data) = cell.data.as_ref().filter(|_| !cell.is_empty) else {
                        continue;
                    };
                    if !any && opacity >= 1.0 {
                        tile.copy_from_slice(data);
                    } else {
                        for (d, &s) in tile.iter_mut().zip(data) {
                            let s = crate::canvas::blend::apply_opacity_scale(s, opacity);
                            *d = crate::canvas::blend::alpha_over(s, *d);
                        }
                    }
                    any = true;
                }
                if !any {
                    continue;
                }
                let (ox, oy) = (tx * ts, ty * ts);
                let (cx0, cx1) = (x.max(ox), x1.min(ox + ts));
                for py in y.max(oy)..y1.min(oy + ts) {
                    let src = ((py - oy) * ts + (cx0 - ox)) as usize;
                    let dst = ((py - y) as usize) * w + (cx0 - x) as usize;
                    let n = (cx1 - cx0) as usize;
                    out[dst..dst + n].copy_from_slice(&tile[src..src + n]);
                }
            }
        }
        out
    }

    /// Paint layers that show, bottom first, with opacity multiplied down
    /// through their folders.
    fn reference_layers(&self) -> Vec<(usize, f32)> {
        let by_id: HashMap<LayerId, &Layer> = self.layers.iter().map(|l| (l.id, l)).collect();
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == LayerKind::Paint && l.visible)
            .filter_map(|(i, l)| {
                let mut opacity = l.opacity;
                let mut parent = l.parent;
                while let Some(p) = parent.and_then(|id| by_id.get(&id)) {
                    if !p.visible {
                        return None;
                    }
                    opacity *= p.opacity;
                    parent = p.parent;
                }
                (opacity > 0.0).then_some((i, opacity))
            })
            .collect()
    }

    /// Replace every pixel of layer `layer_idx` by `f(state, pixel, x, y)`
    /// (inside the selection, if any), recording changed tiles in
    /// `history`. Each tile gets its own `state` from `init` (a cache, say).
    /// Returns the changed canvas area.
    pub fn map_layer_pixels<S>(
        &self,
        layer_idx: usize,
        selection: Option<&SelectionManager>,
        init: impl Fn() -> S + Sync,
        f: impl Fn(&mut S, Color32, i32, i32) -> Color32 + Sync,
        history: &mut UndoAction,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let layer_id = layer.id;
        let ts = self.tile_size;
        let cells: Vec<(TileKey, SharedCell)> = layer
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, c)| (k, c.clone()))
            .collect();
        let changed: Vec<(TileKey, TileSnapshot)> = cells
            .par_iter()
            .filter_map(|((tx, ty), cell)| {
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                if cell.is_empty {
                    return None;
                }
                let data = cell.data.as_mut()?;
                let before = data.clone();
                let mut inside = vec![true; ts];
                let mut any = false;
                let mut state = init();
                for ly in 0..ts {
                    let y = *ty * ts as i32 + ly as i32;
                    if y < 0 {
                        continue;
                    }
                    if let Some(sel) = selection {
                        sel.row_mask(y as usize, (*tx * ts as i32).max(0) as usize, &mut inside);
                    }
                    for (lx, _) in inside.iter().enumerate().filter(|(_, i)| **i) {
                        let i = ly * ts + lx;
                        let out = f(&mut state, data[i], *tx * ts as i32 + lx as i32, y);
                        if out != data[i] {
                            data[i] = out;
                            any = true;
                        }
                    }
                }
                if !any {
                    return None;
                }
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                Some((
                    (*tx, *ty),
                    TileSnapshot {
                        tx: *tx,
                        ty: *ty,
                        layer_id,
                        x0: 0,
                        y0: 0,
                        width: ts,
                        height: ts,
                        data: before.into(),
                    },
                ))
            })
            .collect();
        if changed.is_empty() {
            return None;
        }
        let (mut min, mut max) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for ((tx, ty), snap) in changed {
            min = (min.0.min(tx), min.1.min(ty));
            max = (max.0.max(tx + 1), max.1.max(ty + 1));
            history.tiles.push(snap);
        }
        let t = ts as f32;
        Some(eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(min.0 as f32 * t, min.1 as f32 * t),
            eframe::egui::pos2(max.0 as f32 * t, max.1 as f32 * t),
        ))
    }

    /// Write `pixels` (row-major, `w`×`h`) into layer `layer_idx` at `(x, y)`,
    /// the region given as `(x, y, w, h)`
    /// (clipped to the canvas). Each tile's content before its first write
    /// is saved into `before` (if given), for undo.
    pub fn write_layer_region(
        &self,
        layer_idx: usize,
        (x, y, w, h): (i32, i32, usize, usize),
        pixels: &[Color32],
        mut before: Option<&mut HashMap<(i32, i32), Vec<Color32>>>,
    ) {
        let ts = self.tile_size as i32;
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = (
            (x + w as i32).min(self.width as i32),
            (y + h as i32).min(self.height as i32),
        );
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for ty in y0 / ts..=(y1 - 1) / ts {
            for tx in x0 / ts..=(x1 - 1) / ts {
                let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) else {
                    continue;
                };
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; (ts * ts) as usize]);
                if let Some(before) = before.as_deref_mut() {
                    before.entry((tx, ty)).or_insert_with(|| data.clone());
                }
                let (ox, oy) = (tx * ts, ty * ts);
                let (cx0, cx1) = (x0.max(ox), x1.min(ox + ts));
                for py in y0.max(oy)..y1.min(oy + ts) {
                    let src = ((py - y) as usize) * w + (cx0 - x) as usize;
                    let dst = ((py - oy) * ts + (cx0 - ox)) as usize;
                    let n = (cx1 - cx0) as usize;
                    data[dst..dst + n].copy_from_slice(&pixels[src..src + n]);
                }
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
            }
        }
    }

    /// Paint `color` (unmultiplied) into layer `layer_idx` with `mask` as
    /// coverage, recording the touched tiles in `history`. Alpha-locked
    /// layers only recolour. Returns the changed canvas area.
    pub fn paint_mask(
        &self,
        layer_idx: usize,
        mask: &crate::selection::SelectionMask,
        color: Color32,
        history: &mut UndoAction,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let layer_id = layer.id;
        let alpha_lock = layer.alpha_locked;
        let ts = self.tile_size as i32;
        let [bx0, by0, bx1, by1] = mask.content_bounds()?;
        let (bx0, by0) = (bx0.max(0), by0.max(0));
        let (bx1, by1) = (bx1.min(self.width as i32), by1.min(self.height as i32));
        if bx1 <= bx0 || by1 <= by0 {
            return None;
        }
        let mut cells = Vec::new();
        for ty in by0 / ts..=(by1 - 1) / ts {
            for tx in bx0 / ts..=(bx1 - 1) / ts {
                if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
                    cells.push(((tx, ty), cell));
                }
            }
        }
        let tile_len = (ts * ts) as usize;
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        // The fill colour at every coverage level, premultiplied once.
        let colors: Vec<Color32> = (0..=255u32)
            .map(|cov| {
                let alpha = ((a as u32 * cov + 127) / 255) as u8;
                crate::canvas::blend::premultiply(Color32::from_rgba_unmultiplied(r, g, b, alpha))
            })
            .collect();
        let snapshots: Vec<TileSnapshot> = cells
            .par_iter()
            .filter_map(|&((tx, ty), ref cell)| {
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; tile_len]);
                let before = data.clone();
                let mut changed = false;
                for ly in 0..ts {
                    for lx in 0..ts {
                        let cov = mask.value(tx * ts + lx, ty * ts + ly);
                        if cov == 0 {
                            continue;
                        }
                        let i = (ly * ts + lx) as usize;
                        let dst = data[i];
                        if alpha_lock && dst.a() == 0 {
                            continue;
                        }
                        let src = colors[cov as usize];
                        let mut out = crate::canvas::blend::alpha_over(src, dst);
                        if alpha_lock {
                            out = crate::canvas::blend::with_alpha_of(out, dst.a());
                        }
                        changed |= out != dst;
                        data[i] = out;
                    }
                }
                if !changed {
                    return None;
                }
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                Some(TileSnapshot {
                    tx,
                    ty,
                    layer_id,
                    x0: 0,
                    y0: 0,
                    width: tile_len / ts as usize,
                    height: ts as usize,
                    data: before.into(),
                })
            })
            .collect();
        if snapshots.is_empty() {
            return None;
        }
        history.tiles.extend(snapshots);
        Some(eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(bx0 as f32, by0 as f32),
            eframe::egui::pos2(bx1 as f32, by1 as f32),
        ))
    }

    /// Paint over layer `layer_idx` inside `bounds` (`[x0, y0, x1, y1)`,
    /// canvas pixels) with the premultiplied colour `color_at(x, y)` returns
    /// for each pixel (transparent paints nothing), recording the touched
    /// tiles in `history`. Alpha-locked layers only recolour. Tiles are
    /// processed in parallel. Returns the changed canvas area.
    pub fn paint_region(
        &self,
        layer_idx: usize,
        bounds: [i32; 4],
        color_at: impl Fn(i32, i32) -> Color32 + Sync,
        history: &mut UndoAction,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let layer_id = layer.id;
        let alpha_lock = layer.alpha_locked;
        let ts = self.tile_size as i32;
        let [bx0, by0, bx1, by1] = bounds;
        let (bx0, by0) = (bx0.max(0), by0.max(0));
        let (bx1, by1) = (bx1.min(self.width as i32), by1.min(self.height as i32));
        if bx1 <= bx0 || by1 <= by0 {
            return None;
        }
        let mut cells = Vec::new();
        for ty in by0 / ts..=(by1 - 1) / ts {
            for tx in bx0 / ts..=(bx1 - 1) / ts {
                if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
                    cells.push(((tx, ty), cell));
                }
            }
        }
        let tile_len = (ts * ts) as usize;
        let snapshots: Vec<TileSnapshot> = cells
            .par_iter()
            .filter_map(|&((tx, ty), ref cell)| {
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; tile_len]);
                let mut before: Option<Vec<Color32>> = None;
                // Only the part of the tile inside the bounds.
                let (lx0, ly0) = ((bx0 - tx * ts).max(0), (by0 - ty * ts).max(0));
                let (lx1, ly1) = ((bx1 - tx * ts).min(ts), (by1 - ty * ts).min(ts));
                for ly in ly0..ly1 {
                    for lx in lx0..lx1 {
                        let src = color_at(tx * ts + lx, ty * ts + ly);
                        if src.a() == 0 {
                            continue;
                        }
                        let i = (ly * ts + lx) as usize;
                        let dst = data[i];
                        if alpha_lock && dst.a() == 0 {
                            continue;
                        }
                        let mut out = crate::canvas::blend::alpha_over(src, dst);
                        if alpha_lock {
                            out = crate::canvas::blend::with_alpha_of(out, dst.a());
                        }
                        if out != dst {
                            before.get_or_insert_with(|| data.clone());
                            data[i] = out;
                        }
                    }
                }
                let before = before?;
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                Some(TileSnapshot {
                    tx,
                    ty,
                    layer_id,
                    x0: 0,
                    y0: 0,
                    width: ts as usize,
                    height: ts as usize,
                    data: before.into(),
                })
            })
            .collect();
        if snapshots.is_empty() {
            return None;
        }
        history.tiles.extend(snapshots);
        Some(eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(bx0 as f32, by0 as f32),
            eframe::egui::pos2(bx1 as f32, by1 as f32),
        ))
    }

    /// Composite floating layer `float_idx` onto `target_idx` (normal blend,
    /// full opacity), remove it and make the target active. Returns the
    /// tiles it drew into.
    pub fn merge_floating(&mut self, float_idx: usize, target_idx: usize) -> Vec<TileKey> {
        if float_idx >= self.layers.len()
            || target_idx >= self.layers.len()
            || float_idx == target_idx
        {
            return Vec::new();
        }
        let ts = self.tile_size;
        // Pair each floating tile with its target cell, then blend in
        // parallel straight from the floating tiles (no copies).
        let floated: Vec<(TileKey, SharedCell)> = self.layers[float_idx]
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, c)| (k, c.clone()))
            .collect();
        let pairs: Vec<(TileKey, SharedCell, SharedCell)> = {
            let mut tiles = self.layers[target_idx]
                .tiles
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            floated
                .into_iter()
                .map(|(key, top)| {
                    let cell = tiles
                        .entry(key)
                        .or_insert_with(|| {
                            Arc::new(Mutex::new(TileCell {
                                data: None,
                                is_empty: true,
                            }))
                        })
                        .clone();
                    (key, top, cell)
                })
                .collect()
        };
        let touched: Vec<TileKey> = pairs
            .par_iter()
            .filter_map(|(key, top, cell)| {
                let top = top.lock().unwrap_or_else(|e| e.into_inner());
                let top_data = top.data.as_ref().filter(|_| !top.is_empty)?;
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let dst = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; ts * ts]);
                for (d, &t) in dst.iter_mut().zip(top_data) {
                    *d = crate::canvas::blend::alpha_over(t, *d);
                }
                cell.is_empty = dst.iter().all(|&p| p == Color32::TRANSPARENT);
                Some(*key)
            })
            .collect();
        self.layers.remove(float_idx);
        self.active_layer_idx = if target_idx > float_idx {
            target_idx - 1
        } else {
            target_idx
        };
        touched
    }

    /// Merge `layer_idx` down into the layer below it. If `history` is
    /// given, records enough to undo the merge: the bottom layer's
    /// pre-merge tile content (to reverse the blend), the top layer's full
    /// content (to restore it), and a `Removed` structural op describing
    /// the top layer itself (recreated as an empty shell on undo, before
    /// the tile snapshots refill both layers).
    pub fn merge_layer_down(&mut self, layer_idx: usize, mut history: Option<&mut UndoAction>) {
        if layer_idx == 0 || layer_idx >= self.layers.len() {
            return;
        }

        let active_before = self.active_layer_idx;
        let Some(top_id) = self.layer_id_at(layer_idx) else {
            return;
        };
        let Some(bottom_id) = self.layer_id_at(layer_idx - 1) else {
            return;
        };
        let top_meta = self.layer_meta_at(layer_idx);

        // Remove the top layer (source)
        let top_layer = self.layers.remove(layer_idx);
        let tile_size = self.tile_size;

        {
            // Get the bottom layer (destination)
            // Note: indices shifted after remove, so the layer that was at layer_idx - 1 is still at layer_idx - 1
            let bottom_layer = &mut self.layers[layer_idx - 1];

            let top_tiles = top_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut bottom_tiles = bottom_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut src_with_opacity = Vec::new();
            let mut blended = Vec::new();

            for ((tx, ty), top_tile_arc) in top_tiles.iter() {
                let top_guard = top_tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(top_data) = &top_guard.data {
                    // Skip empty top tiles
                    if top_guard.is_empty {
                        continue;
                    }

                    // Ensure bottom tile exists
                    let bottom_tile_arc = bottom_tiles.entry((*tx, *ty)).or_insert_with(|| {
                        Arc::new(Mutex::new(TileCell {
                            data: None,
                            is_empty: true,
                        }))
                    });

                    let mut bottom_guard =
                        bottom_tile_arc.lock().unwrap_or_else(|e| e.into_inner());

                    // Capture pre-merge state for undo, before either tile
                    // is touched: the bottom layer's current content (or
                    // transparent, matching what the init below would
                    // otherwise produce) and the top layer's full content.
                    if let Some(action) = history.as_deref_mut() {
                        let bottom_before = bottom_guard
                            .data
                            .clone()
                            .unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
                        action.tiles.push(TileSnapshot {
                            tx: *tx,
                            ty: *ty,
                            layer_id: bottom_id,
                            x0: 0,
                            y0: 0,
                            width: tile_size,
                            height: tile_size,
                            data: bottom_before.into(),
                        });
                        action.tiles.push(TileSnapshot {
                            tx: *tx,
                            ty: *ty,
                            layer_id: top_id,
                            x0: 0,
                            y0: 0,
                            width: tile_size,
                            height: tile_size,
                            data: top_data.clone().into(),
                        });
                    }

                    // Initialize bottom data if missing
                    if bottom_guard.data.is_none() {
                        bottom_guard.data =
                            Some(vec![Color32::TRANSPARENT; self.tile_size * self.tile_size]);
                    }

                    if let Some(bottom_data) = &mut bottom_guard.data {
                        // Use SIMD batch processing for better performance
                        let tile_len = bottom_data.len();

                        // Apply opacity to source pixels and prepare for batch blend
                        src_with_opacity.resize(tile_len, Color32::TRANSPARENT);
                        for i in 0..tile_len {
                            src_with_opacity[i] =
                                apply_opacity_scale(top_data[i], top_layer.opacity);
                        }

                        // Create temporary output buffer
                        blended.resize(tile_len, Color32::TRANSPARENT);

                        // Batch blend using SIMD
                        alpha_over_batch(&src_with_opacity, bottom_data, &mut blended);

                        // Copy result back
                        bottom_data.copy_from_slice(&blended);

                        // Update is_empty flag
                        bottom_guard.is_empty =
                            bottom_data.iter().all(|&p| p == Color32::TRANSPARENT);
                    }
                }
            }
        }

        // Adjust active layer index if needed
        if self.active_layer_idx >= self.layers.len() {
            self.active_layer_idx = self.layers.len() - 1;
        }

        if let (Some(action), Some(meta)) = (history, top_meta) {
            action.layer_action = Some(crate::canvas::history::LayerHistoryOp::Removed {
                index: layer_idx,
                id: top_id,
                meta,
                also: Vec::new(),
                active_before,
                active_after: self.active_layer_idx,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The previous single-threaded implementation, kept to check the
    /// parallel one maps every pixel identically.
    #[allow(clippy::too_many_arguments)]
    fn transform_tiles_reference(
        src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
        mut src_bounds: eframe::egui::Rect,
        params: TransformParams,
        tile_size: usize,
        canvas_width: usize,
        canvas_height: usize,
        selection: Option<&SelectionManager>,
    ) -> HashMap<(i32, i32), Vec<Color32>> {
        if params.scale.x.abs() < f32::EPSILON
            || params.scale.y.abs() < f32::EPSILON
            || !params.offset.is_finite()
            || !params.scale.is_finite()
            || !params.center.is_finite()
            || !params.rotation.is_finite()
        {
            return HashMap::new();
        }

        src_bounds.max.x += 1.0;
        src_bounds.max.y += 1.0;

        let corners = [
            src_bounds.min,
            eframe::egui::pos2(src_bounds.max.x, src_bounds.min.y),
            src_bounds.max,
            eframe::egui::pos2(src_bounds.min.x, src_bounds.max.y),
        ];

        let (sin_r, cos_r) = params.rotation.sin_cos();
        let transform = |p: eframe::egui::Pos2| -> eframe::egui::Pos2 {
            let dx = p.x - params.center.x;
            let dy = p.y - params.center.y;
            let sx = dx * params.scale.x;
            let sy = dy * params.scale.y;
            let rx = sx * cos_r - sy * sin_r;
            let ry = sx * sin_r + sy * cos_r;
            eframe::egui::pos2(
                rx + params.center.x + params.offset.x,
                ry + params.center.y + params.offset.y,
            )
        };

        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        for corner in corners.map(transform) {
            min_x = min_x.min(corner.x);
            min_y = min_y.min(corner.y);
            max_x = max_x.max(corner.x);
            max_y = max_y.max(corner.y);
        }

        let dst_min_x = min_x.floor() as i32;
        let dst_min_y = min_y.floor() as i32;
        let dst_max_x = max_x.ceil() as i32;
        let dst_max_y = max_y.ceil() as i32;
        let tile_size_i32 = tile_size as i32;
        let center_offset_x = params.center.x + params.offset.x;
        let center_offset_y = params.center.y + params.offset.y;
        let inv_scale_x = 1.0 / params.scale.x;
        let inv_scale_y = 1.0 / params.scale.y;
        let estimated_dst_tiles = ((dst_max_x - dst_min_x) * (dst_max_y - dst_min_y))
            / (tile_size_i32 * tile_size_i32)
            + 4;
        let mut dst_tiles = HashMap::with_capacity(estimated_dst_tiles.max(0) as usize);

        for y in dst_min_y..dst_max_y {
            if y < 0 || y >= canvas_height as i32 {
                continue;
            }
            for x in dst_min_x..dst_max_x {
                if x < 0 || x >= canvas_width as i32 {
                    continue;
                }
                let dx = x as f32 - center_offset_x;
                let dy = y as f32 - center_offset_y;
                let rx = dx * cos_r + dy * sin_r;
                let ry = -dx * sin_r + dy * cos_r;
                let src_x = (rx * inv_scale_x + params.center.x).round() as i32;
                let src_y = (ry * inv_scale_y + params.center.y).round() as i32;

                if src_x < src_bounds.min.x.floor() as i32
                    || src_x >= src_bounds.max.x.ceil() as i32
                    || src_y < src_bounds.min.y.floor() as i32
                    || src_y >= src_bounds.max.y.ceil() as i32
                {
                    continue;
                }

                let pixel = sample_source_tile(src_tiles, src_x, src_y, tile_size, selection);
                if pixel == Color32::TRANSPARENT {
                    continue;
                }

                let ntx = x.div_euclid(tile_size_i32);
                let nty = y.div_euclid(tile_size_i32);
                let npx = (x - ntx * tile_size_i32) as usize;
                let npy = (y - nty * tile_size_i32) as usize;
                let dst_data = dst_tiles
                    .entry((ntx, nty))
                    .or_insert_with(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
                dst_data[npy * tile_size + npx] = pixel;
            }
        }

        dst_tiles
    }

    #[test]
    fn parallel_transform_matches_the_reference() {
        let tile_size = 16;
        let mut src: HashMap<(i32, i32), Vec<Color32>> = HashMap::new();
        for ty in 0..4 {
            for tx in 0..4 {
                let data = (0..tile_size * tile_size)
                    .map(|i| {
                        let v = (i as u32).wrapping_mul(2654435761) ^ ((tx * 7 + ty * 13) as u32);
                        if v.is_multiple_of(5) {
                            Color32::TRANSPARENT
                        } else {
                            Color32::from_rgba_premultiplied(
                                v as u8,
                                (v >> 8) as u8,
                                (v >> 16) as u8,
                                255,
                            )
                        }
                    })
                    .collect();
                src.insert((tx, ty), data);
            }
        }
        let bounds = source_bounds_and_tiles(&src, tile_size, None).unwrap().0;
        let mut selection = SelectionManager::new();
        selection.start_selection(Vec2::new(5.0, 3.0), crate::selection::SelectionType::Circle);
        selection.update_selection(Vec2::new(50.0, 44.0));
        selection.end_selection();
        let cases = [
            TransformParams::new(
                Vec2::new(7.0, -3.0),
                0.0,
                Vec2::new(1.0, 1.0),
                Vec2::new(32.0, 32.0),
            ),
            TransformParams::new(
                Vec2::new(-4.5, 9.25),
                0.7,
                Vec2::new(1.3, 0.8),
                Vec2::new(30.0, 28.0),
            ),
            TransformParams::new(
                Vec2::new(20.0, 20.0),
                -2.1,
                Vec2::new(-0.6, 1.7),
                Vec2::new(10.0, 50.0),
            ),
        ];
        // Whole-pixel moves copy exactly, like the old nearest-neighbour code.
        // (The reference clips at the canvas edge; the real transform keeps
        // off-canvas pixels too, so compare what's on the canvas.)
        let on_canvas =
            |tiles: HashMap<(i32, i32), Vec<Color32>>| -> HashMap<(i32, i32), Vec<Color32>> {
                tiles
                    .into_iter()
                    .filter_map(|((tx, ty), mut data)| {
                        for (i, p) in data.iter_mut().enumerate() {
                            let x = tx * tile_size as i32 + (i % tile_size) as i32;
                            let y = ty * tile_size as i32 + (i / tile_size) as i32;
                            if !(0..70).contains(&x) || !(0..60).contains(&y) {
                                *p = Color32::TRANSPARENT;
                            }
                        }
                        data.iter().any(|p| p.a() > 0).then_some(((tx, ty), data))
                    })
                    .collect()
            };
        for sel in [None, Some(&selection)] {
            let fast = transform_tiles(&src, bounds, cases[0], tile_size, 70, 60, sel);
            let reference =
                transform_tiles_reference(&src, bounds, cases[0], tile_size, 70, 60, sel);
            assert_eq!(on_canvas(fast), on_canvas(reference));
        }
        // Moved partly off the canvas: those pixels are kept, not cut.
        let off = TransformParams::new(Vec2::new(-30.0, 0.0), 0.0, Vec2::new(1.0, 1.0), Vec2::ZERO);
        let moved = transform_tiles(&src, bounds, off, tile_size, 70, 60, None);
        assert!(moved.keys().any(|&(tx, _)| tx < 0), "off-canvas tiles kept");
        // Rotated/scaled/flipped transforms resample smoothly instead.
        for params in &cases[1..] {
            assert!(!transform_tiles(&src, bounds, *params, tile_size, 70, 60, None).is_empty());
        }
    }

    fn pixel_at(tiles: &HashMap<(i32, i32), Vec<Color32>>, x: i32, y: i32, ts: i32) -> Color32 {
        tiles
            .get(&(x.div_euclid(ts), y.div_euclid(ts)))
            .map_or(Color32::TRANSPARENT, |t| {
                t[(y.rem_euclid(ts) * ts + x.rem_euclid(ts)) as usize]
            })
    }

    fn pattern_tiles(ts: usize) -> HashMap<(i32, i32), Vec<Color32>> {
        let data = (0..ts * ts)
            .map(|i| {
                Color32::from_rgba_premultiplied((i * 3) as u8, (i * 7) as u8, (i * 11) as u8, 255)
            })
            .collect();
        HashMap::from([((0, 0), data)])
    }

    #[test]
    fn identity_distort_copies_pixels_exactly() {
        let ts = 16;
        let src = pattern_tiles(ts);
        let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
        let area =
            eframe::egui::Rect::from_min_max(bounds.min, bounds.max + eframe::egui::vec2(1.0, 1.0));
        let params = TransformParams::distorted(Distort {
            src: area,
            dst: rect_corners(area),
        });
        let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
        assert_eq!(out.get(&(0, 0)), src.get(&(0, 0)));
    }

    #[test]
    fn quarter_turn_maps_pixels_exactly() {
        let ts = 16;
        let src = pattern_tiles(ts);
        let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
        // About the tile centre (8, 8): pixel (x, y) goes to (15 - y, x).
        let params = TransformParams::new(
            Vec2::ZERO,
            std::f32::consts::FRAC_PI_2,
            Vec2::new(1.0, 1.0),
            Vec2::new(8.0, 8.0),
        );
        let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
        for (x, y) in [(0, 0), (3, 1), (15, 15), (7, 12)] {
            assert_eq!(
                pixel_at(&out, 15 - y, x, ts as i32),
                pixel_at(&src, x, y, ts as i32),
                "({x},{y})"
            );
        }
    }

    #[test]
    fn upscaling_blends_between_pixels() {
        let ts = 16;
        let mut data = vec![Color32::BLACK; ts * ts];
        data[0] = Color32::WHITE;
        let src = HashMap::from([((0, 0), data)]);
        let bounds = source_bounds_and_tiles(&src, ts, None).unwrap().0;
        let params = TransformParams::new(Vec2::ZERO, 0.0, Vec2::new(4.0, 4.0), Vec2::ZERO);
        let out = transform_tiles(&src, bounds, params, ts, 64, 64, None);
        let mid = pixel_at(&out, 4, 1, ts as i32);
        assert!(mid.r() > 0 && mid.r() < 255, "smooth edge, got {mid:?}");
    }

    const T: usize = 8;

    /// An 8x8 canvas (one tile): white background, layer 1 above it.
    fn one_tile_canvas() -> Canvas {
        Canvas::new(T, T, Color32::WHITE, T)
    }

    fn fill(canvas: &Canvas, idx: usize, color: Color32) {
        canvas.set_layer_tile_data(idx, 0, 0, vec![color; T * T]);
    }

    fn pixel(canvas: &Canvas) -> Color32 {
        let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(0, 0, 1, 1, &mut img, 1);
        img.pixels[0]
    }

    #[test]
    fn downsampled_composite_matches_composite_then_downsample() {
        use crate::canvas::blend::downsample;
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut random_tile = |n: usize| -> Vec<Color32> {
            (0..n)
                .map(|_| {
                    let v = next();
                    let a = match v % 4 {
                        0 => 0,
                        1 => 255,
                        _ => (v >> 24) as u8,
                    };
                    let pm = |c: u32| ((c & 0xff) * a as u32 / 255) as u8;
                    Color32::from_rgba_premultiplied(pm(v >> 1), pm(v >> 9), pm(v >> 17), a)
                })
                .collect()
        };
        // 100 px canvas, 64 px tiles: tile (1, 1) is a 36 px edge tile, so
        // blocks at its far edges are partial.
        for case in 0..3 {
            let mut canvas = Canvas::new(100, 100, Color32::WHITE, 64);
            for (tx, ty) in [(0, 0), (1, 1)] {
                canvas.set_layer_tile_data(1, tx, ty, random_tile(64 * 64));
            }
            match case {
                0 => {} // background + one opaque layer: the fast path
                1 => {
                    canvas.set_layer_tile_data(0, 1, 1, random_tile(64 * 64));
                    canvas.layers[1].opacity = 0.5; // general path
                }
                _ => {
                    let owner = canvas.layers[1].id;
                    let m = canvas.insert_new_layer(2, "m".into(), LayerKind::Mask { owner }, None);
                    let mi = canvas.layer_index_of(m).unwrap();
                    canvas.set_layer_tile_data(mi, 1, 1, random_tile(64 * 64));
                }
            }
            for (tx, ty, rect) in [
                (0, 0, [8, 16, 40, 48]),
                (1, 1, [0, 0, 36, 36]),
                (1, 1, [4, 8, 36, 36]),
            ] {
                for level in 1..=3u32 {
                    let block = 1usize << level;
                    let mut full = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    canvas.write_tile_rect_to_color_image(tx, ty, rect, &mut full, None);
                    let expected = downsample(&full, level);
                    let mut fused = ColorImage::new([0, 0], Color32::TRANSPARENT);
                    canvas.write_tile_rect_downsampled(tx, ty, rect, block, &mut fused, None);
                    assert_eq!(
                        fused.size, expected.size,
                        "case {case} rect {rect:?} level {level}"
                    );
                    for (i, (a, b)) in fused.pixels.iter().zip(&expected.pixels).enumerate() {
                        let close = a
                            .to_array()
                            .iter()
                            .zip(b.to_array())
                            .all(|(x, y)| x.abs_diff(y) <= 1);
                        assert!(
                            close,
                            "case {case} rect {rect:?} level {level} px {i}: {a:?} vs {b:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn half_black_over_white_depends_on_the_blend_space() {
        let mut canvas = one_tile_canvas();
        fill(&canvas, 1, Color32::BLACK);
        canvas.layers[1].opacity = 0.5;
        // Linear light: half the light of white, re-encoded to sRGB.
        assert_eq!(pixel(&canvas).r(), 188);
        // Gamma: half of the stored value.
        canvas.blend_space = BlendSpace::Gamma;
        assert_eq!(pixel(&canvas).r(), 128);
    }

    #[test]
    fn gamma_fast_path_matches_the_tree_compositor() {
        let mut seed = 0x1234_5678_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let data: Vec<Color32> = (0..T * T)
            .map(|_| {
                let v = next();
                let a = (v >> 24) as u8;
                let pm = |c: u32| ((c & 0xff) * a as u32 / 255) as u8;
                Color32::from_rgba_premultiplied(pm(v), pm(v >> 8), pm(v >> 16), a)
            })
            .collect();
        let mut canvas = one_tile_canvas();
        canvas.blend_space = BlendSpace::Gamma;
        canvas.set_layer_tile_data(1, 0, 0, data);
        let mut fast = ColorImage::new([0, 0], Color32::TRANSPARENT);
        canvas.write_tile_rect_to_color_image(0, 0, [0, 0, T, T], &mut fast, None);
        // Force the tree compositor with an empty folder.
        canvas.insert_new_layer(2, "empty".into(), LayerKind::Group, None);
        let mut tree = ColorImage::new([0, 0], Color32::TRANSPARENT);
        canvas.write_tile_rect_to_color_image(0, 0, [0, 0, T, T], &mut tree, None);
        assert_eq!(fast.pixels, tree.pixels);
    }

    #[test]
    fn multiply_layer_multiplies_stored_values_in_gamma_space() {
        let mut canvas = one_tile_canvas();
        canvas.blend_space = BlendSpace::Gamma;
        fill(&canvas, 1, Color32::from_rgb(200, 100, 50));
        canvas.insert_new_layer(2, "shade".into(), LayerKind::Paint, None);
        fill(&canvas, 2, Color32::from_rgb(128, 128, 128));
        canvas.layers[2].blend = LayerBlend::Multiply;
        let px = pixel(&canvas);
        // 200 * 128 / 255 = 100.4, 50.2, 25.1
        assert_eq!([px.r(), px.g(), px.b()], [100, 50, 25]);
    }

    #[test]
    fn multiply_folder_blends_its_whole_contents() {
        let mut canvas = one_tile_canvas();
        canvas.blend_space = BlendSpace::Gamma;
        fill(&canvas, 1, Color32::from_rgb(200, 100, 50));
        let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
        canvas.layers[2].blend = LayerBlend::Multiply;
        let inner = canvas.insert_new_layer(3, "inner".into(), LayerKind::Paint, Some(folder));
        let inner_idx = canvas.layer_index_of(inner).unwrap();
        fill(&canvas, inner_idx, Color32::from_rgb(128, 128, 128));
        let px = pixel(&canvas);
        assert_eq!([px.r(), px.g(), px.b()], [100, 50, 25]);
    }

    #[test]
    fn new_mask_shows_everything_and_black_hides() {
        let mut canvas = one_tile_canvas();
        fill(&canvas, 1, Color32::RED);
        let owner = canvas.layers[1].id;
        let mask = canvas.insert_new_layer(2, "mask".into(), LayerKind::Mask { owner }, None);
        assert_eq!(
            pixel(&canvas),
            Color32::RED,
            "a mask without tiles shows everything"
        );

        // A freshly created mask tile starts white (shows everything).
        let mask_idx = canvas.layer_index_of(mask).unwrap();
        canvas.ensure_layer_tile_exists(mask_idx, 0, 0);
        assert_eq!(pixel(&canvas), Color32::RED);

        fill(&canvas, mask_idx, Color32::BLACK);
        assert_eq!(pixel(&canvas), Color32::WHITE, "black mask hides the layer");
        fill(&canvas, mask_idx, Color32::TRANSPARENT);
        assert_eq!(
            pixel(&canvas),
            Color32::WHITE,
            "erased mask hides the layer"
        );

        canvas.layers[mask_idx].visible = false;
        assert_eq!(
            pixel(&canvas),
            Color32::RED,
            "a disabled mask masks nothing"
        );
    }

    #[test]
    fn folder_opacity_applies_to_its_contents_as_one() {
        // Reference: a single red layer at 50% over white.
        let mut flat = one_tile_canvas();
        fill(&flat, 1, Color32::RED);
        flat.layers[1].opacity = 0.5;
        let expected = pixel(&flat);

        // Blue under red, both opaque, in a 50% folder: the folder's composite
        // (all red) is what gets faded, so blue must not show through.
        let mut canvas = one_tile_canvas();
        let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
        canvas.layers[2].opacity = 0.5;
        fill(&canvas, 1, Color32::BLUE);
        canvas.layers[1].parent = Some(folder);
        let top = canvas.insert_new_layer(2, "top".into(), LayerKind::Paint, Some(folder));
        let top_idx = canvas.layer_index_of(top).unwrap();
        fill(&canvas, top_idx, Color32::RED);
        assert_eq!(pixel(&canvas), expected);
    }

    #[test]
    fn hidden_folder_hides_its_contents() {
        let mut canvas = one_tile_canvas();
        let folder = canvas.insert_new_layer(2, "folder".into(), LayerKind::Group, None);
        fill(&canvas, 1, Color32::RED);
        canvas.layers[1].parent = Some(folder);
        assert_eq!(pixel(&canvas), Color32::RED);
        canvas.layers[2].visible = false;
        assert_eq!(pixel(&canvas), Color32::WHITE);
    }

    #[test]
    fn floating_selection_sits_right_above_its_source() {
        let mut canvas = one_tile_canvas();
        canvas.insert_new_layer(2, "top".into(), LayerKind::Paint, None);
        fill(&canvas, 1, Color32::RED);
        canvas.active_layer_idx = 1;
        let mut selection = SelectionManager::new();
        selection.start_selection(
            Vec2::new(0.0, 0.0),
            crate::selection::SelectionType::Rectangle,
        );
        selection.update_selection(Vec2::new(4.0, 4.0));
        selection.end_selection();
        let idx = canvas.float_selection(&selection).unwrap();
        assert_eq!(
            idx, 2,
            "floated layer goes directly above layer 1, below 'top'"
        );
        assert_eq!(canvas.layers[3].name, "top");
    }

    #[test]
    fn zero_sized_region_clears_output() {
        let canvas = Canvas::new(8, 8, Color32::WHITE, 4);
        let mut image = ColorImage::new([2, 2], Color32::BLACK);

        canvas.write_region_to_color_image(0, 0, 0, 8, &mut image, 1);

        assert_eq!(image.size, [0, 0]);
        assert!(image.pixels.is_empty());
    }

    /// Deterministic, dependency-free hash (FNV-1a) over raw RGBA bytes. See
    /// the equivalent helper in brush_engine::brush::tests for why: a
    /// golden-master checksum lets a refactor of write_region_to_color_image
    /// be proven pixel-identical without a display to compare renders on.
    fn checksum_pixels(pixels: &[Color32]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for p in pixels {
            for b in p.to_array() {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        hash
    }

    /// 8x8 canvas (2x2 grid of 4x4 tiles), two layers, each tile given a
    /// distinct semi-transparent pattern so a tile-indexing bug in either
    /// layer would change the checksum.
    fn build_region_test_canvas() -> Canvas {
        let canvas = Canvas::new(8, 8, Color32::from_rgba_unmultiplied(230, 230, 230, 255), 4);
        for (li, base) in [(0usize, 10u8), (1usize, 60u8)] {
            for (tx, ty) in [(0i32, 0i32), (1, 0), (0, 1), (1, 1)] {
                let mut data = vec![Color32::TRANSPARENT; 16];
                for (i, px) in data.iter_mut().enumerate() {
                    let v = base
                        .wrapping_add((tx as u8) * 40)
                        .wrapping_add((ty as u8) * 20)
                        .wrapping_add(i as u8 * 3);
                    *px = Color32::from_rgba_unmultiplied(
                        v,
                        v.wrapping_add(50),
                        v.wrapping_add(90),
                        180,
                    );
                }
                canvas.set_layer_tile_data(li, tx, ty, data);
            }
        }
        canvas
    }

    /// Golden-master check for the single-tile fast path
    /// (`try_write_single_tile_fast`, step == 1, region within one tile).
    #[test]
    fn write_region_single_tile_step1_is_stable() {
        let canvas = build_region_test_canvas();
        let mut image = ColorImage::new([4, 4], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(0, 0, 4, 4, &mut image, 1);
        assert_eq!(
            checksum_pixels(&image.pixels),
            0xf9e95eb826f8c02f,
            "GOLDEN_PLACEHOLDER:write_region_single_tile_step1_is_stable"
        );
    }

    /// Golden-master check for the single-tile downsampling path (step > 1,
    /// still within one tile: falls through try_write_single_tile_fast into
    /// the "Fast path: Single tile access" block's step != 1 branch).
    #[test]
    fn write_region_single_tile_step2_is_stable() {
        let canvas = build_region_test_canvas();
        let mut image = ColorImage::new([2, 2], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(0, 0, 4, 4, &mut image, 2);
        assert_eq!(
            checksum_pixels(&image.pixels),
            0x14933deaac974042,
            "GOLDEN_PLACEHOLDER:write_region_single_tile_step2_is_stable"
        );
    }

    /// Golden-master check for the multi-tile fallback path, step == 1,
    /// covering the full 2x2 tile grid.
    #[test]
    fn write_region_multi_tile_step1_is_stable() {
        let canvas = build_region_test_canvas();
        let mut image = ColorImage::new([8, 8], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(0, 0, 8, 8, &mut image, 1);
        assert_eq!(
            checksum_pixels(&image.pixels),
            0xa3d9415bbd039077,
            "GOLDEN_PLACEHOLDER:write_region_multi_tile_step1_is_stable"
        );
    }

    /// Golden-master check for the multi-tile fallback path with step > 1.
    #[test]
    fn write_region_multi_tile_step2_is_stable() {
        let canvas = build_region_test_canvas();
        let mut image = ColorImage::new([4, 4], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(0, 0, 8, 8, &mut image, 2);
        assert_eq!(
            checksum_pixels(&image.pixels),
            0xd25bf6d6129a85bc,
            "GOLDEN_PLACEHOLDER:write_region_multi_tile_step2_is_stable"
        );
    }

    /// Golden-master check for a region that straddles tile boundaries
    /// without being canvas-aligned (offset start, spans 3 of the 4 tiles).
    #[test]
    fn write_region_offset_multi_tile_is_stable() {
        let canvas = build_region_test_canvas();
        let mut image = ColorImage::new([5, 5], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(2, 2, 5, 5, &mut image, 1);
        assert_eq!(
            checksum_pixels(&image.pixels),
            0xff2bb4312a21f9bd,
            "GOLDEN_PLACEHOLDER:write_region_offset_multi_tile_is_stable"
        );
    }

    #[test]
    fn transform_output_is_clipped_to_canvas() {
        let mut canvas = Canvas::new(8, 8, Color32::WHITE, 4);
        let mut data = vec![Color32::TRANSPARENT; 16];
        data[0] = Color32::BLACK;
        canvas.set_layer_tile_data(1, 0, 0, data);

        canvas.apply_transform(
            TransformParams::new(Vec2::new(-20.0, -20.0), 0.0, Vec2::splat(1.0), Vec2::ZERO),
            None,
            None,
        );

        let tiles = canvas.layers[1]
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(tiles.keys().all(|(tx, ty)| *tx >= 0 && *ty >= 0));
    }

    #[test]
    fn tile_composite_from_cached_below_matches_full_composite() {
        let mut canvas = build_region_test_canvas();
        canvas.add_layer();
        canvas.add_layer();
        for (li, base) in [(2usize, 120u8), (3usize, 200u8)] {
            for (tx, ty) in [(0i32, 0i32), (1, 1)] {
                let data = (0..16)
                    .map(|i| {
                        let v = base.wrapping_add(i as u8 * 7);
                        Color32::from_rgba_unmultiplied(v, 255 - v, v / 2, 40 + i as u8 * 12)
                    })
                    .collect();
                canvas.set_layer_tile_data(li, tx, ty, data);
            }
        }
        canvas.layers[1].opacity = 0.6;
        canvas.layers[3].opacity = 0.8;

        for active in 1..=3 {
            for step in [1, 2] {
                for (tx, ty) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
                    let mut full = ColorImage::new([1, 1], Color32::TRANSPARENT);
                    canvas.write_tile_to_color_image(tx, ty, &mut full, step, None);

                    let below = canvas.composite_below(active, tx as i32, ty as i32);
                    let mut cached = ColorImage::new([1, 1], Color32::TRANSPARENT);
                    let prefix = BelowComposite {
                        first_layer: active,
                        pixels: &below,
                    };
                    canvas.write_tile_to_color_image(tx, ty, &mut cached, step, Some(prefix));

                    assert_eq!(
                        full.pixels, cached.pixels,
                        "active={active} step={step} tile=({tx},{ty})"
                    );
                }
            }
        }
    }

    fn empty_action() -> UndoAction {
        UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        }
    }

    #[test]
    fn paint_region_paints_inside_its_bounds_across_tiles() {
        let canvas = Canvas::new(128, 128, Color32::WHITE, 64);
        let mut undo = empty_action();
        let red = Color32::from_rgb(255, 0, 0);
        let rect = canvas.paint_region(1, [60, 10, 70, 20], |_, _| red, &mut undo);
        assert!(rect.is_some());
        assert_eq!(undo.tiles.len(), 2, "one snapshot per touched tile");
        let left = canvas.get_layer_tile_data(1, 0, 0).unwrap();
        let right = canvas.get_layer_tile_data(1, 1, 0).unwrap();
        assert_eq!(left[15 * 64 + 63], red);
        assert_eq!(right[15 * 64 + 5], red);
        assert_eq!(
            right[15 * 64 + 6],
            Color32::TRANSPARENT,
            "outside the bounds"
        );
        assert_eq!(
            left[25 * 64 + 63],
            Color32::TRANSPARENT,
            "outside the bounds"
        );
    }

    #[test]
    fn paint_region_respects_alpha_lock_and_skips_unchanged_tiles() {
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut data = vec![Color32::TRANSPARENT; 64 * 64];
        data[0] = Color32::from_rgb(0, 0, 255);
        canvas.set_layer_tile_data(1, 0, 0, data);
        canvas.layers[1].alpha_locked = true;
        let mut undo = empty_action();
        let red = Color32::from_rgb(255, 0, 0);
        canvas.paint_region(1, [0, 0, 64, 64], |_, _| red, &mut undo);
        let tile = canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(tile[0], red, "painted pixel recoloured");
        assert_eq!(tile[1], Color32::TRANSPARENT, "empty pixel stays empty");

        let mut undo = empty_action();
        let none = canvas.paint_region(1, [0, 0, 64, 64], |_, _| Color32::TRANSPARENT, &mut undo);
        assert!(none.is_none());
        assert!(undo.tiles.is_empty());
    }
}
