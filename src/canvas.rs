use eframe::egui::ColorImage;

use crate::color::Color;

pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, clear_color: Color) -> Self {
        Self {
            width,
            height,
            pixels: vec![clear_color; width * height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn get_pixel(&self, x: usize, y: usize) -> Color {
        if x >= self.width || y >= self.height {
            return Color::rgba(0, 0, 0, 0);
        }
        self.pixels[y * self.width + x]
    }

    pub fn region_to_color_image(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) -> ColorImage {
        let mut pixels = Vec::with_capacity(w * h);
        for yy in 0..h {
            for xx in 0..w {
                let c = self.get_pixel(x + xx, y + yy);
                pixels.push(c.to_color32());
            }
        }
        ColorImage {
            size: [w, h],
            pixels,
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(color);
    }

    pub fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(y * self.width + x)
    }

    pub fn blend_pixel(&mut self, x: i32, y: i32, src: Color) {
        if let Some(idx) = self.index(x, y) {
            let dst = self.pixels[idx];
            self.pixels[idx] = alpha_over(src, dst);
        }
    }

}

pub fn alpha_over(src: Color, dst: Color) -> Color {
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
