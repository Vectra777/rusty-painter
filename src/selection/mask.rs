//! Per-pixel selections: what add/subtract combinations, the selection brush
//! and transformed selections are stored as.
//!
//! A mask holds coverage (0..=255) for the pixels of its bounding box; pixels
//! outside it are unselected. It's shared behind an `Arc` so undo steps and
//! transform previews copy it cheaply. The outline (for the marching-ants
//! display) is traced once per mask and cached.

use eframe::egui::Vec2;
use std::collections::HashMap;
use std::sync::OnceLock;

/// How a new selection combines with the current one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionMode {
    #[default]
    Replace,
    Add,
    Subtract,
    /// Keep only what both selections cover.
    Intersect,
}

#[derive(Debug)]
pub struct SelectionMask {
    /// Canvas position of the mask's top-left pixel.
    pub x0: i32,
    pub y0: i32,
    pub w: usize,
    pub h: usize,
    /// Row-major coverage, 0 = unselected, 255 = fully selected.
    pub data: Vec<u8>,
    outline: OnceLock<Vec<Vec<Vec2>>>,
}

impl Clone for SelectionMask {
    fn clone(&self) -> Self {
        Self::new(self.x0, self.y0, self.w, self.h, self.data.clone())
    }
}

impl SelectionMask {
    pub fn new(x0: i32, y0: i32, w: usize, h: usize, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), w * h);
        Self {
            x0,
            y0,
            w,
            h,
            data,
            outline: OnceLock::new(),
        }
    }

    /// An empty (all unselected) mask covering `w`×`h` pixels at `(x0, y0)`.
    pub fn empty(x0: i32, y0: i32, w: usize, h: usize) -> Self {
        Self::new(x0, y0, w, h, vec![0; w * h])
    }

    /// Coverage (0..=255) of canvas pixel `(x, y)`.
    #[inline]
    pub fn value(&self, x: i32, y: i32) -> u8 {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx as usize >= self.w || ly as usize >= self.h {
            return 0;
        }
        self.data[ly as usize * self.w + lx as usize]
    }

    /// Whether the pixel containing canvas point `(x, y)` is selected
    /// (at least half covered).
    #[inline]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.value(x.floor() as i32, y.floor() as i32) >= 128
    }

    /// Bounding box of the selected pixels, in canvas coordinates
    /// (`[min_x, min_y, max_x + 1, max_y + 1]`), or `None` if empty.
    pub fn content_bounds(&self) -> Option<[i32; 4]> {
        use rayon::prelude::*;
        if self.w == 0 {
            return None;
        }
        let rows: Vec<(usize, usize, usize)> = self
            .data
            .par_chunks(self.w)
            .enumerate()
            .filter_map(|(y, row)| {
                let first = row.iter().position(|&v| v > 0)?;
                let last = row.iter().rposition(|&v| v > 0).unwrap_or(first);
                Some((y, first, last))
            })
            .collect();
        let (y0, y1) = (rows.first()?.0, rows.last()?.0);
        let x0 = rows.iter().map(|r| r.1).min()?;
        let x1 = rows.iter().map(|r| r.2).max()?;
        Some([
            self.x0 + x0 as i32,
            self.y0 + y0 as i32,
            self.x0 + x1 as i32 + 1,
            self.y0 + y1 as i32 + 1,
        ])
    }

    /// The same selection cropped to its content (`None` if nothing is selected).
    pub fn cropped(&self) -> Option<Self> {
        let [x0, y0, x1, y1] = self.content_bounds()?;
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let (ox, oy) = ((x0 - self.x0) as usize, (y0 - self.y0) as usize);
        if (ox, oy, w, h) == (0, 0, self.w, self.h) {
            return Some(self.clone());
        }
        let mut data = Vec::with_capacity(w * h);
        for y in 0..h {
            let start = (oy + y) * self.w + ox;
            data.extend_from_slice(&self.data[start..start + w]);
        }
        Some(Self::new(x0, y0, w, h, data))
    }

    /// Rasterize `coverage(y, x0, out)` (anti-aliased row coverage, as
    /// `SelectionManager::row_coverage` gives) over `[x0, y0, x1, y1)`.
    pub fn rasterize(bounds: [i32; 4], coverage: impl Fn(usize, usize, &mut [f32]) + Sync) -> Self {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut data = vec![0; w * h];
        use rayon::prelude::*;
        // Rows in parallel, in strips so each thread reuses its buffer.
        data.par_chunks_mut((w * 16).max(1))
            .enumerate()
            .for_each(|(strip, rows)| {
                let mut row = vec![0.0f32; w];
                for (r, dst_row) in rows.chunks_mut(w.max(1)).enumerate() {
                    let y = strip * 16 + r;
                    coverage((y0 + y as i32) as usize, x0 as usize, &mut row);
                    for (dst, &c) in dst_row.iter_mut().zip(&row) {
                        *dst = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                }
            });
        Self::new(x0, y0, w, h, data)
    }

    /// Combine `self` (the current selection) with `other` (a new one).
    pub fn combine(&self, other: &Self, mode: SelectionMode) -> Self {
        let (x0, y0, x1, y1) = match mode {
            SelectionMode::Replace => return other.clone(),
            SelectionMode::Add => (
                self.x0.min(other.x0),
                self.y0.min(other.y0),
                (self.x0 + self.w as i32).max(other.x0 + other.w as i32),
                (self.y0 + self.h as i32).max(other.y0 + other.h as i32),
            ),
            // Subtracting can only shrink the current selection.
            SelectionMode::Subtract => (
                self.x0,
                self.y0,
                self.x0 + self.w as i32,
                self.y0 + self.h as i32,
            ),
            SelectionMode::Intersect => (
                self.x0.max(other.x0),
                self.y0.max(other.y0),
                (self.x0 + self.w as i32).min(other.x0 + other.w as i32),
                (self.y0 + self.h as i32).min(other.y0 + other.h as i32),
            ),
        };
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut data = vec![0; w * h];
        use rayon::prelude::*;
        data.par_chunks_mut(w.max(1))
            .enumerate()
            .for_each(|(y, row)| {
                let cy = y0 + y as i32;
                for (x, out) in row.iter_mut().enumerate() {
                    let cx = x0 + x as i32;
                    let (a, b) = (self.value(cx, cy) as u16, other.value(cx, cy) as u16);
                    *out = match mode {
                        SelectionMode::Add => a.max(b) as u8,
                        SelectionMode::Subtract => (a * (255 - b) / 255) as u8,
                        SelectionMode::Intersect => a.min(b) as u8,
                        SelectionMode::Replace => b as u8,
                    };
                }
            });
        Self::new(x0, y0, w, h, data)
    }

    /// The selection grown (`radius` > 0) or shrunk (< 0) by `radius`
    /// pixels (exact round distances), with a soft one-pixel edge.
    pub fn grown(&self, radius: i32) -> Option<Self> {
        if radius == 0 {
            return Some(self.clone());
        }
        use rayon::prelude::*;
        let pad = radius.max(0) + 1;
        let (w, h) = (self.w + 2 * pad as usize, self.h + 2 * pad as usize);
        let (x0, y0) = (self.x0 - pad, self.y0 - pad);
        // Squared distance to the nearest pixel on the other side: selected
        // when growing, unselected when shrinking.
        let grow = radius > 0;
        let far = ((w * w + h * h) as f32) * 4.0;
        let mut dist: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let inside = self.value(x0 + (i % w) as i32, y0 + (i / w) as i32) >= 128;
                if inside == grow { 0.0 } else { far }
            })
            .collect();
        squared_distance_transform(&mut dist, w, h);
        let reach = (radius as f32).powi(2);
        let selected: Vec<bool> = dist
            .par_iter()
            .map(|&d| if grow { d <= reach } else { d > reach })
            .collect();
        // Soft edge: unselected pixels next to selected ones get partial
        // coverage.
        let data: Vec<u8> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                if selected[i] {
                    return 255;
                }
                let (x, y) = (i % w, i / w);
                let mut n = 0u32;
                for ny in y.saturating_sub(1)..(y + 2).min(h) {
                    for nx in x.saturating_sub(1)..(x + 2).min(w) {
                        n += selected[ny * w + nx] as u32;
                    }
                }
                (n * 255 / 9) as u8
            })
            .collect();
        Self::new(x0, y0, w, h, data).cropped()
    }

    /// The selection with its edge blurred over `radius` pixels either side:
    /// pixels further than that from the edge keep their coverage, those
    /// nearer fade from selected to unselected (three box blurs, close to a
    /// Gaussian). `None` if nothing is left selected.
    pub fn feathered(&self, radius: u32) -> Option<Self> {
        if radius == 0 {
            return self.cropped();
        }
        let pad = radius as usize;
        let (w, h) = (self.w + 2 * pad, self.h + 2 * pad);
        let mut buf = vec![0.0f32; w * h];
        for y in 0..self.h {
            let (src, dst) = (y * self.w, (y + pad) * w + pad);
            for (d, &s) in buf[dst..dst + self.w]
                .iter_mut()
                .zip(&self.data[src..src + self.w])
            {
                *d = s as f32;
            }
        }
        // Three passes whose radii add up to `radius`, so the blur reaches
        // exactly that far.
        let r = radius as usize;
        let passes = [r.div_ceil(3), (r + 1) / 3, r / 3];
        use rayon::prelude::*;
        let mut t = vec![0.0f32; w * h];
        buf.par_chunks_mut(w).for_each(|row| {
            let mut scratch = Vec::with_capacity(w);
            for &b in &passes {
                box_blur_1d(row, b, &mut scratch);
            }
        });
        // Columns through a transposed copy, so each is contiguous.
        t.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
            for (y, v) in col.iter_mut().enumerate() {
                *v = buf[y * w + x];
            }
            let mut scratch = Vec::with_capacity(h);
            for &b in &passes {
                box_blur_1d(col, b, &mut scratch);
            }
        });
        let data: Vec<u8> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % w, i / w);
                (t[x * h + y] + 0.5).clamp(0.0, 255.0) as u8
            })
            .collect();
        Self::new(self.x0 - pad as i32, self.y0 - pad as i32, w, h, data).cropped()
    }

    /// A band `width` pixels wide along the selection's edge, half outside
    /// and half inside it. `None` if the selection is empty.
    pub fn border(&self, width: u32) -> Option<Self> {
        let width = width.max(1);
        let (outer, inner) = (width.div_ceil(2) as i32, (width / 2) as i32);
        let grown = self.grown(outer)?;
        match self.grown(-inner) {
            Some(core) if inner > 0 => grown.combine(&core, SelectionMode::Subtract).cropped(),
            // Too thin to have an inside: the band is all of it.
            _ if inner > 0 => Some(grown),
            // Only the outer half: take the selection itself away.
            _ => grown.combine(self, SelectionMode::Subtract).cropped(),
        }
    }

    /// The part of the selection inside the canvas `[0, w) × [0, h)`.
    pub fn clipped_to(&self, w: usize, h: usize) -> Option<Self> {
        let (w, h) = (
            w.min(i32::MAX as usize) as i32,
            h.min(i32::MAX as usize) as i32,
        );
        let (x0, y0) = (self.x0.max(0), self.y0.max(0));
        let x1 = (self.x0 + self.w as i32).min(w);
        let y1 = (self.y0 + self.h as i32).min(h);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        if (x0, y0, x1, y1)
            == (
                self.x0,
                self.y0,
                self.x0 + self.w as i32,
                self.y0 + self.h as i32,
            )
        {
            return self.cropped();
        }
        let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let (ox, oy) = ((x0 - self.x0) as usize, (y0 - self.y0) as usize);
        let mut data = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            let start = (oy + y) * self.w + ox;
            data.extend_from_slice(&self.data[start..start + cw]);
        }
        Self::new(x0, y0, cw, ch, data).cropped()
    }

    /// Paint a soft round dab into the mask (growing nothing: the mask must
    /// already cover the area). `add` selects, otherwise deselects.
    pub fn stamp(&mut self, center: Vec2, radius: f32, hardness: f32, add: bool) {
        let r = radius.max(0.5);
        let (min_x, max_x) = ((center.x - r).floor() as i32, (center.x + r).ceil() as i32);
        let (min_y, max_y) = ((center.y - r).floor() as i32, (center.y + r).ceil() as i32);
        let inner = r * hardness.clamp(0.0, 1.0);
        for y in min_y.max(self.y0)..max_y.min(self.y0 + self.h as i32) {
            for x in min_x.max(self.x0)..max_x.min(self.x0 + self.w as i32) {
                let d = Vec2::new(x as f32 + 0.5 - center.x, y as f32 + 0.5 - center.y).length();
                if d >= r {
                    continue;
                }
                let a = if d <= inner {
                    1.0
                } else {
                    1.0 - (d - inner) / (r - inner).max(1e-3)
                };
                let v = (a * 255.0 + 0.5) as u8;
                let i = (y - self.y0) as usize * self.w + (x - self.x0) as usize;
                self.data[i] = if add {
                    self.data[i].max(v)
                } else {
                    (self.data[i] as u16 * (255 - v as u16) / 255) as u8
                };
            }
        }
        self.outline = OnceLock::new();
    }

    /// Map through `inverse` (canvas point in the result -> canvas point in
    /// `self`) over the canvas rect `bounds`, nearest-neighbour.
    pub fn resample(&self, bounds: [i32; 4], inverse: impl Fn(Vec2) -> Vec2) -> Self {
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut data = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                let p = inverse(Vec2::new(
                    (x0 + x as i32) as f32 + 0.5,
                    (y0 + y as i32) as f32 + 0.5,
                ));
                data[y * w + x] = self.value(p.x.floor() as i32, p.y.floor() as i32);
            }
        }
        Self::new(x0, y0, w, h, data)
    }

    /// Closed outlines of the selected area (threshold 50%), in canvas
    /// coordinates along pixel edges. Traced once, then cached.
    pub fn outline(&self) -> &[Vec<Vec2>] {
        self.outline.get_or_init(|| trace_outline(self))
    }
}

