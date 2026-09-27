//! Content-aware fill: fill a hole with texture synthesized from the rest of
//! the image (the smart patch).
//!
//! Multi-scale PatchMatch with coherence voting (Barnes et al. 2009, Wexler
//! et al. 2007), the approach behind Photoshop's Content-Aware Fill and
//! GIMP's Resynthesizer:
//! - an image pyramid, down to where the hole is about a patch across;
//! - at the coarsest level, the hole starts as a smooth fill from its edge;
//! - at each level, a few rounds of: find for every patch overlapping the
//!   hole the most similar patch entirely outside it (PatchMatch: random
//!   search plus propagation of good matches to neighbours), then set each
//!   hole pixel to the similarity-weighted vote of all matches covering it;
//! - the matches are carried up to the next level as its starting point.
//!
//! Pixels are linear premultiplied RGBA (0..1), so blending votes mixes
//! light like the compositor does.

use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Pixel = [f32; 4];

fn add(sum: &mut Pixel, p: Pixel) {
    for (s, v) in sum.iter_mut().zip(p) {
        *s += v;
    }
}

/// One band's improved matches: (target index, match, distance).
type BandUpdates = Vec<(usize, (i32, i32), f32)>;

/// Patch half-size: patches are 7×7.
const R: i32 = 3;

/// An image and the pixels to fill.
#[derive(Clone)]
pub struct Problem {
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<Pixel>,
    /// True where the pixel is to be filled.
    pub hole: Vec<bool>,
}

struct Level {
    w: usize,
    h: usize,
    px: Vec<Pixel>,
    hole: Vec<bool>,
    /// Where a patch may be copied from: its whole 7×7 window is inside
    /// the image and outside the hole.
    source: Vec<bool>,
}

fn sources(w: usize, h: usize, hole: &[bool]) -> Vec<bool> {
    // Distance-free check: a pixel is a source if no hole pixel is within
    // R in x and y. Two separable passes of "any hole nearby".
    let (w_i, h_i) = (w as i32, h as i32);
    let mut near_x = vec![false; w * h];
    for y in 0..h {
        let mut last_hole = i32::MIN / 2;
        for x in 0..w_i {
            if hole[y * w + x as usize] {
                last_hole = x;
            }
            near_x[y * w + x as usize] = x - last_hole <= R;
        }
        let mut next_hole = i32::MAX / 2;
        for x in (0..w_i).rev() {
            if hole[y * w + x as usize] {
                next_hole = x;
            }
            if next_hole - x <= R {
                near_x[y * w + x as usize] = true;
            }
        }
    }
    let mut src = vec![false; w * h];
    for x in 0..w {
        let mut last = i32::MIN / 2;
        let mut near = vec![false; h];
        for y in 0..h_i {
            if near_x[y as usize * w + x] {
                last = y;
            }
            near[y as usize] = y - last <= R;
        }
        let mut next = i32::MAX / 2;
        for y in (0..h_i).rev() {
            if near_x[y as usize * w + x] {
                next = y;
            }
            let inside = x as i32 >= R && y >= R && (x as i32) < w_i - R && y < h_i - R;
            src[y as usize * w + x] = inside && !(near[y as usize] || next - y <= R);
        }
    }
    src
}

impl Level {
    fn new(w: usize, h: usize, px: Vec<Pixel>, hole: Vec<bool>) -> Self {
        let source = sources(w, h, &hole);
        Self {
            w,
            h,
            px,
            hole,
            source,
        }
    }

    /// Half the size: box-filtered pixels; a pixel is a hole if any of the
    /// four it covers is (the hole never shrinks away).
    fn half(&self) -> Self {
        let (w, h) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let mut px = vec![[0.0; 4]; w * h];
        let mut hole = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 4];
                let mut n = 0.0;
                let mut any_hole = false;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    if sx < self.w && sy < self.h {
                        let i = sy * self.w + sx;
                        any_hole |= self.hole[i];
                        if !self.hole[i] {
                            add(&mut sum, self.px[i]);
                            n += 1.0;
                        }
                    }
                }
                let i = y * w + x;
                hole[i] = any_hole;
                if n > 0.0 {
                    px[i] = sum.map(|v| v / n);
                }
            }
        }
        Self::new(w, h, px, hole)
    }
}

