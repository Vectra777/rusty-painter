//! Wet paint: water and the pigment it carries, on top of the paint
//! already dry, tile by tile. A wet brush lays them down when the pen
//! lifts; then, step by step, the water spreads (the paint bleeds), carries
//! pigment to the drying edges (they darken), runs downhill where it pools
//! (drips), and dries, leaving the pigment where it settled. A brush of
//! water alone wets dry paint again and lifts some of it.
//!
//! The layer's pixels always show it: the suspended pigment over the dry
//! paint. Nothing of it is saved: a document saved while wet keeps what it
//! shows, as dry paint.

use eframe::egui::{Color32, Vec2};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::sync::Mutex;

/// One simulation step, seconds.
pub const STEP: f64 = 1.0 / 30.0;
/// Water below this is dry.
const DRY: f32 = 1e-3;
/// Water above this runs downhill.
const POOLED: f32 = 0.6;

/// A wet brush's paint.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WetPaint {
    /// How much water a full dab leaves (0..1.5).
    pub water: f32,
    /// How much of the colour it carries (0: water alone, which wets and
    /// lifts paint already there).
    pub pigment: f32,
    /// Seconds to dry.
    pub drying: f32,
    /// How fast the water spreads (0..1).
    pub flow: f32,
    /// How much pigment gathers at the drying edges (0..1).
    pub edge_darkening: f32,
    /// How much dry paint water lifts back into the wash (0..1).
    pub lift: f32,
    /// How readily pooled water runs downhill (0: never).
    pub drips: f32,
}

impl Default for WetPaint {
    fn default() -> Self {
        Self {
            water: 1.0,
            pigment: 1.0,
            drying: 4.0,
            flow: 0.5,
            edge_darkening: 0.35,
            lift: 0.2,
            drips: 0.0,
        }
    }
}

/// Note tile `key` of layer `layer`'s wet paint as it was (`before`) in
/// `undo` (once per tile).
pub fn record_undo(
    undo: &mut crate::canvas::history::UndoAction,
    layer: crate::canvas::storage::LayerId,
    key: (i32, i32),
    before: Option<WetTile>,
) {
    use crate::canvas::history::LayerHistoryOp;
    match &mut undo.layer_action {
        Some(LayerHistoryOp::Wet {
            layer: l, tiles, ..
        }) if *l == layer => {
            if !tiles.iter().any(|(k, _)| *k == key) {
                tiles.push((key, before.map(Box::new)));
            }
        }
        other => {
            let inner = other.take().map(Box::new);
            *other = Some(LayerHistoryOp::Wet {
                layer,
                tiles: vec![(key, before.map(Box::new))],
                inner,
            });
        }
    }
}

/// Wet tiles as a step changes them (`None`: dry).
pub type WetTiles = Vec<((i32, i32), Option<Box<WetTile>>)>;

/// One wet tile.
#[derive(Clone, Debug, PartialEq)]
pub struct WetTile {
    pub water: Vec<f32>,
    /// Suspended pigment, premultiplied (0..1 each).
    pub pigment: Vec<[f32; 4]>,
    /// The paint under it, dry.
    pub dry: Vec<Color32>,
    /// What the layer was last shown (an edit since dries it as it is).
    pub shown: Vec<Color32>,
    /// How the paint on it behaves (the last brush's).
    pub paint: WetPaint,
}

impl WetTile {
    fn new(dry: Vec<Color32>, paint: WetPaint) -> Self {
        let n = dry.len();
        Self {
            water: vec![0.0; n],
            pigment: vec![[0.0; 4]; n],
            shown: dry.clone(),
            dry,
            paint,
        }
    }

    /// The pigment over the dry paint.
    fn show(&self) -> Vec<Color32> {
        (self.dry.iter().zip(&self.pigment))
            .map(|(&d, p)| over(*p, d))
            .collect()
    }

