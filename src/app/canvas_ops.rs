//! Document-level operations on the app: marking tiles for redraw,
//! replacing the document (new canvas, opened project), and layer
//! add/remove/move/merge with the per-layer state kept in step.

use crate::app::stroke_ops::exclusive;
use crate::app::{
    PainterApp,
    document::{CanvasTile, ColorModel, TILE_SIZE},
    state::{LayerState, RenderCache},
};
use crate::canvas::Canvas;
use crate::canvas::blend::Unmultiply;
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
        // A layer's border shows past its paint: the tiles around change too.
        let reach = self.canvas.style_reach();
        if reach > 0 {
            let ts = TILE_SIZE;
            let (ox, oy) = ((tx * ts) as i32, (ty * ts) as i32);
            let [x0, y0, x1, y1] = rect.map(|v| v as i32);
            let grown = [
                ox + x0 - reach,
                oy + y0 - reach,
                ox + x1 + reach,
                oy + y1 + reach,
            ];
            self.mark_rect_damage_exact(grown);
            return;
        }
        if let Some(tile) = self.tile_mut(tx, ty) {
            tile.mark_rect(rect);
        }
    }

    /// Mark exactly the canvas pixels `[x0, y0, x1, y1)` changed: each tile
    /// gets just its part as damage, so the display recomposites and
    /// uploads that part rather than whole tiles (as brush strokes do).
    pub(crate) fn mark_rect_damage(&mut self, rect: [i32; 4]) {
        let reach = self.canvas.style_reach();
        self.mark_rect_damage_exact([
            rect[0] - reach,
            rect[1] - reach,
            rect[2] + reach,
            rect[3] + reach,
        ]);
    }

    /// [`Self::mark_rect_damage`] without growing it for borders.
    fn mark_rect_damage_exact(&mut self, rect: [i32; 4]) {
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
                if let Some(tile) = self.tile_mut(tx as usize, ty as usize) {
                    tile.mark_rect(local);
                }
            }
        }
    }

    pub(crate) fn mark_tile_dirty(&mut self, tx: usize, ty: usize) {
        // A layer's border shows past its paint: the tiles around change too.
        let around = if self.canvas.style_reach() > 0 { 1 } else { 0 };
        for ny in ty.saturating_sub(around)..=ty + around {
            for nx in tx.saturating_sub(around)..=tx + around {
                if let Some(tile) = self.tile_mut(nx, ny) {
                    tile.mark_full();
                }
            }
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
        // Shader state is per layer id, which the new document reuses.
        self.workspace.shaders.forget_document();
        // 4. Selection and view.
        self.selection_manager.clear_selection();
        self.selection_manager.canvas_size = [width, height];
        self.workspace.select.saved.clear();
        self.reset_viewport_state();
        self.mark_saved();
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
        ws.filter.session = None;
        ws.filter.editing = None;
        ws.text.session = None;
        ws.text.stroke_rasterised.clear();
        ws.fill.path.clear();
        ws.guides.end_drag();
        ws.view_aids.guides.end_drag();
        // Its layer and history belong to the old document.
        ws.select.quick_mask = None;
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
        self.workspace.library.has_document = true;
        self.canvas_mut().blend_space = self.modal_state.new_canvas.blend_space;
        let depth = self.modal_state.new_canvas.depth;
        self.canvas_mut().convert_depth(depth);
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

    /// Resize, crop, turn or flip the whole document (the Image menu), as
    /// one undo step. Refused (with a message) past the size limits.
    pub(crate) fn apply_image_op(&mut self, op: crate::canvas::geometry::ImageOp) {
        self.quick_mask_leave();
        let (w, h) = op.new_size(self.canvas.width(), self.canvas.height());
        if let Err(err) = crate::app::document::validate_canvas_size(w, h) {
            self.export_state.message = Some(err);
            return;
        }
        // Sessions hold tiles or coordinates of the document as it is now.
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.gradient_commit();
        self.shape_commit();
        self.filter_cancel();
        self.release_canvas();
        let before = exclusive(&mut self.canvas).apply_image_op(op);
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Document(std::sync::Arc::new(
                std::sync::Mutex::new(before),
            ))),
        });
        self.after_document_swap();
    }

    /// Keep `depth` bits for each channel from now on (Image → Colour
    /// Depth), one undo step.
    pub(crate) fn convert_depth(&mut self, depth: crate::canvas::storage::Depth) {
        if self.canvas.depth() == depth {
            return;
        }
        self.quick_mask_leave();
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.gradient_commit();
        self.shape_commit();
        self.filter_cancel();
        self.release_canvas();
        let before = exclusive(&mut self.canvas).change_depth(depth);
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Document(std::sync::Arc::new(
                std::sync::Mutex::new(before),
            ))),
        });
        self.after_document_swap();
    }

    /// Merge the selected layer into the one below (Ctrl+Alt+E).
    pub(crate) fn merge_down(&mut self) {
        let idx = self.canvas.active_layer_idx;
        self.apply_merge(move |canvas| canvas.plan_merge_down(idx));
    }

    /// Merge every layer that shows into one (Ctrl+Shift+E).
    pub(crate) fn merge_visible(&mut self) {
        self.apply_merge(Canvas::plan_merge_visible);
    }

    /// Flatten everything into the background (drafts stay).
    pub(crate) fn flatten_image(&mut self) {
        self.apply_merge(Canvas::plan_flatten_image);
    }

    /// Run a merge as one undo step, or say why it can't be done. The
    /// layers are composited on the stroke worker (the frames go on); the
    /// merged layer takes their place once that's done.
    fn apply_merge(
        &mut self,
        plan: impl FnOnce(&Canvas) -> Result<crate::canvas::storage::MergePlan, &'static str>
        + Send
        + 'static,
    ) {
        self.quick_mask_leave();
        // Sessions hold tiles or indices of the layers as they are now.
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.gradient_commit();
        self.shape_commit();
        self.filter_cancel();
        self.release_canvas();
        // Shader layers merge as their current frame.
        self.bake_shader_layers();
        let canvas = std::sync::Arc::clone(&self.canvas);
        self.run_on_worker("Merging…", move || {
            let plan = plan(&canvas);
            drop(canvas);
            Box::new(move |app: &mut PainterApp| app.finish_merge(plan))
        });
    }

    fn finish_merge(&mut self, plan: Result<crate::canvas::storage::MergePlan, &'static str>) {
        match plan.map(|plan| self.canvas_mut().apply_merge_plan(plan)) {
            Ok(swap) => {
                let (out, put) = swap.applied.clone();
                self.replace_layer_states(&out, &put);
                self.layer_state.history.push_action(UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: Some(LayerHistoryOp::Replaced(std::sync::Arc::new(
                        std::sync::Mutex::new(swap),
                    ))),
                });
                self.mark_all_tiles_dirty();
                self.layer_state.thumbnails_dirty = true;
            }
            Err(why) => self.export_state.message = Some(why.to_string()),
        }
    }

    /// The document's size or layers were swapped wholesale (an Image menu
    /// step, or its undo): rebuild what depends on them.
    pub(crate) fn after_document_swap(&mut self) {
        let (w, h) = (self.canvas.width(), self.canvas.height());
        self.recreate_render_cache(w, h);
        // The selection's coordinates belong to the old geometry.
        self.selection_manager.clear_selection();
        self.selection_manager.canvas_size = [w, h];
        let count = self.canvas.layers.len();
        self.layer_state
            .layer_ui_colors
            .resize(count, Color32::from_gray(40));
        self.layer_state.thumbnails_dirty = true;
        self.workspace.auto_fit = true;
        self.workspace.fitted_to = None;
    }

    /// Clip the active layer (or folder) to the layer below, or unclip it
    /// (Ctrl+Alt+G). A selected mask stands for its layer.
    pub(crate) fn toggle_clip_active(&mut self) {
        let idx = self.canvas.active_layer_idx;
        let idx = match self.canvas.layers.get(idx).map(|l| l.kind) {
            Some(LayerKind::Mask { owner }) => self.canvas.layer_index_of(owner),
            Some(_) => Some(idx),
            None => None,
        };
        let Some(idx) = idx.filter(|&i| i != 0) else {
            return;
        };
        let layer = &mut self.canvas_mut().layers[idx];
        layer.clipped = !layer.clipped;
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
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
        // A layer's border shows past its paint.
        let bounds = bounds.expand(self.canvas.style_reach() as f32);

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
                            .data()
                            .is_some()
                    })
                    .unwrap_or(false);
                if has_data {
                    self.mark_tile_dirty(tx, ty);
                }
            }
        }
    }

    /// Move layer `from` so it ends at position `to` (as `Vec::remove` then
    /// `insert`) inside folder `parent`, with undo. The background (index 0)
    /// stays at the bottom, and a folder can't move into itself.
    pub(crate) fn move_layer(&mut self, from: usize, to: usize, parent: Option<LayerId>) {
        self.quick_mask_leave();
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
    pub(crate) fn next_layer_name(&self, base: &str) -> String {
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
        self.quick_mask_leave();
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

    /// Record a pixel edit as one undo step. A text layer it paints on
    /// becomes plain pixels in that same step (undo brings the text back).
    /// Run `command`; the undo step it makes (if any) is called `label`
    /// in the History panel.
    pub(crate) fn labelled(&mut self, label: &str, command: impl FnOnce(&mut Self)) {
        let before = self.layer_state.history.push_count();
        command(self);
        if self.layer_state.history.push_count() > before {
            self.layer_state.history.rename_last(label);
        }
    }

    /// Go back or forward through the history until `undo_len` steps can
    /// be undone (the History panel's click).
    pub(crate) fn history_jump(&mut self, undo_len: usize) {
        // Once the strokes queued are painted (they're steps too).
        self.when_strokes_painted(move |app| app.history_jump_now(undo_len));
    }

    fn history_jump_now(&mut self, undo_len: usize) {
        loop {
            let (undo, redo) = self.layer_state.history.labels();
            let (have, can_redo) = (undo.len(), redo.len());
            if have > undo_len {
                self.apply_history_now(false);
            } else if have < undo_len && can_redo > 0 {
                self.apply_history_now(true);
            } else {
                break;
            }
            // A step that couldn't move (nothing changed): stop.
            if self.layer_state.history.labels().0.len() == have {
                break;
            }
        }
    }

    pub(crate) fn push_undo(&mut self, mut action: UndoAction) {
        self.rasterise_painted_text(&mut action);
        self.rasterise_painted_vector(&mut action);
        self.layer_state.history.push_action(action);
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
        self.insert_entry_with(index, name, kind, parent, select, |_| {})
    }

    /// [`Self::insert_entry`], with `setup` adjusting the entry before it's
    /// recorded (so undo and redo bring it back as set up).
    pub(crate) fn insert_entry_with(
        &mut self,
        index: usize,
        name: String,
        kind: LayerKind,
        parent: Option<LayerId>,
        select: bool,
        setup: impl FnOnce(&mut crate::canvas::storage::Layer),
    ) -> usize {
        self.quick_mask_leave();
        let active_before = self.canvas.active_layer_idx;
        let id = self
            .canvas_mut()
            .insert_new_layer(index, name, kind, parent);
        let idx = self.canvas.layer_index_of(id).unwrap_or(index);
        setup(&mut self.canvas_mut().layers[idx]);
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
        self.quick_mask_leave();
        let (index, parent) = self.insertion_point(true);
        let name = self.next_layer_name("Layer");
        self.insert_entry(index, name, LayerKind::Paint, parent, true);
    }

    /// Add an adjustment layer (`filter` over everything below it) above the
    /// selected layer, select it and open its settings. It's locked: its own
    /// pixels don't show (add a mask to limit where it applies).
    pub(crate) fn add_adjustment_layer(&mut self, filter: crate::canvas::filters::Filter) {
        self.quick_mask_leave();
        let (index, parent) = self.insertion_point(true);
        let name = filter.name().to_string();
        let idx = self.insert_entry_with(index, name, LayerKind::Paint, parent, true, |l| {
            l.adjustment = Some(filter);
            l.locked = true;
        });
        self.workspace.filter.editing = self.canvas.layer_id_at(idx);
    }

    /// Add a fill layer (a colour or gradient everywhere) above the
    /// selected layer, select it and open its settings. It's locked: it
    /// can't be painted on (a mask or clipping says where it shows).
    pub(crate) fn add_fill_layer(&mut self, fill: crate::canvas::layer_style::LayerFill) {
        self.quick_mask_leave();
        let (index, parent) = self.insertion_point(true);
        let name = self.next_layer_name(fill.name());
        let idx = self.insert_entry_with(index, name, LayerKind::Paint, parent, true, |l| {
            l.style.fill = Some(fill);
            l.locked = true;
        });
        // Unlike a new empty layer, it changes the whole picture.
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
        self.workspace.filter.fill_editing = self.canvas.layer_id_at(idx);
    }

    /// A colour fill in the brush colour.
    pub(crate) fn add_colour_fill_layer(&mut self) {
        let [r, g, b, _] = self.brush_state.brush.brush_options.color.unmultiplied();
        self.add_fill_layer(crate::canvas::layer_style::LayerFill::Colour([r, g, b]));
    }

    /// A gradient fill from the brush colour to the secondary colour,
    /// left to right across the canvas.
    pub(crate) fn add_gradient_fill_layer(&mut self) {
        let rgb = |c: Color32| {
            let [r, g, b, _] = c.unmultiplied();
            [r, g, b]
        };
        let colours = crate::canvas::filters::GradientMap::from_stops(&[
            (0.0, rgb(self.brush_state.brush.brush_options.color)),
            (1.0, rgb(self.brush_state.secondary_color)),
        ]);
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        self.add_fill_layer(crate::canvas::layer_style::LayerFill::Gradient {
            colours,
            shape: crate::canvas::gradient::GradientShape::Linear,
            start: [0.0, h / 2.0],
            end: [w, h / 2.0],
        });
    }

    /// Change layer `idx`'s fill or border (live, from their dialogs).
    pub(crate) fn set_layer_style(
        &mut self,
        idx: usize,
        style: crate::canvas::layer_style::LayerStyle,
    ) {
        if self.canvas.layers.get(idx).is_none_or(|l| l.style == style) {
            return;
        }
        self.canvas_mut().layers[idx].style = style;
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Add an empty folder above the selected layer and select it.
    pub(crate) fn add_folder(&mut self) {
        self.quick_mask_leave();
        let (index, parent) = self.insertion_point(false);
        let name = self.next_layer_name("Folder");
        self.insert_entry(index, name, LayerKind::Group, parent, true);
    }

    /// Give the selected paint layer a mask (showing everything) and select
    /// the mask for painting; selects the existing mask if there is one.
    pub(crate) fn add_mask_to_active(&mut self) {
        self.quick_mask_leave();
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
        self.quick_mask_leave();
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

    /// Mirror a [`LayerSwap`](crate::canvas::storage::LayerSwap): entries
    /// taken out from positions `out` (before), others put in at `put`
    /// (after).
    pub(crate) fn replace_layer_states(&mut self, out: &[usize], put: &[usize]) {
        let colors = &mut self.layer_state.layer_ui_colors;
        for &idx in out.iter().rev() {
            if idx < colors.len() {
                colors.remove(idx);
            }
        }
        for &idx in put {
            let idx = idx.min(colors.len());
            colors.insert(idx, Color32::from_gray(40));
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

#[cfg(test)]
mod merge_tests {
    use crate::PainterApp;
    use crate::canvas::Canvas;
    use crate::canvas::blend_modes::LayerBlend;
    use crate::canvas::filters::Filter;
    use crate::canvas::storage::{LayerId, LayerKind};
    use crate::project::tests::test_app_pub;
    use eframe::egui::{Color32, Vec2};

    fn solid(c: Color32) -> Vec<Color32> {
        vec![c; 64 * 64]
    }

    /// Top half of a tile painted `c`, the rest transparent.
    fn half(c: Color32) -> Vec<Color32> {
        let mut t = vec![Color32::TRANSPARENT; 64 * 64];
        t[..64 * 32].fill(c);
        t
    }

    /// A 128×128 document with most things a merge must honour: a painted
    /// layer, a folder (Screen, 80 %) holding a Multiply layer at 60 % with
    /// a mask and a layer clipped to it, an Invert adjustment over half the
    /// picture, and a hidden layer.
    fn app() -> PainterApp {
        let mut app = test_app_pub(Canvas::new(128, 128, Color32::from_rgb(230, 220, 200), 64));
        let c = app.canvas_mut();
        c.set_layer_tile_data(
            1,
            0,
            0,
            solid(Color32::from_rgba_unmultiplied(200, 40, 40, 200)),
        );
        c.set_layer_tile_data(1, 1, 1, half(Color32::from_rgb(20, 90, 200)));
        let folder = c.insert_new_layer(2, "Folder".into(), LayerKind::Group, None);
        c.layers[2].blend = LayerBlend::Screen;
        c.layers[2].opacity = 0.8;
        let shade = c.insert_new_layer(3, "Shade".into(), LayerKind::Paint, Some(folder));
        c.set_layer_tile_data(3, 0, 0, half(Color32::from_gray(120)));
        c.set_layer_tile_data(3, 1, 0, solid(Color32::from_rgb(90, 160, 60)));
        c.layers[3].blend = LayerBlend::Multiply;
        c.layers[3].opacity = 0.6;
        c.insert_new_layer(4, "Clipped".into(), LayerKind::Paint, Some(folder));
        c.set_layer_tile_data(4, 0, 0, solid(Color32::from_rgb(250, 200, 0)));
        c.set_layer_tile_data(4, 1, 0, solid(Color32::from_rgb(0, 0, 250)));
        c.layers[4].clipped = true;
        let adjust = c.insert_new_layer(5, "Invert".into(), LayerKind::Paint, None);
        c.layers[5].adjustment = Some(Filter::Invert);
        c.layers[5].opacity = 0.5;
        c.insert_new_layer(6, "Hidden".into(), LayerKind::Paint, None);
        c.set_layer_tile_data(6, 0, 1, solid(Color32::BLUE));
        c.layers[6].visible = false;
        c.insert_new_layer(
            7,
            "Shade mask".into(),
            LayerKind::Mask { owner: shade },
            None,
        );
        c.set_layer_tile_data(7, 0, 0, half(Color32::BLACK));
        c.insert_new_layer(
            8,
            "Invert mask".into(),
            LayerKind::Mask { owner: adjust },
            None,
        );
        c.set_layer_tile_data(8, 1, 0, solid(Color32::BLACK));
        c.set_layer_tile_data(8, 1, 1, solid(Color32::BLACK));
        let n = app.canvas.layers.len();
        app.layer_state
            .layer_ui_colors
            .resize(n, Color32::from_gray(40));
        app.canvas_mut().active_layer_idx = 4;
        app
    }

    /// Largest channel difference between two pictures.
    fn worst(a: &[Color32], b: &[Color32]) -> u8 {
        a.iter()
            .zip(b)
            .flat_map(|(x, y)| {
                let (x, y) = (x.to_array(), y.to_array());
                (0..4).map(move |i| x[i].abs_diff(y[i]))
            })
            .max()
            .unwrap_or(0)
    }

    /// One entry of the layer list: id, name, kind, folder, shown,
    /// opacity, clipped, and its tiles' pixels.
    type Entry = (
        LayerId,
        String,
        LayerKind,
        Option<LayerId>,
        bool,
        u32,
        bool,
        Vec<Vec<Color32>>,
    );

    /// Everything about the layer list: ids, settings and pixels.
    fn tree(app: &PainterApp) -> Vec<Entry> {
        let c = &app.canvas;
        (0..c.layers.len())
            .map(|i| {
                let l = &c.layers[i];
                let mut keys = c.layer_tile_keys(i);
                keys.sort_unstable();
                let tiles = keys
                    .into_iter()
                    .filter_map(|(tx, ty)| c.get_layer_tile_data(i, tx, ty))
                    .collect();
                (
                    l.id,
                    l.name.clone(),
                    l.kind,
                    l.parent,
                    l.visible,
                    l.opacity.to_bits(),
                    l.clipped,
                    tiles,
                )
            })
            .collect()
    }

    /// Run `merge`, check the picture didn't change and that it's one undo
    /// step which puts everything back (and redo does it again).
    fn check(app: &mut PainterApp, merge: fn(&mut PainterApp), layers_after: usize) {
        let before = app.canvas.flatten().pixels;
        let tree_before = tree(app);
        merge(app);
        let merged = app.canvas.flatten().pixels;
        let off = worst(&before, &merged);
        assert!(off <= 2, "off by {off}");
        assert_eq!(app.canvas.layers.len(), layers_after);
        assert_eq!(app.layer_state.layer_ui_colors.len(), layers_after);
        let tree_after = tree(app);
        assert_eq!(app.layer_state.history.stacks().0.len(), 1, "one undo step");
        app.apply_history(false);
        assert!(tree(app) == tree_before, "undo restores the layers");
        assert_eq!(app.canvas.flatten().pixels, before);
        app.apply_history(true);
        assert!(tree(app) == tree_after, "redo merges again");
    }

    #[test]
    fn merge_down_keeps_the_picture_and_undoes_in_one_step() {
        let mut app = app();
        // The clipped layer into the masked Multiply one, inside the folder.
        check(&mut app, PainterApp::merge_down, 7);
        let shade = &app.canvas.layers[3];
        assert_eq!(shade.name, "Shade");
        assert_eq!(shade.blend, LayerBlend::Multiply, "keeps its blend mode");
        assert_eq!(app.canvas.layers[2].kind, LayerKind::Group);
        assert_eq!(shade.parent, Some(app.canvas.layers[2].id));
        assert_eq!(app.canvas.active_layer_idx, 3);
        assert!(
            app.canvas.mask_index_of(shade.id).is_none(),
            "mask baked in"
        );
    }

    #[test]
    fn merge_down_onto_a_normal_layer_bakes_its_opacity() {
        let mut app = test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        let c = app.canvas_mut();
        c.set_layer_tile_data(1, 0, 0, half(Color32::RED));
        c.layers[1].opacity = 0.5;
        c.insert_new_layer(2, "Top".into(), LayerKind::Paint, None);
        c.set_layer_tile_data(
            2,
            1,
            0,
            solid(Color32::from_rgba_unmultiplied(0, 0, 255, 128)),
        );
        c.set_layer_tile_data(2, 0, 0, solid(Color32::from_rgb(0, 200, 0)));
        c.layers[2].opacity = 0.7;
        c.active_layer_idx = 2;
        app.layer_state
            .layer_ui_colors
            .resize(3, Color32::from_gray(40));
        check(&mut app, PainterApp::merge_down, 2);
        assert_eq!(app.canvas.layers[1].opacity, 1.0);
        assert_eq!(app.canvas.layers[1].blend, LayerBlend::Normal);
    }

    #[test]
    fn merge_visible_keeps_the_picture_and_hidden_layers() {
        let mut app = app();
        // Everything that shows goes into the background; the hidden layer
        // stays.
        check(&mut app, PainterApp::merge_visible, 2);
        assert_eq!(app.canvas.layers[0].id, LayerId(0));
        assert_eq!(app.canvas.layers[1].name, "Hidden");
    }

    #[test]
    fn flatten_keeps_the_picture_and_undoes_in_one_step() {
        let mut app = app();
        check(&mut app, PainterApp::flatten_image, 1);
        assert_eq!(app.canvas.layers[0].name, "Background");
    }

    #[test]
    fn flatten_over_a_hidden_background_stays_transparent() {
        let mut app = app();
        app.canvas_mut().layers[0].visible = false;
        check(&mut app, PainterApp::flatten_image, 1);
    }

    #[test]
    fn merge_down_says_why_it_cannot() {
        let mut app = app();
        app.canvas_mut().active_layer_idx = 3; // lowest in its folder
        app.merge_down();
        assert!(app.export_state.message.is_some());
        assert!(app.layer_state.history.stacks().0.is_empty());
    }

    #[test]
    fn draft_layers_are_left_out_of_export_merging_and_sampling() {
        let mut app = test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        let c = app.canvas_mut();
        c.set_layer_tile_data(1, 0, 0, half(Color32::RED));
        c.insert_new_layer(2, "Sketch".into(), LayerKind::Paint, None);
        c.set_layer_tile_data(2, 0, 0, solid(Color32::BLUE));
        c.layers[2].draft = true;
        app.layer_state
            .layer_ui_colors
            .resize(3, Color32::from_gray(40));
        // Still on screen.
        assert_eq!(app.canvas.flatten().pixels[40 * 128], Color32::BLUE);
        // Not exported.
        let mut without = test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        without
            .canvas_mut()
            .set_layer_tile_data(1, 0, 0, half(Color32::RED));
        let expected = without.canvas.flatten().pixels;
        assert_eq!(app.canvas.flatten_final().pixels, expected);
        let psd = crate::project::psd::PsdDocument::from_canvas(&app.canvas);
        assert_eq!(psd.composite, expected);
        // Not picked by the eyedropper.
        app.pick_color(Vec2::new(5.0, 40.0));
        let picked = app
            .brush_state
            .brush
            .brush_options
            .color
            .to_srgba_unmultiplied();
        assert_eq!(picked[..3], [255, 255, 255]);
        // Not in "all layers".
        assert_eq!(
            app.canvas.render_reference(None, 0, 40, 1, 1)[0],
            Color32::TRANSPARENT
        );
        // Merge Visible leaves it as it is.
        app.merge_visible();
        assert_eq!(app.canvas.layers.len(), 2);
        assert!(app.canvas.layers[1].draft);
        assert_eq!(app.canvas.layers[0].id, LayerId(0));
        assert_eq!(
            app.canvas.get_layer_tile_data(0, 0, 0).unwrap()[40 * 64],
            Color32::WHITE
        );
        // So does Flatten.
        app.apply_history(false);
        app.flatten_image();
        assert_eq!(app.canvas.layers.len(), 2);
        assert!(app.canvas.layers[1].draft);
    }

    #[test]
    fn a_fill_can_find_its_area_in_the_reference_layer() {
        use crate::app::tools::fill::FillSource;
        let mut app = test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        // Line art on layer 1: a square outline from 10 to 40.
        let mut lines = vec![Color32::TRANSPARENT; 64 * 64];
        for i in 10..=40 {
            for (x, y) in [(i, 10), (i, 40), (10, i), (40, i)] {
                lines[y * 64 + x] = Color32::BLACK;
            }
        }
        app.canvas_mut().set_layer_tile_data(1, 0, 0, lines.clone());
        app.canvas_mut()
            .insert_new_layer(2, "Colours".into(), LayerKind::Paint, None);
        app.layer_state
            .layer_ui_colors
            .resize(3, Color32::from_gray(40));
        app.canvas_mut().active_layer_idx = 2;
        app.workspace.fill.source = FillSource::Reference;
        app.workspace.fill.settings.antialias = false;
        app.workspace.fill.settings.expand = 0;
        app.brush_state.brush.brush_options.color = Color32::GREEN;
        // No reference layer yet: nothing happens, and a notice says so.
        app.fill_press(Vec2::new(20.0, 20.0));
        assert!(app.layer_state.history.stacks().0.is_empty());
        assert!(app.export_state.message.is_some());
        app.canvas_mut().layers[1].reference = true;
        // Hidden, the reference still counts.
        app.canvas_mut().layers[1].visible = false;
        app.fill_press(Vec2::new(20.0, 20.0));
        let colours = app.canvas.get_layer_tile_data(2, 0, 0).unwrap();
        assert_eq!(colours[20 * 64 + 20], Color32::GREEN, "inside the lines");
        assert_eq!(colours[5 * 64 + 5], Color32::TRANSPARENT, "outside");
        assert_eq!(colours[50 * 64 + 50], Color32::TRANSPARENT, "outside");
        assert_eq!(app.canvas.get_layer_tile_data(1, 0, 0).unwrap(), lines);
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
    }

    #[test]
    fn layer_flags_survive_saving_and_opening() {
        let mut app = app();
        app.canvas_mut().layers[1].position_locked = true;
        app.canvas_mut().layers[3].draft = true;
        app.canvas_mut().layers[2].reference = true;
        // A merge in the history: saved, so the reopened file can undo it.
        app.canvas_mut().active_layer_idx = 4;
        let tree_before = tree(&app);
        let pixels_before = app.canvas.flatten().pixels;
        app.merge_down();
        let tree_after = tree(&app);
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let flags = |c: &Canvas| {
            c.layers
                .iter()
                .map(|l| (l.id, l.position_locked, l.draft, l.reference))
                .collect::<Vec<_>>()
        };
        assert_eq!(flags(&loaded.canvas), flags(&app.canvas));
        assert!(loaded.canvas.layers[1].position_locked);
        assert_eq!(loaded.history.stacks().0.len(), 1);
        let mut reopened = test_app_pub(Canvas::new(8, 8, Color32::WHITE, 64));
        reopened.replace_document(loaded.canvas, loaded.history);
        assert!(tree(&reopened) == tree_after);
        reopened.apply_history(false);
        assert!(
            tree(&reopened) == tree_before,
            "undo goes back through the merge"
        );
        assert_eq!(reopened.canvas.flatten().pixels, pixels_before);
        reopened.apply_history(true);
        assert!(tree(&reopened) == tree_after, "and redo merges again");
    }
}
