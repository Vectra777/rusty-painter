//! Bucket fill and "enclose and fill", with line-art awareness.
//!
//! Both work on a *barrier* map: pixels whose reference colour differs from
//! the area being filled by more than the tolerance (the lines). On top of
//! the plain flood fill:
//! - **gap closing** floods only where lines are more than `gap / 2` px
//!   away, so it can't slip through openings narrower than `gap`, then grows
//!   the result back up to the lines;
//! - **expand** grows the filled area under the lines, so no halo is left
//!   between the colour and anti-aliased line art;
//! - **enclose** (lasso) fills every area the lasso encloses, ignoring the
//!   lines it crosses and the background around it.
//!
//! Everything returns a [`SelectionMask`] of coverage; the caller paints it.

use crate::selection::{SelectionMask, SelectionShape};
use eframe::egui::{Color32, Vec2};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FillSettings {
    /// Largest per-channel difference (0..=255) still counted as the same area.
    pub tolerance: u8,
    /// Close openings in the lines up to this many pixels wide (0 = off).
    pub gap: u8,
    /// Grow the fill this many pixels under the lines.
    pub expand: u8,
    /// Soften the fill's edge by one pixel.
    pub antialias: bool,
}

impl Default for FillSettings {
    fn default() -> Self {
        Self {
            tolerance: 32,
            gap: 0,
            expand: 2,
            antialias: true,
        }
    }
}

/// Renders the reference image for a canvas rectangle `(x, y, w, h)`,
/// row-major premultiplied pixels. Called from several threads.
pub trait Reference: Sync {
    fn render(&self, x: i32, y: i32, w: usize, h: usize) -> Vec<Color32>;
}

impl<F: Fn(i32, i32, usize, usize) -> Vec<Color32> + Sync> Reference for F {
    fn render(&self, x: i32, y: i32, w: usize, h: usize) -> Vec<Color32> {
        self(x, y, w, h)
    }
}

#[inline]
fn diff(a: Color32, b: Color32) -> u8 {
    let d = |x: u8, y: u8| x.abs_diff(y);
    d(a.r(), b.r())
        .max(d(a.g(), b.g()))
        .max(d(a.b(), b.b()))
        .max(d(a.a(), b.a()))
}

/// Chamfer distance units per pixel (3-4 chamfer).
const UNIT: u16 = 3;

/// A working area of the canvas and its barrier map.
struct Grid {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    barrier: Vec<u8>,
}

