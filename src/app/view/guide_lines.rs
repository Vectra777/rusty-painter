//! Guide lines: horizontal and vertical lines the user places (View →
//! Guides), saved with the project, that points snap to.
//!
//! Ctrl+drag a guide to move it, or from beside the canvas to pull out a
//! new one (upright from the left or right, level from above or below);
//! dragged off the canvas, a guide is removed. They aren't undo steps (like
//! the ruler and the assistants).

use crate::app::PainterApp;
use crate::app::view::render::ScreenMap;
use eframe::egui::{self, Color32, Rect, Stroke, Vec2};

/// Hit distance of a guide, in screen points.
const HIT: f32 = 6.0;
const GUIDE_COLOR: Color32 = Color32::from_rgb(230, 70, 190);

/// A guide: the line `x = pos` (vertical) or `y = pos`, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuideLine {
    pub vertical: bool,
    pub pos: f32,
}

impl GuideLine {
    /// Distance from canvas point `p`.
    fn distance(self, p: Vec2) -> f32 {
        (if self.vertical { p.x } else { p.y } - self.pos).abs()
    }
}

/// The New Guide dialog's fields.
#[derive(Clone, Copy, Debug, Default)]
pub struct NewGuide {
    pub open: bool,
    pub vertical: bool,
    pub pos: f32,
}

#[derive(Debug)]
pub struct GuideLines {
    pub lines: Vec<GuideLine>,
    pub show: bool,
    /// Locked guides can't be moved or removed by dragging.
    pub locked: bool,
    /// The guide being dragged.
    drag: Option<usize>,
    pub new_guide: NewGuide,
}

impl Default for GuideLines {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            show: true,
            locked: false,
            drag: None,
            new_guide: NewGuide::default(),
        }
    }
}

impl GuideLines {
    /// Let go of the guide being dragged (the document is replaced).
    pub(crate) fn end_drag(&mut self) {
        self.drag = None;
    }
}

/// The nearest vertical guide's `x` and horizontal guide's `y` within
/// `threshold` of `p`, if any.
pub(crate) fn snap_to_lines(
    p: Vec2,
    lines: &[GuideLine],
    threshold: f32,
) -> (Option<f32>, Option<f32>) {
    let nearest = |vertical: bool| {
        lines
            .iter()
            .filter(|l| l.vertical == vertical && l.distance(p) <= threshold)
            .min_by(|a, b| a.distance(p).total_cmp(&b.distance(p)))
            .map(|l| l.pos)
    };
    (nearest(true), nearest(false))
}

/// Which way a guide pulled out from canvas point `p` (beside the canvas)
/// runs: upright beside the left or right edge, level above or below.
fn pulled_out_vertical(p: Vec2, width: f32, height: f32) -> Option<bool> {
    let over_x = (-p.x).max(p.x - width).max(0.0);
    let over_y = (-p.y).max(p.y - height).max(0.0);
    if over_x == 0.0 && over_y == 0.0 {
        return None;
    }
    Some(over_x >= over_y)
}

