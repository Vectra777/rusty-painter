//! The Pen tool: Bézier paths, as Photoshop's. Click for a corner, drag for
//! a smooth point (Alt while dragging breaks its handles); click the first
//! point to close the shape; Enter or Esc ends the line, Backspace takes
//! back its last point. The document has one work path, any number of
//! lines; it's kept with the project.
//!
//! With no line being drawn, the Pen edits the path as Photoshop's does
//! with its keys held (or the mode chosen, on a touch screen): Ctrl drags a
//! point, a handle or a curve, Alt turns a point from corner to smooth and
//! back, a click on a curve adds a point and on a point deletes it, and on
//! an open line's end carries the line on.
//!
//! The path becomes a selection (Ctrl+Enter), is filled with the brush
//! colour, or stroked with the brush (on a vector layer, as lines). Path
//! edits undo on their own while the Pen is chosen.

use crate::app::PainterApp;
use crate::canvas::path::{Handle, Hit, Knot, KnotRef, Path, Subpath};
use eframe::egui::{self, Color32, Stroke, Vec2};

/// What a press does, chosen in the options (the keys pick one for the
/// press too).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PenMode {
    /// Draw new lines, and add or delete points on the path.
    #[default]
    Draw,
    /// Drag points, handles and curves (Ctrl).
    Edit,
    /// Turn points from corner to smooth and back (Alt).
    Convert,
}

#[derive(Clone, Copy, Debug)]
enum PenDrag {
    /// The newest point's handles, dragged out of it; `broken` once Alt
    /// lets the out handle move alone.
    New {
        knot: KnotRef,
        broken: bool,
    },
    /// The first point's handles, dragged as it closes the line.
    Close {
        knot: KnotRef,
    },
    /// Points moved by the pointer since `from`.
    Anchor {
        knot: KnotRef,
        from: Vec2,
    },
    Handle {
        knot: KnotRef,
        handle: Handle,
    },
    /// A curve's two ends moved together.
    Segment {
        s: usize,
        i: usize,
        from: Vec2,
    },
}

/// The Pen's work path and what it's doing.
#[derive(Default)]
pub struct PenState {
    pub path: Path,
    /// The line being drawn: its subpath (new points go after its last).
    pub drawing: Option<usize>,
    /// The point whose handles show.
    pub selected: Option<KnotRef>,
    pub mode: PenMode,
    drag: Option<PenDrag>,
    /// Where the pointer is, for the line to the next point.
    pub hover: Option<Vec2>,
    /// The path before each edit, for undo while the Pen is chosen.
    undo: Vec<Path>,
    redo: Vec<Path>,
}

/// In screen points.
const HIT: f32 = 8.0;
/// A press moving less than this (screen points) is a click.
const CLICK: f32 = 2.0;

/// The keys held with a press.
#[derive(Clone, Copy, Debug, Default)]
pub struct PenKeys {
    /// Ctrl (Cmd): edit.
    pub command: bool,
    /// Alt: convert, or break the handles being dragged.
    pub alt: bool,
    /// Shift: keep a new segment to 45° steps.
    pub shift: bool,
}

impl PenState {
    /// The path stays; what was being done to it ends (another document's
    /// path takes its place, or this one comes back).
    pub fn end_session(&mut self) {
        self.drawing = None;
        self.selected = None;
        self.drag = None;
        self.undo.clear();
        self.redo.clear();
    }
}

impl PainterApp {
    fn pen_tolerance(&self) -> f32 {
        HIT / self.viewport.zoom.max(0.01)
    }

    /// Keep the path as it is now for undo, before changing it.
    fn pen_checkpoint(&mut self) {
        let pen = &mut self.workspace.pen;
        pen.undo.push(pen.path.clone());
        pen.redo.clear();
        // ponytail: whole-path snapshots; plenty for hand-drawn paths, keep
        // diffs if paths with thousands of points ever show up.
        if pen.undo.len() > 200 {
            pen.undo.remove(0);
        }
    }

