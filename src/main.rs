use eframe::egui;
use eframe::egui::{Color32, TextureHandle, TextureOptions};
mod brush;
mod canvas;
mod color;
mod vector;

use crate::brush::{Brush, StrokeState};
use crate::canvas::Canvas;
use crate::color::Color;
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

    tiles: Vec<CanvasTile>,
    tiles_x: usize,
    tiles_y: usize,

    zoom: f32,
    offset: Vec2,
    first_frame: bool,
}

impl PainterApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let canvas_w = 4000;
        let canvas_h = 4000;
        let canvas = Canvas::new(canvas_w, canvas_h, Color::white());

        let brush = Brush {
            radius: 12.0,
            hardness: 0.2,
            color: Color::rgba(0, 0, 0, 255),
            spacing: 0.25,
        };

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

            ui.heading("Rust Dab Painter (eframe + egui)");
            ui.label("Left click to paint. 'C' to clear.");

            // sync dirty tiles to GPU
            for tile in &mut self.tiles {
                if tile.dirty {
                    let x = tile.tx * TILE_SIZE;
                    let y = tile.ty * TILE_SIZE;
                    let w = TILE_SIZE.min(self.canvas.width() - x);
                    let h = TILE_SIZE.min(self.canvas.height() - y);

                    let img = self.canvas.region_to_color_image(x, y, w, h);
                    tile.texture.set(img, TextureOptions::NEAREST);
                    tile.dirty = false;
                }
            }

            let desired_size = egui::vec2(self.canvas.width() as f32, self.canvas.height() as f32);
            let (rect, response) =
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

            if response.dragged() || response.clicked() {
                if let Some(pointer_pos) = response.interact_pointer_pos() {
                    // convert from screen space to canvas space
                    let local = (pointer_pos - origin) / self.zoom;

                    let pos = Vec2 {
                        x: local.x,
                        y: local.y,
                    };

                    // ignore clicks outside the canvas
                    // if pos.x >= 0.0 && pos.y >= 0.0
                    //     && pos.x < self.canvas.width() as f32
                    //     && pos.y < self.canvas.height() as f32
                    {
                        if self.stroke.is_none() {
                            self.stroke = Some(StrokeState::new());
                        }
                        if let Some(stroke) = &mut self.stroke {
                            let prev = stroke.last_pos.unwrap_or(pos);
                            stroke.add_point(&mut self.canvas, &self.brush, pos);
                            self.mark_segment_dirty(prev, pos, self.brush.radius);
                            ctx.request_repaint();
                        }
                    }
                }
            } else if response.drag_stopped() {
                if let Some(stroke) = &mut self.stroke {
                    stroke.end();
                }
                self.stroke = None;
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
