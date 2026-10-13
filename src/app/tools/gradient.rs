//! The Gradient tool: drag from where the first colour is to where the
//! second is. The gradient shows on the layer as it's dragged and stays
//! adjustable (drag its ends) until it's applied: Enter, a press away from
//! it, or another tool. Esc cancels.
//!
//! On a gradient fill layer the tool edits the fill instead: its ends show,
//! a drag moves one or, away from them, places the gradient anew, each an
//! undo step. With **Fill layer** chosen, a drag makes a new gradient fill
//! layer, which stays editable that way.

use crate::app::PainterApp;
pub use crate::app::tools::gradient_colors::{GradientColors, GradientLibrary};
use crate::canvas::gradient::{Gradient, GradientRepeat, GradientShape, Ramp};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::LayerKind;
use crate::selection::SelectionMask;
use eframe::egui::{self, Color32, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GradientSettings {
    pub shape: GradientShape,
    pub colors: GradientColors,
    pub repeat: GradientRepeat,
    pub reverse: bool,
    pub opacity: f32,
    pub dither: bool,
    /// How the gradient mixes with the paint under it.
    pub blend: crate::canvas::blend_modes::LayerBlend,
    /// A drag makes a gradient fill layer (editable later) rather than
    /// painting the gradient.
    pub fill_layer: bool,
}

impl Default for GradientSettings {
    fn default() -> Self {
        Self {
            shape: GradientShape::Linear,
            colors: GradientColors::ForegroundToBackground,
            repeat: GradientRepeat::None,
            reverse: false,
            opacity: 1.0,
            dither: true,
            blend: crate::canvas::blend_modes::LayerBlend::Normal,
            fill_layer: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Start,
    End,
}

/// A gradient being placed.
pub struct GradientSession {
    pub start: Vec2,
    pub end: Vec2,
    drag: Option<Drag>,
    layer: usize,
    /// The area's tiles before the gradient, captured when it's first
    /// painted; every repaint composites over them.
    original: Option<crate::canvas::storage::Region>,
    /// The selection's coverage over the area, rasterized once.
    coverage: Option<SelectionMask>,
    bounds: [i32; 4],
    /// Needs painting again (moved, or settings changed).
    dirty: bool,
}

/// A gradient fill layer's ends being dragged, or a new one's being
/// placed.
pub struct FillEdit {
    /// The fill layer, or `None` for one made on release.
    layer: Option<crate::canvas::storage::LayerId>,
    pub start: Vec2,
    pub end: Vec2,
    drag: Drag,
    /// Moved since the press (a click changes nothing).
    moved: bool,
}

#[derive(Default)]
pub struct GradientToolState {
    pub settings: GradientSettings,
    pub session: Option<GradientSession>,
    /// A fill layer's gradient being dragged.
    pub fill_edit: Option<FillEdit>,
    /// Presets and the user's own gradients.
    pub library: GradientLibrary,
    /// The Gradient Editor, while open.
    pub editor: Option<GradientEditor>,
    /// The last repaint isn't all on screen yet (a big area takes a few
    /// frames to upload); repainting before then would show bands of old
    /// and new gradient.
    uploading: bool,
}

/// The Gradient Editor's state: which user gradient it edits.
#[derive(Clone, Copy, Debug, Default)]
pub struct GradientEditor {
    /// Index into [`GradientLibrary::custom`].
    pub index: usize,
}

/// In screen points.
const HANDLE_HIT: f32 = 12.0;
const TINY: f32 = 2.0;

impl PainterApp {
    /// The selected layer's gradient fill ends, if it's a gradient fill.
    pub(crate) fn active_gradient_fill(&self) -> Option<(Vec2, Vec2)> {
        match &self
            .canvas
            .layers
            .get(self.canvas.active_layer_idx)?
            .style
            .fill
        {
            Some(crate::canvas::layer_style::LayerFill::Gradient { start, end, .. }) => {
                Some((Vec2::from(*start), Vec2::from(*end)))
            }
            _ => None,
        }
    }

    /// A press with the Gradient tool at canvas point `pos`.
    pub(crate) fn gradient_press(&mut self, pos: Vec2) {
        let hit = HANDLE_HIT / self.viewport.zoom.max(0.01);
        // A gradient fill: drag one of its ends, or place it anew.
        if self.workspace.gradient.session.is_none()
            && let Some((start, end)) = self.active_gradient_fill()
        {
            let layer = self.canvas.layer_id_at(self.canvas.active_layer_idx);
            let edit = if (pos - end).length() <= hit {
                FillEdit {
                    layer,
                    start,
                    end,
                    drag: Drag::End,
                    moved: false,
                }
            } else if (pos - start).length() <= hit {
                FillEdit {
                    layer,
                    start,
                    end,
                    drag: Drag::Start,
                    moved: false,
                }
            } else {
                FillEdit {
                    layer,
                    start: pos,
                    end: pos,
                    drag: Drag::End,
                    moved: false,
                }
            };
            self.workspace.gradient.fill_edit = Some(edit);
            return;
        }
        if self.workspace.gradient.settings.fill_layer && self.workspace.gradient.session.is_none()
        {
            self.workspace.gradient.fill_edit = Some(FillEdit {
                layer: None,
                start: pos,
                end: pos,
                drag: Drag::End,
                moved: false,
            });
            return;
        }
        if let Some(session) = self.workspace.gradient.session.as_mut() {
            if (pos - session.end).length() <= hit {
                session.drag = Some(Drag::End);
                return;
            }
            if (pos - session.start).length() <= hit {
                session.drag = Some(Drag::Start);
                return;
            }
            self.gradient_commit();
        }
        let layer = self.canvas.active_layer_idx;
        let Some(l) = self.canvas.layers.get(layer) else {
            return;
        };
        if l.locked || l.kind == LayerKind::Group {
            return;
        }
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        // The selection's area, or the whole canvas.
        // (On a moved layer: the selection carried onto its pixels.)
        let selection = self.layer_selection();
        let (bounds, coverage) = match selection.get_bounds() {
            Some(b) if selection.has_selection() => {
                let bounds = [
                    (b.min.x.floor() as i32 - 1).clamp(0, w),
                    (b.min.y.floor() as i32 - 1).clamp(0, h),
                    (b.max.x.ceil() as i32 + 1).clamp(0, w),
                    (b.max.y.ceil() as i32 + 1).clamp(0, h),
                ];
                let sel = &selection;
                let mask =
                    SelectionMask::rasterize(bounds, |y, x0, out| sel.row_coverage(y, x0, out));
                (bounds, Some(mask))
            }
            _ => ([0, 0, w, h], None),
        };
        self.workspace.gradient.session = Some(GradientSession {
            start: pos,
            end: pos,
            drag: Some(Drag::End),
            layer,
            original: None,
            coverage,
            bounds,
            dirty: false,
        });
    }

    /// Drag with the Gradient tool; `snap` (Shift) keeps it to 15° steps.
    pub(crate) fn gradient_drag(&mut self, pos: Vec2, snap: bool) {
        let snapped = |from: Vec2, to: Vec2| {
            if !snap {
                return to;
            }
            let d = to - from;
            let step = std::f32::consts::PI / 12.0;
            let a = (d.y.atan2(d.x) / step).round() * step;
            from + Vec2::new(a.cos(), a.sin()) * d.length()
        };
        if let Some(edit) = self.workspace.gradient.fill_edit.as_mut() {
            match edit.drag {
                Drag::End => edit.end = snapped(edit.start, pos),
                Drag::Start => edit.start = snapped(edit.end, pos),
            }
            let zoom = self.viewport.zoom.max(0.01);
            edit.moved |= (edit.end - edit.start).length() * zoom >= TINY;
            if let (Some(id), true) = (edit.layer, edit.moved) {
                let (start, end) = (edit.start, edit.end);
                self.gradient_fill_set(id, start, end);
            }
            return;
        }
        let Some(session) = self.workspace.gradient.session.as_mut() else {
            return;
        };
        let Some(drag) = session.drag else {
            return;
        };
        match drag {
            Drag::End => session.end = snapped(session.start, pos),
            Drag::Start => session.start = snapped(session.end, pos),
        }
        session.dirty = true;
    }

    /// Fill layer `id`'s gradient ends set to `start` and `end` (undone
    /// with the rest of the drag, once let go).
    fn gradient_fill_set(&mut self, id: crate::canvas::storage::LayerId, start: Vec2, end: Vec2) {
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        let mut style = self.canvas.layers[idx].style.clone();
        if let Some(crate::canvas::layer_style::LayerFill::Gradient {
            start: s, end: e, ..
        }) = style.fill.as_mut()
        {
            (*s, *e) = ([start.x, start.y], [end.x, end.y]);
            self.set_layer_style(idx, style);
        }
    }

    pub(crate) fn gradient_release(&mut self) {
        if let Some(edit) = self.workspace.gradient.fill_edit.take() {
            if edit.moved && edit.layer.is_none() {
                // A new fill layer with the tool's gradient and settings.
                let settings = self.workspace.gradient.settings;
                let colours = self.workspace.gradient.library.stops(
                    settings.colors,
                    self.brush_state.brush.brush_options.color,
                    self.brush_state.secondary_color,
                );
                self.add_fill_layer(crate::canvas::layer_style::LayerFill::Gradient {
                    colours,
                    shape: settings.shape,
                    start: [edit.start.x, edit.start.y],
                    end: [edit.end.x, edit.end.y],
                    repeat: settings.repeat,
                    reverse: settings.reverse,
                });
                // Edited on the canvas: no dialog over it.
                self.workspace.filter.fill_editing = None;
            }
            self.settings_record(false);
            return;
        }
        let zoom = self.viewport.zoom.max(0.01);
        let Some(session) = self.workspace.gradient.session.as_mut() else {
            return;
        };
        session.drag = None;
        // A click without a drag paints nothing.
        if session.original.is_none() && (session.end - session.start).length() * zoom < TINY {
            self.workspace.gradient.session = None;
        }
    }

    /// Paint the gradient again after it (or its settings) changed. Called
    /// once a frame, so dragging costs at most one repaint per frame, and
    /// none until the last one is fully on screen.
    pub(crate) fn gradient_update(&mut self) {
        let state = &self.workspace.gradient;
        let dirty = state
            .session
            .as_ref()
            .is_some_and(|s| s.dirty && (s.end - s.start).length() > 0.5);
        if dirty && !state.uploading {
            self.gradient_paint();
            self.workspace.gradient.uploading = true;
        }
    }

    /// After this frame's tile uploads: whether the gradient waits to be
    /// repainted (so another frame is needed).
    pub(crate) fn gradient_uploaded(&mut self, more_tiles: bool) -> bool {
        let state = &mut self.workspace.gradient;
        state.uploading &= more_tiles;
        !state.uploading && state.session.as_ref().is_some_and(|s| s.dirty)
    }

    /// Open the Gradient Editor on the chosen gradient, or on a copy of it
    /// when `copy` (a new gradient). The brush colours and presets can't
    /// change, so they're always copied first (the copy looks the same, so
    /// nothing repaints).
    pub(crate) fn gradient_edit(&mut self, copy: bool) {
        let state = &mut self.workspace.gradient;
        let index = match state.settings.colors {
            GradientColors::Custom(i) if !copy && i < state.library.custom.len() => i,
            other => {
                let mut copy = state.library.editable(other);
                copy.name = format!("{} copy", copy.name);
                state.settings.colors = state.library.add(copy);
                state.library.custom.len() - 1
            }
        };
        state.editor = Some(GradientEditor { index });
    }

    /// Settings changed: repaint the gradient being placed, or give the
    /// selected gradient fill the tool's gradient, shape, repeat and
    /// reverse (an undo step once let go of).
    pub(crate) fn gradient_settings_changed(&mut self) {
        if let Some(session) = self.workspace.gradient.session.as_mut() {
            session.dirty = true;
            return;
        }
        if self.active_tool != crate::app::tools::Tool::Gradient
            || self.active_gradient_fill().is_none()
        {
            return;
        }
        let settings = self.workspace.gradient.settings;
        let stops = self.workspace.gradient.library.stops(
            settings.colors,
            self.brush_state.brush.brush_options.color,
            self.brush_state.secondary_color,
        );
        let idx = self.canvas.active_layer_idx;
        let mut style = self.canvas.layers[idx].style.clone();
        if let Some(crate::canvas::layer_style::LayerFill::Gradient {
            colours,
            shape,
            repeat,
            reverse,
            ..
        }) = style.fill.as_mut()
        {
            (*colours, *shape, *repeat, *reverse) =
                (stops, settings.shape, settings.repeat, settings.reverse);
            self.set_layer_style(idx, style);
        }
    }

    fn gradient_area(&self) -> Option<egui::Rect> {
        let [x0, y0, x1, y1] = self.workspace.gradient.session.as_ref()?.bounds;
        Some(egui::Rect::from_min_max(
            egui::pos2(x0 as f32, y0 as f32),
            egui::pos2(x1 as f32, y1 as f32),
        ))
    }

    fn gradient_paint(&mut self) {
        self.release_canvas();
        let settings = self.workspace.gradient.settings;
        let model = self.workspace.color_model;
        let mut stops = self.workspace.gradient.library.stops(
            settings.colors,
            self.brush_state.brush.brush_options.color,
            self.brush_state.secondary_color,
        );
        for stop in &mut stops.colors {
            let [r, g, b] = stop.color;
            let [r, g, b, _] =
                PainterApp::convert_color_for_model(Color32::from_rgb(r, g, b), model).to_array();
            stop.color = [r, g, b];
        }
        let ramp = Ramp::new(&stops, self.canvas.blend_space, settings.opacity);
        let pool = std::sync::Arc::clone(&self.workspace.pool);
        let canvas = std::sync::Arc::clone(&self.canvas);
        let Some(session) = self.workspace.gradient.session.as_mut() else {
            return;
        };
        let (layer, bounds) = (session.layer, session.bounds);
        let original = session
            .original
            .get_or_insert_with(|| pool.install(|| canvas.capture_region(layer, bounds)));
        let gradient = Gradient {
            shape: settings.shape,
            repeat: settings.repeat,
            start: session.start,
            end: session.end,
            reverse: settings.reverse,
        };
        let coverage = session.coverage.as_ref();
        let dither = settings.dither;
        thread_local! {
            static POSITIONS: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let row = |x0: i32, y: i32, out: &mut [Color32]| {
            POSITIONS.with_borrow_mut(|t| {
                t.resize(out.len(), 0.0);
                gradient.row_positions(x0, y, t);
                for (i, (o, &t)) in out.iter_mut().zip(t.iter()).enumerate() {
                    let x = x0 + i as i32;
                    let cov = coverage.map_or(255, |m| m.value(x, y));
                    *o = if cov == 0 {
                        Color32::TRANSPARENT
                    } else {
                        let noise = dither
                            .then(|| crate::canvas::blend_modes::pixel_noise(x as u32, y as u32));
                        ramp.pixel_covered(t, noise, cov)
                    };
                }
            });
        };
        if canvas.depth().is_deep() {
            // At full depth: no rounding, so no dither needed.
            let row = |x0: i32, y: i32, out: &mut [[f32; 4]]| {
                POSITIONS.with_borrow_mut(|t| {
                    t.resize(out.len(), 0.0);
                    gradient.row_positions(x0, y, t);
                    for (i, (o, &t)) in out.iter_mut().zip(t.iter()).enumerate() {
                        let cov = coverage.map_or(255, |m| m.value(x0 + i as i32, y));
                        *o = if cov == 0 {
                            [0.0; 4]
                        } else {
                            ramp.linear_covered(t, cov as f32 / 255.0)
                        };
                    }
                });
            };
            pool.install(|| canvas.paint_over_region_deep(layer, original, settings.blend, row));
        } else {
            pool.install(|| canvas.paint_over_region(layer, original, settings.blend, row));
        }
        session.dirty = false;
        drop(canvas);
        if let Some(area) = self.gradient_area() {
            self.mark_tiles_in_bounds_dirty(area);
        }
        self.layer_state.thumbnails_dirty = true;
    }

    /// Keep the gradient as one undo step.
    pub(crate) fn gradient_commit(&mut self) {
        if self
            .workspace
            .gradient
            .session
            .as_ref()
            .is_some_and(|s| s.dirty)
        {
            self.gradient_paint();
        }
        let Some(session) = self.workspace.gradient.session.take() else {
            return;
        };
        let Some(original) = session.original else {
            return;
        };
        let pool = std::sync::Arc::clone(&self.workspace.pool);
        let tiles = pool.install(|| self.canvas.region_snapshots(session.layer, &original));
        if tiles.is_empty() {
            return;
        }
        self.push_undo(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: None,
        });
    }

    /// Take the gradient back off the layer.
    pub(crate) fn gradient_cancel(&mut self) {
        let area = self.gradient_area();
        if let Some(session) = self.workspace.gradient.session.take()
            && let Some(original) = &session.original
        {
            self.release_canvas();
            self.canvas.restore_region(original);
            if let Some(area) = area {
                self.mark_tiles_in_bounds_dirty(area);
            }
            self.layer_state.thumbnails_dirty = true;
        }
    }
}

/// Draw the gradient's line and handles.
pub(crate) fn draw_gradient(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> egui::Pos2,
) {
    let state = &app.workspace.gradient;
    let settings = state.settings;
    let tool_stops = || {
        state.library.stops(
            settings.colors,
            app.brush_state.brush.brush_options.color,
            app.brush_state.secondary_color,
        )
    };
    // The gradient being placed, a new fill being dragged out, or the
    // selected gradient fill (while the tool is chosen).
    let fill = app
        .canvas
        .layers
        .get(app.canvas.active_layer_idx)
        .and_then(|l| l.style.fill.as_ref());
    let (start, end, shape, stops, reverse) = if let Some(session) = &state.session {
        (
            session.start,
            session.end,
            settings.shape,
            tool_stops(),
            settings.reverse,
        )
    } else if let Some(edit) = state.fill_edit.as_ref().filter(|e| e.layer.is_none()) {
        (
            edit.start,
            edit.end,
            settings.shape,
            tool_stops(),
            settings.reverse,
        )
    } else if let (
        crate::app::tools::Tool::Gradient,
        Some(crate::canvas::layer_style::LayerFill::Gradient {
            colours,
            shape,
            start,
            end,
            reverse,
            ..
        }),
    ) = (app.active_tool, fill)
    {
        (
            Vec2::from(*start),
            Vec2::from(*end),
            *shape,
            colours.clone(),
            *reverse,
        )
    } else {
        return;
    };
    let (a, b) = (to_screen(start), to_screen(end));
    match shape {
        GradientShape::Radial => {
            let r = (b - a).length();
            painter.circle_stroke(a, r, Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
            painter.circle_stroke(a, r, Stroke::new(1.0_f32, Color32::from_white_alpha(200)));
        }
        GradientShape::Diamond => {
            // The square the end colour reaches, turned towards the end.
            let d = b - a;
            let n = egui::vec2(-d.y, d.x);
            let corners = vec![b, a + n, a - d, a - n];
            for (width, color) in [
                (3.0_f32, Color32::from_black_alpha(140)),
                (1.0_f32, Color32::from_white_alpha(200)),
            ] {
                painter.add(egui::Shape::closed_line(
                    corners.clone(),
                    Stroke::new(width, color),
                ));
            }
        }
        _ => {}
    }
    painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::BLACK));
    painter.line_segment([a, b], Stroke::new(1.0_f32, Color32::WHITE));
    let swatch = |p: egui::Pos2, color: Color32| {
        let r = egui::Rect::from_center_size(p, egui::vec2(12.0, 12.0));
        painter.rect_filled(r.expand(1.5), 0.0, Color32::BLACK);
        crate::ui::widgets::paint_swatch(painter, r, color);
    };
    let end = |t| {
        let [r, g, b, a] = stops.eval(t, crate::canvas::blend_modes::BlendSpace::Gamma);
        let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        Color32::from_rgba_unmultiplied(q(r), q(g), q(b), q(a * 255.0))
    };
    let (from, to) = (end(0.0), end(1.0));
    let (first, second) = if reverse { (to, from) } else { (from, to) };
    swatch(a, first);
    swatch(b, second);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tools::gradient_colors::PRESETS;
    use crate::canvas::Canvas;

    fn app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.color = Color32::BLACK;
        app.brush_state.secondary_color = Color32::WHITE;
        app
    }

    fn pixel(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
        let tile = app
            .canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .unwrap_or_default();
        tile.get(((y % 64) * 64 + x % 64) as usize)
            .copied()
            .unwrap_or_default()
    }

    #[test]
    fn a_gradient_previews_adjusts_and_applies_as_one_step() {
        let mut app = app();
        app.gradient_press(Vec2::new(0.0, 32.0));
        app.gradient_drag(Vec2::new(128.0, 32.0), false);
        app.gradient_update();
        app.gradient_uploaded(false);
        app.gradient_release();
        // Black to white, mixed in linear light: the middle is sRGB ~188.
        let (left, mid, right) = (
            pixel(&app, 2, 10),
            pixel(&app, 64, 10),
            pixel(&app, 125, 10),
        );
        assert!(
            left.r() < 50 && right.r() > 235,
            "dark to light: {left:?} {right:?}"
        );
        assert!(
            (180..=196).contains(&mid.r()),
            "linear-light middle: {mid:?}"
        );
        // Drag the end back: repainted from the original, not on top.
        app.gradient_press(Vec2::new(128.0, 32.0));
        app.gradient_drag(Vec2::new(64.0, 32.0), false);
        app.gradient_update();
        app.gradient_release();
        assert!(
            pixel(&app, 80, 10).r() > 235,
            "past the end: the end colour"
        );
        app.gradient_commit();
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(pixel(&app, 80, 10).a(), 0, "undone");
    }

    #[test]
    fn a_gradient_mixes_with_the_paint_under_it_by_its_blend_mode() {
        let mut app = app();
        // Grey paint first: a gradient from grey to grey.
        let grey = Color32::from_gray(128);
        (
            app.brush_state.brush.brush_options.color,
            app.brush_state.secondary_color,
        ) = (grey, grey);
        app.gradient_press(Vec2::new(0.0, 32.0));
        app.gradient_drag(Vec2::new(128.0, 32.0), false);
        app.gradient_commit();
        // Black to white over it, multiplied: white leaves the grey.
        app.brush_state.brush.brush_options.color = Color32::BLACK;
        app.brush_state.secondary_color = Color32::WHITE;
        app.workspace.gradient.settings.blend = crate::canvas::blend_modes::LayerBlend::Multiply;
        app.gradient_press(Vec2::new(0.0, 32.0));
        app.gradient_drag(Vec2::new(128.0, 32.0), false);
        app.gradient_commit();
        let (left, right) = (pixel(&app, 1, 10), pixel(&app, 126, 10));
        assert!(left.r() < 10, "black multiplied: {left:?}");
        assert!(
            right.r().abs_diff(128) <= 4,
            "white multiplied keeps the grey: {right:?}"
        );
    }

    #[test]
    fn a_fill_layer_gradient_is_made_and_edited_on_the_canvas() {
        use crate::canvas::layer_style::LayerFill;
        let mut app = app();
        app.active_tool = crate::app::tools::Tool::Gradient;
        let layers = app.canvas.layers.len();
        app.workspace.gradient.settings.fill_layer = true;
        app.gradient_press(Vec2::new(10.0, 32.0));
        app.gradient_drag(Vec2::new(110.0, 32.0), false);
        assert_eq!(app.canvas.layers.len(), layers, "made when let go");
        app.gradient_release();
        assert_eq!(app.canvas.layers.len(), layers + 1);
        let ends = |app: &crate::PainterApp| app.active_gradient_fill().expect("a gradient fill");
        assert_eq!(ends(&app), (Vec2::new(10.0, 32.0), Vec2::new(110.0, 32.0)));
        assert!(
            app.workspace.filter.fill_editing.is_none(),
            "no dialog over it"
        );
        // Its end dragged: the fill follows, one undo step.
        let steps = app.layer_state.history.stacks().0.len();
        app.gradient_press(Vec2::new(110.0, 32.0));
        app.gradient_drag(Vec2::new(60.0, 50.0), false);
        app.gradient_drag(Vec2::new(70.0, 50.0), false);
        app.gradient_release();
        assert_eq!(ends(&app).1, Vec2::new(70.0, 50.0));
        assert_eq!(app.layer_state.history.stacks().0.len(), steps + 1);
        // Away from its ends: placed anew.
        app.gradient_press(Vec2::new(64.0, 5.0));
        app.gradient_drag(Vec2::new(64.0, 60.0), false);
        app.gradient_release();
        assert_eq!(ends(&app), (Vec2::new(64.0, 5.0), Vec2::new(64.0, 60.0)));
        // A click on it changes nothing.
        app.gradient_press(Vec2::new(20.0, 20.0));
        app.gradient_release();
        assert_eq!(ends(&app), (Vec2::new(64.0, 5.0), Vec2::new(64.0, 60.0)));
        // Undo back through the edits, then the layer itself.
        app.apply_history(false);
        assert_eq!(ends(&app).1, Vec2::new(70.0, 50.0));
        app.apply_history(false);
        assert_eq!(ends(&app).1, Vec2::new(110.0, 32.0));
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), layers);
        assert!(
            app.canvas
                .layers
                .iter()
                .all(|l| !matches!(l.style.fill, Some(LayerFill::Gradient { .. })))
        );
    }

    #[test]
    fn the_tool_settings_change_a_selected_gradient_fill() {
        let mut app = app();
        app.active_tool = crate::app::tools::Tool::Gradient;
        app.add_gradient_fill_layer();
        app.workspace.gradient.settings.shape = GradientShape::Diamond;
        app.gradient_settings_changed();
        app.settings_record(false);
        let idx = app.canvas.active_layer_idx;
        let shape = |app: &crate::PainterApp| {
            app.canvas.layers[idx]
                .style
                .fill
                .as_ref()
                .and_then(|f| f.gradient())
                .map(|g| g.shape)
        };
        assert_eq!(shape(&app), Some(GradientShape::Diamond));
        app.apply_history(false);
        assert_eq!(shape(&app), Some(GradientShape::Linear), "undone");
    }

    #[test]
    fn a_gradient_waits_for_its_last_repaint_to_reach_the_screen() {
        let mut app = app();
        app.gradient_press(Vec2::new(0.0, 32.0));
        app.gradient_drag(Vec2::new(128.0, 32.0), false);
        app.gradient_update();
        // Tiles still uploading: a move doesn't repaint yet.
        app.gradient_drag(Vec2::new(64.0, 32.0), false);
        assert!(!app.gradient_uploaded(true));
        app.gradient_update();
        assert!(pixel(&app, 80, 10).r() < 235, "repainted too early");
        // All on screen: the next frame repaints.
        assert!(app.gradient_uploaded(false), "asks for another frame");
        app.gradient_update();
        assert!(pixel(&app, 80, 10).r() > 235, "repainted");
    }

    #[test]
    fn a_preset_paints_its_own_colours() {
        let mut app = app();
        let sunset = PRESETS.iter().position(|p| p.name == "Sunset").unwrap();
        app.workspace.gradient.settings.colors = GradientColors::Preset(sunset);
        app.workspace.gradient.settings.dither = false;
        // The first column is before the start.
        app.gradient_press(Vec2::new(8.0, 32.0));
        app.gradient_drag(Vec2::new(128.0, 32.0), false);
        app.gradient_commit();
        let [r, g, b, _] = PRESETS[sunset].stops[0].1;
        let first = pixel(&app, 0, 10);
        assert!(
            first.r().abs_diff(r) <= 2 && first.g().abs_diff(g) <= 2 && first.b().abs_diff(b) <= 2,
            "starts at the first stop: {first:?}"
        );
        assert!(pixel(&app, 127, 10).r() > 240, "ends on the yellow");
        // Every preset lists its stops in order, from 0 to 1.
        for preset in PRESETS {
            let pos: Vec<f32> = preset.stops.iter().map(|s| s.0).collect();
            assert!(pos.windows(2).all(|w| w[0] <= w[1]), "{}", preset.name);
            assert_eq!((pos[0], pos[pos.len() - 1]), (0.0, 1.0), "{}", preset.name);
        }
    }

    #[test]
    fn cancelling_leaves_the_layer_as_it_was() {
        let mut app = app();
        app.gradient_press(Vec2::new(0.0, 0.0));
        app.gradient_drag(Vec2::new(100.0, 0.0), false);
        app.gradient_update();
        app.gradient_cancel();
        assert_eq!(pixel(&app, 50, 10).a(), 0);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    #[test]
    fn a_gradient_stays_inside_the_selection() {
        let mut app = app();
        app.selection_manager.canvas_size = [128, 64];
        app.selection_manager.apply_shape(
            crate::selection::SelectionShape::Rectangle {
                start: Vec2::new(10.0, 10.0),
                end: Vec2::new(50.0, 50.0),
            },
            crate::selection::SelectionMode::Replace,
        );
        app.gradient_press(Vec2::new(0.0, 0.0));
        app.gradient_drag(Vec2::new(128.0, 0.0), false);
        app.gradient_commit();
        assert!(pixel(&app, 30, 30).a() > 0);
        assert_eq!(pixel(&app, 80, 30).a(), 0, "outside the selection");
    }

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn gradient_4k() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(4096, 4096, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.workspace.pool = std::sync::Arc::new(rayon::ThreadPoolBuilder::new().build().unwrap());
        for colors in [
            GradientColors::ForegroundToBackground,
            GradientColors::ForegroundToTransparent,
        ] {
            app.workspace.gradient.settings.colors = colors;
            app.gradient_press(Vec2::new(0.0, 0.0));
            for i in 1..=4 {
                app.gradient_drag(Vec2::new(1000.0 * i as f32, 3000.0), false);
                let t = std::time::Instant::now();
                app.gradient_update();
                app.gradient_uploaded(false);
                eprintln!("{colors:?} repaint {i}: {:?}", t.elapsed());
            }
            app.gradient_cancel();
        }
    }
}