    /// A press with the Pen at canvas point `pos`.
    pub(crate) fn pen_press(&mut self, pos: Vec2, keys: PenKeys) {
        let tol = self.pen_tolerance();
        let pen = &self.workspace.pen;
        let shown: Vec<KnotRef> = pen.selected.into_iter().chain(self.pen_last()).collect();
        let hit = pen.path.hit(pos, tol, &shown);
        let mode = if keys.command {
            PenMode::Edit
        } else if keys.alt && pen.drawing.is_none() {
            PenMode::Convert
        } else {
            pen.mode
        };
        match (mode, pen.drawing) {
            (PenMode::Edit, _) => self.pen_edit_press(pos, hit),
            (PenMode::Convert, _) => {
                if let Some(Hit::Anchor(k)) = hit {
                    self.pen_checkpoint();
                    self.workspace.pen.path.convert(k);
                    self.workspace.pen.selected = Some(k);
                } else if let Some(Hit::Handle(k, h)) = hit {
                    self.pen_checkpoint();
                    self.workspace.pen.selected = Some(k);
                    self.workspace.pen.drag = Some(PenDrag::Handle { knot: k, handle: h });
                }
            }
            (PenMode::Draw, Some(s)) => self.pen_draw_press(s, pos, hit, keys),
            (PenMode::Draw, None) => self.pen_idle_press(pos, hit),
        }
    }

    /// The last point of the line being drawn.
    fn pen_last(&self) -> Option<KnotRef> {
        let s = self.workspace.pen.drawing?;
        let n = self.workspace.pen.path.subpaths.get(s)?.knots.len();
        (n > 0).then(|| (s, n - 1))
    }

    fn pen_draw_press(&mut self, s: usize, pos: Vec2, hit: Option<Hit>, keys: PenKeys) {
        let Some(last) = self.pen_last() else {
            self.workspace.pen.drawing = None;
            return;
        };
        self.pen_checkpoint();
        let pen = &mut self.workspace.pen;
        let count = pen.path.subpaths[s].knots.len();
        match hit {
            // The first point: the line closes (a drag shapes its handles).
            Some(Hit::Anchor((hs, 0))) if hs == s && count >= 2 => {
                pen.path.subpaths[s].closed = true;
                pen.drawing = None;
                pen.selected = Some((s, 0));
                pen.drag = Some(PenDrag::Close { knot: (s, 0) });
            }
            // The last point again: its out handle is taken back, and a
            // drag pulls it out alone (a corner into the next curve).
            Some(Hit::Anchor(k)) if k == last => {
                let knot = &mut pen.path.subpaths[s].knots[last.1];
                knot.out = knot.anchor;
                pen.drag = Some(PenDrag::New {
                    knot: last,
                    broken: true,
                });
            }
            _ => {
                let at = if keys.shift {
                    snap45(pen.path.subpaths[s].knots[last.1].anchor, pos)
                } else {
                    pos
                };
                pen.path.subpaths[s].knots.push(Knot::corner(at));
                let knot = (s, count);
                pen.selected = Some(knot);
                pen.drag = Some(PenDrag::New {
                    knot,
                    broken: false,
                });
            }
        }
    }

    /// A press with no line being drawn: on a curve adds a point, on a
    /// point deletes it, on an open line's end carries it on; elsewhere a
    /// new line starts.
    fn pen_idle_press(&mut self, pos: Vec2, hit: Option<Hit>) {
        self.pen_checkpoint();
        let pen = &mut self.workspace.pen;
        match hit {
            Some(Hit::Segment(s, i, t)) => {
                pen.selected = pen.path.split(s, i, t);
            }
            Some(Hit::Anchor((s, k))) => {
                let sub = &mut pen.path.subpaths[s];
                let n = sub.knots.len();
                if !sub.closed && (k == 0 || k == n - 1) {
                    // Carried on from its end (the first point: reversed).
                    if k == 0 {
                        sub.knots.reverse();
                        for knot in &mut sub.knots {
                            std::mem::swap(&mut knot.in_, &mut knot.out);
                        }
                    }
                    pen.drawing = Some(s);
                    pen.selected = Some((s, n - 1));
                } else {
                    pen.path.delete((s, k));
                    pen.selected = None;
                }
            }
            Some(Hit::Handle(k, h)) => {
                pen.selected = Some(k);
                pen.drag = Some(PenDrag::Handle { knot: k, handle: h });
            }
            None => {
                pen.path.subpaths.push(Subpath {
                    knots: vec![Knot::corner(pos)],
                    closed: false,
                });
                let s = pen.path.subpaths.len() - 1;
                pen.drawing = Some(s);
                pen.selected = Some((s, 0));
                pen.drag = Some(PenDrag::New {
                    knot: (s, 0),
                    broken: false,
                });
            }
        }
    }

