//! Moved layers as the canvas shows them (see [`crate::canvas::motion`]):
//! a layer that's moved, turned, scaled or faded at the frame showing
//! keeps its own pixels where they were painted, and a posed copy of them
//! where they show (with their keyed look: blur, colour), which
//! compositing reads instead. The copy is made
//! again when the frame changes, and in part when the layer is painted.

use super::{Canvas, Layer, LayerKind, SharedCell, TileCell, TileMap};
use crate::canvas::motion::{Look, is_identity};
use crate::canvas::rig::Affine;
use eframe::egui::Color32;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::sync::{Arc, Mutex};

/// Tiles of a layer as painted, for sampling.
type Source = FxHashMap<(i32, i32), Vec<Color32>>;

/// A layer's shown copy, and the pose it was made for.
#[derive(Debug)]
pub(crate) struct Posed {
    map: TileMap,
    pose: Pose,
}

/// How a layer shows: moved, faded and with its look.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    affine: Affine,
    opacity: f32,
    look: Look,
}

impl Pose {
    fn is_plain(&self) -> bool {
        is_identity(&self.affine, self.opacity) && self.look.is_plain()
    }

    /// The radius of each of the three box blurs approximating its blur,
    /// and how far past a tile they reach.
    fn blur_reach(&self) -> (usize, usize) {
        if self.look.blur < 0.25 {
            return (0, 0);
        }
        let sigma = self.look.blur.min(200.0) / 2.0;
        let width = (4.0 * sigma * sigma + 1.0).sqrt();
        let radius = (((width - 1.0) / 2.0).round() as usize).max(1);
        (radius, radius * 3)
    }
}

impl Layer {
    /// Its tile `(tx, ty)` as shown, when it's shown moved (`Some(None)`:
    /// nothing shows there).
    pub(super) fn posed_tile(&self, tx: i32, ty: i32) -> Option<Option<SharedCell>> {
        let posed = self.posed.lock().unwrap_or_else(|e| e.into_inner());
        posed.as_ref().map(|p| p.map.get(&(tx, ty)).cloned())
    }

    /// Whether it shows moved.
    pub fn is_posed(&self) -> bool {
        self.posed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn set_posed(&self, posed: Option<Posed>) {
        *self.posed.lock().unwrap_or_else(|e| e.into_inner()) = posed;
    }

    /// The pose its shown copy was made for.
    fn posed_as(&self) -> Option<Pose> {
        (self
            .posed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref())
        .map(|p| p.pose)
    }
}

impl Canvas {
    /// Tile `(tx, ty)` of layer `idx` as the canvas shows it (moved, if it
    /// moves).
    pub(super) fn shown_tile_cell(&self, idx: usize, tx: i32, ty: i32) -> Option<SharedCell> {
        super::layer_tile(self.layers.get(idx)?, tx, ty)
    }

    /// A copy (sharing tiles) whose layers hold their pixels as they show
    /// at the frame showing, moved and with their look, and no keys: what
    /// merging or a layered export works from. `None` if no layer is
    /// moved or keyed (the canvas itself will do).
    pub fn shown_copy(&self) -> Option<Canvas> {
        if !self
            .layers
            .iter()
            .any(|l| l.motion.is_some() || l.is_posed())
        {
            return None;
        }
        let mut copy = self.shared_copy();
        for (layer, original) in copy.layers.iter_mut().zip(&self.layers) {
            let posed = (original.posed.lock().unwrap_or_else(|e| e.into_inner()))
                .as_ref()
                .map(|p| p.map.clone());
            if let Some(map) = posed {
                *layer.tiles.lock().unwrap_or_else(|e| e.into_inner()) = map;
            }
            layer.motion = None;
        }
        Some(copy)
    }

    /// Pose every layer for the frame showing: the moved ones get their
    /// posed copies, the others lose theirs. Returns whether any is moved.
    /// (After pixels changed: every copy is made again.)
    pub fn pose_motions(&self) -> bool {
        self.pose_layers(true)
    }

    /// [`Self::pose_motions`] after only the frame changed: a copy made for
    /// the same pose is kept (a rig layer's, whose pixels follow the frame,
    /// is made again).
    pub fn pose_for_time(&self) -> bool {
        self.pose_layers(false)
    }

