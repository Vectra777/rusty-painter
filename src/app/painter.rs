use super::{
    layout::{self, ToolTab},
    painter_helpers::BrushData,
    painter_state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
    state::{ColorModel, NewCanvasSettings, TILE_SIZE},
};
use crate::app::input_handler;
use crate::app::render_helper;
use crate::brush_engine::brush_options::{BlendMode, PixelBrushShape};
use crate::{
    brush_engine::{
        brush::{Brush, BrushPreset},
        stroke::{StrokeContext, StrokeState},
    },
    canvas::{
        Canvas,
        history::{History, UndoAction},
    },
    tablet::TabletInput,
    ui,
};
use eframe::egui;
use eframe::egui::{Color32, TextureOptions, Vec2};
use egui_dock::DockState;
use rayon::ThreadPoolBuilder;
use std::path::PathBuf;
use std::thread;
// use std::time::Duration;

use crate::selection::SelectionManager;

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
pub struct PainterApp {
    pub(crate) canvas: Canvas,

    // Grouped state
    pub(crate) brush_state: BrushState,
    pub(crate) viewport: ViewportState,
    pub(crate) render_cache: RenderCache,
    pub(crate) layer_state: LayerState,
    pub(crate) modal_state: ModalState,
    pub(crate) export_state: ExportState,
    pub(crate) workspace: WorkspaceState,

    // Standalone components
    pub(crate) active_tool: super::tools::Tool,
    pub(crate) selection_manager: SelectionManager,
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

        let presets = Self::create_default_brush_presets(black);
        let workspace = Self::create_workspace(color_model);
        let render_cache =
            Self::initialize_render_cache(&cc.egui_ctx, canvas_w, canvas_h, layer_count);
        let dock_left = layout::default_left_dock();
        let dock_right = layout::default_right_dock();

        let brush_state = BrushState::new(
            Brush::new(24.0, 20.0, black, 25.0),
            presets,
            Self::get_brushes_path(),
            true,
        );
        let viewport = ViewportState::new(1.0, Vec2 { x: 300.0, y: 100.0 });
        let layer_state = LayerState::new(layer_count);
        let modal_state = ModalState::new(new_canvas);
        let export_state = ExportState::new();

        let mut app = Self {
            canvas,
            brush_state,
            viewport,
            render_cache,
            layer_state,
            modal_state,
            export_state,
            workspace,
            active_tool: super::tools::Tool::Brush,
            selection_manager: SelectionManager::new(),
            dock_left,
            dock_right,
            tablet: TabletInput::new(cc),
        };