    fn pen_edit_press(&mut self, pos: Vec2, hit: Option<Hit>) {
        let drag = match hit {
            Some(Hit::Anchor(k)) => {
                self.workspace.pen.selected = Some(k);
                PenDrag::Anchor { knot: k, from: pos }
            }
            Some(Hit::Handle(k, h)) => {
                self.workspace.pen.selected = Some(k);
                PenDrag::Handle { knot: k, handle: h }
            }
            Some(Hit::Segment(s, i, _)) => PenDrag::Segment { s, i, from: pos },
            None => {
                self.workspace.pen.selected = None;
                return;
            }
        };
        self.pen_checkpoint();
        self.workspace.pen.drag = Some(drag);
    }

    /// A drag with the Pen; `keys.alt` breaks the handles of the point
    /// being pulled out, or moves a handle alone.
    pub(crate) fn pen_drag(&mut self, pos: Vec2, keys: PenKeys) {
        let pen = &mut self.workspace.pen;
        pen.hover = Some(pos);
        let Some(drag) = pen.drag else {
            return;
        };
        match drag {
            PenDrag::New { knot, broken } => {
                let broken = broken || keys.alt;
                pen.drag = Some(PenDrag::New { knot, broken });
                if let Some(k) = pen.path.knot_mut(knot) {
                    if broken {
                        k.out = pos;
                    } else {
                        *k = Knot::smooth(k.anchor, pos);
                    }
                }
            }
            PenDrag::Close { knot } => {
                // Its in handle (the closing curve's) mirrors the pointer;
                // its out handle stays, unless Alt breaks them.
                if let Some(k) = pen.path.knot_mut(knot) {
                    k.in_ = k.anchor * 2.0 - pos;
                    if !keys.alt && k.out == k.anchor {
                        k.out = pos;
                    }
                }
            }
            PenDrag::Anchor { knot, from } => {
                if let Some(k) = pen.path.knot_mut(knot) {
                    *k = k.moved(pos - from);
                }
                pen.drag = Some(PenDrag::Anchor { knot, from: pos });
            }
            PenDrag::Handle { knot, handle } => {
                pen.path.move_handle(knot, handle, pos, keys.alt);
            }
            PenDrag::Segment { s, i, from } => {
                let d = pos - from;
                if let Some(sub) = pen.path.subpaths.get_mut(s) {
                    let n = sub.knots.len();
                    sub.knots[i] = sub.knots[i].moved(d);
                    let j = (i + 1) % n;
                    if j != i {
                        sub.knots[j] = sub.knots[j].moved(d);
                    }
                }
                pen.drag = Some(PenDrag::Segment { s, i, from: pos });
            }
        }
    }

    /// The pointer moved with no button down: the line to the next point
    /// follows it.
    pub(crate) fn pen_hover(&mut self, pos: Vec2) {
        self.workspace.pen.hover = Some(pos);
    }

    pub(crate) fn pen_release(&mut self) {
        let zoom = self.viewport.zoom.max(0.01);
        let pen = &mut self.workspace.pen;
        // A click (no drag) on a new point leaves it a corner.
        if let Some(PenDrag::New { knot, .. }) = pen.drag
            && let Some(k) = pen.path.knot_mut(knot)
            && (k.out - k.anchor).length() * zoom < CLICK
        {
            k.out = k.anchor;
            if !k.is_corner() && (k.in_ - k.anchor).length() * zoom < CLICK * 2.0 {
                k.in_ = k.anchor;
            }
        }
        pen.drag = None;
        // An edit that changed nothing isn't an undo step.
        if pen.undo.last() == Some(&pen.path) {
            pen.undo.pop();
        }
    }

    /// Enter or Esc: the line being drawn ends (open). Returns whether one
    /// was.
    pub(crate) fn pen_finish(&mut self) -> bool {
        let pen = &mut self.workspace.pen;
        let Some(s) = pen.drawing.take() else {
            return false;
        };
        // A lone point isn't a line.
        if pen
            .path
            .subpaths
            .get(s)
            .is_some_and(|sub| sub.knots.len() < 2)
        {
            pen.path.subpaths.remove(s);
            pen.selected = None;
        }
        true
    }

    /// Backspace while drawing: the line's last point goes.
    pub(crate) fn pen_undo_point(&mut self) -> bool {
        let Some(last) = self.pen_last() else {
            return false;
        };
        self.pen_checkpoint();
        let pen = &mut self.workspace.pen;
        pen.path.subpaths[last.0].knots.pop();
        if pen.path.subpaths[last.0].knots.is_empty() {
            pen.path.subpaths.remove(last.0);
            pen.drawing = None;
        }
        let last = self.pen_last();
        self.workspace.pen.selected = last;
        true
    }

