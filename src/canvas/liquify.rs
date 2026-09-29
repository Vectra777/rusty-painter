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
}

impl LiquifyMode {
    pub const ALL: [(LiquifyMode, &'static str); 7] = [
        (LiquifyMode::Push, "Push"),
        (LiquifyMode::TwirlCw, "Twirl ↻"),
        (LiquifyMode::TwirlCcw, "Twirl ↺"),
        (LiquifyMode::Pinch, "Pinch"),
        (LiquifyMode::Bloat, "Bloat"),
        (LiquifyMode::Smooth, "Smooth"),
        (LiquifyMode::Reconstruct, "Restore"),
    ];

    /// Modes that keep acting while the brush is held still.
    pub fn is_continuous(self) -> bool {
        self != LiquifyMode::Push
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

    /// Apply one brush dab. `delta` is the brush movement (Push only),
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
                            LiquifyMode::Reconstruct => tile[i] * (1.0 - (f * 0.5).min(1.0)),
                            LiquifyMode::Smooth => {
                                let mut sum = Vec2::ZERO;
                                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
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
        const ROWS: usize = 4;
        let [x0, y0, x1, y1] = rect;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut out = vec![Color32::TRANSPARENT; w * h];
        if w == 0 {
            return out;
        }
        let k = SCALE as f32;
        // Cell coordinate of canvas pixel x's centre, relative to cell centres.
        let cell_x = |x: i32| (x as f32 + 0.5) / k - 0.5;
        let c0 = cell_x(x0).floor() as i32;
        let c1 = cell_x(x1 - 1).floor() as i32 + 1;
        out.par_chunks_mut(w * ROWS)
            .enumerate()
            .for_each(|(chunk, lines)| {
                let mut field = TileReader::new(&self.tiles, self.ts, Vec2::ZERO);
                let mut src = TileReader::new(source, self.ts, Color32::TRANSPARENT);
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
                    for (i, px) in line.iter_mut().enumerate() {
                        let x = x0 + i as i32;
                        let d = if any {
                            let fx = cell_x(x);
                            let cx = fx.floor();
                            let ax = fx - cx;
                            let j = (cx as i32 - c0) as usize;
                            column[j] * (1.0 - ax) + column[j + 1] * ax
                        } else {
                            Vec2::ZERO
                        };
                        *px = if d == Vec2::ZERO {
                            src.at(x, y)
                        } else {
                            bilinear(&mut src, Vec2::new(x as f32 + 0.5, y as f32 + 0.5) + d)
                        };
                    }
                }
            });
        out
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

/// Premultiplied bilinear sample of the source at canvas point `p`.
fn bilinear(reader: &mut TileReader<'_, Color32>, p: Vec2) -> Color32 {
    let (fx, fy) = (p.x - 0.5, p.y - 0.5);
    let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
    let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
    let [c00, c10, c01, c11] = reader.quad(x0, y0);
    let px = [
        (c00, (1.0 - ax) * (1.0 - ay)),
        (c10, ax * (1.0 - ay)),
        (c01, (1.0 - ax) * ay),
        (c11, ax * ay),
    ];
    let mut acc = [0.0f32; 4];
    for (c, w) in px {
        for (a, v) in acc.iter_mut().zip(c.to_array()) {
            *a += v as f32 * w;
        }
    }
    let q = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
    let a = q(acc[3]);
    // Keep premultiplied colour valid (channels never above alpha).
    Color32::from_rgba_premultiplied(q(acc[0]).min(a), q(acc[1]).min(a), q(acc[2]).min(a), a)
}

#[cfg(test)]
mod tests {
    use super::*;

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
