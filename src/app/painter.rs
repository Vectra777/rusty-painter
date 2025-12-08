use super::{
    layout::{self, ToolTab},
    state::{CanvasTile, ColorModel, NewCanvasSettings, TextureAtlas, TILE_SIZE, ATLAS_SIZE},
};
use crate::{
    brush_engine::{brush::{Brush, BrushPreset}, stroke::StrokeState},
    canvas::{
        canvas::Canvas,
        history::{History, UndoAction},
    },
    tablet::TabletInput,
    ui,
    ui::brush_settings::BrushPreviewState,
    utils::vector::Vec2,
};
use crate::app::render_helper;
use crate::app::input_handler;
use crate::brush_engine::brush_options::{BlendMode, PixelBrushShape};
use eframe::egui;
use eframe::egui::{Color32, TextureOptions};
use egui_dock::DockState;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
// use std::time::Duration;

use crate::selection::{SelectionManager, SelectionType};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tool {
    Brush,
    Select(SelectionType),
}

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
pub struct PainterApp {
    pub(crate) canvas: Canvas,
    pub(crate) brush: Brush,
    pub(crate) brush_preview: BrushPreviewState,
    pub(crate) presets: Vec<BrushPreset>,
    pub(crate) active_tool: Tool,
    pub(crate) selection_manager: SelectionManager,
    pub(crate) preset_previews: HashMap<String, egui::TextureHandle>,
    pub(crate) show_new_preset_modal: bool,
    pub(crate) new_preset_name: String,
    pub(crate) stroke: Option<StrokeState>,
    pub(crate) is_drawing: bool,

    pub(crate) brushes_path: PathBuf,
    pub(crate) loaded_brush_tips: Vec<(String, PixelBrushShape, Option<egui::TextureHandle>)>, // Name, Shape, Optional Preview Texture

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
    // pub(crate) force_full_upload: bool,
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
    pub(crate) show_general_settings: bool,
    pub(crate) dock_left: DockState<ToolTab>,
    pub(crate) dock_right: DockState<ToolTab>,
    pub(crate) tablet: Option<TabletInput>,
}

impl PainterApp {
    /// Initialize the UI, canvas, thread pool and GPU atlases.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let canvas_w = 4000;
        let canvas_h = 4000;
        let canvas = Canvas::new(canvas_w, canvas_h, Color32::WHITE, TILE_SIZE);
        let layer_count = canvas.layers.len();
        let new_canvas = NewCanvasSettings::from_canvas(&canvas);
        let color_model = new_canvas.color_model;

        let black = Color32::from_rgba_unmultiplied(0, 0, 0, 255);
        let brush = Brush::new(24.0, 20.0, black, 25.0);

