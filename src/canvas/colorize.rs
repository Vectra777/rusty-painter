//! Colorize mask (Krita's "lazy brush"): a layer whose own pixels are
//! rough colour hints, and which shows each area enclosed by a line-art
//! layer filled with the colour of the hint inside it.
//!
//! How the areas are found:
//! 1. **Darkness** of the line art: its coverage times how dark it is, so
//!    black lines on transparency and black lines on white paper both work.
//!    Pixels at least half dark are the lines.
//! 2. **Cost** of passing each pixel: its darkness, and, when gaps are
//!    closed, a cost that rises near the lines (from a distance transform),
//!    so the middle of an opening narrower than the gap size costs more
//!    than the open inside of an area does.
//! 3. **Priority flood** (a watershed) from the hints: every hint pixel
//!    starts its colour, and colours spread to neighbours cheapest level
//!    first. An area's own hint covers all of its open inside before
//!    anything gets through a closed gap or over a line, so colours meet in
//!    the gaps and on the lines, never inside an area. Areas with no hint
//!    take a neighbour's colour (as in Krita): give them the transparent
//!    key to leave them empty.
//! 4. **Edges**: line pixels keep their colour only up to `extend` px
//!    in from the line's edge, so colour tucks under anti-aliased lines
//!    without filling thick lines solid.
//!
//! The flood is sequential, but O(pixels) with a bucket queue (256 levels);
//! the rest (darkness, the result's tiles) runs in parallel.

use std::sync::{Arc, Mutex};

use eframe::egui::Color32;
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::canvas::Canvas;
use crate::canvas::storage::{LayerId, SharedPixels, TileCell};

/// A tile's `(tx, ty)` position.
pub type TileKey = (i32, i32);

/// A tile's shared, lockable storage.
type SharedCell = Arc<Mutex<TileCell>>;

/// Darkness (0..=255) from which a pixel counts as part of a line.
const LINE: u8 = 128;
/// Darkness below this counts as none (paper grain, faint sketch).
const NOISE: u8 = 16;
/// The lowest cost of a pixel within gap range of a line: above paper
/// grain, below the lines.
const GAP_FLOOR: u16 = 32;
/// Chamfer distance units per pixel (3-4 chamfer).
const UNIT: u16 = 3;
/// Hint pixels at least this opaque are seeds.
const SEED_ALPHA: u8 = 128;
/// How close (per channel) a hint has to be to the transparent key.
const KEY_TOLERANCE: u8 = 24;
/// Labels: none yet, the transparent key; colours from 2 up.
const NONE: u16 = 0;
const TRANSPARENT: u16 = 1;
/// The frame around the working grid, which counts as already labelled
/// (so the flood never looks past the canvas).
const EDGE: u16 = u16::MAX;
/// Widest gap that can be closed (px).
pub const MAX_GAP: u8 = 50;
/// Furthest colour can go under a line (px).
pub const MAX_EXTEND: u8 = 60;

/// A colorize mask's settings, as saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ColorizeSettings {
    /// The line-art layer (its id) whose lines enclose the areas.
    pub source: Option<u64>,
    /// Close openings in the lines up to this many pixels wide (0 = off).
    pub gap: u8,
    /// How far (px) colour goes in under a line from its edge.
    pub extend: u8,
    /// Show the hints over the result (they're never exported or merged).
    pub show_hints: bool,
    /// Hints in this colour leave their area empty.
    pub transparent: [u8; 3],
}

impl Default for ColorizeSettings {
    fn default() -> Self {
        Self {
            source: None,
            gap: 8,
            extend: 2,
            show_hints: true,
            transparent: [255, 0, 255],
        }
    }
}

impl ColorizeSettings {
    /// Hash of what the result depends on (not whether hints show).
    fn result_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        let shown = Self {
            show_hints: true,
            ..*self
        };
        shown.hash(&mut h);
        h.finish()
    }
}

/// A colorize mask: the layer's own tiles hold the hints; this holds its
/// settings and the result made from them, which is what compositing
/// shows of the layer (see `ColorizeLayer::shown_tile`).
#[derive(Debug, Default)]
pub struct ColorizeLayer {
    pub settings: ColorizeSettings,
    shown: Mutex<Shown>,
}