    /// Undo (or redo) a path edit, while the Pen is chosen; `false` when
    /// there's none, and the document's history goes on instead.
    pub(crate) fn pen_step(&mut self, redo: bool) -> bool {
        if self.active_tool != crate::app::tools::Tool::Pen {
            return false;
        }
        let pen = &mut self.workspace.pen;
        let (from, to) = if redo {
            (&mut pen.redo, &mut pen.undo)
        } else {
            (&mut pen.undo, &mut pen.redo)
        };
        let Some(path) = from.pop() else {
            return false;
        };
        to.push(std::mem::replace(&mut pen.path, path));
        pen.drag = None;
        // Still drawing the line, if it's still there (and open).
        pen.drawing = pen
            .drawing
            .filter(|&s| pen.path.subpaths.get(s).is_some_and(|sub| !sub.closed));
        pen.selected = None;
        true
    }

    /// The whole path goes (an undo step of the Pen's).
    pub(crate) fn pen_clear(&mut self) {
        if self.workspace.pen.path.is_empty() {
            return;
        }
        self.pen_checkpoint();
        let pen = &mut self.workspace.pen;
        pen.path = Path::default();
        pen.drawing = None;
        pen.selected = None;
    }

    /// The path's closed shapes (open lines closed straight), as polygons.
    fn pen_polygons(&self) -> Vec<Vec<Vec2>> {
        self.workspace.pen.path.flatten(1.0)
    }

    /// The path as the selection, by `mode` (replace, add, subtract or
    /// intersect); one undo step.
    pub(crate) fn pen_to_selection(&mut self, mode: crate::selection::SelectionMode) {
        use crate::selection::SelectionMode;
        self.pen_finish();
        let polygons: Vec<_> = self
            .pen_polygons()
            .into_iter()
            .filter(|p| p.len() > 2)
            .collect();
        if polygons.is_empty() {
            return;
        }
        self.change_selection(|sm| {
            // Every shape of the path together, then combined by `mode`.
            for (i, polygon) in polygons.into_iter().enumerate() {
                let shape = crate::selection::new_lasso_shape(polygon);
                let m = match (i, mode) {
                    (0, m) => m,
                    (_, SelectionMode::Subtract) => SelectionMode::Subtract,
                    (_, SelectionMode::Intersect) => SelectionMode::Add,
                    _ => SelectionMode::Add,
                };
                sm.apply_shape(shape, m);
            }
        });
    }

    /// The path's inside filled with the brush colour, on the selected
    /// layer; one undo step.
    pub(crate) fn pen_fill(&mut self) {
        self.pen_finish();
        let layer = self.canvas.active_layer_idx;
        if !self.pen_paintable(layer) {
            return;
        }
        // On a vector layer the path is kept, as a filled shape.
        if self.is_vector_layer(layer) {
            let path = self.workspace.pen.path.clone();
            self.add_vector_shape(layer, path, true, false);
            return;
        }
        let fills: Vec<_> = self
            .pen_polygons()
            .into_iter()
            .filter(|p| p.len() > 2)
            .collect();
        self.paint_outlines(layer, &fills, &[]);
    }

    /// The path drawn with the brush on the selected layer (on a vector
    /// layer, kept as an outlined shape); one undo step.
    pub(crate) fn pen_stroke(&mut self) {
        self.pen_finish();
        let layer = self.canvas.active_layer_idx;
        let lines = self.workspace.pen.path.flatten(2.0);
        if lines.is_empty() {
            return;
        }
        if self.is_vector_layer(layer) {
            let path = self.workspace.pen.path.clone();
            self.add_vector_shape(layer, path, false, true);
            return;
        }
        if !self.pen_paintable(layer) {
            return;
        }
        self.paint_outlines(layer, &[], &lines);
    }

    fn pen_paintable(&self, layer: usize) -> bool {
        self.canvas
            .layers
            .get(layer)
            .is_some_and(|l| !l.locked && l.kind != crate::canvas::storage::LayerKind::Group)
    }
}

/// `to`, turned to the nearest 45° from `from`.
fn snap45(from: Vec2, to: Vec2) -> Vec2 {
    let d = to - from;
    let step = std::f32::consts::FRAC_PI_4;
    let a = (d.y.atan2(d.x) / step).round() * step;
    from + Vec2::new(a.cos(), a.sin()) * d.length()
}