    /// Everything still suspended settles where it is.
    fn settle(&mut self) {
        for (d, p) in self.dry.iter_mut().zip(&mut self.pigment) {
            *d = over(*p, *d);
            *p = [0.0; 4];
        }
        self.water.fill(0.0);
    }
}

/// A share `f` of suspended pigment `p` settles into `dry`, looking the
/// same: what's left suspended, and the dry paint now (`p` over `dry` is
/// what's left over the new dry paint).
fn settle_share(p: [f32; 4], f: f32, dry: Color32) -> ([f32; 4], Color32) {
    if f <= 0.0 || p[3] <= 0.0 {
        return (p, dry);
    }
    if f >= 1.0 {
        return ([0.0; 4], over(p, dry));
    }
    let d = dry.to_array().map(|v| v as f32 / 255.0);
    let pa = p[3].min(1.0);
    let below = 1.0 - pa + pa * f;
    let new: [f32; 4] = std::array::from_fn(|k| (p[k] * f + d[k] * (1.0 - pa)) / below.max(1e-6));
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let a = byte(new[3]);
    let c = |v: f32| byte(v).min(a);
    (
        p.map(|v| v * (1.0 - f)),
        Color32::from_rgba_premultiplied(c(new[0]), c(new[1]), c(new[2]), a),
    )
}

/// `p` (premultiplied, 0..1) over `dry`.
fn over(p: [f32; 4], dry: Color32) -> Color32 {
    if p[3] <= 0.0 {
        return dry;
    }
    let d = dry.to_array().map(|v| v as f32 / 255.0);
    let k = 1.0 - p[3].min(1.0);
    let out: [f32; 4] = std::array::from_fn(|i| p[i] + d[i] * k);
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let a = byte(out[3]);
    let c = |v: f32| byte(v).min(a);
    Color32::from_rgba_premultiplied(c(out[0]), c(out[1]), c(out[2]), a)
}

/// A layer's wet paint.
#[derive(Debug, Default)]
pub struct WetLayer {
    tiles: Mutex<FxHashMap<(i32, i32), WetTile>>,
}

/// What a step changed.
#[derive(Default)]
pub struct Stepped {
    /// Tiles whose pixels changed, and their new pixels.
    pub shown: Vec<((i32, i32), Vec<Color32>)>,
    /// Tiles the water reached that weren't wet: their pixels before.
    pub fresh: Vec<((i32, i32), Vec<Color32>)>,
}

