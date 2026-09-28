//! Filters on the active layer (Filter menu). A filter with settings opens
//! a dialog and previews on the layer as its sliders move; OK keeps it as
//! one undo step, Cancel puts the layer back. Inside a selection only the
//! selected pixels change (soft edges blend).

use crate::app::PainterApp;
use crate::canvas::filters::{Filter, MAX_REACH};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::{LayerId, LayerKind, Region};
use crate::selection::SelectionMask;
use eframe::egui;
use std::sync::Arc;

/// A filter being previewed.
pub struct FilterSession {
    pub filter: Filter,
    /// By id: the layers panel stays usable while the dialog is open.
    layer: LayerId,
    /// The area's tiles before the filter (every preview starts from them).
    original: Region,
    /// The selection's coverage over the area, if there's a selection.
    coverage: Option<SelectionMask>,
    /// Needs running again (a setting changed).
    pub dirty: bool,
    /// The last run took long enough to stall a slider drag: wait for the
    /// release before running again.
    slow: bool,
}

/// A run slower than this waits for the slider to be let go.
const LIVE_BUDGET: std::time::Duration = std::time::Duration::from_millis(80);

#[derive(Default)]
pub struct FilterState {
    pub session: Option<FilterSession>,
    /// The adjustment layer whose settings are open.
    pub editing: Option<LayerId>,
}

impl PainterApp {
    /// Start `filter` on the active layer: at once when it has no settings,
    /// else as a previewed session for the dialog.
    pub(crate) fn filter_open(&mut self, filter: Filter) {
        self.filter_cancel();
        let layer = self.canvas.active_layer_idx;
        let Some(l) = self.canvas.layers.get(layer) else {
            return;
        };
        if l.locked || l.kind == LayerKind::Group {
            return;
        }
        let layer_id = l.id;
        self.release_canvas();
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        // The selection's area plus what a blur reads around it, or the
        // whole canvas.
        let (bounds, coverage) = match self.selection_manager.get_bounds() {
            Some(b) if self.selection_manager.has_selection() => {
                let [x0, y0, x1, y1] = self.pixel_bounds(b);
                let bounds = [
                    (x0 - MAX_REACH).max(0),
                    (y0 - MAX_REACH).max(0),
                    (x1 + MAX_REACH).min(w),
                    (y1 + MAX_REACH).min(h),
                ];
                let sel = &self.selection_manager;
                let mask =
                    SelectionMask::rasterize(bounds, |y, x0, out| sel.row_coverage(y, x0, out));
                (bounds, Some(mask))
            }
            _ => ([0, 0, w, h], None),
        };
        let pool = Arc::clone(&self.workspace.pool);
        let original = pool.install(|| self.canvas.capture_region(layer, bounds));
        self.workspace.filter.session = Some(FilterSession {
            filter,
            layer: layer_id,
            original,
            coverage,
            dirty: true,
            slow: false,
        });
        if !filter.has_settings() {
            self.filter_commit();
        }
    }

    /// Run the filter again if its settings changed (once a frame; a slow
    /// filter only once the pointer is up).
    pub(crate) fn filter_update(&mut self, pointer_down: bool) {
        if self
            .workspace
            .filter
            .session
            .as_ref()
            .is_some_and(|s| s.dirty && !(s.slow && pointer_down))
        {
            self.filter_run();
        }
    }

    /// The session's layer index, or `None` (and the session dropped) when
    /// that layer is gone.
    fn filter_layer(&mut self) -> Option<usize> {
        let id = self.workspace.filter.session.as_ref()?.layer;
        let idx = self.canvas.layers.iter().position(|l| l.id == id);
        if idx.is_none() {
            self.workspace.filter.session = None;
        }
        idx
    }

