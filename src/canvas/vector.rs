//! Vector layers: lines kept as their points, so they stay sharp and can be
//! erased whole, thinned or thickened, or recoloured after they're drawn,
//! as in Clip Studio's vector layers. The layer's pixels are always these
//! lines rendered ([`render_region`]); nothing else paints on it.

use eframe::egui::{Color32, Vec2};

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

/// A vector layer's lines, bottom first.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorLayer {
    pub strokes: Vec<VectorStroke>,
}

impl VectorStroke {
    /// `[x0, y0, x1, y1)` in whole pixels, with the line's width and a
    /// pixel of anti-aliasing.
    pub fn bounds(&self) -> [i32; 4] {
        let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for &[x, y, w] in &self.points {
            let r = w * 0.5 + 1.0;
            b[0] = b[0].min((x - r).floor() as i32);
            b[1] = b[1].min((y - r).floor() as i32);
            b[2] = b[2].max((x + r).ceil() as i32 + 1);
            b[3] = b[3].max((y + r).ceil() as i32 + 1);
        }
        b
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
        let path = self.path();
        if path.len() == 1 {
            let [x, y, w] = path[0];
            return (Vec2::new(x, y) - c).length() <= r + w * 0.5;
        }
        path.windows(2).any(|s| {
            let (d, t) = segment_distance(c, s[0], s[1]);
            let w = s[0][2] + (s[1][2] - s[0][2]) * t;
            d <= r + w * 0.5
        })
    }