impl PainterApp {
    /// The guide within reach of canvas point `pos`, if any.
    fn guide_line_at(&self, pos: Vec2) -> Option<usize> {
        let reach = HIT / self.viewport.zoom.max(0.01);
        let guides = &self.workspace.view_aids.guides;
        guides
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.distance(pos) <= reach)
            .min_by(|a, b| a.1.distance(pos).total_cmp(&b.1.distance(pos)))
            .map(|(i, _)| i)
    }

    /// Whether Ctrl+press at canvas point `pos` would grab a guide (for
    /// the pointer's shape).
    pub(crate) fn over_guide_line(&self, pos: Vec2) -> Option<bool> {
        let guides = &self.workspace.view_aids.guides;
        if !guides.show || guides.locked {
            return None;
        }
        self.guide_line_at(pos).map(|i| guides.lines[i].vertical)
    }

    /// Add a guide.
    pub(crate) fn add_guide(&mut self, vertical: bool, pos: f32) {
        let guides = &mut self.workspace.view_aids.guides;
        guides.lines.push(GuideLine { vertical, pos });
        guides.show = true;
    }

    pub(crate) fn clear_guides(&mut self) {
        let guides = &mut self.workspace.view_aids.guides;
        guides.lines.clear();
        guides.drag = None;
    }

    /// A press at canvas point `pos`; `command` is Ctrl (Cmd) held. With
    /// it, a guide under the press is grabbed, and a press beside the
    /// canvas pulls out a new one. Returns whether the press was a guide's.
    pub(crate) fn guide_lines_press(&mut self, pos: Vec2, command: bool) -> bool {
        let guides = &self.workspace.view_aids.guides;
        if !command || guides.locked {
            return false;
        }
        if guides.show
            && let Some(i) = self.guide_line_at(pos)
        {
            self.workspace.view_aids.guides.drag = Some(i);
            return true;
        }
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        let Some(vertical) = pulled_out_vertical(pos, w, h) else {
            return false;
        };
        self.add_guide(vertical, if vertical { pos.x } else { pos.y });
        let guides = &mut self.workspace.view_aids.guides;
        guides.drag = Some(guides.lines.len() - 1);
        true
    }

    /// Move the grabbed guide to canvas point `pos` (onto the grid when
    /// snapping to it). Returns whether a guide is being dragged.
    pub(crate) fn guide_lines_drag(&mut self, pos: Vec2) -> bool {
        let Some(i) = self.workspace.view_aids.guides.drag else {
            return false;
        };
        let pos = self.snap_to_grid_only(pos);
        if let Some(line) = self.workspace.view_aids.guides.lines.get_mut(i) {
            line.pos = if line.vertical { pos.x } else { pos.y };
        }
        true
    }

    /// Let go of the grabbed guide: off the canvas, it's removed. Returns
    /// whether one was being dragged.
    pub(crate) fn guide_lines_release(&mut self) -> bool {
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        let guides = &mut self.workspace.view_aids.guides;
        let Some(i) = guides.drag.take() else {
            return false;
        };
        if let Some(line) = guides.lines.get(i) {
            let extent = if line.vertical { w } else { h };
            if line.pos < 0.0 || line.pos > extent {
                guides.lines.remove(i);
            }
        }
        true
    }

    pub(crate) fn guide_lines_dragging(&self) -> bool {
        self.workspace.view_aids.guides.drag.is_some()
    }
}

