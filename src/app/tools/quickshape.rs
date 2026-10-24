//! QuickShape: draw a line, circle or polygon by hand and hold the pen
//! still at the end; the stroke turns into the clean shape, editable with
//! the Shape tool's handles until it's applied (a press elsewhere, Enter,
//! or another tool), then the brush is back.
//!
//! Ellipses keep the slant they were drawn with: they're fitted to the
//! stroke by least squares, so a lopsided one still finds its centre. One
//! drawn nearly upright comes out upright. An open stroke can become an
//! arc or a polyline (as a curve, editable like one).

use crate::app::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::shape::{ShapeKind, turned};
use eframe::egui::Vec2;
use std::time::{Duration, Instant};

/// How long the pen must rest.
const HOLD: Duration = Duration::from_millis(600);
/// How far (screen points) it may drift while resting.
const DRIFT: f32 = 4.0;
/// Shortest stroke (screen points) that can become a shape.
const MIN_LENGTH: f32 = 40.0;
/// An ellipse slanted less than this comes out upright.
const UPRIGHT: f32 = 4.0 * std::f32::consts::PI / 180.0;
/// Axes closer than this ratio (of their spreads) make a circle, whose
/// slant means nothing: it comes out upright.
const ROUND: f32 = 1.1;

pub struct QuickShapeState {
    pub enabled: bool,
    /// The stroke's points so far (canvas pixels).
    path: Vec<Vec2>,
    /// Where the pen came to rest, and since when.
    rest: Option<(Vec2, Instant)>,
    /// The shape being edited came from a stroke: back to the brush after.
    pub from_stroke: bool,
}

impl Default for QuickShapeState {
    fn default() -> Self {
        Self {
            enabled: true,
            path: Vec::new(),
            rest: None,
            from_stroke: false,
        }
    }
}

impl QuickShapeState {
    pub(crate) fn begin(&mut self, pos: Vec2) {
        self.path.clear();
        self.path.push(pos);
        self.rest = Some((pos, Instant::now()));
    }

    /// No stroke to watch (it ended, or isn't one QuickShape follows):
    /// nothing left over for the next stroke to be judged by.
    pub(crate) fn reset(&mut self) {
        self.path.clear();
        self.rest = None;
    }

    pub(crate) fn sample(&mut self, pos: Vec2, zoom: f32) {
        if self.path.is_empty() {
            return;
        }
        self.path.push(pos);
        let moved = self
            .rest
            .is_none_or(|(at, _)| (pos - at).length() * zoom > DRIFT);
        if moved {
            self.rest = Some((pos, Instant::now()));
        }
    }
}

impl PainterApp {
    /// While a brush stroke is held still: turn it into a shape.
    pub(crate) fn quickshape_tick(&mut self) {
        let q = &self.workspace.quickshape;
        if !q.enabled
            || !self.brush_state.is_drawing
            || !matches!(self.active_tool, Tool::Brush)
            // A ruler or assistant already shapes the stroke.
            || self.workspace.guides.ruler.enabled
            || self.stroke_on_curve()
        {
            return;
        }
        let Some((_, since)) = q.rest else {
            return;
        };
        if since.elapsed() < HOLD {
            return;
        }
        let zoom = self.viewport.zoom.max(0.01);
        let length: f32 = q.path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
        if length * zoom < MIN_LENGTH {
            return;
        }
        let Some((kind, points, angle)) = fit_shape(&q.path) else {
            // Not a shape: stop checking until the pen moves on.
            self.workspace.quickshape.rest = None;
            return;
        };
        // The freehand stroke goes; the shape takes its place.
        self.discard_current_action();
        self.workspace.quickshape.path.clear();
        self.workspace.quickshape.rest = None;
        self.workspace.quickshape.from_stroke = true;
        self.active_tool = Tool::Shape(kind);
        self.workspace.shapes.start_editing(kind, points, angle);
    }

