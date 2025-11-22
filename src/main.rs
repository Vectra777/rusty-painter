use eframe::egui;
use eframe::egui::{Color32, TextureHandle, TextureOptions};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::thread;
use std::collections::HashSet;
mod brush;
mod canvas;
mod color;
mod profiler;
mod ui;
mod vector;
mod history;

use crate::brush::{Brush, StrokeState, BrushPreset};
use crate::canvas::Canvas;
use crate::color::Color;
use crate::profiler::ScopeTimer;
use crate::vector::Vec2;
use crate::history::{History, UndoAction};

const TILE_SIZE: usize = 256;

struct CanvasTile {
    texture: TextureHandle,
    dirty: bool,
    // tile index in the grid
    tx: usize,
    ty: usize,
}

struct PainterApp {
    canvas: Canvas,
    brush: Brush,
    presets: Vec<BrushPreset>,
    stroke: Option<StrokeState>,
    is_drawing: bool,
    
    history: History,
    current_undo_action: Option<UndoAction>,
    modified_tiles: HashSet<(usize, usize)>,

    tiles: Vec<CanvasTile>,
    tiles_x: usize,
    tiles_y: usize,

    zoom: f32,
    offset: Vec2,
    first_frame: bool,
    use_masked_brush: bool,
    thread_count: usize,
    max_threads: usize,
    pool: ThreadPool,
}

impl PainterApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let canvas_w = 8000;
        let canvas_h = 8000;
        let canvas = Canvas::new(canvas_w, canvas_h, Color::white(), TILE_SIZE);

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
                }
            },
            BrushPreset {
                name: "Stabilized".to_string(),
                brush: {
                    let mut b = Brush::new(20.0, 80.0, Color::rgba(0, 0, 0, 255), 10.0);
                    b.stabilizer = 0.8;
                    b
                }
            }
        ];

        let max_threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .max(1);
        let thread_count = max_threads.saturating_sub(1).max(1);
        let pool = ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build()
            .expect("failed to build thread pool");

        let tiles_x = (canvas_w + TILE_SIZE - 1) / TILE_SIZE;
        let tiles_y = (canvas_h + TILE_SIZE - 1) / TILE_SIZE;

        let mut tiles = Vec::new();

        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let tile_w = TILE_SIZE.min(canvas_w - tx * TILE_SIZE);
                let tile_h = TILE_SIZE.min(canvas_h - ty * TILE_SIZE);

                let img = egui::ColorImage::new([tile_w, tile_h], Color32::WHITE);

                let texture = cc.egui_ctx.load_texture(
                    format!("canvas_tile_{}_{}", tx, ty),
                    img,
                    TextureOptions::NEAREST,
                );

                tiles.push(CanvasTile {
                    texture,
                    dirty: true,
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
            history: History::new(),
            current_undo_action: None,
            modified_tiles: HashSet::new(),
            tiles,
            tiles_x,
            tiles_y,
            zoom: 1.0,
            offset: Vec2 { x: 0.0, y: 0.0 },
            first_frame: true,
            use_masked_brush: true,
            thread_count,
            max_threads,
            pool,
        }
    }

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
                    // Warm tiles so allocation happens off the upload loop.
                    self.canvas.ensure_tile_exists(tx, ty);
                }
            }
        }
    }

    fn tile_mut(&mut self, tx: usize, ty: usize) -> Option<&mut CanvasTile> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        let idx = ty * self.tiles_x + tx;
        self.tiles.get_mut(idx)
    }
}


