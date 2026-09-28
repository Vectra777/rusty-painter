//! QuickShape: draw a line, circle or polygon by hand and hold the pen
//! still at the end; the stroke turns into the clean shape, editable with
//! the Shape tool's handles until it's applied (a press elsewhere, Enter,
//! or another tool), then the brush is back.
//!
//! Ellipses come out upright (the Shape tool's ellipses have no rotation).

use crate::app::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::shape::ShapeKind;
use eframe::egui::Vec2;
use std::time::{Duration, Instant};

/// How long the pen must rest.
const HOLD: Duration = Duration::from_millis(600);
/// How far (screen points) it may drift while resting.
const DRIFT: f32 = 4.0;
/// Shortest stroke (screen points) that can become a shape.
const MIN_LENGTH: f32 = 40.0;

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
        let Some((kind, points)) = fit_shape(&q.path) else {
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
        self.workspace.shapes.start_editing(kind, points);
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

/// The clean shape a hand-drawn stroke was meant to be, if any.
pub fn fit_shape(pts: &[Vec2]) -> Option<(ShapeKind, Vec<Vec2>)> {
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
        return Some((ShapeKind::Line, vec![first, last]));
    }
    // Otherwise only closed shapes: the end near the start.
    if chord > size * 0.25 {
        return None;
    }
    let half = ((max.x - min.x) + (max.y - min.y)) * 0.25;
    // As an upright ellipse in its box: how far points stray from it.
    let c = (min + max) * 0.5;
    let r = ((max - min) * 0.5).max(Vec2::splat(1.0));
    let ellipse_err = pts
        .iter()
        .map(|&p| {
            let q = (p - c) / r;
            (q.length() - 1.0).abs()
        })
        .sum::<f32>()
        / pts.len() as f32;
    // As a polygon: its corners, and how far points stray from its sides.
    let mut corners = simplify(pts, size * 0.06);
    if corners.len() > 2 && (corners[0] - corners[corners.len() - 1]).length() < size * 0.25 {
        corners.pop(); // the end that came back to the start
    }
    let n = corners.len();
    let polygon_err = if (3..=8).contains(&n) {
        pts.iter()
            .map(|&p| {
                (0..n)
                    .map(|i| to_segment(p, corners[i], corners[(i + 1) % n]))
                    .fold(f32::MAX, f32::min)
            })
            .sum::<f32>()
            / pts.len() as f32
            / half.max(1.0)
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
                return Some((ShapeKind::Rectangle, vec![lo, hi]));
            }
        }
        return Some((ShapeKind::Polygon, corners));
    }
    (ellipse_err < 0.12).then(|| (ShapeKind::Ellipse, vec![min, max]))
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
        let (kind, p) = fit_shape(&pts).unwrap();
        assert_eq!(kind, ShapeKind::Line);
        assert_eq!(p[0], pts[0]);
    }

    #[test]
    fn a_rough_circle_becomes_an_ellipse() {
        let circle = (0..=90).map(|i| {
            let a = i as f32 / 90.0 * std::f32::consts::TAU;
            Vec2::new(200.0 + 100.0 * a.cos(), 150.0 + 60.0 * a.sin())
        });
        let (kind, p) = fit_shape(&wobble(circle, 4.0)).unwrap();
        assert_eq!(kind, ShapeKind::Ellipse);
        assert!((p[0] - Vec2::new(100.0, 90.0)).length() < 8.0, "{p:?}");
        assert!((p[1] - Vec2::new(300.0, 210.0)).length() < 8.0, "{p:?}");
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
        let (kind, p) = fit_shape(&wobble(quad.into_iter(), 2.0)).unwrap();
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
        let (kind, p) = fit_shape(&wobble(tri.into_iter(), 2.0)).unwrap();
        assert_eq!(kind, ShapeKind::Polygon);
        assert_eq!(p.len(), 3, "{p:?}");
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
