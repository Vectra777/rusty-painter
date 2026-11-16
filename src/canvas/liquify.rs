//! Liquify: a per-pixel displacement field over the original layer.
//!
//! Output pixel `p` shows the original at `p + D(p)`, sampled bilinearly.
//! Brush operations edit `D` (never the pixels), so strokes build on each
//! other without resampling the image again and again, and Reconstruct can
//! ease any area back to the original.

use eframe::egui::{Color32, Vec2};
use rayon::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LiquifyMode {
    /// Drag pixels along with the brush.
    Push,
    TwirlCw,
    TwirlCcw,
    /// Pull pixels toward the centre (shrink).
    Pinch,
    /// Push pixels away from the centre (grow).
    Bloat,
    /// Ease back to the original.
    Reconstruct,
    /// Even out the distortion.
    Smooth,
    /// Break the picture into shards, each pushed its own way (Procreate's
    /// Crystals).
    Crystals,
    /// Pull the pixels on either side toward a line along the stroke
    /// (Procreate's Edge).
    Edge,
}

impl LiquifyMode {
    pub const ALL: [(LiquifyMode, &'static str); 9] = [
        (LiquifyMode::Push, "Push"),
        (LiquifyMode::TwirlCw, "Twirl ↻"),
        (LiquifyMode::TwirlCcw, "Twirl ↺"),
        (LiquifyMode::Pinch, "Pinch"),
        (LiquifyMode::Bloat, "Bloat"),
        (LiquifyMode::Crystals, "Crystals"),
        (LiquifyMode::Edge, "Edge"),
        (LiquifyMode::Smooth, "Smooth"),
        (LiquifyMode::Reconstruct, "Restore"),
    ];

    /// Modes that keep acting while the brush is held still (Push and Edge
    /// need the way the brush moves).
    pub fn is_continuous(self) -> bool {
        !matches!(self, LiquifyMode::Push | LiquifyMode::Edge)
    }
}

type Key = (i32, i32);

/// Reads pixels of a tiled map, remembering the last tile it looked up:
/// neighbouring reads almost always hit the same tile, which skips the
/// hash lookup (four per bilinear sample otherwise).
struct TileReader<'a, T: Copy> {
    map: &'a HashMap<Key, Vec<T>>,
    /// Tile size as a shift and mask (tiles are a power of two wide):
    /// `x >> shift` and `x & mask` are floor-division and its remainder,
    /// negatives included, without the cost of real divisions.
    shift: u32,
    mask: i32,
    empty: T,
    key: Key,
    tile: Option<&'a Vec<T>>,
}

impl<'a, T: Copy> TileReader<'a, T> {
    fn new(map: &'a HashMap<Key, Vec<T>>, ts: i32, empty: T) -> Self {
        debug_assert!(
            ts > 0 && (ts & (ts - 1)) == 0,
            "tile size must be a power of two"
        );
        Self {
            map,
            shift: ts.trailing_zeros(),
            mask: ts - 1,
            empty,
            key: (i32::MIN, i32::MIN),
            tile: None,
        }
    }

    #[inline]
    fn fetch(&mut self, key: Key) -> Option<&'a Vec<T>> {
        if key != self.key {
            self.key = key;
            self.tile = self.map.get(&key);
        }
        self.tile
    }

    #[inline]
    fn at(&mut self, x: i32, y: i32) -> T {
        let key = (x >> self.shift, y >> self.shift);
        match self.fetch(key) {
            Some(t) => t[(((y & self.mask) << self.shift) | (x & self.mask)) as usize],
            None => self.empty,
        }
    }

    /// The 2×2 block at `(x, y)`..`(x + 1, y + 1)`: one lookup when it sits
    /// inside a tile, as it nearly always does.
    #[inline]
    fn quad(&mut self, x: i32, y: i32) -> [T; 4] {
        let (lx, ly) = (x & self.mask, y & self.mask);
        if lx < self.mask && ly < self.mask {
            return match self.fetch((x >> self.shift, y >> self.shift)) {
                Some(t) => {
                    let i = ((ly << self.shift) | lx) as usize;
                    let w = 1usize << self.shift;
                    [t[i], t[i + 1], t[i + w], t[i + w + 1]]
                }
                None => [self.empty; 4],
            };
        }
        [
            self.at(x, y),
            self.at(x + 1, y),
            self.at(x, y + 1),
            self.at(x + 1, y + 1),
        ]
    }
}

