use super::state::{CanvasTile, ColorModel, NewCanvasSettings, TextureAtlas};
use crate::{
    brush_engine::brush::{Brush, BrushPreset, StrokeState},
    canvas::{
        canvas::Canvas,
        history::{History, UndoAction},
    },
    ui,
    utils::{color::Color, profiler::ScopeTimer, vector::Vec2},
};
use eframe::egui;
use eframe::egui::{Color32, TextureOptions};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const TILE_SIZE: usize = 64;
const ATLAS_SIZE: usize = 2048;

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
pub struct PainterApp {
    pub(crate) canvas: Canvas,
    pub(crate) brush: Brush,
    pub(crate) presets: Vec<BrushPreset>,
    pub(crate) stroke: Option<StrokeState>,
    pub(crate) is_drawing: bool,

    pub(crate) histories: Vec<History>,
    pub(crate) current_undo_action: Option<UndoAction>,
    pub(crate) modified_tiles: HashSet<(usize, usize)>,

    pub(crate) tiles: Vec<CanvasTile>,
    pub(crate) atlases: Vec<TextureAtlas>,
    pub(crate) tiles_x: usize,
    pub(crate) tiles_y: usize,
    pub(crate) layer_caches: Vec<HashMap<(usize, usize), egui::ColorImage>>,
    pub(crate) layer_cache_dirty: Vec<HashSet<(usize, usize)>>,
    pub(crate) layer_ui_colors: Vec<Color32>,
    pub(crate) layer_dragging: Option<usize>,

    pub(crate) zoom: f32,
    pub(crate) offset: Vec2,
    pub(crate) first_frame: bool,
    pub(crate) use_masked_brush: bool,
    pub(crate) thread_count: usize,
    pub(crate) max_threads: usize,
    pub(crate) pool: ThreadPool,
    pub(crate) is_panning: bool,
    pub(crate) is_rotating: bool,
    pub(crate) rotation: f32,
    pub(crate) is_primary_down: bool,
    pub(crate) disable_lod: bool,
    pub(crate) force_full_upload: bool,
    pub(crate) show_new_canvas_modal: bool,
    pub(crate) show_export_modal: bool,
    pub(crate) new_canvas: NewCanvasSettings,
    pub(crate) export_settings: crate::ui::export_modal::ExportSettings,
    pub(crate) export_message: Option<String>,
    pub(crate) export_in_progress: bool,
    pub(crate) export_task: Option<std::thread::JoinHandle<Result<String, String>>>,
    pub(crate) export_progress: f32,
    pub(crate) export_progress_rx: Option<mpsc::Receiver<crate::ui::export_modal::ExportProgress>>,
    pub(crate) color_model: ColorModel,
    pub(crate) texture_generation: u64,
}

impl PainterApp {
    /// Initialize the UI, canvas, thread pool and GPU atlases.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let canvas_w = 8000;
        let canvas_h = 8000;
        let canvas = Canvas::new(canvas_w, canvas_h, Color32::WHITE, TILE_SIZE);
        let layer_count = canvas.layers.len();
        let new_canvas = NewCanvasSettings::from_canvas(&canvas);
        let color_model = new_canvas.color_model;

        let brush = Brush::new(24.0, 20.0, Color::rgba(0, 0, 0, 255), 25.0);

        let presets = vec![
            BrushPreset {
                name: "Soft Round".to_string(),
                brush: Brush::new(24.0, 20.0, Color::rgba(0, 0, 0, 255), 25.0),
            },
            BrushPreset {
                name: "Hard Round".to_string(),
                brush: Brush::new(20.0, 100.0, Color::rgba(0, 0, 0, 255), 10.0),
            },
            BrushPreset {
                name: "Pixel".to_string(),
                brush: Brush::new_pixel(1.0, Color::rgba(0, 0, 0, 255)),
            },
            BrushPreset {
                name: "Airbrush".to_string(),
                brush: {
                    let mut b = Brush::new(50.0, 0.0, Color::rgba(0, 0, 0, 255), 20.0);
                    b.flow = 10.0;
                    b
                },
            },
            BrushPreset {
                name: "Stabilized".to_string(),
                brush: {
                    let mut b = Brush::new(20.0, 80.0, Color::rgba(0, 0, 0, 255), 10.0);
                    b.stabilizer = 0.8;
                    b
                },
            },
        ];