    fn filter_run(&mut self) {
        let Some(layer) = self.filter_layer() else {
            return;
        };
        self.release_canvas();
        let Some(session) = self.workspace.filter.session.as_mut() else {
            return;
        };
        let pool = Arc::clone(&self.workspace.pool);
        let canvas = &self.canvas;
        let [x0, y0, x1, y1] = session.original.bounds;
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let started = std::time::Instant::now();
        pool.install(|| {
            let src = session.original.pixels(canvas.tile_size());
            let out = session.filter.apply(&src, w, h, (x0, y0));
            canvas.replace_region(layer, &session.original, &out, session.coverage.as_ref());
        });
        session.dirty = false;
        session.slow = started.elapsed() > LIVE_BUDGET;
        let area = egui::Rect::from_min_max(
            egui::pos2(x0 as f32, y0 as f32),
            egui::pos2(x1 as f32, y1 as f32),
        );
        self.mark_tiles_in_bounds_dirty(area);
        self.layer_state.thumbnails_dirty = true;
    }

    /// Keep the filter as one undo step.
    pub(crate) fn filter_commit(&mut self) {
        if self
            .workspace
            .filter
            .session
            .as_ref()
            .is_some_and(|s| s.dirty)
        {
            self.filter_run();
        }
        let Some(layer) = self.filter_layer() else {
            return;
        };
        let Some(session) = self.workspace.filter.session.take() else {
            return;
        };
        let pool = Arc::clone(&self.workspace.pool);
        let tiles = pool.install(|| self.canvas.region_snapshots(layer, &session.original));
        if tiles.is_empty() {
            return;
        }
        self.layer_state.history.push_action(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: None,
        });
    }

    /// Put the layer back as it was.
    pub(crate) fn filter_cancel(&mut self) {
        let Some(session) = self.workspace.filter.session.take() else {
            return;
        };
        self.release_canvas();
        self.canvas.restore_region(&session.original);
        let [x0, y0, x1, y1] = session.original.bounds;
        self.mark_tiles_in_bounds_dirty(egui::Rect::from_min_max(
            egui::pos2(x0 as f32, y0 as f32),
            egui::pos2(x1 as f32, y1 as f32),
        ));
        self.layer_state.thumbnails_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::filters::Filter;
    use crate::selection::{SelectionMode, SelectionShape};
    use eframe::egui::{Color32, Vec2};

    /// A 128×64 canvas whose paint layer is solid red.
    fn app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for tx in 0..2 {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, 0, vec![Color32::RED; 64 * 64]);
        }
        app.selection_manager.canvas_size = [128, 64];
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
    fn a_filter_without_settings_applies_as_one_undo_step() {
        let mut app = app();
        app.filter_open(Filter::Invert);
        assert!(app.workspace.filter.session.is_none());
        assert_eq!(pixel(&app, 10, 10), Color32::from_rgb(0, 255, 255));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(pixel(&app, 10, 10), Color32::RED);
    }

    #[test]
    fn the_preview_shows_and_cancel_puts_the_layer_back() {
        let mut app = app();
        app.filter_open(Filter::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        app.filter_update(false);
        assert_eq!(
            pixel(&app, 10, 10),
            Color32::from_rgb(0, 255, 0),
            "previewed"
        );
        app.filter_cancel();
        assert_eq!(pixel(&app, 10, 10), Color32::RED);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    #[test]
    fn changed_settings_are_what_ok_keeps() {
        let mut app = app();
        app.filter_open(Filter::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        app.filter_update(false);
        let session = app.workspace.filter.session.as_mut().unwrap();
        session.filter = Filter::HueSaturation {
            hue: -120.0,
            saturation: 0.0,
            lightness: 0.0,
        };
        session.dirty = true;
        app.filter_commit();
        assert_eq!(pixel(&app, 10, 10), Color32::from_rgb(0, 0, 255));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
    }

    #[test]
    fn only_the_selection_changes() {
        let mut app = app();
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(32.0, 64.0),
            },
            SelectionMode::Replace,
        );
        app.filter_open(Filter::Invert);
        assert_eq!(pixel(&app, 10, 10), Color32::from_rgb(0, 255, 255));
        assert_eq!(pixel(&app, 100, 10), Color32::RED);
    }

    #[test]
    fn a_locked_layer_is_left_alone() {
        let mut app = app();
        app.canvas_mut().layers[1].locked = true;
        app.filter_open(Filter::Invert);
        assert_eq!(pixel(&app, 10, 10), Color32::RED);
    }
}