        let presets = vec![
            BrushPreset {
                name: "Pencil (Sketch)".to_string(),
                brush: {
                    let mut b = Brush::new(6.0, 60.0, black, 10.0);
                    b.brush_options.flow = 30.0;
                    b.brush_options.opacity = 0.8;
                    b.jitter = 0.5;
                    b
                },
            },
            BrushPreset {
                name: "Ink Pen".to_string(),
                brush: {
                    let mut b = Brush::new(8.0, 100.0, black, 5.0);
                    b.stabilizer = 0.2;
                    b.brush_options.flow = 100.0;
                    b
                },
            },
            BrushPreset {
                name: "Soft Airbrush".to_string(),
                brush: {
                    let mut b = Brush::new(50.0, 0.0, black, 10.0);
                    b.brush_options.flow = 8.0;
                    b.brush_options.opacity = 0.6;
                    b
                },
            },
            BrushPreset {
                name: "Hard Round".to_string(),
                brush: Brush::new(20.0, 100.0, black, 10.0),
            },
            BrushPreset {
                name: "Eraser (Soft)".to_string(),
                brush: {
                    let mut b = Brush::new(40.0, 20.0, black, 10.0);
                    b.brush_options.blend_mode = BlendMode::Eraser;
                    b.brush_options.opacity = 0.8;
                    b
                },
            },
            BrushPreset {
                name: "Eraser (Hard)".to_string(),
                brush: {
                    let mut b = Brush::new(20.0, 100.0, black, 5.0);
                    b.brush_options.blend_mode = BlendMode::Eraser;
                    b
                },
            },
            BrushPreset {
                name: "Chalk".to_string(),
                brush: {
                    let mut b = Brush::new(30.0, 80.0, black, 40.0);
                    b.jitter = 5.0;
                    b.brush_options.flow = 50.0;
                    b
                },
            },
            BrushPreset {
                name: "Pixel Art".to_string(),
                brush: Brush::new_pixel(1.0, black),
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

        let dock_left = layout::default_left_dock();
        let dock_right = layout::default_right_dock();

        let brushes_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("brushes");

        let mut app = Self {
            canvas,
            brush,
            brush_preview: BrushPreviewState::default(),
            presets,
            active_tool: Tool::Brush,
            selection_manager: SelectionManager::new(),
            preset_previews: HashMap::new(),
            show_new_preset_modal: false,
            new_preset_name: String::new(),
            stroke: None,
            is_drawing: false,
            is_panning: false,
            is_rotating: false,
            rotation: 0.0,
            is_primary_down: false,
            brushes_path,
            loaded_brush_tips: Vec::new(),
            histories: (0..layer_count).map(|_| History::new()).collect(),
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
            disable_lod: true,
            // force_full_upload: false,
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
            show_general_settings: false,
            dock_left,
            dock_right,
            tablet: TabletInput::new(cc),
        };

        app.load_brush_tips(cc.egui_ctx.clone());
        app
    }

    pub fn load_brush_tips(&mut self, ctx: egui::Context) {
        // Create directory if it doesn't exist
        if !self.brushes_path.exists() {
            let _ = std::fs::create_dir_all(&self.brushes_path);
        }

        self.loaded_brush_tips.clear();

        if let Ok(entries) = std::fs::read_dir(&self.brushes_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if ["png", "jpg", "jpeg", "bmp"].contains(&ext.to_lowercase().as_str()) {
                            if let Ok(img) = image::open(&path) {
                                let img = img.to_luma8();
                                let width = img.width() as usize;
                                let height = img.height() as usize;
                                let data = img.into_raw();
                                
                                let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                                let shape = PixelBrushShape::Custom { width, height, data: data.clone() };
                                
                                // Create UI texture for the tip
                                // Invert for display if needed, but usually brush tips are white on black or alpha.
                                // PixelBrushShape uses 0-255 as alpha mask.
                                // Let's display it as white pixels with alpha.
                                let mut pixels = Vec::with_capacity(width * height);
                                for &alpha in &data {
                                    pixels.push(Color32::from_white_alpha(alpha));
                                }
                                let texture_img = egui::ColorImage {
                                    size: [width, height],
                                    pixels,
                                };
                                let texture = ctx.load_texture(
                                    format!("brush_tip_{}", name),
                                    texture_img,
                                    TextureOptions::NEAREST,
                                );

                                self.loaded_brush_tips.push((name, shape, Some(texture)));
                            }
                        }
                    }
                }
            }
        }
        self.loaded_brush_tips.sort_by(|a, b| a.0.cmp(&b.0));
    }

    /// Mark all tiles that intersect a stroke segment as dirty so they re-upload to the atlas.
    pub(crate) fn mark_segment_dirty(&mut self, start: Vec2, end: Vec2, radius: f32) {
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
    pub(crate) fn start_stroke(&mut self, pos: Vec2) {
        // Check if active layer is locked
        if self.canvas.layers.get(self.canvas.active_layer_idx).map(|l| l.locked).unwrap_or(false) {
            return;
        }

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
            self.mark_segment_dirty(pos, pos, self.brush.brush_options.diameter / 2.0);
        }
    }

    /// Finalize the current stroke and push it to the undo stack.
    pub(crate) fn finish_stroke(&mut self) {
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
    pub(crate) fn rotate_point(point: egui::Pos2, center: egui::Pos2, cos: f32, sin: f32) -> egui::Pos2 {
        let delta = point - center;
        egui::Pos2::new(
            center.x + delta.x * cos - delta.y * sin,
            center.y + delta.x * sin + delta.y * cos,
        )
    }

    /// Convert a screen-space position into canvas space considering zoom and rotation.
    pub(crate) fn screen_to_canvas(
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
        self.brush.brush_options.color = Self::convert_color_for_model(self.brush.brush_options.color, self.color_model);
    }

    fn convert_color_for_model(color: Color32, model: ColorModel) -> Color32 {
        match model {
            ColorModel::Rgba => color,
            ColorModel::Grayscale => color,
        }
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

        ui::top_bar::top_bar(self, ctx);

        layout::show_tool_docks(self, ctx);

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

            render_helper::update_dirty_textures(self);
            let view = render_helper::draw_canvas(self, ui);

            input_handler::handle_input(
                self,
                ctx,
                &view.response,
                view.origin,
                view.canvas_center,
            );

            if self.is_drawing {
                ctx.request_repaint();
            }

            self.selection_manager.draw_overlay(
                ui.painter(),
                self.zoom,
                view.origin,
                self.canvas.height() as f32,
            );

            if ui.input(|i| i.key_pressed(egui::Key::C)) {
                self.canvas.clear(Color32::WHITE);
                for tile in &mut self.tiles {
                    tile.dirty = true;
                }
                ctx.request_repaint();
            }
        });

        ui::canvas_creation::canvas_creation_modal(self, ctx);
        ui::general_settings::general_settings_modal(self, ctx);
        ui::export_modal::export_modal(self, ctx);
    }
}
