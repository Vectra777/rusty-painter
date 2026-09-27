//! Warp (the Transform tool's Distort mode): a grid of control points over
//! the box, the picture bent smoothly through them. The grid is a
//! Catmull-Rom surface, so it passes through every point and has no kinks
//! at the grid lines; a straight grid is exactly the identity.
//!
//! Rendering works from a fine mesh of the surface: the same triangles the
//! screen preview draws, so what's shown while dragging is what's applied.

use eframe::egui::{Rect, Vec2};

/// Most control points along a side.
pub const MAX_POINTS: usize = 6;
/// Fewest: just the corners (an even stretch between them).
pub const MIN_POINTS: usize = 2;
/// Mesh cells along each side, for rendering and the preview.
pub const MESH_STEPS: usize = 48;

/// `n`×`n` control points, row by row from the top-left; `points[j * n + i]`
/// is where the source's (i / (n − 1), j / (n − 1)) point goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpGrid {
    pub n: usize,
    pub points: [Vec2; MAX_POINTS * MAX_POINTS],
}

impl WarpGrid {
    /// A straight grid over `rect`: no warp yet.
    pub fn regular(rect: Rect, n: usize) -> Self {
        let n = n.clamp(MIN_POINTS, MAX_POINTS);
        let mut points = [Vec2::ZERO; MAX_POINTS * MAX_POINTS];
        for j in 0..n {
            for i in 0..n {
                let (u, v) = (i as f32 / (n - 1) as f32, j as f32 / (n - 1) as f32);
                points[j * n + i] = Vec2::new(
                    rect.min.x + u * rect.width(),
                    rect.min.y + v * rect.height(),
                );
            }
        }
        Self { n, points }
    }

    /// The `n * n` control points in use.
    pub fn used(&self) -> &[Vec2] {
        &self.points[..self.n * self.n]
    }

    pub fn used_mut(&mut self) -> &mut [Vec2] {
        let len = self.n * self.n;
        &mut self.points[..len]
    }

    #[inline]
    fn at(&self, i: usize, j: usize) -> Vec2 {
        self.points[j * self.n + i]
    }

    /// Where source point (u, v) of the unit square goes.
    pub fn eval(&self, u: f32, v: f32) -> Vec2 {
        let n = self.n;
        let (kv, tv) = segment(v, n);
        // Rows kv − 1 ..= kv + 2, each evaluated along u; the rows past an
        // edge are extrapolated from the two nearest (a straight grid stays
        // straight).
        let row = |j: isize| -> Vec2 {
            let clamped = j.clamp(0, n as isize - 1) as usize;
            let along = |j: usize| {
                let (ku, tu) = segment(u, n);
                let p = |i: isize| {
                    let c = i.clamp(0, n as isize - 1) as usize;
                    let base = self.at(c, j);
                    if i < 0 {
                        base * 2.0 - self.at(1, j)
                    } else if i >= n as isize {
                        base * 2.0 - self.at(n - 2, j)
                    } else {
                        base
                    }
                };
                let k = ku as isize;
                catmull_rom(p(k - 1), p(k), p(k + 1), p(k + 2), tu)
            };
            if j < 0 {
                along(0) * 2.0 - along(1)
            } else if j >= n as isize {
                along(n - 1) * 2.0 - along(n - 2)
            } else {
                along(clamped)
            }
        };
        let k = kv as isize;
        catmull_rom(row(k - 1), row(k), row(k + 1), row(k + 2), tv)
    }

    /// The same warp with `n` points a side (the new points sit on the
    /// current surface, so the picture keeps its shape).
    pub fn resized(&self, n: usize) -> Self {
        let n = n.clamp(MIN_POINTS, MAX_POINTS);
        let mut points = [Vec2::ZERO; MAX_POINTS * MAX_POINTS];
        for j in 0..n {
            for i in 0..n {
                points[j * n + i] = self.eval(i as f32 / (n - 1) as f32, j as f32 / (n - 1) as f32);
            }
        }
        Self { n, points }
    }

    /// The picture mirrored inside the grid (left-right or top-bottom).
    pub fn flipped(&self, horizontal: bool) -> Self {
        let n = self.n;
        self.permuted(|i, j| {
            if horizontal {
                (n - 1 - i, j)
            } else {
                (i, n - 1 - j)
            }
        })
    }

    /// The picture turned a quarter inside the grid.
    pub fn rotated(&self, clockwise: bool) -> Self {
        let n = self.n;
        self.permuted(|i, j| {
            if clockwise {
                (j, n - 1 - i)
            } else {
                (n - 1 - j, i)
            }
        })
    }

