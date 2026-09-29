//! Shape tools: line, rectangle, ellipse and polygon, drawn with the current
//! brush (outline), filled with the brush colour, or both.
//!
//! A shape stays editable (drag its handles, or inside it to move it) until
//! it's applied: Enter, a press outside it, or another tool. Esc cancels.
//! An ellipse also has a round handle above it that turns it.
//! Applying paints it as one undo step, mirrored like any stroke.

use crate::app::PainterApp;
use crate::brush_engine::brush::StabilizerAlgorithm;
use crate::brush_engine::brush_options::BlendMode;
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::LayerKind;
use crate::selection::{SelectionManager, SelectionMask, SelectionMode};
use eframe::egui::{self, Color32, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Line,
    Rectangle,
    Ellipse,
    Polygon,
}

impl ShapeKind {
    pub const ALL: [(ShapeKind, &'static str); 4] = [
        (ShapeKind::Line, "Line"),
        (ShapeKind::Rectangle, "Rectangle"),
        (ShapeKind::Ellipse, "Ellipse"),
        (ShapeKind::Polygon, "Polygon"),
    ];

    /// The next kind, for the shortcut that cycles them.
    pub fn next(self) -> Self {
        match self {
            ShapeKind::Line => ShapeKind::Rectangle,
            ShapeKind::Rectangle => ShapeKind::Ellipse,
            ShapeKind::Ellipse => ShapeKind::Polygon,
            ShapeKind::Polygon => ShapeKind::Line,
        }
    }
}

/// What applying a shape paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeStyle {
    /// Its outline, with the brush.
    Outline,
    /// Its inside, with the brush colour.
    Fill,
    Both,
}

#[derive(Clone, Copy, Debug)]
pub struct ShapeSettings {
    pub style: ShapeStyle,
    /// Polygons: join the last point back to the first.
    pub closed: bool,
}