impl eframe::App for PainterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle Undo/Redo
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Z)) {
            let affected = if ctx.input(|i| i.modifiers.shift) {
                self.history.redo(&self.canvas)
            } else {
                self.history.undo(&self.canvas)
            };
            
            for (tx, ty) in affected {
                if let Some(tile) = self.tile_mut(tx, ty) {
                    tile.dirty = true;
                }
            }
            ctx.request_repaint();
        }

        ui::brush_settings_window(ctx, &mut self.brush);
        ui::color_picker_window(ctx, &mut self.brush);
        ui::brush_list_window(ctx, &mut self.brush, &self.presets);
        ui::layers_window(ctx, &mut self.canvas);

        egui::Window::new("General Settings").show(ctx, |ui| {
            ui.checkbox(&mut self.use_masked_brush, "Use masked brush (fast)");
            // self.brush.set_masked(self.use_masked_brush);
            let threads_changed = ui
                .add(
                    egui::Slider::new(&mut self.thread_count, 1..=self.max_threads)
                        .text("Brush threads"),
                )
                .changed();
            if threads_changed {
                if let Ok(pool) = ThreadPoolBuilder::new()
                    .num_threads(self.thread_count)
                    .build()
                {
                    self.pool = pool;
                }
            }
            ui.separator();
            ui.label("Controls:");
            ui.label("Left click: Paint");
            ui.label("C: Clear Canvas");
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.first_frame {
                let available = ui.available_size();
                let canvas_w = self.canvas.width() as f32;
                let canvas_h = self.canvas.height() as f32;

                let zoom_x = available.x / canvas_w;
                let zoom_y = available.y / canvas_h;
                self.zoom = zoom_x.min(zoom_y) * 0.9; // 90% fit
                self.first_frame = false;
            }

            // sync dirty tiles to GPU, with simple LOD when zoomed out
            let lod_step = if self.zoom < 1.0 {
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
                    tile.texture.set(img, TextureOptions::NEAREST);
                    tile.dirty = false;
                }
            }

            let desired_size = egui::vec2(self.canvas.width() as f32, self.canvas.height() as f32);
            let (rect, _response) =
                ui.allocate_exact_size(desired_size * self.zoom, egui::Sense::click_and_drag());

            // Top-left of the canvas in UI coordinates
            let origin = rect.min + egui::vec2(self.offset.x, self.offset.y);

            for tile in &self.tiles {
                let x = (tile.tx * TILE_SIZE) as f32 * self.zoom;
                let y = (tile.ty * TILE_SIZE) as f32 * self.zoom;

                let w =
                    (TILE_SIZE.min(self.canvas.width() - tile.tx * TILE_SIZE)) as f32 * self.zoom;
                let h =
                    (TILE_SIZE.min(self.canvas.height() - tile.ty * TILE_SIZE)) as f32 * self.zoom;

                let tile_rect =
                    egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(w, h));

                ui.painter().image(
                    tile.texture.id(),
                    tile_rect,
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }

            let events = ctx.input(|i| i.events.clone());

            for event in events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        ..
                    } => {
                        // Only care if click is inside our canvas rect:
                        if rect.contains(pos) {
                            let local = (pos - origin) / self.zoom;
                            let pos = Vec2 {
                                x: local.x,
                                y: local.y,
                            };

                            if pressed {
                                // Start stroke:
                                if pos.x >= 0.0
                                    && pos.y >= 0.0
                                    && pos.x < self.canvas.width() as f32
                                    && pos.y < self.canvas.height() as f32
                                {
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
                            } else {
                                // Button released -> end stroke
                                if let Some(stroke) = &mut self.stroke {
                                    stroke.end();
                                }
                                if let Some(action) = self.current_undo_action.take() {
                                    if !action.tiles.is_empty() {
                                        self.history.push_action(action);
                                    }
                                }
                                self.stroke = None;
                                self.is_drawing = false;
                            }
                        } else if !pressed {
                            // Released outside canvas: also end stroke if any
                            if let Some(stroke) = &mut self.stroke {
                                stroke.end();
                            }
                            if let Some(action) = self.current_undo_action.take() {
                                if !action.tiles.is_empty() {
                                    self.history.push_action(action);
                                }
                            }
                            self.stroke = None;
                            self.is_drawing = false;
                        }
                    }

                    egui::Event::PointerMoved(pos) => {
                        if self.is_drawing {
                            let local = (pos - origin) / self.zoom;
                            // Clamp to canvas bounds so drawing continues along the edge while cursor is outside.
                            let clamped = Vec2 {
                                x: local.x.clamp(0.0, self.canvas.width() as f32),
                                y: local.y.clamp(0.0, self.canvas.height() as f32),
                            };

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
                        }
                    }

                    _ => {}
                }
            }

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
    }
}

fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rust Dab Painter",
        options,
        Box::new(|cc| Ok(Box::new(PainterApp::new(cc)))),
    )
}
