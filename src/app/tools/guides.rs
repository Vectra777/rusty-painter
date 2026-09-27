//! On-canvas guides with draggable handles: the mirror-painting axes and the
//! ruler.
//!
//! A press on a handle is the guide's, not the active tool's: it moves the
//! symmetry centre, turns its axes, or moves the ruler.
//!
//! The ruler straightens freehand strokes: a stroke starting near it runs
//! along it, one starting elsewhere runs parallel to it (like sliding a pen
//! along a real ruler).

use crate::app::PainterApp;
use crate::app::view::render::ScreenMap;
use crate::brush_engine::symmetry::SymmetryMode;
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

/// Handle hit radius and drawn size, in screen points.
const HANDLE_HIT: f32 = 14.0;
const HANDLE_RADIUS: f32 = 6.0;
/// Distance of the rotation handle from the centre, in screen points.
const ROTATE_HANDLE: f32 = 72.0;
/// Snaps, in screen points / radians.
const SNAP_DISTANCE: f32 = 10.0;
const ANGLE_STEP: f32 = std::f32::consts::PI / 12.0;

const GUIDE_COLOR: Color32 = Color32::from_rgb(90, 200, 250);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    SymmetryCenter { grab: Vec2 },
    SymmetryAngle,
    RulerEnd(usize),
    RulerMove { last: Vec2 },
}

/// A straight ruler between two points (canvas coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ruler {
    pub a: Vec2,
    pub b: Vec2,
    /// Shown, and straightening strokes.
    pub enabled: bool,
    /// Strokes starting away from the ruler run parallel to it (otherwise
    /// they're left alone).
    pub parallel: bool,
}

impl Default for Ruler {
    fn default() -> Self {
        Self {
            a: Vec2::ZERO,
            b: Vec2::ZERO,
            enabled: false,
            parallel: true,
        }
    }
}

/// Within this distance of the ruler (screen points), a stroke runs along
/// the ruler itself.
const RULER_SNAP: f32 = 28.0;

#[derive(Default)]
pub struct GuideState {
    drag: Option<Drag>,
    /// Show the mirror axes (and their handles) while mirror painting.
    pub hide_symmetry: bool,
    pub ruler: Ruler,
    /// The line the current stroke follows: a point on it and its direction.
    ruler_line: Option<(Vec2, Vec2)>,
}

impl GuideState {
    /// Let go of any handle being dragged.
    pub(crate) fn end_drag(&mut self) {
        self.drag = None;
        self.ruler_line = None;
    }
}

impl PainterApp {
    fn symmetry_guides_shown(&self) -> bool {
        self.workspace.symmetry.is_active() && !self.workspace.guides.hide_symmetry
    }

    fn rotate_handle(&self) -> Vec2 {
        // Along the horizontal axis's direction, whatever the mode.
        let s = &self.workspace.symmetry;
        let dir = Vec2::new(s.angle.cos(), s.angle.sin());
        s.center + dir * (ROTATE_HANDLE / self.viewport.zoom.max(0.01))
    }

    /// Put the symmetry centre in the middle of the canvas.
    pub(crate) fn centre_symmetry(&mut self) {
        self.workspace.symmetry.center =
            Vec2::new(self.canvas.width() as f32, self.canvas.height() as f32) * 0.5;
    }

    /// Keep the symmetry centre on the canvas (a new or smaller canvas).
    pub(crate) fn keep_guides_on_canvas(&mut self) {
        let c = self.workspace.symmetry.center;
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        if c == Vec2::ZERO || c.x < 0.0 || c.y < 0.0 || c.x > w || c.y > h {
            self.centre_symmetry();
        }
    }

    /// Show or hide the ruler, placing it across the middle of the canvas
    /// the first time.
    pub(crate) fn set_ruler(&mut self, enabled: bool) {
        let ruler = &mut self.workspace.guides.ruler;
        ruler.enabled = enabled;
        if enabled && ruler.a == ruler.b {
            let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
            ruler.a = Vec2::new(w * 0.2, h * 0.5);
            ruler.b = Vec2::new(w * 0.8, h * 0.5);
        }
    }

    /// The start of a freehand stroke at `pos`: pick the line it follows
    /// (the ruler, or a parallel one) and put `pos` on it.
    pub(crate) fn ruler_begin_stroke(&mut self, pos: Vec2) -> Vec2 {
        let zoom = self.viewport.zoom.max(0.01);
        let guides = &mut self.workspace.guides;
        guides.ruler_line = None;
        let ruler = guides.ruler;
        let d = ruler.b - ruler.a;
        if !ruler.enabled || d.length() < 1e-3 {
            return pos;
        }
        let dir = d.normalized();
        let off = pos - ruler.a;
        let distance = (off.x * dir.y - off.y * dir.x).abs();
        guides.ruler_line = if distance * zoom <= RULER_SNAP {
            Some((ruler.a, dir))
        } else if ruler.parallel {
            Some((pos, dir))
        } else {
            None
        };
        self.ruler_snap(pos)
    }