/// Walk the boundary edges between selected and unselected pixels and chain
/// them into closed loops.
fn trace_outline(mask: &SelectionMask) -> Vec<Vec<Vec2>> {
    use rayon::prelude::*;
    let (w, h) = (mask.w, mask.h);
    let (x0, y0) = (mask.x0, mask.y0);
    let row_inside = |y: isize| -> Option<&[u8]> {
        (y >= 0 && (y as usize) < h).then(|| &mask.data[y as usize * w..(y as usize + 1) * w])
    };
    // Boundary edges, found row by row in parallel. Directed so the
    // selection is on the right.
    let found: Vec<((i32, i32), (i32, i32))> = (0..h)
        .into_par_iter()
        .flat_map_iter(|ly| {
            let (above, row, below) = (
                row_inside(ly as isize - 1),
                row_inside(ly as isize).unwrap(),
                row_inside(ly as isize + 1),
            );
            let at = |r: Option<&[u8]>, x: isize| -> bool {
                r.is_some_and(|r| x >= 0 && (x as usize) < w && r[x as usize] >= 128)
            };
            let y = y0 + ly as i32;
            let mut out = Vec::new();
            for lx in 0..w {
                if row[lx] < 128 {
                    continue;
                }
                let xi = lx as isize;
                let x = x0 + lx as i32;
                if !at(above, xi) {
                    out.push(((x, y), (x + 1, y)));
                }
                if !at(Some(row), xi + 1) {
                    out.push(((x + 1, y), (x + 1, y + 1)));
                }
                if !at(below, xi) {
                    out.push(((x + 1, y + 1), (x, y + 1)));
                }
                if !at(Some(row), xi - 1) {
                    out.push(((x, y + 1), (x, y)));
                }
            }
            out
        })
        .collect();
    // Each vertex has at most two outgoing edges (diagonal touch points).
    let mut edges: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::with_capacity(found.len());
    for (from, to) in found {
        edges.entry(from).or_default().push(to);
    }
    let mut loops = Vec::new();
    while let Some((&start, _)) = edges.iter().next() {
        let mut points = vec![start];
        let mut at = start;
        while let Some(outs) = edges.get_mut(&at) {
            let next = outs.pop().unwrap_or(start);
            if outs.is_empty() {
                edges.remove(&at);
            }
            at = next;
            if at == start {
                break;
            }
            points.push(at);
        }
        // Drop collinear vertices so long straight edges stay cheap to draw.
        let n = points.len();
        let simplified: Vec<Vec2> = (0..n)
            .filter(|&i| {
                let (a, b, c) = (points[(i + n - 1) % n], points[i], points[(i + 1) % n]);
                (b.0 - a.0) * (c.1 - b.1) != (b.1 - a.1) * (c.0 - b.0)
            })
            .map(|i| Vec2::new(points[i].0 as f32, points[i].1 as f32))
            .collect();
        if simplified.len() >= 3 {
            loops.push(simplified);
        }
    }
    loops
}