impl Default for ShapeSettings {
    fn default() -> Self {
        Self {
            style: ShapeStyle::Outline,
            closed: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ShapeDrag {
    /// Drawing it out from `anchor` (where the press was).
    Create { anchor: Vec2 },
    /// Moving point / corner `i`.
    Point(usize),
    /// Moving the whole shape.
    Move { last: Vec2 },
    /// Turning an ellipse: its angle when the drag began, and the
    /// pointer's direction from the centre then.
    Turn { from: f32, grab: f32 },
}

/// A shape being drawn or edited.
pub struct ShapeSession {
    pub kind: ShapeKind,
    /// Line: its ends. Rectangle, ellipse: opposite corners of the box.
    /// Polygon: its points.
    pub points: Vec<Vec2>,
    drag: Option<ShapeDrag>,
    /// A polygon still taking points (press to add one).
    pub building: bool,
    /// The pointer, for the polygon's rubber band.
    cursor: Option<Vec2>,
    /// Ellipse: how far it's turned about its centre (radians, clockwise
    /// on screen). `points` is then its box before turning.
    pub angle: f32,
}

pub struct ShapeToolState {
    pub settings: ShapeSettings,
    pub session: Option<ShapeSession>,
    /// The shape the tool button picks.
    pub last_kind: ShapeKind,
}

impl ShapeToolState {
    /// Start editing a finished shape (QuickShape hands one over), turned
    /// by `angle` if it's an ellipse.
    pub(crate) fn start_editing(&mut self, kind: ShapeKind, points: Vec<Vec2>, angle: f32) {
        self.session = Some(ShapeSession {
            kind,
            points,
            drag: None,
            building: false,
            cursor: None,
            angle: if kind == ShapeKind::Ellipse {
                angle
            } else {
                0.0
            },
        });
    }
}

impl Default for ShapeToolState {
    fn default() -> Self {
        Self {
            settings: ShapeSettings::default(),
            session: None,
            last_kind: ShapeKind::Rectangle,
        }
    }
}

/// Distances in screen points (divided by the zoom).
const HANDLE_HIT: f32 = 12.0;
const HANDLE_SIZE: f32 = 4.5;
const TINY: f32 = 2.0;
/// How far the turning handle sits above an ellipse's box.
const TURN_HANDLE_GAP: f32 = 24.0;

/// Modifier keys that shape a drag.
#[derive(Clone, Copy, Default)]
pub struct ShapeMods {
    /// Keep proportions: square, circle, 15° lines.
    pub constrain: bool,
    /// Draw out from the centre.
    pub from_center: bool,
}

/// `b` moved so that `a → b` is at a multiple of 15°.
fn snap_angle(a: Vec2, b: Vec2) -> Vec2 {
    let d = b - a;
    let step = std::f32::consts::PI / 12.0;
    let angle = (d.y.atan2(d.x) / step).round() * step;
    a + Vec2::new(angle.cos(), angle.sin()) * d.length()
}

/// `b` moved so that the box from `a` is square.
fn square(a: Vec2, b: Vec2) -> Vec2 {
    let d = b - a;
    let side = d.x.abs().max(d.y.abs());
    a + Vec2::new(side.copysign(d.x), side.copysign(d.y))
}

/// `v` turned by `angle` (clockwise on screen).
pub(crate) fn turned(v: Vec2, angle: f32) -> Vec2 {
    let (sin, cos) = angle.sin_cos();
    Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
}

/// `angle` in (-π, π].
fn wrap_angle(angle: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let a = angle.rem_euclid(TAU);
    if a > PI { a - TAU } else { a }
}

/// The box corners of `[p, q]`, clockwise from the top left.
fn corners(p: Vec2, q: Vec2) -> [Vec2; 4] {
    let (min, max) = (p.min(q), p.max(q));
    [min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)]
}

impl ShapeSession {
    /// Whether this is an ellipse turned away from upright.
    fn is_turned(&self) -> bool {
        self.kind == ShapeKind::Ellipse && self.angle != 0.0
    }

    /// The centre of the box (what an ellipse turns about).
    fn box_center(&self) -> Vec2 {
        (self.points[0] + self.points[1]) * 0.5
    }

    /// Canvas point `p` of the unturned box, where it is once turned.
    fn to_canvas(&self, p: Vec2) -> Vec2 {
        if !self.is_turned() {
            return p;
        }
        let c = self.box_center();
        c + turned(p - c, self.angle)
    }

    /// Canvas point `p` in the unturned box's frame.
    fn to_box(&self, p: Vec2) -> Vec2 {
        if !self.is_turned() {
            return p;
        }
        let c = self.box_center();
        c + turned(p - c, -self.angle)
    }

    /// The points handles sit on.
    fn handles(&self) -> Vec<Vec2> {
        match self.kind {
            ShapeKind::Rectangle | ShapeKind::Ellipse => corners(self.points[0], self.points[1])
                .map(|p| self.to_canvas(p))
                .to_vec(),
            ShapeKind::Line | ShapeKind::Polygon => self.points.clone(),
        }
    }

    /// An ellipse's turning handle: above the middle of its top side,
    /// `TURN_HANDLE_GAP` screen points out.
    fn turn_handle(&self, zoom: f32) -> Option<Vec2> {
        if self.kind != ShapeKind::Ellipse || self.building {
            return None;
        }
        let (min, max) = (
            self.points[0].min(self.points[1]),
            self.points[0].max(self.points[1]),
        );
        let top = Vec2::new((min.x + max.x) * 0.5, min.y - TURN_HANDLE_GAP / zoom);
        let c = self.box_center();
        Some(c + turned(top - c, self.angle))
    }

    /// The middle of the top side, where the turning handle's stem starts.
    fn top_middle(&self) -> Vec2 {
        let min_y = self.points[0].y.min(self.points[1].y);
        self.to_canvas(Vec2::new(self.box_center().x, min_y))
    }

    fn bounds(&self) -> (Vec2, Vec2) {
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for p in &self.points {
            min = min.min(*p);
            max = max.max(*p);
        }
        (min, max)
    }

    /// The outline as a polyline (closed shapes end where they start).
    pub fn outline(&self, closed_polygon: bool) -> Vec<Vec2> {
        match self.kind {
            ShapeKind::Line => self.points.clone(),
            ShapeKind::Rectangle => {
                let c = corners(self.points[0], self.points[1]);
                vec![c[0], c[1], c[2], c[3], c[0]]
            }
            ShapeKind::Ellipse => {
                let (min, max) = (
                    self.points[0].min(self.points[1]),
                    self.points[0].max(self.points[1]),
                );
                let (center, r) = ((min + max) * 0.5, (max - min) * 0.5);
                // Segments of about two pixels: the brush's spacing places
                // dabs along them, so the curve stays smooth.
                let perimeter = std::f32::consts::TAU * ((r.x * r.x + r.y * r.y) * 0.5).sqrt();
                let n = ((perimeter / 2.0).ceil() as usize).clamp(24, 4096);
                let turned_by = self.is_turned().then_some(self.angle);
                (0..=n)
                    .map(|i| {
                        let a = std::f32::consts::TAU * i as f32 / n as f32;
                        let v = Vec2::new(a.cos() * r.x, a.sin() * r.y);
                        match turned_by {
                            Some(angle) => center + turned(v, angle),
                            None => center + v,
                        }
                    })
                    .collect()
            }
            ShapeKind::Polygon => {
                let mut pts = self.points.clone();
                if closed_polygon && pts.len() > 2 {
                    pts.push(pts[0]);
                }
                pts
            }
        }
    }

    /// Whether the shape has an inside to fill.
    fn has_area(&self, closed_polygon: bool) -> bool {
        match self.kind {
            ShapeKind::Line => false,
            ShapeKind::Polygon => closed_polygon && self.points.len() > 2,
            ShapeKind::Rectangle | ShapeKind::Ellipse => true,
        }
    }

    fn is_tiny(&self, zoom: f32) -> bool {
        let (min, max) = self.bounds();
        (max - min).length() * zoom < TINY
    }
}

impl PainterApp {
    pub(crate) fn set_shape_tool(&mut self, kind: ShapeKind) {
        if let Some(session) = &self.workspace.shapes.session
            && session.kind != kind
        {
            self.shape_commit();
        }
        self.active_tool = crate::app::tools::Tool::Shape(kind);
        self.workspace.shapes.last_kind = kind;
    }

    fn shape_zoom(&self) -> f32 {
        self.viewport.zoom.max(0.01)
    }

    /// A press with the Shape tool at canvas point `pos`.
    pub(crate) fn shape_press(&mut self, kind: ShapeKind, pos: Vec2) {
        let hit = HANDLE_HIT / self.shape_zoom();
        if let Some(session) = self.workspace.shapes.session.as_mut() {
            if session.building {
                // Back at the first point: the polygon is done.
                if session.points.len() > 2 && (pos - session.points[0]).length() <= hit {
                    session.building = false;
                    return;
                }
                session.points.push(pos);
                session.drag = Some(ShapeDrag::Point(session.points.len() - 1));
                return;
            }
            if let Some(h) = session.turn_handle(self.viewport.zoom.max(0.01))
                && (h - pos).length() <= hit
            {
                let d = pos - session.box_center();
                session.drag = Some(ShapeDrag::Turn {
                    from: session.angle,
                    grab: d.y.atan2(d.x),
                });
                return;
            }
            if let Some(i) = session
                .handles()
                .iter()
                .position(|h| (*h - pos).length() <= hit)
            {
                session.drag = Some(ShapeDrag::Point(i));
                return;
            }
            // (A turned ellipse: in its own frame.)
            let pos = session.to_box(pos);
            let (min, max) = session.bounds();
            let inside = pos.x >= min.x - hit
                && pos.y >= min.y - hit
                && pos.x <= max.x + hit
                && pos.y <= max.y + hit;
            if inside {
                session.drag = Some(ShapeDrag::Move { last: pos });
                return;
            }
            // Pressing elsewhere applies this shape and starts the next
            // (or, for a QuickShape, goes back to the brush).
            let quick = self.workspace.quickshape.from_stroke;
            self.shape_commit();
            if quick {
                return;
            }
        }
        self.workspace.shapes.session = Some(match kind {
            ShapeKind::Polygon => ShapeSession {
                kind,
                points: vec![pos],
                drag: None,
                building: true,
                cursor: Some(pos),
                angle: 0.0,
            },
            _ => ShapeSession {
                kind,
                points: vec![pos, pos],
                drag: Some(ShapeDrag::Create { anchor: pos }),
                building: false,
                cursor: Some(pos),
                angle: 0.0,
            },
        });
    }

    /// Movement with the Shape tool: drags when pressed, the polygon's
    /// rubber band otherwise.
    pub(crate) fn shape_move(&mut self, pos: Vec2, mods: ShapeMods) {
        let Some(session) = self.workspace.shapes.session.as_mut() else {
            return;
        };
        session.cursor = Some(pos);
        match session.drag {
            None => {}
            Some(ShapeDrag::Create { anchor }) => {
                let mut end = pos;
                if mods.constrain {
                    end = match session.kind {
                        ShapeKind::Line => snap_angle(anchor, end),
                        _ => square(anchor, end),
                    };
                }
                session.points = if mods.from_center && session.kind != ShapeKind::Line {
                    vec![anchor - (end - anchor), end]
                } else {
                    vec![anchor, end]
                };
            }
            Some(ShapeDrag::Point(i)) => match session.kind {
                ShapeKind::Ellipse if session.is_turned() => {
                    // The opposite corner stays put; the box is kept square
                    // to the ellipse's own axes.
                    let angle = session.angle;
                    let opposite = session.handles()[(i + 2) % 4];
                    let mut d = turned(pos - opposite, -angle);
                    if mods.constrain {
                        d = square(Vec2::ZERO, d);
                    }
                    let center = opposite + turned(d, angle) * 0.5;
                    let local = |p: Vec2| center + turned(p - center, -angle);
                    session.points = vec![local(opposite), local(opposite + turned(d, angle))];
                }
                ShapeKind::Rectangle | ShapeKind::Ellipse => {
                    let c = corners(session.points[0], session.points[1]);
                    let opposite = c[(i + 2) % 4];
                    let end = if mods.constrain {
                        square(opposite, pos)
                    } else {
                        pos
                    };
                    session.points = vec![opposite, end];
                }
                ShapeKind::Line | ShapeKind::Polygon => {
                    let pos = if mods.constrain && i > 0 {
                        snap_angle(session.points[i - 1], pos)
                    } else if mods.constrain && session.kind == ShapeKind::Line {
                        snap_angle(session.points[1], pos)
                    } else {
                        pos
                    };
                    if let Some(p) = session.points.get_mut(i) {
                        *p = pos;
                    }
                }
            },
            Some(ShapeDrag::Move { last }) => {
                let d = pos - last;
                for p in &mut session.points {
                    *p += d;
                }
                session.drag = Some(ShapeDrag::Move { last: pos });
            }
            Some(ShapeDrag::Turn { from, grab }) => {
                let d = pos - session.box_center();
                let mut angle = from + d.y.atan2(d.x) - grab;
                if mods.constrain {
                    let step = std::f32::consts::PI / 12.0;
                    angle = (angle / step).round() * step;
                }
                session.angle = wrap_angle(angle);
                // Close enough to upright is upright.
                if session.angle.abs() < 1e-4 {
                    session.angle = 0.0;
                }
            }
        }
    }

    pub(crate) fn shape_release(&mut self) {
        let zoom = self.shape_zoom();
        let Some(session) = self.workspace.shapes.session.as_mut() else {
            return;
        };
        let created = matches!(session.drag, Some(ShapeDrag::Create { .. }));
        session.drag = None;
        // A click without a drag draws nothing.
        if created && session.is_tiny(zoom) {
            self.workspace.shapes.session = None;
        }
    }

    /// Finish the polygon being built (double-click, Enter).
    pub(crate) fn shape_finish_polygon(&mut self) {
        if let Some(session) = self.workspace.shapes.session.as_mut()
            && session.building
        {
            session.building = false;
            // A double-click also added a point where it landed.
            let n = session.points.len();
            if n > 2 && (session.points[n - 1] - session.points[n - 2]).length() < 1.0 {
                session.points.pop();
            }
        }
    }

    /// Remove the polygon's last point (Backspace).
    pub(crate) fn shape_undo_point(&mut self) {
        if let Some(session) = self.workspace.shapes.session.as_mut()
            && session.kind == ShapeKind::Polygon
        {
            session.points.pop();
            session.building = true;
            if session.points.is_empty() {
                self.workspace.shapes.session = None;
            }
        }
    }

    pub(crate) fn shape_cancel(&mut self) {
        self.workspace.shapes.session = None;
        self.quickshape_done();
    }

    /// Paint the shape (if any) as one undo step and end it.
    pub(crate) fn shape_commit(&mut self) {
        let Some(mut session) = self.workspace.shapes.session.take() else {
            return;
        };
        self.quickshape_done();
        session.building = false;
        let settings = self.workspace.shapes.settings;
        if session.is_tiny(self.shape_zoom()) || session.points.len() < 2 {
            return;
        }
        let layer_idx = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(layer_idx) else {
            return;
        };
        if layer.locked || layer.kind == LayerKind::Group {
            return;
        }
        self.release_canvas();
        let mut undo = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let symmetry = self.workspace.symmetry;
        let copies = symmetry.copies();
        let mut changed: Option<egui::Rect> = None;
        let mut grow = |r: egui::Rect| changed = Some(changed.map_or(r, |c| c.union(r)));

        // The inside first, so the outline paints over its edge.
        let eraser = self.brush_state.brush.brush_options.blend_mode == BlendMode::Eraser;
        let fill = matches!(settings.style, ShapeStyle::Fill | ShapeStyle::Both);
        if fill && !eraser && session.has_area(settings.closed) {
            let outline = session.outline(true);
            let color = self.brush_state.brush.brush_options.color;
            let mut polygons = vec![outline.clone()];
            for copy in &copies {
                polygons.push(outline.iter().map(|&p| symmetry.map(copy, p)).collect());
            }
            for polygon in polygons {
                if let Some(mask) = self.shape_fill_mask(polygon)
                    && let Some(rect) = self.canvas.paint_mask(layer_idx, &mask, color, &mut undo)
                {
                    grow(rect);
                }
            }
        }

        // The outline, with the brush (steady: no stabilizer lag at corners).
        let outline_style = matches!(settings.style, ShapeStyle::Outline | ShapeStyle::Both);
        if outline_style || !session.has_area(settings.closed) {
            let mut brush = self.brush_state.brush.clone();
            brush.stabilizer_algorithm = StabilizerAlgorithm::None;
            let selection = self.selection_manager.has_selection().then(|| {
                SelectionManager::with_shape(self.selection_manager.current_shape.clone())
            });
            let mut stroke = StrokeState::new();
            let mut tiles = StrokeTiles::default();
            let pool = std::sync::Arc::clone(&self.workspace.pool);
            {
                let canvas = &*self.canvas;
                let mut context =
                    StrokeContext::new(&pool, canvas, selection.as_ref(), &mut undo, &mut tiles)
                        .with_symmetry(&symmetry, &copies);
                pool.install(|| {
                    for p in session.outline(settings.closed) {
                        stroke.add_point(&mut brush, p, 1.0, &mut context);
                    }
                    stroke.finish(&mut brush, &mut context);
                });
            }
            let ts = self.canvas.tile_size() as f32;
            for &(tx, ty) in tiles.buffers.keys() {
                grow(egui::Rect::from_min_size(
                    egui::pos2(tx as f32 * ts, ty as f32 * ts),
                    egui::vec2(ts, ts),
                ));
            }
            if !eraser {
                let color = self.brush_state.brush.brush_options.color;
                self.brush_state.remember_color(color);
            }
        }

        // A tile painted by both keeps only its first (original) snapshot.
        let mut seen = std::collections::HashSet::new();
        undo.tiles.retain(|t| seen.insert((t.layer_id, t.tx, t.ty)));
        if undo.tiles.is_empty() {
            return;
        }
        self.push_undo(undo);
        if let Some(rect) = changed {
            self.mark_tiles_in_bounds_dirty(rect);
        }
        self.layer_state.thumbnails_dirty = true;
    }

    /// Coverage of `polygon`'s inside, clipped to the selection.
    fn shape_fill_mask(&self, polygon: Vec<Vec2>) -> Option<SelectionMask> {
        let shape = crate::selection::new_lasso_shape(polygon);
        let mut manager = SelectionManager::new();
        manager.canvas_size = [self.canvas.width(), self.canvas.height()];
        manager.apply_shape(shape, SelectionMode::Add);
        let Some(crate::selection::SelectionShape::Mask(mask)) = manager.current_shape else {
            return None;
        };
        let mask = (*mask).clone();
        if !self.selection_manager.has_selection() {
            return Some(mask);
        }
        let bounds = [
            mask.x0,
            mask.y0,
            mask.x0 + mask.w as i32,
            mask.y0 + mask.h as i32,
        ];
        let sel = &self.selection_manager;
        let selection = SelectionMask::rasterize(bounds, |y, x0, out| sel.row_coverage(y, x0, out));
        mask.combine(&selection, SelectionMode::Intersect).cropped()
    }
}

/// Draw the shape being edited and its handles.
pub(crate) fn draw_shape(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> egui::Pos2,
) {
    let Some(session) = &app.workspace.shapes.session else {
        return;
    };
    let settings = app.workspace.shapes.settings;
    let closed = session.has_area(settings.closed) && !session.building;
    let mut points: Vec<egui::Pos2> = session
        .outline(false)
        .iter()
        .map(|&p| to_screen(p))
        .collect();
    if session.building
        && let Some(cursor) = session.cursor
    {
        points.push(to_screen(cursor));
    }
    // egui's tessellator spikes on zero-length edges: no repeated points,
    // and closed shapes are drawn closed rather than returning to the start.
    points.dedup_by(|a, b| (*a - *b).length_sq() < 0.01);
    if closed && points.len() > 2 && (points[0] - points[points.len() - 1]).length_sq() < 0.01 {
        points.pop();
    }
    let path = |stroke: Stroke, fill: Color32| {
        egui::Shape::Path(egui::epaint::PathShape {
            points: points.clone(),
            closed,
            fill,
            stroke: stroke.into(),
        })
    };
    // The brush's colour when filling, so it previews what applying paints.
    // (egui fills convex paths only; polygons show their outline.)
    let fill_preview = matches!(settings.style, ShapeStyle::Fill | ShapeStyle::Both)
        && closed
        && session.kind != ShapeKind::Polygon;
    if fill_preview {
        let [r, g, b, _] = app
            .brush_state
            .brush
            .brush_options
            .color
            .to_srgba_unmultiplied();
        painter.add(path(
            Stroke::NONE,
            Color32::from_rgba_unmultiplied(r, g, b, 90),
        ));
    }
    // The outline at the brush's width, then a crisp line on top.
    let width = (app.brush_state.brush.brush_options.diameter * app.viewport.zoom).max(1.0);
    painter.add(path(
        Stroke::new(width, Color32::from_black_alpha(50)),
        Color32::TRANSPARENT,
    ));
    painter.add(path(
        Stroke::new(3.0_f32, Color32::BLACK),
        Color32::TRANSPARENT,
    ));
    painter.add(path(
        Stroke::new(1.0_f32, Color32::WHITE),
        Color32::TRANSPARENT,
    ));
    for h in session.handles() {
        let r =
            egui::Rect::from_center_size(to_screen(h), egui::vec2(HANDLE_SIZE, HANDLE_SIZE) * 2.0);
        painter.rect_filled(r.expand(1.0), 0.0, Color32::BLACK);
        painter.rect_filled(r, 0.0, Color32::WHITE);
    }
    if let Some(h) = session.turn_handle(app.viewport.zoom.max(0.01)) {
        let (from, to) = (to_screen(session.top_middle()), to_screen(h));
        painter.line_segment([from, to], Stroke::new(3.0_f32, Color32::BLACK));
        painter.line_segment([from, to], Stroke::new(1.0_f32, Color32::WHITE));
        painter.circle_filled(to, HANDLE_SIZE + 1.0, Color32::BLACK);
        painter.circle_filled(to, HANDLE_SIZE, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn app() -> crate::PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.diameter = 3.0;
        app
    }

    fn painted(app: &crate::PainterApp, x: i32, y: i32) -> bool {
        let tile = app
            .canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .unwrap_or_default();
        tile.get(((y % 64) * 64 + x % 64) as usize)
            .is_some_and(|c| c.a() > 0)
    }

    fn drag(app: &mut crate::PainterApp, kind: ShapeKind, a: Vec2, b: Vec2, mods: ShapeMods) {
        app.shape_press(kind, a);
        app.shape_move(b, mods);
        app.shape_release();
    }

    #[test]
    fn a_rectangle_outline_and_fill_is_one_step() {
        let mut app = app();
        app.workspace.shapes.settings.style = ShapeStyle::Both;
        drag(
            &mut app,
            ShapeKind::Rectangle,
            Vec2::new(20.0, 20.0),
            Vec2::new(100.0, 80.0),
            ShapeMods::default(),
        );
        assert!(!painted(&app, 60, 50), "nothing until applied");
        app.shape_commit();
        assert!(painted(&app, 20, 50), "outline");
        assert!(painted(&app, 60, 50), "fill");
        assert!(!painted(&app, 110, 50));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert!(
            !painted(&app, 20, 50) && !painted(&app, 60, 50),
            "undone together"
        );
        app.apply_history(true);
        assert!(
            painted(&app, 20, 50) && painted(&app, 60, 50),
            "redone together"
        );
    }

    #[test]
    fn constrained_shapes_are_square_and_15_degree_lines() {
        let mut app = app();
        let mods = ShapeMods {
            constrain: true,
            from_center: false,
        };
        app.shape_press(ShapeKind::Ellipse, Vec2::new(10.0, 10.0));
        app.shape_move(Vec2::new(50.0, 30.0), mods);
        let pts = &app.workspace.shapes.session.as_ref().unwrap().points;
        assert_eq!(pts[1] - pts[0], Vec2::new(40.0, 40.0));
        app.shape_cancel();

        app.shape_press(ShapeKind::Line, Vec2::new(0.0, 0.0));
        app.shape_move(Vec2::new(100.0, 4.0), mods);
        let pts = &app.workspace.shapes.session.as_ref().unwrap().points;
        assert!(pts[1].y.abs() < 1e-3, "snapped to horizontal: {:?}", pts[1]);
    }

    #[test]
    fn shapes_stay_editable_until_applied() {
        let mut app = app();
        drag(
            &mut app,
            ShapeKind::Line,
            Vec2::new(10.0, 10.0),
            Vec2::new(60.0, 10.0),
            ShapeMods::default(),
        );
        // Drag the end handle down.
        drag(
            &mut app,
            ShapeKind::Line,
            Vec2::new(60.0, 10.0),
            Vec2::new(60.0, 90.0),
            ShapeMods::default(),
        );
        // Drag the line itself right.
        drag(
            &mut app,
            ShapeKind::Line,
            Vec2::new(35.0, 50.0),
            Vec2::new(45.0, 50.0),
            ShapeMods::default(),
        );
        let pts = app
            .workspace
            .shapes
            .session
            .as_ref()
            .unwrap()
            .points
            .clone();
        assert_eq!(pts, vec![Vec2::new(20.0, 10.0), Vec2::new(70.0, 90.0)]);
        // A press away from it applies it and starts another.
        app.shape_press(ShapeKind::Line, Vec2::new(120.0, 120.0));
        assert!(painted(&app, 45, 50));
    }

    #[test]
    fn the_round_handle_turns_an_ellipse_and_applying_paints_it_turned() {
        let mut app = app();
        app.viewport.zoom = 1.0;
        app.workspace.shapes.settings.style = ShapeStyle::Fill;
        // A long, thin ellipse across the middle.
        drag(
            &mut app,
            ShapeKind::Ellipse,
            Vec2::new(14.0, 54.0),
            Vec2::new(114.0, 74.0),
            ShapeMods::default(),
        );
        let session = app.workspace.shapes.session.as_ref().unwrap();
        let handle = session.turn_handle(1.0).unwrap();
        assert_eq!(handle, Vec2::new(64.0, 54.0 - TURN_HANDLE_GAP));
        // Drag the handle round to the right of the centre (64, 64): a
        // quarter turn clockwise, snapped with Shift.
        let snap = ShapeMods {
            constrain: true,
            from_center: false,
        };
        drag(
            &mut app,
            ShapeKind::Ellipse,
            handle,
            Vec2::new(64.0 + 40.0, 66.0),
            snap,
        );
        let session = app.workspace.shapes.session.as_ref().unwrap();
        assert!((session.angle - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // Its handles turned with it: the top left corner is now top right.
        let h = session.handles();
        assert!((h[0] - Vec2::new(74.0, 14.0)).length() < 1e-3, "{h:?}");
        app.shape_commit();
        assert!(
            painted(&app, 64, 20) && painted(&app, 64, 108),
            "upright now"
        );
        assert!(!painted(&app, 20, 64) && !painted(&app, 108, 64));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1, "one step");
        app.apply_history(false);
        assert!(!painted(&app, 64, 20) && !painted(&app, 64, 64), "undone");
    }

    #[test]
    fn a_turned_ellipse_is_matched_by_the_same_shape_drawn_upright() {
        // An ellipse turned a quarter paints exactly what the same one
        // drawn tall does (its outline points agree to rounding).
        let paint = |points: Vec<Vec2>, angle: f32| {
            let mut app = app();
            app.workspace.shapes.settings.style = ShapeStyle::Both;
            app.workspace
                .shapes
                .start_editing(ShapeKind::Ellipse, points, angle);
            app.shape_commit();
            (0..128)
                .flat_map(|y| (0..128).map(move |x| (x, y)))
                .filter(|&(x, y)| painted(&app, x, y))
                .count()
        };
        let turned = paint(
            vec![Vec2::new(24.0, 44.0), Vec2::new(104.0, 84.0)],
            std::f32::consts::FRAC_PI_2,
        );
        let tall = paint(vec![Vec2::new(44.0, 24.0), Vec2::new(84.0, 104.0)], 0.0);
        let diff = turned.abs_diff(tall);
        assert!(diff * 100 < tall, "{turned} vs {tall}");
    }

    #[test]
    fn a_turned_ellipse_resizes_along_its_own_axes() {
        let mut app = app();
        app.viewport.zoom = 1.0;
        let angle = 30f32.to_radians();
        app.workspace.shapes.start_editing(
            ShapeKind::Ellipse,
            vec![Vec2::new(34.0, 44.0), Vec2::new(94.0, 84.0)],
            angle,
        );
        let before = app.workspace.shapes.session.as_ref().unwrap().handles();
        // Pull the bottom right corner out along the ellipse's width.
        let out = before[2] + turned(Vec2::new(10.0, 0.0), angle);
        drag(
            &mut app,
            ShapeKind::Ellipse,
            before[2],
            out,
            ShapeMods::default(),
        );
        let session = app.workspace.shapes.session.as_ref().unwrap();
        let after = session.handles();
        assert!(
            (after[0] - before[0]).length() < 1e-3,
            "opposite corner kept"
        );
        assert!(
            (after[2] - out).length() < 1e-3,
            "{:?} vs {out:?}",
            after[2]
        );
        let size = (session.points[1] - session.points[0]).abs();
        assert!((size - Vec2::new(70.0, 40.0)).length() < 1e-3, "{size:?}");
        assert_eq!(session.angle, angle);
    }

    #[test]
    fn a_polygon_is_built_point_by_point() {
        let mut app = app();
        app.workspace.shapes.settings.style = ShapeStyle::Fill;
        for p in [(20.0, 20.0), (100.0, 20.0), (60.0, 100.0)] {
            app.shape_press(ShapeKind::Polygon, Vec2::new(p.0, p.1));
            app.shape_release();
        }
        // Back to the first point closes it.
        app.shape_press(ShapeKind::Polygon, Vec2::new(21.0, 21.0));
        app.shape_release();
        assert!(!app.workspace.shapes.session.as_ref().unwrap().building);
        app.shape_commit();
        assert!(painted(&app, 60, 40));
        assert!(!painted(&app, 25, 90));
    }
}