impl Grid {
    /// Distance (chamfer units, capped at 255) from every pixel to the
    /// nearest barrier pixel.
    fn barrier_distance(&self) -> Vec<u8> {
        let (w, h) = (self.w, self.h);
        let mut d: Vec<u8> = self
            .barrier
            .iter()
            .map(|&b| if b != 0 { 0 } else { 255 })
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

    /// Scanline flood fill from `seed` through pixels where `open` holds,
    /// into `region` (set to `label`). Returns the pixel count.
    fn flood(
        &self,
        seed: usize,
        open: &dyn Fn(usize) -> bool,
        region: &mut [u32],
        label: u32,
    ) -> usize {
        let w = self.w;
        let mut count = 0;
        let mut stack = vec![seed];
        while let Some(i) = stack.pop() {
            if region[i] != 0 || !open(i) {
                continue;
            }
            let y = i / w;
            let row = y * w;
            let (mut l, mut r) = (i, i);
            while l > row && region[l - 1] == 0 && open(l - 1) {
                l -= 1;
            }
            while r + 1 < row + w && region[r + 1] == 0 && open(r + 1) {
                r += 1;
            }
            region[l..=r].fill(label);
            count += r - l + 1;
            for ny in [y.wrapping_sub(1), y + 1] {
                if ny >= self.h {
                    continue;
                }
                let base = ny * w;
                let mut in_run = false;
                for x in (l - row)..=(r - row) {
                    let j = base + x;
                    let ok = region[j] == 0 && open(j);
                    if ok && !in_run {
                        stack.push(j);
                    }
                    in_run = ok;
                }
            }
        }
        count
    }

    /// Grow `region` (non-zero = inside) by `radius` pixels through pixels
    /// where `allowed` holds, alternating 4- and 8-neighbour rings so the
    /// growth is roughly round.
    fn grow(&self, region: &mut [u8], radius: usize, allowed: &dyn Fn(usize) -> bool) {
        if radius == 0 {
            return;
        }
        let (w, h) = (self.w, self.h);
        let mut frontier: Vec<usize> = (0..w * h)
            .into_par_iter()
            .filter(|&i| {
                region[i] != 0 && {
                    let (x, y) = (i % w, i / w);
                    (x > 0 && region[i - 1] == 0)
                        || (x + 1 < w && region[i + 1] == 0)
                        || (y > 0 && region[i - w] == 0)
                        || (y + 1 < h && region[i + w] == 0)
                }
            })
            .collect();
        for ring in 0..radius {
            let diagonal = ring % 2 == 1;
            let mut next = Vec::new();
            for &i in &frontier {
                let (x, y) = ((i % w) as isize, (i / w) as isize);
                for (dx, dy) in [
                    (-1, 0),
                    (1, 0),
                    (0, -1),
                    (0, 1),
                    (-1, -1),
                    (1, -1),
                    (-1, 1),
                    (1, 1),
                ]
                .iter()
                .take(if diagonal { 8 } else { 4 })
                {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if region[j] == 0 && allowed(j) {
                        region[j] = 1;
                        next.push(j);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            // Pixels that stayed on the edge (blocked by the ring type) keep
            // growing next ring.
            next.extend(frontier.iter().copied());
            frontier = next;
        }
    }

    /// The filled area as coverage, optionally with a soft one-pixel edge,
    /// cropped to its content.
    fn into_mask(self, region: &[u8], antialias: bool) -> Option<SelectionMask> {
        let (w, h) = (self.w, self.h);
        let data: Vec<u8> = if antialias {
            // Filled neighbours in each 3×3 block, as a row sum then a
            // column sum (separable: two reads a pixel instead of nine).
            let mut across = vec![0u8; w * h];
            across
                .par_chunks_mut(w.max(1))
                .zip(region.par_chunks(w.max(1)))
                .for_each(|(out, row)| {
                    let at = |x: usize| (row[x] != 0) as u8;
                    for (x, o) in out.iter_mut().enumerate() {
                        let left = if x > 0 { at(x - 1) } else { 0 };
                        let right = if x + 1 < w { at(x + 1) } else { 0 };
                        *o = left + at(x) + right;
                    }
                });
            let mut data = vec![0u8; w * h];
            data.par_chunks_mut(w.max(1))
                .enumerate()
                .for_each(|(y, out)| {
                    for (x, o) in out.iter_mut().enumerate() {
                        let i = y * w + x;
                        if region[i] != 0 {
                            *o = 255;
                            continue;
                        }
                        let up = if y > 0 { across[i - w] } else { 0 };
                        let down = if y + 1 < h { across[i + w] } else { 0 };
                        let n = (up + across[i] + down) as u32;
                        *o = (n * 255 / 9) as u8;
                    }
                });
            data
        } else {
            region
                .iter()
                .map(|&r| if r != 0 { 255 } else { 0 })
                .collect()
        };
        SelectionMask::new(self.x0, self.y0, w, h, data).cropped()
    }

    /// Where gap closing may flood: far enough from any line.
    fn passable(&self, gap: u8) -> Option<Vec<u8>> {
        if gap == 0 {
            return None;
        }
        let dist = self.barrier_distance();
        let min = (gap as u16 * UNIT / 2).min(254) as u8;
        Some(dist.iter().map(|&d| (d > min) as u8).collect())
    }

    /// Grow the flooded area back up to the lines, then under them.
    fn finish(&self, region: &mut [u8], settings: &FillSettings) {
        if settings.gap > 0 {
            let barrier = &self.barrier;
            self.grow(region, settings.gap as usize / 2 + 1, &|j| barrier[j] == 0);
        }
        self.grow(region, settings.expand as usize, &|_| true);
    }
}

/// Barrier map over the whole canvas, worked out one tile-sized block at
/// a time as the flood reaches it: a fill only renders and scans the part
/// of the picture it actually covers.
struct LazyBarrier<'a> {
    reference: &'a dyn Reference,
    target: Color32,
    tolerance: u8,
    w: usize,
    h: usize,
    blocks_w: usize,
    /// Per pixel: [`UNKNOWN`], [`OPEN`], [`LINE`] or [`FILLED`].
    state: Vec<u8>,
    known: Vec<bool>,
}

const BLOCK: usize = 64;
const UNKNOWN: u8 = 0;
const OPEN: u8 = 1;
const LINE: u8 = 2;
/// Reached by the flood.
const FILLED: u8 = 3;

impl<'a> LazyBarrier<'a> {
    fn new(
        reference: &'a dyn Reference,
        w: usize,
        h: usize,
        target: Color32,
        tolerance: u8,
    ) -> Self {
        let blocks_w = w.div_ceil(BLOCK);
        Self {
            reference,
            target,
            tolerance,
            w,
            h,
            blocks_w,
            state: vec![UNKNOWN; w * h],
            known: vec![false; blocks_w * h.div_ceil(BLOCK)],
        }
    }

    #[inline]
    fn block_of(&self, i: usize) -> usize {
        (i / self.w / BLOCK) * self.blocks_w + (i % self.w) / BLOCK
    }

    /// Compute the given blocks (in parallel) if they aren't yet.
    fn ensure(&mut self, blocks: impl IntoIterator<Item = usize>) {
        let mut todo: Vec<usize> = blocks.into_iter().filter(|&b| !self.known[b]).collect();
        todo.sort_unstable();
        todo.dedup();
        if todo.is_empty() {
            return;
        }
        let (w, h, bw) = (self.w, self.h, self.blocks_w);
        let (reference, target, tol) = (self.reference, self.target, self.tolerance);
        let computed: Vec<(usize, Vec<u8>)> = todo
            .par_iter()
            .map(|&b| {
                let (x0, y0) = ((b % bw) * BLOCK, (b / bw) * BLOCK);
                let (cw, ch) = (BLOCK.min(w - x0), BLOCK.min(h - y0));
                let px = reference.render(x0 as i32, y0 as i32, cw, ch);
                (
                    b,
                    px.iter()
                        .map(|&p| if diff(p, target) > tol { LINE } else { OPEN })
                        .collect(),
                )
            })
            .collect();
        for (b, values) in computed {
            let (x0, y0) = ((b % bw) * BLOCK, (b / bw) * BLOCK);
            let cw = BLOCK.min(w - x0);
            for (row, chunk) in values.chunks_exact(cw).enumerate() {
                let start = (y0 + row) * w + x0;
                self.state[start..start + cw].copy_from_slice(chunk);
            }
            self.known[b] = true;
        }
    }

    /// Every block overlapping canvas rectangle `[x0, y0, x1, y1)`.
    fn ensure_rect(&mut self, r: [usize; 4]) {
        let blocks: Vec<usize> = (r[1] / BLOCK..r[3].div_ceil(BLOCK))
            .flat_map(|by| (r[0] / BLOCK..r[2].div_ceil(BLOCK)).map(move |bx| (by, bx)))
            .map(|(by, bx)| by * self.blocks_w + bx)
            .collect();
        self.ensure(blocks);
    }

    /// Plain flood fill from `seed` through open pixels, computing blocks
    /// as it reaches them. Filled pixels are marked [`FILLED`] in `state`;
    /// returns their bounds.
    fn flood(&mut self, seed: usize) -> [usize; 4] {
        let (w, h) = (self.w, self.h);
        let mut bounds = [usize::MAX, usize::MAX, 0, 0];
        self.ensure([self.block_of(seed)]);
        let mut stack = vec![seed];
        let mut blocked = Vec::new();
        loop {
            while let Some(i) = stack.pop() {
                match self.state[i] {
                    UNKNOWN => {
                        blocked.push(i);
                        continue;
                    }
                    OPEN => {}
                    _ => continue,
                }
                let y = i / w;
                let row = y * w;
                let state = &mut self.state;
                let (mut l, mut r) = (i, i);
                while l > row && state[l - 1] == OPEN {
                    l -= 1;
                }
                if l > row && state[l - 1] == UNKNOWN {
                    blocked.push(l - 1);
                }
                while r + 1 < row + w && state[r + 1] == OPEN {
                    r += 1;
                }
                if r + 1 < row + w && state[r + 1] == UNKNOWN {
                    blocked.push(r + 1);
                }
                state[l..=r].fill(FILLED);
                bounds = [
                    bounds[0].min(l - row),
                    bounds[1].min(y),
                    bounds[2].max(r - row + 1),
                    bounds[3].max(y + 1),
                ];
                for ny in [y.wrapping_sub(1), y + 1] {
                    if ny >= h {
                        continue;
                    }
                    let base = ny * w;
                    let mut in_run = false;
                    let start = base + (l - row);
                    for (j, &v) in (start..).zip(&state[start..=base + (r - row)]) {
                        if v == UNKNOWN {
                            blocked.push(j);
                        }
                        let open = v == OPEN;
                        if open && !in_run {
                            stack.push(j);
                        }
                        in_run = open;
                    }
                }
            }
            if blocked.is_empty() {
                break;
            }
            // Work out every block the flood ran into, together.
            let blocks: Vec<usize> = blocked.iter().map(|&i| self.block_of(i)).collect();
            self.ensure(blocks);
            stack.append(&mut blocked);
        }
        bounds
    }
}

/// Bucket fill at canvas pixel `seed` over a `width`×`height` canvas.
pub fn bucket_fill(
    reference: &dyn Reference,
    width: usize,
    height: usize,
    seed: (i32, i32),
    settings: &FillSettings,
) -> Option<SelectionMask> {
    let (sx, sy) = seed;
    if sx < 0 || sy < 0 || sx as usize >= width || sy as usize >= height {
        return None;
    }
    let target = *reference.render(sx, sy, 1, 1).first()?;
    let seed_idx = sy as usize * width + sx as usize;
    let mut lazy = LazyBarrier::new(reference, width, height, target, settings.tolerance);
    let bounds = lazy.flood(seed_idx);

    // Everything else only needs the filled area plus room to grow.
    let margin = settings.gap as usize + settings.expand as usize + 3;
    let win = [
        bounds[0].saturating_sub(margin),
        bounds[1].saturating_sub(margin),
        (bounds[2] + margin).min(width),
        (bounds[3] + margin).min(height),
    ];
    lazy.ensure_rect(win);
    let (ww, wh) = (win[2] - win[0], win[3] - win[1]);
    let mut barrier = vec![0u8; ww * wh];
    let mut region = vec![0u8; ww * wh];
    let state = &lazy.state;
    barrier
        .par_chunks_mut(ww)
        .zip(region.par_chunks_mut(ww))
        .enumerate()
        .for_each(|(y, (b, r))| {
            let src = &state[(win[1] + y) * width + win[0]..][..ww];
            for ((b, r), &v) in b.iter_mut().zip(r.iter_mut()).zip(src) {
                *b = (v == LINE) as u8;
                *r = (v == FILLED) as u8;
            }
        });
    drop(lazy);
    let grid = Grid {
        x0: win[0] as i32,
        y0: win[1] as i32,
        w: ww,
        h: wh,
        barrier,
    };

    // Gap closing: flood again, only where the lines are far enough apart.
    let mut gap = 0;
    if let Some(pass) = grid.passable(settings.gap) {
        let local_seed = (sy as usize - win[1]) * ww + (sx as usize - win[0]);
        if let Some(start) = nearest_open(&grid, local_seed, settings.gap as usize * 2, &pass) {
            let barrier = &grid.barrier;
            let mut labels = vec![0u32; ww * wh];
            grid.flood(start, &|j| pass[j] != 0 && barrier[j] == 0, &mut labels, 1);
            region = labels.iter().map(|&l| (l != 0) as u8).collect();
            gap = settings.gap;
        }
        // Otherwise the seed sits in a nook narrower than the gap: keep
        // the plain fill.
    }
    let settings = FillSettings { gap, ..*settings };
    grid.finish(&mut region, &settings);
    grid.into_mask(&region, settings.antialias)
}

/// The closest pixel to `from` (within `radius` steps, not crossing lines)
/// where `open` is set.
fn nearest_open(grid: &Grid, from: usize, radius: usize, open: &[u8]) -> Option<usize> {
    if open[from] != 0 {
        return Some(from);
    }
    let w = grid.w;
    let mut seen = std::collections::HashSet::from([from]);
    let mut frontier = vec![from];
    for _ in 0..radius.max(1) {
        let mut next = Vec::new();
        for &i in &frontier {
            let (x, y) = (i % w, i / w);
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < grid.h).then(|| i + w),
            ];
            for j in neighbours.into_iter().flatten() {
                if grid.barrier[j] == 0 && seen.insert(j) {
                    if open[j] != 0 {
                        return Some(j);
                    }
                    next.push(j);
                }
            }
        }
        frontier = next;
    }
    None
}

/// Fill every area enclosed by the `lasso` polygon (canvas coordinates).
/// Lines the lasso crosses and the background around it are left alone;
/// the fill then grows under the lines.
pub fn enclose_fill(
    reference: &dyn Reference,
    width: usize,
    height: usize,
    lasso: &[Vec2],
    settings: &FillSettings,
) -> Option<SelectionMask> {
    if lasso.len() < 3 {
        return None;
    }
    let shape = crate::selection::new_lasso_shape(lasso.to_vec());
    let SelectionShape::Lasso {
        bbox_min, bbox_max, ..
    } = &shape
    else {
        return None;
    };
    let margin = settings.gap as i32 + settings.expand as i32 + 3;
    let win = [
        (bbox_min.x.floor() as i32 - margin).max(0),
        (bbox_min.y.floor() as i32 - margin).max(0),
        (bbox_max.x.ceil() as i32 + margin).min(width as i32),
        (bbox_max.y.ceil() as i32 + margin).min(height as i32),
    ];
    if win[2] <= win[0] || win[3] <= win[1] {
        return None;
    }
    let (ww, wh) = ((win[2] - win[0]) as usize, (win[3] - win[1]) as usize);

    // Which window pixels the lasso covers.
    let mut inside = vec![0u8; ww * wh];
    inside.par_chunks_mut(ww).enumerate().for_each(|(y, row)| {
        let mut cov = vec![0.0f32; ww];
        crate::selection::shape_row_coverage(
            &shape,
            win[1] as usize + y,
            win[0] as usize,
            &mut cov,
        );
        for (o, c) in row.iter_mut().zip(cov) {
            *o = (c >= 0.5) as u8;
        }
    });

    // The "paper": the most common colour inside the lasso.
    let mut pixels = vec![Color32::TRANSPARENT; ww * wh];
    pixels
        .par_chunks_mut(ww * BLOCK)
        .enumerate()
        .for_each(|(i, strip)| {
            let rows = strip.len() / ww;
            strip.copy_from_slice(&reference.render(win[0], win[1] + (i * BLOCK) as i32, ww, rows));
        });
    let paper = most_common(
        pixels
            .iter()
            .zip(&inside)
            .filter(|(_, i)| **i != 0)
            .map(|(p, _)| *p),
    )?;
    let grid = Grid {
        barrier: pixels
            .iter()
            .map(|&p| (diff(p, paper) > settings.tolerance) as u8)
            .collect(),
        x0: win[0],
        y0: win[1],
        w: ww,
        h: wh,
    };
    drop(pixels);

    let passable = grid.passable(settings.gap);
    let barrier = &grid.barrier;
    let open = |j: usize| barrier[j] == 0 && passable.as_ref().is_none_or(|p| p[j] != 0);

    // Label the open areas and keep the ones the lasso encloses: mostly
    // inside it, and not running off the window (except at the canvas edge).
    let mut labels = vec![0u32; ww * wh];
    let mut keep = vec![false];
    let open_edge = |x: usize, y: usize| {
        (x == 0 && win[0] > 0)
            || (y == 0 && win[1] > 0)
            || (x + 1 == ww && win[2] < width as i32)
            || (y + 1 == wh && win[3] < height as i32)
    };
    let mut next_label = 1u32;
    for i in 0..ww * wh {
        if labels[i] != 0 || !open(i) {
            continue;
        }
        grid.flood(i, &open, &mut labels, next_label);
        keep.push(false);
        next_label += 1;
    }
    let n = next_label as usize;
    let mut stats = vec![(0usize, 0usize, false); n];
    for (i, &l) in labels.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let s = &mut stats[l as usize];
        s.0 += inside[i] as usize;
        s.1 += 1;
        if open_edge(i % ww, i / ww) {
            s.2 = true;
        }
    }
    for (l, (inn, total, edge)) in stats.iter().enumerate().skip(1) {
        keep[l] = !edge && inn * 2 >= *total && *inn > 0;
    }
    let mut region: Vec<u8> = labels.iter().map(|&l| keep[l as usize] as u8).collect();
    if !region.contains(&1) {
        return None;
    }
    grid.finish(&mut region, settings);
    grid.into_mask(&region, settings.antialias)
}

