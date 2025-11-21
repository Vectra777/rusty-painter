use eframe::egui::{Color32, ColorImage};

use crate::color::Color;
use crate::profiler::ScopeTimer;

pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
    pixels_rgba: Vec<Color32>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, clear_color: Color) -> Self {
        let clear_color32 = clear_color.to_color32();
        Self {
            width,
            height,
            pixels: vec![clear_color; width * height],
            pixels_rgba: vec![clear_color32; width * height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn write_region_to_color_image(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        out: &mut ColorImage,
    ) {
        let _timer = ScopeTimer::new("region_to_color_image");

        if out.size != [w, h] {
            out.size = [w, h];
            out.pixels.resize(w * h, Color32::TRANSPARENT);
        }

        for yy in 0..h {
            let src_start = (y + yy) * self.width + x;
            let dst_start = yy * w;
            let src_slice = &self.pixels_rgba[src_start..src_start + w];
            out.pixels[dst_start..dst_start + w].copy_from_slice(src_slice);
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(color);
        self.pixels_rgba.fill(color.to_color32());
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
            let blended = alpha_over(src, dst);
            self.pixels[idx] = blended;
            self.pixels_rgba[idx] = blended.to_color32();
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