/// What a colorize mask shows and how it's kept up to date (not saved:
/// made again from the hints and line art).
#[derive(Debug, Default)]
struct Shown {
    /// The result's painted tiles (never written: replaced whole).
    result: FxHashMap<TileKey, SharedCell>,
    /// The inputs' key ([`Canvas::colorize_key`]) the result was made from.
    made_from: Option<u64>,
    /// Hints over the result, per tile: the hint tile's revision and the
    /// result tile they were made from.
    over: FxHashMap<TileKey, (u64, SharedCell, SharedCell)>,
    /// Since when an update has been made in the background.
    busy: Option<std::time::Instant>,
    /// The inputs' key last seen changed, and when (to wait for the
    /// changes to settle before updating).
    seen: Option<(u64, std::time::Instant)>,
}

/// After this long, an update being made is taken as lost (its result
/// went to another document, open in another tab meanwhile).
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl Shown {
    fn is_busy(&self) -> bool {
        self.busy.is_some_and(|at| at.elapsed() < BUSY_TIMEOUT)
    }
}

impl Clone for ColorizeLayer {
    /// The same settings and result (its tiles shared); nothing being made.
    fn clone(&self) -> Self {
        let shown = self.lock();
        Self {
            settings: self.settings,
            shown: Mutex::new(Shown {
                result: shown.result.clone(),
                made_from: shown.made_from,
                ..Default::default()
            }),
        }
    }
}