/// Displacement at canvas point `p`, bilinear between pixel centres.
#[inline]
fn sample_field(reader: &mut TileReader<'_, Vec2>, p: Vec2) -> Vec2 {
    let (fx, fy) = (p.x - 0.5, p.y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (ax, ay) = (fx - x0, fy - y0);
    let [a, b, c, d] = reader.quad(x0 as i32, y0 as i32);
    (a * (1.0 - ax) + b * ax) * (1.0 - ay) + (c * (1.0 - ax) + d * ax) * ay
}

/// Where Crystals pushes the pixels around canvas point `p` (for a brush of
/// radius `r`), per unit of strength: the picture cut into shards (the
/// cells of a jittered grid about a third of the brush across, each point
/// in the cell of the nearest of their centres), each pushed its own way
/// and its own distance, so neighbouring shards tear apart.
fn shard_offset(p: Vec2, r: f32) -> Vec2 {
    let cell = (r * 0.35).max(4.0);
    let (gx, gy) = ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32);
    let mut best = (f32::MAX, (gx, gy));
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (cx, cy) = (gx + dx, gy + dy);
            let centre = Vec2::new(
                (cx as f32 + hash01(cx, cy, 0)) * cell,
                (cy as f32 + hash01(cx, cy, 1)) * cell,
            );
            let d = (p - centre).length_sq();
            if d < best.0 {
                best = (d, (cx, cy));
            }
        }
    }
    let (cx, cy) = best.1;
    let angle = hash01(cx, cy, 2) * std::f32::consts::TAU;
    let length = (0.25 + 0.75 * hash01(cx, cy, 3)) * cell * 0.3;
    Vec2::angled(angle) * length
}