    fn pose_layers(&self, fresh: bool) -> bool {
        if !self
            .layers
            .iter()
            .any(|l| l.motion.is_some() || l.is_posed())
        {
            return false;
        }
        let onion: Vec<usize> = if self.onion.enabled {
            (self.tracks().into_iter())
                .flat_map(|t| self.onion_frames(self.layers[t].id, self.time))
                .map(|(i, _)| i)
                .collect()
        } else {
            Vec::new()
        };
        let mut any = false;
        for i in 0..self.layers.len() {
            let layer = &self.layers[i];
            let shows = i != 0
                && matches!(layer.kind, LayerKind::Paint | LayerKind::Mask { .. })
                && (self.shown_in_time(i) || onion.contains(&i));
            let pose = match shows {
                true => self.pose_of(i),
                false => None,
            };
            let Some(pose) = pose else {
                layer.set_posed(None);
                continue;
            };
            any = true;
            let same =
                layer.posed_as() == Some(pose) && layer.rig.is_none() && layer.shader.is_none();
            if fresh || !same {
                let map = self.posed_tiles(i, &pose, None);
                layer.set_posed(Some(Posed { map, pose }));
            }
        }
        any
    }

    /// How layer `i` shows now, if not as painted (a mask takes its
    /// layer's motion, not its look).
    fn pose_of(&self, i: usize) -> Option<Pose> {
        let (affine, opacity) = self.world_motion(i);
        let look = if matches!(self.layers[i].kind, LayerKind::Mask { .. }) {
            Look::default()
        } else {
            self.world_look(i)
        };
        let pose = Pose {
            affine,
            opacity,
            look,
        };
        (!pose.is_plain()).then_some(pose)
    }

    /// Pose again the part of moved layer `i` its own pixels `rect` (x0,
    /// y0, x1, y1) show in, after painting there. Returns that part of the
    /// canvas, or `None` if the layer isn't moved.
    pub fn repose_region(&self, i: usize, rect: [f32; 4]) -> Option<[f32; 4]> {
        let layer = self.layers.get(i)?;
        if !layer.is_posed() {
            return None;
        }
        let pose = self.pose_of(i)?;
        let reach = pose.blur_reach().1 as f32;
        let shown = transformed_rect(&pose.affine, rect);
        let shown = [
            shown[0] - reach,
            shown[1] - reach,
            shown[2] + reach,
            shown[3] + reach,
        ];
        let ts = self.tile_size() as f32;
        let range = [
            (shown[0] / ts).floor() as i32,
            (shown[1] / ts).floor() as i32,
            ((shown[2] / ts).ceil() as i32).saturating_sub(1),
            ((shown[3] / ts).ceil() as i32).saturating_sub(1),
        ];
        let fresh = self.posed_tiles(i, &pose, Some(range));
        let mut posed = layer.posed.lock().unwrap_or_else(|e| e.into_inner());
        let posed = posed.get_or_insert_with(|| Posed {
            map: TileMap::default(),
            pose,
        });
        posed.pose = pose;
        let map = &mut posed.map;
        map.retain(|&(tx, ty), _| {
            !(tx >= range[0] && tx <= range[2] && ty >= range[1] && ty <= range[3])
        });
        map.extend(fresh);
        Some(shown)
    }

