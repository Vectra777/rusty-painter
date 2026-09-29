//! Brush strokes at the app level: starting, extending and finishing a
//! stroke on the stroke worker, and getting the canvas back from it
//! ([`PainterApp::canvas_mut`]).

use crate::app::PainterApp;
use crate::app::view::render::ScreenMap;
use crate::brush_engine::brush::StabilizerAlgorithm;
use crate::brush_engine::brush_options::BlendMode;
use crate::brush_engine::stroke_worker::StrokeSetup;
use crate::canvas::Canvas;
use crate::selection::SelectionManager;
use eframe::egui::{self, Color32, Stroke, Vec2};
use std::sync::Arc;

/// A pulled-string stroke's string, as the stroke worker pulls it (the same
/// steps on the same samples), for the canvas overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PulledString {
    /// Where the brush is, and the pen.
    pub tip: Vec2,
    pub pen: Vec2,
    /// The string's length, canvas pixels.
    pub length: f32,
}

impl PainterApp {
    /// Start a stroke whose first dab uses `pressure` (a size factor).
    pub(crate) fn start_stroke_with_pressure(&mut self, pos: Vec2, pressure: f32) {
        if self.is_active_layer_locked() || self.is_active_layer_folder() {
            return;
        }
        // A stroke still running (a second press without a release) is
        // ended first: replacing it would lose its undo step.
        self.finish_stroke();
        // On a vector layer the brush and eraser draw and erase lines.
        if matches!(self.active_tool, crate::app::tools::Tool::Brush)
            && self.vector_stroke_begin(pos, pressure)
        {
            return;
        }
        self.rasterise_text_for_stroke();
        self.rasterise_vector_for_stroke();
        self.mark_action();
        if self.brush_state.brush.brush_options.blend_mode != BlendMode::Eraser {
            let color = self.brush_state.brush.brush_options.color;
            self.brush_state.remember_color(color);
        }
        let selection = self
            .selection_manager
            .has_selection()
            .then(|| SelectionManager::with_shape(self.selection_manager.current_shape.clone()));
        let pos = self.ruler_begin_stroke(pos);
        self.note_curve_start(pos, pressure);
        self.workspace.quickshape.begin(pos);
        let mut brush = self.brush_state.brush.clone();
        brush.second_color = self.brush_state.secondary_color;
        if self.stroke_on_curve() {
            // Smoothing would pull the stroke inside the curve it follows;
            // the assistant keeps it steady anyway.
            brush.stabilizer_algorithm = crate::brush_engine::brush::StabilizerAlgorithm::None;
        }
        // Pixel-perfect dabs aren't stabilised.
        self.brush_state.string = (brush.stabilizer_algorithm == StabilizerAlgorithm::String
            && !brush.pixel_perfect)
            .then(|| {
                let mut settings = brush.stabilizer_settings();
                settings.view_scale = self.viewport.zoom;
                PulledString {
                    tip: pos,
                    pen: pos,
                    length: settings.string_length(),
                }
            });
        let mut symmetry = self.workspace.symmetry;
        // Pixel-perfect dabs sit on pixel centres; so do their mirror
        // images when the axes do (on a half-pixel grid).
        if self.brush_state.brush.pixel_perfect {
            symmetry.center = (symmetry.center * 2.0).round() / 2.0;
        }
        self.stroke_worker.begin(StrokeSetup {
            canvas: Arc::clone(&self.canvas),
            brush,
            selection,
            pool: Arc::clone(&self.workspace.pool),
            layer_idx: self.canvas.active_layer_idx,
            symmetry,
            view_scale: self.viewport.zoom,
            wrap: self.workspace.wrap_around,
        });
        self.brush_state.is_drawing = true;
        self.render_cache.below_cache = None;
        self.stroke_worker
            .sample_tilted(pos, pressure, self.viewport.touch.pen_tilt);
        self.viewport.touch.stroke_started = Some(std::time::Instant::now());
    }