/// Most frequent colour (bucketed to 5 bits per channel).
/// How [`select_color`] decides a pixel matches the target colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorMatch {
    /// Like the bucket fill: every channel (premultiplied) within
    /// `tolerance`; hard-edged.
    Channels { tolerance: u8 },
    /// By how different the colours look (Oklab, in percent of the black to
    /// white distance, alpha included): fully selected up to `tolerance`,
    /// fading out over `softness` beyond it.
    Perceptual { tolerance: f32, softness: f32 },
}

/// Every pixel of the `width`×`height` reference that matches `target`,
/// wherever it is (not only the connected area), as coverage.
pub fn select_color(
    reference: &dyn Reference,
    width: usize,
    height: usize,
    target: Color32,
    matching: ColorMatch,
) -> Option<SelectionMask> {
    if width == 0 || height == 0 {
        return None;
    }
    // Oklab of the colour behind a premultiplied pixel, and its alpha.
    let lab_alpha = |c: Color32| {
        let [r, g, b, a] = crate::canvas::blend::unmultiply(c);
        (
            crate::canvas::palette::Lab::from_rgb(r, g, b).0,
            a as f32 / 255.0,
        )
    };
    let (target_lab, target_a) = lab_alpha(target);
    let coverage = |c: Color32| -> u8 {
        match matching {
            ColorMatch::Channels { tolerance } => {
                if diff(c, target) <= tolerance {
                    255
                } else {
                    0
                }
            }
            ColorMatch::Perceptual {
                tolerance,
                softness,
            } => {
                let (lab, a) = lab_alpha(c);
                let d2 = (0..3)
                    .map(|i| (lab[i] - target_lab[i]).powi(2))
                    .sum::<f32>();
                // Colour counts as much as both pixels show it; transparent
                // pixels match each other whatever their (invisible) colour.
                let d = 100.0 * (d2.sqrt() * a.min(target_a) + (a - target_a).abs());
                if d <= tolerance {
                    255
                } else if softness > 0.0 && d < tolerance + softness {
                    (255.0 * (1.0 - (d - tolerance) / softness)).round() as u8
                } else {
                    0
                }
            }
        }
    };
    let mut data = vec![0u8; width * height];
    data.par_chunks_mut(width * BLOCK)
        .enumerate()
        .for_each(|(i, strip)| {
            let rows = strip.len() / width;
            let pixels = reference.render(0, (i * BLOCK) as i32, width, rows);
            // Pictures repeat colours (runs, flat areas, palettes): a small
            // cache of recent answers skips most colour conversions.
            const SLOTS: usize = 1024;
            let mut cache = vec![(Color32::TRANSPARENT, 0u8, false); SLOTS];
            for (out, &px) in strip.iter_mut().zip(&pixels) {
                let key = u32::from_le_bytes(px.to_array());
                let slot = (key.wrapping_mul(0x9E37_79B1) >> 22) as usize % SLOTS;
                let entry = &mut cache[slot];
                *out = if entry.2 && entry.0 == px {
                    entry.1
                } else {
                    let v = coverage(px);
                    *entry = (px, v, true);
                    v
                };
            }
        });
    SelectionMask::new(0, 0, width, height, data).cropped()
}

