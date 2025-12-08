use std::sync::Mutex;

use eframe::egui::{Color32, ColorImage};

use crate::utils::color::{Color, ColorManipulation};
use crate::utils::profiler::ScopeTimer;

#[derive(Debug)]
/// Single painting layer with its own opacity, visibility and tile storage.
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: u8, // 0..255
    pub locked: bool,
    tiles: Vec<Mutex<TileCell>>,
}

impl Layer {
    /// Allocate a new layer backing store but keep tile data lazy.
    fn new(name: String, width: usize, height: usize, tile_size: usize) -> Self {
        let tiles_x = (width + tile_size - 1) / tile_size;
        let tiles_y = (height + tile_size - 1) / tile_size;
        let tiles = (0..tiles_x * tiles_y)
            .map(|_| Mutex::new(TileCell { data: None }))
            .collect();
        Self {
            name,
            visible: true,
            opacity: 255,
            locked: false,
            tiles,
        }
    }
}

/// Main drawing surface that owns tile grids and blending rules across layers.
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
/// Tile container that is lazily filled with pixel data.
pub(crate) struct TileCell {
    pub data: Option<Vec<Color32>>,
}

impl Canvas {
    /// Create a new canvas with a single background layer and configured tile size.
    pub fn new(width: usize, height: usize, clear_color: Color32, tile_size: usize) -> Self {
        let tiles_x = (width + tile_size - 1) / tile_size;
        let tiles_y = (height + tile_size - 1) / tile_size;

        let mut bg_layer = Layer::new("Background".to_string(), width, height, tile_size);
        bg_layer.locked = true;
        
        let layer1 = Layer::new("Layer 1".to_string(), width, height, tile_size);

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
            clear_color: premultiply(clear_color),
            layers: vec![bg_layer, layer1],
            active_layer_idx: 1,
        }
    }

    pub fn add_layer(&mut self) {
        let name = format!("Layer {}", self.layers.len() + 1);
        let layer = Layer::new(name, self.width, self.height, self.tile_size);
        self.layers.push(layer);
        self.active_layer_idx = self.layers.len() - 1;
    }

    /// Current canvas width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Current canvas height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn clear_color(&self) -> Color32 {
        self.clear_color
    }

    /// Size of a tile edge in pixels.
    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    /// Flatten a tile coordinate into the backing vector index.
    fn tile_index(&self, tx: usize, ty: usize) -> Option<usize> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        Some(ty * self.tiles_x + tx)
    }

    /// Access the active layer's tile.
    fn tile_cell(&self, tx: usize, ty: usize) -> Option<&Mutex<TileCell>> {
        if self.active_layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[self.active_layer_idx];
        self.tile_index(tx, ty).and_then(|idx| layer.tiles.get(idx))
    }

    /// Access a specific layer's tile by index (used for compositing).
    fn layer_tile_cell(&self, layer_idx: usize, tx: usize, ty: usize) -> Option<&Mutex<TileCell>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        self.tile_index(tx, ty).and_then(|idx| layer.tiles.get(idx))
    }

    /// Ensure the tile exists on a specific layer, initializing it if needed.
    fn ensure_layer_tile(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        let cell = self.layer_tile_cell(layer_idx, tx, ty)?;
        let mut guard = cell.lock().unwrap();
        if guard.data.is_none() {
            let fill_color = if layer_idx == 0 {
                self.clear_color
            } else {
                Color32::TRANSPARENT
            };

            let data = vec![fill_color; self.tile_size * self.tile_size];
            guard.data = Some(data);
        }
        Some(guard)
    }

    /// Ensure the active layer has storage for the given tile.
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

    /// Guarantee a tile exists on the active layer.
    pub fn ensure_tile_exists(&self, tx: usize, ty: usize) {
        let _ = self.ensure_tile(tx, ty);
    }

    /// Guarantee a tile exists on the specified layer.
    pub fn ensure_layer_tile_exists(&self, layer_idx: usize, tx: usize, ty: usize) {
        let _ = self.ensure_layer_tile(layer_idx, tx, ty);
    }

    /// Lock a tile in the active layer, initializing it if absent.
    pub(crate) fn lock_tile(&self, tx: usize, ty: usize) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        self.ensure_tile(tx, ty)
    }

    /// Lock a tile in a specific layer, initializing it if absent.
    pub(crate) fn lock_layer_tile(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        self.ensure_layer_tile(layer_idx, tx, ty)
    }

    /// Lock a tile in a specific layer only if it already exists; avoids allocating new data.
    pub(crate) fn lock_layer_tile_if_exists(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<std::sync::MutexGuard<'_, TileCell>> {
        self.layer_tile_cell(layer_idx, tx, ty)
            .map(|m| m.lock().unwrap())
    }

    /// Clone the raw pixel buffer for a tile in a given layer.
    pub fn get_layer_tile_data(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Vec<Color32>> {
        let cell = self.layer_tile_cell(layer_idx, tx, ty)?;
        let guard = cell.lock().unwrap();
        guard.data.clone()
    }

    /// Overwrite a tile's pixel buffer for a given layer.
    pub fn set_layer_tile_data(&self, layer_idx: usize, tx: usize, ty: usize, data: Vec<Color32>) {
        if let Some(cell) = self.layer_tile_cell(layer_idx, tx, ty) {
            let mut guard = cell.lock().unwrap();
            if guard.data.is_none() {
                // If we are setting data, we must ensure the tile is initialized if it wasn't.
                // if layer 0, fill with clear_color, else transparent.
                // Here we are overwriting data anyway.
                // But we need to make sure we are not just setting data on a None if that implies something else.
                // Actually, if we are restoring a snapshot, the snapshot has the full data.
                // So we can just set it.
            }
            guard.data = Some(data);
        } else {
            // If the tile cell doesn't exist (e.g. out of bounds or layer doesn't exist), we can't set it.
            // But layer_tile_cell checks bounds.
        }
    }

    /// Composite a canvas region into a `ColorImage`, optionally downsampled by `step`.
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
                let layer_guards: Vec<Option<std::sync::MutexGuard<'_, TileCell>>> = self
                    .layers
                    .iter()
                    .map(|layer| layer.tiles.get(idx).map(|m| m.lock().unwrap()))
                    .collect();

                for dst_y in 0..dst_h {
                    let global_y_start = y + dst_y * step;
                    let row_start = dst_y * dst_w;

                    for dst_x in 0..dst_w {
                        let global_x_start = x + dst_x * step;

                        if step == 1 {
                            // Fast path for 1:1 rendering
                            let local_y = global_y_start % self.tile_size;
                            let local_x = global_x_start % self.tile_size;
                            let src_idx = local_y * self.tile_size + local_x;

                            let mut final_color = Color32::TRANSPARENT;
                            for (layer_idx, guard_opt) in layer_guards.iter().enumerate() {
                                let layer = &self.layers[layer_idx];
                                if !layer.visible || layer.opacity == 0 {
                                    continue;
                                }

                                if let Some(guard) = guard_opt {
                                    if let Some(data) = &guard.data {
                                        let pixel = data[src_idx];
                                        let src_color =
                                            apply_opacity_scale(pixel, layer.opacity as u32);
                                        final_color = alpha_over(src_color, final_color);
                                    } else if layer_idx == 0 {
                                        let src_color = apply_opacity_scale(
                                            self.clear_color,
                                            layer.opacity as u32,
                                        );
                                        final_color = alpha_over(src_color, final_color);
                                    }
                                } else if layer_idx == 0 {
                                    let src_color =
                                        apply_opacity_scale(self.clear_color, layer.opacity as u32);
                                    final_color = alpha_over(src_color, final_color);
                                }
                            }
                            out.pixels[row_start + dst_x] = final_color;
                        } else {
                            // High quality downsampling (box filter)
                            let mut r_acc = 0.0;
                            let mut g_acc = 0.0;
                            let mut b_acc = 0.0;
                            let mut a_acc = 0.0;
                            let mut count = 0.0;

                            for sy in 0..step {
                                let global_y = global_y_start + sy;
                                if global_y >= y + h {
                                    continue;
                                }
                                let local_y = global_y % self.tile_size;

                                for sx in 0..step {
                                    let global_x = global_x_start + sx;
                                    if global_x >= x + w {
                                        continue;
                                    }
                                    let local_x = global_x % self.tile_size;

                                    let src_idx = local_y * self.tile_size + local_x;
                                    let mut pixel_color = Color32::TRANSPARENT;

                                    for (layer_idx, guard_opt) in layer_guards.iter().enumerate() {
                                        let layer = &self.layers[layer_idx];
                                        if !layer.visible || layer.opacity == 0 {
                                            continue;
                                        }

                                        if let Some(guard) = guard_opt {
                                            if let Some(data) = &guard.data {
                                                let pixel = data[src_idx];
                                                let src_color = apply_opacity_scale(
                                                    pixel,
                                                    layer.opacity as u32,
                                                );
                                                pixel_color = alpha_over(src_color, pixel_color);
                                            } else if layer_idx == 0 {
                                                let src_color = apply_opacity_scale(
                                                    self.clear_color,
                                                    layer.opacity as u32,
                                                );
                                                pixel_color = alpha_over(src_color, pixel_color);
                                            }
                                        } else if layer_idx == 0 {
                                            let src_color = apply_opacity_scale(
                                                self.clear_color,
                                                layer.opacity as u32,
                                            );
                                            pixel_color = alpha_over(src_color, pixel_color);
                                        }
                                    }

                                    r_acc += pixel_color.r() as f32;
                                    g_acc += pixel_color.g() as f32;
                                    b_acc += pixel_color.b() as f32;
                                    a_acc += pixel_color.a() as f32;
                                    count += 1.0;
                                }
                            }

                            if count > 0.0 {
                                let inv = 1.0 / count;
                                let r = (r_acc * inv).clamp(0.0, 255.0) as u8;
                                let g = (g_acc * inv).clamp(0.0, 255.0) as u8;
                                let b = (b_acc * inv).clamp(0.0, 255.0) as u8;
                                let a = (a_acc * inv).clamp(0.0, 255.0) as u8;
                                out.pixels[row_start + dst_x] =
                                    Color32::from_rgba_unmultiplied(r, g, b, a);
                            }
                        }
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
                let mut final_color = Color32::TRANSPARENT; // Start transparent

                for (layer_idx, layer) in self.layers.iter().enumerate() {
                    if !layer.visible {
                        continue;
                    }

                    let layer_opacity = layer.opacity;
                    if layer_opacity == 0 {
                        continue;
                    }

                    if let Some(cell) = self.layer_tile_cell(layer_idx, tx, ty) {
                        let guard = cell.lock().unwrap();
                        if let Some(data) = guard.data.as_ref() {
                            let src_idx = local_y * self.tile_size + local_x;
                            let pixel = data[src_idx];
                            let src_color = apply_opacity_scale(pixel, layer_opacity as u32);
                            final_color = alpha_over(src_color, final_color);
                        } else if layer_idx == 0 {
                            // Background layer default color
                            let src_color =
                                apply_opacity_scale(self.clear_color, layer_opacity as u32);
                            final_color = alpha_over(src_color, final_color);
                        }
                    } else if layer_idx == 0 {
                        // Background layer default color
                        let src_color = apply_opacity_scale(self.clear_color, layer_opacity as u32);
                        final_color = alpha_over(src_color, final_color);
                    }
                }

                out.pixels[dst_start] = final_color;
                dst_x += 1;
            }
        }
    }

    /// Clear the active layer to the provided color (or transparent for non-background).
    pub fn clear(&mut self, color: Color) {
        self.clear_color = premultiply(color.to_color32());
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
}

