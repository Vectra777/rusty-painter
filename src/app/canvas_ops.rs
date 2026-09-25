use super::{
    PainterApp,
    painter_helpers::{AtlasLayout, AtlasPosition},
    state::{ATLAS_SIZE, CanvasTile, ColorModel, TILE_SIZE, TextureAtlas},
};
use crate::canvas::Canvas;
use crate::canvas::history::History;
use eframe::egui::{self, Color32, TextureOptions, Vec2};
use std::collections::{HashMap, HashSet};

impl PainterApp {
    /// Panics in debug builds if the per-layer side-car vecs (undo history,
    /// render cache, cache-dirty set, UI color) have drifted out of sync
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
        debug_assert_eq!(
            self.render_cache.layer_caches.len(),
            layer_count,
            "render_cache.layer_caches desynced from canvas.layers"
        );
        debug_assert_eq!(
            self.render_cache.layer_cache_dirty.len(),
            layer_count,
            "render_cache.layer_cache_dirty desynced from canvas.layers"
        );
    }

    pub(crate) fn initialize_render_cache(
        ctx: &egui::Context,
        canvas_w: usize,
        canvas_h: usize,
        layer_count: usize,
    ) -> super::painter_state::RenderCache {
        let tiles_x = canvas_w.div_ceil(TILE_SIZE);
        let tiles_y = canvas_h.div_ceil(TILE_SIZE);
        let atlas_layout = Self::calculate_atlas_layout();
        let atlases = Self::create_initial_atlases(ctx, tiles_x, tiles_y, &atlas_layout);
        let tiles = Self::create_initial_tiles(canvas_w, canvas_h, tiles_x, tiles_y, &atlas_layout);
        super::painter_state::RenderCache::new(tiles, atlases, tiles_x, tiles_y, layer_count, true)
    }

    fn create_initial_atlases(
        ctx: &egui::Context,
        tiles_x: usize,
        tiles_y: usize,
        layout: &AtlasLayout,
    ) -> Vec<TextureAtlas> {
        let total_tiles = tiles_x * tiles_y;
        let atlas_count = total_tiles.div_ceil(layout.capacity);
        (0..atlas_count)
            .map(|idx| {
                let img = egui::ColorImage::new([ATLAS_SIZE, ATLAS_SIZE], Color32::TRANSPARENT);
                let texture =
                    ctx.load_texture(format!("canvas_atlas_{}", idx), img, TextureOptions::LINEAR);
                TextureAtlas { texture }
            })
            .collect()
    }

    fn create_initial_tiles(
        canvas_w: usize,
        canvas_h: usize,
        tiles_x: usize,
        tiles_y: usize,
        layout: &AtlasLayout,
    ) -> Vec<CanvasTile> {
        let mut tiles = Vec::new();
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let pos = Self::calculate_tile_atlas_position(tx, ty, tiles_x, layout);
                tiles.push(Self::create_canvas_tile(tx, ty, canvas_w, canvas_h, pos));
            }
        }
        tiles
    }

    pub(crate) fn mark_tile_dirty(&mut self, tx: usize, ty: usize) {
        if let Some(tile) = self.tile_mut(tx, ty) {
            tile.dirty = true;
        }
    }

    pub(crate) fn mark_modified_tiles_dirty(&mut self) {
        let tiles: Vec<_> = self.render_cache.modified_tiles.iter().copied().collect();
        for (tx, ty) in tiles {
            self.mark_tile_dirty(tx, ty);
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
        ctx: &egui::Context,
        width: usize,
        height: usize,
        background: Color32,
    ) {
        self.reset_canvas_state(width, height, background);
        self.recreate_render_cache(width, height);
        self.create_atlas_textures(ctx);
        self.generate_tile_grid(width, height);
        self.reset_viewport_state();
    }

    fn reset_canvas_state(&mut self, width: usize, height: usize, background: Color32) {
        self.canvas = Canvas::new(width, height, background, TILE_SIZE);
        let layer_count = self.canvas.layers.len();
        self.layer_state.histories = (0..layer_count).map(|_| History::new()).collect();
        self.layer_state.layer_ui_colors = vec![Color32::from_gray(40); layer_count];
        self.layer_state.layer_dragging = None;
        self.brush_state.session = None;
    }

    fn recreate_render_cache(&mut self, width: usize, height: usize) {
        let layer_count = self.canvas.layers.len();
        self.render_cache.layer_caches = vec![HashMap::new(); layer_count];
        self.render_cache.layer_cache_dirty = vec![HashSet::new(); layer_count];
        self.render_cache.modified_tiles.clear();
        self.render_cache.tiles_x = width.div_ceil(TILE_SIZE);
        self.render_cache.tiles_y = height.div_ceil(TILE_SIZE);
        self.brush_state.is_drawing = false;
        self.viewport.is_panning = false;
        self.viewport.is_rotating = false;
        self.viewport.is_primary_down = false;
    }

    fn create_atlas_textures(&mut self, ctx: &egui::Context) {
        let atlas_layout = Self::calculate_atlas_layout();
        self.render_cache.texture_generation = self.render_cache.texture_generation.wrapping_add(1);
        self.render_cache.atlases.clear();

        let atlas_count =
            (self.render_cache.tiles_x * self.render_cache.tiles_y).div_ceil(atlas_layout.capacity);
        for idx in 0..atlas_count {
            let texture =
                Self::create_atlas_texture(ctx, self.render_cache.texture_generation, idx);
            self.render_cache.atlases.push(TextureAtlas { texture });
        }
    }

    fn calculate_atlas_layout() -> AtlasLayout {
        let cols = (ATLAS_SIZE / TILE_SIZE).max(1);
        AtlasLayout {
            cols,
            capacity: cols * cols,
        }
    }

    fn create_atlas_texture(
        ctx: &egui::Context,
        generation: u64,
        idx: usize,
    ) -> egui::TextureHandle {
        let img = egui::ColorImage::new([ATLAS_SIZE, ATLAS_SIZE], Color32::TRANSPARENT);
        ctx.load_texture(
            format!("canvas_atlas_{}_{}", generation, idx),
            img,
            TextureOptions::NEAREST,
        )
    }

    fn generate_tile_grid(&mut self, width: usize, height: usize) {
        let atlas_layout = Self::calculate_atlas_layout();
        self.render_cache.tiles.clear();

        for ty in 0..self.render_cache.tiles_y {
            for tx in 0..self.render_cache.tiles_x {
                let pos = Self::calculate_tile_atlas_position(
                    tx,
                    ty,
                    self.render_cache.tiles_x,
                    &atlas_layout,
                );
                self.render_cache
                    .tiles
                    .push(Self::create_canvas_tile(tx, ty, width, height, pos));
            }
        }
    }

    fn calculate_tile_atlas_position(
        tx: usize,
        ty: usize,
        tiles_x: usize,
        layout: &AtlasLayout,
    ) -> AtlasPosition {
        let flat_idx = ty * tiles_x + tx;
        let atlas_idx = flat_idx / layout.capacity;
        let atlas_local = flat_idx % layout.capacity;
        AtlasPosition {
            atlas_idx,
            x: (atlas_local % layout.cols) * TILE_SIZE,
            y: (atlas_local / layout.cols) * TILE_SIZE,
        }
    }

    fn create_canvas_tile(
        tx: usize,
        ty: usize,
        width: usize,
        height: usize,
        pos: AtlasPosition,
    ) -> CanvasTile {
        CanvasTile {
            dirty: true,
            atlas_idx: pos.atlas_idx,
            atlas_x: pos.x,
            atlas_y: pos.y,
            pixel_w: TILE_SIZE.min(width - tx * TILE_SIZE),
            pixel_h: TILE_SIZE.min(height - ty * TILE_SIZE),
            tx,
            ty,
        }
    }

    fn reset_viewport_state(&mut self) {
        self.viewport.offset = Vec2::ZERO;
        self.viewport.zoom = 1.0;
        self.viewport.rotation = 0.0;
        self.workspace.first_frame = true;
    }

    pub(crate) fn apply_new_canvas(&mut self, ctx: &egui::Context) {
        let Ok((width, height)) = self.modal_state.new_canvas.validated_dimensions() else {
            return;
        };
        self.workspace.color_model = self.modal_state.new_canvas.color_model;
        let background = self
            .modal_state
            .new_canvas
            .background_color32(self.workspace.color_model);
        self.rebuild_canvas(ctx, width, height, background);
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

        let layer = self.canvas.layers.remove(from);
        self.canvas.layers.insert(to, layer);
        let hist = self.layer_state.histories.remove(from);
        self.layer_state.histories.insert(to, hist);
        let cache = self.render_cache.layer_caches.remove(from);
        self.render_cache.layer_caches.insert(to, cache);
        let cache_dirty = self.render_cache.layer_cache_dirty.remove(from);
        self.render_cache.layer_cache_dirty.insert(to, cache_dirty);
        let ui_color = self.layer_state.layer_ui_colors.remove(from);
        self.layer_state.layer_ui_colors.insert(to, ui_color);

        let active = self.canvas.active_layer_idx;
        self.canvas.active_layer_idx = if active == from {
            to
        } else if from < active && active <= to {
            active - 1
        } else if to <= active && active < from {
            active + 1
        } else {
            active
        };

        self.mark_all_tiles_dirty();
        self.debug_assert_layer_state_in_sync();
    }

    pub(crate) fn add_paint_layer(&mut self) {
        self.canvas.add_layer();
        self.insert_layer_state(self.canvas.layers.len().saturating_sub(1));
    }

    pub(crate) fn remove_paint_layer(&mut self, idx: usize) {
        if idx >= self.canvas.layers.len() || self.canvas.layers.len() <= 1 || idx == 0 {
            return;
        }

        let active = self.canvas.active_layer_idx;
        self.mark_layer_tiles_with_data_dirty(idx);
        self.canvas.layers.remove(idx);
        self.remove_layer_state(idx);
        self.canvas.active_layer_idx = if active == idx {
            idx.min(self.canvas.layers.len().saturating_sub(1))
        } else if idx < active {
            active.saturating_sub(1)
        } else {
            active.min(self.canvas.layers.len().saturating_sub(1))
        };
        self.mark_all_tiles_dirty();
    }

    pub(crate) fn insert_layer_state(&mut self, idx: usize) {
        let idx = idx.min(self.canvas.layers.len());
        self.layer_state.histories.insert(idx, History::new());
        self.render_cache.layer_caches.insert(idx, HashMap::new());
        self.render_cache
            .layer_cache_dirty
            .insert(idx, HashSet::new());
        self.layer_state
            .layer_ui_colors
            .insert(idx, Color32::from_gray(40));
        self.debug_assert_layer_state_in_sync();
    }

    pub(crate) fn remove_layer_state(&mut self, idx: usize) {
        if idx < self.layer_state.histories.len() {
            self.layer_state.histories.remove(idx);
        }
        if idx < self.render_cache.layer_caches.len() {
            self.render_cache.layer_caches.remove(idx);
        }
        if idx < self.render_cache.layer_cache_dirty.len() {
            self.render_cache.layer_cache_dirty.remove(idx);
        }
        if idx < self.layer_state.layer_ui_colors.len() {
            self.layer_state.layer_ui_colors.remove(idx);
        }
        self.debug_assert_layer_state_in_sync();
    }
}
