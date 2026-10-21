//! Vector layers: lines kept as their points, so they stay sharp and can be
//! erased whole, thinned or thickened, or recoloured after they're drawn,
//! as in Clip Studio's vector layers. The layer's pixels are always these
//! lines rendered ([`render_region`]); nothing else paints on it.

use crate::canvas::raster::{Edges, FillRule};
use eframe::egui::{Color32, Vec2};

mod layer;
mod shape;
pub use layer::VectorLayer;
use shape::Painter;
pub use shape::{Fill, Paint, PaintGradient, PathShape, StrokeStyle, VectorGroup, VectorItem};

/// Widest line, pixels.
pub const MAX_WIDTH: f32 = 400.0;

/// One line: its points (x, y and width there), colour and opacity.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorStroke {
    /// Canvas position and width (pixels) at each point, in order.
    pub points: Vec<[f32; 3]>,
    /// sRGB colour.
    pub colour: [u8; 3],
    /// 0..=1.
    pub opacity: f32,
}

impl VectorStroke {
    /// `[x0, y0, x1, y1)` in whole pixels, with the line's width and a
    /// pixel of anti-aliasing.
    pub fn bounds(&self) -> [i32; 4] {
        bounds_of_widths(&self.points, 1.0)
    }

    /// [`Self::bounds`] of its last `n` points (all of them for more).
    pub fn bounds_of_tail(&self, n: usize) -> [i32; 4] {
        let start = self.points.len().saturating_sub(n);
        bounds_of_widths(&self.points[start..], 1.0)
    }

    /// [`Self::bounds`] were it `scale` times as wide.
    pub fn bounds_scaled(&self, scale: f32) -> [i32; 4] {
        bounds_of_widths(&self.points, scale)
    }

    /// Boxes together covering what the line draws, added to `out`: one
    /// per stretch between points, around the smoothed curve there (each
    /// stretch is a Bézier inside the hull of its control points) and its
    /// width, with a pixel of anti-aliasing.
    pub fn reach(&self, out: &mut Vec<[i32; 4]>) {
        let p = &self.points;
        if p.len() < 3 {
            if !p.is_empty() {
                out.push(self.bounds());
            }
            return;
        }
        for i in 0..p.len() - 1 {
            let (a, b, c, d) = (
                p[i.saturating_sub(1)],
                p[i],
                p[i + 1],
                p[(i + 2).min(p.len() - 1)],
            );
            // Catmull-Rom's Bézier control points, width too.
            let p1 = [0, 1, 2].map(|k| b[k] + (c[k] - a[k]) / 6.0);
            let p2 = [0, 1, 2].map(|k| c[k] - (d[k] - b[k]) / 6.0);
            out.push(bounds_of_widths(&[b, p1, p2, c], 1.0));
        }
    }

    /// The line as drawn (see `path`), for exporting it.
    pub fn smoothed(&self) -> Vec<[f32; 3]> {
        self.path()
    }

    /// The line smoothed through its points (Catmull-Rom, a few steps per
    /// segment), for drawing: a hand-drawn line's corners round off.
    fn path(&self) -> Vec<[f32; 3]> {
        let p = &self.points;
        if p.len() < 3 {
            return p.clone();
        }
        let mut out = Vec::with_capacity(p.len() * 4);
        for i in 0..p.len() - 1 {
            let (a, b, c, d) = (
                p[i.saturating_sub(1)],
                p[i],
                p[i + 1],
                p[(i + 2).min(p.len() - 1)],
            );
            let len = ((c[0] - b[0]).powi(2) + (c[1] - b[1]).powi(2)).sqrt();
            let steps = ((len / 3.0).ceil() as usize).clamp(1, 16);
            for s in 0..steps {
                let t = s as f32 / steps as f32;
                let (t2, t3) = (t * t, t * t * t);
                out.push([0, 1, 2].map(|k| {
                    0.5 * (2.0 * b[k]
                        + (-a[k] + c[k]) * t
                        + (2.0 * a[k] - 5.0 * b[k] + 4.0 * c[k] - d[k]) * t2
                        + (-a[k] + 3.0 * b[k] - 3.0 * c[k] + d[k]) * t3)
                }));
            }
        }
        out.push(p[p.len() - 1]);
        // Widths never go negative between points.
        for q in &mut out {
            q[2] = q[2].max(0.0);
        }
        out
    }

    /// Whether a circle at `c` of radius `r` touches the line.
    pub fn touches(&self, c: Vec2, r: f32) -> bool {
        Self::path_touches(&self.path(), c, r)
    }

    /// [`Self::touches`] for the line smoothed as `path` (see
    /// [`Self::smoothed`]): made once for many circles.
    pub fn path_touches(path: &[[f32; 3]], c: Vec2, r: f32) -> bool {
        if path.is_empty() {
            return false;
        }
        if path.len() == 1 {
            let [x, y, w] = path[0];
            return len(Vec2::new(x, y) - c) <= r + w * 0.5;
        }
        // Squared distances, the segments far off skipped on their boxes.
        path.windows(2).any(|s| {
            let reach = r + s[0][2].max(s[1][2]) * 0.5;
            if c.x + reach < s[0][0].min(s[1][0])
                || c.x - reach > s[0][0].max(s[1][0])
                || c.y + reach < s[0][1].min(s[1][1])
                || c.y - reach > s[0][1].max(s[1][1])
            {
                return false;
            }
            let (a, d) = (xy(s[0]), xy(s[1]) - xy(s[0]));
            let len2 = d.x * d.x + d.y * d.y;
            let t = if len2 > 1e-9 {
                ((c - a).dot(d) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let e = a + d * t - c;
            let near = r + (s[0][2] + (s[1][2] - s[0][2]) * t) * 0.5;
            e.x * e.x + e.y * e.y <= near * near
        })
    }

    /// The line with what a circle at `c` of radius `r` covers taken out:
    /// the pieces left (none if it was all covered), each thinned out again.
    pub fn cut(&self, c: Vec2, r: f32) -> Vec<VectorStroke> {
        self.cut_path(&self.path(), c, r)
    }

    /// [`Self::cut`], the line smoothed as `path` (see [`Self::smoothed`]).
    pub fn cut_path(&self, path: &[[f32; 3]], c: Vec2, r: f32) -> Vec<VectorStroke> {
        if !Self::path_touches(path, c, r) {
            return vec![self.clone()];
        }
        // The line as drawn, in steps fine enough to cut cleanly.
        let step = (r * 0.25).clamp(0.25, 2.0);
        let mut dense = Vec::with_capacity(path.len() * 2);
        for pair in path.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let n = (len / step).ceil().max(1.0) as usize;
            for i in 0..n {
                let t = i as f32 / n as f32;
                dense.push([0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t));
            }
        }
        dense.extend(path.last());
        let mut pieces = Vec::new();
        let mut current: Vec<[f32; 3]> = Vec::new();
        for &p in &dense {
            let inside = len(Vec2::new(p[0], p[1]) - c) <= r + p[2] * 0.25;
            if inside {
                if !current.is_empty() {
                    pieces.push(std::mem::take(&mut current));
                }
            } else {
                current.push(p);
            }
        }
        if !current.is_empty() {
            pieces.push(current);
        }
        pieces
            .into_iter()
            .map(|points| VectorStroke {
                points: simplify(&points, 0.2),
                colour: self.colour,
                opacity: self.opacity,
            })
            .collect()
    }
}

impl VectorStroke {
    /// The line with the stretch the eraser at `c` is on taken out, up to
    /// where the line crosses one of `others` or itself, or to its ends
    /// (Clip Studio's "up to intersection"): the pieces left.
    pub fn cut_to_crossings(&self, c: Vec2, others: &[&VectorStroke]) -> Vec<VectorStroke> {
        let paths: Vec<(RunPath, [i32; 4])> = (others.iter())
            .map(|o| (RunPath::new(o.path()), o.bounds()))
            .collect();
        self.cut_to_crossings_by(c, &mut |a, b| {
            let mut found = Vec::new();
            for (path, bounds) in &paths {
                if !box_meets(*bounds, segment_box(a, b)) {
                    continue;
                }
                crossings_with(a, b, path, &mut found);
            }
            found
        })
    }

