use super::{
    PainterApp,
    painter_state::RenderCache,
    state::{CanvasTile, ColorModel, TILE_SIZE},
};
use crate::canvas::Canvas;
use crate::canvas::history::{History, LayerHistoryOp, UndoAction};
use eframe::egui::{self, Color32, Vec2};

impl PainterApp {
    /// Panics in debug builds if the per-layer side-car vecs (undo history,
    /// UI color) have drifted out of sync
    /// with `canvas.layers`. These are kept aligned by convention rather
    /// than by the type system, so any code path that resizes/reorders
    /// `canvas.layers` without going through the paired helpers here would
    /// otherwise corrupt data silently (wrong undo history applied to the
    /// wrong layer, etc). Cheap (length checks only) — safe to call after
    /// every mutation, and compiled out entirely in release builds.
    fn debug_assert_layer_state_in_sync(&self) {
        let layer_count = self.canvas.layers.len();
        debug_assert_eq!(
            self.layer_state.histories.len(),
            layer_count,
            "layer_state.histories desynced from canvas.layers"
        );
        debug_assert_eq!(
            self.layer_state.layer_ui_colors.len(),
            layer_count,
            "layer_state.layer_ui_colors desynced from canvas.layers"
        );
    }

    pub(crate) fn mark_tile_dirty(&mut self, tx: usize, ty: usize) {
        if let Some(tile) = self.tile_mut(tx, ty) {
            tile.dirty = true;
        }
    }

    pub(crate) fn tile_mut(&mut self, tx: usize, ty: usize) -> Option<&mut CanvasTile> {
        if tx >= self.render_cache.tiles_x || ty >= self.render_cache.tiles_y {
            return None;
        }
        let idx = ty * self.render_cache.tiles_x + tx;
        self.render_cache.tiles.get_mut(idx)
    }

    fn rebuild_canvas(
        &mut self,
        width: usize,
        height: usize,
        background: Color32,
    ) {
        self.reset_canvas_state(width, height, background);
        self.recreate_render_cache(width, height);
        self.reset_viewport_state();
    }

    fn reset_canvas_state(&mut self, width: usize, height: usize, background: Color32) {
        *self.canvas_mut() = Canvas::new(width, height, background, TILE_SIZE);
        let layer_count = self.canvas.layers.len();
        self.layer_state.histories = (0..layer_count).map(|_| History::new()).collect();
        self.layer_state.layer_ui_colors = vec![Color32::from_gray(40); layer_count];
        self.layer_state.layer_dragging = None;
    }

    pub(crate) fn recreate_render_cache(&mut self, width: usize, height: usize) {
        let generation = self.render_cache.texture_generation.wrapping_add(1);
        self.render_cache = RenderCache::new(width, height);
        self.render_cache.texture_generation = generation;
        self.brush_state.is_drawing = false;
        self.viewport.is_panning = false;
        self.viewport.is_rotating = false;
        self.viewport.is_primary_down = false;
    }

    fn reset_viewport_state(&mut self) {
        self.viewport.offset = Vec2::ZERO;
        self.viewport.zoom = 1.0;
        self.viewport.rotation = 0.0;
        self.workspace.auto_fit = true;
        self.workspace.fitted_to = None;
    }

    pub(crate) fn apply_new_canvas(&mut self) {
        let Ok((width, height)) = self.modal_state.new_canvas.validated_dimensions() else {
            return;
        };
        self.workspace.color_model = self.modal_state.new_canvas.color_model;
        let background = self
            .modal_state
            .new_canvas
            .background_color32(self.workspace.color_model);
        self.rebuild_canvas(width, height, background);
        self.brush_state.brush.brush_options.color = Self::convert_color_for_model(
            self.brush_state.brush.brush_options.color,
            self.workspace.color_model,
        );
    }

    fn convert_color_for_model(color: Color32, model: ColorModel) -> Color32 {
        match model {
            ColorModel::Rgba => color,
            ColorModel::Grayscale => super::state::to_grayscale(color),
        }
    }

    pub(crate) fn mark_all_tiles_dirty(&mut self) {
        for tile in &mut self.render_cache.tiles {
            tile.dirty = true;
        }
    }

    pub(crate) fn mark_tiles_in_bounds_dirty(&mut self, bounds: egui::Rect) {
        if bounds.is_negative() {
            return;
        }

        let min_x = bounds.min.x.floor().max(0.0) as usize;
        let min_y = bounds.min.y.floor().max(0.0) as usize;
        let max_x = bounds.max.x.ceil().min(self.canvas.width() as f32) as usize;
        let max_y = bounds.max.y.ceil().min(self.canvas.height() as f32) as usize;

        if min_x >= max_x || min_y >= max_y {
            return;
        }

        let min_tx = min_x / TILE_SIZE;
        let max_tx = max_x.saturating_sub(1) / TILE_SIZE;
        let min_ty = min_y / TILE_SIZE;
        let max_ty = max_y.saturating_sub(1) / TILE_SIZE;
        let tiles_x = self.render_cache.tiles_x;

        for ty in min_ty..=max_ty.min(self.render_cache.tiles_y - 1) {
            for tx in min_tx..=max_tx.min(tiles_x - 1) {
                let idx = ty * tiles_x + tx;
                if let Some(tile) = self.render_cache.tiles.get_mut(idx) {
                    tile.dirty = true;
                }
            }
        }
    }