    /// The shape from a stroke was applied or dropped: back to the brush.
    pub(crate) fn quickshape_done(&mut self) {
        if std::mem::take(&mut self.workspace.quickshape.from_stroke)
            && matches!(self.active_tool, Tool::Shape(_))
        {
            self.active_tool = Tool::Brush;
        }
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-9)).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Ramer–Douglas–Peucker: the points of `pts` that keep it within `eps`.
fn simplify(pts: &[Vec2], eps: f32) -> Vec<Vec2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (i, d) = pts[1..pts.len() - 1]
        .iter()
        .enumerate()
        .map(|(i, &p)| (i + 1, to_segment(p, a, b)))
        .fold((0, 0.0), |m, x| if x.1 > m.1 { x } else { m });
    if d <= eps {
        return vec![a, b];
    }
    let mut left = simplify(&pts[..=i], eps);
    left.pop();
    left.extend(simplify(&pts[i..], eps));
    left
}

fn bounds(pts: &[Vec2]) -> (Vec2, Vec2) {
    pts.iter()
        .fold((pts[0], pts[0]), |(lo, hi), &p| (lo.min(p), hi.max(p)))
}

/// An ellipse: its centre, its half-axes (along `angle` and across), and
/// `angle` (radians, clockwise on screen).
#[derive(Clone, Copy, Debug)]
struct Ellipse {
    c: Vec2,
    r: Vec2,
    angle: f32,
}

impl Ellipse {
    /// The point at parameter `t` (radians around it).
    fn at(&self, t: f32) -> Vec2 {
        self.c
            + turned(
                Vec2::new(self.r.x * t.cos(), self.r.y * t.sin()),
                self.angle,
            )
    }

    /// The derivative of [`Self::at`].
    fn tangent(&self, t: f32) -> Vec2 {
        turned(
            Vec2::new(-self.r.x * t.sin(), self.r.y * t.cos()),
            self.angle,
        )
    }

    /// `p` in the ellipse's own frame, scaled so the ellipse is the unit
    /// circle.
    fn unit(&self, p: Vec2) -> Vec2 {
        turned(p - self.c, -self.angle) / self.r
    }

    /// How far `pts` stray from it on average, as a fraction of its size.
    fn error(&self, pts: &[Vec2]) -> f32 {
        pts.iter()
            .map(|&p| (self.unit(p).length() - 1.0).abs())
            .sum::<f32>()
            / pts.len() as f32
    }
}

type M3 = [[f64; 3]; 3];