    pub(crate) fn add_stroke_point(&mut self, pos: Vec2, pressure: f32) {
        if self.vector_stroke_add(pos, pressure) {
            return;
        }
        if !self.brush_state.is_drawing {
            return;
        }
        self.workspace.quickshape.sample(pos, self.viewport.zoom);
        for (pos, pressure) in self.guide_samples(pos, pressure) {
            if let Some(s) = self.brush_state.string.as_mut() {
                s.pen = pos;
                s.tip = crate::brush_engine::stabilizer::pull_string(s.tip, pos, s.length);
            }
            self.stroke_worker
                .sample_tilted(pos, pressure, self.viewport.touch.pen_tilt);
        }
    }

    /// The pen lifted (or the stroke must end): a vector line is kept, a
    /// brush stroke finished.
    pub(crate) fn finish_stroke(&mut self) {
        // (A vector line ends first, and says it's no longer drawing.)
        self.vector_stroke_end();
        if self.brush_state.is_drawing {
            self.stroke_worker.end();
        }
        self.brush_state.is_drawing = false;
        self.brush_state.string = None;
        self.render_cache.below_cache = None;
    }

    fn is_active_layer_folder(&self) -> bool {
        self.canvas
            .layers
            .get(self.canvas.active_layer_idx)
            .is_some_and(|l| l.kind == crate::canvas::storage::LayerKind::Group)
    }

    fn is_active_layer_locked(&self) -> bool {
        self.canvas
            .layers
            .get(self.canvas.active_layer_idx)
            .map(|l| l.locked)
            .unwrap_or(false)
    }

    /// Per-frame hand-off from the stroke worker: mark the tiles it painted
    /// for redraw and file finished strokes into their layer's undo history.
    /// Returns whether it still has queued samples to paint.
    pub(crate) fn sync_stroke_worker(&mut self) -> bool {
        for ((tx, ty), rect) in self.stroke_worker.take_dirty() {
            self.mark_tile_damage(tx, ty, rect);
        }
        for mut finished in self.stroke_worker.take_finished() {
            self.attach_stroke_rasterised(&mut finished.undo);
            self.attach_vector_rasterised(&mut finished.undo);
            self.layer_state.history.push_action(finished.undo);
        }
        self.stroke_worker.is_busy()
    }

    /// Wait until every queued sample is painted and its results collected,
    /// without ending an in-progress stroke (for saving/exporting).
    pub(crate) fn settle_strokes(&mut self) {
        self.stroke_worker.wait_idle();
        self.sync_stroke_worker();
    }

    /// End any in-progress stroke and let the worker go idle, which releases
    /// its share of the canvas. Use [`exclusive`] afterwards at call sites that
    /// also borrow other fields; otherwise prefer [`Self::canvas_mut`].
    pub(crate) fn release_canvas(&mut self) {
        self.finish_stroke();
        self.settle_strokes();
    }

    /// Exclusive access to the canvas (see [`Self::release_canvas`]).
    pub(crate) fn canvas_mut(&mut self) -> &mut Canvas {
        self.release_canvas();
        exclusive(&mut self.canvas)
    }
}

/// The pulled string while drawing: the circle the pen moves in without
/// moving the brush, and the string from the brush to the pen.
pub(crate) fn draw_string(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    let Some(s) = app.brush_state.string else {
        return;
    };
    let (tip, pen) = (map.to_screen(s.tip), map.to_screen(s.pen));
    let radius = s.length * map.zoom();
    let ink = Color32::from_rgb(90, 200, 250);
    let shadow = Color32::from_black_alpha(140);
    painter.circle_stroke(tip, radius, Stroke::new(3.0_f32, shadow));
    painter.circle_stroke(tip, radius, Stroke::new(1.0_f32, ink));
    if (s.pen - s.tip).length() > 0.5 {
        painter.line_segment([tip, pen], Stroke::new(3.0_f32, shadow));
        painter.line_segment([tip, pen], Stroke::new(1.0_f32, ink));
    }
    painter.circle_filled(tip, 3.0, ink);
}

/// The canvas behind `canvas`, which must be unshared: call
/// [`PainterApp::release_canvas`] first.
pub(crate) fn exclusive(canvas: &mut Arc<Canvas>) -> &mut Canvas {
    Arc::get_mut(canvas).expect("an idle stroke worker holds no canvas reference")
}

