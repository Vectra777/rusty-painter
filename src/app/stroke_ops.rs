//! Brush strokes at the app level: starting, extending and finishing a
//! stroke on the stroke worker, and getting the canvas back from it
//! ([`PainterApp::canvas_mut`]).

use crate::app::PainterApp;
use crate::app::view::render::ScreenMap;
use crate::brush_engine::brush::StabilizerAlgorithm;
use crate::brush_engine::brush_options::BlendMode;
use crate::brush_engine::stroke_worker::{Finished, StrokeSetup};
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
    /// Layer `idx` has impasto heights (flat to start with, so nothing
    /// shows until paint is laid thick) and a light on them.
    pub(crate) fn ensure_impasto(&mut self, idx: usize) {
        let has = self.canvas.layers.get(idx).is_none_or(|l| {
            l.height.is_some() && l.style.impasto.is_some() && !l.style.lightness_map
                || l.kind != crate::canvas::storage::LayerKind::Paint
        });
        if has {
            return;
        }
        // A lightness map there: into the paint first (its own undo step).
        if self.canvas.layers[idx].shown_style().lightness_map {
            self.bake_impasto(idx);
        }
        let layer = &mut self.canvas_mut().layers[idx];
        layer.style.lightness_map = false;
        layer.height.get_or_insert_with(Default::default);
        layer.style.impasto.get_or_insert_with(Default::default);
    }

    /// Layer `idx` has a lightness map (for Krita's colour smudge with a
    /// lightness tip): impasto heights there are baked into its paint
    /// first.
    pub(crate) fn ensure_lightness_map(&mut self, idx: usize) {
        let has = self.canvas.layers.get(idx).is_none_or(|l| {
            l.height.is_some() && l.style.lightness_map
                || l.kind != crate::canvas::storage::LayerKind::Paint
        });
        if has {
            return;
        }
        if self.canvas.layers[idx].shown_style().impasto.is_some() {
            self.bake_impasto(idx);
        }
        let layer = &mut self.canvas_mut().layers[idx];
        layer.style.lightness_map = true;
        layer.height.get_or_insert_with(Default::default);
    }

    /// Wet paint dries on (between strokes): each layer's, by the time
    /// passed, in steps of [`crate::canvas::wet::STEP`] (at most four a
    /// frame, and fewer when they're slow: see [`WET_BUDGET`]). Returns
    /// whether any is still wet (to keep the frames coming).
    pub(crate) fn wet_tick(&mut self) -> bool {
        let wet: Vec<(usize, std::sync::Arc<crate::canvas::wet::WetLayer>)> =
            (self.canvas.layers.iter())
                .enumerate()
                .filter_map(|(i, l)| l.wet.clone().filter(|w| !w.is_empty()).map(|w| (i, w)))
                .collect();
        if wet.is_empty() {
            self.workspace.wet_clock = None;
            return false;
        }
        // Not while painting (a stroke lays its own paint first).
        if self.brush_state.is_drawing || self.stroke_worker.is_busy() {
            return true;
        }
        let now = std::time::Instant::now();
        let last = self.workspace.wet_clock.unwrap_or(now);
        let steps = ((now - last).as_secs_f64() / crate::canvas::wet::STEP) as usize;
        if steps == 0 {
            self.workspace.wet_clock.get_or_insert(now);
            return true;
        }
        self.workspace.wet_clock = Some(now);
        // As many as fit the frame's budget (at least one: it always dries,
        // slower when there's a lot of it).
        let fit = (WET_BUDGET / self.workspace.wet_step_secs.max(1e-6)) as usize;
        let steps = steps.min(4).min(fit.max(1));
        self.wet_steps(&wet, steps);
        self.workspace.wet_step_secs = now.elapsed().as_secs_f64() / steps as f64;
        true
    }

    /// `steps` steps of drying for the layers `wet`.
    pub(crate) fn wet_steps(
        &mut self,
        wet: &[(usize, std::sync::Arc<crate::canvas::wet::WetLayer>)],
        steps: usize,
    ) {
        let ts = self.canvas.tile_size();
        let (cols, rows) = (
            self.canvas.width().div_ceil(ts) as i32,
            self.canvas.height().div_ceil(ts) as i32,
        );
        let gravity = Vec2::from(self.workspace.wet_gravity);
        // What it does from here on goes into the step on top, so undoing
        // that puts it all back; where that can't be (a layer added,
        // removed, merged...), the water stays on the tiles it's on.
        let tracked = self.wet_into_top_step(wet);
        for (idx, layer) in wet {
            let canvas = &self.canvas;
            let stepped = layer.step(steps, ts, gravity, tracked, |(tx, ty)| {
                ((0..cols).contains(&tx) && (0..rows).contains(&ty)).then(|| {
                    canvas
                        .get_layer_tile_data(*idx, tx, ty)
                        .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts])
                })
            });
            let id = self.canvas.layers[*idx].id;
            // Where it spread to, as it was.
            if tracked && let Some(action) = self.layer_state.history.top_mut() {
                for (key, before) in stepped.fresh {
                    crate::canvas::wet::record_undo(action, id, key, None);
                    note_tile(action, id, key, ts, before);
                }
            }
            for ((tx, ty), pixels) in stepped.shown {
                self.canvas.set_layer_tile_data(*idx, tx, ty, pixels);
                let (x, y) = (tx * ts as i32, ty * ts as i32);
                self.mark_rect_damage([x, y, x + ts as i32, y + ts as i32]);
            }
        }
        self.layer_state.thumbnails_dirty = true;
    }

    /// The step on top takes in the wet paint as it is now (once per step
    /// on top): every wet tile, and its pixels, as they were before what it
    /// goes on doing. Only a step that changes pixels alone (a stroke, a
    /// fill, a filter) takes it. Returns whether the top step has it.
    fn wet_into_top_step(
        &mut self,
        wet: &[(usize, std::sync::Arc<crate::canvas::wet::WetLayer>)],
    ) -> bool {
        use crate::canvas::history::LayerHistoryOp;
        let token = self.layer_state.history.top_token();
        let ts = self.canvas.tile_size();
        let Some(action) = self.layer_state.history.top_mut() else {
            return false;
        };
        if LayerHistoryOp::structural(action.layer_action.as_ref()).is_some() {
            return false;
        }
        if self.workspace.wet_step == Some(token) {
            return true;
        }
        for (idx, layer) in wet {
            let id = self.canvas.layers[*idx].id;
            for key in layer.keys() {
                crate::canvas::wet::record_undo(action, id, key, layer.tile(key));
                if let Some(pixels) = self.canvas.get_layer_tile_data(*idx, key.0, key.1) {
                    note_tile(action, id, key, ts, pixels);
                }
            }
        }
        self.workspace.wet_step = Some(token);
        true
    }

    /// Layer → Dry Paint Now: the active layer's wet paint settles where it
    /// is (it looks the same).
    pub(crate) fn dry_wet_paint(&mut self, idx: usize) {
        let Some(wet) = self.canvas.layers.get(idx).and_then(|l| l.wet.clone()) else {
            return;
        };
        self.release_canvas();
        for ((tx, ty), pixels) in wet.dry_now() {
            self.canvas.set_layer_tile_data(idx, tx, ty, pixels);
        }
        self.mark_all_tiles_dirty();
    }

    /// Layer `idx`'s impasto made plain paint: its pixels lit (or
    /// lightened and darkened by its lightness map) as they show,
    /// its heights put aside (one undo step brings them back). For what
    /// moves its pixels (the transform tool), which the heights wouldn't
    /// follow. Returns whether it had any.
    pub(crate) fn bake_impasto(&mut self, idx: usize) -> bool {
        use crate::canvas::history::{LayerHistoryOp, TileSnapshot, UndoAction};
        let Some(layer) = self.canvas.layers.get(idx) else {
            return false;
        };
        let (style, id) = (layer.shown_style(), layer.id);
        if layer.height.is_none() || style.impasto.is_none() && !style.lightness_map {
            return false;
        }
        self.release_canvas();
        let ts = self.canvas.tile_size();
        let mut tiles = Vec::new();
        for (tx, ty) in self.canvas.layer_tile_keys(idx) {
            let Some(data) = self.canvas.get_layer_tile_data(idx, tx, ty) else {
                continue;
            };
            let mut lit = data.clone();
            if let Some(h) = self.canvas.layers[idx].height.as_deref() {
                crate::canvas::impasto::relief(style, h, &mut lit, tx, ty, ts);
            }
            if lit == data {
                continue;
            }
            self.canvas.set_layer_tile_data(idx, tx, ty, lit);
            tiles.push(TileSnapshot {
                tx,
                ty,
                layer_id: id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: data.into(),
            });
        }
        let map = crate::app::stroke_ops::exclusive(&mut self.canvas).layers[idx]
            .height
            .take();
        self.layer_state.history.label_next("Bake impasto");
        self.push_undo(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Height {
                layer: id,
                tiles: Vec::new(),
                map: Some(map),
                inner: None,
            }),
        });
        self.mark_all_tiles_dirty();
        true
    }

    /// Start a stroke whose first dab uses `pressure` (a size factor).
    pub(crate) fn start_stroke_with_pressure(&mut self, pos: Vec2, pressure: f32) {
        if self.is_active_layer_locked() || self.is_active_layer_folder() {
            return;
        }
        // Painting stops playback (on the frame showing).
        if self.workspace.animation.playing {
            self.workspace.animation.playing = false;
            self.leave_cached_frame();
        }
        // On a moved layer: where the pointer is on its own pixels.
        let pos = self.to_layer_space(pos);
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
        if self.brush_state.brush.impasto.is_some() {
            self.ensure_impasto(self.canvas.active_layer_idx);
        }
        if self.brush_state.brush.wet.is_some() {
            let idx = self.canvas.active_layer_idx;
            if self.canvas.layers.get(idx).is_some_and(|l| l.wet.is_none()) {
                self.canvas_mut().layers[idx].wet = Some(Default::default());
            }
        }
        self.mark_action();
        let erasing = self.brush_state.brush.brush_options.blend_mode == BlendMode::Eraser;
        if !erasing {
            let color = self.brush_state.brush.brush_options.color;
            self.brush_state.remember_color(color);
        }
        // Colour mixing paints through the Smudge tool's engine (it erases
        // like any brush).
        if self.brush_state.brush.mixing.is_some() && !erasing {
            self.mixing_press(pos, pressure);
            // A stroke is on, as far as the rest of the app goes (moves
            // extend it, nothing autosaves meanwhile).
            self.brush_state.is_drawing = self.mixing_stroke();
            return;
        }
        let selection = self
            .selection_manager
            .has_selection()
            .then(|| SelectionManager::with_shape(self.layer_selection_shape()));
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
            perspective: self.perspective_grids(),
        });
        self.brush_state.is_drawing = true;
        self.render_cache.below_cache = None;
        self.stroke_worker.sample_tilted(
            pos,
            pressure,
            self.viewport.touch.pen_tilt,
            self.viewport.touch.pen_barrel,
        );
        self.viewport.touch.stroke_started = Some(std::time::Instant::now());
    }

    pub(crate) fn add_stroke_point(&mut self, pos: Vec2, pressure: f32) {
        let pos = self.to_layer_space(pos);
        if self.vector_stroke_add(pos, pressure) {
            return;
        }
        if self.mixing_stroke() {
            self.blend_drag(pos, pressure);
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
            self.stroke_worker.sample_tilted(
                pos,
                pressure,
                self.viewport.touch.pen_tilt,
                self.viewport.touch.pen_barrel,
            );
        }
    }

    /// The pen lifted (or the stroke must end): a vector line is kept, a
    /// brush stroke finished.
    pub(crate) fn finish_stroke(&mut self) {
        // (A vector line ends first, and says it's no longer drawing.)
        self.vector_stroke_end();
        // A smudge or blur stroke (a mixing brush's too) runs on the stroke
        // worker like a brush stroke: ended the same way, so the worker
        // lets go of the canvas.
        if self.brush_state.blend_stroke.is_some() {
            self.blend_release();
        } else if self.brush_state.is_drawing {
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
        let dirty = self.stroke_worker.take_dirty();
        self.repose_painted(&dirty);
        for ((tx, ty), rect) in dirty {
            self.mark_tile_damage(tx, ty, rect);
        }
        let (finished, ended) = self.stroke_worker.take_finished();
        // Through a queue on the app: a task's result applied below may
        // sync again, and what it collects must still come after the rest.
        self.workspace.jobs.finished.extend(finished);
        while let Some(next) = self.workspace.jobs.finished.front() {
            // A task's result takes the canvas to itself: once the worker
            // is idle (next frame, if it's busy), so nothing waits for it.
            if matches!(next, Finished::Task(_)) && self.stroke_worker.is_busy() {
                break;
            }
            let Some(next) = self.workspace.jobs.finished.pop_front() else {
                break;
            };
            match next {
                Finished::Stroke(mut finished) => {
                    // A stroke begun after the mark: the history stands
                    // where the mark belongs.
                    self.settle_action_mark(finished.seq - 1);
                    self.attach_stroke_rasterised(&mut finished.undo);
                    self.attach_vector_rasterised(&mut finished.undo);
                    self.layer_state.history.push_action(finished.undo);
                }
                Finished::Task(result) => self.apply_task_result(result),
            }
        }
        if self.workspace.jobs.finished.is_empty() {
            self.settle_action_mark(ended);
        }
        self.stroke_worker.is_busy() || !self.workspace.jobs.finished.is_empty()
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

/// Most time a frame spends drying wet paint, seconds.
const WET_BUDGET: f64 = 0.008;

/// Tile `key` of layer `id` as it was (`pixels`) in `action`, unless it
/// has it already (the first is how it was before the step).
fn note_tile(
    action: &mut crate::canvas::history::UndoAction,
    id: crate::canvas::storage::LayerId,
    key: (i32, i32),
    ts: usize,
    pixels: Vec<Color32>,
) {
    if (action.tiles.iter()).any(|t| (t.tx, t.ty) == key && t.layer_id == id) {
        return;
    }
    action.tiles.push(crate::canvas::history::TileSnapshot {
        tx: key.0,
        ty: key.1,
        layer_id: id,
        x0: 0,
        y0: 0,
        width: ts,
        height: ts,
        data: pixels.into(),
    });
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
    fn an_impasto_stroke_lays_paint_thick_lit_and_undoes_and_saves_exactly() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let brush = &mut app.brush_state.brush;
        brush.brush_options.color = Color32::from_rgb(200, 120, 40);
        brush.brush_options.diameter = 16.0;
        brush.brush_options.hardness = 50.0;
        let flat = {
            let mut plain =
                crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
            plain.canvas_mut().active_layer_idx = 1;
            plain.brush_state.brush = app.brush_state.brush.clone();
            plain.start_stroke_with_pressure(Vec2::new(20.0, 32.0), 1.0);
            plain.add_stroke_point(Vec2::new(100.0, 32.0), 1.0);
            plain.finish_stroke();
            plain.release_canvas();
            plain.canvas.flatten().pixels
        };
        let before = app.canvas.flatten().pixels;
        let pushes = app.layer_state.history.push_count();
        app.brush_state.brush.impasto = Some(crate::canvas::impasto::Impasto {
            depth: 0.8,
            ..Default::default()
        });
        app.start_stroke_with_pressure(Vec2::new(20.0, 32.0), 1.0);
        app.add_stroke_point(Vec2::new(100.0, 32.0), 1.0);
        app.finish_stroke();
        app.release_canvas();
        let heights = app.canvas.layers[1]
            .height
            .as_deref()
            .expect("the layer got heights");
        let tile = heights.tile((0, 0)).unwrap();
        assert!(
            tile[32 * 64 + 50] > tile[20 * 64 + 50],
            "thick where it went"
        );
        let lit = app.canvas.flatten().pixels;
        assert!(lit != flat, "the light shows the thickness");
        assert_eq!(
            lit[2 * 128 + 50],
            flat[2 * 128 + 50],
            "away from the paint, the same"
        );
        assert_eq!(app.layer_state.history.push_count(), pushes + 1, "one step");
        // Saved and opened again: the heights come too.
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        assert!(
            loaded.canvas.layers[1].height.as_deref() == app.canvas.layers[1].height.as_deref()
        );
        assert_eq!(loaded.canvas.flatten().pixels, lit);
        // Undone: the colour and the heights as they were.
        app.apply_history(false);
        assert_eq!(app.canvas.flatten().pixels, before);
        assert!(
            app.canvas.layers[1]
                .height
                .as_deref()
                .is_none_or(|h| h.is_empty())
        );
        // Redone: back exactly.
        app.apply_history(true);
        assert_eq!(app.canvas.flatten().pixels, lit);
    }

    #[test]
    fn impasto_bakes_into_paint_and_turns_with_the_image() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let brush = &mut app.brush_state.brush;
        brush.brush_options.diameter = 16.0;
        brush.impasto = Some(crate::canvas::impasto::Impasto {
            depth: 0.8,
            ..Default::default()
        });
        app.start_stroke_with_pressure(Vec2::new(20.0, 20.0), 1.0);
        app.add_stroke_point(Vec2::new(60.0, 20.0), 1.0);
        app.finish_stroke();
        app.release_canvas();
        let lit = app.canvas.flatten().pixels;
        // Baked: it looks the same, as plain paint; undone, the heights are back.
        assert!(app.bake_impasto(1));
        assert!(app.canvas.layers[1].height.is_none());
        assert_eq!(app.canvas.flatten().pixels, lit);
        app.apply_history(false);
        assert!(app.canvas.layers[1].height.is_some());
        assert_eq!(app.canvas.flatten().pixels, lit);
        // Turned a quarter, the heights turn with the paint: the picture is
        // the same picture turned.
        app.apply_image_op(crate::canvas::geometry::ImageOp::RotateCw);
        let turned = app.canvas.flatten();
        let (w, h) = (turned.size[0], turned.size[1]);
        assert_eq!((w, h), (64, 128));
        let mut differ = 0;
        for y in 0..64 {
            for x in 0..128 {
                // (x, y) goes to (h - 1 - y, x).
                let was = lit[y * 128 + x];
                let now = turned.pixels[x * w + (63 - y)];
                differ += (was != now) as usize;
            }
        }
        // The light comes from the same side of the screen, so slopes
        // light differently once turned; the paint itself is where it was.
        let painted = app.canvas.layers[1].height.as_deref().unwrap();
        let tile = painted.tile((0, 0)).unwrap();
        assert!(
            tile[20 * 64 + (63 - 20)] > 0,
            "heights under the turned paint"
        );
        assert!(differ < 64 * 128 / 4);
    }

    /// A wet (or dry) stroke on layer 1 of a 192×64 canvas, after `setup`,
    /// from `a` to `b` (a dab, the same).
    fn wet_stroke(
        app: &mut crate::PainterApp,
        wet: bool,
        a: Vec2,
        b: Vec2,
        setup: impl Fn(&mut crate::brush_engine::brush::Brush),
    ) {
        let brush = &mut app.brush_state.brush;
        brush.brush_options.color = Color32::from_rgb(30, 60, 200);
        brush.brush_options.diameter = 10.0;
        brush.brush_options.hardness = 100.0;
        brush.wet = wet.then_some(crate::canvas::wet::WetPaint {
            lift: 0.0,
            drying: 2.0,
            ..Default::default()
        });
        setup(brush);
        app.start_stroke_with_pressure(a, 1.0);
        if a != b {
            app.add_stroke_point(b, 1.0);
        }
        app.finish_stroke();
        app.settle_strokes();
    }

    fn wet_app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(192, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app
    }

    fn layer_px(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
        app.canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .map_or(Color32::TRANSPARENT, |t| {
                t[((y % 64) * 64 + x % 64) as usize]
            })
    }

    #[test]
    fn wet_paint_keeps_opacity_erasers_alpha_lock_and_what_was_painted_meanwhile() {
        use crate::brush_engine::brush_options::BlendMode;
        let dab = Vec2::new(30.0, 32.0);
        // Half opacity: as see-through wet as dry.
        let half = |b: &mut crate::brush_engine::brush::Brush| b.brush_options.opacity = 0.5;
        let alpha = |wet| {
            let mut app = wet_app();
            wet_stroke(&mut app, wet, dab, dab, half);
            layer_px(&app, 30, 32).a()
        };
        assert!(
            alpha(true).abs_diff(alpha(false)) <= 1,
            "{} {}",
            alpha(true),
            alpha(false)
        );
        // A wet eraser erases.
        let mut app = wet_app();
        let red = vec![Color32::from_rgb(200, 0, 0); 64 * 64];
        app.canvas.set_layer_tile_data(1, 0, 0, red.clone());
        wet_stroke(&mut app, true, dab, dab, |b| {
            b.brush_options.blend_mode = BlendMode::Eraser
        });
        assert_eq!(layer_px(&app, 30, 32).a(), 0, "erased");
        // On an alpha-locked layer, nothing where there was nothing.
        let mut app = wet_app();
        app.canvas_mut().layers[1].alpha_locked = true;
        wet_stroke(&mut app, true, dab, Vec2::new(50.0, 32.0), |_| {});
        let wet = app.canvas.layers[1].wet.clone().unwrap();
        app.wet_steps(&[(1, wet)], 10);
        assert_eq!(layer_px(&app, 40, 32), Color32::TRANSPARENT);
        // Dry paint laid on a wet tile stays when wet paint goes over it.
        let mut app = wet_app();
        wet_stroke(&mut app, true, dab, dab, |_| {});
        wet_stroke(
            &mut app,
            false,
            Vec2::new(10.0, 10.0),
            Vec2::new(10.0, 10.0),
            |b| b.brush_options.color = Color32::from_rgb(0, 200, 0),
        );
        let green = layer_px(&app, 10, 10);
        assert!(green.g() > 150);
        wet_stroke(
            &mut app,
            true,
            Vec2::new(50.0, 50.0),
            Vec2::new(50.0, 50.0),
            |_| {},
        );
        assert_eq!(
            layer_px(&app, 10, 10),
            green,
            "the dry stroke is still there"
        );
    }

    #[test]
    fn wet_paint_spreading_on_an_empty_layer_keeps_its_colour() {
        // Yellow spreading thin over nothing: over white it's a paler
        // yellow, never browner (its red stays full).
        let mut app = wet_app();
        wet_stroke(
            &mut app,
            true,
            Vec2::new(20.0, 32.0),
            Vec2::new(170.0, 32.0),
            |b| {
                b.brush_options.color = Color32::from_rgb(252, 202, 68);
                b.wet = Some(crate::canvas::wet::WetPaint {
                    water: 1.5,
                    flow: 1.0,
                    drying: 3.0,
                    lift: 0.0,
                    ..Default::default()
                });
            },
        );
        let wet = app.canvas.layers[1].wet.clone().unwrap();
        for _ in 0..100 {
            app.wet_steps(&[(1, wet.clone())], 4);
        }
        assert!(wet.is_empty(), "dry");
        let picture = app.canvas.flatten().pixels;
        let thin: Vec<Color32> = (0..64)
            .map(|y| picture[y * 192 + 90])
            .filter(|c| c.b() < 250 && c.b() > 90)
            .collect();
        assert!(!thin.is_empty(), "spread thin somewhere");
        assert!(thin.iter().all(|c| c.r() >= 250), "{thin:?}");
    }

    #[test]
    fn wet_paint_undoes_exactly_with_another_step_between() {
        let mut app = wet_app();
        let before = app.canvas.flatten().pixels;
        // Up to the tile's edge (it spreads into the next), then a dry
        // stroke elsewhere while it's still wet.
        let soaking = |b: &mut crate::brush_engine::brush::Brush| {
            b.wet = Some(crate::canvas::wet::WetPaint {
                water: 1.5,
                flow: 1.0,
                drying: 3.0,
                lift: 0.0,
                ..Default::default()
            })
        };
        wet_stroke(
            &mut app,
            true,
            Vec2::new(30.0, 32.0),
            Vec2::new(57.0, 32.0),
            soaking,
        );
        assert!(
            app.canvas.get_layer_tile_data(1, 1, 0).is_none(),
            "one tile"
        );
        let wet = app.canvas.layers[1].wet.clone().unwrap();
        wet_stroke(
            &mut app,
            false,
            Vec2::new(150.0, 20.0),
            Vec2::new(180.0, 40.0),
            |_| {},
        );
        for _ in 0..60 {
            app.wet_steps(&[(1, wet.clone())], 4);
        }
        assert!(wet.is_empty(), "dry");
        assert!(layer_px(&app, 64, 32) != Color32::TRANSPARENT, "spread on");
        app.apply_history(false);
        app.apply_history(false);
        assert!(app.canvas.flatten().pixels == before, "all gone");
    }

    #[test]
    fn wet_paint_spreads_dries_saves_as_shown_and_undoes_exactly() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let brush = &mut app.brush_state.brush;
        brush.brush_options.color = Color32::from_rgb(30, 60, 200);
        brush.brush_options.diameter = 10.0;
        brush.brush_options.hardness = 100.0;
        brush.wet = Some(crate::canvas::wet::WetPaint {
            drying: 2.0,
            ..Default::default()
        });
        let before = app.canvas.flatten().pixels;
        // Up to the tile's edge: it spreads into the next tile too.
        app.start_stroke_with_pressure(Vec2::new(30.0, 32.0), 1.0);
        app.add_stroke_point(Vec2::new(62.0, 32.0), 1.0);
        app.finish_stroke();
        app.settle_strokes();
        let wet = app.canvas.layers[1].wet.clone().unwrap();
        assert!(!wet.is_empty(), "wet after the stroke");
        let at =
            |app: &crate::PainterApp, x: usize, y: usize| app.canvas.flatten().pixels[y * 128 + x];
        assert_eq!(at(&app, 45, 38), Color32::WHITE, "not past the stroke yet");
        app.wet_steps(&[(1, wet.clone())], 15);
        assert!(at(&app, 45, 38) != Color32::WHITE, "it bled outward");
        assert!(at(&app, 68, 32) != Color32::WHITE, "and into the next tile");
        // Saved while wet: as it shows.
        let shown = app.canvas.flatten().pixels;
        let bytes = crate::project::encode_project(&app).unwrap();
        assert_eq!(
            crate::project::decode_project(&bytes)
                .unwrap()
                .canvas
                .flatten()
                .pixels,
            shown
        );
        // Dries through.
        for _ in 0..40 {
            app.wet_steps(&[(1, wet.clone())], 4);
        }
        assert!(wet.is_empty(), "dry");
        // One undo: the stroke and everything it spread to, gone.
        app.apply_history(false);
        let after = app.canvas.flatten().pixels;
        let diff: Vec<usize> = (0..after.len())
            .filter(|&i| after[i] != before[i])
            .collect();
        assert!(
            diff.is_empty(),
            "{} differ, first at {:?}: {:?}",
            diff.len(),
            diff.first().map(|i| (i % 128, i / 128)),
            diff.first().map(|&i| after[i])
        );
    }

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

    #[test]
    fn a_mixing_brush_smudges_its_colour_into_the_paint_and_undoes() {
        use crate::brush_engine::brush_options::Mixing;
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let red = Color32::from_rgb(230, 30, 20);
        for tx in 0..2 {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, 0, vec![red; 64 * 64]);
        }
        let brush = &mut app.brush_state.brush;
        brush.brush_options.color = Color32::from_rgb(20, 40, 230);
        brush.brush_options.diameter = 20.0;
        brush.brush_options.hardness = 100.0;
        brush.brush_options.pressure_size = false;
        brush.mixing = Some(Mixing {
            color_rate: 0.3,
            ..Default::default()
        });
        app.release_canvas();
        let before = app.layer_state.history.push_count();
        app.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
        assert!(app.brush_state.is_drawing, "a stroke is on");
        for i in 1..=20 {
            app.add_stroke_point(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
        }
        app.finish_stroke();
        assert!(!app.brush_state.is_drawing);
        app.settle_strokes();
        let c = app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[32 * 64 + 60];
        assert!(c.r() > 40 && c.b() > 40, "red and blue mixed: {c:?}");
        assert_eq!(app.layer_state.history.push_count(), before + 1, "one step");
        app.apply_history(false);
        let c = app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[32 * 64 + 60];
        assert_eq!(c, red, "undo restores");
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