    /// `pos` put on the line the current stroke follows, if any.
    pub(crate) fn ruler_snap(&self, pos: Vec2) -> Vec2 {
        match self.workspace.guides.ruler_line {
            Some((origin, dir)) => origin + dir * (pos - origin).dot(dir),
            None => pos,
        }
    }

    /// Which handle (if any) is under canvas point `pos`.
    fn guide_hit(&self, pos: Vec2) -> Option<Drag> {
        let hit = HANDLE_HIT / self.viewport.zoom.max(0.01);
        let ruler = self.workspace.guides.ruler;
        if ruler.enabled {
            for (i, end) in [ruler.a, ruler.b].into_iter().enumerate() {
                if (pos - end).length() <= hit {
                    return Some(Drag::RulerEnd(i));
                }
            }
            // The middle handle moves it (strokes start along its body).
            if (pos - (ruler.a + ruler.b) * 0.5).length() <= hit {
                return Some(Drag::RulerMove { last: pos });
            }
        }
        if !self.symmetry_guides_shown() {
            return None;
        }
        let center = self.workspace.symmetry.center;
        if (pos - center).length() <= hit {
            return Some(Drag::SymmetryCenter { grab: pos - center });
        }
        if (pos - self.rotate_handle()).length() <= hit {
            return Some(Drag::SymmetryAngle);
        }
        None
    }

    /// Whether canvas point `pos` is over a guide handle.
    pub(crate) fn over_guide_handle(&self, pos: Vec2) -> bool {
        self.guide_hit(pos).is_some()
    }

    /// A press at canvas point `pos`: grab a guide handle under it. Returns
    /// whether the press was the guide's.
    pub(crate) fn guides_press(&mut self, pos: Vec2) -> bool {
        self.workspace.guides.drag = self.guide_hit(pos);
        self.workspace.guides.drag.is_some()
    }

    /// Drag the grabbed handle to `pos`; `snap` (Shift) keeps the angle to
    /// 15° steps. Returns whether a handle is being dragged.
    pub(crate) fn guides_drag(&mut self, pos: Vec2, snap: bool) -> bool {
        let Some(drag) = self.workspace.guides.drag else {
            return false;
        };
        let zoom = self.viewport.zoom.max(0.01);
        match drag {
            Drag::SymmetryCenter { grab } => {
                let mut c = pos - grab;
                // Snap to the middle of the canvas.
                let mid = Vec2::new(self.canvas.width() as f32, self.canvas.height() as f32) * 0.5;
                let snap_distance = SNAP_DISTANCE / zoom;
                if (c.x - mid.x).abs() <= snap_distance {
                    c.x = mid.x;
                }
                if (c.y - mid.y).abs() <= snap_distance {
                    c.y = mid.y;
                }
                self.workspace.symmetry.center = c;
            }
            Drag::RulerEnd(i) => {
                let ruler = &mut self.workspace.guides.ruler;
                let other = if i == 0 { ruler.b } else { ruler.a };
                let end = if snap {
                    let d = pos - other;
                    let a = (d.y.atan2(d.x) / ANGLE_STEP).round() * ANGLE_STEP;
                    other + Vec2::new(a.cos(), a.sin()) * d.length()
                } else {
                    pos
                };
                if i == 0 {
                    ruler.a = end;
                } else {
                    ruler.b = end;
                }
            }
            Drag::RulerMove { last } => {
                let ruler = &mut self.workspace.guides.ruler;
                ruler.a += pos - last;
                ruler.b += pos - last;
                self.workspace.guides.drag = Some(Drag::RulerMove { last: pos });
            }
            Drag::SymmetryAngle => {
                let s = &mut self.workspace.symmetry;
                let d = pos - s.center;
                if d.length() > 1.0 {
                    let mut angle = d.y.atan2(d.x);
                    // Without Shift it still settles on upright within a
                    // few degrees.
                    let step = if snap {
                        ANGLE_STEP
                    } else {
                        std::f32::consts::FRAC_PI_2
                    };
                    let snapped = (angle / step).round() * step;
                    if snap || (angle - snapped).abs() < 0.05 {
                        angle = snapped;
                    }
                    s.angle = angle;
                }
            }
        }
        true
    }

    /// Let go of the handle. Returns whether one was being dragged.
    pub(crate) fn guides_release(&mut self) -> bool {
        self.workspace.guides.drag.take().is_some()
    }

