use std::sync::Mutex;

use eframe::egui::{Color32, ColorImage};

use crate::color::Color;
use crate::profiler::ScopeTimer;

#[derive(Debug)]
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: f32, // 0.0..1.0
    tiles: Vec<Mutex<TileCell>>,
}

impl Layer {
    fn new(name: String, width: usize, height: usize, tile_size: usize) -> Self {
        let tiles_x = (width + tile_size - 1) / tile_size;
        let tiles_y = (height + tile_size - 1) / tile_size;
        let tiles = (0..tiles_x * tiles_y)
            .map(|_| Mutex::new(TileCell { data: None }))
            .collect();
        Self {
            name,
            visible: true,
            opacity: 1.0,
            tiles,
        }
    }
}

pub struct Canvas {
    width: usize,
    height: usize,
    tile_size: usize,
    tiles_x: usize,
    tiles_y: usize,
    clear_color: Color32,
    
    pub layers: Vec<Layer>,
    pub active_layer_idx: usize,
}

#[derive(Debug)]
pub(crate) struct TileCell {
    pub data: Option<Vec<Color32>>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, clear_color: Color, tile_size: usize) -> Self {
        let tiles_x = (width + tile_size - 1) / tile_size;
        let tiles_y = (height + tile_size - 1) / tile_size;
        
        let bg_layer = Layer::new("Background".to_string(), width, height, tile_size);
        
        // Initialize background layer with clear color
        // We can't easily pre-fill all tiles without allocating massive memory.
        // The original code lazily allocated.
        // But if it's the background, it should probably be white (or clear_color).
        // The original code handled `None` as `clear_color` in `ensure_tile`.
        // We should preserve that behavior.

        Self {
            width,
            height,
            tile_size,
            tiles_x,
            tiles_y,
            clear_color: clear_color.to_color32(),
            layers: vec![bg_layer],
            active_layer_idx: 0,
        }
    }

    pub fn add_layer(&mut self) {
        let name = format!("Layer {}", self.layers.len() + 1);
        let layer = Layer::new(name, self.width, self.height, self.tile_size);
        self.layers.push(layer);
        self.active_layer_idx = self.layers.len() - 1;
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    fn tile_index(&self, tx: usize, ty: usize) -> Option<usize> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        Some(ty * self.tiles_x + tx)
    }

    // Access the active layer's tile
    fn tile_cell(&self, tx: usize, ty: usize) -> Option<&Mutex<TileCell>> {
        if self.active_layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[self.active_layer_idx];
        self.tile_index(tx, ty).and_then(|idx| layer.tiles.get(idx))
    }

    // Access a specific layer's tile (for compositing)
    fn layer_tile_cell(&self, layer_idx: usize, tx: usize, ty: usize) -> Option<&Mutex<TileCell>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        self.tile_index(tx, ty).and_then(|idx| layer.tiles.get(idx))
    }

    fn get_tile(&self, tx: usize, ty: usize) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        self.tile_cell(tx, ty).map(|cell| cell.lock().unwrap())
    }

    fn ensure_tile(&self, tx: usize, ty: usize) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        let cell = self.tile_cell(tx, ty)?;
        let mut guard = cell.lock().unwrap();
        if guard.data.is_none() {
            // If it's the background layer (index 0), fill with clear_color.
            // If it's a transparent layer, fill with transparent.
            // Actually, `clear_color` in `Canvas` was used for the whole canvas.
            // For layers, usually the bottom one is white/colored, others are transparent.
            // Let's assume layer 0 is background and uses `clear_color`, others use transparent.
            
            let fill_color = if self.active_layer_idx == 0 {
                self.clear_color
            } else {
                Color32::TRANSPARENT
            };
            
            let data = vec![fill_color; self.tile_size * self.tile_size];
            guard.data = Some(data);
        }
        Some(guard)
    }

    pub fn ensure_tile_exists(&self, tx: usize, ty: usize) {
        let _ = self.ensure_tile(tx, ty);
    }

    pub fn lock_tile(&self, tx: usize, ty: usize) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        self.ensure_tile(tx, ty)
    }

    pub fn get_tile_data(&self, tx: usize, ty: usize) -> Option<Vec<Color32>> {
        let cell = self.tile_cell(tx, ty)?;
        let guard = cell.lock().unwrap();
        guard.data.clone()
    }

    pub fn set_tile_data(&self, tx: usize, ty: usize, data: Vec<Color32>) {
        if let Some(mut guard) = self.ensure_tile(tx, ty) {
            guard.data = Some(data);
        }
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

        // Optimization: Check if the region is within a single tile
        let start_tx = x / self.tile_size;
        let start_ty = y / self.tile_size;
        let end_tx = (x + w - 1) / self.tile_size;
        let end_ty = (y + h - 1) / self.tile_size;

        if start_tx == end_tx && start_ty == end_ty {
            // Fast path: Single tile
            let tx = start_tx;
            let ty = start_ty;
            let tile_idx = self.tile_index(tx, ty);

            if let Some(idx) = tile_idx {
                // Lock all layers for this tile
                let layer_guards: Vec<Option<std::sync::MutexGuard<'_, TileCell>>> = self.layers
                    .iter()
                    .map(|layer| layer.tiles.get(idx).map(|m| m.lock().unwrap()))
                    .collect();

                for dst_y in 0..dst_h {
                    let global_y = y + dst_y * step;
                    let local_y = global_y % self.tile_size;
                    let row_start = dst_y * dst_w;

                    for dst_x in 0..dst_w {
                        let global_x = x + dst_x * step;
                        let local_x = global_x % self.tile_size;
                        let src_idx = local_y * self.tile_size + local_x;

                        let mut final_color = Color::rgba(0, 0, 0, 0);

                        for (layer_idx, guard_opt) in layer_guards.iter().enumerate() {
                            let layer = &self.layers[layer_idx];
                            if !layer.visible || layer.opacity <= 0.0 { continue; }

                            if let Some(guard) = guard_opt {
                                if let Some(data) = &guard.data {
                                    let pixel = data[src_idx];
                                    let mut src_color = Color::from_color32(pixel);
                                    src_color.a *= layer.opacity;
                                    final_color = alpha_over(src_color, final_color);
                                } else if layer_idx == 0 {
                                    let mut src_color = Color::from_color32(self.clear_color);
                                    src_color.a *= layer.opacity;
                                    final_color = alpha_over(src_color, final_color);
                                }
                            } else if layer_idx == 0 {
                                let mut src_color = Color::from_color32(self.clear_color);
                                src_color.a *= layer.opacity;
                                final_color = alpha_over(src_color, final_color);
                            }
                        }
                        out.pixels[row_start + dst_x] = final_color.to_color32();
                    }
                }
            }
            return;
        }

        // Fallback: Slow path for multi-tile regions
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

                // Composite layers
                let mut final_color = Color::rgba(0, 0, 0, 0); // Start transparent
                
                // Background color (if we want one)
                // final_color = Color::from_color32(self.clear_color);

                for (layer_idx, layer) in self.layers.iter().enumerate() {
                    if !layer.visible { continue; }
                    
                    let layer_opacity = layer.opacity;
                    if layer_opacity <= 0.0 { continue; }

                    if let Some(cell) = self.layer_tile_cell(layer_idx, tx, ty) {
                        let guard = cell.lock().unwrap();
                        if let Some(data) = guard.data.as_ref() {
                            let src_idx = local_y * self.tile_size + local_x;
                            let pixel = data[src_idx];
                            let mut src_color = Color::from_color32(pixel);
                            src_color.a *= layer_opacity;
                            
                            // Simple alpha blending
                            // dst = src + dst * (1 - src.a)
                            final_color = alpha_over(src_color, final_color);
                        } else if layer_idx == 0 {
                            // Background layer default color
                            let mut src_color = Color::from_color32(self.clear_color);
                            src_color.a *= layer_opacity;
                            final_color = alpha_over(src_color, final_color);
                        }
                    } else if layer_idx == 0 {
                         // Background layer default color
                        let mut src_color = Color::from_color32(self.clear_color);
                        src_color.a *= layer_opacity;
                        final_color = alpha_over(src_color, final_color);
                    }
                }

                out.pixels[dst_start] = final_color.to_color32();
                dst_x += 1;
            }
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.clear_color = color.to_color32();
        // Clear all layers? Or just active?
        // Usually "Clear" clears the active layer.
        // But if it's "New File", it clears everything.
        // Let's assume this clears the active layer.
        if let Some(layer) = self.layers.get_mut(self.active_layer_idx) {
            for tile in &layer.tiles {
                let mut cell = tile.lock().unwrap();
                cell.data = None;
            }
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

    pub fn blend_pixel(&self, x: i32, y: i32, src: Color) {
        if let Some((tx, ty, idx)) = self.index(x, y) {
            if let Some(mut tile) = self.ensure_tile(tx, ty) {
                if let Some(data) = tile.data.as_mut() {
                    let dst = Color::from_color32(data[idx]);
                    let blended = alpha_over(src, dst);
                    data[idx] = blended.to_color32();
                }
            }
        }
    }
}

pub fn blend_erase(src: Color, dst: Color) -> Color {
    // src.a is the strength of the eraser
    let out_a = dst.a * (1.0 - src.a);
    Color {
        r: dst.r,
        g: dst.g,
        b: dst.b,
        a: out_a,
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
