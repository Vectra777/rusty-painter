//! On-canvas guides with draggable handles: the mirror-painting axes.
//!
//! A press on a handle is the guide's, not the active tool's: it moves the
//! symmetry centre or turns its axes.

use super::PainterApp;
use super::render_helper::ScreenMap;
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
}

#[derive(Default)]
pub struct GuideState {
    drag: Option<Drag>,
    /// Show the mirror axes (and their handles) while mirror painting.
    pub hide_symmetry: bool,
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

    /// Which handle (if any) is under canvas point `pos`.
    fn guide_hit(&self, pos: Vec2) -> Option<Drag> {
        if !self.symmetry_guides_shown() {
            return None;
        }
        let hit = HANDLE_HIT / self.viewport.zoom.max(0.01);
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
}