    pub(crate) fn guides_dragging(&self) -> bool {
        self.workspace.guides.drag.is_some()
    }
}

/// The part of the line through `p` with direction `dir` (from `t0` to
/// `t1` along it) inside the canvas, if any.
fn clip_to_canvas(p: Vec2, dir: Vec2, t0: f32, t1: f32, w: f32, h: f32) -> Option<(Vec2, Vec2)> {
    let (mut lo, mut hi) = (t0, t1);
    for (pv, dv, max) in [(p.x, dir.x, w), (p.y, dir.y, h)] {
        if dv.abs() < 1e-9 {
            if pv < 0.0 || pv > max {
                return None;
            }
            continue;
        }
        let (a, b) = ((0.0 - pv) / dv, (max - pv) / dv);
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
    }
    (lo < hi).then(|| (p + dir * lo, p + dir * hi))
}

/// Draw the guides over the canvas.
pub(crate) fn draw_guides(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    draw_ruler(app, painter, map);
    if !app.symmetry_guides_shown() {
        return;
    }
    let s = &app.workspace.symmetry;
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let far = w.hypot(h) * 2.0;
    let line = |a: Vec2, b: Vec2| {
        let (a, b) = (map.to_screen(a), map.to_screen(b));
        painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
        painter.line_segment([a, b], Stroke::new(1.0_f32, GUIDE_COLOR));
    };
    let half_lines = s.mode == SymmetryMode::Radial;
    for dir in s.axis_directions() {
        let t0 = if half_lines { 0.0 } else { -far };
        if let Some((a, b)) = clip_to_canvas(s.center, dir, t0, far, w, h) {
            line(a, b);
        }
    }
    let handle = |p: Pos2, filled: bool| {
        painter.circle_filled(p, HANDLE_RADIUS + 1.5, Color32::BLACK);
        if filled {
            painter.circle_filled(p, HANDLE_RADIUS, GUIDE_COLOR);
        } else {
            painter.circle_filled(p, HANDLE_RADIUS, Color32::from_gray(30));
            painter.circle_stroke(p, HANDLE_RADIUS - 1.5, Stroke::new(1.5_f32, GUIDE_COLOR));
        }
    };
    let center = map.to_screen(s.center);
    let rotate = map.to_screen(app.rotate_handle());
    painter.line_segment(
        [center, rotate],
        Stroke::new(1.0_f32, GUIDE_COLOR.gamma_multiply(0.6)),
    );
    handle(center, true);
    handle(rotate, false);
}