/// Draw the guides across the canvas panel (`screen`), and the pointer's
/// shape over one Ctrl would grab.
pub(crate) fn draw_guide_lines(
    app: &PainterApp,
    ctx: &egui::Context,
    painter: &egui::Painter,
    map: &ScreenMap,
    screen: Rect,
) {
    let guides = &app.workspace.view_aids.guides;
    if !guides.show || guides.lines.is_empty() {
        return;
    }
    // Long enough to cross the panel whatever the rotation.
    let far = screen.size().length() / map.zoom().max(1e-6);
    let centre = map.to_canvas(screen.center());
    let ppp = ctx.pixels_per_point();
    let crisp = |v: f32| ((v * ppp).floor() + 0.5) / ppp;
    for (i, line) in guides.lines.iter().enumerate() {
        let (a, b) = if line.vertical {
            (
                Vec2::new(line.pos, centre.y - far),
                Vec2::new(line.pos, centre.y + far),
            )
        } else {
            (
                Vec2::new(centre.x - far, line.pos),
                Vec2::new(centre.x + far, line.pos),
            )
        };
        let (mut a, mut b) = (map.to_screen(a), map.to_screen(b));
        if (a.x - b.x).abs() < 0.01 {
            a.x = crisp(a.x);
            b.x = a.x;
        } else if (a.y - b.y).abs() < 0.01 {
            a.y = crisp(a.y);
            b.y = a.y;
        }
        let dragged = guides.drag == Some(i);
        let color = if dragged {
            GUIDE_COLOR
        } else {
            GUIDE_COLOR.gamma_multiply(0.85)
        };
        painter.line_segment([a, b], Stroke::new(1.0_f32, color));
    }
    let command = ctx.input(|i| i.modifiers.command);
    let hovered = app.viewport.cursor_canvas.filter(|_| command);
    let vertical = if app.guide_lines_dragging() {
        guides
            .drag
            .and_then(|i| guides.lines.get(i))
            .map(|l| l.vertical)
    } else {
        hovered.and_then(|p| app.over_guide_line(p))
    };
    if let Some(vertical) = vertical {
        ctx.set_cursor_icon(if vertical {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn app() -> PainterApp {
        crate::project::tests::test_app_pub(Canvas::new(200, 100, Color32::WHITE, 64))
    }

    #[test]
    fn points_snap_to_the_nearest_guide_within_reach() {
        let lines = [
            GuideLine {
                vertical: true,
                pos: 50.0,
            },
            GuideLine {
                vertical: true,
                pos: 56.0,
            },
            GuideLine {
                vertical: false,
                pos: 20.0,
            },
        ];
        assert_eq!(
            snap_to_lines(Vec2::new(54.0, 23.0), &lines, 4.0),
            (Some(56.0), Some(20.0))
        );
        assert_eq!(
            snap_to_lines(Vec2::new(44.0, 26.0), &lines, 4.0),
            (None, None),
            "beyond the threshold"
        );
    }

    #[test]
    fn ctrl_dragging_moves_a_guide_and_off_the_canvas_removes_it() {
        let mut app = app();
        app.add_guide(true, 50.0);
        // Without Ctrl, a press is the tool's.
        assert!(!app.guide_lines_press(Vec2::new(51.0, 40.0), false));
        assert!(app.guide_lines_press(Vec2::new(51.0, 40.0), true));
        assert!(app.guide_lines_drag(Vec2::new(120.0, 10.0)));
        assert!(app.guide_lines_release());
        assert_eq!(app.workspace.view_aids.guides.lines[0].pos, 120.0);
        // Locked: stays put.
        app.workspace.view_aids.guides.locked = true;
        assert!(!app.guide_lines_press(Vec2::new(120.0, 40.0), true));
        app.workspace.view_aids.guides.locked = false;
        // Dragged off the canvas: gone.
        app.guide_lines_press(Vec2::new(120.0, 40.0), true);
        app.guide_lines_drag(Vec2::new(230.0, 40.0));
        app.guide_lines_release();
        assert!(app.workspace.view_aids.guides.lines.is_empty());
        assert!(
            app.layer_state.history.stacks().0.is_empty(),
            "no undo step"
        );
    }

    #[test]
    fn guides_are_saved_with_the_project() {
        let mut app = app();
        app.add_guide(true, 12.5);
        app.add_guide(false, 80.0);
        app.workspace.view_aids.guides.locked = true;
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let mut other = app_with_other_guides();
        loaded.guides.unwrap().apply(&mut other);
        let guides = &other.workspace.view_aids.guides;
        assert_eq!(guides.lines, app.workspace.view_aids.guides.lines);
        assert!(guides.locked);
        // Files from before guide lines: none, unlocked.
        let old: crate::app::tools::guides::StoredGuides =
            serde_json::from_str(r#"{"ruler_enabled": true}"#).unwrap();
        let mut other = app_with_other_guides();
        old.apply(&mut other);
        assert!(other.workspace.view_aids.guides.lines.is_empty());
        assert!(!other.workspace.view_aids.guides.locked);
    }

    fn app_with_other_guides() -> PainterApp {
        let mut app = app();
        app.add_guide(false, 3.0);
        app
    }

    #[test]
    fn a_guide_pulled_out_from_beside_the_canvas_runs_along_that_edge() {
        let mut app = app();
        // Left of the canvas: an upright guide, dragged in.
        assert!(app.guide_lines_press(Vec2::new(-10.0, 50.0), true));
        app.guide_lines_drag(Vec2::new(30.0, 60.0));
        app.guide_lines_release();
        // Above it: a level one.
        app.guide_lines_press(Vec2::new(100.0, -5.0), true);
        app.guide_lines_drag(Vec2::new(90.0, 40.0));
        app.guide_lines_release();
        assert_eq!(
            app.workspace.view_aids.guides.lines,
            [
                GuideLine {
                    vertical: true,
                    pos: 30.0
                },
                GuideLine {
                    vertical: false,
                    pos: 40.0
                },
            ]
        );
        // On the canvas, away from any guide: the tool's.
        assert!(!app.guide_lines_press(Vec2::new(150.0, 80.0), true));
    }
}