fn mul3(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn inv3(m: &M3) -> Option<M3> {
    let cof = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let det: f64 = (0..3).map(|c| m[0][c] * cof(0, c)).sum();
    if det.abs() < 1e-12 {
        return None;
    }
    // The adjugate is the transposed cofactors.
    Some(std::array::from_fn(|i| {
        std::array::from_fn(|j| cof(j, i) / det)
    }))
}

/// The real roots of x³ + b·x² + c·x + d.
fn cubic_roots(b: f64, c: f64, d: f64) -> Vec<f64> {
    let p = c - b * b / 3.0;
    let q = 2.0 * b * b * b / 27.0 - b * c / 3.0 + d;
    let shift = -b / 3.0;
    if p.abs() < 1e-12 {
        return vec![(-q).cbrt() + shift];
    }
    let disc = q * q / 4.0 + p * p * p / 27.0;
    if disc > 0.0 {
        let s = disc.sqrt();
        return vec![(-q / 2.0 + s).cbrt() + (-q / 2.0 - s).cbrt() + shift];
    }
    let m = 2.0 * (-p / 3.0).sqrt();
    let phi = ((3.0 * q / (p * m)).clamp(-1.0, 1.0)).acos() / 3.0;
    (0..3)
        .map(|k| m * (phi - std::f64::consts::TAU * k as f64 / 3.0).cos() + shift)
        .collect()
}

/// The eigenvectors of `m` for its real eigenvalues (not normalised).
fn eigenvectors3(m: &M3) -> Vec<[f64; 3]> {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let minors = m[0][0] * m[1][1] - m[0][1] * m[1][0] + m[0][0] * m[2][2] - m[0][2] * m[2][0]
        + m[1][1] * m[2][2]
        - m[1][2] * m[2][1];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    cubic_roots(-tr, minors, -det)
        .into_iter()
        .filter_map(|l| {
            let r: [[f64; 3]; 3] = std::array::from_fn(|i| {
                std::array::from_fn(|j| m[i][j] - if i == j { l } else { 0.0 })
            });
            let cross = |a: [f64; 3], b: [f64; 3]| {
                [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ]
            };
            // The null space of a rank-2 matrix: the cross product of two
            // of its rows (the pair giving the longest, for precision).
            [cross(r[0], r[1]), cross(r[0], r[2]), cross(r[1], r[2])]
                .into_iter()
                .max_by(|a, b| {
                    let n = |v: &[f64; 3]| v.iter().map(|x| x * x).sum::<f64>();
                    n(a).total_cmp(&n(b))
                })
                .filter(|v| v.iter().any(|x| x.abs() > 1e-15))
        })
        .collect()
}

/// The ellipse nearest `pts` by Fitzgibbon's direct least-squares fit
/// (in Halíř and Flusser's stable form): it always gives an ellipse, and
/// copes with a lopsided or partial one, which a stroke's box or second
/// moments don't. `None` when the points don't pin one down.
fn fit_conic(pts: &[Vec2]) -> Option<Ellipse> {
    if pts.len() < 6 {
        return None;
    }
    // Fitted about the stroke's box, scaled to ±1, for precision.
    let (lo, hi) = bounds(pts);
    let c = (lo + hi) * 0.5;
    let s = ((hi - lo) * 0.5).max(Vec2::splat(1.0));
    let (sx, sy) = (s.x as f64, s.y as f64);
    // The scatter matrix of rows [x², xy, y², x, y, 1].
    let mut scatter = [[0.0f64; 6]; 6];
    for p in pts {
        let (x, y) = ((p.x - c.x) as f64 / sx, (p.y - c.y) as f64 / sy);
        let row = [x * x, x * y, y * y, x, y, 1.0];
        for i in 0..6 {
            for j in 0..6 {
                scatter[i][j] += row[i] * row[j];
            }
        }
    }
    let block = |r: usize, c: usize| -> M3 {
        std::array::from_fn(|i| std::array::from_fn(|j| scatter[r + i][c + j]))
    };
    let (s1, s2, s3) = (block(0, 0), block(0, 3), block(3, 3));
    let s2t: M3 = std::array::from_fn(|i| std::array::from_fn(|j| s2[j][i]));
    // The linear part in terms of the quadratic one: T = -S3⁻¹·S2ᵀ.
    let t = mul3(&inv3(&s3)?, &s2t).map(|row| row.map(|x| -x));
    let reduced = mul3(&s2, &t);
    let m: M3 = std::array::from_fn(|i| std::array::from_fn(|j| s1[i][j] + reduced[i][j]));
    // Times the inverse of the constraint 4ac − b² = 1.
    let m = [
        m[2].map(|x| x * 0.5),
        m[1].map(|x| -x),
        m[0].map(|x| x * 0.5),
    ];
    let q = eigenvectors3(&m)
        .into_iter()
        .find(|v| 4.0 * v[0] * v[2] - v[1] * v[1] > 0.0)?;
    let lin: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| t[i][k] * q[k]).sum());
    // The conic about `c` in canvas pixels.
    let (a, b, cc) = (q[0] / (sx * sx), q[1] / (sx * sy), q[2] / (sy * sy));
    let (d, e, f) = (lin[0] / sx, lin[1] / sy, lin[2]);
    let det = 4.0 * a * cc - b * b;
    if det <= 0.0 {
        return None;
    }
    // Its centre, where the gradient vanishes, and its value there.
    let (u, v) = ((b * e - 2.0 * cc * d) / det, (b * d - 2.0 * a * e) / det);
    let f0 = f + (d * u + e * v) * 0.5;
    // Turned to its axes.
    let angle = 0.5 * b.atan2(a - cc);
    let (sin, cos) = angle.sin_cos();
    let along = a * cos * cos + b * sin * cos + cc * sin * sin;
    let across = a * sin * sin - b * sin * cos + cc * cos * cos;
    let (rx, ry) = ((-f0 / along).sqrt(), (-f0 / across).sqrt());
    if !(rx.is_finite() && ry.is_finite() && rx > 0.0 && ry > 0.0) {
        return None;
    }
    Some(Ellipse {
        c: c + Vec2::new(u as f32, v as f32),
        r: Vec2::new(rx as f32, ry as f32),
        angle: angle as f32,
    })
}