/// A number in 0..1 that looks random, the same every time for `(x, y, k)`.
fn hash01(x: i32, y: i32, k: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ k.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Canvas pixels per field cell, each way. The displacement is smooth, so
/// storing it at half resolution (and interpolating) looks the same while
/// the brush maths does a quarter of the work.
const SCALE: i32 = 2;

/// Displacement at canvas point `p` (canvas pixels), bilinear between field
/// cell centres.
#[inline]
fn sample_at(reader: &mut TileReader<'_, Vec2>, p: Vec2) -> Vec2 {
    sample_field(reader, p / SCALE as f32)
}

/// Sparse displacement field: one `Vec2` (in canvas pixels) per
/// `SCALE`×`SCALE` canvas pixels, in touched tiles of `ts`×`ts` cells.
pub struct LiquifyField {
    ts: i32,
    /// Canvas size.
    width: i32,
    height: i32,
    /// Field size in cells.
    cells_w: i32,
    cells_h: i32,
    tiles: HashMap<Key, Vec<Vec2>>,
    /// How much of the displacement shows (the Adjust slider, 0..=1): the
    /// field itself keeps all of it.
    scale: f32,
    /// The selection's coverage, if liquify is kept inside it: pixels show
    /// the displacement only as far as they're selected.
    coverage: Option<std::sync::Arc<crate::selection::SelectionMask>>,
}

impl LiquifyField {
    pub fn new(tile_size: usize, width: usize, height: usize) -> Self {
        Self {
            ts: tile_size as i32,
            width: width as i32,
            height: height as i32,
            cells_w: (width as i32 + SCALE - 1) / SCALE,
            cells_h: (height as i32 + SCALE - 1) / SCALE,
            tiles: HashMap::new(),
            scale: 1.0,
            coverage: None,
        }
    }

    /// Keep the displacement inside `coverage` (the selection): none
    /// outside it, part of it where it's partly selected.
    pub fn confine_to(
        &mut self,
        coverage: Option<std::sync::Arc<crate::selection::SelectionMask>>,
    ) {
        self.coverage = coverage;
    }

    /// How much of the displacement shows, 0..=1 (rendering only).
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Show `scale` (0..=1) of the displacement. Returns the canvas area
    /// that changes, if any.
    pub fn set_scale(&mut self, scale: f32) -> Option<[i32; 4]> {
        let scale = scale.clamp(0.0, 1.0);
        if scale == self.scale {
            return None;
        }
        self.scale = scale;
        self.bounds()
    }

    /// Make the shown displacement the field's own (the scale back to 1),
    /// so strokes from here on build on what's shown.
    pub fn bake_scale(&mut self) {
        if self.scale == 1.0 {
            return;
        }
        let k = self.scale;
        for tile in self.tiles.values_mut() {
            for d in tile.iter_mut() {
                *d *= k;
            }
        }
        self.scale = 1.0;
    }

    /// The canvas area the field's tiles reach (with the bilinear margin),
    /// clipped to the canvas.
    pub fn bounds(&self) -> Option<[i32; 4]> {
        let ts = self.ts;
        let mut out: Option<[i32; 4]> = None;
        for &(tx, ty) in self.tiles.keys() {
            let r = [
                ((tx * ts - 1) * SCALE).max(0),
                ((ty * ts - 1) * SCALE).max(0),
                (((tx + 1) * ts + 1) * SCALE).min(self.width),
                (((ty + 1) * ts + 1) * SCALE).min(self.height),
            ];
            out = Some(match out {
                None => r,
                Some(a) => [
                    a[0].min(r[0]),
                    a[1].min(r[1]),
                    a[2].max(r[2]),
                    a[3].max(r[3]),
                ],
            });
        }
        out.filter(|r| r[0] < r[2] && r[1] < r[3])
    }

    /// The share of canvas pixel `(x, y)`'s displacement that shows.
    #[inline]
    fn shown(&self, x: i32, y: i32) -> f32 {
        match &self.coverage {
            Some(c) => self.scale * c.value(x, y) as f32 / 255.0,
            None => self.scale,
        }
    }

    /// Set every displacement in field tile `key` (tests).
    #[cfg(test)]
    pub fn fill_offset(&mut self, key: [i32; 2], d: Vec2) {
        let n = (self.ts * self.ts) as usize;
        self.tiles.insert((key[0], key[1]), vec![d; n]);
    }

    /// Displacement at canvas point `p`, interpolated between pixel centres.
    #[cfg(test)]
    pub fn sample(&self, p: Vec2) -> Vec2 {
        sample_at(&mut TileReader::new(&self.tiles, self.ts, Vec2::ZERO), p)
    }

    /// Apply one brush dab. `delta` is the brush movement (Push and Edge),
    /// `amount` the strength for this dab (0..1). Returns the changed canvas
    /// rectangle `[x0, y0, x1, y1)`.
    pub fn dab(
        &mut self,
        mode: LiquifyMode,
        center: Vec2,
        radius: f32,
        amount: f32,
        delta: Vec2,
    ) -> Option<[i32; 4]> {
        let r = radius.max(1.0);
        let k = SCALE as f32;
        // Field cells the dab reaches.
        let x0 = (((center.x - r) / k).floor() as i32).max(0);
        let y0 = (((center.y - r) / k).floor() as i32).max(0);
        let x1 = (((center.x + r) / k).ceil() as i32).min(self.cells_w);
        let y1 = (((center.y + r) / k).ceil() as i32).min(self.cells_h);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        // Edge pulls toward the line the brush moves along (none while
        // it's still).
        let along = delta.normalized();
        if mode == LiquifyMode::Edge && along == Vec2::ZERO {
            return None;
        }
        let ts = self.ts;
        let keys: Vec<Key> = (y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts))
            .flat_map(|ty| (x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts)).map(move |tx| (tx, ty)))
            .collect();
        let field = &*self;
        let updated: Vec<(Key, Vec<Vec2>)> = keys
            .par_iter()
            .filter_map(|&(tx, ty)| {
                let mut reader = TileReader::new(&field.tiles, ts, Vec2::ZERO);
                let mut tile = field
                    .tiles
                    .get(&(tx, ty))
                    .cloned()
                    .unwrap_or_else(|| vec![Vec2::ZERO; (ts * ts) as usize]);
                let mut changed = false;
                for ly in 0..ts {
                    let y = ty * ts + ly;
                    if y < y0 || y >= y1 {
                        continue;
                    }
                    for lx in 0..ts {
                        let x = tx * ts + lx;
                        if x < x0 || x >= x1 {
                            continue;
                        }
                        // The cell centre, in canvas pixels.
                        let p = Vec2::new((x as f32 + 0.5) * k, (y as f32 + 0.5) * k);
                        let d = (p - center).length() / r;
                        if d >= 1.0 {
                            continue;
                        }
                        let f = (1.0 - d * d).powi(2) * amount;
                        let i = (ly * ts + lx) as usize;
                        let new = match mode {
                            LiquifyMode::Push => {
                                let shift = delta * f;
                                sample_at(&mut reader, p - shift) - shift
                            }
                            LiquifyMode::TwirlCw | LiquifyMode::TwirlCcw => {
                                let sign = if mode == LiquifyMode::TwirlCw {
                                    -1.0
                                } else {
                                    1.0
                                };
                                // Small angles (|θ| ≤ 0.35): a short series
                                // is exact to ~1e-5 and cheaper than sin_cos.
                                let a = sign * f * 0.35;
                                let a2 = a * a;
                                let (s, c) = (a * (1.0 - a2 / 6.0), 1.0 - a2 * (0.5 - a2 / 24.0));
                                let v = p - center;
                                let q = center + Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c);
                                q - p + sample_at(&mut reader, q)
                            }
                            LiquifyMode::Pinch | LiquifyMode::Bloat => {
                                let k = if mode == LiquifyMode::Pinch {
                                    0.12
                                } else {
                                    -0.12
                                };
                                let q = center + (p - center) * (1.0 + k * f);
                                q - p + sample_at(&mut reader, q)
                            }
                            LiquifyMode::Crystals => {
                                let q = p + shard_offset(p, r) * f;
                                q - p + sample_at(&mut reader, q)
                            }
                            LiquifyMode::Edge => {
                                // The part across the line, shrunk toward
                                // it as Pinch shrinks toward the centre.
                                let v = p - center;
                                let across = v - along * v.dot(along);
                                let q = p + across * (0.12 * f);
                                q - p + sample_at(&mut reader, q)
                            }
                            // A few passes take a warp right back (it was a
                            // quarter as strong: hard to see it do anything).
                            LiquifyMode::Reconstruct => tile[i] * (1.0 - (f * 2.0).min(1.0)),
                            LiquifyMode::Smooth => {
                                // Neighbours a share of the brush away: one
                                // pixel away, a brush-wide warp barely changed.
                                let d = (r * 0.15).round().max(1.0) as i32;
                                let mut sum = Vec2::ZERO;
                                for (dx, dy) in [(-d, 0), (d, 0), (0, -d), (0, d)] {
                                    sum += reader.at(x + dx, y + dy);
                                }
                                let avg = (sum + tile[i]) / 5.0;
                                tile[i] + (avg - tile[i]) * (f * 0.8).min(1.0)
                            }
                        };
                        if new != tile[i] {
                            tile[i] = new;
                            changed = true;
                        }
                    }
                }
                changed.then_some(((tx, ty), tile))
            })
            .collect();
        if updated.is_empty() {
            return None;
        }
        for (key, tile) in updated {
            self.tiles.insert(key, tile);
        }
        // Canvas pixels whose interpolated displacement changed: the cells
        // plus one cell around (bilinear reach), clipped to the canvas.
        Some([
            ((x0 - 1) * SCALE).max(0),
            ((y0 - 1) * SCALE).max(0),
            ((x1 + 1) * SCALE).min(self.width),
            ((y1 + 1) * SCALE).min(self.height),
        ])
    }

    /// Render just the canvas pixels `rect` (`[x0, y0, x1, y1)`, row-major)
    /// from the original `source`, a few rows per parallel task.
    ///
    /// Per row, the field is first interpolated vertically once per cell
    /// column, so each pixel then only blends two neighbours horizontally
    /// (instead of four field reads per pixel).
    pub fn render_rect(&self, source: &HashMap<Key, Vec<Color32>>, rect: [i32; 4]) -> Vec<Color32> {
        let codec = codec();
        self.render_rect_with(source, rect, Color32::TRANSPARENT, |src, p| {
            bilinear(src, p, codec)
        })
    }

    /// [`Self::render_rect`] at full depth: `source` is premultiplied
    /// linear light.
    pub fn render_rect_deep(
        &self,
        source: &HashMap<Key, Vec<[f32; 4]>>,
        rect: [i32; 4],
    ) -> Vec<[f32; 4]> {
        self.render_rect_with(source, rect, [0.0; 4], bilinear_deep)
    }

    fn render_rect_with<T: Copy + PartialEq + Send + Sync>(
        &self,
        source: &HashMap<Key, Vec<T>>,
        rect: [i32; 4],
        empty: T,
        sample: impl Fn(&mut TileReader<'_, T>, Vec2) -> T + Sync,
    ) -> Vec<T> {
        const ROWS: usize = 4;
        let [x0, y0, x1, y1] = rect;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut out = vec![empty; w * h];
        if w == 0 {
            return out;
        }
        let k = SCALE as f32;
        // Cell coordinate of canvas pixel x's centre, relative to cell centres.
        let cell_x = |x: i32| (x as f32 + 0.5) / k - 0.5;
        let c0 = cell_x(x0).floor() as i32;
        let c1 = cell_x(x1 - 1).floor() as i32 + 1;
        // All of the displacement everywhere: as it is (no multiply).
        let plain = self.scale == 1.0 && self.coverage.is_none();
        out.par_chunks_mut(w * ROWS)
            .enumerate()
            .for_each(|(chunk, lines)| {
                let mut field = TileReader::new(&self.tiles, self.ts, Vec2::ZERO);
                let mut src = TileReader::new(source, self.ts, empty);
                let mut column = vec![Vec2::ZERO; (c1 - c0 + 1) as usize];
                for (row, line) in lines.chunks_mut(w).enumerate() {
                    let y = y0 + (chunk * ROWS + row) as i32;
                    let fy = (y as f32 + 0.5) / k - 0.5;
                    let cy = fy.floor();
                    let ay = fy - cy;
                    let cy = cy as i32;
                    let mut any = false;
                    for (i, c) in column.iter_mut().enumerate() {
                        let cx = c0 + i as i32;
                        let (top, bottom) = (field.at(cx, cy), field.at(cx, cy + 1));
                        *c = top * (1.0 - ay) + bottom * ay;
                        any |= *c != Vec2::ZERO;
                    }
                    any &= self.scale > 0.0;
                    for (i, px) in line.iter_mut().enumerate() {
                        let x = x0 + i as i32;
                        let d = if any {
                            let fx = cell_x(x);
                            let cx = fx.floor();
                            let ax = fx - cx;
                            let j = (cx as i32 - c0) as usize;
                            let d = column[j] * (1.0 - ax) + column[j + 1] * ax;
                            if plain { d } else { d * self.shown(x, y) }
                        } else {
                            Vec2::ZERO
                        };
                        *px = if d == Vec2::ZERO {
                            src.at(x, y)
                        } else {
                            sample(&mut src, Vec2::new(x as f32 + 0.5, y as f32 + 0.5) + d)
                        };
                    }
                }
            });
        out
    }

    /// Tile `(tx, ty)` (`w`×`h` canvas pixels) rendered `block` times
    /// smaller, for the screen while zoomed out: one sample at each block's
    /// middle, `w.div_ceil(block)` × `h.div_ceil(block)`, row-major (the
    /// layout [`crate::canvas::Canvas::write_tile_preview`] takes).
    pub fn render_tile_shrunk(
        &self,
        source: &HashMap<Key, Vec<Color32>>,
        (tx, ty): Key,
        block: usize,
        (w, h): (usize, usize),
    ) -> Vec<Color32> {
        let (dw, dh) = (w.div_ceil(block), h.div_ceil(block));
        let mut field = TileReader::new(&self.tiles, self.ts, Vec2::ZERO);
        let mut src = TileReader::new(source, self.ts, Color32::TRANSPARENT);
        let codec = codec();
        let (ox, oy) = ((tx * self.ts) as f32, (ty * self.ts) as f32);
        let half = block as f32 * 0.5;
        (0..dw * dh)
            .map(|i| {
                let p = Vec2::new(
                    ox + (i % dw * block) as f32 + half,
                    oy + (i / dw * block) as f32 + half,
                );
                let shown = self.shown(p.x as i32, p.y as i32);
                bilinear(&mut src, p + sample_at(&mut field, p) * shown, codec)
            })
            .collect()
    }

    /// Render the whole tiles covering `rect` (tests).
    #[cfg(test)]
    pub fn render(
        &self,
        source: &HashMap<Key, Vec<Color32>>,
        rect: [i32; 4],
    ) -> Vec<(Key, Vec<Color32>)> {
        let ts = self.ts;
        let [x0, y0, x1, y1] = rect;
        let mut out = Vec::new();
        for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
            for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
                let r = [tx * ts, ty * ts, (tx + 1) * ts, (ty + 1) * ts];
                out.push(((tx, ty), self.render_rect(source, r)));
            }
        }
        out
    }
}