/// Exact squared Euclidean distance transform of `f` (0 at the feature
/// pixels, large elsewhere), in place: Felzenszwalb & Huttenlocher's
/// lower envelope of parabolas, columns then rows, each in parallel.
fn squared_distance_transform(f: &mut [f32], w: usize, h: usize) {
    use rayon::prelude::*;
    // Columns, through a transposed copy so each is contiguous.
    let mut t = vec![0.0f32; w * h];
    t.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = f[y * w + x];
        }
        transform_1d(col);
    });
    f.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = t[x * h + y];
        }
        transform_1d(row);
    });
}

/// Average of each value's `2 * radius + 1` neighbourhood (zero past the
/// ends), in place. `scratch` is reused between calls.
fn box_blur_1d(f: &mut [f32], radius: usize, scratch: &mut Vec<f32>) {
    if radius == 0 || f.is_empty() {
        return;
    }
    scratch.clear();
    scratch.extend_from_slice(f);
    let n = f.len();
    let inv = 1.0 / (2 * radius + 1) as f32;
    // Running sum over [i - radius, i + radius], in f64 so it doesn't drift.
    let mut sum: f64 = scratch[..radius.min(n)].iter().map(|&v| v as f64).sum();
    for (i, out) in f.iter_mut().enumerate() {
        if i + radius < n {
            sum += scratch[i + radius] as f64;
        }
        if i > radius {
            sum -= scratch[i - radius - 1] as f64;
        }
        *out = sum as f32 * inv;
    }
}