/// Erase blend mode: reduce destination alpha by the source alpha.
pub fn blend_erase(src: Color32, dst: Color32) -> Color32 {
    let src_a = src.a() as u32;
    let inv = 255 - src_a;
    let out_a = (dst.a() as u32 * inv + 127) / 255;
    let out_r = (dst.r() as u32 * inv + 127) / 255;
    let out_g = (dst.g() as u32 * inv + 127) / 255;
    let out_b = (dst.b() as u32 * inv + 127) / 255;
    Color32::from_rgba_premultiplied(
        out_r.min(255) as u8,
        out_g.min(255) as u8,
        out_b.min(255) as u8,
        out_a.min(255) as u8,
    )
}

/// Standard "source over" alpha compositing for premultiplied colors.
pub fn alpha_over(src: Color32, dst: Color32) -> Color32 {
    let src_a = src.a() as u32;
    let dst_a = dst.a() as u32;
    let inv = 255 - src_a;
    let out_a = src_a + (dst_a * inv + 127) / 255;
    if out_a == 0 {
        return Color32::TRANSPARENT;
    }

    let out_r = src.r() as u32 + (dst.r() as u32 * inv + 127) / 255;
    let out_g = src.g() as u32 + (dst.g() as u32 * inv + 127) / 255;
    let out_b = src.b() as u32 + (dst.b() as u32 * inv + 127) / 255;

    Color32::from_rgba_premultiplied(
        out_r.min(255) as u8,
        out_g.min(255) as u8,
        out_b.min(255) as u8,
        out_a.min(255) as u8,
    )
}

