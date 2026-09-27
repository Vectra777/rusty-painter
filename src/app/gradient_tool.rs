//! The Gradient tool: drag from where the first colour is to where the
//! second is. The gradient shows on the layer as it's dragged and stays
//! adjustable (drag its ends) until it's applied: Enter, a press away from
//! it, or another tool. Esc cancels.

use super::PainterApp;
use crate::canvas::gradient::{Gradient, GradientRepeat, GradientShape, Ramp};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::LayerKind;
use crate::selection::SelectionMask;
use eframe::egui::{self, Color32, Stroke, Vec2};

/// Which colours a gradient runs between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientColors {
    /// Brush colour to secondary colour.
    ForegroundToBackground,
    /// Brush colour fading out.
    ForegroundToTransparent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientSettings {
    pub shape: GradientShape,
    pub colors: GradientColors,
    pub repeat: GradientRepeat,
    pub reverse: bool,
    pub opacity: f32,
    pub dither: bool,
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

#[derive(Default)]
pub struct GradientToolState {
    pub settings: GradientSettings,
    pub session: Option<GradientSession>,
}

/// In screen points.
const HANDLE_HIT: f32 = 12.0;
const TINY: f32 = 2.0;

impl PainterApp {
    /// A press with the Gradient tool at canvas point `pos`.
    pub(crate) fn gradient_press(&mut self, pos: Vec2) {
        let hit = HANDLE_HIT / self.viewport.zoom.max(0.01);
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
        let (bounds, coverage) = match self.selection_manager.get_bounds() {
            Some(b) if self.selection_manager.has_selection() => {
                let bounds = [
                    (b.min.x.floor() as i32 - 1).clamp(0, w),
                    (b.min.y.floor() as i32 - 1).clamp(0, h),
                    (b.max.x.ceil() as i32 + 1).clamp(0, w),
                    (b.max.y.ceil() as i32 + 1).clamp(0, h),
                ];
                let sel = &self.selection_manager;
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
        let Some(session) = self.workspace.gradient.session.as_mut() else {
            return;
        };
        let Some(drag) = session.drag else {
            return;
        };
        let snapped = |from: Vec2, to: Vec2| {
            if !snap {
                return to;
            }
            let d = to - from;
            let step = std::f32::consts::PI / 12.0;
            let a = (d.y.atan2(d.x) / step).round() * step;
            from + Vec2::new(a.cos(), a.sin()) * d.length()
        };
        match drag {
            Drag::End => session.end = snapped(session.start, pos),
            Drag::Start => session.start = snapped(session.end, pos),
        }
        session.dirty = true;
    }

    pub(crate) fn gradient_release(&mut self) {
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
    /// once a frame, so dragging costs one repaint per frame.
    pub(crate) fn gradient_update(&mut self) {
        let dirty = self
            .workspace
            .gradient
            .session
            .as_ref()
            .is_some_and(|s| s.dirty && (s.end - s.start).length() > 0.5);
        if dirty {
            self.gradient_paint();
        }
    }

    /// Settings changed: repaint the gradient being placed.
    pub(crate) fn gradient_settings_changed(&mut self) {
        if let Some(session) = self.workspace.gradient.session.as_mut() {
            session.dirty = true;
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
        let brush = self.brush_state.brush.brush_options.color;
        let model = self.workspace.color_model;
        let convert = |c: Color32| PainterApp::convert_color_for_model(c, model);
        let from = convert(brush);
        let to = match settings.colors {
            GradientColors::ForegroundToBackground => convert(self.brush_state.secondary_color),
            GradientColors::ForegroundToTransparent => {
                let [r, g, b, _] = from.to_srgba_unmultiplied();
                Color32::from_rgba_unmultiplied(r, g, b, 0)
            }
        };
        let ramp = Ramp::new(from, to, self.canvas.blend_space, settings.opacity);
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
        pool.install(|| canvas.paint_over_region(layer, original, row));
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
        if let Some(history) = self.layer_state.histories.get_mut(session.layer) {
            history.push_action(UndoAction {
                tiles,
                selection: None,
                transform: None,
                layer_action: None,
            });
        }
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
    let Some(session) = &app.workspace.gradient.session else {
        return;
    };
    let (a, b) = (to_screen(session.start), to_screen(session.end));
    if app.workspace.gradient.settings.shape == GradientShape::Radial {
        let r = (b - a).length();
        painter.circle_stroke(a, r, Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
        painter.circle_stroke(a, r, Stroke::new(1.0_f32, Color32::from_white_alpha(200)));
    }
    painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::BLACK));
    painter.line_segment([a, b], Stroke::new(1.0_f32, Color32::WHITE));
    let swatch = |p: egui::Pos2, color: Color32| {
        let r = egui::Rect::from_center_size(p, egui::vec2(12.0, 12.0));
        painter.rect_filled(r.expand(1.5), 0.0, Color32::BLACK);
        crate::ui::widgets::paint_swatch(painter, r, color);
    };
    let settings = app.workspace.gradient.settings;
    let from = app.brush_state.brush.brush_options.color;
    let to = match settings.colors {
        GradientColors::ForegroundToBackground => app.brush_state.secondary_color,
        GradientColors::ForegroundToTransparent => Color32::TRANSPARENT,
    };
    let (first, second) = if settings.reverse {
        (to, from)
    } else {
        (from, to)
    };
    swatch(a, first);
    swatch(b, second);
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(app.layer_state.histories[1].stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(pixel(&app, 80, 10).a(), 0, "undone");
    }

    #[test]
    fn cancelling_leaves_the_layer_as_it_was() {
        let mut app = app();
        app.gradient_press(Vec2::new(0.0, 0.0));
        app.gradient_drag(Vec2::new(100.0, 0.0), false);
        app.gradient_update();
        app.gradient_cancel();
        assert_eq!(pixel(&app, 50, 10).a(), 0);
        assert_eq!(app.layer_state.histories[1].stacks().0.len(), 0);
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
                eprintln!("{colors:?} repaint {i}: {:?}", t.elapsed());
            }
            app.gradient_cancel();
        }
    }
}