/// One-dimensional squared distance transform, in place.
fn transform_1d(f: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let src = f.to_vec();
    // Parabola vertices and the boundaries between them.
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        let intersect = |p: usize| {
            ((src[q] + (q * q) as f32) - (src[p] + (p * p) as f32))
                / (2.0 * q as f32 - 2.0 * p as f32)
        };
        let mut s = intersect(v[k]);
        while s <= z[k] {
            k -= 1;
            s = intersect(v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, out) in f.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let d = q as f32 - v[k] as f32;
        *out = d * d + src[v[k]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_transform_is_exact() {
        let (w, h) = (23, 17);
        let features = [(3usize, 4usize), (18, 12), (10, 0)];
        let mut f = vec![1e9f32; w * h];
        for &(x, y) in &features {
            f[y * w + x] = 0.0;
        }
        squared_distance_transform(&mut f, w, h);
        for y in 0..h {
            for x in 0..w {
                let want = features
                    .iter()
                    .map(|&(fx, fy)| {
                        (x as f32 - fx as f32).powi(2) + (y as f32 - fy as f32).powi(2)
                    })
                    .fold(f32::INFINITY, f32::min);
                assert_eq!(f[y * w + x], want, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn grow_and_shrink_move_the_edge() {
        let square = SelectionMask::new(10, 10, 10, 10, vec![255; 100]);
        let grown = square.grown(3).unwrap();
        assert!(grown.value(7, 15) >= 128, "3 px out");
        assert!(!grown.value(5, 15) >= 128, "5 px out");
        // Corners grow round-ish, not square.
        assert!(!grown.value(7, 7) >= 128);
        let shrunk = square.grown(-3).unwrap();
        assert!(shrunk.value(15, 15) >= 128);
        assert!(!shrunk.value(11, 15) >= 128, "edge eaten away");
        assert!(square.grown(-6).is_none(), "shrunk to nothing");
    }

    fn square(x0: i32, y0: i32, size: usize) -> SelectionMask {
        SelectionMask::new(x0, y0, size, size, vec![255; size * size])
    }

    #[test]
    fn add_and_subtract_combine_coverage() {
        let a = square(0, 0, 10);
        let b = square(5, 5, 10);
        let sum = a.combine(&b, SelectionMode::Add);
        assert_eq!((sum.x0, sum.y0, sum.w, sum.h), (0, 0, 15, 15));
        assert!(sum.contains(2.5, 2.5) && sum.contains(12.5, 12.5));
        assert!(!sum.contains(12.5, 2.5));
        let cut = a.combine(&b, SelectionMode::Subtract);
        assert!(cut.contains(2.5, 2.5));
        assert!(!cut.contains(7.5, 7.5), "overlap removed");
        assert_eq!((cut.w, cut.h), (10, 10), "subtract never grows");
    }

    #[test]
    fn outline_of_a_square_is_its_four_corners() {
        let loops = square(3, 4, 5).outline().to_vec();
        assert_eq!(loops.len(), 1);
        let mut corners: Vec<(i32, i32)> =
            loops[0].iter().map(|p| (p.x as i32, p.y as i32)).collect();
        corners.sort();
        assert_eq!(corners, vec![(3, 4), (3, 9), (8, 4), (8, 9)]);
    }

    #[test]
    fn a_ring_has_an_outer_and_an_inner_outline() {
        let mut ring = square(0, 0, 9);
        for y in 3..6 {
            for x in 3..6 {
                ring.data[y * 9 + x] = 0;
            }
        }
        assert_eq!(ring.outline().len(), 2);
    }

    #[test]
    fn stamping_adds_and_removes() {
        let mut m = SelectionMask::empty(0, 0, 40, 40);
        m.stamp(Vec2::new(20.0, 20.0), 8.0, 1.0, true);
        assert!(m.contains(20.0, 20.0) && !m.contains(35.0, 20.0));
        m.stamp(Vec2::new(20.0, 20.0), 3.0, 1.0, false);
        assert!(!m.contains(20.0, 20.0) && m.contains(20.0, 26.0));
        assert_eq!(m.cropped().unwrap().content_bounds(), m.content_bounds());
    }

    /// Selected pixels (at least half covered) of `m`, in canvas coordinates.
    fn selected(m: &SelectionMask) -> std::collections::BTreeSet<(i32, i32)> {
        let mut out = std::collections::BTreeSet::new();
        for y in 0..m.h {
            for x in 0..m.w {
                if m.data[y * m.w + x] >= 128 {
                    out.insert((m.x0 + x as i32, m.y0 + y as i32));
                }
            }
        }
        out
    }

    #[test]
    fn growing_then_shrinking_a_rectangle_gives_it_back() {
        let rect = SelectionMask::new(20, 30, 40, 25, vec![255; 40 * 25]);
        for r in [1, 3, 8] {
            let back = rect.grown(r).unwrap().grown(-r).unwrap();
            assert_eq!(selected(&back), selected(&rect), "radius {r}");
        }
        // And a shrink undone by a grow, away from the corners.
        let back = rect.grown(-4).unwrap().grown(4).unwrap();
        assert!(back.contains(22.5, 42.5) && back.contains(40.5, 31.5));
        assert!(!back.contains(19.5, 42.5));
    }

    #[test]
    fn border_is_a_band_of_the_given_width() {
        let rect = SelectionMask::new(20, 20, 40, 40, vec![255; 40 * 40]);
        for width in [1u32, 4, 7] {
            let band = rect.border(width).unwrap();
            // Across the left edge, along the middle row.
            let across: Vec<i32> = (0..40)
                .filter(|&x| band.contains(x as f32 + 0.5, 40.5))
                .collect();
            assert_eq!(across.len(), width as usize, "width {width}: {across:?}");
            let (first, last) = (across[0], *across.last().unwrap());
            assert_eq!(last - first + 1, width as i32, "one band");
            assert!(first < 20 && last >= 20 - 1, "straddles the edge");
            assert!(!band.contains(40.5, 40.5), "the middle isn't in it");
        }
        // A selection thinner than the band: all of it, plus the outside.
        let thin = SelectionMask::new(0, 0, 2, 2, vec![255; 4]);
        assert!(thin.border(10).unwrap().contains(0.5, 0.5));
    }

    #[test]
    fn feathering_softens_only_near_the_edge() {
        let rect = SelectionMask::new(50, 50, 60, 60, vec![255; 60 * 60]);
        let soft = rect.feathered(9).unwrap();
        assert_eq!(soft.value(80, 80), 255, "the inside stays full");
        assert_eq!(soft.value(60, 80), 255, "10 px in");
        assert_eq!(soft.value(40, 80), 0, "10 px out");
        assert_eq!(soft.value(20, 80), 0);
        let edge = soft.value(50, 80);
        assert!(
            (100..=160).contains(&edge),
            "about half on the edge: {edge}"
        );
        // Coverage falls steadily across the edge.
        let row: Vec<u8> = (38..62).map(|x| soft.value(x, 80)).collect();
        assert!(row.windows(2).all(|p| p[0] <= p[1]), "{row:?}");
        assert!(SelectionMask::empty(0, 0, 8, 8).feathered(3).is_none());
    }

    #[test]
    #[ignore = "timing; run with --release --ignored --nocapture"]
    fn modifying_a_4000_px_selection_is_quick() {
        let n = 4000;
        let all = SelectionMask::new(0, 0, n, n, vec![255; n * n]);
        for (name, f) in [
            (
                "grow 50",
                &(|m: &SelectionMask| m.grown(50)) as &dyn Fn(&_) -> _,
            ),
            ("shrink 50", &|m: &SelectionMask| m.grown(-50)),
            ("feather 50", &|m: &SelectionMask| m.feathered(50)),
            ("border 20", &|m: &SelectionMask| m.border(20)),
        ] {
            let t = std::time::Instant::now();
            assert!(f(&all).is_some());
            println!("{name}: {:?}", t.elapsed());
        }
    }

    #[test]
    fn clipping_keeps_the_part_on_the_canvas() {
        let m = SelectionMask::new(-5, -5, 20, 20, vec![255; 400]);
        let c = m.clipped_to(10, 10).unwrap();
        assert_eq!((c.x0, c.y0, c.w, c.h), (0, 0, 10, 10));
        assert!(m.clipped_to(0, 0).is_none());
    }

    #[test]
    fn resample_by_translation_moves_the_selection() {
        let m = square(0, 0, 4);
        let moved = m.resample([10, 0, 20, 10], |p| p - Vec2::new(10.0, 0.0));
        assert!(moved.contains(11.5, 1.5) && !moved.contains(1.5, 1.5));
    }
}