    /// [`Self::cut_to_crossings`], the other lines' crossings with a
    /// segment of this one found by `crossing(a, b)` (how far along it,
    /// 0..=1). From the eraser, the line is followed each way to the first
    /// crossing only: on a page of line art, that's a few segments, not
    /// every line's every segment.
    pub fn cut_to_crossings_by(
        &self,
        c: Vec2,
        crossing: &mut dyn FnMut(Vec2, Vec2) -> Vec<f32>,
    ) -> Vec<VectorStroke> {
        self.cut_path_to_crossings_by(self.path(), c, crossing)
    }

    /// [`Self::cut_to_crossings_by`], the line smoothed as `path` (see
    /// [`Self::smoothed`]).
    pub fn cut_path_to_crossings_by(
        &self,
        path: Vec<[f32; 3]>,
        c: Vec2,
        crossing: &mut dyn FnMut(Vec2, Vec2) -> Vec<f32>,
    ) -> Vec<VectorStroke> {
        let own = RunPath::new(path);
        let path = &own.path;
        if path.len() < 2 {
            return Vec::new();
        }
        // Distance along the line to each point.
        let mut along = Vec::with_capacity(path.len());
        let mut total = 0.0;
        along.push(0.0);
        for w in path.windows(2) {
            total += len(xy(w[1]) - xy(w[0]));
            along.push(total);
        }
        // Where along it the eraser is: its nearest point.
        let (hit_seg, hit_t) = path
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let (d, t) = segment_distance(c, w[0], w[1]);
                (d, i, t)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or((0, 0.0), |(_, i, t)| (i, t));
        let segments = path.len() - 1;
        // Where segment `i` is crossed, by the others or by the line itself
        // (not by its neighbours, which share its ends).
        let mut crossed = |i: usize| -> Vec<f32> {
            let (a, b) = (xy(path[i]), xy(path[i + 1]));
            let mut ts = crossing(a, b);
            for j in own
                .segments_near(segment_box(a, b))
                .filter(|&j| j.abs_diff(i) > 1)
            {
                if let Some((t, _)) = segment_cross(a, b, xy(path[j]), xy(path[j + 1])) {
                    ts.push(t);
                }
            }
            ts
        };
        let at = |i: usize, t: f32| along[i] + (along[i + 1] - along[i]) * t;
        let mut before = None;
        for i in (0..=hit_seg).rev() {
            let ts = crossed(i);
            let best = (ts.into_iter())
                .filter(|&t| i < hit_seg || t < hit_t)
                .max_by(f32::total_cmp);
            if let Some(t) = best {
                before = Some(at(i, t));
                break;
            }
        }
        let mut after = None;
        for i in hit_seg..segments {
            let ts = crossed(i);
            let best = (ts.into_iter())
                .filter(|&t| i > hit_seg || t > hit_t)
                .min_by(f32::total_cmp);
            if let Some(t) = best {
                after = Some(at(i, t));
                break;
            }
        }
        let piece = |from: f32, to: f32| VectorStroke {
            points: simplify(&stretch(path, &along, from, to), 0.2),
            colour: self.colour,
            opacity: self.opacity,
        };
        let mut pieces = Vec::new();
        if let Some(b) = before.filter(|&b| b > 0.0) {
            pieces.push(piece(0.0, b));
        }
        if let Some(a) = after.filter(|&a| a < total) {
            pieces.push(piece(a, total));
        }
        pieces
    }
}

/// `[x0, y0, x1, y1)` around a segment.
pub fn segment_box(a: Vec2, b: Vec2) -> [i32; 4] {
    [
        floor(a.x.min(b.x)),
        floor(a.y.min(b.y)),
        ceil(a.x.max(b.x)) + 1,
        ceil(a.y.max(b.y)) + 1,
    ]
}

fn box_meets(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// A smoothed path with a box around each run of [`RUN`] segments, so
/// finding what crosses a segment skips the runs nowhere near it.
pub struct RunPath {
    pub path: Vec<[f32; 3]>,
    runs: Vec<[i32; 4]>,
}

/// Segments to a run.
const RUN: usize = 16;

impl RunPath {
    pub fn new(path: Vec<[f32; 3]>) -> Self {
        let runs = (0..path.len().saturating_sub(1))
            .step_by(RUN)
            .map(|i| {
                let end = (i + RUN + 1).min(path.len());
                let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
                for p in &path[i..end] {
                    b = [
                        b[0].min(floor(p[0])),
                        b[1].min(floor(p[1])),
                        b[2].max(ceil(p[0]) + 1),
                        b[3].max(ceil(p[1]) + 1),
                    ];
                }
                b
            })
            .collect();
        RunPath { path, runs }
    }

    /// Segments (by index) whose run's box meets `reach`.
    fn segments_near(&self, reach: [i32; 4]) -> impl Iterator<Item = usize> + '_ {
        let n = self.path.len().saturating_sub(1);
        (self.runs.iter().enumerate())
            .filter(move |(_, b)| box_meets(**b, reach))
            .flat_map(move |(r, _)| r * RUN..((r + 1) * RUN).min(n))
    }
}

/// Where the segment `a`→`b` crosses `path` (how far along it, 0..=1),
/// added to `found`.
pub fn crossings_with(a: Vec2, b: Vec2, path: &RunPath, found: &mut Vec<f32>) {
    let reach = segment_box(a, b);
    for i in path.segments_near(reach) {
        let (c, d) = (xy(path.path[i]), xy(path.path[i + 1]));
        if !box_meets(reach, segment_box(c, d)) {
            continue;
        }
        if let Some((t, _)) = segment_cross(a, b, c, d) {
            found.push(t);
        }
    }
}

/// Where segments `a`→`b` and `c`→`d` cross: how far along each (0..=1).
/// Parallel ones (overlapping or not) don't cross.
pub(crate) fn segment_cross(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<(f32, f32)> {
    let (r, s) = (b - a, d - c);
    let denom = r.x * s.y - r.y * s.x;
    if denom.abs() < 1e-9 {
        return None;
    }
    let q = c - a;
    let t = (q.x * s.y - q.y * s.x) / denom;
    let u = (q.x * r.y - q.y * r.x) / denom;
    const EPS: f32 = 1e-5;
    ((-EPS..=1.0 + EPS).contains(&t) && (-EPS..=1.0 + EPS).contains(&u))
        .then(|| (t.clamp(0.0, 1.0), u.clamp(0.0, 1.0)))
}

/// The part of `path` (its points `along` that far) from `from` to `to`
/// along it, the ends interpolated (width too).
fn stretch(path: &[[f32; 3]], along: &[f32], from: f32, to: f32) -> Vec<[f32; 3]> {
    let at = |s: f32| {
        let i = along.partition_point(|&a| a <= s).clamp(1, path.len() - 1);
        let (a, b) = (path[i - 1], path[i]);
        let len = along[i] - along[i - 1];
        let t = if len > 1e-9 {
            ((s - along[i - 1]) / len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t)
    };
    let mut out = vec![at(from)];
    out.extend(
        (path.iter().zip(along))
            .filter(|&(_, &s)| s > from && s < to)
            .map(|(p, _)| *p),
    );
    out.push(at(to));
    out
}

fn xy(p: [f32; 3]) -> Vec2 {
    Vec2::new(p[0], p[1])
}

/// `v`'s length, without the `hypot` library call `Vec2::length` makes
/// (it showed in profiles of erasing).
fn len(v: Vec2) -> f32 {
    (v.x * v.x + v.y * v.y).sqrt()
}

/// [`VectorStroke::bounds`] of `points` with their widths times `scale`,
/// rounded once at the end (it runs over every point of a line).
pub(super) fn bounds_of_widths(points: &[[f32; 3]], scale: f32) -> [i32; 4] {
    if points.is_empty() {
        return [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    }
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for &[x, y, w] in points {
        let r = (w * scale).clamp(0.0, MAX_WIDTH) * 0.5 + 1.0;
        b[0] = b[0].min(x - r);
        b[1] = b[1].min(y - r);
        b[2] = b[2].max(x + r);
        b[3] = b[3].max(y + r);
    }
    [floor(b[0]), floor(b[1]), ceil(b[2]) + 1, ceil(b[3]) + 1]
}

/// Distance from `p` to the segment `a`→`b` (x, y of each), and where along
/// it (0..=1) the nearest point is.
fn segment_distance(p: Vec2, a: [f32; 3], b: [f32; 3]) -> (f32, f32) {
    let (a2, b2) = (Vec2::new(a[0], a[1]), Vec2::new(b[0], b[1]));
    let d = b2 - a2;
    let len2 = d.length_sq();
    let t = if len2 > 1e-9 {
        ((p - a2).dot(d) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (len(a2 + d * t - p), t)
}

/// Drop points closer than `min_gap` to the last one kept (the first and
/// last always stay), and those that lie on a straight run to within
/// `tolerance` pixels (Ramer–Douglas–Peucker): a hand-drawn line keeps
/// its shape in far fewer points.
pub fn simplify(points: &[[f32; 3]], tolerance: f32) -> Vec<[f32; 3]> {
    simplify_indices(points, tolerance)
        .into_iter()
        .map(|i| points[i])
        .collect()
}

/// Which points [`simplify`] keeps, in order.
pub fn simplify_indices(points: &[[f32; 3]], tolerance: f32) -> Vec<usize> {
    if points.len() < 3 {
        return (0..points.len()).collect();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((i, j)) = stack.pop() {
        let (mut worst, mut at) = (0.0, i);
        for k in i + 1..j {
            let (d, _) =
                segment_distance(Vec2::new(points[k][0], points[k][1]), points[i], points[j]);
            // A width change counts too, so pressure stays.
            let t = (k - i) as f32 / (j - i) as f32;
            let w = points[i][2] + (points[j][2] - points[i][2]) * t;
            let d = d.max((points[k][2] - w).abs() * 0.5);
            if d > worst {
                (worst, at) = (d, k);
            }
        }
        if worst > tolerance {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
    (0..points.len()).filter(|&i| keep[i]).collect()
}

/// `points` with the one at `handles[h]` moved by `delta` and widened by
/// `widen`, the points between it and the handles either side following
/// less the further they are (a smooth falloff along the line), so the line
/// bends and keeps its detail. `handles` are point indices, in order.
pub fn bend(
    points: &[[f32; 3]],
    handles: &[usize],
    h: usize,
    delta: Vec2,
    widen: f32,
) -> Vec<[f32; 3]> {
    let at = handles[h];
    let from = if h > 0 { handles[h - 1] } else { at };
    let to = handles.get(h + 1).copied().unwrap_or(at);
    let mut along = vec![0.0; points.len()];
    for k in 1..points.len() {
        along[k] = along[k - 1] + len(xy(points[k]) - xy(points[k - 1]));
    }
    let weight = |k: usize| -> f32 {
        let (near, far) = match k.cmp(&at) {
            std::cmp::Ordering::Equal => return 1.0,
            std::cmp::Ordering::Less if k > from => (along[at] - along[k], along[at] - along[from]),
            std::cmp::Ordering::Greater if k < to => (along[k] - along[at], along[to] - along[at]),
            _ => return 0.0,
        };
        let t = 1.0 - (near / far.max(1e-6)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    (points.iter().enumerate())
        .map(|(k, &[x, y, w])| {
            let f = weight(k);
            [
                x + delta.x * f,
                y + delta.y * f,
                (w + widen * f).clamp(0.0, MAX_WIDTH),
            ]
        })
        .collect()
}

/// `points` without the handle `handles[h]`: the line runs straight (then
/// smoothed) from the handle before it to the one after. An end handle
/// takes the line back to its neighbour.
pub fn remove_handle(points: &[[f32; 3]], handles: &[usize], h: usize) -> Vec<[f32; 3]> {
    let at = handles[h];
    let (from, to) = match (h.checked_sub(1).map(|p| handles[p]), handles.get(h + 1)) {
        (Some(from), Some(&to)) => (from + 1, to),
        // An end: everything up to (or from) the next handle goes.
        (None, Some(&to)) => (0, to),
        (Some(from), None) => (from + 1, points.len()),
        (None, None) => (at, at + 1),
    };
    let mut out = points[..from].to_vec();
    out.extend_from_slice(&points[to..]);
    out
}

/// `items` drawn over transparent, for the canvas pixels
/// `[x0, y0, x1, y1)`: row-major, premultiplied, as layer tiles hold them.
/// A large region is drawn in bands of rows at once, on the rayon pool.
/// `items` are those that may reach it (all of a layer's, or the ones its
/// index finds near), bottom first: brush lines or a layer's items.
pub fn render_region<'a, T: Drawable + Sync + 'a>(
    items: impl IntoIterator<Item = &'a T>,
    region: [i32; 4],
) -> Vec<Color32> {
    use rayon::prelude::*;
    let [x0, y0, x1, y1] = region;
    let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
    // What reaches the region, each made ready once for every band.
    // (On the pool too: smoothing thousands of lines is a lot of work.)
    let items: Vec<&T> = items.into_iter().collect();
    let items: Vec<Prepared> = if items.len() > 64 {
        items.par_iter().filter_map(|i| i.prepare(region)).collect()
    } else {
        items.iter().filter_map(|i| i.prepare(region)).collect()
    };
    let mut out = vec![Color32::TRANSPARENT; w * h];
    if w == 0 || items.is_empty() {
        return out;
    }
    // Bands small enough to share out, big enough to be worth it.
    const BAND_PIXELS: usize = 1 << 16;
    let band = (BAND_PIXELS / w).clamp(16, h.max(16));
    if w * h <= BAND_PIXELS * 2 {
        render_band(&items, [x0, y0, x1, y1], &mut out);
    } else {
        out.par_chunks_mut(band * w)
            .enumerate()
            .for_each(|(i, rows)| {
                let top = y0 + (i * band) as i32;
                let bottom = top + (rows.len() / w) as i32;
                render_band(&items, [x0, top, x1, bottom], rows);
            });
    }
    out
}

/// Something [`render_region`] draws.
pub trait Drawable {
    /// Made ready to draw over `region`, if it reaches it.
    fn prepare(&self, region: [i32; 4]) -> Option<Prepared>;
}

/// `b` clipped to `region`, if anything's left.
fn clipped(b: [i32; 4], region: [i32; 4]) -> Option<[i32; 4]> {
    let c = [
        b[0].max(region[0]),
        b[1].max(region[1]),
        b[2].min(region[2]),
        b[3].min(region[3]),
    ];
    (c[0] < c[2] && c[1] < c[3]).then_some(c)
}

impl Drawable for VectorStroke {
    fn prepare(&self, region: [i32; 4]) -> Option<Prepared> {
        if self.points.is_empty() {
            return None;
        }
        let clip = clipped(self.bounds(), region)?;
        Some(Prepared::Line(Line::new(self, clip)))
    }
}

impl Drawable for VectorItem {
    fn prepare(&self, region: [i32; 4]) -> Option<Prepared> {
        match self {
            VectorItem::Line(l) => l.prepare(region),
            VectorItem::Shape(s) => {
                let clip = clipped(s.bounds(), region)?;
                let (fill, outline) = s.edges();
                let fill = fill
                    .zip(s.fill.as_ref())
                    .map(|(e, f)| (e, f.rule, Painter::new(&f.paint)));
                let outline = outline
                    .zip(s.stroke.as_ref())
                    .map(|(e, st)| (e, Painter::new(&st.paint)));
                (fill.is_some() || outline.is_some()).then(|| {
                    Prepared::Shape(Box::new(PreparedShape {
                        fill,
                        outline,
                        opacity: s.opacity.clamp(0.0, 1.0),
                        clip,
                    }))
                })
            }
            VectorItem::Group(g) => {
                if !g.visible || g.opacity <= 0.0 {
                    return None;
                }
                let clip = clipped(self.bounds(), region)?;
                let items: Vec<Prepared> =
                    g.items.iter().filter_map(|i| i.prepare(region)).collect();
                (!items.is_empty()).then_some(Prepared::Group {
                    items,
                    opacity: g.opacity.min(1.0),
                    clip,
                })
            }
        }
    }
}

impl<T: Drawable> Drawable for std::sync::Arc<T> {
    fn prepare(&self, region: [i32; 4]) -> Option<Prepared> {
        (**self).prepare(region)
    }
}

/// A shape ready to draw: the edges of its fill and outline, and their
/// paints.
pub struct PreparedShape {
    fill: Option<(Edges, FillRule, Painter)>,
    outline: Option<(Edges, Painter)>,
    opacity: f32,
    clip: [i32; 4],
}

/// An item ready to draw.
pub enum Prepared {
    Line(Line),
    Shape(Box<PreparedShape>),
    Group {
        items: Vec<Prepared>,
        opacity: f32,
        clip: [i32; 4],
    },
}

/// A line ready to draw: its smoothed path as segments, where it's
/// clipped to the region, and its paint.
pub struct Line {
    segments: Vec<Segment>,
    /// The box round each run of [`SEGMENT_RUN`] segments, so a band skips
    /// the runs nowhere near it rather than looking at every segment.
    runs: Vec<[f32; 4]>,
    clip: [i32; 4],
    /// See [`Line::paint`].
    paint: Box<[[Color32; 2]; 256]>,
    opacity: f32,
}

/// Segments to a [`Line::runs`] box.
const SEGMENT_RUN: usize = 16;

/// One stretch of a smoothed path, with what every pixel near it needs.
struct Segment {
    a: Vec2,
    ab: Vec2,
    /// 1 / |ab|², or 0 for a dot.
    inv_len2: f32,
    /// Width at `a`, and how it changes towards `b`.
    width: f32,
    dw: f32,
    /// The square of how far from it a pixel's centre can be and still be
    /// covered, and its normal that long: what bounds each row it covers.
    radius2: f32,
    normal: Vec2,
    /// The same for how far a pixel's centre is surely covered whole (the
    /// square of a negative radius where nowhere is).
    solid2: f32,
    solid_normal: Vec2,
    /// How x moves along it per row (0 if it runs flat).
    dxdy: f32,
    /// Pixels it can cover, `[x0, y0, x1, y1]` before clipping.
    reach: [f32; 4],
    /// Its ends' widths differ: it's drawn as the shape round both ends'
    /// circles (see [`Cone`]) rather than straight sides between them.
    /// (Worked out as it's drawn: the segments stay small.)
    cone: bool,
}

/// A segment whose width changes, as the shape its two end circles span
/// (their tangents and arcs, so the edges of a line whose width keeps
/// changing meet smoothly at its points): what each pixel's distance to
/// it needs.
#[derive(Clone, Copy)]
struct Cone {
    /// |ab|², and that less the radii's difference squared (times 1/|ab|²).
    len2: f32,
    a2: f32,
    a2_inv_len2: f32,
    /// The end circles' radii (half a pixel at least), their difference,
    /// and that squared with its sign.
    ra: f32,
    rb: f32,
    rr: f32,
    krr: f32,
}

impl Line {
    fn new(stroke: &VectorStroke, clip: [i32; 4]) -> Self {
        let path = stroke.path();
        let segment = |a: [f32; 3], b: [f32; 3]| {
            let (pa, ab) = (Vec2::new(a[0], a[1]), Vec2::new(b[0] - a[0], b[1] - a[1]));
            let len2 = ab.length_sq();
            let r = a[2].max(b[2]) * 0.5 + 1.0;
            // (Coverage reaches half the width, at least half a pixel, and
            // half a pixel more for the anti-aliased edge.)
            let reach = (a[2].max(b[2]) * 0.5).max(0.5) + 0.5;
            // (Whole where coverage's edge is a pixel or more off: half the
            // narrower end's width less half a pixel, a little less to be
            // sure.)
            let solid = a[2].min(b[2]) * 0.5 - 0.51;
            let normal = |r: f32| {
                if len2 > 1e-9 {
                    Vec2::new(-ab.y, ab.x) * (r / len2.sqrt())
                } else {
                    Vec2::ZERO
                }
            };
            Segment {
                a: pa,
                ab,
                inv_len2: if len2 > 1e-9 { 1.0 / len2 } else { 0.0 },
                width: a[2],
                dw: b[2] - a[2],
                radius2: reach * reach,
                normal: normal(reach),
                solid2: if solid > 0.0 { solid * solid } else { -1.0 },
                solid_normal: normal(solid.max(0.0)),
                dxdy: if ab.y.abs() > 1e-6 { ab.x / ab.y } else { 0.0 },
                reach: [
                    a[0].min(b[0]) - r,
                    a[1].min(b[1]) - r,
                    a[0].max(b[0]) + r + 1.0,
                    a[1].max(b[1]) + r + 1.0,
                ],
                cone: Cone::new(len2, a[2], b[2]).is_some(),
            }
        };
        let segments = if path.len() == 1 {
            vec![segment(path[0], path[0])]
        } else {
            path.windows(2).map(|s| segment(s[0], s[1])).collect()
        };
        let runs = segments
            .chunks(SEGMENT_RUN)
            .map(|run| {
                run.iter()
                    .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, s| {
                        [
                            b[0].min(s.reach[0]),
                            b[1].min(s.reach[1]),
                            b[2].max(s.reach[2]),
                            b[3].max(s.reach[3]),
                        ]
                    })
            })
            .collect();
        Line {
            segments,
            runs,
            clip,
            paint: Line::paint(stroke.colour),
            opacity: stroke.opacity.clamp(0.0, 1.0),
        }
    }
}

/// The centre of pixel row `y`.
fn cy_of(y: i32) -> f32 {
    y as f32 + 0.5
}

/// `v` rounded down, without the library call `f32::floor` makes on
/// x86-64 targets without SSE4.1 (it showed in profiles of the renderer).
fn floor(v: f32) -> i32 {
    let i = v as i32;
    i - (v < i as f32) as i32
}

/// `v` rounded up (see [`floor`]).
fn ceil(v: f32) -> i32 {
    let i = v as i32;
    i + (v > i as f32) as i32
}

/// What drawing a band needs besides its pixels, kept between items.
struct Scratch {
    /// A line's coverage, kept all zero between lines: each clears what it
    /// used.
    coverage: Vec<f32>,
    /// The columns of each row the line touched, `[from, to)` (band
    /// columns; empty: from past to), so only those are laid down.
    spans: Vec<[i32; 2]>,
    /// The columns of each row the line surely covers whole so far,
    /// `[from, to)`, which its other segments needn't look at.
    solid: Vec<[i32; 2]>,
}

/// No columns touched.
const NO_SPAN: [i32; 2] = [i32::MAX, i32::MIN];

/// `items` drawn into `out`, the canvas pixels `[x0, y0, x1, y1)`.
fn render_band(items: &[Prepared], band: [i32; 4], out: &mut [Color32]) {
    let (w, h) = ((band[2] - band[0]) as usize, (band[3] - band[1]) as usize);
    let mut scratch = Scratch {
        coverage: vec![0.0; w * h],
        spans: vec![NO_SPAN; h],
        solid: vec![NO_SPAN; h],
    };
    draw_items(items, band, out, &mut scratch);
}

fn draw_items(items: &[Prepared], band: [i32; 4], out: &mut [Color32], scratch: &mut Scratch) {
    for item in items {
        match item {
            Prepared::Line(line) => draw_line(line, band, out, scratch),
            Prepared::Shape(shape) => {
                let PreparedShape {
                    fill,
                    outline,
                    opacity,
                    clip,
                } = &**shape;
                let Some(area) = clipped(*clip, band) else {
                    continue;
                };
                // Faint with both parts: they're one picture, faded together.
                if *opacity < 1.0 && fill.is_some() && outline.is_some() {
                    let mut layer = vec![Color32::TRANSPARENT; out.len()];
                    draw_shape(fill.as_ref(), outline.as_ref(), 1.0, area, band, &mut layer);
                    fade_over(&layer, *opacity, out);
                } else {
                    draw_shape(fill.as_ref(), outline.as_ref(), *opacity, area, band, out);
                }
            }
            Prepared::Group {
                items,
                opacity,
                clip,
            } => {
                if clipped(*clip, band).is_none() {
                    continue;
                }
                if *opacity >= 1.0 {
                    draw_items(items, band, out, scratch);
                } else {
                    let mut layer = vec![Color32::TRANSPARENT; out.len()];
                    draw_items(items, band, &mut layer, scratch);
                    fade_over(&layer, *opacity, out);
                }
            }
        }
    }
}

/// `layer` faded to `opacity` and laid over `out`.
fn fade_over(layer: &[Color32], opacity: f32, out: &mut [Color32]) {
    for (o, &p) in out.iter_mut().zip(layer) {
        if p.a() == 0 {
            continue;
        }
        let f = |v: u8| (v as f32 * opacity + 0.5) as u8;
        let src = Color32::from_rgba_premultiplied(f(p.r()), f(p.g()), f(p.b()), f(p.a()));
        *o = crate::canvas::storage::gamma_over(src, *o);
    }
}

/// A shape's fill then outline over `area` (within `band`, whose pixels
/// `out` holds), at `opacity`.
fn draw_shape(
    fill: Option<&(Edges, FillRule, Painter)>,
    outline: Option<&(Edges, Painter)>,
    opacity: f32,
    area: [i32; 4],
    band: [i32; 4],
    out: &mut [Color32],
) {
    let (aw, ah) = ((area[2] - area[0]) as usize, (area[3] - area[1]) as usize);
    let bw = (band[2] - band[0]) as usize;
    let mut cov = vec![0.0f32; aw * ah];
    let parts = [
        fill.map(|(e, rule, p)| (e, *rule, p)),
        outline.map(|(e, p)| (e, FillRule::NonZero, p)),
    ];
    for (edges, rule, painter) in parts.into_iter().flatten() {
        cov.iter_mut().for_each(|c| *c = 0.0);
        if !edges.coverage(rule, area, &mut cov) {
            continue;
        }
        for y in 0..ah {
            for x in 0..aw {
                let c = cov[y * aw + x] * opacity;
                if c <= 0.0 {
                    continue;
                }
                let (px, py) = (area[0] + x as i32, area[1] + y as i32);
                let i = (py - band[1]) as usize * bw + (px - band[0]) as usize;
                out[i] = crate::canvas::storage::gamma_over(painter.at(px, py, c), out[i]);
            }
        }
    }
}

/// A brush line drawn into `out` (the band's pixels).
fn draw_line(line: &Line, band: [i32; 4], out: &mut [Color32], scratch: &mut Scratch) {
    let [x0, y0, x1, y1] = band;
    let w = (x1 - x0) as usize;
    let Scratch {
        coverage,
        spans,
        solid,
    } = scratch;
    let [bx0, by0, bx1, by1] = [
        line.clip[0].max(x0),
        line.clip[1].max(y0),
        line.clip[2].min(x1),
        line.clip[3].min(y1),
    ];
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    // The line's coverage of each pixel (the most any part of it gives,
    // so where it overlaps itself it doesn't darken).
    let (fx0, fy0, fx1, fy1) = (bx0 as f32, by0 as f32, bx1 as f32, by1 as f32);
    let meets =
        |[rx0, ry0, rx1, ry1]: [f32; 4]| !(rx1 < fx0 || rx0 > fx1 || ry1 < fy0 || ry0 > fy1);
    let near = (line.runs.iter().zip(line.segments.chunks(SEGMENT_RUN)))
        .filter(|(run, _)| meets(**run))
        .flat_map(|(_, segments)| segments);
    for seg in near {
        let [rx0, ry0, rx1, ry1] = seg.reach;
        if rx1 < fx0 || rx0 > fx1 || ry1 < fy0 || ry0 > fy1 {
            continue;
        }
        let sy0 = floor(ry0.max(fy0)).max(by0);
        let sy1 = ceil(ry1.min(fy1)).min(by1);
        let cone = seg.cone();
        for y in sy0..sy1 {
            let cy = cy_of(y);
            // Only the pixels of the row it can reach.
            let Some((lo, hi)) = row_span(seg, cy, seg.radius2, seg.normal) else {
                continue;
            };
            let sx0 = floor(lo - 0.5).max(bx0);
            let sx1 = (ceil(hi - 0.5) + 1).min(bx1);
            if sx0 >= sx1 {
                continue;
            }
            let span = &mut spans[(y - y0) as usize];
            span[0] = span[0].min(sx0 - x0);
            span[1] = span[1].max(sx1 - x0);
            // Without branches or a division, so it runs several pixels at
            // a time: a pixel the segment doesn't reach gets a coverage
            // below zero, which the max leaves out.
            let dy0 = cy - seg.a.y;
            let along_y = dy0 * seg.ab.y;
            let row = (y - y0) as usize * w;
            // Less what's covered whole already: a thick line's segments
            // overlap a lot, and only their edges are left to work out.
            let whole = &mut solid[(y - y0) as usize];
            let [f0, f1] = *whole;
            let pieces = if f0 < f1 {
                [(sx0, sx1.min(f0)), (sx0.max(f1), sx1)]
            } else {
                [(sx0, sx1), (0, 0)]
            };
            for (p0, p1) in pieces {
                if p0 >= p1 {
                    continue;
                }
                let cov = &mut coverage[row + (p0 - x0) as usize..row + (p1 - x0) as usize];
                let fx = p0 as f32 + 0.5 - seg.a.x;
                match &cone {
                    Some(cone) => {
                        for (k, slot) in cov.iter_mut().enumerate() {
                            let v = seg.cone_coverage(cone, fx + k as f32, dy0, along_y);
                            *slot = if v > *slot { v } else { *slot };
                        }
                    }
                    None => {
                        for (k, slot) in cov.iter_mut().enumerate() {
                            let v = seg.plain_coverage(fx + k as f32, dy0, along_y);
                            *slot = if v > *slot { v } else { *slot };
                        }
                    }
                }
            }
            // What this one covers whole, kept if it meets what's kept (so
            // it stays one range) or is wider.
            if seg.solid2 > 0.0
                && let Some((lo, hi)) = row_span(seg, cy, seg.solid2, seg.solid_normal)
            {
                let g0 = ceil(lo - 0.5).max(sx0);
                let g1 = (floor(hi - 0.5) + 1).min(sx1);
                if g0 < g1 {
                    *whole = if f0 < f1 && g0 <= f1 && f0 <= g1 {
                        [f0.min(g0), f1.max(g1)]
                    } else if f0 >= f1 || g1 - g0 > f1 - f0 {
                        [g0, g1]
                    } else {
                        [f0, f1]
                    };
                }
            }
        }
    }
    let paint = &line.paint;
    for y in by0..by1 {
        solid[(y - y0) as usize] = NO_SPAN;
        let [from, to] = std::mem::replace(&mut spans[(y - y0) as usize], NO_SPAN);
        if from >= to {
            continue;
        }
        let row = (y - y0) as usize * w;
        for i in row + from as usize..row + to as usize {
            let a = std::mem::take(&mut coverage[i]) * line.opacity;
            let a8 = (a * 255.0 + 0.5) as u8;
            if a <= 0.0 || a8 == 0 {
                continue;
            }
            let [src, alone] = paint[a8 as usize];
            out[i] = if out[i].a() == 0 {
                alone
            } else {
                crate::canvas::storage::gamma_over(src, out[i])
            };
        }
    }
}

impl Cone {
    /// For a segment `len2` long squared whose ends are `wa` and `wb` wide,
    /// if they differ enough to show (and neither end's circle holds the
    /// other's).
    fn new(len2: f32, wa: f32, wb: f32) -> Option<Cone> {
        let (ra, rb) = ((wa * 0.5).max(0.5), (wb * 0.5).max(0.5));
        let rr = ra - rb;
        let a2 = len2 - rr * rr;
        // Straight sides between the circles miss their tangents by about
        // r·rr²/2|ab|²: a twentieth of a pixel or less doesn't show (and a
        // thin line is quicker drawn so).
        let off = ra.max(rb) * rr * rr * 0.5;
        (off > 0.05 * len2 && a2 > 1e-6 * len2).then(|| Cone {
            len2,
            a2,
            a2_inv_len2: a2 / len2,
            ra,
            rb,
            rr,
            krr: rr.signum() * rr * rr,
        })
    }
}

/// How far `p` is outside the shape circles of radius `ra` round `a` and
/// `rb` round `b` span (below zero inside): the plain sum, for checking
/// the renderer against.
#[cfg(test)]
fn cone_distance(p: Vec2, a: Vec2, ra: f32, b: Vec2, rb: f32) -> f32 {
    let ba = b - a;
    let l2 = ba.length_sq();
    let rr = ra - rb;
    let a2 = l2 - rr * rr;
    if l2 <= 1e-9 || a2 <= 0.0 {
        return ((p - a).length() - ra).min((p - b).length() - rb);
    }
    let pa = p - a;
    let y = pa.dot(ba);
    let z = y - l2;
    let x2 = (pa * l2 - ba * y).length_sq();
    let (y2, z2) = (y * y * l2, z * z * l2);
    let k = rr.signum() * rr * rr * x2;
    if z.signum() * a2 * z2 > k {
        (x2 + z2).sqrt() / l2 - rb
    } else if y.signum() * a2 * y2 < k {
        (x2 + y2).sqrt() / l2 - ra
    } else {
        ((x2 * a2 / l2).sqrt() + y * rr) / l2 - ra
    }
}

impl Segment {
    /// How much it covers the pixel whose centre is `dx0`, `dy0` from its
    /// start (`along_y`: `dy0` times its height): below zero where it
    /// doesn't reach.
    #[cfg(test)]
    fn coverage(&self, dx0: f32, dy0: f32, along_y: f32) -> f32 {
        match self.cone() {
            Some(cone) => self.cone_coverage(&cone, dx0, dy0, along_y),
            None => self.plain_coverage(dx0, dy0, along_y),
        }
    }

    /// Its [`Cone`], if it's drawn as one.
    fn cone(&self) -> Option<Cone> {
        if !self.cone {
            return None;
        }
        Cone::new(self.ab.length_sq(), self.width, self.width + self.dw)
    }

    /// [`Self::coverage`] of a width-changing one: without branches (each
    /// case worked out and one picked), so it too runs several pixels at a
    /// time.
    #[inline(always)]
    fn cone_coverage(&self, c: &Cone, dx0: f32, dy0: f32, along_y: f32) -> f32 {
        let y = dx0 * self.ab.x + along_y;
        let t = (y * self.inv_len2).clamp(0.0, 1.0);
        let half = (self.width + self.dw * t) * 0.5;
        let fade = (half * 2.0).clamp(0.0, 1.0);
        let z = y - c.len2;
        let (qx, qy) = (dx0 * c.len2 - self.ab.x * y, dy0 * c.len2 - self.ab.y * y);
        let x2 = qx * qx + qy * qy;
        let (y2, z2) = (y * y * c.len2, z * z * c.len2);
        let k = c.krr * x2;
        // Past b's circle, before a's, or along the sides.
        let past = (if z > 0.0 { c.a2 * z2 } else { -c.a2 * z2 }) > k;
        let before = !past && (if y > 0.0 { c.a2 * y2 } else { -c.a2 * y2 }) < k;
        let arg = if past {
            x2 + z2
        } else if before {
            x2 + y2
        } else {
            x2 * c.a2_inv_len2
        };
        let root = arg.sqrt();
        let outside = if past {
            root * self.inv_len2 - c.rb
        } else if before {
            root * self.inv_len2 - c.ra
        } else {
            (root + y * c.rr) * self.inv_len2 - c.ra
        };
        (0.5 - outside).min(1.0) * fade
    }

    /// [`Self::coverage`] of one as wide all along.
    #[inline(always)]
    fn plain_coverage(&self, dx0: f32, dy0: f32, along_y: f32) -> f32 {
        let t = ((dx0 * self.ab.x + along_y) * self.inv_len2).clamp(0.0, 1.0);
        let (dx, dy) = (self.ab.x * t - dx0, self.ab.y * t - dy0);
        let d2 = dx * dx + dy * dy;
        let half = (self.width + self.dw * t) * 0.5;
        // Thinner than a pixel: fainter rather than vanishing.
        let fade = (half * 2.0).clamp(0.0, 1.0);
        let edge = half.max(0.5) + 0.5;
        (edge - d2.sqrt()).min(1.0) * fade
    }
}

impl Line {
    /// For each alpha: the line's colour at it, and that laid over nothing
    /// (most of a vector layer is empty under its lines).
    fn paint(colour: [u8; 3]) -> Box<[[Color32; 2]; 256]> {
        let [r, g, b] = colour;
        let mut out = Box::new([[Color32::TRANSPARENT; 2]; 256]);
        for (a, p) in out.iter_mut().enumerate() {
            let src = Color32::from_rgba_unmultiplied(r, g, b, a as u8);
            *p = [
                src,
                crate::canvas::storage::gamma_over(src, Color32::TRANSPARENT),
            ];
        }
        out
    }
}

/// Where row `cy` (a pixel centre's y) meets the points within a radius of
/// `seg` (its square `radius2`, `normal` its normal that long): the pixels
/// it can cover for [`Segment::radius2`]. That's a capsule, so one range:
/// the circles round its ends and the two sides between them.
fn row_span(seg: &Segment, cy: f32, radius2: f32, normal: Vec2) -> Option<(f32, f32)> {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    let b = seg.a + seg.ab;
    for c in [seg.a, b] {
        let dy = cy - c.y;
        let h = radius2 - dy * dy;
        if h >= 0.0 {
            let half = h.sqrt();
            lo = lo.min(c.x - half);
            hi = hi.max(c.x + half);
        }
    }
    // The sides: the segment moved its radius out each way. (Flat ones'
    // sides lie within the ends' circles' ranges, or off the row.)
    if seg.dxdy != 0.0 || seg.ab.x == 0.0 {
        for n in [normal, -normal] {
            let (p, q) = (seg.a + n, b + n);
            if (p.y - cy) * (q.y - cy) <= 0.0 && p.y != q.y {
                let x = p.x + (cy - p.y) * seg.dxdy;
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
    }
    (lo <= hi).then_some((lo, hi))
}

/// The union of `[x0, y0, x1, y1)` boxes (`None` if there are none).
pub fn union(boxes: impl IntoIterator<Item = [i32; 4]>) -> Option<[i32; 4]> {
    boxes.into_iter().reduce(|a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[[f32; 3]]) -> VectorStroke {
        VectorStroke {
            points: points.to_vec(),
            colour: [0, 0, 0],
            opacity: 1.0,
        }
    }

    /// The renderer before it was optimised (`hypot`, no bands), to check
    /// the fast one against: `strokes` drawn over transparent, for the canvas pixels
    /// `[x0, y0, x1, y1)`: row-major, premultiplied, as layer tiles hold them.
    fn reference_render(strokes: &[VectorStroke], region: [i32; 4]) -> Vec<Color32> {
        let [x0, y0, x1, y1] = region;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut out = vec![Color32::TRANSPARENT; w * h];
        // Kept all zero between lines: each line clears what it used.
        let mut coverage = vec![0.0f32; w * h];
        // Which `CHUNK`-wide pieces of each row the line touched, so a long
        // line only visits the pixels near it rather than its whole box.
        const CHUNK: i32 = 32;
        let chunks = (w as i32 + CHUNK - 1) / CHUNK;
        let mut touched = vec![false; (chunks as usize) * h];
        for stroke in strokes {
            let b = stroke.bounds();
            let [bx0, by0, bx1, by1] = [b[0].max(x0), b[1].max(y0), b[2].min(x1), b[3].min(y1)];
            if bx0 >= bx1 || by0 >= by1 || stroke.points.is_empty() {
                continue;
            }
            // The line's coverage of each pixel (the most any part of it gives,
            // so where it overlaps itself it doesn't darken).
            let path = stroke.path();
            let segments: Vec<([f32; 3], [f32; 3])> = if path.len() == 1 {
                vec![(path[0], path[0])]
            } else {
                path.windows(2).map(|s| (s[0], s[1])).collect()
            };
            for (a, c) in segments {
                let r = a[2].max(c[2]) * 0.5 + 1.0;
                let sx0 = ((a[0].min(c[0]) - r).floor() as i32).max(bx0);
                let sy0 = ((a[1].min(c[1]) - r).floor() as i32).max(by0);
                let sx1 = ((a[0].max(c[0]) + r).ceil() as i32 + 1).min(bx1);
                let sy1 = ((a[1].max(c[1]) + r).ceil() as i32 + 1).min(by1);
                if sx0 >= sx1 {
                    continue;
                }
                for y in sy0..sy1 {
                    let row = (y - y0) as usize * w;
                    let flags = (y - y0) as usize * chunks as usize;
                    for k in (sx0 - x0) / CHUNK..=(sx1 - 1 - x0) / CHUNK {
                        touched[flags + k as usize] = true;
                    }
                    for x in sx0..sx1 {
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let (plain, t) = segment_distance(p, a, c);
                        let half = (a[2] + (c[2] - a[2]) * t) * 0.5;
                        // Thinner than a pixel: fainter rather than vanishing.
                        let fade = (half * 2.0).clamp(0.0, 1.0);
                        // The shape the ends' circles span (straight sides
                        // when they're alike, or one holds the other).
                        let r = |w: f32| (w * 0.5).max(0.5);
                        let xy = |q: [f32; 3]| Vec2::new(q[0], q[1]);
                        let len2 = (xy(c) - xy(a)).length_sq();
                        let d = match Cone::new(len2, a[2], c[2]) {
                            Some(_) => cone_distance(p, xy(a), r(a[2]), xy(c), r(c[2])),
                            None => plain - r(a[2] + (c[2] - a[2]) * t),
                        };
                        let v = (0.5 - d).clamp(0.0, 1.0) * fade;
                        let slot = &mut coverage[row + (x - x0) as usize];
                        if v > *slot {
                            *slot = v;
                        }
                    }
                }
            }
            let [r, g, b] = stroke.colour;
            let opacity = stroke.opacity.clamp(0.0, 1.0);
            for y in by0..by1 {
                let row = (y - y0) as usize * w;
                let flags = (y - y0) as usize * chunks as usize;
                for k in (bx0 - x0) / CHUNK..=(bx1 - 1 - x0) / CHUNK {
                    if !std::mem::take(&mut touched[flags + k as usize]) {
                        continue;
                    }
                    let from = (k * CHUNK) as usize;
                    let to = ((k + 1) * CHUNK).min(w as i32) as usize;
                    for i in row + from..row + to {
                        let a = std::mem::take(&mut coverage[i]) * opacity;
                        if a <= 0.0 {
                            continue;
                        }
                        let src = Color32::from_rgba_unmultiplied(r, g, b, (a * 255.0 + 0.5) as u8);
                        out[i] = crate::canvas::storage::gamma_over(src, out[i]);
                    }
                }
            }
        }
        out
    }

    /// Line art of `n` wavy lines over `size`, some thinner than a pixel,
    /// some faint, some dots.
    fn scattered(n: usize, size: f32) -> Vec<VectorStroke> {
        let mut seed = 0x9e37_79b9_u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        (0..n)
            .map(|i| {
                let start = Vec2::new(rand(), rand()) * size;
                let (dir, len) = (Vec2::angled(rand() * 6.3), 5.0 + rand() * 200.0);
                let width = if i % 7 == 0 {
                    rand()
                } else {
                    0.5 + rand() * 14.0
                };
                let n = if i % 11 == 0 { 0 } else { (len / 4.0) as usize };
                let points = (0..=n)
                    .map(|k| {
                        let t = k as f32 / n.max(1) as f32;
                        let p = start + dir * len * t + dir.rot90() * 12.0 * (t * 7.0).sin();
                        [p.x, p.y, width * (0.3 + t)]
                    })
                    .collect();
                VectorStroke {
                    points,
                    colour: [(i * 40 % 256) as u8, 90, 200],
                    opacity: if i % 5 == 0 { 0.6 } else { 1.0 },
                }
            })
            .collect()
    }

    #[test]
    fn a_rows_span_holds_every_pixel_the_segment_covers() {
        let mut seed = 99u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        for _ in 0..20000 {
            let a = [rand() * 40.0, rand() * 40.0, rand() * 12.0];
            let b = [
                a[0] + (rand() - 0.5) * 20.0,
                a[1] + (rand() - 0.5) * 20.0,
                rand() * 12.0,
            ];
            let st = line(&[a, b]);
            let l = Line::new(&st, [-100, -100, 200, 200]);
            for seg in &l.segments {
                for y in -30..80 {
                    let cy = y as f32 + 0.5;
                    let span = row_span(seg, cy, seg.radius2, seg.normal);
                    let whole = (seg.solid2 > 0.0)
                        .then(|| row_span(seg, cy, seg.solid2, seg.solid_normal))
                        .flatten();
                    for x in -30..80 {
                        let cx = x as f32 + 0.5;
                        let t = (((cx - seg.a.x) * seg.ab.x + (cy - seg.a.y) * seg.ab.y)
                            * seg.inv_len2)
                            .clamp(0.0, 1.0);
                        let (dx, dy) = (seg.a.x + seg.ab.x * t - cx, seg.a.y + seg.ab.y * t - cy);
                        let half = (seg.width + seg.dw * t) * 0.5;
                        let edge = half.max(0.5) + 0.5;
                        // What's taken as covered whole is.
                        if whole
                            .is_some_and(|(lo, hi)| x >= ceil(lo - 0.5) && x < floor(hi - 0.5) + 1)
                        {
                            let v =
                                seg.coverage(cx - seg.a.x, cy - seg.a.y, (cy - seg.a.y) * seg.ab.y);
                            assert!(v >= 1.0, "{a:?} {b:?} x {x} y {y} covers {v}");
                        }
                        if (dx * dx + dy * dy).sqrt() < edge {
                            let ok = span.is_some_and(|(lo, hi)| {
                                x >= floor(lo - 0.5) && x < ceil(hi - 0.5) + 1
                            });
                            assert!(
                                ok,
                                "{a:?} {b:?} x {x} y {y} span {span:?} seg a {:?} ab {:?} r {}",
                                seg.a,
                                seg.ab,
                                seg.radius2.sqrt()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_fast_renderer_draws_what_the_plain_one_does() {
        let thin = scattered(400, 700.0);
        // Thick ones too: their segments overlap, most pixels whole.
        let mut thick = scattered(40, 700.0);
        for s in &mut thick {
            s.points.iter_mut().for_each(|p| p[2] *= 20.0);
        }
        // Small (one band) and large (bands on the pool), off the origin.
        for (strokes, region) in [
            (&thin, [100, 150, 260, 300]),
            (&thin, [-20, -10, 720, 690]),
            (&thick, [100, 150, 260, 300]),
            (&thick, [-20, -10, 720, 690]),
        ] {
            let (fast, plain) = (
                render_region(strokes, region),
                reference_render(strokes, region),
            );
            // The same coverage but for the last bit (the fast one runs its
            // sums in another order): alpha off by one at most, and where
            // it's the same, the colour too. (One step of alpha moves a
            // faint pixel's stored colour by more.)
            for (i, (a, b)) in fast.iter().zip(&plain).enumerate() {
                let (a, b) = (a.to_array(), b.to_array());
                let off = |k: usize| a[k].abs_diff(b[k]);
                assert!(off(3) <= 1, "region {region:?}, pixel {i}: {a:?} {b:?}");
                if a[3] == b[3] {
                    assert!(
                        (0..3).all(|k| off(k) <= 1),
                        "region {region:?}, pixel {i}: {a:?} {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn segments_cross_only_within_both() {
        let v = Vec2::new;
        let x = segment_cross(v(0.0, 0.0), v(10.0, 10.0), v(0.0, 10.0), v(10.0, 0.0));
        assert_eq!(x, Some((0.5, 0.5)));
        // Lines that would cross further on.
        assert_eq!(
            segment_cross(v(0.0, 0.0), v(4.0, 4.0), v(0.0, 10.0), v(10.0, 0.0)),
            None
        );
        // Touching at an end counts; parallel and collinear ones don't cross.
        assert!(segment_cross(v(0.0, 5.0), v(5.0, 5.0), v(5.0, 0.0), v(5.0, 10.0)).is_some());
        assert_eq!(
            segment_cross(v(0.0, 0.0), v(10.0, 0.0), v(0.0, 1.0), v(10.0, 1.0)),
            None
        );
        assert_eq!(
            segment_cross(v(0.0, 0.0), v(10.0, 0.0), v(5.0, 0.0), v(15.0, 0.0)),
            None
        );
    }

    #[test]
    fn erasing_to_crossings_takes_out_only_the_stretch_between_them() {
        let across = line(&[[0.0, 50.0, 2.0], [100.0, 50.0, 2.0]]);
        let down = line(&[[30.0, 0.0, 2.0], [30.0, 100.0, 2.0]]);
        let down2 = line(&[[70.0, 0.0, 2.0], [70.0, 100.0, 2.0]]);
        let others = [&down, &down2];
        let ends = |s: &VectorStroke| {
            let (a, b) = (s.points[0], s.points[s.points.len() - 1]);
            (a[0].round(), b[0].round())
        };
        // Between the two crossings: what's either side stays.
        let pieces = across.cut_to_crossings(Vec2::new(50.0, 50.0), &others);
        assert_eq!(
            pieces.iter().map(ends).collect::<Vec<_>>(),
            [(0.0, 30.0), (70.0, 100.0)]
        );
        // Past the last one: to the line's end.
        let pieces = across.cut_to_crossings(Vec2::new(90.0, 50.0), &others);
        assert_eq!(pieces.iter().map(ends).collect::<Vec<_>>(), [(0.0, 70.0)]);
        // Nothing crossing: the whole line goes.
        assert!(
            across
                .cut_to_crossings(Vec2::new(50.0, 50.0), &[])
                .is_empty()
        );
        // A line ending on another (a T) counts as crossing it.
        let stem = line(&[[50.0, 50.0, 2.0], [50.0, 100.0, 2.0]]);
        let pieces = across.cut_to_crossings(Vec2::new(20.0, 50.0), &[&stem]);
        assert_eq!(pieces.iter().map(ends).collect::<Vec<_>>(), [(50.0, 100.0)]);
    }

    #[test]
    fn a_loop_is_cut_where_it_crosses_itself() {
        // Right, down, left, then up through the first stretch: a loop with
        // a tail at each end.
        let s = line(&[
            [0.0, 10.0, 2.0],
            [40.0, 10.0, 2.0],
            [40.0, 30.0, 2.0],
            [20.0, 30.0, 2.0],
            [20.0, 0.0, 2.0],
        ]);
        let s = VectorStroke {
            points: s.path(),
            ..s
        };
        // Erasing on the loop's far side keeps both tails.
        let pieces = s.cut_to_crossings(Vec2::new(40.0, 20.0), &[]);
        assert_eq!(pieces.len(), 2);
        let first_end = pieces[0].points.last().unwrap();
        assert!((first_end[0] - 20.0).abs() < 1.5 && (first_end[1] - 10.0).abs() < 1.5);
    }

    #[test]
    fn bending_moves_the_handle_and_fades_to_its_neighbours() {
        let points: Vec<[f32; 3]> = (0..=20).map(|i| [i as f32 * 5.0, 0.0, 2.0]).collect();
        let handles = [0, 10, 20];
        let bent = bend(&points, &handles, 1, Vec2::new(0.0, 10.0), 0.0);
        assert_eq!(bent[10][1], 10.0, "the handle moves all the way");
        assert_eq!((bent[0][1], bent[20][1]), (0.0, 0.0), "the neighbours stay");
        assert!(bent[5][1] > 0.0 && bent[5][1] < 10.0);
        assert!(
            bent[9][1] > bent[5][1] && bent[5][1] > bent[2][1],
            "it fades"
        );
        // Widening, the same way; an end handle bends only its side.
        let wide = bend(&points, &handles, 0, Vec2::ZERO, 4.0);
        assert_eq!(wide[0][2], 6.0);
        assert!(wide[5][2] > 2.0 && wide[5][2] < 6.0);
        assert_eq!(wide[10][2], 2.0);
    }

    #[test]
    fn picking_finds_the_topmost_line_and_removing_a_handle_straightens() {
        let a = line(&[[0.0, 10.0, 2.0], [100.0, 10.0, 2.0]]);
        let b = line(&[[50.0, 0.0, 2.0], [50.0, 100.0, 2.0]]);
        let layer = VectorLayer::from_strokes(vec![a, b]);
        assert_eq!(layer.pick(Vec2::new(50.0, 10.0), 3.0), Some(1));
        assert_eq!(layer.pick(Vec2::new(10.0, 12.0), 3.0), Some(0));
        assert_eq!(layer.pick(Vec2::new(10.0, 50.0), 3.0), None);
        let points: Vec<[f32; 3]> = (0..=10).map(|i| [i as f32, (i % 2) as f32, 2.0]).collect();
        let handles = [0, 5, 10];
        let out = remove_handle(&points, &handles, 1);
        assert_eq!(out, [points[0], points[10]]);
        assert_eq!(remove_handle(&points, &handles, 0), points[5..].to_vec());
        assert_eq!(simplify_indices(&points[..3], 0.1), [0, 1, 2]);
    }

    #[test]
    fn a_line_is_as_wide_as_its_points_say() {
        // Off the pixel grid, so its edges fall inside pixels.
        let s = line(&[[2.0, 10.3, 4.0], [30.0, 10.3, 4.0]]);
        let px = render_region(&[s], [0, 0, 32, 20]);
        let at = |x: usize, y: usize| px[y * 32 + x];
        assert_eq!(at(15, 10), Color32::BLACK, "on the line");
        assert_eq!(at(15, 14), Color32::TRANSPARENT, "past its edge");
        // Across it, the coverage adds up to its width, the edges soft.
        let across: f32 = (0..20).map(|y| at(15, y).a() as f32 / 255.0).sum();
        assert!((across - 4.0).abs() < 0.1, "{across}");
        assert!(
            (0..20).any(|y| (1..255).contains(&at(15, y).a())),
            "soft edges"
        );
    }

    #[test]
    fn a_line_crossing_itself_doesnt_darken_and_opacity_applies() {
        let mut s = line(&[[2.0, 10.0, 6.0], [30.0, 10.0, 6.0], [2.0, 10.0, 6.0]]);
        s.opacity = 0.5;
        let px = render_region(&[s], [0, 0, 32, 20]);
        assert_eq!(px[10 * 32 + 15].a(), 128);
    }

    #[test]
    fn a_region_shows_the_same_pixels_as_the_whole() {
        let strokes = vec![
            line(&[[3.0, 3.0, 5.0], [40.0, 30.0, 9.0], [10.0, 50.0, 2.0]]),
            VectorStroke {
                points: vec![[0.0, 40.0, 12.0], [60.0, 10.0, 3.0]],
                colour: [200, 30, 30],
                opacity: 0.7,
            },
        ];
        let whole = render_region(&strokes, [0, 0, 64, 64]);
        let part = render_region(&strokes, [20, 16, 44, 40]);
        for y in 16..40 {
            for x in 20..44 {
                assert_eq!(part[(y - 16) * 24 + (x - 20)], whole[y * 64 + x], "{x},{y}");
            }
        }
    }

    #[test]
    fn erasing_splits_or_removes_a_line() {
        let s = line(
            &(0..=20)
                .map(|i| [i as f32 * 2.0, 5.0, 2.0])
                .collect::<Vec<_>>(),
        );
        assert!(s.touches(Vec2::new(20.0, 7.0), 1.5));
        assert!(!s.touches(Vec2::new(20.0, 12.0), 1.5));
        let pieces = s.cut(Vec2::new(20.0, 5.0), 3.0);
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].points.last().unwrap()[0] < 17.5);
        assert!(pieces[1].points[0][0] > 22.5);
        assert!(s.cut(Vec2::new(20.0, 5.0), 100.0).is_empty());
        // A line of two far-apart points is cut in the middle too.
        let long = line(&[[0.0, 5.0, 2.0], [100.0, 5.0, 2.0]]);
        let pieces = long.cut(Vec2::new(50.0, 5.0), 4.0);
        assert_eq!(pieces.len(), 2);
        assert!(
            pieces[0].points.last().unwrap()[0] > 44.0,
            "cut near, not at a point"
        );
        // Missed, it's left as it was.
        assert_eq!(long.cut(Vec2::new(50.0, 30.0), 4.0), vec![long.clone()]);
    }

    #[test]
    fn simplifying_keeps_the_shape_in_fewer_points() {
        // A straight run with jitter below the tolerance, then a corner.
        let mut pts: Vec<[f32; 3]> = (0..50)
            .map(|i| [i as f32, if i % 2 == 0 { 0.1 } else { -0.1 }, 3.0])
            .collect();
        pts.extend((1..20).map(|i| [49.0, i as f32, 3.0]));
        let s = simplify(&pts, 0.5);
        assert!(s.len() <= 4, "{}", s.len());
        assert_eq!(s.first(), pts.first());
        assert_eq!(s.last(), pts.last());
        assert!(
            s.iter()
                .any(|p| (p[0] - 49.0).abs() < 1.0 && p[1].abs() < 1.0),
            "the corner"
        );
        // A pressure swell is kept.
        let swell: Vec<[f32; 3]> = (0..30)
            .map(|i| [i as f32, 0.0, if i == 15 { 12.0 } else { 2.0 }])
            .collect();
        assert!(simplify(&swell, 0.5).iter().any(|p| p[2] == 12.0));
    }

    #[test]
    fn layers_round_trip_as_json() {
        let layer = VectorLayer::from_strokes(vec![line(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])]);
        let json = serde_json::to_string(&layer).unwrap();
        assert_eq!(serde_json::from_str::<VectorLayer>(&json).unwrap(), layer);
    }
}