/// Fill the hole smoothly from its edge inward (repeated averaging of known
/// neighbours): the starting guess at the coarsest level.
fn fill_from_edges(level: &mut Level) {
    let (w, h) = (level.w, level.h);
    let mut known: Vec<bool> = level.hole.iter().map(|&h| !h).collect();
    loop {
        let mut next = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if known[i] {
                    continue;
                }
                let mut sum = [0.0f32; 4];
                let mut n = 0.0;
                for (dx, dy) in [
                    (-1i32, 0i32),
                    (1, 0),
                    (0, -1),
                    (0, 1),
                    (-1, -1),
                    (1, 1),
                    (-1, 1),
                    (1, -1),
                ] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        let j = ny as usize * w + nx as usize;
                        if known[j] {
                            add(&mut sum, level.px[j]);
                            n += 1.0;
                        }
                    }
                }
                if n > 0.0 {
                    next.push((i, sum.map(|v| v / n)));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        for (i, p) in next {
            level.px[i] = p;
            known[i] = true;
        }
    }
}

/// Small deterministic random numbers (xorshift), one stream per row.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1).max(1) as u64) as i32
    }
}

/// Sum of squared differences between the patches at `a` and `b` (centres),
/// over the pixels inside the image; stops early past `limit`.
fn patch_distance(level: &Level, a: (i32, i32), b: (i32, i32), limit: f32) -> f32 {
    let (w, h) = (level.w as i32, level.h as i32);
    // Almost every patch is wholly inside the image (sources always are):
    // compare row slices, which the compiler vectorizes.
    if a.0 >= R && a.1 >= R && a.0 < w - R && a.1 < h - R {
        let side = (2 * R + 1) as usize;
        let mut d = 0.0;
        for dy in -R..=R {
            let ra = ((a.1 + dy) * w + a.0 - R) as usize;
            let rb = ((b.1 + dy) * w + b.0 - R) as usize;
            let (pa, pb) = (&level.px[ra..ra + side], &level.px[rb..rb + side]);
            for (p, q) in pa.iter().zip(pb) {
                let e = [p[0] - q[0], p[1] - q[1], p[2] - q[2], p[3] - q[3]];
                d += e[0] * e[0] + e[1] * e[1] + e[2] * e[2] + e[3] * e[3];
            }
            if d > limit {
                return d;
            }
        }
        return d;
    }
    let mut d = 0.0;
    for dy in -R..=R {
        let (ay, by) = (a.1 + dy, b.1 + dy);
        if ay < 0 || ay >= h {
            continue;
        }
        for dx in -R..=R {
            let ax = a.0 + dx;
            if ax < 0 || ax >= w {
                continue;
            }
            let p = level.px[(ay * w + ax) as usize];
            let q = level.px[(by * w + b.0 + dx) as usize];
            for k in 0..4 {
                let e = p[k] - q[k];
                d += e * e;
            }
        }
        if d > limit {
            return d;
        }
    }
    d
}

/// The nearest-neighbour field: for each target patch centre, the source
/// patch centre it copies, and how far apart they are.
struct Field {
    /// Target pixels covered (patches overlapping the hole).
    targets: Vec<(i32, i32)>,
    matches: Vec<(i32, i32)>,
    dist: Vec<f32>,
}

/// Source centres, for random starting guesses.
fn source_list(level: &Level) -> Vec<(i32, i32)> {
    (0..level.w * level.h)
        .filter(|&i| level.source[i])
        .map(|i| ((i % level.w) as i32, (i / level.w) as i32))
        .collect()
}

/// Patch centres whose patch overlaps the hole.
fn target_list(level: &Level) -> Vec<(i32, i32)> {
    let (w, h) = (level.w as i32, level.h as i32);
    let mut near = vec![false; level.w * level.h];
    for y in 0..h {
        for x in 0..w {
            if level.hole[(y * w + x) as usize] {
                for ny in (y - R).max(0)..=(y + R).min(h - 1) {
                    for nx in (x - R).max(0)..=(x + R).min(w - 1) {
                        near[(ny * w + nx) as usize] = true;
                    }
                }
            }
        }
    }
    (0..level.w * level.h)
        .filter(|&i| near[i])
        .map(|i| ((i % level.w) as i32, (i / level.w) as i32))
        .collect()
}

impl Field {
    fn random(level: &Level, sources: &[(i32, i32)], seed: u64) -> Self {
        let targets = target_list(level);
        let mut rng = Rng::new(seed);
        let matches: Vec<(i32, i32)> = targets
            .iter()
            .map(|_| sources[rng.next() as usize % sources.len()])
            .collect();
        let dist = targets
            .iter()
            .zip(&matches)
            .map(|(&t, &m)| patch_distance(level, t, m, f32::INFINITY))
            .collect();
        Self {
            targets,
            matches,
            dist,
        }
    }