    /// New point (i, j) is old point `from(i, j)`.
    fn permuted(&self, from: impl Fn(usize, usize) -> (usize, usize)) -> Self {
        let mut out = *self;
        for j in 0..self.n {
            for i in 0..self.n {
                let (fi, fj) = from(i, j);
                out.points[j * self.n + i] = self.at(fi, fj);
            }
        }
        out
    }
}

/// Which of the `n − 1` segments `t` (0..1) is in, and where in it.
#[inline]
fn segment(t: f32, n: usize) -> (usize, f32) {
    let s = t * (n - 1) as f32;
    let k = (s.floor().max(0.0) as usize).min(n - 2);
    (k, s - k as f32)
}

#[inline]
fn catmull_rom(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let (t2, t3) = (t * t, t * t * t);
    (p1 * 2.0
        + (p2 - p0) * t
        + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * t2
        + (p1 * 3.0 - p0 - p2 * 3.0 + p3) * t3)
        * 0.5
}

/// A warp of the `src` box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    pub src: Rect,
    pub grid: WarpGrid,
}

impl Warp {
    pub fn forward(&self, p: Vec2) -> Vec2 {
        let u = (p.x - self.src.min.x) / self.src.width().max(1e-6);
        let v = (p.y - self.src.min.y) / self.src.height().max(1e-6);
        self.grid.eval(u, v)
    }
}

/// Canvas point → source point of a warp: the fine mesh's triangles,
/// sorted into square bins so each lookup tests only a few.
pub struct MeshInverse {
    /// Destination corners, then the matching source corners.
    tris: Vec<([Vec2; 3], [Vec2; 3])>,
    bins: Vec<Vec<u32>>,
    origin: Vec2,
    cell: f32,
    cols: usize,
    rows: usize,
}

/// Most bins along a side, so a point thrown far away can't make the
/// index huge.
const MAX_BINS: f32 = 512.0;

