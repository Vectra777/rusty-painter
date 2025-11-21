use eframe::egui::{Color32, ColorImage};

use crate::color::Color;
use crate::profiler::ScopeTimer;

pub struct Canvas {
    width: usize,
    height: usize,
    tile_size: usize,
    tiles_x: usize,
    tiles_y: usize,
    clear_color: Color32,
    tiles: Vec<Option<Vec<Color32>>>, // lazily allocated tiles
}

impl Canvas {
    pub fn new(width: usize, height: usize, clear_color: Color, tile_size: usize) -> Self {
        let tiles_x = (width + tile_size - 1) / tile_size;
        let tiles_y = (height + tile_size - 1) / tile_size;
        Self {
            width,
            height,
            tile_size,
            tiles_x,
            tiles_y,
            clear_color: clear_color.to_color32(),
            tiles: vec![None; tiles_x * tiles_y],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    fn tile_index(&self, tx: usize, ty: usize) -> Option<usize> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        Some(ty * self.tiles_x + tx)
    }

    fn get_tile(&self, tx: usize, ty: usize) -> Option<&Vec<Color32>> {
        self.tile_index(tx, ty)
            .and_then(|idx| self.tiles.get(idx))
            .and_then(|t| t.as_ref())
    }

    fn ensure_tile(&mut self, tx: usize, ty: usize) -> Option<&mut Vec<Color32>> {
        let idx = self.tile_index(tx, ty)?;
        if self.tiles[idx].is_none() {
            let data = vec![self.clear_color; self.tile_size * self.tile_size];
            self.tiles[idx] = Some(data);
        }
        self.tiles[idx].as_mut()
    }

    pub fn ensure_tile_exists(&mut self, tx: usize, ty: usize) {
        let _ = self.ensure_tile(tx, ty);
    }

    pub fn write_region_to_color_image(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        out: &mut ColorImage,
        step: usize,
    ) {
        let _timer = ScopeTimer::new("region_to_color_image");

        let step = step.max(1);
        let dst_w = (w + step - 1) / step;
        let dst_h = (h + step - 1) / step;

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let mut dst_x = 0;
            while dst_x < dst_w {
                let global_x = x + dst_x * step;
                let tx = global_x / self.tile_size;
                let ty = global_y / self.tile_size;
                let local_x = global_x % self.tile_size;
                let local_y = global_y % self.tile_size;

                let dst_start = dst_y * dst_w + dst_x;

                if let Some(tile) = self.get_tile(tx, ty) {
                    let src_start = local_y * self.tile_size + local_x;
                    out.pixels[dst_start] = tile[src_start];
                } else {
                    out.pixels[dst_start] = self.clear_color;
                }

                dst_x += 1;
            }
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.clear_color = color.to_color32();
        for tile in &mut self.tiles {
            *tile = None;
        }
    }

    pub fn index(&self, x: i32, y: i32) -> Option<(usize, usize, usize)> {
        if x < 0 || y < 0 {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return None;
        }
        let tx = x / self.tile_size;
        let ty = y / self.tile_size;
        let local_x = x % self.tile_size;
        let local_y = y % self.tile_size;
        Some((tx, ty, local_y * self.tile_size + local_x))
    }

    pub fn blend_pixel(&mut self, x: i32, y: i32, src: Color) {
        if let Some((tx, ty, idx)) = self.index(x, y) {
            if let Some(tile) = self.ensure_tile(tx, ty) {
                let dst = Color::from_color32(tile[idx]);
                let blended = alpha_over(src, dst);
                tile[idx] = blended.to_color32();
            }
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