    /// The field from the level below (half the size), scaled up.
    fn upscaled(level: &Level, coarse: &Field, coarse_w: usize, sources: &[(i32, i32)]) -> Self {
        let mut lookup = std::collections::HashMap::with_capacity(coarse.targets.len());
        for (t, m) in coarse.targets.iter().zip(&coarse.matches) {
            lookup.insert(t.1 as usize * coarse_w + t.0 as usize, *m);
        }
        let targets = target_list(level);
        let mut rng = Rng::new(0xA5A5);
        let matches: Vec<(i32, i32)> = targets
            .iter()
            .map(|&(x, y)| {
                let guess = lookup
                    .get(&((y / 2) as usize * coarse_w + (x / 2) as usize))
                    .map(|&(mx, my)| (mx * 2 + (x & 1), my * 2 + (y & 1)));
                match guess {
                    Some((gx, gy))
                        if gx >= 0
                            && gy >= 0
                            && (gx as usize) < level.w
                            && (gy as usize) < level.h
                            && level.source[gy as usize * level.w + gx as usize] =>
                    {
                        (gx, gy)
                    }
                    _ => sources[rng.next() as usize % sources.len()],
                }
            })
            .collect();
        let dist = targets
            .iter()
            .zip(&matches)
            .map(|(&t, &m)| patch_distance(level, t, m, f32::INFINITY))
            .collect();
        Self {
            targets,
            matches,
            dist,
        }
    }

    /// PatchMatch: improve every match by trying its neighbours' matches
    /// (shifted) and random ones nearby at shrinking radii. Runs in row
    /// bands in parallel; good matches spread across bands next round.
    fn improve(&mut self, level: &Level, rounds: usize, seed: u64) {
        let w = level.w;
        let mut index = vec![u32::MAX; level.w * level.h];
        for (i, &(x, y)) in self.targets.iter().enumerate() {
            index[y as usize * w + x as usize] = i as u32;
        }
        let max_radius = level.w.max(level.h) as i32;
        let is_source = |x: i32, y: i32| {
            x >= 0
                && y >= 0
                && (x as usize) < level.w
                && (y as usize) < level.h
                && level.source[y as usize * w + x as usize]
        };
        for round in 0..rounds {
            let reverse = round % 2 == 1;
            let order: Vec<usize> = if reverse {
                (0..self.targets.len()).rev().collect()
            } else {
                (0..self.targets.len()).collect()
            };
            let bands = rayon::current_num_threads().max(1) * 2;
            let chunk = order.len().div_ceil(bands).max(1);
            // Snapshot read by every band; each band writes its own part.
            let matches = self.matches.clone();
            let dist = self.dist.clone();
            let updates: Vec<BandUpdates> = order
                .par_chunks(chunk)
                .enumerate()
                .map(|(band, part)| {
                    let mut rng = Rng::new(seed ^ ((round as u64) << 32) ^ band as u64);
                    let mut local: rustc_hash::FxHashMap<usize, ((i32, i32), f32)> =
                        rustc_hash::FxHashMap::with_capacity_and_hasher(
                            part.len(),
                            Default::default(),
                        );
                    let current =
                        |i: usize, local: &rustc_hash::FxHashMap<usize, ((i32, i32), f32)>| {
                            local.get(&i).copied().unwrap_or((matches[i], dist[i]))
                        };
                    for &i in part {
                        let (tx, ty) = self.targets[i];
                        let (mut best, mut best_d) = current(i, &local);
                        // Propagation from the neighbours already visited.
                        let step: i32 = if reverse { 1 } else { -1 };
                        for (nx, ny) in [(tx + step, ty), (tx, ty + step)] {
                            if nx < 0 || ny < 0 || nx as usize >= level.w || ny as usize >= level.h
                            {
                                continue;
                            }
                            let j = index[ny as usize * w + nx as usize];
                            if j == u32::MAX {
                                continue;
                            }
                            let (m, _) = current(j as usize, &local);
                            let cand = (m.0 - (nx - tx), m.1 - (ny - ty));
                            if is_source(cand.0, cand.1) && cand != best {
                                let d = patch_distance(level, (tx, ty), cand, best_d);
                                if d < best_d {
                                    best = cand;
                                    best_d = d;
                                }
                            }
                        }
                        // Random search around the best match.
                        let mut radius = max_radius;
                        while radius >= 1 {
                            let cand = (
                                rng.range(best.0 - radius, best.0 + radius),
                                rng.range(best.1 - radius, best.1 + radius),
                            );
                            if is_source(cand.0, cand.1) {
                                let d = patch_distance(level, (tx, ty), cand, best_d);
                                if d < best_d {
                                    best = cand;
                                    best_d = d;
                                }
                            }
                            radius /= 2;
                        }
                        local.insert(i, (best, best_d));
                    }
                    local.into_iter().map(|(i, (m, d))| (i, m, d)).collect()
                })
                .collect();
            for band in updates {
                for (i, m, d) in band {
                    self.matches[i] = m;
                    self.dist[i] = d;
                }
            }
        }
    }