    /// Layer `i`'s pixels as `pose` shows them: the tiles where they land
    /// (only those in `only`, a range of tiles x0, y0, x1, y1, if given).
    fn posed_tiles(&self, i: usize, pose: &Pose, only: Option<[i32; 4]>) -> TileMap {
        let mut out = TileMap::default();
        let Some(inverse) = pose.affine.inverse() else {
            return out;
        };
        let mask = matches!(self.layers[i].kind, LayerKind::Mask { .. });
        let ts = self.tile_size();
        let source: Source = (self.layer_tile_keys(i).into_iter())
            .filter_map(|(tx, ty)| {
                let cell = self.layer_tile_cell(i, tx, ty)?;
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty && !mask {
                    return None;
                }
                Some(((tx, ty), guard.data()?.clone()))
            })
            .collect();
        if source.is_empty() {
            return out;
        }
        // Where its painted tiles land.
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(tx, ty) in source.keys() {
            x0 = x0.min(tx);
            y0 = y0.min(ty);
            x1 = x1.max(tx + 1);
            y1 = y1.max(ty + 1);
        }
        let painted = [
            (x0 * ts as i32) as f32,
            (y0 * ts as i32) as f32,
            (x1 * ts as i32) as f32,
            (y1 * ts as i32) as f32,
        ];
        let reach = pose.blur_reach().1 as f32;
        let shown = transformed_rect(&pose.affine, painted);
        let shown = [
            shown[0] - reach,
            shown[1] - reach,
            shown[2] + reach,
            shown[3] + reach,
        ];
        let (w, h) = (self.width() as i32, self.height() as i32);
        let tsf = ts as f32;
        let mut range = [
            ((shown[0] / tsf).floor() as i32).max(0),
            ((shown[1] / tsf).floor() as i32).max(0),
            ((shown[2] / tsf).ceil() as i32)
                .saturating_sub(1)
                .min((w - 1).div_euclid(ts as i32)),
            ((shown[3] / tsf).ceil() as i32)
                .saturating_sub(1)
                .min((h - 1).div_euclid(ts as i32)),
        ];
        if let Some(o) = only {
            range = [
                range[0].max(o[0]),
                range[1].max(o[1]),
                range[2].min(o[2]),
                range[3].min(o[3]),
            ];
        }
        if range[0] > range[2] || range[1] > range[3] {
            return out;
        }
        let keys: Vec<(i32, i32)> = (range[1]..=range[3])
            .flat_map(|ty| (range[0]..=range[2]).map(move |tx| (tx, ty)))
            .collect();
        let missing = if mask {
            Color32::WHITE
        } else {
            Color32::TRANSPARENT
        };
        let tiles: Vec<((i32, i32), Vec<Color32>)> = keys
            .par_iter()
            .filter_map(|&(tx, ty)| {
                let pixels = pose_tile(&source, ts, &inverse, pose, (tx, ty), missing, mask);
                Some(((tx, ty), pixels?))
            })
            .collect();
        for (key, pixels) in tiles {
            out.insert(
                key,
                Arc::new(Mutex::new(TileCell::new(Some(pixels), false))),
            );
        }
        out
    }
}

/// The box around `rect` (x0, y0, x1, y1) moved by `affine`.
pub(crate) fn transformed_rect(affine: &Affine, rect: [f32; 4]) -> [f32; 4] {
    let corners = [
        [rect[0], rect[1]],
        [rect[2], rect[1]],
        [rect[2], rect[3]],
        [rect[0], rect[3]],
    ]
    .map(|p| affine.apply(p));
    let xs = corners.map(|p| p[0]);
    let ys = corners.map(|p| p[1]);
    let min = |v: [f32; 4]| v.into_iter().fold(f32::INFINITY, f32::min);
    let max = |v: [f32; 4]| v.into_iter().fold(f32::NEG_INFINITY, f32::max);
    [min(xs), min(ys), max(xs), max(ys)]
}

/// One tile of the posed copy, each pixel read (bilinearly) from where it
/// came from, blurred and coloured as `pose` says. `None` if nothing lands
/// there.
fn pose_tile(
    source: &Source,
    ts: usize,
    inverse: &Affine,
    pose: &Pose,
    (tx, ty): (i32, i32),
    missing: Color32,
    mask: bool,
) -> Option<Vec<Color32>> {
    let tsi = ts as i32;
    // A blur reads past the tile: those pixels are posed too.
    let (radius, pad) = if mask { (0, 0) } else { pose.blur_reach() };
    let side = ts + 2 * pad;
    let (ox, oy) = (tx * tsi - pad as i32, ty * tsi - pad as i32);
    // The part of the layer this tile reads, gathered into one buffer.
    let reads = transformed_rect(
        inverse,
        [
            ox as f32,
            oy as f32,
            (ox + side as i32) as f32,
            (oy + side as i32) as f32,
        ],
    );
    // (Shrunk a long way, a tile reads too much: read it pixel by pixel.)
    let (span_w, span_h) = (reads[2] - reads[0] + 4.0, reads[3] - reads[1] + 4.0);
    let small = span_w.is_finite()
        && span_h.is_finite()
        && span_w * span_h <= (16 * side * side) as f32
        && reads.iter().all(|v| v.abs() < 1e8);
    let (wx, wy) = if small {
        (reads[0].floor() as i32 - 1, reads[1].floor() as i32 - 1)
    } else {
        (0, 0)
    };
    let (ww, wh) = if small {
        (
            (reads[2].ceil() as i32 + 2 - wx).max(1) as usize,
            (reads[3].ceil() as i32 + 2 - wy).max(1) as usize,
        )
    } else {
        (0, 0)
    };
    let window = small.then(|| gather(source, ts, [wx, wy], [ww, wh], missing));
    let missing_px = [
        missing.r() as f32,
        missing.g() as f32,
        missing.b() as f32,
        missing.a() as f32,
    ];
    let mut cached: ((i32, i32), Option<&Vec<Color32>>) = ((i32::MIN, i32::MIN), None);
    let mut fetch = |x: i32, y: i32| -> [f32; 4] {
        let p = match &window {
            Some(w) => {
                let (lx, ly) = (x - wx, y - wy);
                if lx < 0 || ly < 0 || lx as usize >= ww || ly as usize >= wh {
                    return missing_px;
                }
                w[ly as usize * ww + lx as usize]
            }
            None => {
                let key = (x.div_euclid(tsi), y.div_euclid(tsi));
                if cached.0 != key {
                    cached = (key, source.get(&key));
                }
                match cached.1 {
                    Some(data) => data[(y.rem_euclid(tsi) * tsi + x.rem_euclid(tsi)) as usize],
                    None => missing,
                }
            }
        };
        [p.r() as f32, p.g() as f32, p.b() as f32, p.a() as f32]
    };
    // Moved by whole pixels, not turned or scaled: each pixel is one of
    // the layer's.
    let [a, b, c, d] = inverse.m;
    let whole = (a - 1.0).abs() < 1e-6
        && b.abs() < 1e-6
        && c.abs() < 1e-6
        && (d - 1.0).abs() < 1e-6
        && (inverse.t[0] - inverse.t[0].round()).abs() < 1e-4
        && (inverse.t[1] - inverse.t[1].round()).abs() < 1e-4;
    // Only moved: its rows copied as they are.
    if whole
        && pad == 0
        && !mask
        && pose.look.is_plain()
        && pose.opacity >= 1.0
        && let Some(w) = &window
    {
        let (dx, dy) = (inverse.t[0].round() as i32, inverse.t[1].round() as i32);
        let (sx, sy) = ((ox + dx - wx) as usize, (oy + dy - wy) as usize);
        let mut pixels = vec![Color32::TRANSPARENT; ts * ts];
        for row in 0..ts {
            let src = (sy + row) * ww + sx;
            pixels[row * ts..(row + 1) * ts].copy_from_slice(&w[src..src + ts]);
        }
        return pixels.iter().any(|p| p.a() > 0).then_some(pixels);
    }
    let mut buffer = vec![[0.0f32; 4]; side * side];
    if whole {
        let (dx, dy) = (inverse.t[0].round() as i32, inverse.t[1].round() as i32);
        for py in 0..side {
            for px in 0..side {
                buffer[py * side + px] = fetch(ox + px as i32 + dx, oy + py as i32 + dy);
            }
        }
    } else {
        for py in 0..side {
            for px in 0..side {
                let x = (ox + px as i32) as f32 + 0.5;
                let y = (oy + py as i32) as f32 + 0.5;
                let [sx, sy] = inverse.apply([x, y]);
                let (fx, fy) = (sx - 0.5, sy - 0.5);
                let (x0, y0) = (fx.floor(), fy.floor());
                let (u, v) = (fx - x0, fy - y0);
                let (x0, y0) = (x0 as i32, y0 as i32);
                let (x1, y1) = (x0.saturating_add(1), y0.saturating_add(1));
                let a = fetch(x0, y0);
                let b = fetch(x1, y0);
                let c = fetch(x0, y1);
                let d = fetch(x1, y1);
                buffer[py * side + px] = std::array::from_fn(|k| {
                    let top = a[k] + (b[k] - a[k]) * u;
                    let bottom = c[k] + (d[k] - c[k]) * u;
                    top + (bottom - top) * v
                });
            }
        }
    }
    if radius > 0 {
        for _ in 0..3 {
            box_blur(&mut buffer, side, radius);
        }
    }
    let plain = pose.look.is_plain();
    let mut pixels = vec![Color32::TRANSPARENT; ts * ts];
    let mut any = false;
    for py in 0..ts {
        for px in 0..ts {
            let mut p = buffer[(py + pad) * side + px + pad];
            if !mask {
                if !plain {
                    p = pose.look.apply(p);
                }
                p = p.map(|v| v * pose.opacity);
            }
            let [r, g, b, a] = p.map(|v| v.round().clamp(0.0, 255.0) as u8);
            let p = Color32::from_rgba_premultiplied(r.min(a), g.min(a), b.min(a), a);
            any |= p.a() > 0 || mask;
            pixels[py * ts + px] = p;
        }
    }
    any.then_some(pixels)
}

/// The layer's pixels over `size` from `origin` (where it has no tiles,
/// `missing`), row by row from its tiles.
fn gather(
    source: &Source,
    ts: usize,
    origin: [i32; 2],
    size: [usize; 2],
    missing: Color32,
) -> Vec<Color32> {
    let tsi = ts as i32;
    let ([x0, y0], [w, h]) = (origin, size);
    let (x1, y1) = (x0 + w as i32, y0 + h as i32);
    let mut out = vec![missing; w * h];
    for ty in y0.div_euclid(tsi)..=(y1 - 1).div_euclid(tsi) {
        for tx in x0.div_euclid(tsi)..=(x1 - 1).div_euclid(tsi) {
            let Some(data) = source.get(&(tx, ty)) else {
                continue;
            };
            let (ox, oy) = (tx * tsi, ty * tsi);
            let (cx0, cx1) = (x0.max(ox), x1.min(ox + tsi));
            let n = (cx1 - cx0) as usize;
            for y in y0.max(oy)..y1.min(oy + tsi) {
                let src = ((y - oy) * tsi + (cx0 - ox)) as usize;
                let dst = (y - y0) as usize * w + (cx0 - x0) as usize;
                out[dst..dst + n].copy_from_slice(&data[src..src + n]);
            }
        }
    }
    out
}

/// A box blur of `radius` across and then down a `side`² buffer (past its
/// edges counts as nothing).
fn box_blur(buffer: &mut [[f32; 4]], side: usize, radius: usize) {
    let width = (2 * radius + 1) as f32;
    let mut line = vec![[0.0f32; 4]; side];
    let mut run = |get: &dyn Fn(usize) -> usize, buffer: &mut [[f32; 4]]| {
        let mut sum = [0.0f32; 4];
        for k in 0..=radius.min(side - 1) {
            let p = buffer[get(k)];
            (0..4).for_each(|c| sum[c] += p[c]);
        }
        for i in 0..side {
            line[i] = sum.map(|v| v / width);
            if let Some(add) = (i + radius + 1 < side).then(|| buffer[get(i + radius + 1)]) {
                (0..4).for_each(|c| sum[c] += add[c]);
            }
            if i >= radius {
                let sub = buffer[get(i - radius)];
                (0..4).for_each(|c| sum[c] -= sub[c]);
            }
        }
        for i in 0..side {
            buffer[get(i)] = line[i];
        }
    };
    for row in 0..side {
        run(&|i| row * side + i, buffer);
    }
    for col in 0..side {
        run(&|i| i * side + col, buffer);
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::motion::{Motion, Prop};
    use eframe::egui::{Color32, ColorImage};

    fn pixel(canvas: &Canvas, x: usize, y: usize) -> Color32 {
        let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(x, y, 1, 1, &mut img, 1);
        img.pixels[0]
    }

    #[test]
    fn a_moved_layer_shows_moved_and_keeps_its_pixels() {
        let mut canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut motion = Motion::new([32.0, 32.0]);
        motion.set(Prop::Position, 0, [0.0, 0.0]);
        motion.set(Prop::Position, 10, [64.0, 0.0]);
        canvas.layers[1].motion = Some(Box::new(motion));
        assert!(!canvas.pose_motions(), "not moved at frame 0");
        assert_eq!(pixel(&canvas, 80, 10), Color32::WHITE);
        canvas.set_time(10);
        assert!(canvas.layers[1].is_posed());
        assert_eq!(pixel(&canvas, 80, 10), Color32::RED);
        assert_eq!(pixel(&canvas, 10, 10), Color32::WHITE);
        // Its own pixels are where they were painted.
        assert_eq!(
            canvas.get_layer_tile_data(1, 0, 0).unwrap()[0],
            Color32::RED
        );
        // Painted again: shown where it lands.
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::BLUE; 64 * 64]);
        let shown = canvas.repose_region(1, [0.0, 0.0, 64.0, 64.0]).unwrap();
        assert_eq!(shown[0], 64.0);
        assert_eq!(pixel(&canvas, 80, 10), Color32::BLUE);
        // Faded out.
        let m = canvas.layers[1].motion.as_mut().unwrap();
        m.set(Prop::Opacity, 10, [0.0, 0.0]);
        canvas.pose_motions();
        assert_eq!(pixel(&canvas, 80, 10), Color32::WHITE);
    }
}