#[cfg(test)]
mod tests {
    use crate::brush_engine::brush::StabilizerAlgorithm;
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    #[test]
    fn a_stroke_mixes_in_the_secondary_colour() {
        use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.secondary_color = Color32::from_rgb(230, 20, 20);
        let brush = &mut app.brush_state.brush;
        brush.brush_options.color = Color32::BLACK;
        brush.brush_options.diameter = 12.0;
        brush.brush_options.hardness = 100.0;
        brush.inputs = vec![InputMapping {
            sensor: Sensor::Pressure,
            setting: DabSetting::ColorMix,
            amount: 1.0,
            ..Default::default()
        }];
        app.start_stroke_with_pressure(Vec2::new(20.0, 32.0), 1.0);
        app.add_stroke_point(Vec2::new(100.0, 32.0), 1.0);
        app.finish_stroke();
        app.release_canvas();
        let c = app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[32 * 64 + 50];
        assert!(c.r() > 200 && c.g() < 40, "the secondary colour: {c:?}");
    }

    /// Layer 1's pixels (a tile never painted counts as transparent).
    fn tiles(app: &crate::app::PainterApp) -> Vec<Vec<Color32>> {
        (0..2)
            .flat_map(|ty| (0..2).map(move |tx| (tx, ty)))
            .map(|(tx, ty)| {
                app.canvas
                    .get_layer_tile_data(1, tx, ty)
                    .unwrap_or_else(|| vec![Color32::TRANSPARENT; 64 * 64])
            })
            .collect()
    }

    #[test]
    fn a_post_corrected_stroke_is_one_undo_step_and_undo_restores_exactly() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let pattern: Vec<Color32> = (0..64 * 64)
            .map(|i| Color32::from_rgba_premultiplied((i % 200) as u8, 30, 60, 255))
            .collect();
        app.canvas_mut().set_layer_tile_data(1, 0, 0, pattern);
        let b = &mut app.brush_state.brush;
        b.brush_options.diameter = 10.0;
        b.stabilizer_algorithm = StabilizerAlgorithm::PostCorrection;
        b.stabilizer_modes.correction = 1.0;
        app.release_canvas();
        let before = tiles(&app);
        let pushes = app.layer_state.history.push_count();

        app.start_stroke_with_pressure(Vec2::new(10.0, 60.0), 1.0);
        for i in 1..60 {
            let wobble = if i % 2 == 0 { 4.0 } else { -4.0 };
            app.add_stroke_point(Vec2::new(10.0 + i as f32 * 1.8, 60.0 + wobble), 1.0);
        }
        app.finish_stroke();
        app.release_canvas();
        let painted = tiles(&app);
        assert!(painted != before, "painted");
        assert_eq!(app.layer_state.history.push_count(), pushes + 1, "one step");

        app.apply_history(false);
        assert!(tiles(&app) == before, "undo restores exactly");
        app.apply_history(true);
        assert!(tiles(&app) == painted, "redo paints it again");
    }

    #[test]
    fn the_pulled_string_is_shown_while_drawing() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let b = &mut app.brush_state.brush;
        b.stabilizer_algorithm = StabilizerAlgorithm::String;
        b.stabilizer_modes.string_length = 20.0;
        app.viewport.zoom = 2.0;
        app.start_stroke_with_pressure(Vec2::new(10.0, 10.0), 1.0);
        app.add_stroke_point(Vec2::new(15.0, 10.0), 1.0);
        let s = app.brush_state.string.expect("a string");
        assert_eq!(
            (s.tip, s.pen, s.length),
            (Vec2::new(10.0, 10.0), Vec2::new(15.0, 10.0), 10.0)
        );
        app.add_stroke_point(Vec2::new(40.0, 10.0), 1.0);
        assert_eq!(app.brush_state.string.unwrap().tip, Vec2::new(30.0, 10.0));
        app.finish_stroke();
        assert!(app.brush_state.string.is_none());
    }
}