/// A closed ellipse as the Shape tool keeps it: its box before turning
/// and its slant (within ±45°: the box can take either axis across).
/// Nearly upright, or nearly round (its slant means nothing), it comes out
/// upright, as the stroke's own box `min`–`max`.
fn ellipse_shape(e: Ellipse, min: Vec2, max: Vec2) -> (ShapeKind, Vec<Vec2>, f32) {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    let (mut angle, mut r) = (e.angle.rem_euclid(PI), e.r);
    if angle > FRAC_PI_2 {
        angle -= PI;
    }
    if angle > FRAC_PI_4 {
        angle -= FRAC_PI_2;
        r = Vec2::new(r.y, r.x);
    } else if angle <= -FRAC_PI_4 {
        angle += FRAC_PI_2;
        r = Vec2::new(r.y, r.x);
    }
    let round = r.max_elem() < r.min_elem() * ROUND;
    if round || angle.abs() < UPRIGHT {
        return (ShapeKind::Ellipse, vec![min, max], 0.0);
    }
    (ShapeKind::Ellipse, vec![e.c - r, e.c + r], angle)
}

/// Curve nodes (anchor, handle in, handle out) for the arc of `e` from
/// parameter `from` turning by `span`: cubic Béziers of at most a quarter
/// turn each, which follow an ellipse to well under a pixel.
fn arc_nodes(e: &Ellipse, from: f32, span: f32) -> Vec<Vec2> {
    let pieces = (span.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = span / pieces as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let mut nodes = Vec::with_capacity((pieces + 1) * 3);
    for i in 0..=pieces {
        let t = from + step * i as f32;
        let (p, d) = (e.at(t), e.tangent(t) * k);
        let handle_in = if i == 0 { p } else { p - d };
        let handle_out = if i == pieces { p } else { p + d };
        nodes.extend([p, handle_in, handle_out]);
    }
    nodes
}

/// Mean distance from `pts` to the polyline through `corners` (closed:
/// back to the first), as a fraction of `half`.
fn polyline_error(pts: &[Vec2], corners: &[Vec2], closed: bool, half: f32) -> f32 {
    let n = corners.len();
    let sides = if closed { n } else { n - 1 };
    pts.iter()
        .map(|&p| {
            (0..sides)
                .map(|i| to_segment(p, corners[i], corners[(i + 1) % n]))
                .fold(f32::MAX, f32::min)
        })
        .sum::<f32>()
        / pts.len() as f32
        / half.max(1.0)
}

/// The clean shape a hand-drawn stroke was meant to be, if any: its kind,
/// its points (as the Shape tool keeps them) and, for an ellipse, its slant.
pub fn fit_shape(pts: &[Vec2]) -> Option<(ShapeKind, Vec<Vec2>, f32)> {
    if pts.len() < 3 {
        return None;
    }
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    let (min, max) = pts
        .iter()
        .fold((pts[0], pts[0]), |(lo, hi), &p| (lo.min(p), hi.max(p)));
    let size = (max - min).length();
    if size < 4.0 {
        return None;
    }
    // A line: every point close to the chord.
    let chord = (last - first).length();
    let off_line = pts
        .iter()
        .map(|&p| to_segment(p, first, last))
        .fold(0.0, f32::max);
    if chord > size * 0.9 && off_line <= (chord * 0.04).max(2.0) {
        return Some((ShapeKind::Line, vec![first, last], 0.0));
    }
    let ellipse = fit_conic(pts);
    // An open stroke: an arc, or a polyline.
    if chord > size * 0.25 {
        return open_shape(pts, ellipse, size);
    }
    // A closed one, as an ellipse: how far points stray from it.
    let ellipse_err = ellipse.map_or(f32::MAX, |e| e.error(pts));
    // (Polygons are judged against the ellipse's size, so a slanted shape
    // is judged like the same shape upright.)
    let half = ellipse.map_or(size * 0.35, |e| (e.r.x + e.r.y) * 0.5);
    // As a polygon: its corners, and how far points stray from its sides.
    let mut corners = simplify(pts, size * 0.06);
    if corners.len() > 2 && (corners[0] - corners[corners.len() - 1]).length() < size * 0.25 {
        corners.pop(); // the end that came back to the start
    }
    let n = corners.len();
    let polygon_err = if (3..=8).contains(&n) {
        polyline_error(pts, &corners, true, half)
    } else {
        f32::MAX
    };
    if polygon_err < ellipse_err {
        // Four upright corners at right angles: a rectangle.
        if n == 4 {
            let upright = (0..4).all(|i| {
                let d = corners[(i + 1) % 4] - corners[i];
                let angle = d.y.atan2(d.x).abs() % std::f32::consts::FRAC_PI_2;
                let off = angle.min(std::f32::consts::FRAC_PI_2 - angle);
                off < 15f32.to_radians()
            });
            if upright {
                let (lo, hi) = corners
                    .iter()
                    .fold((corners[0], corners[0]), |(lo, hi), &p| {
                        (lo.min(p), hi.max(p))
                    });
                return Some((ShapeKind::Rectangle, vec![lo, hi], 0.0));
            }
        }
        return Some((ShapeKind::Polygon, corners, 0.0));
    }
    match ellipse {
        Some(e) if ellipse_err < 0.12 => Some(ellipse_shape(e, min, max)),
        _ => None,
    }
}

/// An open stroke that isn't a line: an arc of an ellipse, or a polyline
/// of 3 to 8 corners, whichever it follows more closely; both as a curve
/// (a polyline's handles on its corners, so they stay sharp).
fn open_shape(
    pts: &[Vec2],
    ellipse: Option<Ellipse>,
    size: f32,
) -> Option<(ShapeKind, Vec<Vec2>, f32)> {
    // As an arc: how far it turns around the ellipse, followed along the
    // stroke (so it can't jump the seam at ±π).
    let arc = ellipse
        // A huge ellipse is a nearly straight stroke, not an arc.
        .filter(|e| e.r.max_elem() < size * 2.0)
        .map(|e| {
            let turn = |p: Vec2| {
                let q = e.unit(p);
                q.y.atan2(q.x)
            };
            let from = turn(pts[0]);
            let (mut span, mut last) = (0.0f32, from);
            for &p in &pts[1..] {
                let t = turn(p);
                let step = (t - last + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                span += step;
                last = t;
            }
            (e, from, span, e.error(pts))
        })
        .filter(|&(_, _, span, err)| {
            (30f32.to_radians()..330f32.to_radians()).contains(&span.abs()) && err < 0.05
        });
    // As a polyline: its corners, none of its sides tiny.
    let corners = simplify(pts, size * 0.06);
    let polyline = ((3..=8).contains(&corners.len())
        && corners
            .windows(2)
            .all(|w| (w[1] - w[0]).length() > size * 0.1))
    .then(|| polyline_error(pts, &corners, false, size * 0.5))
    .filter(|&err| err < 0.03);
    match (arc, polyline) {
        (Some((e, from, span, arc_err)), poly) if poly.is_none_or(|p| arc_err < p) => {
            Some((ShapeKind::Curve, arc_nodes(&e, from, span), 0.0))
        }
        (_, Some(_)) => {
            let nodes = corners.iter().flat_map(|&c| [c, c, c]).collect();
            Some((ShapeKind::Curve, nodes, 0.0))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shaky hand: points nudged by a repeatable wobble.
    fn wobble(pts: impl Iterator<Item = Vec2>, amount: f32) -> Vec<Vec2> {
        pts.enumerate()
            .map(|(i, p)| {
                let w = ((i as f32 * 1.7).sin() + (i as f32 * 0.37).cos()) * 0.5 * amount;
                p + Vec2::new(w, -w * 0.6)
            })
            .collect()
    }

    fn along(a: Vec2, b: Vec2, n: usize) -> impl Iterator<Item = Vec2> {
        (0..n).map(move |i| a + (b - a) * (i as f32 / n as f32))
    }

    #[test]
    fn a_shaky_line_becomes_a_line() {
        let pts = wobble(
            along(Vec2::new(10.0, 10.0), Vec2::new(300.0, 120.0), 80),
            3.0,
        );
        let (kind, p, _) = fit_shape(&pts).unwrap();
        assert_eq!(kind, ShapeKind::Line);
        assert_eq!(p[0], pts[0]);
    }

    #[test]
    fn a_rough_circle_becomes_an_ellipse() {
        let circle = (0..=90).map(|i| {
            let a = i as f32 / 90.0 * std::f32::consts::TAU;
            Vec2::new(200.0 + 100.0 * a.cos(), 150.0 + 60.0 * a.sin())
        });
        let (kind, p, _) = fit_shape(&wobble(circle, 4.0)).unwrap();
        assert_eq!(kind, ShapeKind::Ellipse);
        assert!((p[0] - Vec2::new(100.0, 90.0)).length() < 8.0, "{p:?}");
        assert!((p[1] - Vec2::new(300.0, 210.0)).length() < 8.0, "{p:?}");
    }

    /// An ellipse of half-axes `a` × `b` about `c`, turned by `angle`.
    fn ellipse(c: Vec2, a: f32, b: f32, angle: f32, n: usize) -> impl Iterator<Item = Vec2> {
        (0..=n).map(move |i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            c + turned(Vec2::new(a * t.cos(), b * t.sin()), angle)
        })
    }

    #[test]
    fn a_slanted_ellipse_keeps_its_slant_and_axes() {
        let c = Vec2::new(200.0, 150.0);
        for degrees in [0.0_f32, 30.0, -25.0, 60.0, 90.0] {
            let angle = degrees.to_radians();
            let pts = wobble(ellipse(c, 120.0, 60.0, angle, 120), 3.0);
            let (kind, p, slant) = fit_shape(&pts).unwrap();
            assert_eq!(kind, ShapeKind::Ellipse, "{degrees}°");
            let center = (p[0] + p[1]) * 0.5;
            let size = (p[1] - p[0]).abs();
            assert!((center - c).length() < 4.0, "{degrees}°: {center:?}");
            // Either axis may lie across: the same ellipse turned a
            // quarter the other way.
            let (along, across) = if size.x >= size.y {
                (slant, size)
            } else {
                (
                    slant + std::f32::consts::FRAC_PI_2,
                    Vec2::new(size.y, size.x),
                )
            };
            let off = (along - angle).rem_euclid(std::f32::consts::PI);
            let off = off.min(std::f32::consts::PI - off);
            if degrees == 90.0 {
                assert_eq!(slant, 0.0, "upright on its end");
            } else {
                assert!(off < 3f32.to_radians(), "{degrees}°: slant {slant}");
            }
            assert!((across.x - 240.0).abs() < 10.0, "{degrees}°: {size:?}");
            assert!((across.y - 120.0).abs() < 10.0, "{degrees}°: {size:?}");
        }
    }

    #[test]
    fn an_upright_ellipse_and_a_circle_stay_upright() {
        let upright = wobble(ellipse(Vec2::new(200.0, 150.0), 100.0, 60.0, 0.0, 90), 4.0);
        let (kind, p, slant) = fit_shape(&upright).unwrap();
        assert_eq!((kind, slant), (ShapeKind::Ellipse, 0.0));
        let (min, max) = upright
            .iter()
            .fold((upright[0], upright[0]), |(lo, hi), &q| {
                (lo.min(q), hi.max(q))
            });
        assert_eq!(p, vec![min, max], "the stroke's box, as before");
        let circle = wobble(ellipse(Vec2::new(200.0, 150.0), 80.0, 80.0, 0.7, 90), 4.0);
        let (kind, _, slant) = fit_shape(&circle).unwrap();
        assert_eq!((kind, slant), (ShapeKind::Ellipse, 0.0));
    }

    #[test]
    fn a_slanted_box_is_still_a_polygon() {
        let (a, b, c, d) = (
            Vec2::new(100.0, 50.0),
            Vec2::new(260.0, 140.0),
            Vec2::new(220.0, 210.0),
            Vec2::new(60.0, 120.0),
        );
        let quad: Vec<Vec2> = along(a, b, 30)
            .chain(along(b, c, 15))
            .chain(along(c, d, 30))
            .chain(along(d, a + Vec2::new(2.0, 2.0), 15))
            .collect();
        let (kind, p, _) = fit_shape(&wobble(quad.into_iter(), 2.0)).unwrap();
        assert_eq!(kind, ShapeKind::Polygon, "{p:?}");
    }

    #[test]
    fn a_rough_box_becomes_a_rectangle_and_a_triangle_a_polygon() {
        let (a, b, c, d) = (
            Vec2::new(50.0, 50.0),
            Vec2::new(250.0, 52.0),
            Vec2::new(248.0, 180.0),
            Vec2::new(52.0, 178.0),
        );
        let quad: Vec<Vec2> = along(a, b, 30)
            .chain(along(b, c, 20))
            .chain(along(c, d, 30))
            .chain(along(d, a + Vec2::new(3.0, 2.0), 20))
            .collect();
        let (kind, p, _) = fit_shape(&wobble(quad.into_iter(), 2.0)).unwrap();
        assert_eq!(kind, ShapeKind::Rectangle, "{p:?}");
        let (t1, t2, t3) = (
            Vec2::new(100.0, 200.0),
            Vec2::new(200.0, 30.0),
            Vec2::new(300.0, 200.0),
        );
        let tri: Vec<Vec2> = along(t1, t2, 30)
            .chain(along(t2, t3, 30))
            .chain(along(t3, t1 + Vec2::new(4.0, 0.0), 30))
            .collect();
        let (kind, p, _) = fit_shape(&wobble(tri.into_iter(), 2.0)).unwrap();
        assert_eq!(kind, ShapeKind::Polygon);
        assert_eq!(p.len(), 3, "{p:?}");
    }

    /// The anchors of curve `nodes`.
    fn anchors(nodes: &[Vec2]) -> Vec<Vec2> {
        nodes.chunks(3).map(|c| c[0]).collect()
    }

    #[test]
    fn an_open_zigzag_becomes_a_polyline() {
        let (a, b, c, d) = (
            Vec2::new(20.0, 200.0),
            Vec2::new(120.0, 40.0),
            Vec2::new(220.0, 200.0),
            Vec2::new(320.0, 40.0),
        );
        let zig: Vec<Vec2> = along(a, b, 30)
            .chain(along(b, c, 30))
            .chain(along(c, d, 30))
            .chain(std::iter::once(d))
            .collect();
        let (kind, nodes, _) = fit_shape(&wobble(zig.into_iter(), 2.0)).unwrap();
        assert_eq!(kind, ShapeKind::Curve);
        let corners = anchors(&nodes);
        assert_eq!(corners.len(), 4, "{corners:?}");
        for (got, want) in corners.iter().zip([a, b, c, d]) {
            assert!((*got - want).length() < 6.0, "{corners:?}");
        }
        // Sharp corners: the handles sit on their anchors.
        assert!(nodes.chunks(3).all(|n| n[1] == n[0] && n[2] == n[0]));
    }

    /// Part of an ellipse, from `from` turning by `span` (radians).
    fn arc(c: Vec2, a: f32, b: f32, from: f32, span: f32, n: usize) -> Vec<Vec2> {
        (0..=n)
            .map(|i| {
                let t = from + span * i as f32 / n as f32;
                c + Vec2::new(a * t.cos(), b * t.sin())
            })
            .collect()
    }

    #[test]
    fn half_and_three_quarter_circles_become_arcs() {
        let c = Vec2::new(200.0, 200.0);
        for span in [std::f32::consts::PI, 1.5 * std::f32::consts::PI] {
            let pts = wobble(arc(c, 100.0, 100.0, 0.3, span, 90).into_iter(), 2.0);
            let (kind, nodes, _) = fit_shape(&pts).unwrap();
            assert_eq!(kind, ShapeKind::Curve, "span {span}");
            let ends = anchors(&nodes);
            assert!((ends[0] - pts[0]).length() < 6.0, "starts where drawn");
            assert!(
                (ends[ends.len() - 1] - pts[pts.len() - 1]).length() < 6.0,
                "ends there"
            );
            // Every anchor and the curve between them lie on the circle.
            let mut tool = crate::app::tools::shape::ShapeToolState::default();
            tool.start_editing(kind, nodes, 0.0);
            for p in tool.session.unwrap().outline(false) {
                assert!(((p - c).length() - 100.0).abs() < 4.0, "{p:?}");
            }
        }
    }

    #[test]
    fn a_lopsided_ellipse_finds_its_centre() {
        // Drawn round past its start, the overlap bulging the box.
        let c = Vec2::new(200.0, 150.0);
        let mut pts = arc(c, 120.0, 70.0, 0.0, std::f32::consts::TAU, 100);
        pts.extend(
            arc(c, 120.0, 70.0, 0.0, 0.6, 12)
                .into_iter()
                .map(|p| p + Vec2::new(0.0, -6.0)),
        );
        let (kind, p, _) = fit_shape(&wobble(pts.into_iter(), 2.0)).unwrap();
        assert_eq!(kind, ShapeKind::Ellipse);
        let fitted = fit_conic(&arc(c, 120.0, 70.0, 0.0, std::f32::consts::TAU, 100)).unwrap();
        assert!((fitted.c - c).length() < 0.5, "{fitted:?}");
        assert!(((p[0] + p[1]) * 0.5 - c).length() < 8.0, "{p:?}");
    }

    #[test]
    fn a_scribble_stays_a_scribble() {
        let s: Vec<Vec2> = (0..100)
            .map(|i| {
                let t = i as f32 / 10.0;
                Vec2::new(t * 30.0, (t * 3.1).sin() * 40.0 + (t * 7.3).cos() * 25.0)
            })
            .collect();
        assert!(fit_shape(&s).is_none());
    }

    /// A mixing brush's stroke (painted by the smudge engine) never turns
    /// into the shape the stroke before it was.
    #[test]
    fn a_mixing_stroke_isnt_judged_by_the_last_strokes_path() {
        use crate::canvas::Canvas;
        use eframe::egui::Color32;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(400, 300, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.viewport.zoom = 1.0;
        // A circle, the pen resting long enough at its end only after it
        // has lifted.
        let circle: Vec<Vec2> = ellipse(Vec2::new(200.0, 150.0), 80.0, 60.0, 0.0, 60).collect();
        app.start_stroke_with_pressure(circle[0], 1.0);
        for &p in &circle[1..] {
            app.add_stroke_point(p, 1.0);
        }
        if let Some((at, _)) = app.workspace.quickshape.rest {
            app.workspace.quickshape.rest = Some((at, Instant::now() - HOLD * 2));
        }
        app.release_canvas();
        // Now a wet brush: the first frame of its stroke.
        app.brush_state.brush.mixing = Some(Default::default());
        app.start_stroke_with_pressure(Vec2::new(30.0, 30.0), 1.0);
        app.quickshape_tick();
        assert!(matches!(app.active_tool, Tool::Brush), "no shape popped up");
        app.release_canvas();
    }

    #[test]
    fn holding_a_stroke_still_turns_it_into_an_editable_shape() {
        use crate::canvas::Canvas;
        use eframe::egui::Color32;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(400, 300, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.viewport.zoom = 1.0;
        let pts: Vec<Vec2> = along(Vec2::new(20.0, 20.0), Vec2::new(350.0, 200.0), 40).collect();
        app.start_stroke_with_pressure(pts[0], 1.0);
        for &p in &pts[1..] {
            app.add_stroke_point(p, 1.0);
        }
        // The pen has rested long enough.
        let rest = app.workspace.quickshape.rest.unwrap().0;
        app.workspace.quickshape.rest = Some((rest, Instant::now() - HOLD * 2));
        app.quickshape_tick();
        assert!(matches!(app.active_tool, Tool::Shape(ShapeKind::Line)));
        assert!(app.workspace.shapes.session.is_some(), "editable");
        app.release_canvas();
        assert_eq!(
            app.layer_state.history.stacks().0.len(),
            0,
            "the freehand stroke went"
        );
        app.shape_commit();
        assert!(matches!(app.active_tool, Tool::Brush), "back to the brush");
        assert_eq!(
            app.layer_state.history.stacks().0.len(),
            1,
            "the line, one step"
        );
    }
}