    /// Distances are stale after the pixels changed: recompute them.
    fn refresh(&mut self, level: &Level) {
        let dist: Vec<f32> = self
            .targets
            .par_iter()
            .zip(&self.matches)
            .map(|(&t, &m)| patch_distance(level, t, m, f32::INFINITY))
            .collect();
        self.dist = dist;
    }

    /// Set each hole pixel to the weighted vote of every match covering it
    /// (better matches count more). Each pixel gathers its own votes, so
    /// rows run in parallel.
    fn vote(&self, level: &mut Level) {
        let (w, h) = (level.w as i32, level.h as i32);
        let mut sorted = self.dist.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // A robust scale for "how good": the 75th percentile distance.
        let sigma2 = sorted
            .get(sorted.len() * 3 / 4)
            .copied()
            .unwrap_or(1.0)
            .max(1e-6);
        // Which match (if any) is centred on each pixel, and its weight.
        let mut at = vec![u32::MAX; level.w * level.h];
        for (i, &(x, y)) in self.targets.iter().enumerate() {
            at[(y * w + x) as usize] = i as u32;
        }
        let weights: Vec<f32> = self
            .dist
            .iter()
            .map(|&d| (-d / (2.0 * sigma2)).exp().max(1e-8))
            .collect();
        let px = &level.px;
        let hole = &level.hole;
        let voted: Vec<Pixel> = (0..level.w * level.h)
            .into_par_iter()
            .map(|i| {
                if !hole[i] {
                    return px[i];
                }
                let (x, y) = ((i % level.w) as i32, (i / level.w) as i32);
                let mut acc = [0.0f32; 5];
                // Every patch covering (x, y) is centred within R of it.
                for dy in -R..=R {
                    let ty = y - dy;
                    if ty < 0 || ty >= h {
                        continue;
                    }
                    for dx in -R..=R {
                        let tx = x - dx;
                        if tx < 0 || tx >= w {
                            continue;
                        }
                        let t = at[(ty * w + tx) as usize];
                        if t == u32::MAX {
                            continue;
                        }
                        let (mx, my) = self.matches[t as usize];
                        let s = px[((my + dy) * w + mx + dx) as usize];
                        let weight = weights[t as usize];
                        for k in 0..4 {
                            acc[k] += s[k] * weight;
                        }
                        acc[4] += weight;
                    }
                }
                if acc[4] > 0.0 {
                    [
                        acc[0] / acc[4],
                        acc[1] / acc[4],
                        acc[2] / acc[4],
                        acc[3] / acc[4],
                    ]
                } else {
                    px[i]
                }
            })
            .collect();
        level.px = voted;
    }
}