/// The colour tables [`bilinear`] blends through, looked up once per
/// render rather than once a sample.
type Codec = (
    crate::canvas::blend::LinearDecoder,
    crate::canvas::blend::LinearEncoder,
);

fn codec() -> Codec {
    (
        crate::canvas::blend::LinearDecoder::new(),
        crate::canvas::blend::LinearEncoder::new(),
    )
}

/// Bilinear sample of the source at canvas point `p`, blended in linear
/// light as paint is elsewhere (blending the stored, gamma-encoded bytes
/// darkened soft edges).
fn bilinear(reader: &mut TileReader<'_, Color32>, p: Vec2, (dec, enc): Codec) -> Color32 {
    let (fx, fy) = (p.x - 0.5, p.y - 0.5);
    let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
    let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
    let [c00, c10, c01, c11] = reader.quad(x0, y0);
    // One colour all round (flat paint, or nothing): as it is.
    if c00 == c10 && c00 == c01 && c00 == c11 {
        return c00;
    }
    let px = [
        (c00, (1.0 - ax) * (1.0 - ay)),
        (c10, ax * (1.0 - ay)),
        (c01, (1.0 - ax) * ay),
        (c11, ax * ay),
    ];
    let mut acc = eframe::egui::Rgba::TRANSPARENT;
    for (c, w) in px {
        acc = acc + dec.decode(c) * w;
    }
    enc.encode(acc)
}

