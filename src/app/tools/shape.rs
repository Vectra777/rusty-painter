//! Shape tools: line, rectangle, ellipse and polygon, drawn with the current
//! brush (outline), filled with the brush colour, or both.
//!
//! A shape stays editable (drag its handles, or inside it to move it) until
//! it's applied: Enter, a press outside it, or another tool. Esc cancels.
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
}

pub struct ShapeToolState {
    pub settings: ShapeSettings,
    pub session: Option<ShapeSession>,
    /// The shape the tool button picks.
    pub last_kind: ShapeKind,
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

/// The box corners of `[p, q]`, clockwise from the top left.
fn corners(p: Vec2, q: Vec2) -> [Vec2; 4] {
    let (min, max) = (p.min(q), p.max(q));
    [min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)]
}

impl ShapeSession {
    /// The points handles sit on.
    fn handles(&self) -> Vec<Vec2> {
        match self.kind {
            ShapeKind::Rectangle | ShapeKind::Ellipse => {
                corners(self.points[0], self.points[1]).to_vec()
            }
            ShapeKind::Line | ShapeKind::Polygon => self.points.clone(),
        }
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
                (0..=n)
                    .map(|i| {
                        let a = std::f32::consts::TAU * i as f32 / n as f32;
                        center + Vec2::new(a.cos() * r.x, a.sin() * r.y)
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
            if let Some(i) = session
                .handles()
                .iter()
                .position(|h| (*h - pos).length() <= hit)
            {
                session.drag = Some(ShapeDrag::Point(i));
                return;
            }
            let (min, max) = session.bounds();
            let inside = pos.x >= min.x - hit
                && pos.y >= min.y - hit
                && pos.x <= max.x + hit
                && pos.y <= max.y + hit;
            if inside {
                session.drag = Some(ShapeDrag::Move { last: pos });
                return;
            }
            // Pressing elsewhere applies this shape and starts the next.
            self.shape_commit();
        }
        self.workspace.shapes.session = Some(match kind {
            ShapeKind::Polygon => ShapeSession {
                kind,
                points: vec![pos],
                drag: None,
                building: true,
                cursor: Some(pos),
            },
            _ => ShapeSession {
                kind,
                points: vec![pos, pos],
                drag: Some(ShapeDrag::Create { anchor: pos }),
                building: false,
                cursor: Some(pos),
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
    }

    /// Paint the shape (if any) as one undo step and end it.
    pub(crate) fn shape_commit(&mut self) {
        let Some(mut session) = self.workspace.shapes.session.take() else {
            return;
        };
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
        if let Some(history) = self.layer_state.histories.get_mut(layer_idx) {
            history.push_action(undo);
        }
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
        assert_eq!(app.layer_state.histories[1].stacks().0.len(), 1);
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