impl WetLayer {
    fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<(i32, i32), WetTile>> {
        self.tiles.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Tile `key` as it is (for undo).
    pub fn tile(&self, key: (i32, i32)) -> Option<WetTile> {
        self.lock().get(&key).cloned()
    }

    /// Put tile `key` as it was (`None`: not wet), returning what was there.
    pub fn set_tile(&self, key: (i32, i32), tile: Option<WetTile>) -> Option<WetTile> {
        let mut tiles = self.lock();
        match tile {
            Some(t) => tiles.insert(key, t),
            None => tiles.remove(&key),
        }
    }

    /// Lay a wet stroke on tile `key`: `coverage` of `colour` (premultiplied,
    /// 0..1) over `before` (the tile's pixels before the stroke). Returns
    /// the tile's pixels now.
    pub fn lay(
        &self,
        key: (i32, i32),
        before: &[Color32],
        coverage: &[f32],
        colour: impl Fn(usize) -> [f32; 4],
        paint: WetPaint,
    ) -> Vec<Color32> {
        let mut tiles = self.lock();
        let tile = tiles
            .entry(key)
            .or_insert_with(|| WetTile::new(before.to_vec(), paint));
        tile.paint = paint;
        let load = paint.pigment.clamp(0.0, 1.0);
        let lift = paint.lift.clamp(0.0, 1.0);
        for (i, &c) in coverage.iter().enumerate() {
            if c <= 0.0 {
                continue;
            }
            tile.water[i] = tile.water[i].max(c * paint.water.clamp(0.0, 1.5));
            // Water lifts some of the dry paint back into the wash (it looks
            // the same: the paint left, under what's lifted, under the wash).
            if lift > 0.0 {
                let d = tile.dry[i].to_array().map(|v| v as f32 / 255.0);
                let up = d.map(|v| v * lift * c);
                let rest = 1.0 - up[3];
                let left: [f32; 4] = if rest > 1e-4 {
                    std::array::from_fn(|k| ((d[k] - up[k]) / rest).max(0.0))
                } else {
                    [0.0; 4]
                };
                let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
                let a = byte(left[3]);
                tile.dry[i] = Color32::from_rgba_premultiplied(
                    byte(left[0]).min(a),
                    byte(left[1]).min(a),
                    byte(left[2]).min(a),
                    a,
                );
                let p = tile.pigment[i];
                tile.pigment[i] = std::array::from_fn(|k| p[k] + up[k] * (1.0 - p[3]));
            }
            let col = colour(i);
            let a = col[3] * c * load;
            let p = &mut tile.pigment[i];
            for k in 0..4 {
                p[k] = col[k] * c * load + p[k] * (1.0 - a);
            }
        }
        tile.shown = tile.show();
        tile.shown.clone()
    }

    /// The paint goes on drying for `steps` steps of [`STEP`]. `pixels`
    /// gives a tile's pixels as the layer holds them (to start wetting a
    /// dry one, and to notice one edited meanwhile); `side` is the tiles'
    /// side; `gravity` which way is down (canvas pixels, any length: its
    /// direction).
    pub fn step(
        &self,
        steps: usize,
        side: usize,
        gravity: Vec2,
        pixels: impl Fn((i32, i32)) -> Option<Vec<Color32>> + Sync,
    ) -> Stepped {
        let mut out = Stepped::default();
        let mut tiles = self.lock();
        // Edited since it was last shown (a filter, a transform): dry as it is.
        tiles.retain(|&key, t| pixels(key).is_none_or(|p| p == t.shown));
        let g = if gravity.length_sq() > 0.0 {
            gravity.normalized()
        } else {
            Vec2::ZERO
        };
        let mut changed: std::collections::BTreeSet<(i32, i32)> = Default::default();
        for _ in 0..steps {
            if tiles.is_empty() {
                break;
            }
            // Water at a tile's edge spills into the dry tile next to it.
            let mut spill: Vec<((i32, i32), WetPaint)> = Vec::new();
            for (&(tx, ty), t) in tiles.iter() {
                let edge = |x: usize, y: usize| t.water[y * side + x] > DRY;
                let sides = [
                    ((-1, 0), (0..side).any(|y| edge(0, y))),
                    ((1, 0), (0..side).any(|y| edge(side - 1, y))),
                    ((0, -1), (0..side).any(|x| edge(x, 0))),
                    ((0, 1), (0..side).any(|x| edge(x, side - 1))),
                ];
                for ((dx, dy), wet) in sides {
                    let key = (tx + dx, ty + dy);
                    if wet && !tiles.contains_key(&key) && !spill.iter().any(|(k, _)| *k == key) {
                        spill.push((key, t.paint));
                    }
                }
            }
            for (key, paint) in spill {
                // (Off the canvas there are no pixels: it stays dry.)
                if let Some(before) = pixels(key) {
                    out.fresh.push((key, before.clone()));
                    tiles.insert(key, WetTile::new(before, paint));
                }
            }
            let old = &*tiles;
            let next: Vec<((i32, i32), WetTile)> = old
                .par_iter()
                .map(|(&key, t)| (key, step_tile(old, key, t, side, g)))
                .collect();
            for (key, t) in next {
                changed.insert(key);
                tiles.insert(key, t);
            }
            // Dry through: settled, and wet no longer.
            let dried: Vec<(i32, i32)> = (tiles.iter())
                .filter(|(_, t)| t.water.iter().all(|&w| w <= DRY))
                .map(|(&k, _)| k)
                .collect();
            for key in dried {
                if let Some(mut t) = tiles.remove(&key) {
                    t.settle();
                    out.shown.push((key, t.dry));
                    changed.remove(&key);
                }
            }
        }
        // What each changed tile shows now (worked out across the cores).
        let shown: Vec<((i32, i32), Vec<Color32>)> = changed
            .into_par_iter()
            .filter_map(|key| tiles.get(&key).map(|t| (key, t.show())))
            .collect();
        for (key, pixels) in shown {
            if let Some(t) = tiles.get_mut(&key) {
                t.shown.clone_from(&pixels);
            }
            out.shown.push((key, pixels));
        }
        out
    }

    /// Dry it all now, where it is: the tiles and their pixels.
    pub fn dry_now(&self) -> Vec<((i32, i32), Vec<Color32>)> {
        let mut tiles = self.lock();
        tiles
            .drain()
            .map(|(k, mut t)| {
                t.settle();
                (k, t.dry)
            })
            .collect()
    }
}

/// Tile `key` (`t`) one step on, reading its neighbours in `tiles` (not
/// wet: dry paper).
fn step_tile(
    tiles: &FxHashMap<(i32, i32), WetTile>,
    key: (i32, i32),
    t: &WetTile,
    side: usize,
    g: Vec2,
) -> WetTile {
    let s = side as i32;
    // The tile's water and pigment with a pixel of its neighbours' round
    // it (none: dry paper), read by index from here on.
    let w2 = side + 2;
    let mut pad_w = vec![0.0f32; w2 * w2];
    let mut pad_p = vec![[0.0f32; 4]; w2 * w2];
    for y in 0..side {
        let (src, dst) = (y * side, (y + 1) * w2 + 1);
        pad_w[dst..dst + side].copy_from_slice(&t.water[src..src + side]);
        pad_p[dst..dst + side].copy_from_slice(&t.pigment[src..src + side]);
    }
    let border = [
        (-1, 0),
        (1, 0),
        (0, -1),
        (0, 1),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (1, 1),
    ];
    for (dx, dy) in border {
        let Some(n) = tiles.get(&(key.0 + dx, key.1 + dy)) else {
            continue;
        };
        // The neighbour's pixels next to this tile, and where they go.
        let xs: Vec<(usize, usize)> = match dx {
            -1 => vec![(side - 1, 0)],
            1 => vec![(0, side + 1)],
            _ => (0..side).map(|x| (x, x + 1)).collect(),
        };
        let ys: Vec<(usize, usize)> = match dy {
            -1 => vec![(side - 1, 0)],
            1 => vec![(0, side + 1)],
            _ => (0..side).map(|y| (y, y + 1)).collect(),
        };
        for &(sy, py) in &ys {
            for &(sx, px) in &xs {
                pad_w[py * w2 + px] = n.water[sy * side + sx];
                pad_p[py * w2 + px] = n.pigment[sy * side + sx];
            }
        }
    }
    let at = |x: i32, y: i32| -> (f32, [f32; 4]) {
        let i = (y + 1) as usize * w2 + (x + 1) as usize;
        (pad_w[i], pad_p[i])
    };
    let paint = t.paint;
    let d = paint.flow.clamp(0.0, 1.0) * 0.5;
    // (At most a fifth to each side: never all of it moves.)
    let edge = (paint.edge_darkening.clamp(0.0, 1.0) * d * 3.0).min(0.2);
    let drip = paint.drips.max(0.0) * 0.5;
    let dt = STEP as f32;
    let evaporate = dt / paint.drying.max(0.1);
    let mut next = t.clone();
    const NB: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    // Downhill: the neighbour water runs to, as weights along x and y.
    let down = [
        (g.x.signum() as i32, 0, g.x.abs()),
        (0, g.y.signum() as i32, g.y.abs()),
    ];
    for y in 0..s {
        for x in 0..s {
            let i = (y * s + x) as usize;
            let (w, p) = (t.water[i], t.pigment[i]);
            let nbs = NB.map(|(dx, dy)| at(x + dx, y + dy));
            if w <= DRY && nbs.iter().all(|n| n.0 <= DRY) {
                continue;
            }
            // Water evens out with its neighbours, creeping onto dry paper
            // only slowly (it soaks in).
            let avg_w = nbs
                .iter()
                .map(|n| if n.0 <= DRY { w * 0.8 } else { n.0 })
                .sum::<f32>()
                / 4.0;
            let mut nw = w + d * (avg_w - w);
            // Pigment moves with the water: it evens out where it's wet...
            let wet = |v: f32| (v * 2.0).clamp(0.0, 1.0);
            let mut np = p;
            for (nwat, npig) in nbs {
                // (Only where both are wet: the water has to get there first.)
                let k = d * 0.1 * wet(w.min(nwat));
                for c in 0..4 {
                    np[c] += k * (npig[c] - p[c]);
                }
                // ...and is carried towards thinner water (the edges darken;
                // never onto dry paper: that's the water spreading).
                if edge > 0.0 && w > DRY && nwat > DRY {
                    // (A small difference in water already draws it.)
                    let out = edge * (2.0 * (w - nwat).max(0.0) / w.max(1e-3)).min(1.0);
                    let inn = edge * (2.0 * (nwat - w).max(0.0) / nwat.max(1e-3)).min(1.0);
                    for c in 0..4 {
                        np[c] += inn * npig[c] - out * p[c];
                    }
                }
            }
            // Pooled water runs downhill, carrying its pigment.
            if drip > 0.0 {
                for (dx, dy, weight) in down {
                    if weight <= 0.0 || (dx, dy) == (0, 0) {
                        continue;
                    }
                    let out = drip * weight * (w - POOLED).max(0.0);
                    let (uw, up) = at(x - dx, y - dy);
                    let inn = drip * weight * (uw - POOLED).max(0.0);
                    nw += inn - out;
                    for c in 0..4 {
                        np[c] += inn / uw.max(1e-3) * up[c] - out / w.max(1e-3) * p[c];
                    }
                }
            }
            // Thinner water dries faster (the edges first), but only damp
            // paper keeps a little a while (so the water can creep).
            let thin = 1.0 + 2.0 * (1.0 - nw.min(1.0));
            nw = (nw - evaporate * thin * (nw / 0.2).min(1.0)).max(0.0);
            next.water[i] = nw;
            // As it dries the pigment settles: all of it once dry.
            let settle = if nw <= DRY {
                1.0
            } else {
                (evaporate / nw.max(evaporate)).clamp(0.0, 1.0) * 0.5
            };
            let np = np.map(|v| v.max(0.0));
            let (left, dry) = settle_share(np, settle, t.dry[i]);
            next.pigment[i] = left;
            next.dry[i] = dry;
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: usize = 16;

    fn white() -> Vec<Color32> {
        vec![Color32::WHITE; S * S]
    }

    /// A wash of blue across the middle of tile (0, 0), on white.
    fn wash(paint: WetPaint) -> WetLayer {
        let wet = WetLayer::default();
        let coverage: Vec<f32> = (0..S * S)
            .map(|i| ((4..12).contains(&(i % S)) && (4..12).contains(&(i / S))) as u8 as f32)
            .collect();
        wet.lay((0, 0), &white(), &coverage, |_| [0.0, 0.0, 0.8, 0.8], paint);
        wet
    }

    fn pixels(key: (i32, i32)) -> Option<Vec<Color32>> {
        (key.0.abs() <= 1 && key.1.abs() <= 1).then(white)
    }

    #[test]
    fn a_wash_spreads_darkens_at_its_edge_and_dries() {
        let wet = wash(WetPaint::default());
        let shown = |wet: &WetLayer| wet.tile((0, 0)).map(|t| t.show());
        let start = shown(&wet).unwrap();
        // (Its pixels as the layer has them: not edited.)
        let layer = |key: (i32, i32)| {
            if key == (0, 0) {
                Some(start.clone())
            } else {
                pixels(key)
            }
        };
        let s = wet.step(1, S, Vec2::ZERO, layer);
        assert!(s.shown.iter().any(|(k, _)| *k == (0, 0)));
        let t = wet.tile((0, 0)).unwrap();
        assert!(
            t.water[3 * S + 8] > 0.0,
            "the water spreads past the stroke"
        );
        // Dried through, in the drying time (and a little).
        let mut last = wet.tile((0, 0)).unwrap().show();
        for _ in 0..200 {
            let at = |key: (i32, i32)| {
                if key == (0, 0) {
                    Some(last.clone())
                } else {
                    pixels(key)
                }
            };
            let s = wet.step(1, S, Vec2::ZERO, at);
            if let Some((_, p)) = s.shown.iter().find(|(k, _)| *k == (0, 0)) {
                last = p.clone();
            }
            if wet.is_empty() {
                break;
            }
        }
        assert!(wet.is_empty(), "dry");
        // The rim (wherever the water took it) darker than the middle.
        let blue = |y: usize| 255 - last[y * S + 8].r() as i32;
        let rim = (2..6).map(blue).max().unwrap();
        assert!(rim > blue(8), "{:?}", (0..S).map(blue).collect::<Vec<_>>());
        // And bled a little past where it was laid.
        assert!(last[3 * S + 8] != Color32::WHITE);
    }

    #[test]
    fn pigment_is_kept_not_made() {
        let wet = wash(WetPaint {
            edge_darkening: 0.8,
            ..Default::default()
        });
        // How much paint shows: its coverage over the tile.
        let total = |w: &WetLayer| -> f32 {
            let t = w.tile((0, 0)).unwrap();
            t.show().iter().map(|c| c.a() as f32 / 255.0).sum()
        };
        let before = total(&wet);
        let shown = wet.tile((0, 0)).unwrap().shown;
        let at = |key: (i32, i32)| {
            if key == (0, 0) {
                Some(shown.clone())
            } else {
                None
            }
        };
        wet.step(3, S, Vec2::ZERO, at);
        let after = total(&wet);
        // (Some reaches past the tile, off the canvas here, and rounding.)
        assert!(
            after <= before * 1.1 && after > before * 0.9,
            "{before} → {after}"
        );
    }

    #[test]
    fn pooled_water_runs_downhill_not_up() {
        let paint = WetPaint {
            water: 1.5,
            drips: 1.0,
            drying: 30.0,
            ..Default::default()
        };
        let wet = wash(paint);
        let shown = wet.tile((0, 0)).unwrap().shown;
        let at = |key: (i32, i32)| {
            if key == (0, 0) {
                Some(shown.clone())
            } else {
                pixels(key)
            }
        };
        wet.step(20, S, Vec2::new(0.0, 1.0), at);
        let t = wet.tile((0, 0)).unwrap();
        let row = |y: usize| (4..12).map(|x| t.water[y * S + x]).sum::<f32>();
        assert!(
            row(13) > row(2) * 1.5,
            "below {} vs above {}",
            row(13),
            row(2)
        );
    }

    #[test]
    fn water_alone_lifts_dry_paint_and_an_edited_tile_dries_as_it_is() {
        let wet = WetLayer::default();
        let red = vec![Color32::from_rgb(200, 0, 0); S * S];
        let water = WetPaint {
            pigment: 0.0,
            lift: 0.5,
            ..Default::default()
        };
        wet.lay((0, 0), &red, &vec![1.0; S * S], |_| [0.0; 4], water);
        let t = wet.tile((0, 0)).unwrap();
        assert!(t.pigment[0][0] > 0.3, "red lifted into the wash");
        assert_eq!(
            t.shown[0],
            Color32::from_rgb(200, 0, 0),
            "looks the same at first"
        );
        // Edited meanwhile: no longer wet, left as it is.
        let s = wet.step(1, S, Vec2::ZERO, |_| Some(white()));
        assert!(wet.is_empty() && s.shown.is_empty());
    }
}