fn draw_ruler(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    let ruler = app.workspace.guides.ruler;
    if !ruler.enabled || (ruler.b - ruler.a).length() < 1e-3 {
        return;
    }
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let dir = (ruler.b - ruler.a).normalized();
    // Where strokes are straightened: the whole line, faintly.
    let far = w.hypot(h) * 2.0;
    if let Some((a, b)) = clip_to_canvas(ruler.a, dir, -far, far, w, h) {
        painter.line_segment(
            [map.to_screen(a), map.to_screen(b)],
            Stroke::new(1.0_f32, GUIDE_COLOR.gamma_multiply(0.35)),
        );
    }
    let (a, b) = (map.to_screen(ruler.a), map.to_screen(ruler.b));
    // The ruler's body: a band with tick marks.
    let normal = {
        let d = (b - a).normalized();
        egui::vec2(-d.y, d.x)
    };
    let band = 6.0;
    let quad = vec![
        a + normal * band,
        b + normal * band,
        b - normal * band,
        a - normal * band,
    ];
    painter.add(egui::Shape::convex_polygon(
        quad,
        GUIDE_COLOR.gamma_multiply(0.18),
        Stroke::new(1.0_f32, Color32::from_black_alpha(120)),
    ));
    painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
    painter.line_segment([a, b], Stroke::new(1.0_f32, GUIDE_COLOR));
    let length = (b - a).length();
    let ticks = (length / 16.0).floor() as usize;
    for i in 1..ticks {
        let t = i as f32 / ticks as f32;
        let p = a + (b - a) * t;
        let size = if i % 4 == 0 { band } else { band * 0.5 };
        painter.line_segment([p, p + normal * size], Stroke::new(1.0_f32, GUIDE_COLOR));
    }
    for end in [a, b] {
        painter.circle_filled(end, HANDLE_RADIUS + 1.5, Color32::BLACK);
        painter.circle_filled(end, HANDLE_RADIUS, GUIDE_COLOR);
    }
    // The move handle: a square in the middle.
    let mid = egui::Rect::from_center_size(
        a + (b - a) * 0.5,
        egui::vec2(HANDLE_RADIUS, HANDLE_RADIUS) * 2.0,
    );
    painter.rect_filled(mid.expand(1.5), 0.0, Color32::BLACK);
    painter.rect_filled(mid, 0.0, GUIDE_COLOR);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_clipped_to_the_canvas() {
        let (a, b) = clip_to_canvas(
            Vec2::new(50.0, 50.0),
            Vec2::new(1.0, 0.0),
            -1e4,
            1e4,
            100.0,
            80.0,
        )
        .unwrap();
        assert!((a - Vec2::new(0.0, 50.0)).length() < 1e-3);
        assert!((b - Vec2::new(100.0, 50.0)).length() < 1e-3);
        assert!(
            clip_to_canvas(
                Vec2::new(50.0, 90.0),
                Vec2::new(1.0, 0.0),
                -1e4,
                1e4,
                100.0,
                80.0
            )
            .is_none()
        );
        // A half line from the centre only goes one way.
        let (a, _) = clip_to_canvas(
            Vec2::new(50.0, 40.0),
            Vec2::new(0.0, 1.0),
            0.0,
            1e4,
            100.0,
            80.0,
        )
        .unwrap();
        assert!((a - Vec2::new(50.0, 40.0)).length() < 1e-3);
    }

    #[test]
    fn dragging_the_centre_moves_the_mirror_and_snaps_to_the_middle() {
        use crate::canvas::Canvas;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(200, 100, Color32::WHITE, 64));
        app.workspace.symmetry.mode = SymmetryMode::Vertical;
        app.centre_symmetry();
        assert!(
            app.guides_press(Vec2::new(101.0, 50.0)),
            "grabbed the centre handle"
        );
        app.guides_drag(Vec2::new(141.0, 70.0), false);
        assert!(app.guides_release());
        assert_eq!(app.workspace.symmetry.center, Vec2::new(140.0, 70.0));
        // Back near the middle: it snaps there.
        app.guides_press(Vec2::new(140.0, 70.0));
        app.guides_drag(Vec2::new(103.0, 52.0), false);
        app.guides_release();
        assert_eq!(app.workspace.symmetry.center, Vec2::new(100.0, 50.0));
        // Away from the handles a press is the tool's.
        assert!(!app.guides_press(Vec2::new(20.0, 20.0)));
    }

    #[test]
    fn a_mirrored_stroke_paints_both_sides_as_one_step() {
        use crate::canvas::Canvas;
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.diameter = 6.0;
        app.workspace.symmetry.mode = SymmetryMode::Vertical;
        app.centre_symmetry();
        app.start_stroke_with_pressure(Vec2::new(20.0, 20.0), 1.0);
        app.add_stroke_point(Vec2::new(30.0, 40.0), 1.0);
        app.release_canvas();
        let painted = |app: &crate::PainterApp, x: i32, y: i32| {
            let tile = app
                .canvas
                .get_layer_tile_data(1, x / 64, y / 64)
                .unwrap_or_default();
            tile.get(((y % 64) * 64 + x % 64) as usize)
                .is_some_and(|c| c.a() > 0)
        };
        assert!(painted(&app, 20, 20));
        assert!(painted(&app, 108, 20), "the mirror image across x = 64");
        assert!(painted(&app, 98, 40));
        assert_eq!(
            app.layer_state.histories[1].stacks().0.len(),
            1,
            "one undo step"
        );
        app.apply_history(false);
        assert!(!painted(&app, 20, 20) && !painted(&app, 108, 20));
    }

    #[test]
    fn the_ruler_straightens_strokes() {
        use crate::canvas::Canvas;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(200, 200, Color32::WHITE, 64));
        app.set_ruler(true);
        app.workspace.guides.ruler.a = Vec2::new(20.0, 100.0);
        app.workspace.guides.ruler.b = Vec2::new(180.0, 100.0);
        // Starting next to the ruler: along the ruler itself.
        let start = app.ruler_begin_stroke(Vec2::new(50.0, 110.0));
        assert_eq!(start, Vec2::new(50.0, 100.0));
        assert_eq!(
            app.ruler_snap(Vec2::new(120.0, 90.0)),
            Vec2::new(120.0, 100.0)
        );
        // Starting away from it: parallel to it, through the start.
        app.ruler_begin_stroke(Vec2::new(50.0, 40.0));
        assert_eq!(
            app.ruler_snap(Vec2::new(120.0, 60.0)),
            Vec2::new(120.0, 40.0)
        );
        // Not parallel: left alone.
        app.workspace.guides.ruler.parallel = false;
        app.ruler_begin_stroke(Vec2::new(50.0, 40.0));
        assert_eq!(
            app.ruler_snap(Vec2::new(120.0, 60.0)),
            Vec2::new(120.0, 60.0)
        );
    }
}
