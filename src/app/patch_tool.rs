//! Smart patch: fill the selection with texture synthesized from its
//! surroundings (content-aware fill), or paint over something with the
//! selection brush in smart patch mode and it's filled on release.
//!
//! The synthesis runs on a background thread (with progress and cancel);
//! the canvas takes no edits meanwhile. The result is blended in through
//! the selection's soft edge, as one undo step.

use super::PainterApp;
use crate::canvas::blend::{color32_to_linear, rgba_to_color32_fast};
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::canvas::inpaint::{self, Pixel, Problem};
use crate::canvas::storage::{LayerId, LayerKind};
use crate::selection::SelectionMask;
use eframe::egui::{self, Rgba};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Surroundings sampled around the hole: its size, within these bounds.
const MIN_MARGIN: i32 = 32;
const MAX_MARGIN: i32 = 480;

struct PatchTask {
    handle: std::thread::JoinHandle<Option<Vec<Pixel>>>,
    cancel: Arc<AtomicBool>,
    /// Progress 0..=1000.
    progress: Arc<AtomicU32>,
    layer_id: LayerId,
    /// The area synthesized (canvas pixels) and the hole's coverage in it.
    crop: [i32; 4],
    coverage: SelectionMask,
}

#[derive(Default)]
pub struct PatchState {
    /// Synthesize from everything visible (otherwise the active layer).
    pub sample_all: bool,
    /// The selection brush fills what it paints on release.
    pub smart_patch: bool,
    task: Option<PatchTask>,
}

impl PainterApp {
    /// Whether a fill is running (the canvas takes no edits meanwhile).
    pub(crate) fn patch_running(&self) -> bool {
        self.workspace.patch.task.is_some()
    }

    /// Progress of the running fill, 0..1.
    pub(crate) fn patch_progress(&self) -> Option<f32> {
        let task = self.workspace.patch.task.as_ref()?;
        Some(task.progress.load(Ordering::Relaxed) as f32 / 1000.0)
    }