/// Draw the work path: its curves, its points (the selected one filled),
/// the selected point's handles, and the line to where the next point
/// goes.
pub(crate) fn draw_pen(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> egui::Pos2,
) {
    if app.active_tool != crate::app::tools::Tool::Pen {
        return;
    }
    let pen = &app.workspace.pen;
    let outline = |points: Vec<egui::Pos2>, closed: bool| {
        for (w, c) in [
            (3.0_f32, Color32::from_black_alpha(160)),
            (1.0_f32, crate::ui::style::accent()),
        ] {
            let stroke = Stroke::new(w, c);
            painter.add(if closed {
                egui::Shape::closed_line(points.clone(), stroke)
            } else {
                egui::Shape::line(points.clone(), stroke)
            });
        }
    };
    // Flattened finely enough for the screen at any zoom.
    let step = 2.0 / app.viewport.zoom.max(0.01);
    for sub in &pen.path.subpaths {
        let points: Vec<egui::Pos2> = sub.flatten(step).into_iter().map(to_screen).collect();
        if points.len() > 1 {
            outline(points, false);
        }
    }
    // The line on to the pointer.
    if let (Some(s), Some(hover)) = (pen.drawing, pen.hover)
        && pen.drag.is_none()
        && let Some(last) = pen.path.subpaths.get(s).and_then(|sub| sub.knots.last())
    {
        let seg = [last.anchor, last.out, hover, hover];
        let points: Vec<egui::Pos2> = (0..=24)
            .map(|i| to_screen(crate::canvas::path::bezier(seg, i as f32 / 24.0)))
            .collect();
        painter.add(egui::Shape::dashed_line(
            &points,
            Stroke::new(1.0_f32, crate::ui::style::accent()),
            4.0,
            3.0,
        ));
    }
    // Handles of the selected point and of the line's last.
    let last = pen
        .drawing
        .and_then(|s| Some((s, pen.path.subpaths.get(s)?.knots.len().checked_sub(1)?)));
    for r in pen.selected.into_iter().chain(last) {
        let Some(k) = pen.path.knot(r) else { continue };
        let a = to_screen(k.anchor);
        for h in [k.in_, k.out] {
            if h != k.anchor {
                let p = to_screen(h);
                painter.line_segment([a, p], Stroke::new(1.0_f32, crate::ui::style::accent()));
                painter.circle_filled(p, 3.5, crate::ui::style::accent());
                painter.circle_stroke(p, 3.5, Stroke::new(1.0_f32, Color32::BLACK));
            }
        }
    }
    for (s, sub) in pen.path.subpaths.iter().enumerate() {
        for (k, knot) in sub.knots.iter().enumerate() {
            let r = egui::Rect::from_center_size(to_screen(knot.anchor), egui::vec2(7.0, 7.0));
            let selected = pen.selected == Some((s, k));
            painter.rect_filled(
                r,
                0.0,
                if selected {
                    crate::ui::style::accent()
                } else {
                    Color32::WHITE
                },
            );
            painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::BLACK));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.active_tool = crate::app::tools::Tool::Pen;
        app.brush_state.brush.brush_options.color = Color32::BLACK;
        app
    }

    fn click(app: &mut crate::PainterApp, x: f32, y: f32) {
        app.pen_press(Vec2::new(x, y), PenKeys::default());
        app.pen_release();
    }

    fn drag(app: &mut crate::PainterApp, from: Vec2, to: Vec2, keys: PenKeys) {
        app.pen_press(from, keys);
        app.pen_drag(to, keys);
        app.pen_release();
    }

    #[test]
    fn clicks_make_corners_and_drags_smooth_points_and_the_first_closes() {
        let mut app = app();
        click(&mut app, 10.0, 10.0);
        drag(
            &mut app,
            Vec2::new(60.0, 10.0),
            Vec2::new(80.0, 10.0),
            PenKeys::default(),
        );
        click(&mut app, 60.0, 50.0);
        let path = &app.workspace.pen.path;
        let knots = &path.subpaths[0].knots;
        assert!(knots[0].is_corner() && knots[2].is_corner());
        assert_eq!(
            (knots[1].out, knots[1].in_),
            (Vec2::new(80.0, 10.0), Vec2::new(40.0, 10.0))
        );
        // Back on the first point: closed, and no longer drawing.
        click(&mut app, 10.0, 10.0);
        assert!(app.workspace.pen.path.subpaths[0].closed);
        assert!(app.workspace.pen.drawing.is_none());
        // Undo takes the close back, then the points, one by one.
        assert!(app.pen_step(false));
        assert!(!app.workspace.pen.path.subpaths[0].closed);
        app.pen_step(false);
        assert_eq!(app.workspace.pen.path.subpaths[0].knots.len(), 2);
        assert!(app.pen_step(true), "and redo");
        assert_eq!(app.workspace.pen.path.subpaths[0].knots.len(), 3);
    }

    #[test]
    fn alt_breaks_the_handle_being_pulled_out() {
        let mut app = app();
        click(&mut app, 10.0, 10.0);
        let alt = PenKeys {
            alt: true,
            ..Default::default()
        };
        app.pen_press(Vec2::new(60.0, 10.0), PenKeys::default());
        app.pen_drag(Vec2::new(80.0, 10.0), PenKeys::default());
        app.pen_drag(Vec2::new(60.0, 40.0), alt);
        app.pen_release();
        let k = app.workspace.pen.path.subpaths[0].knots[1];
        assert_eq!(k.in_, Vec2::new(40.0, 10.0), "the in handle stayed");
        assert_eq!(k.out, Vec2::new(60.0, 40.0), "the out handle moved alone");
    }

    #[test]
    fn points_are_added_deleted_moved_and_converted() {
        let mut app = app();
        click(&mut app, 10.0, 10.0);
        click(&mut app, 110.0, 10.0);
        assert!(app.pen_finish());
        // On the line: a point is added; on it again: deleted.
        click(&mut app, 60.0, 10.0);
        assert_eq!(app.workspace.pen.path.subpaths[0].knots.len(), 3);
        click(&mut app, 60.0, 10.0);
        assert_eq!(app.workspace.pen.path.subpaths[0].knots.len(), 2);
        // Ctrl drags a point; Alt makes it smooth.
        let ctrl = PenKeys {
            command: true,
            ..Default::default()
        };
        drag(
            &mut app,
            Vec2::new(110.0, 10.0),
            Vec2::new(110.0, 40.0),
            ctrl,
        );
        assert_eq!(
            app.workspace.pen.path.subpaths[0].knots[1].anchor,
            Vec2::new(110.0, 40.0)
        );
        app.pen_press(
            Vec2::new(110.0, 40.0),
            PenKeys {
                alt: true,
                ..Default::default()
            },
        );
        app.pen_release();
        assert!(!app.workspace.pen.path.subpaths[0].knots[1].is_corner());
        // An open line's end carries it on.
        click(&mut app, 10.0, 10.0);
        assert!(app.workspace.pen.drawing.is_some());
        click(&mut app, 10.0, 50.0);
        assert_eq!(app.workspace.pen.path.subpaths[0].knots.len(), 3);
        assert!(app.pen_undo_point());
        assert_eq!(app.workspace.pen.path.subpaths[0].knots.len(), 2);
    }

    #[test]
    fn the_path_is_kept_with_the_project() {
        let mut app = app();
        click(&mut app, 10.0, 10.0);
        drag(
            &mut app,
            Vec2::new(60.0, 10.0),
            Vec2::new(80.0, 30.0),
            PenKeys::default(),
        );
        let path = app.workspace.pen.path.clone();
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        assert_eq!(loaded.work_path, path);
    }

    #[test]
    fn a_path_fills_strokes_and_becomes_the_selection() {
        let mut app = app();
        for (x, y) in [
            (20.0, 10.0),
            (100.0, 10.0),
            (100.0, 50.0),
            (20.0, 50.0),
            (20.0, 10.0),
        ] {
            click(&mut app, x, y);
        }
        assert!(app.workspace.pen.path.subpaths[0].closed);
        let px = |app: &crate::PainterApp, x: i32, y: i32| {
            app.canvas
                .get_layer_tile_data(1, x / 64, y / 64)
                .map_or(Color32::TRANSPARENT, |t| {
                    t[((y % 64) * 64 + x % 64) as usize]
                })
        };
        app.pen_fill();
        assert_eq!(px(&app, 60, 30), Color32::BLACK, "inside filled");
        assert_eq!(px(&app, 5, 5), Color32::TRANSPARENT, "outside not");
        let steps = app.layer_state.history.stacks().0.len();
        app.pen_stroke();
        assert_eq!(
            app.layer_state.history.stacks().0.len(),
            steps + 1,
            "one step"
        );
        app.pen_to_selection(crate::selection::SelectionMode::Replace);
        assert!(app.selection_manager.has_selection());
        assert!(app.selection_manager.contains(Vec2::new(60.0, 30.0)));
        assert!(!app.selection_manager.contains(Vec2::new(5.0, 5.0)));
    }
}