    pub(crate) fn mark_layer_tiles_with_data_dirty(&mut self, layer_idx: usize) {
        let tiles_x = self.render_cache.tiles_x;
        let tiles_y = self.render_cache.tiles_y;
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let has_data = self
                    .canvas
                    .lock_layer_tile_if_exists(layer_idx, tx, ty)
                    .map(|cell_arc| {
                        cell_arc
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .data
                            .is_some()
                    })
                    .unwrap_or(false);
                if has_data {
                    let idx = ty * tiles_x + tx;
                    if let Some(tile) = self.render_cache.tiles.get_mut(idx) {
                        tile.dirty = true;
                    }
                }
            }
        }
    }

    pub(crate) fn reorder_layers(&mut self, from: usize, to: usize) {
        let len = self.canvas.layers.len();
        if from >= len {
            return;
        }
        let to = to.min(len.saturating_sub(1));
        if from == to {
            return;
        }
        let Some(moved_id) = self.canvas.layer_id_at(from) else {
            return;
        };

        let canvas = self.canvas_mut();
        let layer = canvas.layers.remove(from);
        canvas.layers.insert(to, layer);
        self.reorder_layer_state(from, to);

        let active_before = self.canvas.active_layer_idx;
        let active_after = if active_before == from {
            to
        } else if from < active_before && active_before <= to {
            active_before - 1
        } else if to <= active_before && active_before < from {
            active_before + 1
        } else {
            active_before
        };
        self.canvas_mut().active_layer_idx = active_after;

        let action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Moved {
                id: moved_id,
                from,
                to,
                active_before,
                active_after,
            }),
        };
        if let Some(hist) = self.layer_state.histories.get_mut(active_after) {
            hist.push_action(action);
        }

        self.mark_all_tiles_dirty();
        self.debug_assert_layer_state_in_sync();
    }

    /// Move the side-car per-layer state (undo history, render cache,
    /// cache-dirty set, UI color) from `from` to `to`, mirroring a move
    /// already applied to `canvas.layers` itself. Shared by the UI reorder
    /// entry point (`reorder_layers`) and undo/redo of a layer move, which
    /// moves `canvas.layers` itself inside `History::undo`/`redo` and can't
    /// also reach into `LayerState`/`RenderCache` from there.
    pub(crate) fn reorder_layer_state(&mut self, from: usize, to: usize) {
        let hist = self.layer_state.histories.remove(from);
        self.layer_state.histories.insert(to, hist);
        let ui_color = self.layer_state.layer_ui_colors.remove(from);
        self.layer_state.layer_ui_colors.insert(to, ui_color);
        self.debug_assert_layer_state_in_sync();
    }

    pub(crate) fn add_paint_layer(&mut self) {
        let active_before = self.canvas.active_layer_idx;
        let id = self.canvas_mut().add_layer();
        let new_idx = self.canvas.layers.len().saturating_sub(1);
        self.insert_layer_state(new_idx);

        let action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Added {
                index: new_idx,
                id,
                active_before,
                active_after: new_idx,
            }),
        };
        if let Some(hist) = self.layer_state.histories.get_mut(new_idx) {
            hist.push_action(action);
        }
    }

    pub(crate) fn remove_paint_layer(&mut self, idx: usize) {
        if idx >= self.canvas.layers.len() || self.canvas.layers.len() <= 1 || idx == 0 {
            return;
        }

        let active = self.canvas.active_layer_idx;
        self.mark_layer_tiles_with_data_dirty(idx);

        let Some(id) = self.canvas.layer_id_at(idx) else {
            return;
        };
        let Some(meta) = self.canvas.layer_meta_at(idx) else {
            return;
        };
        let tile_snapshots = self.canvas.snapshot_layer_tiles(idx);

        self.canvas_mut().layers.remove(idx);
        self.remove_layer_state(idx);
        let active_after = if active == idx {
            idx.min(self.canvas.layers.len().saturating_sub(1))
        } else if idx < active {
            active.saturating_sub(1)
        } else {
            active.min(self.canvas.layers.len().saturating_sub(1))
        };
        self.canvas_mut().active_layer_idx = active_after;

        let action = UndoAction {
            tiles: tile_snapshots,
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Removed {
                index: idx,
                id,
                meta,
                active_before: active,
                active_after,
            }),
        };
        if let Some(hist) = self.layer_state.histories.get_mut(active_after) {
            hist.push_action(action);
        }

        self.mark_all_tiles_dirty();
    }

    pub(crate) fn insert_layer_state(&mut self, idx: usize) {
        let idx = idx.min(self.canvas.layers.len());
        self.layer_state.histories.insert(idx, History::new());
        self.layer_state
            .layer_ui_colors
            .insert(idx, Color32::from_gray(40));
        self.debug_assert_layer_state_in_sync();
    }

    pub(crate) fn remove_layer_state(&mut self, idx: usize) {
        if idx < self.layer_state.histories.len() {
            self.layer_state.histories.remove(idx);
        }
        if idx < self.layer_state.layer_ui_colors.len() {
            self.layer_state.layer_ui_colors.remove(idx);
        }
        self.debug_assert_layer_state_in_sync();
    }
}