#[inline]
fn apply_opacity_scale(color: Color32, opacity_scale: u32) -> Color32 {
    if opacity_scale >= 255 {
        return color;
    }
    let a = (color.a() as u32 * opacity_scale + 127) / 255;
    let r = (color.r() as u32 * opacity_scale + 127) / 255;
    let g = (color.g() as u32 * opacity_scale + 127) / 255;
    let b = (color.b() as u32 * opacity_scale + 127) / 255;
    Color32::from_rgba_premultiplied(r as u8, g as u8, b as u8, a as u8)
}

fn premultiply(color: Color32) -> Color32 {
    let a = color.a() as u32;
    if a >= 255 {
        return color;
    }
    let r = (color.r() as u32 * a + 127) / 255;
    let g = (color.g() as u32 * a + 127) / 255;
    let b = (color.b() as u32 * a + 127) / 255;
    Color32::from_rgba_premultiplied(r as u8, g as u8, b as u8, a as u8)
}

#[allow(dead_code)]
fn unpremultiply(color: Color32) -> Color32 {
    let a = color.a() as u32;
    if a == 0 || a >= 255 {
        return color;
    }
    let r = ((color.r() as u32 * 255 + a / 2) / a).min(255);
    let g = ((color.g() as u32 * 255 + a / 2) / a).min(255);
    let b = ((color.b() as u32 * 255 + a / 2) / a).min(255);
    Color32::from_rgba_unmultiplied(r as u8, g as u8, b as u8, a as u8)
}
