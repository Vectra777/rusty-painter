use eframe::egui;
use eframe::egui::{Color32, TextureHandle, TextureOptions};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::collections::HashSet;
use std::time::Duration;
use std::thread;

mod canvas;
mod ui;
mod brush_engine;
mod utils;
mod gpu_painter;

use canvas::canvas::Canvas;
use canvas::history::{History, UndoAction};
use brush_engine::brush::{Brush,BrushPreset, StrokeState};
use utils::{profiler::ScopeTimer, vector::Vec2, color::Color};

const TILE_SIZE: usize = 64;
const ATLAS_SIZE: usize = 2048;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PaintBackend {
    Cpu,
    Gpu,
}

fn parse_backend_arg() -> PaintBackend {
    let mut backend = PaintBackend::Cpu;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--gpu" | "--backend=gpu" => backend = PaintBackend::Gpu,
            "--cpu" | "--backend=cpu" => backend = PaintBackend::Cpu,
            "--backend" => {
                if let Some(next) = args.next() {
                    if next.eq_ignore_ascii_case("gpu") {
                        backend = PaintBackend::Gpu;
                    } else if next.eq_ignore_ascii_case("cpu") {
                        backend = PaintBackend::Cpu;
                    }
                }
            }
            _ => {}
        }
    }
    backend
}

/// Metadata that links a canvas tile to its slot in the GPU atlas.
struct CanvasTile {
    dirty: bool,
    atlas_idx: usize,
    atlas_x: usize,
    atlas_y: usize,
    pixel_w: usize,
    pixel_h: usize,
    // tile index in the grid
    tx: usize,
    ty: usize,
}

/// Wrapper so we can swap out atlas textures easily.
struct TextureAtlas {
    texture: TextureHandle,
}

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
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
    atlases: Vec<TextureAtlas>,
    tiles_x: usize,
    tiles_y: usize,

    zoom: f32,
    offset: Vec2,
    first_frame: bool,
    use_masked_brush: bool,
    thread_count: usize,
    max_threads: usize,
    pool: ThreadPool,
    is_panning: bool,
    is_zooming: bool,
    is_rotating: bool,
    rotation: f32,
    is_primary_down: bool,
    disable_lod: bool,
    force_full_upload: bool,
}

impl PainterApp {
    /// Initialize the UI, canvas, thread pool and GPU atlases.
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
                TextureOptions::NEAREST,
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
            is_zooming: false,
            is_rotating: false,
            rotation: 0.0,
            is_primary_down: false,
            history: History::new(),
            current_undo_action: None,
            modified_tiles: HashSet::new(),
            tiles,
            atlases,
            tiles_x,
            tiles_y,
            zoom: 1.0,
            offset: Vec2 { x: 0.0, y: 0.0 },
            first_frame: true,
            use_masked_brush: true,
            thread_count,
            max_threads,
            pool,
            disable_lod: false,
            force_full_upload: false,
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
                self.history.push_action(action);
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
}

impl eframe::App for PainterApp {
    /// Handle UI, input, painting updates, and tile uploads each frame.
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
        ui::brush_settings::brush_settings_window(ctx, &mut self.brush);
        ui::color_picker::color_picker_window(ctx, &mut self.brush);
        ui::brush_list::brush_list_window(ctx, &mut self.brush, &self.presets);
        ui::layers::layers_window(ctx, &mut self.canvas);
        ui::general_settings::general_settings_ui(self, ctx);

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
                        let mut img =
                            egui::ColorImage::new([out_w, out_h], Color32::TRANSPARENT);
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
                        atlas
                            .texture
                            .set_partial([tile.atlas_x, tile.atlas_y], img, TextureOptions::NEAREST);
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

                let tile_rect =
                    egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(tile_w, tile_h));

                let corners = [
                    Self::rotate_point(tile_rect.left_top(), canvas_center, cos, sin),
                    Self::rotate_point(tile_rect.right_top(), canvas_center, cos, sin),
                    Self::rotate_point(tile_rect.right_bottom(), canvas_center, cos, sin),
                    Self::rotate_point(tile_rect.left_bottom(), canvas_center, cos, sin),
                ];

                let u0 = (tile.atlas_x as f32 + half_texel) / ATLAS_SIZE as f32;
                let v0 = (tile.atlas_y as f32 + half_texel) / ATLAS_SIZE as f32;
                let u1 = (tile.atlas_x as f32 + tile.pixel_w as f32 - half_texel)
                    / ATLAS_SIZE as f32;
                let v1 = (tile.atlas_y as f32 + tile.pixel_h as f32 - half_texel)
                    / ATLAS_SIZE as f32;

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
                    mesh.indices
                        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
                        if button == egui::PointerButton::Middle {
                            self.is_zooming = pressed;
                        } else if button == egui::PointerButton::Secondary {
                            self.is_rotating = pressed;
                        }

                        let canvas_pos = self.screen_to_canvas(pos, origin, canvas_center);
                        if button == egui::PointerButton::Primary {
                            self.is_primary_down = pressed;
                            let space_down = ctx.input(|i| i.key_down(egui::Key::Space));
                            if pressed && space_down {
                                self.is_panning = true;
                            }
                            if !pressed {
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

                        if !pressed {
                            if button == egui::PointerButton::Middle {
                                self.is_zooming = false;
                            } else if button == egui::PointerButton::Secondary {
                                self.is_rotating = false;
                            }
                        }
                    }

                    egui::Event::PointerMoved(pos) => {
                        let delta = ctx.input(|i| i.pointer.delta());
                        if self.is_zooming {
                            let zoom_change = -delta.y * 0.005;
                            self.zoom = (self.zoom * (1.0 + zoom_change)).clamp(0.1, 20.0);
                            ctx.request_repaint();
                        } else if self.is_rotating {
                            self.rotation += delta.x * -0.005;
                            ctx.request_repaint();
                        } else if self.is_panning {
                            self.offset.x += delta.x;
                            self.offset.y += delta.y;
                            ctx.request_repaint();
                        } else if self.is_drawing {
                            let (clamped, _is_inside) = self.screen_to_canvas(pos, origin, canvas_center);
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
                                self.mark_segment_dirty(
                                    prev,
                                    clamped,
                                    self.brush.diameter / 2.0,
                                );
                            }
                        } else if self.is_primary_down && !self.is_panning && response.hovered() {
                            let (clamped, is_inside) = self.screen_to_canvas(pos, origin, canvas_center);
                            if is_inside {
                                self.start_stroke(clamped);
                            }
                        }
                    }

                    _ => {}
                }
            }

            egui::TopBottomPanel::top("quick settings").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut self.brush.diameter, 1.0..=300.0))
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

/// Launch the native egui application.
fn main() -> eframe::Result<()> {
    env_logger::init();

    match parse_backend_arg() {
        PaintBackend::Gpu => {
            println!("Launching GPU painting backend (wgpu)");
            if let Err(err) = gpu_painter::run() {
                eprintln!("Failed to start GPU backend: {err}");
            }
            Ok(())
        }
        PaintBackend::Cpu => {
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
    }
}