impl MeshInverse {
    /// The mesh over `area` (source pixel edges) through `forward`.
    pub fn new(area: Rect, forward: impl Fn(Vec2) -> Vec2) -> Option<Self> {
        let steps = MESH_STEPS;
        let mut src = Vec::with_capacity((steps + 1) * (steps + 1));
        let mut dst = Vec::with_capacity(src.capacity());
        for j in 0..=steps {
            for i in 0..=steps {
                let p = Vec2::new(
                    area.min.x + area.width() * i as f32 / steps as f32,
                    area.min.y + area.height() * j as f32 / steps as f32,
                );
                let q = forward(p);
                if !q.is_finite() {
                    return None;
                }
                src.push(p);
                dst.push(q);
            }
        }
        let row = steps + 1;
        let mut tris = Vec::with_capacity(steps * steps * 2);
        for j in 0..steps {
            for i in 0..steps {
                let k = j * row + i;
                for [a, b, c] in [[k, k + 1, k + row + 1], [k, k + row + 1, k + row]] {
                    tris.push(([dst[a], dst[b], dst[c]], [src[a], src[b], src[c]]));
                }
            }
        }
        let (min, max) = dst.iter().fold(
            (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
            |(lo, hi), p| (lo.min(*p), hi.max(*p)),
        );
        let extent = (max - min).max_elem().max(1.0);
        let cell = (extent / MAX_BINS).max(8.0);
        let cols = ((max.x - min.x) / cell).floor() as usize + 1;
        let rows = ((max.y - min.y) / cell).floor() as usize + 1;
        let mut bins = vec![Vec::new(); cols * rows];
        for (t, (d, _)) in tris.iter().enumerate() {
            let lo = d[0].min(d[1]).min(d[2]) - min;
            let hi = d[0].max(d[1]).max(d[2]) - min;
            let (c0, c1) = (
                (lo.x / cell) as usize,
                ((hi.x / cell) as usize).min(cols - 1),
            );
            let (r0, r1) = (
                (lo.y / cell) as usize,
                ((hi.y / cell) as usize).min(rows - 1),
            );
            for r in r0..=r1 {
                for c in c0..=c1 {
                    bins[r * cols + c].push(t as u32);
                }
            }
        }
        Some(Self {
            tris,
            bins,
            origin: min,
            cell,
            cols,
            rows,
        })
    }

    /// The source point that lands at `p`, if any (where the surface folds
    /// over itself, the first layer of it).
    pub fn map(&self, p: Vec2) -> Option<Vec2> {
        let q = (p - self.origin) / self.cell;
        if q.x < 0.0 || q.y < 0.0 {
            return None;
        }
        let (c, r) = (q.x as usize, q.y as usize);
        if c >= self.cols || r >= self.rows {
            return None;
        }
        for &t in &self.bins[r * self.cols + c] {
            let (d, s) = &self.tris[t as usize];
            if let Some([w0, w1, w2]) = barycentric(p, d) {
                return Some(s[0] * w0 + s[1] * w1 + s[2] * w2);
            }
        }
        None
    }
}

/// Weights of `p` in triangle `t`, if it's inside (edges included).
#[inline]
fn barycentric(p: Vec2, t: &[Vec2; 3]) -> Option<[f32; 3]> {
    let (a, b, c) = (t[0], t[1], t[2]);
    let det = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
    if det.abs() < 1e-9 {
        return None;
    }
    let w0 = ((b.y - c.y) * (p.x - c.x) + (c.x - b.x) * (p.y - c.y)) / det;
    let w1 = ((c.y - a.y) * (p.x - c.x) + (a.x - c.x) * (p.y - c.y)) / det;
    let w2 = 1.0 - w0 - w1;
    const EPS: f32 = -1e-4;
    (w0 >= EPS && w1 >= EPS && w2 >= EPS).then_some([w0, w1, w2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    fn rect() -> Rect {
        Rect::from_min_max(pos2(10.0, 20.0), pos2(110.0, 70.0))
    }

    #[test]
    fn a_straight_grid_is_the_identity() {
        for n in MIN_POINTS..=MAX_POINTS {
            let warp = Warp {
                src: rect(),
                grid: WarpGrid::regular(rect(), n),
            };
            for p in [
                pos2(10.0, 20.0),
                pos2(33.3, 41.0),
                pos2(110.0, 70.0),
                pos2(111.0, 71.0),
            ] {
                let p = p.to_vec2();
                assert!((warp.forward(p) - p).length() < 1e-3, "{n}: {p:?}");
            }
        }
    }

    #[test]
    fn the_surface_passes_through_every_point_and_moves_locally() {
        let mut grid = WarpGrid::regular(rect(), 4);
        let inner = 5; // (1, 1): an inner point
        let was = grid.points[inner];
        grid.points[inner] += Vec2::new(8.0, -5.0);
        for j in 0..4 {
            for i in 0..4 {
                let got = grid.eval(i as f32 / 3.0, j as f32 / 3.0);
                assert!((got - grid.points[j * 4 + i]).length() < 1e-3);
            }
        }
        // Far from the moved point, nothing moved.
        let far = grid.eval(1.0, 1.0);
        assert!((far - Vec2::new(110.0, 70.0)).length() < 1e-3);
        // Near it, the picture follows (smoothly, not just at the point).
        let near = grid.eval(0.4, 0.4);
        let straight = Vec2::new(10.0 + 40.0, 20.0 + 20.0);
        assert!((near - straight).x > 2.0 && was != grid.points[inner]);
    }

    #[test]
    fn resizing_keeps_the_shape() {
        let mut grid = WarpGrid::regular(rect(), 4);
        grid.points[6] += Vec2::new(6.0, 9.0);
        let finer = grid.resized(6);
        for (u, v) in [(0.2, 0.3), (0.5, 0.5), (0.8, 0.9)] {
            assert!(
                (finer.eval(u, v) - grid.eval(u, v)).length() < 1.5,
                "{u} {v}"
            );
        }
        assert_eq!(finer.used().len(), 36);
    }

    #[test]
    fn flips_and_quarter_turns_permute_the_points() {
        let grid = WarpGrid::regular(rect(), 3);
        let flipped = grid.flipped(true);
        assert_eq!(flipped.points[0], grid.points[2]);
        let turned = grid.rotated(true);
        // The top-left of the picture goes where the bottom-left was.
        assert_eq!(turned.points[0], grid.points[6]);
        assert_eq!(turned.rotated(false), grid);
        assert_eq!(flipped.flipped(true), grid);
    }

    #[test]
    fn the_mesh_inverse_undoes_the_warp() {
        let mut grid = WarpGrid::regular(rect(), 4);
        grid.points[5] += Vec2::new(10.0, 6.0);
        grid.points[10] += Vec2::new(-7.0, 4.0);
        let warp = Warp { src: rect(), grid };
        let inverse = MeshInverse::new(rect(), |p| warp.forward(p)).unwrap();
        for (x, y) in [(15.0, 25.0), (50.0, 40.0), (90.0, 60.0), (60.0, 30.0)] {
            let p = Vec2::new(x, y);
            let back = inverse.map(warp.forward(p)).unwrap();
            // Within the mesh's straight-line approximation of the curve.
            assert!((back - p).length() < 0.3, "{p:?} -> {back:?}");
        }
        assert!(inverse.map(Vec2::new(-500.0, 0.0)).is_none());
    }
}