        let max_threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .max(1);
        let thread_count = max_threads;
        let pool = ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build()
            .expect("failed to build thread pool");

        let tiles_x = (canvas_w + TILE_SIZE - 1) / TILE_SIZE;
        let tiles_y = (canvas_h + TILE_SIZE - 1) / TILE_SIZE;
        debug_assert!(
            ATLAS_SIZE % TILE_SIZE == 0,
            "ATLAS_SIZE must be divisible by TILE_SIZE for clean packing"
        );

        let atlas_cols = (ATLAS_SIZE / TILE_SIZE).max(1);
        let atlas_capacity = atlas_cols * atlas_cols;
        let total_tiles = tiles_x * tiles_y;
        let atlas_count = (total_tiles + atlas_capacity - 1) / atlas_capacity;

        let mut atlases = Vec::new();
        for idx in 0..atlas_count {
            let img = egui::ColorImage::new([ATLAS_SIZE, ATLAS_SIZE], Color32::TRANSPARENT);
            let texture = cc.egui_ctx.load_texture(
                format!("canvas_atlas_{}", idx),
                img,
                TextureOptions::LINEAR,
            );
            atlases.push(TextureAtlas { texture });
        }

        let mut tiles = Vec::new();

        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let flat_idx = ty * tiles_x + tx;
                let atlas_idx = flat_idx / atlas_capacity;
                let atlas_local = flat_idx % atlas_capacity;
                let atlas_tile_x = (atlas_local % atlas_cols) * TILE_SIZE;
                let atlas_tile_y = (atlas_local / atlas_cols) * TILE_SIZE;
                let tile_w = TILE_SIZE.min(canvas_w - tx * TILE_SIZE);
                let tile_h = TILE_SIZE.min(canvas_h - ty * TILE_SIZE);
                tiles.push(CanvasTile {
                    dirty: true,
                    atlas_idx,
                    atlas_x: atlas_tile_x,
                    atlas_y: atlas_tile_y,
                    pixel_w: tile_w,
                    pixel_h: tile_h,
                    tx,
                    ty,
                });
            }
        }

        Self {
            canvas,
            brush,
            presets,
            stroke: None,
            is_drawing: false,
            is_panning: false,
            is_rotating: false,
            rotation: 0.0,
            is_primary_down: false,
            histories: vec![History::new()],
            current_undo_action: None,
            modified_tiles: HashSet::new(),
            tiles,
            atlases,
            tiles_x,
            tiles_y,
            layer_caches: vec![HashMap::new(); layer_count],
            layer_cache_dirty: vec![HashSet::new(); layer_count],
            layer_ui_colors: vec![Color32::from_gray(40); layer_count],
            layer_dragging: None,
            zoom: 1.0,
            offset: Vec2 { x: 300.0, y: 100.0 },
            first_frame: true,
            use_masked_brush: true,
            thread_count,
            max_threads,
            pool,
            disable_lod: false,
            force_full_upload: false,
            show_new_canvas_modal: false,
            show_export_modal: false,
            new_canvas,
            export_settings: crate::ui::export_modal::ExportSettings::new(),
            export_message: None,
            export_in_progress: false,
            export_task: None,
            export_progress: 0.0,
            export_progress_rx: None,
            color_model,
            texture_generation: 0,
        }
    }

    /// Mark all tiles that intersect a stroke segment as dirty so they re-upload to the atlas.
    fn mark_segment_dirty(&mut self, start: Vec2, end: Vec2, radius: f32) {
        let r_i32 = radius.ceil() as i32;

        let min_x_f = start.x.min(end.x).floor() as i32 - r_i32;
        let max_x_f = start.x.max(end.x).ceil() as i32 + r_i32;
        let min_y_f = start.y.min(end.y).floor() as i32 - r_i32;
        let max_y_f = start.y.max(end.y).ceil() as i32 + r_i32;

        let canvas_w = self.canvas.width() as i32;
        let canvas_h = self.canvas.height() as i32;

        if max_x_f < 0 || min_x_f >= canvas_w || max_y_f < 0 || min_y_f >= canvas_h {
            return;
        }

        let min_x = min_x_f.max(0) as usize;
        let max_x = max_x_f.min(canvas_w - 1) as usize;
        let min_y = min_y_f.max(0) as usize;
        let max_y = max_y_f.min(canvas_h - 1) as usize;

        if min_x > max_x || min_y > max_y {
            return;
        }

        let min_tx = min_x / TILE_SIZE;
        let max_tx = max_x / TILE_SIZE;
        let min_ty = min_y / TILE_SIZE;
        let max_ty = max_y / TILE_SIZE;

        for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                if let Some(tile) = self.tile_mut(tx, ty) {
                    tile.dirty = true;
                    self.canvas.ensure_tile_exists(tx, ty);
                }
            }
        }
    }

    /// Get a mutable reference to a tile entry if coordinates are valid.
    fn tile_mut(&mut self, tx: usize, ty: usize) -> Option<&mut CanvasTile> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        let idx = ty * self.tiles_x + tx;
        self.tiles.get_mut(idx)
    }

    /// Begin a stroke at the given canvas coordinate and register undo state.
    fn start_stroke(&mut self, pos: Vec2) {
        self.stroke = Some(StrokeState::new());
        self.is_drawing = true;
        self.current_undo_action = Some(UndoAction { tiles: Vec::new() });
        self.modified_tiles.clear();

        if let Some(stroke) = &mut self.stroke {
            stroke.add_point(
                &self.pool,
                &self.canvas,
                &mut self.brush,
                pos,
                self.current_undo_action.as_mut().unwrap(),
                &mut self.modified_tiles,
            );
            self.mark_segment_dirty(pos, pos, self.brush.diameter / 2.0);
        }
    }

    /// Finalize the current stroke and push it to the undo stack.
    fn finish_stroke(&mut self) {
        if let Some(stroke) = &mut self.stroke {
            stroke.end();
        }
        if let Some(action) = self.current_undo_action.take() {
            if !action.tiles.is_empty() {
                if let Some(hist) = self.active_history_mut() {
                    hist.push_action(action);
                }
            }
        }
        self.stroke = None;
        self.is_drawing = false;
    }

    /// Rotate a point around a center by the given cos/sin pair.
    fn rotate_point(point: egui::Pos2, center: egui::Pos2, cos: f32, sin: f32) -> egui::Pos2 {
        let delta = point - center;
        egui::Pos2::new(
            center.x + delta.x * cos - delta.y * sin,
            center.y + delta.x * sin + delta.y * cos,
        )
    }

    /// Convert a screen-space position into canvas space considering zoom and rotation.
    fn screen_to_canvas(
        &self,
        pos: egui::Pos2,
        origin: egui::Pos2,
        canvas_center: egui::Pos2,
    ) -> (Vec2, bool) {
        let cos = self.rotation.cos();
        let sin = self.rotation.sin();
        let delta = pos - canvas_center;
        let unrotated = egui::Vec2::new(
            delta.x * cos + delta.y * sin,
            -delta.x * sin + delta.y * cos,
        );
        let point_world = canvas_center + unrotated;
        let canvas_point = (point_world - origin) / self.zoom;
        let clamped = Vec2 {
            x: canvas_point.x.clamp(0.0, self.canvas.width() as f32),
            y: canvas_point.y.clamp(0.0, self.canvas.height() as f32),
        };
        let is_inside = canvas_point.x >= 0.0
            && canvas_point.y >= 0.0
            && canvas_point.x <= self.canvas.width() as f32
            && canvas_point.y <= self.canvas.height() as f32;
        (clamped, is_inside)
    }

    /// Recreate the canvas, tile metadata, atlases and undo history with new dimensions.
    fn rebuild_canvas(
        &mut self,
        ctx: &egui::Context,
        width: usize,
        height: usize,
        background: Color32,
    ) {
        self.canvas = Canvas::new(width, height, background, TILE_SIZE);
        let layer_count = self.canvas.layers.len();
        self.histories = (0..layer_count).map(|_| History::new()).collect();
        self.layer_caches = vec![HashMap::new(); layer_count];
        self.layer_cache_dirty = vec![HashSet::new(); layer_count];
        self.layer_ui_colors = vec![Color32::from_gray(40); layer_count];
        self.layer_dragging = None;
        self.current_undo_action = None;
        self.modified_tiles.clear();
        self.stroke = None;
        self.is_drawing = false;
        self.is_panning = false;
        self.is_rotating = false;
        self.is_primary_down = false;

        self.tiles_x = (width + TILE_SIZE - 1) / TILE_SIZE;
        self.tiles_y = (height + TILE_SIZE - 1) / TILE_SIZE;

        let atlas_cols = (ATLAS_SIZE / TILE_SIZE).max(1);
        let atlas_capacity = atlas_cols * atlas_cols;
        let total_tiles = self.tiles_x * self.tiles_y;
        let atlas_count = (total_tiles + atlas_capacity - 1) / atlas_capacity;

        self.texture_generation = self.texture_generation.wrapping_add(1);
        self.atlases.clear();
        for idx in 0..atlas_count {
            let img = egui::ColorImage::new([ATLAS_SIZE, ATLAS_SIZE], Color32::TRANSPARENT);
            let texture = ctx.load_texture(
                format!("canvas_atlas_{}_{}", self.texture_generation, idx),
                img,
                TextureOptions::NEAREST,
            );
            self.atlases.push(TextureAtlas { texture });
        }

        self.tiles.clear();
        for ty in 0..self.tiles_y {
            for tx in 0..self.tiles_x {
                let flat_idx = ty * self.tiles_x + tx;
                let atlas_idx = flat_idx / atlas_capacity;
                let atlas_local = flat_idx % atlas_capacity;
                let atlas_tile_x = (atlas_local % atlas_cols) * TILE_SIZE;
                let atlas_tile_y = (atlas_local / atlas_cols) * TILE_SIZE;
                let tile_w = TILE_SIZE.min(width - tx * TILE_SIZE);
                let tile_h = TILE_SIZE.min(height - ty * TILE_SIZE);
                self.tiles.push(CanvasTile {
                    dirty: true,
                    atlas_idx,
                    atlas_x: atlas_tile_x,
                    atlas_y: atlas_tile_y,
                    pixel_w: tile_w,
                    pixel_h: tile_h,
                    tx,
                    ty,
                });
            }
        }

        self.offset = Vec2 { x: 0.0, y: 0.0 };
        self.zoom = 1.0;
        self.rotation = 0.0;
        self.first_frame = true;
    }

    pub(crate) fn apply_new_canvas(&mut self, ctx: &egui::Context) {
        let (width, height) = self.new_canvas.dimensions_in_pixels();
        self.color_model = self.new_canvas.color_model;
        let background = self.new_canvas.background_color32(self.color_model);
        self.rebuild_canvas(ctx, width, height, background);
        self.brush.color = Self::convert_color_for_model(self.brush.color, self.color_model);
    }

    fn convert_color_for_model(color: Color, model: ColorModel) -> Color {
        match model {
            ColorModel::Rgba => color,
            ColorModel::Grayscale => color,
        }
    }

    fn layer_tile_image(
        layer_idx: usize,
        tx: usize,
        ty: usize,
        canvas: &Canvas,
        layer_caches: &mut [HashMap<(usize, usize), egui::ColorImage>],
        layer_cache_dirty: &mut [HashSet<(usize, usize)>],
    ) -> egui::ColorImage {
        if let Some(dirty) = layer_cache_dirty.get_mut(layer_idx) {
            if dirty.remove(&(tx, ty)) {
                layer_caches
                    .get_mut(layer_idx)
                    .and_then(|m| m.remove(&(tx, ty)));
            }
        }

        if let Some(img) = layer_caches.get(layer_idx).and_then(|m| m.get(&(tx, ty))) {
            return img.clone();
        }

        let tile_w = TILE_SIZE.min(canvas.width() - tx * TILE_SIZE);
        let tile_h = TILE_SIZE.min(canvas.height() - ty * TILE_SIZE);
        let mut img = egui::ColorImage::new([tile_w, tile_h], Color32::TRANSPARENT);

        if let Some(data) = canvas.get_layer_tile_data(layer_idx, tx, ty) {
            let tile_size = canvas.tile_size();
            for y in 0..tile_h {
                for x in 0..tile_w {
                    let src_idx = y * tile_size + x;
                    img.pixels[y * tile_w + x] = data[src_idx];
                }
            }
        } else if layer_idx == 0 {
            for px in &mut img.pixels {
                *px = canvas.clear_color();
            }
        }

        if let Some(cache) = layer_caches.get_mut(layer_idx) {
            cache.insert((tx, ty), img.clone());
        }
        img
    }

    fn active_history_mut(&mut self) -> Option<&mut History> {
        self.histories.get_mut(self.canvas.active_layer_idx)
    }

    pub(crate) fn ensure_layer_history_len(&mut self) {
        let target = self.canvas.layers.len();
        if self.histories.len() < target {
            self.histories
                .extend((self.histories.len()..target).map(|_| History::new()));
        } else if self.histories.len() > target {
            self.histories.truncate(target);
        }
    }

    pub(crate) fn mark_all_tiles_dirty(&mut self) {
        for tile in &mut self.tiles {
            tile.dirty = true;
        }
    }

    pub(crate) fn mark_layer_tiles_with_data_dirty(&mut self, layer_idx: usize) {
        let tiles_x = self.tiles_x;
        let tiles_y = self.tiles_y;
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let has_data = self
                    .canvas
                    .lock_layer_tile_if_exists(layer_idx, tx, ty)
                    .map(|cell| cell.data.is_some())
                    .unwrap_or(false);
                if has_data {
                    if let Some(tile) = self.tile_mut(tx, ty) {
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

        let hist = self.histories.remove(from);
        self.histories.insert(to, hist);

        let cache = self.layer_caches.remove(from);
        self.layer_caches.insert(to, cache);

        let cache_dirty = self.layer_cache_dirty.remove(from);
        self.layer_cache_dirty.insert(to, cache_dirty);

        let ui_color = self.layer_ui_colors.remove(from);
        self.layer_ui_colors.insert(to, ui_color);

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
    }
}

impl eframe::App for PainterApp {
    /// Handle UI, input, painting updates, and tile uploads each frame.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle Undo/Redo
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Z)) {
            let active_idx = self.canvas.active_layer_idx;
            let affected = if ctx.input(|i| i.modifiers.shift) {
                self.histories
                    .get_mut(active_idx)
                    .map(|h| h.redo(&self.canvas))
                    .unwrap_or_default()
            } else {
                self.histories
                    .get_mut(active_idx)
                    .map(|h| h.undo(&self.canvas))
                    .unwrap_or_default()
            };

            for (tx, ty) in affected {
                if let Some(tile) = self.tile_mut(tx, ty) {
                    tile.dirty = true;
                }
            }
            ctx.request_repaint();
        }

        // Poll export tasks
        if let Some(handle) = self.export_task.as_ref() {
            if handle.is_finished() {
                let result = self
                    .export_task
                    .take()
                    .and_then(|h| h.join().ok())
                    .unwrap_or_else(|| Err("Export thread panicked".to_string()));
                self.export_in_progress = false;
                match result {
                    Ok(msg) => {
                        self.export_message = Some(msg);
                        self.show_export_modal = false;
                    }
                    Err(err) => {
                        self.export_message = Some(err);
                    }
                }
            }
        }

        // Drain progress updates
        if let Some(rx) = &self.export_progress_rx {
            for update in rx.try_iter() {
                self.export_progress = update.progress;
                if let Some(msg) = update.message {
                    self.export_message = Some(msg);
                }
            }
        }

        ui::brush_settings::brush_settings_window(ctx, &mut self.brush);
        ui::color_picker::color_picker_window(ctx, &mut self.brush, self.color_model);
        ui::brush_list::brush_list_window(ctx, &mut self.brush, &self.presets);
        ui::layers::layers_window(ctx, self);
        ui::general_settings::general_settings_ui(self, ctx);
        ui::canvas_creation::canvas_creation_modal(self, ctx);
        ui::export_modal::export_modal(self, ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.first_frame {
                let available = ui.available_size();
                let canvas_w = self.canvas.width() as f32;
                let canvas_h = self.canvas.height() as f32;

                let zoom_x = available.x / canvas_w;
                let zoom_y = available.y / canvas_h;
                self.zoom = zoom_x.min(zoom_y) * 0.9; // 90% fit
                let canvas_size = egui::vec2(canvas_w, canvas_h) * self.zoom;
                let offset = (available - canvas_size) * 0.5;
                self.offset = Vec2 {
                    x: offset.x,
                    y: offset.y,
                };
                self.first_frame = false;
            }

            // sync dirty tiles to GPU with simple LOD when zoomed out
            let lod_step = if self.disable_lod {
                1
            } else if self.zoom < 1.0 {
                (1.0 / self.zoom).ceil() as usize
            } else {
                1
            }
            .clamp(1, TILE_SIZE);

            let canvas_ref = &self.canvas;
            let dirty_images: Vec<(usize, egui::ColorImage)> = self.pool.install(|| {
                self.tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.dirty)
                    .collect::<Vec<_>>()
                    .par_iter()
                    .map(|(idx, tile)| {
                        let x = tile.tx * TILE_SIZE;
                        let y = tile.ty * TILE_SIZE;
                        let w = TILE_SIZE.min(canvas_ref.width() - x);
                        let h = TILE_SIZE.min(canvas_ref.height() - y);

                        let out_w = (w + lod_step - 1) / lod_step;
                        let out_h = (h + lod_step - 1) / lod_step;
                        let mut img = egui::ColorImage::new([out_w, out_h], Color32::TRANSPARENT);
                        canvas_ref.write_region_to_color_image(x, y, w, h, &mut img, lod_step);
                        (*idx, img)
                    })
                    .collect()
            });

            for (idx, img) in dirty_images {
                if let Some(tile) = self.tiles.get_mut(idx) {
                    let _timer = ScopeTimer::new("texture_set");
                    let img_w = img.size[0];
                    let img_h = img.size[1];
                    if let Some(atlas) = self.atlases.get_mut(tile.atlas_idx) {
                        atlas.texture.set_partial(
                            [tile.atlas_x, tile.atlas_y],
                            img,
                            TextureOptions::NEAREST,
                        );
                    }
                    tile.pixel_w = img_w;
                    tile.pixel_h = img_h;
                    tile.dirty = false;
                }
            }

            let desired_size = egui::vec2(self.canvas.width() as f32, self.canvas.height() as f32);
            let canvas_size = desired_size * self.zoom;
            let (rect, response) =
                ui.allocate_at_least(ui.available_size(), egui::Sense::click_and_drag());

            // Top-left of the canvas in UI coordinates
            let origin = rect.min + egui::vec2(self.offset.x, self.offset.y);
            let canvas_center = origin + canvas_size * 0.5;
            let cos = self.rotation.cos();
            let sin = self.rotation.sin();

            let mut meshes: Vec<egui::Mesh> = self
                .atlases
                .iter()
                .map(|atlas| egui::Mesh::with_texture(atlas.texture.id()))
                .collect();

            let half_texel = 0.5 / ATLAS_SIZE as f32;

            for tile in &self.tiles {
                let x = (tile.tx * TILE_SIZE) as f32 * self.zoom;
                let y = (tile.ty * TILE_SIZE) as f32 * self.zoom;

                let tile_w =
                    (TILE_SIZE.min(self.canvas.width() - tile.tx * TILE_SIZE)) as f32 * self.zoom;
                let tile_h =
                    (TILE_SIZE.min(self.canvas.height() - tile.ty * TILE_SIZE)) as f32 * self.zoom;

                let tile_rect = egui::Rect::from_min_size(
                    origin + egui::vec2(x, y),
                    egui::vec2(tile_w, tile_h),
                );

                let corners = [
                    Self::rotate_point(tile_rect.left_top(), canvas_center, cos, sin),
                    Self::rotate_point(tile_rect.right_top(), canvas_center, cos, sin),
                    Self::rotate_point(tile_rect.right_bottom(), canvas_center, cos, sin),
                    Self::rotate_point(tile_rect.left_bottom(), canvas_center, cos, sin),
                ];

                let u0 = (tile.atlas_x as f32 + half_texel) / ATLAS_SIZE as f32;
                let v0 = (tile.atlas_y as f32 + half_texel) / ATLAS_SIZE as f32;
                let u1 =
                    (tile.atlas_x as f32 + tile.pixel_w as f32 - half_texel) / ATLAS_SIZE as f32;
                let v1 =
                    (tile.atlas_y as f32 + tile.pixel_h as f32 - half_texel) / ATLAS_SIZE as f32;

                let uv_coords = [
                    egui::Pos2::new(u0, v0),
                    egui::Pos2::new(u1, v0),
                    egui::Pos2::new(u1, v1),
                    egui::Pos2::new(u0, v1),
                ];

                if let Some(mesh) = meshes.get_mut(tile.atlas_idx) {
                    let base = mesh.vertices.len() as u32;
                    for (corner, uv) in corners.iter().zip(uv_coords.iter()) {
                        mesh.vertices.push(egui::epaint::Vertex {
                            pos: *corner,
                            uv: *uv,
                            color: Color32::WHITE,
                        });
                    }
                    mesh.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
                }
            }

            for mesh in meshes {
                if !mesh.vertices.is_empty() {
                    ui.painter().add(mesh);
                }
            }

            let events = ctx.input(|i| i.events.clone());

            for event in events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        ..
                    } => {
                        let canvas_pos = self.screen_to_canvas(pos, origin, canvas_center);
                        match button {
                            egui::PointerButton::Primary => {
                                self.is_primary_down = pressed;
                                let (space_down, secondary_down) = ctx.input(|i| {
                                    (
                                        i.key_down(egui::Key::Space),
                                        i.pointer.button_down(egui::PointerButton::Secondary),
                                    )
                                });
                                if pressed && space_down {
                                    self.is_panning = true;
                                }
                                if !pressed && !secondary_down {
                                    self.is_panning = false;
                                }

                                if pressed && !self.is_panning && response.hovered() {
                                    if canvas_pos.1 {
                                        self.start_stroke(canvas_pos.0);
                                    }
                                } else if !pressed {
                                    self.finish_stroke();
                                }
                            }
                            egui::PointerButton::Secondary => {
                                if pressed && response.hovered() {
                                    self.is_panning = true;
                                }
                                if !pressed {
                                    self.is_panning = false;
                                }
                            }
                            egui::PointerButton::Middle => {
                                if pressed && response.hovered() {
                                    self.is_rotating = true;
                                }
                                if !pressed {
                                    self.is_rotating = false;
                                }
                            }
                            _ => {}
                        }
                    }

                    egui::Event::PointerMoved(pos) => {
                        let delta = ctx.input(|i| i.pointer.delta());
                        if self.is_rotating {
                            self.rotation += delta.x * -0.005;
                            ctx.request_repaint();
                        } else if self.is_panning {
                            self.offset.x += delta.x;
                            self.offset.y += delta.y;
                            ctx.request_repaint();
                        } else if self.is_drawing {
                            let (clamped, _is_inside) =
                                self.screen_to_canvas(pos, origin, canvas_center);
                            if let Some(stroke) = &mut self.stroke {
                                let prev = stroke.last_pos.unwrap_or(clamped);
                                stroke.add_point(
                                    &self.pool,
                                    &self.canvas,
                                    &mut self.brush,
                                    clamped,
                                    self.current_undo_action.as_mut().unwrap(),
                                    &mut self.modified_tiles,
                                );
                                self.mark_segment_dirty(prev, clamped, self.brush.diameter / 2.0);
                            }
                        } else if self.is_primary_down && !self.is_panning && response.hovered() {
                            let (clamped, is_inside) =
                                self.screen_to_canvas(pos, origin, canvas_center);
                            if is_inside {
                                self.start_stroke(clamped);
                            }
                        }
                    }

                    egui::Event::MouseWheel { unit, delta, .. } => {
                        if response.hovered() {
                            let scroll = match unit {
                                egui::MouseWheelUnit::Point => delta.y / 120.0_f32,
                                egui::MouseWheelUnit::Line => delta.y,
                                egui::MouseWheelUnit::Page => delta.y * 10.0_f32,
                            };
                            let zoom_factor = (1.0 - scroll * 0.1_f32).clamp(0.5_f32, 2.0_f32);
                            self.zoom = (self.zoom * zoom_factor).clamp(0.1, 20.0);
                            ctx.request_repaint();
                        }
                    }

                    _ => {}
                }
            }

            egui::TopBottomPanel::top("quick settings").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("New Canvas").clicked() {
                        self.new_canvas.sync_from_canvas(&self.canvas);
                        self.new_canvas.color_model = self.color_model;
                        self.show_new_canvas_modal = true;
                    }
                    ui.add(egui::Slider::new(&mut self.brush.diameter, 1.0..=300.0));
                    if ui.button("Export").clicked() {
                        self.export_settings.chosen_path = None;
                        self.export_message = None;
                        self.show_export_modal = true;
                    }
                });
            });

            // 4) Request repaint only while drawing
            if self.is_drawing {
                ctx.request_repaint();
            }

            if ui.input(|i| i.key_pressed(egui::Key::C)) {
                self.canvas.clear(Color::white());
                // mark all tiles dirty
                for tile in &mut self.tiles {
                    tile.dirty = true;
                }
                ctx.request_repaint();
            }
        });

        // Soft cap frame rate to reduce CPU/GPU load when many tiles are present.
        ctx.request_repaint_after(Duration::from_millis(10));
    }
}
