use eframe::egui;
use eframe::egui::{Color32, ColorImage, TextureHandle, TextureOptions};

#[derive(Clone, Copy, Debug)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Color {
    fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    fn white() -> Self {
        Self::rgba(255, 255, 255, 255)
    }

    fn to_color32(&self) -> Color32 {
        Color32::from_rgba_premultiplied(
            (self.r * 255.0) as u8,
            (self.g * 255.0) as u8,
            (self.b * 255.0) as u8,
            (self.a * 255.0) as u8,
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct Vec2 {
    x: f32,
    y: f32,
}

fn distance(a: Vec2, b: Vec2) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
}

impl Canvas {
    fn new(width: usize, height: usize, clear_color: Color) -> Self {
        Self {
            width,
            height,
            pixels: vec![clear_color; width * height],
        }
    }

    fn clear(&mut self, color: Color) {
        self.pixels.fill(color);
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(y * self.width + x)
    }

    fn blend_pixel(&mut self, x: i32, y: i32, src: Color) {
        if let Some(idx) = self.index(x, y) {
            let dst = self.pixels[idx];
            self.pixels[idx] = alpha_over(src, dst);
        }
    }

    fn to_color_image(&self) -> ColorImage {
        let pixels: Vec<Color32> = self.pixels.iter().map(|c| c.to_color32()).collect();
        ColorImage {
            size: [self.width, self.height],
            pixels,
        }
    }
}

// "source over" alpha compositing
fn alpha_over(src: Color, dst: Color) -> Color {
    let out_a = src.a + dst.a * (1.0 - src.a);
    if out_a <= 0.0 {
        return Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }

    let r = (src.r * src.a + dst.r * dst.a * (1.0 - src.a)) / out_a;
    let g = (src.g * src.a + dst.g * dst.a * (1.0 - src.a)) / out_a;
    let b = (src.b * src.a + dst.b * dst.a * (1.0 - src.a)) / out_a;

    Color { r, g, b, a: out_a }
}

struct Brush {
    radius: f32,
    hardness: f32, // 0..1
    color: Color,
    spacing: f32, // multiple of radius
}

impl Brush {
    fn dab(&self, canvas: &mut Canvas, center: Vec2) {
        let r = self.radius;
        let r_i32 = r.ceil() as i32;

        let min_x = (center.x.floor() as i32) - r_i32;
        let max_x = (center.x.floor() as i32) + r_i32;
        let min_y = (center.y.floor() as i32) - r_i32;
        let max_y = (center.y.floor() as i32) + r_i32;

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let dx = x as f32 + 0.5 - center.x;
                let dy = y as f32 + 0.5 - center.y;
                let dist = (dx * dx + dy * dy).sqrt();

                if dist > r {
                    continue;
                }

                let t = dist / r; // 0 center, 1 edge
                let softness = (1.0 - t).powf(2.0);
                let hardness = self.hardness.clamp(0.0, 1.0);
                let alpha = hardness + (1.0 - hardness) * softness;

                let src = Color {
                    r: self.color.r,
                    g: self.color.g,
                    b: self.color.b,
                    a: self.color.a * alpha,
                };

                canvas.blend_pixel(x, y, src);
            }
        }
    }
}

struct StrokeState {
    last_pos: Option<Vec2>,
    distance_since_last_dab: f32,
}

impl StrokeState {
    fn new() -> Self {
        Self {
            last_pos: None,
            distance_since_last_dab: 0.0,
        }
    }

    fn add_point(&mut self, canvas: &mut Canvas, brush: &Brush, pos: Vec2) {
        let step = brush.radius * brush.spacing.max(0.01);

        if let Some(prev) = self.last_pos {
            let segment_len = distance(prev, pos);

            if segment_len == 0.0 {
                // Just dab once at this position
                brush.dab(canvas, pos);
                return;
            }

            let mut d = self.distance_since_last_dab;
            let mut t = d / segment_len;

            while t <= 1.0 {
                let x = prev.x + (pos.x - prev.x) * t;
                let y = prev.y + (pos.y - prev.y) * t;
                let p = Vec2 { x, y };
                brush.dab(canvas, p);

                d += step;
                t = d / segment_len;
            }

            self.distance_since_last_dab = d - segment_len;
            if self.distance_since_last_dab < 0.0 {
                self.distance_since_last_dab = 0.0;
            }
        } else {
            // first point
            brush.dab(canvas, pos);
            self.distance_since_last_dab = 0.0;
        }

        self.last_pos = Some(pos);
    }

    fn end(&mut self) {
        self.last_pos = None;
        self.distance_since_last_dab = 0.0;
    }
}

struct PainterApp {
    canvas: Canvas,
    brush: Brush,
    stroke: Option<StrokeState>,
    texture: Option<TextureHandle>,
}

impl PainterApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let width = 800;
        let height = 600;
        let canvas = Canvas::new(width, height, Color::white());
        let brush = Brush {
            radius: 12.0,
            hardness: 0.2,
            color: Color::rgba(0, 0, 0, 255),
            spacing: 0.25,
        };

        Self {
            canvas,
            brush,
            stroke: None,
            texture: None,
        }
    }
}

impl eframe::App for PainterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Rust Dab Painter (eframe + egui)");
            ui.label("Left click to paint. 'C' to clear.");

            // Update texture
            let image = self.canvas.to_color_image();
            self.texture = Some(ui.ctx().load_texture(
                "canvas",
                image,
                TextureOptions::NEAREST,
            ));

            if let Some(texture) = &self.texture {
                let size = texture.size_vec2();
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());
                
                // Draw the texture
                ui.painter().image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
                    Color32::WHITE,
                );

                // Handle input
                if response.dragged() || response.clicked() {
                    if let Some(pointer_pos) = response.interact_pointer_pos() {
                        // Convert pointer pos to canvas coordinates
                        let canvas_pos = pointer_pos - rect.min;
                        let pos = Vec2 {
                            x: canvas_pos.x,
                            y: canvas_pos.y,
                        };

                        if self.stroke.is_none() {
                            self.stroke = Some(StrokeState::new());
                        }

                        if let Some(stroke) = &mut self.stroke {
                            stroke.add_point(&mut self.canvas, &self.brush, pos);
                        }
                    }
                } else if response.drag_stopped() {
                     if let Some(stroke) = &mut self.stroke {
                        stroke.end();
                    }
                    self.stroke = None;
                }
            }
            
            if ui.input(|i| i.key_pressed(egui::Key::C)) {
                self.canvas.clear(Color::white());
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rust Dab Painter",
        options,
        Box::new(|cc| Ok(Box::new(PainterApp::new(cc)))),
    )
}
