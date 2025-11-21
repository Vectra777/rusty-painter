use eframe::egui;
use eframe::egui::{Color32, TextureHandle, TextureOptions};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::thread;
mod brush;
mod canvas;
mod color;
mod profiler;
mod vector;

use crate::brush::{Brush, StrokeState};
use crate::canvas::Canvas;
use crate::color::Color;
use crate::profiler::ScopeTimer;
use crate::vector::Vec2;

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
    stroke: Option<StrokeState>,
    is_drawing: bool,

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

        let brush = Brush::new(12.0, 0.2, Color::rgba(0, 0, 0, 255), 0.25);

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
            stroke: None,
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
            is_drawing: false,
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

fn color_picker(ctx: &egui::Context, app: &mut PainterApp) {
    egui::Window::new("Color Picker").show(&ctx, |ui| {
        ui.label("Select Brush Color:");
        let (mut hue, mut sat, mut val, mut alpha) = app.brush.color.to_hsva();
        let mut color_changed = false;

        fn barycentric(
            p: egui::Pos2,
            a: egui::Pos2,
            b: egui::Pos2,
            c: egui::Pos2,
        ) -> (f32, f32, f32) {
            let v0 = b - a;
            let v1 = c - a;
            let v2 = p - a;
            let denom = v0.x * v1.y - v1.x * v0.y;
            if denom.abs() < f32::EPSILON {
                return (0.0, 0.0, 0.0);
            }
            let inv_denom = 1.0 / denom;
            let v = (v2.x * v1.y - v1.x * v2.y) * inv_denom;
            let w = (v0.x * v2.y - v2.x * v0.y) * inv_denom;
            let u = 1.0 - v - w;
            (u, v, w)
        }

        let side = 220.0;
        let tri_height = side * (3.0_f32).sqrt() * 0.5;
        let (rect, response) =
            ui.allocate_at_least(egui::vec2(side, tri_height), egui::Sense::click_and_drag());

        let base_x = rect.center().x - side * 0.5;
        let base_y = rect.top();
        let tri_top = egui::pos2(base_x + side * 0.5, base_y);
        let tri_left = egui::pos2(base_x, base_y + tri_height);
        let tri_right = egui::pos2(base_x + side, base_y + tri_height);

        let hue_color = Color::from_hsva(hue, 1.0, 1.0, 1.0).to_color32();

        let mut mesh = egui::Mesh::default();
        let top_idx = mesh.vertices.len();
        mesh.vertices.push(egui::epaint::Vertex {
            pos: tri_top,
            uv: egui::Pos2::ZERO,
            color: Color32::WHITE,
        });
        let left_idx = mesh.vertices.len();
        mesh.vertices.push(egui::epaint::Vertex {
            pos: tri_left,
            uv: egui::Pos2::ZERO,
            color: Color32::BLACK,
        });
        let right_idx = mesh.vertices.len();
        mesh.vertices.push(egui::epaint::Vertex {
            pos: tri_right,
            uv: egui::Pos2::ZERO,
            color: hue_color,
        });
        mesh.indices
            .extend_from_slice(&[top_idx as u32, left_idx as u32, right_idx as u32]);
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter().add(egui::Shape::line(
            vec![tri_top, tri_right, tri_left, tri_top],
            egui::Stroke::new(1.5, Color32::from_gray(80)),
        ));

        let tri_sat = sat.clamp(0.0, 1.0);
        let tri_val = val.clamp(0.0, 1.0);
        let mut w_hue = tri_sat.min(tri_val);
        let mut w_white = (tri_val - w_hue).max(0.0);
        let mut w_black = (1.0 - w_hue - w_white).max(0.0);
        let sum = w_hue + w_white + w_black;
        if sum > 0.0 {
            w_hue /= sum;
            w_white /= sum;
            w_black /= sum;
        }
        let indicator = egui::pos2(
            tri_top.x * w_white + tri_left.x * w_black + tri_right.x * w_hue,
            tri_top.y * w_white + tri_left.y * w_black + tri_right.y * w_hue,
        );
        ui.painter()
            .circle_filled(indicator, 7.0, Color32::from_white_alpha(32));
        ui.painter().circle_stroke(
            indicator,
            7.0,
            egui::Stroke::new(2.0, Color32::from_gray(30)),
        );

        let pointer_down = ui.input(|i| i.pointer.primary_down());
        if (response.hovered() || response.dragged()) && pointer_down {
            if let Some(pointer) = response.interact_pointer_pos() {
                let (w_top, w_left, w_right) = barycentric(pointer, tri_top, tri_left, tri_right);
                if w_top >= -0.01 && w_left >= -0.01 && w_right >= -0.01 {
                    let mut w_top = w_top.clamp(0.0, 1.0);
                    let w_left = w_left.clamp(0.0, 1.0);
                    let mut w_right = w_right.clamp(0.0, 1.0);
                    let total = w_top + w_left + w_right;
                    if total > 0.0 {
                        w_top /= total;
                        w_right /= total;
                    }
                    sat = w_right;
                    val = (w_top + w_right).clamp(0.0, 1.0);
                    color_changed = true;
                }
            }
        }

        ui.add_space(8.0);
        fn draw_checkerboard(painter: &egui::Painter, rect: egui::Rect, cell: f32) {
            let rows = ((rect.height() / cell).ceil() as i32).max(1);
            let cols = ((rect.width() / cell).ceil() as i32).max(1);
            for y in 0..rows {
                for x in 0..cols {
                    let dark = (x + y) % 2 == 0;
                    let min =
                        egui::pos2(rect.left() + x as f32 * cell, rect.top() + y as f32 * cell);
                    let max = min + egui::vec2(cell, cell);
                    painter.rect_filled(
                        egui::Rect::from_min_max(min, max.min(rect.max)),
                        2.0,
                        if dark {
                            Color32::from_gray(200)
                        } else {
                            Color32::from_gray(240)
                        },
                    );
                }
            }
        }

        let gradient_slider = |ui: &mut egui::Ui,
                               value: &mut f32,
                               label: &str,
                               color_at: &dyn Fn(f32) -> Color32,
                               checker: bool|
         -> bool {
            ui.label(label);
            let bar_height = 18.0;
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(side, bar_height), egui::Sense::click_and_drag());
            let painter = ui.painter();
            let radius = bar_height * 0.5;
            if checker {
                draw_checkerboard(painter, rect, 8.0);
            }

            let steps = 64;
            let mut mesh = egui::Mesh::default();
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let x = egui::lerp(rect.x_range(), t);
                let color = color_at(t);
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(x, rect.top()),
                    uv: egui::Pos2::ZERO,
                    color,
                });
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(x, rect.bottom()),
                    uv: egui::Pos2::ZERO,
                    color,
                });
                if i > 0 {
                    let base = (i * 2) as u32;
                    mesh.indices.extend_from_slice(&[
                        base - 2,
                        base - 1,
                        base,
                        base - 1,
                        base + 1,
                        base,
                    ]);
                }
            }
            painter.add(egui::Shape::mesh(mesh));

            painter.rect_stroke(
                rect.expand(0.25),
                radius,
                egui::Stroke::new(1.0, Color32::from_gray(80)),
            );

            let t = value.clamp(0.0, 1.0);
            let handle_x = egui::lerp(rect.x_range(), t);
            let handle_rect = egui::Rect::from_center_size(
                egui::pos2(handle_x, rect.center().y),
                egui::vec2(8.0, bar_height + 4.0),
            );
            painter.rect_filled(handle_rect, radius, Color32::from_white_alpha(180));
            painter.rect_stroke(
                handle_rect,
                radius,
                egui::Stroke::new(1.0, Color32::from_gray(40)),
            );

            let pointer_down = ui.input(|i| i.pointer.primary_down());
            if (response.hovered() || response.dragged()) && pointer_down {
                if let Some(pos) = response.interact_pointer_pos() {
                    let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    if (t - *value).abs() > f32::EPSILON {
                        *value = t;
                        return true;
                    }
                }
            }
            false
        };

        color_changed |= gradient_slider(
            ui,
            &mut hue,
            "Hue:",
            &|t| Color::from_hsva(t, 1.0, 1.0, 1.0).to_color32(),
            false,
        );
        color_changed |= gradient_slider(
            ui,
            &mut val,
            "Brightness:",
            &|t| Color::from_hsva(hue, sat, t, 1.0).to_color32(),
            false,
        );
        color_changed |= gradient_slider(
            ui,
            &mut sat,
            "Saturation:",
            &|t| Color::from_hsva(hue, t, val, 1.0).to_color32(),
            false,
        );
        color_changed |= gradient_slider(
            ui,
            &mut alpha,
            "Opacity:",
            &|t| Color::from_hsva(hue, sat, val, t).to_color32(),
            true,
        );

        if color_changed {
            app.brush.color = Color::from_hsva(hue, sat, val, alpha);
        }
    });
}

impl eframe::App for PainterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

            color_picker(ctx, self);

            ui.heading("Rust Dab Painter (eframe + egui)");
            ui.label("Left click to paint. 'C' to clear.");
            ui.checkbox(&mut self.use_masked_brush, "Use masked brush (fast)");
            self.brush.set_masked(self.use_masked_brush);
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

                                    if let Some(stroke) = &mut self.stroke {
                                        stroke.add_point(
                                            &self.pool,
                                            &self.canvas,
                                            &mut self.brush,
                                            pos,
                                        );
                                        self.mark_segment_dirty(pos, pos, self.brush.radius);
                                    }
                                }
                            } else {
                                // Button released -> end stroke
                                if let Some(stroke) = &mut self.stroke {
                                    stroke.end();
                                }
                                self.stroke = None;
                                self.is_drawing = false;
                            }
                        } else if !pressed {
                            // Released outside canvas: also end stroke if any
                            if let Some(stroke) = &mut self.stroke {
                                stroke.end();
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
                                );
                                self.mark_segment_dirty(prev, clamped, self.brush.radius);
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