/// [`bilinear`] at full depth (premultiplied linear light).
fn bilinear_deep(reader: &mut TileReader<'_, [f32; 4]>, p: Vec2) -> [f32; 4] {
    let (fx, fy) = (p.x - 0.5, p.y - 0.5);
    let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
    let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
    let [c00, c10, c01, c11] = reader.quad(x0, y0);
    let w = [
        (1.0 - ax) * (1.0 - ay),
        ax * (1.0 - ay),
        (1.0 - ax) * ay,
        ax * ay,
    ];
    std::array::from_fn(|c| c00[c] * w[0] + c10[c] * w[1] + c01[c] * w[2] + c11[c] * w[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn gradient_source(ts: i32) -> HashMap<Key, Vec<Color32>> {
        let mut src = HashMap::new();
        for ty in 0..2 {
            for tx in 0..2 {
                let tile = (0..ts * ts)
                    .map(|i| {
                        let x = tx * ts + i % ts;
                        Color32::from_rgb((x * 2) as u8, 0, 0)
                    })
                    .collect();
                src.insert((tx, ty), tile);
            }
        }
        src
    }

    #[test]
    fn untouched_areas_render_exactly() {
        let field = LiquifyField::new(64, 128, 128);
        let src = gradient_source(64);
        let out = field.render(&src, [0, 0, 128, 128]);
        for (key, tile) in out {
            assert_eq!(&tile, &src[&key]);
        }
    }

    #[test]
    fn push_moves_content_along_the_stroke() {
        let mut field = LiquifyField::new(64, 128, 128);
        let src = gradient_source(64);
        field.dab(
            LiquifyMode::Push,
            Vec2::new(64.0, 64.0),
            30.0,
            1.0,
            Vec2::new(10.0, 0.0),
        );
        // At the centre the image moved right by ~10 px: it shows what was
        // 10 px to the left.
        let d = field.sample(Vec2::new(64.5, 64.5));
        assert!((d.x + 10.0).abs() < 0.5, "{d:?}");
        let out: HashMap<_, _> = field.render(&src, [60, 60, 70, 70]).into_iter().collect();
        let px = out[&(1, 1)][0];
        assert!((px.r() as i32 - 2 * 54).abs() <= 2, "{}", px.r());
    }

    #[test]
    fn restore_brings_back_the_original() {
        let mut field = LiquifyField::new(64, 128, 128);
        field.dab(
            LiquifyMode::Bloat,
            Vec2::new(64.0, 64.0),
            30.0,
            1.0,
            Vec2::ZERO,
        );
        assert!(field.sample(Vec2::new(70.5, 64.5)).length() > 0.1);
        for _ in 0..40 {
            field.dab(
                LiquifyMode::Reconstruct,
                Vec2::new(64.0, 64.0),
                40.0,
                1.0,
                Vec2::ZERO,
            );
        }
        assert!(field.sample(Vec2::new(70.5, 64.5)).length() < 0.01);
    }

    #[test]
    fn pinch_and_bloat_are_opposites() {
        let mut a = LiquifyField::new(64, 128, 128);
        let mut b = LiquifyField::new(64, 128, 128);
        a.dab(
            LiquifyMode::Pinch,
            Vec2::new(64.0, 64.0),
            30.0,
            1.0,
            Vec2::ZERO,
        );
        b.dab(
            LiquifyMode::Bloat,
            Vec2::new(64.0, 64.0),
            30.0,
            1.0,
            Vec2::ZERO,
        );
        let p = Vec2::new(74.5, 64.5);
        // Pinch samples farther out (content shrinks), bloat nearer in.
        assert!(a.sample(p).x > 0.0 && b.sample(p).x < 0.0);
    }

    /// A field cell's centre, in canvas pixels.
    fn cell(cx: i32, cy: i32) -> Vec2 {
        Vec2::new((cx as f32 + 0.5) * 2.0, (cy as f32 + 0.5) * 2.0)
    }

    #[test]
    fn crystals_break_the_picture_into_shards_pushed_their_own_ways() {
        let mut field = LiquifyField::new(64, 256, 256);
        let (centre, r) = (Vec2::new(128.0, 128.0), 80.0);
        field.dab(LiquifyMode::Crystals, centre, r, 1.0, Vec2::ZERO);
        let mut directions = HashSet::new();
        let (mut same, mut pairs) = (0, 0);
        for cy in 34..94 {
            for cx in 34..94 {
                let p = cell(cx, cy);
                if (p - centre).length() > r * 0.7 {
                    continue;
                }
                let d = field.sample(p);
                assert!(d.length() > 0.05, "every pixel near the middle moves");
                let bucket = (d.angle() / std::f32::consts::TAU * 8.0).floor() as i32;
                directions.insert(bucket.rem_euclid(8));
                // Its neighbour: mostly in the same shard (the same way).
                let n = field.sample(cell(cx + 1, cy));
                pairs += 1;
                same += usize::from(d.normalized().dot(n.normalized()) > 0.999);
            }
        }
        assert!(directions.len() >= 5, "shards go many ways: {directions:?}");
        assert!(same * 10 > pairs * 7, "pieces, not noise: {same}/{pairs}");
        assert!(same < pairs, "and they tear apart at their edges");
        // Nothing beyond the brush.
        assert_eq!(field.sample(cell(10, 64)), Vec2::ZERO);
    }

    #[test]
    fn edge_pulls_both_sides_toward_the_line_the_brush_moves_along() {
        let mut field = LiquifyField::new(64, 256, 256);
        let centre = Vec2::new(128.0, 128.0);
        assert!(
            field
                .dab(LiquifyMode::Edge, centre, 60.0, 1.0, Vec2::ZERO)
                .is_none(),
            "no line without a direction"
        );
        field.dab(LiquifyMode::Edge, centre, 60.0, 1.0, Vec2::new(5.0, 0.0));
        let above = field.sample(cell(64, 54));
        let below = field.sample(cell(64, 73));
        // Each side shows what was farther out: its content comes in.
        assert!(above.y < -0.5 && below.y > 0.5, "{above:?} {below:?}");
        assert!(above.x.abs() < 1e-3 && below.x.abs() < 1e-3, "only across");
        assert!((above.y + below.y).abs() < 0.05, "both sides alike");
        assert!(field.sample(cell(64, 63)).length() < 0.3, "the line stays");
        // Along a diagonal stroke, across the diagonal.
        let mut diagonal = LiquifyField::new(64, 256, 256);
        diagonal.dab(LiquifyMode::Edge, centre, 60.0, 1.0, Vec2::new(3.0, 3.0));
        let d = diagonal.sample(cell(54, 73));
        assert!(
            d.length() > 0.5 && d.normalized().dot(Vec2::new(1.0, 1.0).normalized()).abs() < 1e-3
        );
    }

    #[test]
    fn adjust_scales_what_shows_and_baking_keeps_it() {
        let src = gradient_source(64);
        let mut field = LiquifyField::new(64, 128, 128);
        field.dab(
            LiquifyMode::Push,
            Vec2::new(64.0, 64.0),
            30.0,
            1.0,
            Vec2::new(10.0, 0.0),
        );
        let all = [0, 0, 128, 128];
        let full = field.render_rect(&src, all);
        let original = LiquifyField::new(64, 128, 128).render_rect(&src, all);
        assert!(full != original);
        assert!(field.set_scale(0.0).is_some(), "the area to redraw");
        assert!(field.render_rect(&src, all) == original, "none shows");
        field.set_scale(1.0);
        assert!(field.render_rect(&src, all) == full, "all of it again");
        field.set_scale(0.5);
        let half = field.render_rect(&src, all);
        // At the middle it moved ~10 px at full: ~5 at half.
        let red = |px: &[Color32]| px[64 * 128 + 64].r() as i32;
        assert!(
            (red(&half) - (red(&original) - 10)).abs() <= 2,
            "{}",
            red(&half)
        );
        field.bake_scale();
        assert_eq!(field.scale(), 1.0);
        let baked = field.render_rect(&src, all);
        let worst = (baked.iter().zip(&half))
            .map(|(a, b)| a.r().abs_diff(b.r()))
            .max()
            .unwrap();
        assert!(worst <= 1, "baking shows the same: {worst}");
    }

    #[test]
    fn a_selection_keeps_the_displacement_inside_it() {
        let src = gradient_source(64);
        let mut field = LiquifyField::new(64, 128, 128);
        let left = crate::selection::SelectionMask::rasterize([0, 0, 64, 128], |_, _, out| {
            out.fill(1.0);
        });
        field.confine_to(Some(std::sync::Arc::new(left)));
        field.dab(
            LiquifyMode::Push,
            Vec2::new(64.0, 64.0),
            30.0,
            1.0,
            Vec2::new(10.0, 0.0),
        );
        let out = field.render_rect(&src, [0, 0, 128, 128]);
        let original = LiquifyField::new(64, 128, 128).render_rect(&src, [0, 0, 128, 128]);
        for y in 0..128 {
            for x in 64..128 {
                assert_eq!(out[y * 128 + x], original[y * 128 + x], "({x}, {y})");
            }
        }
        assert!((0..128 * 128).any(|i| i % 128 < 64 && out[i] != original[i]));
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    #[test]
    #[ignore = "timing"]
    fn dab_vs_render() {
        let ts = 64;
        let n = 4096;
        let mut src = HashMap::new();
        for ty in 0..n / ts {
            for tx in 0..n / ts {
                src.insert(
                    (tx, ty),
                    vec![Color32::from_rgb(tx as u8, ty as u8, 9); (ts * ts) as usize],
                );
            }
        }
        for r in [60.0f32, 150.0, 400.0] {
            for mode in [LiquifyMode::Push, LiquifyMode::TwirlCw, LiquifyMode::Pinch] {
                let mut f = LiquifyField::new(ts as usize, n as usize, n as usize);
                let t = std::time::Instant::now();
                let mut rect = None;
                for i in 0..100 {
                    rect = f.dab(
                        mode,
                        Vec2::new(2000.0 + i as f32 * 5.0, 2000.0),
                        r,
                        0.5,
                        Vec2::new(5.0, 0.0),
                    );
                }
                let dab = t.elapsed() / 100;
                let t = std::time::Instant::now();
                for _ in 0..100 {
                    let _ = f.render_rect(&src, rect.unwrap());
                }
                eprintln!(
                    "r {r:>4} {mode:?}: dab {dab:?}  render {:?}",
                    t.elapsed() / 100
                );
            }
        }
    }
}
