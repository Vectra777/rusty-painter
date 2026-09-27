//! Document-level operations on the app: marking tiles for redraw,
//! replacing the document (new canvas, opened project), and layer
//! add/remove/move/merge with the per-layer state kept in step.

use crate::app::{
    PainterApp,
    document::{CanvasTile, ColorModel, TILE_SIZE},
    state::{LayerState, RenderCache},
};
use crate::canvas::Canvas;
use crate::canvas::history::{History, LayerHistoryOp, RemovedLayer, TileSnapshot, UndoAction};
use crate::canvas::storage::{LayerId, LayerKind};
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
            self.layer_state.layer_ui_colors.len(),
            layer_count,
            "layer_state.layer_ui_colors desynced from canvas.layers"
        );
    }

    /// Only `rect` (tile-local pixels) of tile `(tx, ty)` changed.
    pub(crate) fn mark_tile_damage(
        &mut self,
        tx: usize,
        ty: usize,
        rect: crate::app::document::TileRect,
    ) {
        if let Some(tile) = self.tile_mut(tx, ty) {
            tile.mark_rect(rect);
        }
    }

    /// Mark exactly the canvas pixels `[x0, y0, x1, y1)` changed: each tile
    /// gets just its part as damage, so the display recomposites and
    /// uploads that part rather than whole tiles (as brush strokes do).
    pub(crate) fn mark_rect_damage(&mut self, rect: [i32; 4]) {
        let ts = TILE_SIZE as i32;
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let [x0, y0, x1, y1] = [
            rect[0].max(0),
            rect[1].max(0),
            rect[2].min(w),
            rect[3].min(h),
        ];
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for ty in y0 / ts..=(y1 - 1) / ts {
            for tx in x0 / ts..=(x1 - 1) / ts {
                let (ox, oy) = (tx * ts, ty * ts);
                let local = [
                    (x0.max(ox) - ox) as usize,
                    (y0.max(oy) - oy) as usize,
                    (x1.min(ox + ts) - ox) as usize,
                    (y1.min(oy + ts) - oy) as usize,
                ];
                self.mark_tile_damage(tx as usize, ty as usize, local);
            }
        }
    }

    pub(crate) fn mark_tile_dirty(&mut self, tx: usize, ty: usize) {
        if let Some(tile) = self.tile_mut(tx, ty) {
            tile.mark_full();
        }
    }

    pub(crate) fn tile_mut(&mut self, tx: usize, ty: usize) -> Option<&mut CanvasTile> {
        if tx >= self.render_cache.tiles_x || ty >= self.render_cache.tiles_y {
            return None;
        }
        let idx = ty * self.render_cache.tiles_x + tx;
        self.render_cache.tiles.get_mut(idx)
    }

    fn rebuild_canvas(&mut self, width: usize, height: usize, background: Color32) {
        let canvas = Canvas::new(width, height, background, TILE_SIZE);
        self.replace_document(canvas, History::new());
    }

    /// Make `canvas` (with its undo `history`) the document, for a
    /// new canvas or an opened project.
    ///
    /// Everything tied to the old document goes, in this order:
    /// 1. background work holding it: the stroke worker is finished and a
    ///    running content-aware fill is cancelled (its result would land in
    ///    whichever new layer shares an id);
    /// 2. tool sessions are dropped, not applied (a floating transform or
    ///    liquify refers to old layer indices; a gradient or shape preview
    ///    holds old tiles);
    /// 3. the canvas, fresh per-layer state and render cache are installed;
    /// 4. the selection is cleared and the view refits.
    pub(crate) fn replace_document(&mut self, canvas: Canvas, history: History) {
        // 1. Background work.
        self.release_canvas();
        self.patch_abandon();
        // 2. Tool sessions.
        self.end_tool_sessions();
        // 3. The document and what mirrors its layers.
        let (width, height) = (canvas.width(), canvas.height());
        *self.canvas_mut() = canvas;
        let old = std::mem::replace(
            &mut self.layer_state,
            LayerState::new(self.canvas.layers.len()),
        );
        // The thumbnails may have been updated this frame.
        let retired = &mut self.workspace.retired_textures;
        retired.extend(old.thumbnails.into_iter().flatten());
        retired.extend(old.float_overlay.map(|o| o.texture));
        self.layer_state.history = history;
        self.recreate_render_cache(width, height);
        // 4. Selection and view.
        self.selection_manager.clear_selection();
        self.selection_manager.canvas_size = [width, height];
        self.reset_viewport_state();
    }

    /// Drop every in-progress tool session without applying it.
    fn end_tool_sessions(&mut self) {
        self.selection_manager.clear_selection();
        self.select_cancel();
        self.forget_last_pick();
        self.brush_state.blend_stroke = None;
        let ws = &mut self.workspace;
        ws.shapes.session = None;
        ws.gradient.session = None;
        ws.fill.path.clear();
        ws.guides.end_drag();
        self.viewport.touch.pen_on_canvas = false;
        self.viewport.touch.action_mark = None;
        // Transform and liquify sessions live in `layer_state`, which the
        // caller replaces.
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

    pub(crate) fn reset_viewport_state(&mut self) {
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
        self.canvas_mut().blend_space = self.modal_state.new_canvas.blend_space;
        self.brush_state.brush.brush_options.color = Self::convert_color_for_model(
            self.brush_state.brush.brush_options.color,
            self.workspace.color_model,
        );
    }

    pub(crate) fn convert_color_for_model(color: Color32, model: ColorModel) -> Color32 {
        match model {
            ColorModel::Rgba => color,
            ColorModel::Grayscale => crate::app::document::to_grayscale(color),
        }
    }

    pub(crate) fn mark_all_tiles_dirty(&mut self) {
        for tile in &mut self.render_cache.tiles {
            tile.mark_full();
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
                    tile.mark_full();
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
                        tile.mark_full();
                    }
                }
            }
        }
    }

    /// Move layer `from` so it ends at position `to` (as `Vec::remove` then
    /// `insert`) inside folder `parent`, with undo. The background (index 0)
    /// stays at the bottom, and a folder can't move into itself.
    pub(crate) fn move_layer(&mut self, from: usize, to: usize, parent: Option<LayerId>) {
        let len = self.canvas.layers.len();
        if from == 0 || from >= len {
            return;
        }
        let to = to.clamp(1, len - 1);
        let Some(moved_id) = self.canvas.layer_id_at(from) else {
            return;
        };
        let parent_before = self.canvas.layers[from].parent;
        if from == to && parent_before == parent {
            return;
        }
        if let Some(p) = parent
            && self.canvas.is_within(p, moved_id)
        {
            return;
        }

        let canvas = self.canvas_mut();
        let mut layer = canvas.layers.remove(from);
        layer.parent = parent;
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
                parent_before,
                parent_after: parent,
                active_before,
                active_after,
            }),
        };
        self.layer_state.history.push_action(action);

        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
        self.debug_assert_layer_state_in_sync();
    }

    /// Move the side-car per-layer state (UI color) from `from` to `to`,
    /// mirroring a move
    /// already applied to `canvas.layers` itself. Shared by the UI reorder
    /// entry point (`reorder_layers`) and undo/redo of a layer move, which
    /// moves `canvas.layers` itself inside `History::undo`/`redo` and can't
    /// also reach into `LayerState`/`RenderCache` from there.
    pub(crate) fn reorder_layer_state(&mut self, from: usize, to: usize) {
        let ui_color = self.layer_state.layer_ui_colors.remove(from);
        self.layer_state.layer_ui_colors.insert(to, ui_color);
        self.debug_assert_layer_state_in_sync();
    }

    /// "`base` N" with N one more than the highest number already used.
    fn next_layer_name(&self, base: &str) -> String {
        let prefix = format!("{base} ");
        let highest = self
            .canvas
            .layers
            .iter()
            .filter_map(|l| l.name.strip_prefix(&prefix)?.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        format!("{base} {}", highest + 1)
    }

    /// Where a new entry goes to sit just above the selected one: in the
    /// same folder, or at the top of the selected folder when `into_folder`.
    pub(crate) fn insertion_point(&self, into_folder: bool) -> (usize, Option<LayerId>) {
        let active = self
            .canvas
            .active_layer_idx
            .min(self.canvas.layers.len() - 1);
        let layer = &self.canvas.layers[active];
        match layer.kind {
            LayerKind::Group if into_folder => {
                let top_child = self
                    .canvas
                    .layers
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.parent == Some(layer.id))
                    .map(|(i, _)| i + 1)
                    .max();
                (top_child.unwrap_or(active), Some(layer.id))
            }
            LayerKind::Mask { owner } => {
                let owner_idx = self.canvas.layer_index_of(owner).unwrap_or(active);
                (owner_idx + 1, self.canvas.layers[owner_idx].parent)
            }
            _ => (active + 1, layer.parent),
        }
    }

    /// A new paint layer just above the selected one holding `tiles`
    /// (whole tiles by coordinate), selected, as one undo step: undo takes
    /// the layer away, redo brings it back with its pixels. `setup` adjusts
    /// the layer (opacity, blend...) before it's recorded. Returns its index.
    pub(crate) fn add_layer_with_tiles(
        &mut self,
        name: String,
        tiles: Vec<((i32, i32), Vec<Color32>)>,
        setup: impl FnOnce(&mut crate::canvas::storage::Layer),
    ) -> Option<usize> {
        // Leave any running session first; it would target the old layer.
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.release_canvas();
        let active_before = self.canvas.active_layer_idx;
        let (index, parent) = self.insertion_point(true);
        let id = self
            .canvas_mut()
            .insert_new_layer(index, name, LayerKind::Paint, parent);
        let idx = self.canvas.layer_index_of(id)?;
        setup(&mut self.canvas_mut().layers[idx]);
        self.insert_layer_state(idx);
        self.canvas_mut().active_layer_idx = idx;
        let ts = self.canvas.tile_size();
        let snapshots = tiles
            .into_iter()
            .map(|((tx, ty), data)| {
                self.canvas.set_layer_tile_data(idx, tx, ty, data);
                TileSnapshot {
                    tx,
                    ty,
                    layer_id: id,
                    x0: 0,
                    y0: 0,
                    width: ts,
                    height: ts,
                    data: vec![Color32::TRANSPARENT; ts * ts].into(),
                }
            })
            .collect();
        self.layer_state.history.push_action(UndoAction {
            tiles: snapshots,
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Added {
                index: idx,
                id,
                meta: self.canvas.layer_meta_at(idx),
                active_before,
                active_after: idx,
            }),
        });
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
        Some(idx)
    }

    /// Insert a new entry with undo; returns its index.
    pub(crate) fn insert_entry(
        &mut self,
        index: usize,
        name: String,
        kind: LayerKind,
        parent: Option<LayerId>,
        select: bool,
    ) -> usize {
        let active_before = self.canvas.active_layer_idx;
        let id = self
            .canvas_mut()
            .insert_new_layer(index, name, kind, parent);
        let idx = self.canvas.layer_index_of(id).unwrap_or(index);
        self.insert_layer_state(idx);
        let active_after = if select {
            idx
        } else if active_before >= idx {
            active_before + 1
        } else {
            active_before
        };
        self.canvas_mut().active_layer_idx = active_after;

        let action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Added {
                index: idx,
                id,
                meta: self.canvas.layer_meta_at(idx),
                active_before,
                active_after,
            }),
        };
        self.layer_state.history.push_action(action);
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
        idx
    }

    /// Add a paint layer above the selected one (inside a selected folder)
    /// and select it.
    pub(crate) fn add_layer_and_select(&mut self) {
        let (index, parent) = self.insertion_point(true);
        let name = self.next_layer_name("Layer");
        self.insert_entry(index, name, LayerKind::Paint, parent, true);
    }

    /// Add an empty folder above the selected layer and select it.
    pub(crate) fn add_folder(&mut self) {
        let (index, parent) = self.insertion_point(false);
        let name = self.next_layer_name("Folder");
        self.insert_entry(index, name, LayerKind::Group, parent, true);
    }

    /// Give the selected paint layer a mask (showing everything) and select
    /// the mask for painting; selects the existing mask if there is one.
    pub(crate) fn add_mask_to_active(&mut self) {
        let active = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(active) else {
            return;
        };
        if layer.kind != LayerKind::Paint || active == 0 {
            return;
        }
        let owner = layer.id;
        if let Some(mask) = self.canvas.mask_index_of(owner) {
            self.canvas_mut().active_layer_idx = mask;
            return;
        }
        let name = format!("{} mask", layer.name);
        // Masks go on top of the list; their position doesn't matter (they
        // follow their owner by id), and it keeps them off index 0.
        let end = self.canvas.layers.len();
        self.insert_entry(end, name, LayerKind::Mask { owner }, None, true);
    }

    /// Delete a layer with undo, together with its mask, or a folder with
    /// everything inside it.
    pub(crate) fn remove_layer(&mut self, idx: usize) {
        let len = self.canvas.layers.len();
        if idx == 0 || idx >= len || len <= 1 {
            return;
        }
        let Some(id) = self.canvas.layer_id_at(idx) else {
            return;
        };
        let Some(meta) = self.canvas.layer_meta_at(idx) else {
            return;
        };

        // Everything that goes with it: nested layers (for a folder) and the
        // masks of every removed layer.
        let mut doomed: Vec<usize> = (0..len)
            .filter(|&i| i != idx && self.canvas.is_within(self.canvas.layers[i].id, id))
            .collect();
        let owners: Vec<LayerId> = std::iter::once(idx)
            .chain(doomed.iter().copied())
            .map(|i| self.canvas.layers[i].id)
            .collect();
        for owner in owners {
            if let Some(mask) = self.canvas.mask_index_of(owner)
                && mask != idx
                && !doomed.contains(&mask)
            {
                doomed.push(mask);
            }
        }
        doomed.sort_unstable();

        let also: Vec<RemovedLayer> = doomed
            .iter()
            .filter_map(|&i| {
                Some(RemovedLayer {
                    index: i,
                    id: self.canvas.layer_id_at(i)?,
                    meta: self.canvas.layer_meta_at(i)?,
                })
            })
            .collect();
        let mut tiles = self.canvas.snapshot_layer_tiles(idx);
        for &i in &doomed {
            tiles.extend(self.canvas.snapshot_layer_tiles(i));
        }

        // Pick what's selected afterwards, by id: the selected layer if it
        // survives, else the owner of a deleted mask, else what was below.
        let active = self.canvas.active_layer_idx;
        let removed: Vec<usize> = std::iter::once(idx).chain(doomed.iter().copied()).collect();
        let keep_id = if !removed.contains(&active) {
            self.canvas.layer_id_at(active)
        } else if let LayerKind::Mask { owner } = meta.kind {
            Some(owner)
        } else {
            (0..idx)
                .rev()
                .find(|i| !removed.contains(i))
                .and_then(|i| self.canvas.layer_id_at(i))
        };

        for &i in &removed {
            self.mark_layer_tiles_with_data_dirty(i);
        }
        let mut descending = removed.clone();
        descending.sort_unstable_by(|a, b| b.cmp(a));
        for i in descending {
            self.canvas_mut().layers.remove(i);
            self.remove_layer_state(i);
        }
        let active_after = keep_id
            .and_then(|k| self.canvas.layer_index_of(k))
            .unwrap_or(0)
            .min(self.canvas.layers.len().saturating_sub(1));
        self.canvas_mut().active_layer_idx = active_after;

        let action = UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Removed {
                index: idx,
                id,
                meta,
                also,
                active_before: active,
                active_after,
            }),
        };
        self.layer_state.history.push_action(action);

        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    pub(crate) fn insert_layer_state(&mut self, idx: usize) {
        let idx = idx.min(self.canvas.layers.len());
        self.layer_state
            .layer_ui_colors
            .insert(idx, Color32::from_gray(40));
        self.debug_assert_layer_state_in_sync();
    }

    pub(crate) fn remove_layer_state(&mut self, idx: usize) {
        if idx < self.layer_state.layer_ui_colors.len() {
            self.layer_state.layer_ui_colors.remove(idx);
        }
        self.debug_assert_layer_state_in_sync();
    }

    /// Mirror several entries inserted into `canvas.layers` at once (a layer
    /// with its mask, a folder with its contents). `ascending` are their
    /// final positions; the per-layer state is only in sync after all of them.
    pub(crate) fn insert_layer_states(&mut self, ascending: &[usize]) {
        for &idx in ascending {
            let idx = idx.min(self.layer_state.layer_ui_colors.len());
            self.layer_state
                .layer_ui_colors
                .insert(idx, Color32::from_gray(40));
        }
        self.debug_assert_layer_state_in_sync();
    }

    /// Mirror several entries removed from `canvas.layers` at once, given
    /// their positions before removal.
    pub(crate) fn remove_layer_states(&mut self, indices: &[usize]) {
        let mut descending = indices.to_vec();
        descending.sort_unstable_by(|a, b| b.cmp(a));
        for idx in descending {
            if idx < self.layer_state.layer_ui_colors.len() {
                self.layer_state.layer_ui_colors.remove(idx);
            }
        }
        self.debug_assert_layer_state_in_sync();
    }
}

