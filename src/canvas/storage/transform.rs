//! Moving pixels: affine and perspective transforms of a layer's tiles,
//! floating a selection into its own layer and merging it back.

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use eframe::egui::Color32;

use super::{Canvas, Layer, SharedCell, TileCell, TileKey, TileMap};
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;
use eframe::egui::Vec2;

const MAX_TRANSFORM_SOURCE_PIXELS: usize = 67_108_864;

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
    pub kind: DistortKind,
}

/// How the inside follows the four corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DistortKind {
    /// A plane seen at an angle: straight lines stay straight, and the far
    /// side is foreshortened (a homography).
    #[default]
    Perspective,
    /// Each corner pulled on its own: the picture is stretched evenly
    /// between them, with no foreshortening (bilinear).
    Bilinear,
}

/// Whether the quad is convex (and not folded or flat): the corners a
/// perspective or distort can go to.
pub fn is_convex_quad(q: &[Vec2; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b, c) = (q[i], q[(i + 1) % 4], q[(i + 2) % 4]);
        let cross = (b - a).x * (c - b).y - (b - a).y * (c - b).x;
        if cross.abs() < 1e-3 || !cross.is_finite() {
            return false;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Canvas point → source point for one transform, prepared once for
/// per-pixel use. `None` where nothing of the source lands.
pub enum InverseMap {
    Affine {
        center: Vec2,
        offset: Vec2,
        sin: f32,
        cos: f32,
        inv_scale: Vec2,
    },
    Perspective {
        h: [f64; 9],
        /// Sign of the homogeneous w inside the quad: points with the other
        /// sign lie beyond the horizon (they'd come back mirrored).
        sign: f64,
    },
    Bilinear {
        /// P(u, v) = a + b·u + c·v + d·u·v over the unit square.
        a: Vec2,
        b: Vec2,
        c: Vec2,
        d: Vec2,
        src: eframe::egui::Rect,
    },
}

impl InverseMap {
    #[inline]
    pub fn map(&self, p: Vec2) -> Option<Vec2> {
        match self {
            Self::Affine {
                center,
                offset,
                sin,
                cos,
                inv_scale,
            } => {
                let (dx, dy) = (p.x - center.x - offset.x, p.y - center.y - offset.y);
                let (rx, ry) = (dx * cos + dy * sin, -dx * sin + dy * cos);
                Some(Vec2::new(
                    rx * inv_scale.x + center.x,
                    ry * inv_scale.y + center.y,
                ))
            }
            Self::Perspective { h, sign } => {
                let (x, y) = (p.x as f64, p.y as f64);
                let w = h[6] * x + h[7] * y + h[8];
                if w * sign <= 1e-12 {
                    return None;
                }
                Some(Vec2::new(
                    ((h[0] * x + h[1] * y + h[2]) / w) as f32,
                    ((h[3] * x + h[4] * y + h[5]) / w) as f32,
                ))
            }
            Self::Bilinear { a, b, c, d, src } => {
                let (u, v) = inverse_bilinear(p - *a, *b, *c, *d)?;
                Some(Vec2::new(
                    src.min.x + u * src.width(),
                    src.min.y + v * src.height(),
                ))
            }
        }
    }
}

/// (u, v) in the unit square with a + b·u + c·v + d·u·v = a + `e`.
fn inverse_bilinear(e: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<(f32, f32)> {
    let cross = |p: Vec2, q: Vec2| p.x as f64 * q.y as f64 - p.y as f64 * q.x as f64;
    // v solves k2·v² + k1·v + k0 = 0.
    let k2 = cross(d, c);
    let k1 = cross(e, d) + cross(b, c);
    let k0 = cross(e, b);
    let solve_u = |v: f64| -> Option<f64> {
        let den_x = b.x as f64 + d.x as f64 * v;
        let den_y = b.y as f64 + d.y as f64 * v;
        if den_x.abs() > den_y.abs() {
            (den_x.abs() > 1e-12).then(|| (e.x as f64 - c.x as f64 * v) / den_x)
        } else {
            (den_y.abs() > 1e-12).then(|| (e.y as f64 - c.y as f64 * v) / den_y)
        }
    };
    // A little past the square too: the box runs through the edge pixels'
    // corners, and the pixels outside the source are dropped later anyway.
    const REACH: f64 = 0.25;
    let outside = |t: f64| (-t).max(t - 1.0).max(0.0);
    let candidates: [Option<f64>; 2] = if k2.abs() < 1e-9 * (k1.abs() + 1.0) {
        [(k1.abs() > 1e-12).then(|| -k0 / k1), None]
    } else {
        let disc = k1 * k1 - 4.0 * k0 * k2;
        if disc < 0.0 {
            return None;
        }
        let root = disc.sqrt();
        [
            Some((-k1 - root) / (2.0 * k2)),
            Some((-k1 + root) / (2.0 * k2)),
        ]
    };
    // Of two roots, the one on the picture (the other is on the fold of
    // the surface far outside it).
    candidates
        .into_iter()
        .flatten()
        .filter_map(|v| Some((solve_u(v)?, v)))
        .map(|(u, v)| (outside(u).max(outside(v)), u, v))
        .filter(|&(off, _, _)| off <= REACH)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, u, v)| (u as f32, v as f32))
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
        if let Some(d) = &self.distort {
            match d.kind {
                DistortKind::Perspective => {
                    if let Some(h) = homography(rect_corners(d.src), d.dst) {
                        return apply_homography(&h, p);
                    }
                }
                DistortKind::Bilinear => {
                    let u = (p.x - d.src.min.x) / d.src.width().max(1e-6);
                    let v = (p.y - d.src.min.y) / d.src.height().max(1e-6);
                    let [p0, p1, p2, p3] = d.dst;
                    let top = p0 + (p1 - p0) * u;
                    let bottom = p3 + (p2 - p3) * u;
                    return top + (bottom - top) * v;
                }
            }
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

    /// Canvas point → source point, prepared for per-pixel use; `None` if
    /// the transform can't be undone (flat or degenerate).
    pub fn inverse_map(&self) -> Option<InverseMap> {
        let Some(d) = self.distort else {
            if self.scale.x.abs() < f32::EPSILON || self.scale.y.abs() < f32::EPSILON {
                return None;
            }
            let (sin, cos) = self.rotation.sin_cos();
            return Some(InverseMap::Affine {
                center: self.center,
                offset: self.offset,
                sin,
                cos,
                inv_scale: Vec2::new(1.0 / self.scale.x, 1.0 / self.scale.y),
            });
        };
        match d.kind {
            DistortKind::Perspective => {
                let h = invert3(&homography(rect_corners(d.src), d.dst)?)?;
                let centre = (d.dst[0] + d.dst[1] + d.dst[2] + d.dst[3]) / 4.0;
                let w = h[6] * centre.x as f64 + h[7] * centre.y as f64 + h[8];
                (w.abs() > 1e-12).then_some(InverseMap::Perspective {
                    h,
                    sign: w.signum(),
                })
            }
            DistortKind::Bilinear => {
                let [p0, p1, p2, p3] = d.dst;
                Some(InverseMap::Bilinear {
                    a: p0,
                    b: p1 - p0,
                    c: p3 - p0,
                    d: p0 - p1 + p2 - p3,
                    src: d.src,
                })
            }
        }
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

pub(super) fn source_bounds_and_tiles(
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
pub(super) fn sample_source_tile(
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

pub(super) fn transform_tiles(
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
    let Some(inverse_map) = params.inverse_map() else {
        return HashMap::new();
    };
    let inverse = |p: Vec2| inverse_map.map(p);
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
                        let Some(p) = inverse(Vec2::new(x as f32 + 0.5, y as f32 + 0.5)) else {
                            continue;
                        };
                        source(p.x.floor() as i32, p.y.floor() as i32)
                    } else {
                        let Some(p) = inverse(Vec2::new(x as f32 + 0.5, y as f32 + 0.5)) else {
                            continue;
                        };
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

impl Canvas {
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
        // Composited the way the preview showed it (the document's space).
        let over = match self.blend_space {
            crate::canvas::blend_modes::BlendSpace::Linear => crate::canvas::blend::alpha_over,
            crate::canvas::blend_modes::BlendSpace::Gamma => super::composite::gamma_over,
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
                    *d = over(t, *d);
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
}