#[cfg(test)]
mod look_tests {
    use crate::canvas::Canvas;
    use crate::canvas::motion::{Motion, Prop};
    use eframe::egui::{Color32, ColorImage};

    fn pixel(canvas: &Canvas, x: usize, y: usize) -> Color32 {
        let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(x, y, 1, 1, &mut img, 1);
        img.pixels[0]
    }

    /// A 64² canvas, a grey square on its left half.
    fn canvas_with(edit: impl FnOnce(&mut Motion)) -> Canvas {
        let mut canvas = Canvas::new(64, 64, Color32::BLACK, 64);
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        for y in 0..64 {
            for x in 0..32 {
                tile[y * 64 + x] = Color32::from_gray(100);
            }
        }
        canvas.set_layer_tile_data(1, 0, 0, tile);
        let mut motion = Motion::new([32.0, 32.0]);
        edit(&mut motion);
        canvas.layers[1].motion = Some(Box::new(motion));
        canvas.pose_motions();
        canvas
    }

    #[test]
    fn colour_effects_are_keyed_and_eased() {
        let canvas = canvas_with(|m| m.set(Prop::Tint, 0, [1.0, 0.0]));
        assert_eq!(
            pixel(&canvas, 10, 10),
            Color32::from_rgb(255, 150, 40),
            "tinted"
        );
        let mut canvas = canvas_with(|m| {
            m.set(Prop::Brightness, 0, [0.0, 0.0]);
            m.set(Prop::Brightness, 10, [0.5, 0.0]);
        });
        assert_eq!(
            pixel(&canvas, 10, 10),
            Color32::from_gray(100),
            "as painted at 0"
        );
        canvas.set_time(10);
        let lighter = pixel(&canvas, 10, 10);
        assert!(lighter.r() > 200, "{lighter:?}");
        let canvas = canvas_with(|m| m.set(Prop::Saturation, 0, [0.0, 0.0]));
        let p = pixel(&canvas, 10, 10);
        assert_eq!((p.r(), p.g()), (p.g(), p.b()), "grey stays grey");
    }