/// Fill `problem`'s hole. `progress` hears 0..1; returns `None` if
/// `cancel` was set, or there's nothing to copy from.
pub fn inpaint(
    problem: &Problem,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f32) + Sync),
) -> Option<Vec<Pixel>> {
    let mut levels = vec![Level::new(
        problem.w,
        problem.h,
        problem.pixels.clone(),
        problem.hole.clone(),
    )];
    // Down to where the hole is about two patches across (or the image is
    // too small to shrink).
    loop {
        let top = levels.last()?;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, &h) in top.hole.iter().enumerate() {
            if h {
                let (x, y) = (i % top.w, i / top.w);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        if x0 == usize::MAX {
            return Some(problem.pixels.clone());
        }
        let span = (x1 - x0).max(y1 - y0) + 1;
        if span <= 4 * R as usize || top.w < 8 * R as usize || top.h < 8 * R as usize {
            break;
        }
        let half = top.half();
        if source_list(&half).is_empty() {
            break;
        }
        levels.push(half);
    }

    let n = levels.len();
    let mut field: Option<(Field, usize)> = None;
    for li in (0..n).rev() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let level = &mut levels[li];
        let sources = source_list(level);
        if sources.is_empty() {
            return None;
        }
        let mut f = match field.take() {
            None => {
                fill_from_edges(level);
                Field::random(level, &sources, 0x1234)
            }
            Some((coarse, coarse_w)) => Field::upscaled(level, &coarse, coarse_w, &sources),
        };
        // More refinement where it's cheap (coarse), less at full size.
        let em = if li == 0 { 3 } else { 5 };
        for e in 0..em {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            f.improve(level, 4, (li as u64) << 8 | e as u64);
            f.vote(level);
            f.refresh(level);
            let done = (n - 1 - li) as f32 + (e + 1) as f32 / em as f32;
            progress(done / n as f32);
        }
        // The next level starts from this one's pixels, scaled up.
        if li > 0 {
            let (cw, ch) = (level.w, level.h);
            let coarse_px = level.px.clone();
            let fine = &mut levels[li - 1];
            for y in 0..fine.h {
                for x in 0..fine.w {
                    let i = y * fine.w + x;
                    if fine.hole[i] {
                        fine.px[i] = coarse_px[(y / 2).min(ch - 1) * cw + (x / 2).min(cw - 1)];
                    }
                }
            }
        }
        field = Some((f, levels[li].w));
    }
    Some(levels.swap_remove(0).px)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &Problem) -> Vec<Pixel> {
        inpaint(p, &AtomicBool::new(false), &|_| {}).unwrap()
    }

    fn with_hole(
        w: usize,
        h: usize,
        px: impl Fn(usize, usize) -> Pixel,
        hole: impl Fn(usize, usize) -> bool,
    ) -> Problem {
        Problem {
            w,
            h,
            pixels: (0..w * h).map(|i| px(i % w, i / w)).collect(),
            hole: (0..w * h).map(|i| hole(i % w, i / w)).collect(),
        }
    }

    #[test]
    fn a_flat_colour_fills_with_that_colour() {
        let colour = [0.2, 0.4, 0.1, 1.0];
        let p = with_hole(
            64,
            64,
            |_, _| colour,
            |x, y| (20..40).contains(&x) && (24..44).contains(&y),
        );
        let out = run(&p);
        for (i, px) in out.iter().enumerate() {
            for k in 0..4 {
                assert!((px[k] - colour[k]).abs() < 1e-3, "pixel {i}: {px:?}");
            }
        }
    }

    #[test]
    fn stripes_continue_through_the_hole() {
        // Vertical stripes 4 px wide; the hole should come back striped.
        let stripe = |x: usize, _y: usize| {
            if (x / 4).is_multiple_of(2) {
                [0.9, 0.9, 0.9, 1.0]
            } else {
                [0.05, 0.05, 0.05, 1.0]
            }
        };
        let p = with_hole(96, 96, stripe, |x, y| {
            (36..60).contains(&x) && (36..60).contains(&y)
        });
        let out = run(&p);
        let mut error = 0.0;
        let mut count = 0.0;
        for y in 36..60 {
            for x in 36..60 {
                error += (out[y * 96 + x][0] - stripe(x, y)[0]).abs();
                count += 1.0;
            }
        }
        let mean = error / count;
        assert!(
            mean < 0.15,
            "mean error {mean} (a plain blur would be ~0.43)"
        );
    }

    #[test]
    fn only_the_hole_changes() {
        let p = with_hole(
            48,
            48,
            |x, y| [x as f32 / 48.0, y as f32 / 48.0, 0.5, 1.0],
            |x, y| (16..30).contains(&x) && (16..30).contains(&y),
        );
        let out = run(&p);
        for ((o, p), hole) in out.iter().zip(&p.pixels).zip(&p.hole) {
            if !hole {
                assert_eq!(o, p);
            }
        }
    }

    #[test]
    fn cancelling_stops_it() {
        let p = with_hole(64, 64, |_, _| [0.5; 4], |x, _| (20..40).contains(&x));
        assert!(inpaint(&p, &AtomicBool::new(true), &|_| {}).is_none());
    }

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn inpaint_300px_hole() {
        // A textured 900×900 crop (what a 300 px hole gets, with margins).
        let texture = |x: usize, y: usize| {
            let v =
                ((x * 7 + y * 13) % 23) as f32 / 23.0 * 0.5 + ((x / 9 + y / 11) % 2) as f32 * 0.4;
            [v, v * 0.8, v * 0.6, 1.0]
        };
        let p = with_hole(900, 900, texture, |x, y| {
            (300..600).contains(&x) && (300..600).contains(&y)
        });
        let t = std::time::Instant::now();
        let out = run(&p);
        eprintln!(
            "300 px hole in 900x900: {:?} ({} px)",
            t.elapsed(),
            out.len()
        );
    }
}