        app.load_brush_tips(cc.egui_ctx.clone());
        app
    }

    /// Create default brush presets.
    fn create_default_brush_presets(black: Color32) -> Vec<BrushPreset> {
        vec![
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
        ]
    }

    /// Create workspace with thread pool.
    fn create_workspace(color_model: ColorModel) -> WorkspaceState {
        let max_threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .max(1);
        let pool = ThreadPoolBuilder::new()
            .num_threads(max_threads)
            .build()
            .expect("failed to build thread pool");
        WorkspaceState::new(max_threads, max_threads, pool, color_model)
    }

    /// Get the path to the brushes directory.
    fn get_brushes_path() -> PathBuf {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("brushes")
    }

    pub fn load_brush_tips(&mut self, ctx: egui::Context) {
        self.ensure_brushes_directory_exists();
        self.brush_state.loaded_brush_tips.clear();
        self.scan_and_load_brush_images(ctx);
        self.sort_loaded_brushes();
    }

    /// Create brushes directory if it doesn't exist.
    fn ensure_brushes_directory_exists(&self) {
        if !self.brush_state.brushes_path.exists() {
            let _ = std::fs::create_dir_all(&self.brush_state.brushes_path);
        }
    }

    /// Scan directory and load all valid brush tip images.
    fn scan_and_load_brush_images(&mut self, ctx: egui::Context) {
        if let Ok(entries) = std::fs::read_dir(&self.brush_state.brushes_path) {
            for entry in entries.flatten() {
                if let Some(brush_tip) = self.try_load_brush_from_path(entry.path(), &ctx) {
                    self.brush_state.loaded_brush_tips.push(brush_tip);
                }
            }
        }
    }

    /// Try to load a brush tip from a file path.
    fn try_load_brush_from_path(
        &self,
        path: std::path::PathBuf,
        ctx: &egui::Context,
    ) -> Option<(String, PixelBrushShape, Option<egui::TextureHandle>)> {
        if !path.is_file() || !Self::is_valid_image_extension(&path) {
            return None;
        }
        let img = image::open(&path).ok()?.to_luma8();
        let brush_data = Self::extract_brush_data(&img);
        let texture = Self::create_brush_texture(&brush_data, ctx);
        Some(brush_data.into_brush_tip(texture))
    }

    /// Check if path has a valid image extension.
    fn is_valid_image_extension(path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .map(|ext| ["png", "jpg", "jpeg", "bmp"].contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
    }

    /// Extract brush data from a loaded image.
    fn extract_brush_data(img: &image::GrayImage) -> BrushData {
        BrushData {
            width: img.width() as usize,
            height: img.height() as usize,
            data: img.clone().into_raw(),
        }
    }

    /// Create UI texture for brush tip visualization.
    fn create_brush_texture(brush_data: &BrushData, ctx: &egui::Context) -> egui::TextureHandle {
        let pixels: Vec<Color32> = brush_data
            .data
            .iter()
            .map(|&alpha| Color32::from_white_alpha(alpha))
            .collect();
        let texture_img = egui::ColorImage {
            size: [brush_data.width, brush_data.height],
            pixels,
        };
        ctx.load_texture(
            format!("brush_tip_{}", brush_data.width),
            texture_img,
            TextureOptions::NEAREST,
        )
    }

    /// Sort loaded brushes alphabetically by name.
    fn sort_loaded_brushes(&mut self) {
        self.brush_state
            .loaded_brush_tips
            .sort_by(|a, b| a.0.cmp(&b.0));
    }

    /// Begin a stroke at the given canvas coordinate and register undo state.
    pub(crate) fn start_stroke(&mut self, pos: Vec2) {
        if self.is_active_layer_locked() {
            return;
        }
        self.initialize_stroke_state();
        self.add_initial_stroke_point(pos);
    }

    /// Check if the active layer is locked.
    fn is_active_layer_locked(&self) -> bool {
        self.canvas
            .layers
            .get(self.canvas.active_layer_idx)
            .map(|l| l.locked)
            .unwrap_or(false)
    }

    /// Initialize stroke state for a new stroke.
    fn initialize_stroke_state(&mut self) {
        self.brush_state.stroke = Some(StrokeState::new());
        self.brush_state.is_drawing = true;
        self.layer_state.current_undo_action = Some(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
        });
        self.render_cache.modified_tiles.clear();
    }

    /// Add the first point to the newly started stroke.
    fn add_initial_stroke_point(&mut self, pos: Vec2) {
        if let Some(stroke) = &mut self.brush_state.stroke {
            let has_selection = self.selection_manager.has_selection();
            let selection = if has_selection {
                Some(&self.selection_manager)
            } else {
                None
            };
            let mut context = StrokeContext::new(
                &self.workspace.pool,
                &self.canvas,
                selection,
                self.layer_state.current_undo_action.as_mut().unwrap(),
                &mut self.render_cache.modified_tiles,
            );
            stroke.add_point(&mut self.brush_state.brush, pos, &mut context);
            self.mark_segment_dirty(
                pos,
                pos,
                self.brush_state.brush.brush_options.diameter / 2.0,
            );
        }
    }

    /// Get selection reference if active, otherwise None.
    /// Finalize the current stroke and push it to the undo stack.
    pub(crate) fn finish_stroke(&mut self) {
        self.end_current_stroke();
        self.save_undo_action_if_valid();
        self.clear_stroke_state();
    }

    /// End the stroke recording.
    fn end_current_stroke(&mut self) {
        if let Some(stroke) = &mut self.brush_state.stroke {
            stroke.end();
        }
    }

    /// Save the undo action if it contains tile changes.
    fn save_undo_action_if_valid(&mut self) {
        if let Some(action) = self.layer_state.current_undo_action.take()
            && !action.tiles.is_empty()
            && let Some(hist) = self.active_history_mut()
        {
            hist.push_action(action);
        }
    }

    /// Clear stroke drawing state.
    fn clear_stroke_state(&mut self) {
        self.brush_state.stroke = None;
        self.brush_state.is_drawing = false;
    }

    /// Rotate a point around a center by the given cos/sin pair.
    pub(crate) fn rotate_point(
        point: egui::Pos2,
        center: egui::Pos2,
        cos: f32,
        sin: f32,
    ) -> egui::Pos2 {
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
        let unrotated = self.unrotate_point_around_center(pos, canvas_center);
        let world_point = canvas_center + unrotated;
        let canvas_point = self.world_to_canvas_coords(world_point, origin);
        let clamped = self.clamp_to_canvas_bounds(canvas_point);
        let is_inside = self.is_point_in_canvas(canvas_point);
        (clamped, is_inside)
    }

    /// Unrotate a point around the canvas center.
    fn unrotate_point_around_center(&self, pos: egui::Pos2, center: egui::Pos2) -> egui::Vec2 {
        let cos = self.viewport.rotation.cos();
        let sin = self.viewport.rotation.sin();
        let delta = pos - center;
        egui::Vec2::new(
            delta.x * cos + delta.y * sin,
            -delta.x * sin + delta.y * cos,
        )
    }

    /// Convert world coordinates to canvas coordinates.
    fn world_to_canvas_coords(&self, world: egui::Pos2, origin: egui::Pos2) -> egui::Pos2 {
        let delta = (world - origin) / self.viewport.zoom;
        egui::Pos2::new(delta.x, delta.y)
    }

    /// Clamp point to canvas dimensions.
    fn clamp_to_canvas_bounds(&self, point: egui::Pos2) -> Vec2 {
        Vec2 {
            x: point.x.clamp(0.0, self.canvas.width() as f32),
            y: point.y.clamp(0.0, self.canvas.height() as f32),
        }
    }

    /// Check if point is within canvas bounds.
    fn is_point_in_canvas(&self, point: egui::Pos2) -> bool {
        point.x >= 0.0
            && point.y >= 0.0
            && point.x <= self.canvas.width() as f32
            && point.y <= self.canvas.height() as f32
    }

    fn active_history_mut(&mut self) -> Option<&mut History> {
        self.layer_state
            .histories
            .get_mut(self.canvas.active_layer_idx)
    }

    pub fn draw_transform_overlay(&mut self, painter: &egui::Painter, origin: egui::Pos2) {
        if let super::tools::Tool::Transform(ref mut info) = self.active_tool {
            // If bounds are not set, try to set them
            if info.bounds.is_none() {
                info.bounds = self.canvas.get_content_bounds(
                    self.canvas.active_layer_idx,
                    if self.selection_manager.has_selection() {
                        Some(&self.selection_manager)
                    } else {
                        None
                    },
                );
            }

            if let Some(bounds) = info.bounds {
                let center = Vec2::new(bounds.center().x, bounds.center().y);
                let (sin_r, cos_r) = info.rotation.sin_cos();

                // Helper to transform a point
                let transform_point = |p: egui::Pos2| -> egui::Pos2 {
                    let dx = p.x - center.x;
                    let dy = p.y - center.y;

                    let sx = dx * info.scale.x;
                    let sy = dy * info.scale.y;

                    let rx = sx * cos_r - sy * sin_r;
                    let ry = sx * sin_r + sy * cos_r;

                    let tx = rx + center.x + info.offset.x;
                    let ty = ry + center.y + info.offset.y;

                    egui::pos2(
                        origin.x + tx * self.viewport.zoom,
                        origin.y + ty * self.viewport.zoom,
                    )
                };

                let corners = [
                    bounds.min,                                     // Top-Left
                    eframe::egui::pos2(bounds.max.x, bounds.min.y), // Top-Right
                    bounds.max,                                     // Bottom-Right
                    eframe::egui::pos2(bounds.min.x, bounds.max.y), // Bottom-Left
                ];

                let t_corners = [
                    transform_point(corners[0]),
                    transform_point(corners[1]),
                    transform_point(corners[2]),
                    transform_point(corners[3]),
                ];

                // Draw box
                let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 120, 255));
                painter.line_segment([t_corners[0], t_corners[1]], stroke);
                painter.line_segment([t_corners[1], t_corners[2]], stroke);
                painter.line_segment([t_corners[2], t_corners[3]], stroke);
                painter.line_segment([t_corners[3], t_corners[0]], stroke);

                // Draw handles
                let handle_points = [
                    bounds.min,                                          // Top-Left
                    eframe::egui::pos2(bounds.center().x, bounds.min.y), // Top-Center
                    eframe::egui::pos2(bounds.max.x, bounds.min.y),      // Top-Right
                    eframe::egui::pos2(bounds.max.x, bounds.center().y), // Right-Center
                    bounds.max,                                          // Bottom-Right
                    eframe::egui::pos2(bounds.center().x, bounds.max.y), // Bottom-Center
                    eframe::egui::pos2(bounds.min.x, bounds.max.y),      // Bottom-Left
                    eframe::egui::pos2(bounds.min.x, bounds.center().y), // Left-Center
                ];

                for p in handle_points {
                    let tp = transform_point(p);
                    painter.circle_filled(tp, 4.0, egui::Color32::WHITE);
                    painter.circle_stroke(tp, 4.0, stroke);
                }
            }
        }
    }
}

