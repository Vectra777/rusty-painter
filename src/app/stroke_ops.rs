//! Brush strokes at the app level: starting, extending and finishing a
//! stroke on the stroke worker, and getting the canvas back from it
//! ([`PainterApp::canvas_mut`]).

use crate::app::PainterApp;
use crate::brush_engine::brush_options::BlendMode;
use crate::brush_engine::stroke_worker::StrokeSetup;
use crate::canvas::Canvas;
use crate::selection::SelectionManager;
use eframe::egui::Vec2;
use std::sync::Arc;

impl PainterApp {
    /// Start a stroke whose first dab uses `pressure` (a size factor).
    pub(crate) fn start_stroke_with_pressure(&mut self, pos: Vec2, pressure: f32) {
        if self.is_active_layer_locked() || self.is_active_layer_folder() {
            return;
        }
        // A stroke still running (a second press without a release) is
        // ended first: replacing it would lose its undo step.
        self.finish_stroke();
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
        let mut brush = self.brush_state.brush.clone();
        if self.stroke_on_curve() {
            // Smoothing would pull the stroke inside the curve it follows;
            // the assistant keeps it steady anyway.
            brush.stabilizer_algorithm = crate::brush_engine::brush::StabilizerAlgorithm::None;
        }
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
        if !self.brush_state.is_drawing {
            return;
        }
        for (pos, pressure) in self.guide_samples(pos, pressure) {
            self.stroke_worker
                .sample_tilted(pos, pressure, self.viewport.touch.pen_tilt);
        }
    }

    pub(crate) fn finish_stroke(&mut self) {
        if self.brush_state.is_drawing {
            self.stroke_worker.end();
        }
        self.brush_state.is_drawing = false;
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
        for finished in self.stroke_worker.take_finished() {
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

/// The canvas behind `canvas`, which must be unshared: call
/// [`PainterApp::release_canvas`] first.
pub(crate) fn exclusive(canvas: &mut Arc<Canvas>) -> &mut Canvas {
    Arc::get_mut(canvas).expect("an idle stroke worker holds no canvas reference")
}