#[cfg(test)]
mod document_tests {
    use crate::canvas::Canvas;
    use crate::project::tests::test_app_pub;
    use eframe::egui::{self, Color32, Vec2};

    /// A 256 px app with layer 1 active.
    fn app() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.selection_manager.canvas_size = [256, 256];
        app
    }

    /// Start a new 128 px canvas the way the New Canvas dialog does.
    fn new_canvas(app: &mut crate::PainterApp) {
        app.modal_state.new_canvas.width = 128.0;
        app.modal_state.new_canvas.height = 128.0;
        app.modal_state.new_canvas.unit = crate::app::document::CanvasUnit::Pixels;
        app.apply_new_canvas();
    }

    #[test]
    fn a_new_canvas_drops_the_old_documents_selection_and_sessions() {
        let mut app = app();
        app.select_all();
        app.gradient_press(Vec2::new(0.0, 0.0));
        app.gradient_drag(Vec2::new(200.0, 0.0), false);
        app.gradient_update();
        app.shape_press(
            crate::app::tools::shape::ShapeKind::Line,
            Vec2::new(10.0, 10.0),
        );
        new_canvas(&mut app);

        assert!(!app.selection_manager.has_selection(), "selection kept");
        assert!(app.workspace.gradient.session.is_none(), "gradient kept");
        assert!(app.workspace.shapes.session.is_none(), "shape kept");
        // Applying whatever was left must not touch the new document.
        app.gradient_commit();
        app.shape_commit();
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    #[test]
    fn a_new_canvas_keeps_the_old_thumbnails_until_the_next_frame() {
        let mut app = app();
        let ctx = egui::Context::default();
        let image = egui::ColorImage::new([4, 4], Color32::RED);
        app.layer_state.thumbnails = vec![Some(ctx.load_texture(
            "layer_thumb_0",
            image,
            egui::TextureOptions::LINEAR,
        ))];
        new_canvas(&mut app);
        // Freed this frame, egui-wgpu would destroy it before submitting
        // this frame's update to it.
        assert_eq!(app.workspace.retired_textures.len(), 1);
    }

    #[test]
    fn a_new_canvas_drops_a_floating_transform() {
        let mut app = app();
        // Something to float.
        app.canvas_mut()
            .set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        app.active_tool = crate::app::tools::Tool::Transform(Default::default());
        crate::app::tools::transform::transform_press(&mut app, Vec2::new(10.0, 10.0));
        crate::app::tools::transform::transform_release(&mut app);
        assert!(app.layer_state.floating_layer_idx.is_some());
        new_canvas(&mut app);
        assert!(app.layer_state.floating_layer_idx.is_none());
        assert_eq!(app.canvas.layers.len(), 2, "no stray floating layer");
        // Leaving the tool (which applies a float) is harmless now.
        crate::app::tools::transform::commit_floating_layer(&mut app);
        assert_eq!(app.canvas.layers.len(), 2);
    }

    #[test]
    fn opening_a_project_drops_the_old_sessions() {
        let mut app = app();
        let bytes = crate::project::encode_project(&app).unwrap();
        let path = std::env::temp_dir().join(format!("rp-open-{}.rpainter", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        app.select_all();
        app.gradient_press(Vec2::new(0.0, 0.0));
        app.gradient_drag(Vec2::new(200.0, 0.0), false);
        app.gradient_update();
        app.load_project_from_path(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(app.workspace.gradient.session.is_none(), "gradient kept");
        assert!(!app.selection_manager.has_selection(), "selection kept");
        app.gradient_commit();
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }
}