impl eframe::App for PainterApp {
    /// Handle UI, input, painting updates, and tile uploads each frame.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut needs_repaint = false;

        // Cache input state for this frame
        let (ctrl_z_pressed, shift_held) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(egui::Key::Z),
                i.modifiers.shift,
            )
        });

        // Handle Undo/Redo
        if ctrl_z_pressed {
            let active_idx = self.canvas.active_layer_idx;
            let affected = if shift_held {
                self.layer_state
                    .histories
                    .get_mut(active_idx)
                    .map(|h| {
                        h.redo(
                            &self.canvas,
                            &mut self.selection_manager,
                            &mut self.active_tool,
                        )
                    })
                    .unwrap_or_default()
            } else {
                self.layer_state
                    .histories
                    .get_mut(active_idx)
                    .map(|h| {
                        h.undo(
                            &self.canvas,
                            &mut self.selection_manager,
                            &mut self.active_tool,
                        )
                    })
                    .unwrap_or_default()
            };

            for (tx, ty) in affected {
                if let Some(tile) = self.tile_mut(tx.max(0) as usize, ty.max(0) as usize) {
                    tile.dirty = true;
                }
            }

            // Reset transform tool state if active so it recalculates bounds
            // Only reset if the undo action didn't restore a transform state
            if let super::tools::Tool::Transform(ref mut info) = self.active_tool
                && info.bounds.is_none()
                && info.rotation == 0.0
                && info.offset.x == 0.0
                && info.offset.y == 0.0
            {
                *info = crate::selection::transform::TransformInfo::default();
            }

            needs_repaint = true;
        }

        // Poll export tasks
        if let Some(handle) = self.export_state.task.as_ref()
            && handle.is_finished()
        {
            let result = self
                .export_state
                .task
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("Export thread panicked".to_string()));
            self.export_state.in_progress = false;
            match result {
                Ok(msg) => {
                    self.export_state.message = Some(msg);
                    self.export_state.show_modal = false;
                }
                Err(err) => {
                    self.export_state.message = Some(err);
                }
            }
        }

        // Drain progress updates
        if let Some(rx) = &self.export_state.progress_rx {
            for update in rx.try_iter() {
                self.export_state.progress = update.progress;
                if let Some(msg) = update.message {
                    self.export_state.message = Some(msg);
                }
            }
        }

        ui::top_bar::top_bar(self, ctx);

        layout::show_tool_docks(self, ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.workspace.first_frame {
                let available = ui.available_size();
                let canvas_w = self.canvas.width() as f32;
                let canvas_h = self.canvas.height() as f32;

                let zoom_x = available.x / canvas_w;
                let zoom_y = available.y / canvas_h;
                self.viewport.zoom = zoom_x.min(zoom_y) * 0.9; // 90% fit
                let canvas_size = egui::vec2(canvas_w, canvas_h) * self.viewport.zoom;
                let offset = (available - canvas_size) * 0.5;
                self.viewport.offset = Vec2 {
                    x: offset.x,
                    y: offset.y,
                };
                self.workspace.first_frame = false;
            }

            render_helper::update_dirty_textures(self);
            let view = render_helper::draw_canvas(self, ui);

            input_handler::handle_input(self, ctx, &view.response, view.origin, view.canvas_center);

            if self.brush_state.is_drawing {
                needs_repaint = true;
            }

            if !matches!(self.active_tool, super::tools::Tool::Transform(_)) {
                self.selection_manager.draw_overlay(
                    ui.painter(),
                    self.viewport.zoom,
                    view.origin,
                    self.canvas.height() as f32,
                    None,
                );
            }

            self.draw_transform_overlay(ui.painter(), view.origin);

            // Cache keyboard input for this frame
            let (c_pressed, escape_pressed) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::C),
                    i.key_pressed(egui::Key::Escape),
                )
            });

            if c_pressed {
                self.canvas.clear(Color32::WHITE);
                for tile in &mut self.render_cache.tiles {
                    tile.dirty = true;
                }
                needs_repaint = true;
            }

            if escape_pressed {
                self.selection_manager.clear_selection();
                needs_repaint = true;
            }
        });

        ui::canvas_creation::canvas_creation_modal(self, ctx);
        ui::general_settings::general_settings_modal(self, ctx);
        ui::export_modal::export_modal(self, ctx);

        // Single consolidated repaint request
        if needs_repaint {
            ctx.request_repaint();
        }
    }
}