    #[test]
    fn a_blur_softens_the_edge_on_both_sides() {
        let canvas = canvas_with(|m| m.set(Prop::Blur, 0, [8.0, 0.0]));
        let inside = pixel(&canvas, 30, 10);
        let outside = pixel(&canvas, 34, 10);
        assert!(inside.r() < 100 && inside.r() > 30, "{inside:?}");
        assert!(outside.r() > 5 && outside.r() < 70, "{outside:?}");
        // Far from the edge it's as painted.
        assert!(pixel(&canvas, 16, 10).r() >= 95, "far from the edges");
    }

    #[test]
    fn a_keyed_pivot_moves_what_a_turn_goes_about() {
        let mut canvas = canvas_with(|m| {
            m.set(Prop::Rotation, 0, [180.0, 0.0]);
            m.set(Prop::Anchor, 0, [32.0, 32.0]);
            m.set(Prop::Anchor, 10, [16.0, 32.0]);
        });
        // Turned about the middle: the grey half is on the right.
        assert_eq!(pixel(&canvas, 50, 10), Color32::from_gray(100));
        assert_eq!(pixel(&canvas, 10, 10), Color32::BLACK);
        // About the grey half's middle: it stays on the left.
        canvas.set_time(10);
        assert_eq!(pixel(&canvas, 10, 10), Color32::from_gray(100));
        assert_eq!(pixel(&canvas, 50, 10), Color32::BLACK);
    }
}