impl ColorizeLayer {
    pub fn new(settings: ColorizeSettings) -> Self {
        Self {
            settings,
            shown: Default::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tile `key` as the layer shows: the result, with the hints (`hints`,
    /// the layer's own tile) over it while they're shown.
    pub(crate) fn shown_tile(&self, key: TileKey, hints: Option<SharedCell>) -> Option<SharedCell> {
        // (Never a tile locked while the result is: a tile's holder may be
        // compositing.)
        let result = self.lock().result.get(&key).cloned();
        if !self.settings.show_hints {
            return result;
        }
        let Some(hints) = hints else {
            return result;
        };
        let (revision, data) = {
            let g = hints.lock().unwrap_or_else(|e| e.into_inner());
            match g.shared_data() {
                Some(data) if !g.is_empty => (g.revision(), Arc::clone(data)),
                _ => return result,
            }
        };
        let Some(result) = result else {
            return Some(hints);
        };
        if let Some((rev, under, over)) = self.lock().over.get(&key)
            && *rev == revision
            && Arc::ptr_eq(under, &result)
        {
            return Some(Arc::clone(over));
        }
        let under = (result.lock().unwrap_or_else(|e| e.into_inner()))
            .shared_data()
            .cloned()?;
        let pixels: Vec<Color32> = data
            .iter()
            .zip(under.iter())
            .map(|(&h, &r)| over_8bit(h, r))
            .collect();
        let over = Arc::new(Mutex::new(TileCell::new(Some(pixels), false)));
        self.lock()
            .over
            .insert(key, (revision, result, Arc::clone(&over)));
        Some(over)
    }

    /// The result's painted tiles (shared).
    pub(crate) fn result_tiles(&self) -> FxHashMap<TileKey, SharedCell> {
        self.lock().result.clone()
    }

    /// The key the result was made from.
    pub fn made_from(&self) -> Option<u64> {
        self.lock().made_from
    }

    /// Whether an update is being made.
    pub fn is_busy(&self) -> bool {
        self.lock().is_busy()
    }

    /// Whether the result should be made again for inputs `key` now: they
    /// differ from what it was made from, nothing is being made, and they
    /// have stayed the same for `settle` (`None`: at once). Remembers when
    /// `key` was first seen. Returns how long is left to wait otherwise.
    pub fn due(
        &self,
        key: u64,
        settle: Option<std::time::Duration>,
    ) -> Result<(), Option<std::time::Duration>> {
        let mut shown = self.lock();
        if shown.made_from == Some(key) || shown.is_busy() {
            return Err(None);
        }
        let now = std::time::Instant::now();
        let since = match shown.seen {
            Some((k, at)) if k == key => at,
            _ => {
                shown.seen = Some((key, now));
                now
            }
        };
        match settle {
            Some(wait) if now.duration_since(since) < wait => {
                Err(Some(wait - now.duration_since(since)))
            }
            _ => Ok(()),
        }
    }

    /// Mark an update as being made, or not.
    pub fn set_busy(&self, busy: bool) {
        self.lock().busy = busy.then(std::time::Instant::now);
    }

    /// Forget what the result was made from (so it's made again).
    pub fn invalidate(&self) {
        let mut shown = self.lock();
        shown.made_from = None;
        shown.seen = None;
    }

    /// Show `tiles` (made from inputs `key`) as the result; returns the
    /// tiles where it may have changed.
    pub fn set_result(&self, key: u64, tiles: FxHashMap<TileKey, Vec<Color32>>) -> Vec<TileKey> {
        let new: FxHashMap<TileKey, SharedCell> = tiles
            .into_iter()
            .map(|(k, data)| (k, Arc::new(Mutex::new(TileCell::new(Some(data), false)))))
            .collect();
        let mut shown = self.lock();
        let mut changed: Vec<TileKey> = shown.result.keys().chain(new.keys()).copied().collect();
        changed.sort_unstable();
        changed.dedup();
        shown.result = new;
        shown.over.clear();
        shown.made_from = Some(key);
        shown.busy = None;
        changed
    }
}

/// `src` over `dst`, both premultiplied 8-bit.
#[inline]
fn over_8bit(src: Color32, dst: Color32) -> Color32 {
    let k = 255 - src.a() as u32;
    let mix = |s: u8, d: u8| (s as u32 + (d as u32 * k + 127) / 255).min(255) as u8;
    Color32::from_rgba_premultiplied(
        mix(src.r(), dst.r()),
        mix(src.g(), dst.g()),
        mix(src.b(), dst.b()),
        mix(src.a(), dst.a()),
    )
}

/// Everything an update reads, copied (shared) from the canvas so it can
/// run in the background.
#[derive(Clone)]
pub struct ColorizeJob {
    pub width: usize,
    pub height: usize,
    pub tile_size: usize,
    /// The line art's painted tiles.
    pub lines: FxHashMap<TileKey, SharedPixels>,
    /// The hints' painted tiles.
    pub hints: FxHashMap<TileKey, SharedPixels>,
    pub settings: ColorizeSettings,
}

/// How dark (0..=255) a premultiplied pixel is: its coverage times one
/// minus its lightness.
#[inline]
fn darkness(c: Color32) -> u8 {
    let lum = (c.r() as u32 * 54 + c.g() as u32 * 183 + c.b() as u32 * 19) >> 8;
    (c.a() as u32).saturating_sub(lum) as u8
}

/// Chamfer distance (units of [`UNIT`] per pixel, capped at 255) from every
/// pixel of a `w`×`h` grid to the nearest one where `from` holds.
fn chamfer(w: usize, h: usize, from: impl Fn(usize) -> bool + Sync) -> Vec<u8> {
    let mut d: Vec<u8> = (0..w * h)
        .into_par_iter()
        .map(|i| if from(i) { 0 } else { 255 })
        .collect();
    let relax = |d: &mut [u8], i: usize, j: usize, cost: u16| {
        let v = (d[j] as u16 + cost).min(255) as u8;
        if v < d[i] {
            d[i] = v;
        }
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if x > 0 {
                relax(&mut d, i, i - 1, UNIT);
            }
            if y > 0 {
                relax(&mut d, i, i - w, UNIT);
                if x > 0 {
                    relax(&mut d, i, i - w - 1, 4);
                }
                if x + 1 < w {
                    relax(&mut d, i, i - w + 1, 4);
                }
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if x + 1 < w {
                relax(&mut d, i, i + 1, UNIT);
            }
            if y + 1 < h {
                relax(&mut d, i, i + w, UNIT);
                if x + 1 < w {
                    relax(&mut d, i, i + w + 1, 4);
                }
                if x > 0 {
                    relax(&mut d, i, i + w - 1, 4);
                }
            }
        }
    }
    d
}

/// The hint colours as labels: each colour (5 bits a channel) gets one,
/// the transparent key [`TRANSPARENT`].
struct Palette {
    bins: Vec<u16>,
    colours: Vec<Color32>,
    key: [u8; 3],
}

impl Palette {
    fn new(key: [u8; 3]) -> Self {
        Self {
            bins: vec![NONE; 1 << 15],
            // Labels 0 and 1 have no colour.
            colours: vec![Color32::TRANSPARENT; 2],
            key,
        }
    }

    /// The label of hint pixel `c` (premultiplied), if it's a seed.
    fn label(&mut self, c: Color32) -> Option<u16> {
        if c.a() < SEED_ALPHA {
            return None;
        }
        let [r, g, b, _] = crate::canvas::blend::unmultiply(c);
        let near = |x: u8, y: u8| x.abs_diff(y) <= KEY_TOLERANCE;
        if near(r, self.key[0]) && near(g, self.key[1]) && near(b, self.key[2]) {
            return Some(TRANSPARENT);
        }
        let bin = ((r as usize >> 3) << 10) | ((g as usize >> 3) << 5) | (b as usize >> 3);
        if self.bins[bin] == NONE {
            // (At most 2^15 colours: always room below `EDGE`.)
            self.bins[bin] = self.colours.len() as u16;
            self.colours.push(Color32::from_rgb(r, g, b));
        }
        Some(self.bins[bin])
    }
}

/// The colorize result for `job`: its painted tiles.
pub fn compute(job: &ColorizeJob) -> FxHashMap<TileKey, Vec<Color32>> {
    let (w, h, ts) = (job.width, job.height, job.tile_size);
    if w == 0 || h == 0 || ts == 0 {
        return FxHashMap::default();
    }
    // The working grid: the canvas in a one-pixel frame.
    let (gw, gh) = (w + 2, h + 2);
    let at = |x: usize, y: usize| (y + 1) * gw + x + 1;

    // 1. Darkness, a row of tiles at a time.
    let mut dark = vec![0u8; gw * gh];
    dark[gw..gw * (h + 1)]
        .par_chunks_mut(gw * ts)
        .enumerate()
        .for_each(|(ty, band)| {
            let rows = band.len() / gw;
            for tx in 0..w.div_ceil(ts) {
                let Some(tile) = job.lines.get(&(tx as i32, ty as i32)) else {
                    continue;
                };
                let cols = ts.min(w - tx * ts);
                for ly in 0..rows.min(ts) {
                    let row = &mut band[ly * gw + 1 + tx * ts..][..cols];
                    let src = &tile[ly * ts..][..cols];
                    for (d, &c) in row.iter_mut().zip(src) {
                        *d = darkness(c);
                    }
                }
            }
        });

    // 2-3. The flood, and meanwhile 4: how far each line pixel is in from
    // the line's edge (neither needs the other).
    let extend = job.settings.extend.min(MAX_EXTEND) as u16 * UNIT;
    let into_lines = extend < 255;
    let ((labels, palette), depth) = rayon::join(
        || flood_from_hints(job, &dark, gw, gh),
        || into_lines.then(|| chamfer(gw, gh, |i| dark[i] < LINE)),
    );

    // 5. The result's tiles.
    let colours = &palette.colours;
    let (tw, th) = (w.div_ceil(ts), h.div_ceil(ts));
    (0..tw * th)
        .into_par_iter()
        .filter_map(|k| {
            let (tx, ty) = (k % tw, k / tw);
            let mut tile = vec![Color32::TRANSPARENT; ts * ts];
            let mut any = false;
            for ly in 0..ts.min(h - ty * ts) {
                for lx in 0..ts.min(w - tx * ts) {
                    let i = at(tx * ts + lx, ty * ts + ly);
                    let label = labels[i];
                    if label <= TRANSPARENT || label == EDGE {
                        continue;
                    }
                    if depth.as_ref().is_some_and(|d| d[i] as u16 > extend) {
                        continue;
                    }
                    tile[ly * ts + lx] = colours[label as usize];
                    any = true;
                }
            }
            any.then_some(((tx as i32, ty as i32), tile))
        })
        .collect()
}

/// Each pixel's label (see [`NONE`]) on the `gw`×`gh` grid of `dark`, by
/// a priority flood from the hints; and the hints' colours.
fn flood_from_hints(job: &ColorizeJob, dark: &[u8], gw: usize, gh: usize) -> (Vec<u16>, Palette) {
    let (w, h, ts) = (job.width, job.height, job.tile_size);
    let settings = &job.settings;
    let at = |x: usize, y: usize| (y + 1) * gw + x + 1;
    // Cost of passing each pixel.
    let cost: Vec<u8> = if settings.gap > 0 {
        let gap = settings.gap.min(MAX_GAP) as u16;
        // A gap `gap` px wide has its middle about (gap + 1) / 2 px from
        // the lines: anything nearer than that costs more than open space.
        let reach = UNIT * (gap + 1) / 2 + 1;
        let span = LINE as u16 - 1 - GAP_FLOOR;
        let mut d = chamfer(gw, gh, |i| dark[i] >= LINE);
        d.par_iter_mut().zip(dark.par_iter()).for_each(|(c, &k)| {
            let near = *c as u16;
            let gap_cost = if near >= reach {
                0
            } else {
                GAP_FLOOR + (reach - near) * span / reach
            };
            let own = if k < NOISE { 0 } else { k as u16 };
            *c = own.max(gap_cost) as u8;
        });
        d
    } else {
        dark.par_iter()
            .map(|&k| if k < NOISE { 0 } else { k })
            .collect()
    };

    // Seeds.
    let mut labels = vec![NONE; gw * gh];
    labels[..gw].fill(EDGE);
    labels[(gh - 1) * gw..].fill(EDGE);
    for row in labels.chunks_exact_mut(gw) {
        row[0] = EDGE;
        row[gw - 1] = EDGE;
    }
    let mut palette = Palette::new(settings.transparent);
    let mut queues: Vec<Vec<u32>> = vec![Vec::new(); 256];
    let mut hint_keys: Vec<&TileKey> = job.hints.keys().collect();
    // (In order, so the colours' labels are always the same.)
    hint_keys.sort_unstable();
    for &(tx, ty) in hint_keys {
        let tile = &job.hints[&(tx, ty)];
        let (Ok(tx), Ok(ty)) = (usize::try_from(tx), usize::try_from(ty)) else {
            continue;
        };
        for ly in 0..ts {
            let y = ty * ts + ly;
            if y >= h {
                break;
            }
            for lx in 0..ts {
                let x = tx * ts + lx;
                if x >= w {
                    break;
                }
                if let Some(label) = palette.label(tile[ly * ts + lx]) {
                    let i = at(x, y);
                    if labels[i] == NONE {
                        queues[0].push(i as u32);
                    }
                    labels[i] = label;
                }
            }
        }
    }
    flood(&mut labels, &cost, &mut queues, gw);
    (labels, palette)
}

/// Spread each labelled pixel's label to its unlabelled neighbours,
/// cheapest first: a neighbour waits at the higher of its own cost and the
/// level reached (a priority flood over 256 buckets, each spread a ring of
/// neighbours at a time, so ties spread evenly from every seed).
fn flood(labels: &mut [u16], cost: &[u8], queues: &mut [Vec<u32>], gw: usize) {
    let mut next = Vec::new();
    for level in 0..queues.len() {
        let mut ring = std::mem::take(&mut queues[level]);
        while !ring.is_empty() {
            for &i in &ring {
                let i = i as usize;
                let label = labels[i];
                // (The frame is labelled: these never leave the grid.)
                for n in [i - 1, i + 1, i - gw, i + gw] {
                    if labels[n] == NONE {
                        labels[n] = label;
                        let at = (cost[n] as usize).max(level);
                        if at == level {
                            next.push(n as u32);
                        } else {
                            queues[at].push(n as u32);
                        }
                    }
                }
            }
            ring.clear();
            std::mem::swap(&mut ring, &mut next);
        }
    }
}

impl Canvas {
    /// Layer `idx`'s colorize mask, if it is one.
    pub fn colorize_of(&self, idx: usize) -> Option<&ColorizeLayer> {
        self.layers.get(idx)?.colorize.as_deref()
    }

    /// Where colorize mask `idx`'s line-art layer is.
    pub fn colorize_source(&self, idx: usize) -> Option<usize> {
        let source = LayerId(self.colorize_of(idx)?.settings.source?);
        self.layer_index_of(source).filter(|&s| s != idx)
    }

    /// A key for everything colorize mask `idx`'s result is made from: its
    /// settings, its hints and its line art (their tiles' revisions).
    pub fn colorize_key(&self, idx: usize) -> Option<u64> {
        Some(self.colorize_inputs(idx, false)?.0)
    }

    /// What updating colorize mask `idx` reads, and its key.
    pub fn colorize_job(&self, idx: usize) -> Option<(u64, ColorizeJob)> {
        let (key, job) = self.colorize_inputs(idx, true)?;
        Some((key, job?))
    }

    fn colorize_inputs(&self, idx: usize, take: bool) -> Option<(u64, Option<ColorizeJob>)> {
        use std::hash::{Hash, Hasher};
        let settings = self.colorize_of(idx)?.settings;
        let mut key = settings.result_hash() ^ ((self.width() as u64) << 32 | self.height() as u64);
        let mut gather = |layer: Option<usize>, which: u8| {
            let mut tiles = FxHashMap::default();
            let Some(layer) = layer else {
                return tiles;
            };
            for (pos, revision, data) in self.layer_tiles_shared(layer) {
                let mut h = rustc_hash::FxHasher::default();
                (which, pos, revision).hash(&mut h);
                // Order-free: the tiles come in no order.
                key = key.wrapping_add(h.finish());
                if take && let Some(data) = data {
                    tiles.insert(pos, data);
                }
            }
            tiles
        };
        let hints = gather(Some(idx), 0);
        let lines = gather(self.colorize_source(idx), 1);
        let job = take.then(|| ColorizeJob {
            width: self.width(),
            height: self.height(),
            tile_size: self.tile_size(),
            lines,
            hints,
            settings,
        });
        Some((key, job))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: usize = 64;
    const RED: Color32 = Color32::from_rgb(220, 30, 30);
    const BLUE: Color32 = Color32::from_rgb(30, 30, 220);

    /// A `w`×`h` picture made into 64-px tiles (only the painted ones).
    fn tiles(w: usize, h: usize, px: &[Color32]) -> FxHashMap<TileKey, SharedPixels> {
        let mut out = FxHashMap::default();
        for ty in 0..h.div_ceil(TS) {
            for tx in 0..w.div_ceil(TS) {
                let mut tile = vec![Color32::TRANSPARENT; TS * TS];
                for ly in 0..TS.min(h - ty * TS) {
                    for lx in 0..TS.min(w - tx * TS) {
                        tile[ly * TS + lx] = px[(ty * TS + ly) * w + tx * TS + lx];
                    }
                }
                if tile.iter().any(|&c| c != Color32::TRANSPARENT) {
                    out.insert((tx as i32, ty as i32), Arc::new(tile));
                }
            }
        }
        out
    }

    /// The result's pixel at `(x, y)`.
    fn at(result: &FxHashMap<TileKey, Vec<Color32>>, x: usize, y: usize) -> Color32 {
        result
            .get(&((x / TS) as i32, (y / TS) as i32))
            .map_or(Color32::TRANSPARENT, |t| t[(y % TS) * TS + x % TS])
    }

    struct Picture {
        w: usize,
        h: usize,
        lines: Vec<Color32>,
        hints: Vec<Color32>,
    }

    impl Picture {
        fn new(w: usize, h: usize) -> Self {
            Self {
                w,
                h,
                lines: vec![Color32::TRANSPARENT; w * h],
                hints: vec![Color32::TRANSPARENT; w * h],
            }
        }

        /// A black line `width` px wide, from x0..x1 at rows y0..y0+width
        /// (or a column, with `vertical`), skipping `skip` along it.
        fn line(&mut self, vertical: bool, at: usize, from: usize, to: usize, width: usize) {
            self.line_gap(vertical, at, from, to, width, 0..0);
        }

        fn line_gap(
            &mut self,
            vertical: bool,
            at: usize,
            from: usize,
            to: usize,
            width: usize,
            skip: std::ops::Range<usize>,
        ) {
            for k in from..to {
                if skip.contains(&k) {
                    continue;
                }
                for d in 0..width {
                    let (x, y) = if vertical { (at + d, k) } else { (k, at + d) };
                    self.lines[y * self.w + x] = Color32::BLACK;
                }
            }
        }

        fn hint(&mut self, x: usize, y: usize, c: Color32) {
            for dy in 0..3 {
                for dx in 0..3 {
                    self.hints[(y + dy) * self.w + x + dx] = c;
                }
            }
        }

        fn run(&self, settings: ColorizeSettings) -> FxHashMap<TileKey, Vec<Color32>> {
            compute(&ColorizeJob {
                width: self.w,
                height: self.h,
                tile_size: TS,
                lines: tiles(self.w, self.h, &self.lines),
                hints: tiles(self.w, self.h, &self.hints),
                settings,
            })
        }
    }

    /// Two boxes side by side: x 10..60 and 60..110 (a shared wall at
    /// 60..63), y 10..90.
    fn two_boxes(gap: std::ops::Range<usize>) -> Picture {
        let mut p = Picture::new(130, 100);
        p.line(false, 10, 10, 113, 3);
        p.line(false, 87, 10, 113, 3);
        p.line(true, 10, 10, 90, 3);
        p.line(true, 110, 10, 90, 3);
        p.line_gap(true, 60, 10, 90, 3, gap);
        p
    }

    #[test]
    fn each_area_fills_with_its_hint() {
        let mut p = two_boxes(0..0);
        p.hint(30, 50, RED);
        p.hint(80, 50, BLUE);
        let r = p.run(ColorizeSettings::default());
        assert_eq!(at(&r, 20, 20), RED);
        assert_eq!(at(&r, 57, 85), RED);
        assert_eq!(at(&r, 64, 14), BLUE);
        assert_eq!(at(&r, 100, 80), BLUE);
        // Colours stay off the inside of each other's areas, and the
        // outside (no hint) takes a neighbour's colour, as in Krita.
        assert_ne!(at(&r, 2, 2), Color32::TRANSPARENT);
    }

    #[test]
    fn gaps_up_to_the_closing_size_dont_leak() {
        // A 6-px opening in the wall between the boxes.
        let mut p = two_boxes(47..53);
        p.hint(30, 50, RED);
        p.hint(85, 30, BLUE);
        let closed = p.run(ColorizeSettings {
            gap: 8,
            ..Default::default()
        });
        let count =
            |r: &FxHashMap<TileKey, Vec<Color32>>, c: Color32, xs: std::ops::Range<usize>| {
                (13..87)
                    .flat_map(|y| xs.clone().map(move |x| (x, y)))
                    .filter(|&(x, y)| at(r, x, y) == c)
                    .count()
            };
        // Nothing of red past the wall (beyond the opening itself).
        assert_eq!(count(&closed, RED, 66..110), 0);
        assert_eq!(count(&closed, BLUE, 13..57), 0);
        // Without closing, the hints' colours meet wherever they reach
        // first: red (nearer the opening) gets well into the right box.
        let open = p.run(ColorizeSettings {
            gap: 0,
            ..Default::default()
        });
        assert!(count(&open, RED, 66..110) > 100);
    }

    #[test]
    fn the_transparent_key_leaves_an_area_empty() {
        let mut p = two_boxes(47..53);
        p.hint(30, 50, RED);
        p.hint(85, 30, Color32::from_rgb(255, 0, 255));
        p.hint(2, 2, Color32::from_rgb(250, 10, 250));
        let r = p.run(ColorizeSettings::default());
        assert_eq!(at(&r, 30, 30), RED);
        for (x, y) in [(70, 20), (100, 80), (85, 50), (2, 50), (120, 95)] {
            assert_eq!(at(&r, x, y), Color32::TRANSPARENT, "at {x},{y}");
        }
    }

    #[test]
    fn colour_goes_under_lines_by_the_extend() {
        // A box with a 9-px wall on the left, x 10..19.
        let mut p = Picture::new(100, 100);
        p.line(true, 10, 10, 90, 9);
        p.line(true, 87, 10, 90, 3);
        p.line(false, 10, 10, 90, 3);
        p.line(false, 87, 10, 90, 3);
        p.hint(50, 50, RED);
        p.hint(2, 2, Color32::from_rgb(255, 0, 255));
        let under = |extend: u8| {
            let r = p.run(ColorizeSettings {
                extend,
                ..Default::default()
            });
            (10..19).filter(|&x| at(&r, x, 50) == RED).count()
        };
        assert_eq!(under(0), 0);
        assert_eq!(under(2), 2);
        assert_eq!(under(4), 4);
        // Far enough, all the way (the left half is the outside's, empty).
        assert!(under(MAX_EXTEND) >= 4);
        // The default, 2 px: from the wall's inside edge (x = 18) in.
        let r = p.run(ColorizeSettings::default());
        assert_eq!(at(&r, 19, 50), RED);
        assert_eq!(at(&r, 17, 50), RED);
        assert_eq!(at(&r, 16, 50), Color32::TRANSPARENT);
    }

    #[test]
    fn lines_on_white_paper_work_as_on_transparency() {
        let mut p = two_boxes(0..0);
        for c in &mut p.lines {
            if *c == Color32::TRANSPARENT {
                *c = Color32::WHITE;
            }
        }
        p.hint(30, 50, RED);
        p.hint(80, 50, BLUE);
        let r = p.run(ColorizeSettings::default());
        assert_eq!(at(&r, 20, 20), RED);
        assert_eq!(at(&r, 100, 80), BLUE);
    }

    #[test]
    fn hints_show_over_the_result_until_hidden() {
        let layer = ColorizeLayer::new(ColorizeSettings::default());
        let mut result = FxHashMap::default();
        result.insert((0, 0), vec![RED; TS * TS]);
        layer.set_result(7, result);
        let mut hint = vec![Color32::TRANSPARENT; TS * TS];
        hint[0] = BLUE;
        let hints = Arc::new(Mutex::new(TileCell::new(Some(hint), false)));
        let shown = layer.shown_tile((0, 0), Some(Arc::clone(&hints))).unwrap();
        let px = |c: &SharedCell, i: usize| c.lock().unwrap().data().unwrap()[i];
        assert_eq!((px(&shown, 0), px(&shown, 1)), (BLUE, RED));
        // Kept while neither changes.
        let again = layer.shown_tile((0, 0), Some(Arc::clone(&hints))).unwrap();
        assert!(Arc::ptr_eq(&shown, &again));
        let mut hidden = layer.clone();
        hidden.settings.show_hints = false;
        let plain = hidden.shown_tile((0, 0), Some(hints)).unwrap();
        assert_eq!(px(&plain, 0), RED);
    }

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn colorize_4k() {
        // A 4000² page of 40-px cells (2-px anti-aliased walls with a 4-px
        // opening in every other one), a hint in every third cell.
        let (w, h) = (4000usize, 4000usize);
        let mut p = Picture::new(w, h);
        for k in (0..w).step_by(40) {
            for t in 0..h {
                let open = (t / 40 + k / 40) % 2 == 0 && t % 40 >= 18 && t % 40 < 22;
                if open {
                    continue;
                }
                p.lines[t * w + k] = Color32::BLACK;
                p.lines[k * w + t] = Color32::BLACK;
                if k + 1 < w {
                    p.lines[t * w + k + 1] = Color32::from_black_alpha(140);
                    p.lines[(k + 1) * w + t] = Color32::from_black_alpha(140);
                }
            }
        }
        let colours = [RED, BLUE, Color32::from_rgb(30, 200, 30)];
        let mut n = 0;
        for cy in (0..h - 40).step_by(40) {
            for cx in (0..w - 40).step_by(40) {
                if (cx / 40 + cy / 40) % 3 == 0 {
                    p.hint(cx + 15, cy + 15, colours[n % 3]);
                    n += 1;
                }
            }
        }
        let job = ColorizeJob {
            width: w,
            height: h,
            tile_size: TS,
            lines: tiles(w, h, &p.lines),
            hints: tiles(w, h, &p.hints),
            settings: ColorizeSettings::default(),
        };
        for gap in [0u8, 8] {
            let job = ColorizeJob {
                settings: ColorizeSettings {
                    gap,
                    ..job.settings
                },
                ..job.clone()
            };
            let t = std::time::Instant::now();
            let r = compute(&job);
            eprintln!("4000² gap {gap}: {:?} ({} tiles)", t.elapsed(), r.len());
        }
    }
}