    /// Cancel a running fill and forget it (the document is going away).
    pub(crate) fn patch_abandon(&mut self) {
        if let Some(task) = self.workspace.patch.task.take() {
            task.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn patch_cancel(&mut self) {
        if let Some(task) = &self.workspace.patch.task {
            task.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Fill the selection from its surroundings.
    pub(crate) fn content_aware_fill(&mut self) {
        let Some(bounds) = self.selection_manager.get_bounds() else {
            return;
        };
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let area = [
            (bounds.min.x.floor() as i32 - 1).clamp(0, w),
            (bounds.min.y.floor() as i32 - 1).clamp(0, h),
            (bounds.max.x.ceil() as i32 + 1).clamp(0, w),
            (bounds.max.y.ceil() as i32 + 1).clamp(0, h),
        ];
        let sel = &self.selection_manager;
        let mask = SelectionMask::rasterize(area, |y, x0, out| sel.row_coverage(y, x0, out));
        self.fill_hole(mask);
    }

    /// Start synthesizing `hole` (coverage > 0 is filled; partial coverage
    /// blends at the edge).
    pub(crate) fn fill_hole(&mut self, hole: SelectionMask) {
        if self.patch_running() {
            return;
        }
        let Some(hole) = hole.cropped() else {
            return;
        };
        let layer_idx = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(layer_idx) else {
            return;
        };
        if layer.locked || layer.kind != LayerKind::Paint {
            return;
        }
        let layer_id = layer.id;
        self.release_canvas();
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let margin = (hole.w.max(hole.h) as i32).clamp(MIN_MARGIN, MAX_MARGIN);
        let crop = [
            (hole.x0 - margin).max(0),
            (hole.y0 - margin).max(0),
            (hole.x0 + hole.w as i32 + margin).min(cw),
            (hole.y0 + hole.h as i32 + margin).min(ch),
        ];
        let (x0, y0) = (crop[0], crop[1]);
        let (w, h) = ((crop[2] - x0) as usize, (crop[3] - y0) as usize);
        let source = (!self.workspace.patch.sample_all).then_some(layer_idx);
        let pixels = self
            .canvas
            .render_reference(source, x0, y0, w, h)
            .into_iter()
            .map(|c| {
                let l = color32_to_linear(c);
                [l.r(), l.g(), l.b(), l.a()]
            })
            .collect();
        let hole_px = (0..w * h)
            .map(|i| hole.value(x0 + (i % w) as i32, y0 + (i / w) as i32) > 0)
            .collect();
        let problem = Problem {
            w,
            h,
            pixels,
            hole: hole_px,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0));
        let pool = Arc::clone(&self.workspace.pool);
        let (c, p) = (Arc::clone(&cancel), Arc::clone(&progress));
        let handle = std::thread::spawn(move || {
            pool.install(|| {
                inpaint::inpaint(&problem, &c, &|f| {
                    p.store((f.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed)
                })
            })
        });
        self.workspace.patch.task = Some(PatchTask {
            handle,
            cancel,
            progress,
            layer_id,
            crop,
            coverage: hole,
        });
    }

    /// Once a frame: put a finished fill on the layer. Returns whether one
    /// is still running (keep repainting for the progress).
    pub(crate) fn poll_patch(&mut self) -> bool {
        let finished = self
            .workspace
            .patch
            .task
            .as_ref()
            .is_some_and(|t| t.handle.is_finished());
        if !finished {
            return self.patch_running();
        }
        let Some(task) = self.workspace.patch.task.take() else {
            return false;
        };
        let Ok(Some(result)) = task.handle.join() else {
            return false;
        };
        let Some(layer_idx) = self.canvas.layer_index_of(task.layer_id) else {
            return false;
        };
        self.release_canvas();
        // Only the hole's rows and columns are written.
        let m = &task.coverage;
        let (hx, hy, hw, hh) = (m.x0, m.y0, m.w, m.h);
        let [cx0, cy0, cx1, _] = task.crop;
        let cw = (cx1 - cx0) as usize;
        let original = self
            .canvas
            .render_reference(Some(layer_idx), hx, hy, hw, hh);
        let pixels: Vec<egui::Color32> = (0..hw * hh)
            .map(|i| {
                let (x, y) = (hx + (i % hw) as i32, hy + (i / hw) as i32);
                let cov = m.value(x, y) as f32 / 255.0;
                let orig = original[i];
                if cov <= 0.0 {
                    return orig;
                }
                let s = result[(y - cy0) as usize * cw + (x - cx0) as usize];
                let o = color32_to_linear(orig);
                let mix = |a: f32, b: f32| b + (a - b) * cov;
                let out = Rgba::from_rgba_premultiplied(
                    mix(s[0], o.r()),
                    mix(s[1], o.g()),
                    mix(s[2], o.b()),
                    mix(s[3], o.a()),
                );
                let mut px = rgba_to_color32_fast(out);
                if self.canvas.layers[layer_idx].alpha_locked {
                    px = crate::canvas::blend::with_alpha_of(px, orig.a());
                }
                px
            })
            .collect();
        let mut before: HashMap<(i32, i32), Vec<egui::Color32>> = HashMap::new();
        self.canvas
            .write_layer_region(layer_idx, (hx, hy, hw, hh), &pixels, Some(&mut before));
        let ts = self.canvas.tile_size();
        let tiles: Vec<TileSnapshot> = before
            .into_iter()
            .map(|((tx, ty), data)| TileSnapshot {
                tx,
                ty,
                layer_id: task.layer_id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: data.into(),
            })
            .collect();
        if !tiles.is_empty()
            && let Some(history) = self.layer_state.histories.get_mut(layer_idx)
        {
            history.push_action(UndoAction {
                tiles,
                selection: None,
                transform: None,
                layer_action: None,
            });
        }
        self.mark_rect_damage([hx, hy, hx + hw as i32, hy + hh as i32]);
        self.layer_state.thumbnails_dirty = true;
        false
    }

    /// Wait for a running fill and put it on the layer (tests, saving).
    #[cfg(test)]
    pub(crate) fn finish_patch_for_test(&mut self) {
        while self
            .workspace
            .patch
            .task
            .as_ref()
            .is_some_and(|t| !t.handle.is_finished())
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        self.poll_patch();
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    #[test]
    fn filling_a_selection_removes_what_was_in_it_as_one_step() {
        // A grey layer with a red blob; select the blob and fill it.
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let grey = Color32::from_rgb(120, 120, 120);
        let red = Color32::from_rgb(220, 20, 20);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let mut tile = vec![grey; 64 * 64];
            if (tx, ty) == (0, 0) {
                for y in 40..60 {
                    for x in 40..60 {
                        tile[y * 64 + x] = red;
                    }
                }
            }
            app.canvas_mut().set_layer_tile_data(1, tx, ty, tile);
        }
        app.selection_manager.canvas_size = [128, 128];
        app.selection_manager.apply_shape(
            crate::selection::SelectionShape::Rectangle {
                start: Vec2::new(36.0, 36.0),
                end: Vec2::new(64.0, 64.0),
            },
            crate::selection::SelectionMode::Replace,
        );
        app.content_aware_fill();
        assert!(app.patch_running());
        app.finish_patch_for_test();
        let tile = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        let px = tile[50 * 64 + 50];
        assert!(
            (px.r() as i32 - 120).abs() <= 3 && (px.g() as i32 - 120).abs() <= 3,
            "the red is gone: {px:?}"
        );
        assert_eq!(app.layer_state.histories[1].stacks().0.len(), 1);
        app.apply_history(false);
        let tile = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(tile[50 * 64 + 50], red, "undone");
    }

    #[test]
    fn smart_patch_fills_what_the_brush_paints_and_keeps_the_selection() {
        use crate::selection::{SelectionMode, SelectionType};
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let grey = Color32::from_rgb(90, 90, 90);
        let blue = Color32::from_rgb(20, 20, 220);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let mut tile = vec![grey; 64 * 64];
            if (tx, ty) == (1, 1) {
                for y in 10..20 {
                    for x in 10..20 {
                        tile[y * 64 + x] = blue;
                    }
                }
            }
            app.canvas_mut().set_layer_tile_data(1, tx, ty, tile);
        }
        app.selection_manager.canvas_size = [128, 128];
        app.workspace.patch.smart_patch = true;
        app.selection_manager.brush_radius = 12.0;
        app.select_press(
            Vec2::new(79.0, 79.0),
            SelectionType::Brush,
            SelectionMode::Add,
        );
        app.select_move(Vec2::new(80.0, 80.0));
        app.select_release();
        app.finish_patch_for_test();
        let tile = app.canvas.get_layer_tile_data(1, 1, 1).unwrap();
        let px = tile[15 * 64 + 15];
        assert!((px.b() as i32 - 90).abs() <= 4, "the blue is gone: {px:?}");
        assert!(
            !app.selection_manager.has_selection(),
            "the selection is as it was"
        );
    }
}