    /// The line with what a circle at `c` of radius `r` covers taken out:
    /// the pieces left (none if it was all covered), each thinned out again.
    pub fn cut(&self, c: Vec2, r: f32) -> Vec<VectorStroke> {
        if !self.touches(c, r) {
            return vec![self.clone()];
        }
        // The line as drawn, in steps fine enough to cut cleanly.
        let step = (r * 0.25).clamp(0.25, 2.0);
        let path = self.path();
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
            let inside = (Vec2::new(p[0], p[1]) - c).length() <= r + p[2] * 0.25;
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
        let path = self.path();
        if path.len() < 2 {
            return Vec::new();
        }
        // Distance along the line to each point.
        let mut along = Vec::with_capacity(path.len());
        let mut total = 0.0;
        along.push(0.0);
        for w in path.windows(2) {
            total += (xy(w[1]) - xy(w[0])).length();
            along.push(total);
        }
        // Where along it the eraser is: its nearest point.
        let hit = path
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let (d, t) = segment_distance(c, w[0], w[1]);
                (d, along[i] + (along[i + 1] - along[i]) * t)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(0.0, |(_, s)| s);
        let mut before: Option<f32> = None;
        let mut after: Option<f32> = None;
        for s in crossings(&path, &along, others) {
            if s < hit {
                before = Some(before.map_or(s, |b: f32| b.max(s)));
            } else if s > hit {
                after = Some(after.map_or(s, |a: f32| a.min(s)));
            }
        }
        let piece = |from: f32, to: f32| VectorStroke {
            points: simplify(&stretch(&path, &along, from, to), 0.2),
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

/// Grid cell side (pixels) for finding segments that might cross.
const CROSS_CELL: f32 = 32.0;

/// Where (distance along `path`, whose points are `along` that far) the
/// path crosses one of `others` or itself, unsorted, maybe repeated.
fn crossings(path: &[[f32; 3]], along: &[f32], others: &[&VectorStroke]) -> Vec<f32> {
    let cells = |a: [f32; 3], b: [f32; 3]| {
        let cell = |v: f32| (v / CROSS_CELL).floor() as i32;
        let (x0, x1) = (cell(a[0].min(b[0])), cell(a[0].max(b[0])));
        let (y0, y1) = (cell(a[1].min(b[1])), cell(a[1].max(b[1])));
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (x, y)))
    };
    // Every other segment that could cross: the other lines' (near this one
    // only), then this line's own (for where it loops over itself).
    let b = bounds_of(path);
    let mut segments: Vec<([f32; 3], [f32; 3], Option<usize>)> = Vec::new();
    for other in others {
        let ob = other.bounds();
        if ob[0] > b[2] || ob[2] < b[0] || ob[1] > b[3] || ob[3] < b[1] {
            continue;
        }
        let p = other.path();
        segments.extend(p.windows(2).map(|w| (w[0], w[1], None)));
    }
    segments.extend(
        path.windows(2)
            .enumerate()
            .map(|(i, w)| (w[0], w[1], Some(i))),
    );
    let mut grid: rustc_hash::FxHashMap<(i32, i32), Vec<usize>> = Default::default();
    for (n, &(a, b, _)) in segments.iter().enumerate() {
        for key in cells(a, b) {
            grid.entry(key).or_default().push(n);
        }
    }
    let mut found = Vec::new();
    for (i, w) in path.windows(2).enumerate() {
        for key in cells(w[0], w[1]) {
            for &n in grid.get(&key).into_iter().flatten() {
                let (c, d, own) = segments[n];
                // Its own neighbouring segments share a point, not a crossing.
                if own.is_some_and(|j| j.abs_diff(i) <= 1) {
                    continue;
                }
                if let Some((t, _)) = segment_cross(xy(w[0]), xy(w[1]), xy(c), xy(d)) {
                    found.push(along[i] + (along[i + 1] - along[i]) * t);
                }
            }
        }
    }
    found
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

/// `[x0, y0, x1, y1]` around the points (x, y only).
fn bounds_of(points: &[[f32; 3]]) -> [i32; 4] {
    let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for &[x, y, _] in points {
        b[0] = b[0].min(x.floor() as i32);
        b[1] = b[1].min(y.floor() as i32);
        b[2] = b[2].max(x.ceil() as i32);
        b[3] = b[3].max(y.ceil() as i32);
    }
    b
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
    ((a2 + d * t - p).length(), t)
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

/// The topmost of `strokes` within `tolerance` of `p`.
pub fn pick(strokes: &[VectorStroke], p: Vec2, tolerance: f32) -> Option<usize> {
    (0..strokes.len())
        .rev()
        .find(|&i| strokes[i].touches(p, tolerance))
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
        along[k] = along[k - 1] + (xy(points[k]) - xy(points[k - 1])).length();
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

/// `strokes` drawn over transparent, for the canvas pixels
/// `[x0, y0, x1, y1)`: row-major, premultiplied, as layer tiles hold them.
pub fn render_region(strokes: &[VectorStroke], region: [i32; 4]) -> Vec<Color32> {
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
                    let (d, t) = segment_distance(p, a, c);
                    let half = (a[2] + (c[2] - a[2]) * t) * 0.5;
                    // Thinner than a pixel: fainter rather than vanishing.
                    let (half, fade) = if half < 0.5 {
                        (0.5, (half * 2.0).max(0.0))
                    } else {
                        (half, 1.0)
                    };
                    let v = (half + 0.5 - d).clamp(0.0, 1.0) * fade;
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
                    let src = Color32::from_rgba_unmultiplied(r, g, b, (a * 255.0).round() as u8);
                    out[i] = crate::canvas::storage::gamma_over(src, out[i]);
                }
            }
        }
    }
    out
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
        let strokes = [a, b];
        assert_eq!(pick(&strokes, Vec2::new(50.0, 10.0), 3.0), Some(1));
        assert_eq!(pick(&strokes, Vec2::new(10.0, 12.0), 3.0), Some(0));
        assert_eq!(pick(&strokes, Vec2::new(10.0, 50.0), 3.0), None);
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
        let layer = VectorLayer {
            strokes: vec![line(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])],
        };
        let json = serde_json::to_string(&layer).unwrap();
        assert_eq!(serde_json::from_str::<VectorLayer>(&json).unwrap(), layer);
    }
}