fn most_common(pixels: impl Iterator<Item = Color32>) -> Option<Color32> {
    // A flat table over 5-bit-per-channel buckets (4 MB) beats hashing
    // millions of pixels; a spread-out sample finds the same winner.
    let mut counts = vec![0u32; 1 << 20];
    let mut example = std::collections::HashMap::new();
    let mut any = false;
    for p in pixels.step_by(7) {
        let key = (p.r() as usize >> 3) << 15
            | (p.g() as usize >> 3) << 10
            | (p.b() as usize >> 3) << 5
            | (p.a() as usize >> 3);
        if counts[key] == 0 {
            example.insert(key, p);
        }
        counts[key] += 1;
        any = true;
    }
    if !any {
        return None;
    }
    let best = (0..counts.len()).max_by_key(|&k| counts[k])?;
    example.get(&best).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_select_finds_every_matching_area() {
        let mut img = Image::new(32, 8);
        let red = Color32::from_rgb(220, 30, 30);
        for x in [2usize, 3, 20, 21] {
            img.px[4 * 32 + x] = red;
        }
        // A slightly different red matches perceptually, not by channels.
        img.px[4 * 32 + 10] = Color32::from_rgb(210, 36, 32);
        let reference = img.reference();
        let strict = select_color(
            &reference,
            32,
            8,
            red,
            ColorMatch::Channels { tolerance: 0 },
        )
        .unwrap();
        assert_eq!(strict.value(2, 4), 255);
        assert_eq!(strict.value(21, 4), 255, "not only the connected area");
        assert_eq!(strict.value(10, 4), 0);
        assert_eq!(strict.value(5, 4), 0);
        let loose = select_color(
            &reference,
            32,
            8,
            red,
            ColorMatch::Perceptual {
                tolerance: 5.0,
                softness: 0.0,
            },
        )
        .unwrap();
        assert_eq!(loose.value(10, 4), 255);
        assert_eq!(loose.value(5, 4), 0, "white is far from red");
    }

    #[test]
    fn color_select_softness_fades_coverage() {
        let img = Image::new(4, 1);
        let reference = img.reference();
        let grey = Color32::from_gray(200);
        let mask = select_color(
            &reference,
            4,
            1,
            grey,
            ColorMatch::Perceptual {
                tolerance: 0.0,
                softness: 100.0,
            },
        )
        .unwrap();
        let v = mask.value(0, 0);
        assert!(v > 0 && v < 255, "partly selected: {v}");
    }

    /// A white canvas with black axis-aligned rectangles outlined (1 px).
    struct Image {
        w: usize,
        px: Vec<Color32>,
    }

    impl Image {
        fn new(w: usize, h: usize) -> Self {
            Self {
                w,
                px: vec![Color32::WHITE; w * h],
            }
        }
        fn outline(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, gap_at_top: usize) {
            for x in x0..=x1 {
                let in_gap = gap_at_top > 0 && x >= (x0 + x1) / 2 && x < (x0 + x1) / 2 + gap_at_top;
                if !in_gap {
                    self.px[y0 * self.w + x] = Color32::BLACK;
                }
                self.px[y1 * self.w + x] = Color32::BLACK;
            }
            for y in y0..=y1 {
                self.px[y * self.w + x0] = Color32::BLACK;
                self.px[y * self.w + x1] = Color32::BLACK;
            }
        }
        fn reference(&self) -> impl Fn(i32, i32, usize, usize) -> Vec<Color32> + Sync + '_ {
            move |x, y, w, h| {
                let mut out = Vec::with_capacity(w * h);
                for yy in y as usize..y as usize + h {
                    out.extend_from_slice(
                        &self.px[yy * self.w + x as usize..yy * self.w + x as usize + w],
                    );
                }
                out
            }
        }
    }

    fn sharp(expand: u8, gap: u8) -> FillSettings {
        FillSettings {
            tolerance: 32,
            gap,
            expand,
            antialias: false,
        }
    }

    #[test]
    fn fills_inside_the_lines_only() {
        let mut img = Image::new(100, 100);
        img.outline(20, 20, 60, 60, 0);
        let m = bucket_fill(&img.reference(), 100, 100, (40, 40), &sharp(0, 0)).unwrap();
        assert_eq!(m.value(40, 40), 255);
        assert_eq!(m.value(21, 21), 255);
        assert_eq!(m.value(20, 40), 0, "the line itself");
        assert_eq!(m.value(10, 10), 0, "outside");
        // Expanding reaches under the line but not past it by much.
        let m = bucket_fill(&img.reference(), 100, 100, (40, 40), &sharp(2, 0)).unwrap();
        assert_eq!(m.value(20, 40), 255);
        assert_eq!(m.value(15, 40), 0);
    }

    #[test]
    fn gap_closing_stops_leaks() {
        let mut img = Image::new(100, 100);
        img.outline(20, 20, 60, 60, 3);
        let leaky = bucket_fill(&img.reference(), 100, 100, (40, 40), &sharp(0, 0)).unwrap();
        assert_eq!(leaky.value(5, 5), 255, "leaks through a 3 px gap");
        let closed = bucket_fill(&img.reference(), 100, 100, (40, 40), &sharp(0, 6)).unwrap();
        assert_eq!(closed.value(5, 5), 0, "gap closed");
        assert_eq!(closed.value(40, 40), 255);
        assert_eq!(closed.value(22, 58), 255, "reaches the corners");
    }

    #[test]
    fn enclose_fills_what_the_lasso_surrounds() {
        let mut img = Image::new(120, 80);
        img.outline(10, 10, 40, 40, 0);
        img.outline(70, 10, 100, 40, 0);
        // A rough lasso around the first box that cuts across its lines.
        let lasso = [
            Vec2::new(5.0, 12.0),
            Vec2::new(45.0, 5.0),
            Vec2::new(47.0, 45.0),
            Vec2::new(12.0, 44.0),
        ];
        let m = enclose_fill(&img.reference(), 120, 80, &lasso, &sharp(1, 0)).unwrap();
        assert_eq!(m.value(25, 25), 255, "inside the box");
        assert_eq!(m.value(10, 25), 255, "under its line");
        assert_eq!(m.value(85, 25), 0, "the other box");
        assert_eq!(m.value(60, 60), 0, "the background");
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn bucket_fill_4k() {
        let (w, h) = (4096usize, 4096usize);
        let mut px = vec![Color32::WHITE; w * h];
        for i in 0..w {
            px[2000 * w + i] = Color32::BLACK;
        }
        let reference = |x: i32, y: i32, rw: usize, rh: usize| {
            let mut out = Vec::with_capacity(rw * rh);
            for yy in y as usize..y as usize + rh {
                out.extend_from_slice(&px[yy * w + x as usize..yy * w + x as usize + rw]);
            }
            out
        };
        for gap in [0u8, 8] {
            let t = std::time::Instant::now();
            let settings = FillSettings {
                gap,
                ..FillSettings::default()
            };
            let m = bucket_fill(&reference, w, h, (100, 100), &settings).unwrap();
            eprintln!("gap {gap}: {:?} ({}x{})", t.elapsed(), m.w, m.h);
        }
    }

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn color_select_4k() {
        let (w, h) = (4096usize, 4096usize);
        let px: Vec<Color32> = (0..w * h)
            .map(|i| Color32::from_rgb((i % 251) as u8, (i % 241) as u8, 128))
            .collect();
        let reference = |x: i32, y: i32, rw: usize, rh: usize| {
            let mut out = Vec::with_capacity(rw * rh);
            for yy in y as usize..y as usize + rh {
                out.extend_from_slice(&px[yy * w + x as usize..yy * w + x as usize + rw]);
            }
            out
        };
        let target = Color32::from_rgb(100, 100, 128);
        for matching in [
            ColorMatch::Channels { tolerance: 20 },
            ColorMatch::Perceptual {
                tolerance: 8.0,
                softness: 6.0,
            },
        ] {
            let t = std::time::Instant::now();
            let m = select_color(&reference, w, h, target, matching);
            eprintln!(
                "{matching:?}: {:?} ({:?})",
                t.elapsed(),
                m.map(|m| (m.w, m.h))
            );
        }
    }
}
