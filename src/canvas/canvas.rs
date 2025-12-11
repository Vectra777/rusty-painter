use std::sync::{Arc, Mutex};
use std::collections::HashMap;

use eframe::egui::{Color32, ColorImage};

use crate::utils::color::{Color, ColorManipulation};
use crate::utils::profiler::ScopeTimer;
use crate::utils::vector::Vec2;
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;

#[derive(Debug)]
/// Single painting layer with its own opacity, visibility and tile storage.
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: u8, // 0..255
    pub locked: bool,
    tiles: Mutex<HashMap<(i32, i32), Arc<Mutex<TileCell>>>>,
}

impl Layer {
    /// Allocate a new layer backing store but keep tile data lazy.
    fn new(name: String, _width: usize, _height: usize, _tile_size: usize) -> Self {
        Self {
            name,
            visible: true,
            opacity: 255,
            locked: false,
            tiles: Mutex::new(HashMap::new()),
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

    /// Access the active layer's tile.
    fn tile_cell(&self, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        if self.active_layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[self.active_layer_idx];
        let tiles = layer.tiles.lock().unwrap();
        tiles.get(&(tx, ty)).cloned()
    }

    /// Access a specific layer's tile by index (used for compositing).
    fn layer_tile_cell(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        let tiles = layer.tiles.lock().unwrap();
        tiles.get(&(tx, ty)).cloned()
    }

    /// Ensure the tile exists on a specific layer, initializing it if needed.
    fn ensure_layer_tile(
        &self,
        layer_idx: usize,
        tx: i32,
        ty: i32,
    ) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        
        let tile_arc = {
            let mut tiles = layer.tiles.lock().unwrap();
            tiles.entry((tx, ty))
                .or_insert_with(|| Arc::new(Mutex::new(TileCell { data: None })))
                .clone()
        };

        {
            let mut guard = tile_arc.lock().unwrap();
            if guard.data.is_none() {
                let fill_color = if layer_idx == 0 {
                    self.clear_color
                } else {
                    Color32::TRANSPARENT
                };

                let data = vec![fill_color; self.tile_size * self.tile_size];
                guard.data = Some(data);
            }
        }
        Some(tile_arc)
    }

    /// Ensure the active layer has storage for the given tile.
    fn ensure_tile(&self, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(self.active_layer_idx, tx, ty)
    }

    /// Guarantee a tile exists on the active layer.
    pub fn ensure_tile_exists(&self, tx: usize, ty: usize) {
        let _ = self.ensure_tile(tx as i32, ty as i32);
    }

    /// Guarantee a tile exists on the specified layer.
    pub fn ensure_layer_tile_exists(&self, layer_idx: usize, tx: usize, ty: usize) {
        let _ = self.ensure_layer_tile(layer_idx, tx as i32, ty as i32);
    }

    /// Guarantee a tile exists on the specified layer (i32 coords).
    pub fn ensure_layer_tile_exists_i32(&self, layer_idx: usize, tx: i32, ty: i32) {
        let _ = self.ensure_layer_tile(layer_idx, tx, ty);
    }

    /// Lock a tile in the active layer, initializing it if absent.
    pub(crate) fn lock_tile(&self, tx: usize, ty: usize) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_tile(tx as i32, ty as i32)
    }

    /// Lock a tile in a specific layer, initializing it if absent.
    pub(crate) fn lock_layer_tile(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(layer_idx, tx as i32, ty as i32)
    }

    /// Lock a tile in a specific layer (i32 coords).
    pub(crate) fn lock_layer_tile_i32(
        &self,
        layer_idx: usize,
        tx: i32,
        ty: i32,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(layer_idx, tx, ty)
    }

    /// Lock a tile in a specific layer only if it already exists; avoids allocating new data.
    pub(crate) fn lock_layer_tile_if_exists(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.layer_tile_cell(layer_idx, tx as i32, ty as i32)
    }

    /// Clone the raw pixel buffer for a tile in a given layer.
    pub fn get_layer_tile_data(
        &self,
        layer_idx: usize,
        tx: i32,
        ty: i32,
    ) -> Option<Vec<Color32>> {
        let cell = self.layer_tile_cell(layer_idx, tx, ty)?;
        let guard = cell.lock().unwrap();
        guard.data.clone()
    }

    /// Overwrite a tile's pixel buffer for a given layer.
    pub fn set_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32, data: Vec<Color32>) {
        // Ensure tile exists
        if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
            let mut guard = cell.lock().unwrap();
            guard.data = Some(data);
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
            let tx = start_tx as i32;
            let ty = start_ty as i32;

            // Get Arcs first
            let layer_arcs: Vec<Option<Arc<Mutex<TileCell>>>> = self
                .layers
                .iter()
                .map(|layer| {
                    let tiles = layer.tiles.lock().unwrap();
                    tiles.get(&(tx, ty)).cloned()
                })
                .collect();

            // Then lock them
            let layer_guards: Vec<Option<std::sync::MutexGuard<'_, TileCell>>> = layer_arcs
                .iter()
                .map(|opt| opt.as_ref().map(|arc| arc.lock().unwrap()))
                .collect();

            if true { // Scope to keep indentation similar
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
                let tx = (global_x / self.tile_size) as i32;
                let ty = (global_y / self.tile_size) as i32;
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
        if let Some(layer) = self.layers.get(self.active_layer_idx) {
            let tiles = layer.tiles.lock().unwrap();
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap();
                cell.data = None;
            }
        }
    }

    pub fn capture_layer_pixels(&self, layer_idx: usize) -> HashMap<(i32, i32), Vec<Color32>> {
        let mut pixels = HashMap::new();
        if let Some(layer) = self.layers.get(layer_idx) {
            let tiles = layer.tiles.lock().unwrap();
            for ((tx, ty), tile_arc) in tiles.iter() {
                let guard = tile_arc.lock().unwrap();
                if let Some(data) = &guard.data {
                    pixels.insert((*tx, *ty), data.clone());
                }
            }
        }
        pixels
    }

    pub fn preview_transform(&mut self, layer_idx: usize, src_tiles: &HashMap<(i32, i32), Vec<Color32>>, offset: Vec2, rotation: f32, scale: Vec2, center: Vec2) {
        let tile_size = self.tile_size;
        
        // 1. Collect all source pixels from buffer
        let mut src_pixels: HashMap<(i32, i32), Color32> = HashMap::new();
        let mut src_bounds = eframe::egui::Rect::NOTHING;
        let mut first = true;

        for ((tx, ty), data) in src_tiles {
            for py in 0..tile_size {
                for px in 0..tile_size {
                    let idx = py * tile_size + px;
                    if data[idx].a() > 0 {
                        let gx = *tx * tile_size as i32 + px as i32;
                        let gy = *ty * tile_size as i32 + py as i32;
                        
                        src_pixels.insert((gx, gy), data[idx]);
                        
                        let pos = eframe::egui::pos2(gx as f32, gy as f32);
                        if first {
                            src_bounds = eframe::egui::Rect::from_min_max(pos, pos);
                            first = false;
                        } else {
                            src_bounds.extend_with(pos);
                        }
                    }
                }
            }
        }
        
        if src_pixels.is_empty() { return; }
        // Expand bounds slightly to cover the pixels fully
        src_bounds.max.x += 1.0;
        src_bounds.max.y += 1.0;

        // 2. Calculate destination bounds
        let corners = [
            src_bounds.min,
            eframe::egui::pos2(src_bounds.max.x, src_bounds.min.y),
            src_bounds.max,
            eframe::egui::pos2(src_bounds.min.x, src_bounds.max.y),
        ];
        
        let (sin_r, cos_r) = rotation.sin_cos();
        
        let transform = |p: eframe::egui::Pos2| -> eframe::egui::Pos2 {
            let dx = p.x - center.x;
            let dy = p.y - center.y;
            let sx = dx * scale.x;
            let sy = dy * scale.y;
            let rx = sx * cos_r - sy * sin_r;
            let ry = sx * sin_r + sy * cos_r;
            eframe::egui::pos2(rx + center.x + offset.x, ry + center.y + offset.y)
        };
        
        let t_corners: Vec<eframe::egui::Pos2> = corners.iter().map(|&c| transform(c)).collect();
        
        let mut min_x = t_corners[0].x;
        let mut min_y = t_corners[0].y;
        let mut max_x = t_corners[0].x;
        let mut max_y = t_corners[0].y;
        
        for c in &t_corners {
            min_x = min_x.min(c.x);
            min_y = min_y.min(c.y);
            max_x = max_x.max(c.x);
            max_y = max_y.max(c.y);
        }
        
        let dst_min_x = min_x.floor() as i32;
        let dst_min_y = min_y.floor() as i32;
        let dst_max_x = max_x.ceil() as i32;
        let dst_max_y = max_y.ceil() as i32;

        // 3. Reverse mapping
        let mut dst_tiles: HashMap<(i32, i32), Vec<Color32>> = HashMap::new();
        
        for y in dst_min_y..dst_max_y {
            for x in dst_min_x..dst_max_x {
                // Inverse transform
                let dx = x as f32 - (center.x + offset.x);
                let dy = y as f32 - (center.y + offset.y);
                
                // Inverse Rotate
                let rx = dx * cos_r + dy * sin_r;
                let ry = -dx * sin_r + dy * cos_r;
                
                // Inverse Scale
                let sx = rx / scale.x;
                let sy = ry / scale.y;
                
                let src_x = (sx + center.x).round() as i32;
                let src_y = (sy + center.y).round() as i32;
                
                if let Some(pixel) = src_pixels.get(&(src_x, src_y)) {
                    let ntx = x.div_euclid(tile_size as i32);
                    let nty = y.div_euclid(tile_size as i32);
                    
                    let npx = (x - ntx * tile_size as i32) as usize;
                    let npy = (y - nty * tile_size as i32) as usize;

                    let dst_data = dst_tiles.entry((ntx, nty)).or_insert_with(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
                    let dst_idx = npy * tile_size + npx;
                    dst_data[dst_idx] = *pixel;
                }
            }
        }

        // 4. Apply back to layer (Clear first)
        if let Some(layer) = self.layers.get(layer_idx) {
            let mut tiles = layer.tiles.lock().unwrap();
            
            // Clear existing tiles
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap();
                cell.data = None;
            }

            // Write destination pixels
            for ((tx, ty), data) in dst_tiles {
                let tile_arc = tiles.entry((tx, ty)).or_insert_with(|| Arc::new(Mutex::new(TileCell { data: Some(vec![Color32::TRANSPARENT; tile_size * tile_size]) })));
                let mut guard = tile_arc.lock().unwrap();
                if guard.data.is_none() {
                    guard.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
                }
                
                if let Some(target_data) = &mut guard.data {
                    for i in 0..data.len() {
                        if data[i].a() > 0 {
                            target_data[i] = data[i];
                        }
                    }
                }
            }
        }
    }

    pub fn apply_transform(&mut self, offset: Vec2, rotation: f32, scale: Vec2, center: Vec2, selection: Option<&crate::selection::SelectionManager>, history: Option<&mut UndoAction>) {
        let layer_idx = self.active_layer_idx;
        let tile_size = self.tile_size;
        
        // 1. Collect all source pixels
        let mut src_pixels: HashMap<(i32, i32), Color32> = HashMap::new();
        let mut src_bounds = eframe::egui::Rect::NOTHING;
        let mut first = true;

        if let Some(layer) = self.layers.get(layer_idx) {
            let tiles = layer.tiles.lock().unwrap();
            for ((tx, ty), tile_arc) in tiles.iter() {
                let guard = tile_arc.lock().unwrap();
                if let Some(data) = &guard.data {
                    for py in 0..tile_size {
                        for px in 0..tile_size {
                            let idx = py * tile_size + px;
                            if data[idx].a() > 0 {
                                let gx = *tx * tile_size as i32 + px as i32;
                                let gy = *ty * tile_size as i32 + py as i32;
                                
                                if let Some(sel) = selection {
                                    if !sel.contains(Vec2::new(gx as f32, gy as f32)) {
                                        continue;
                                    }
                                }
                                src_pixels.insert((gx, gy), data[idx]);
                                
                                let pos = eframe::egui::pos2(gx as f32, gy as f32);
                                if first {
                                    src_bounds = eframe::egui::Rect::from_min_max(pos, pos);
                                    first = false;
                                } else {
                                    src_bounds.extend_with(pos);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        if src_pixels.is_empty() { return; }
        // Expand bounds slightly to cover the pixels fully
        src_bounds.max.x += 1.0;
        src_bounds.max.y += 1.0;

        // 2. Calculate destination bounds
        let corners = [
            src_bounds.min,
            eframe::egui::pos2(src_bounds.max.x, src_bounds.min.y),
            src_bounds.max,
            eframe::egui::pos2(src_bounds.min.x, src_bounds.max.y),
        ];
        
        let (sin_r, cos_r) = rotation.sin_cos();
        
        let transform = |p: eframe::egui::Pos2| -> eframe::egui::Pos2 {
            let dx = p.x - center.x;
            let dy = p.y - center.y;
            let sx = dx * scale.x;
            let sy = dy * scale.y;
            let rx = sx * cos_r - sy * sin_r;
            let ry = sx * sin_r + sy * cos_r;
            eframe::egui::pos2(rx + center.x + offset.x, ry + center.y + offset.y)
        };
        
        let t_corners: Vec<eframe::egui::Pos2> = corners.iter().map(|&c| transform(c)).collect();
        
        let mut min_x = t_corners[0].x;
        let mut min_y = t_corners[0].y;
        let mut max_x = t_corners[0].x;
        let mut max_y = t_corners[0].y;
        
        for c in &t_corners {
            min_x = min_x.min(c.x);
            min_y = min_y.min(c.y);
            max_x = max_x.max(c.x);
            max_y = max_y.max(c.y);
        }
        
        let dst_min_x = min_x.floor() as i32;
        let dst_min_y = min_y.floor() as i32;
        let dst_max_x = max_x.ceil() as i32;
        let dst_max_y = max_y.ceil() as i32;

        // 3. Reverse mapping
        let mut dst_tiles: HashMap<(i32, i32), Vec<Color32>> = HashMap::new();
        
        for y in dst_min_y..dst_max_y {
            for x in dst_min_x..dst_max_x {
                // Inverse transform
                let dx = x as f32 - (center.x + offset.x);
                let dy = y as f32 - (center.y + offset.y);
                
                // Inverse Rotate
                let rx = dx * cos_r + dy * sin_r;
                let ry = -dx * sin_r + dy * cos_r;
                
                // Inverse Scale
                let sx = rx / scale.x;
                let sy = ry / scale.y;
                
                let src_x = (sx + center.x).round() as i32;
                let src_y = (sy + center.y).round() as i32;
                
                if let Some(pixel) = src_pixels.get(&(src_x, src_y)) {
                    let ntx = x.div_euclid(tile_size as i32);
                    let nty = y.div_euclid(tile_size as i32);
                    
                    let npx = (x - ntx * tile_size as i32) as usize;
                    let npy = (y - nty * tile_size as i32) as usize;

                    let dst_data = dst_tiles.entry((ntx, nty)).or_insert_with(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
                    let dst_idx = npy * tile_size + npx;
                    dst_data[dst_idx] = *pixel;
                }
            }
        }

        // 4. Apply back to layer
        if let Some(layer) = self.layers.get(layer_idx) {
            let mut tiles = layer.tiles.lock().unwrap();
            
            // Record history
            if let Some(action) = history {
                let mut affected_tiles = std::collections::HashSet::new();
                
                // Source tiles
                for ((gx, gy), _) in &src_pixels {
                     let tx = gx.div_euclid(tile_size as i32);
                     let ty = gy.div_euclid(tile_size as i32);
                     affected_tiles.insert((tx, ty));
                }
                
                // Destination tiles
                for ((tx, ty), _) in &dst_tiles {
                    affected_tiles.insert((*tx, *ty));
                }
                
                for (tx, ty) in affected_tiles {
                    let data = if let Some(tile_arc) = tiles.get(&(tx, ty)) {
                        let guard = tile_arc.lock().unwrap();
                        guard.data.clone().unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_size * tile_size])
                    } else {
                        vec![Color32::TRANSPARENT; tile_size * tile_size]
                    };

                    action.tiles.push(crate::canvas::history::TileSnapshot {
                         tx,
                         ty,
                         layer_idx,
                         x0: 0,
                         y0: 0,
                         width: tile_size,
                         height: tile_size,
                         data,
                     });
                }
            }
            
            // Clear source pixels
            for ((gx, gy), _) in &src_pixels {
                 let tx = gx.div_euclid(tile_size as i32);
                 let ty = gy.div_euclid(tile_size as i32);
                 if let Some(tile_arc) = tiles.get(&(tx, ty)) {
                     let mut guard = tile_arc.lock().unwrap();
                     if let Some(data) = &mut guard.data {
                         let px = (gx - tx * tile_size as i32) as usize;
                         let py = (gy - ty * tile_size as i32) as usize;
                         let idx = py * tile_size + px;
                         data[idx] = Color32::TRANSPARENT;
                     }
                 }
            }

            // Write destination pixels
            for ((tx, ty), data) in dst_tiles {
                let tile_arc = tiles.entry((tx, ty)).or_insert_with(|| Arc::new(Mutex::new(TileCell { data: Some(vec![Color32::TRANSPARENT; tile_size * tile_size]) })));
                let mut guard = tile_arc.lock().unwrap();
                if guard.data.is_none() {
                    guard.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
                }
                
                if let Some(target_data) = &mut guard.data {
                    for i in 0..data.len() {
                        if data[i].a() > 0 {
                            target_data[i] = data[i];
                        }
                    }
                }
            }
        }
    }

    pub fn get_content_bounds(&self, layer_idx: usize, selection: Option<&crate::selection::SelectionManager>) -> Option<eframe::egui::Rect> {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        let mut found = false;

        if let Some(layer) = self.layers.get(layer_idx) {
            let tiles = layer.tiles.lock().unwrap();
            for ((tx, ty), tile_arc) in tiles.iter() {
                let guard = tile_arc.lock().unwrap();
                if let Some(data) = &guard.data {
                    for py in 0..self.tile_size {
                        for px in 0..self.tile_size {
                            let idx = py * self.tile_size + px;
                            if data[idx].a() > 0 {
                                let gx = *tx * self.tile_size as i32 + px as i32;
                                let gy = *ty * self.tile_size as i32 + py as i32;

                                if let Some(sel) = selection {
                                    if !sel.contains(Vec2::new(gx as f32, gy as f32)) {
                                        continue;
                                    }
                                }

                                min_x = min_x.min(gx);
                                min_y = min_y.min(gy);
                                max_x = max_x.max(gx);
                                max_y = max_y.max(gy);
                                found = true;
                            }
                        }
                    }
                }
            }
        }

        if found {
            Some(eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(min_x as f32, min_y as f32),
                eframe::egui::pos2(max_x as f32 + 1.0, max_y as f32 + 1.0),
            ))
        } else {
            None
        }
    }

    /// Merge the specified layer down into the layer below it.
    /// This combines their tile data according to the visible pixels and opacity.
    /// The upper layer (source) is removed after the merge.
    pub fn float_selection(&mut self, selection: &SelectionManager) -> Option<usize> {
        if !selection.has_selection() {
            return None;
        }

        let active_idx = self.active_layer_idx;
        if active_idx >= self.layers.len() {
            return None;
        }

        // Create new layer
        let new_layer = Layer::new("Floating Selection".to_string(), self.width, self.height, self.tile_size);
        
        let active_layer = &self.layers[active_idx];
        let active_tiles_map = active_layer.tiles.lock().unwrap();
        
        let mut tiles_to_process = Vec::new();
        for (&(tx, ty), tile_arc) in active_tiles_map.iter() {
            tiles_to_process.push(((tx, ty), tile_arc.clone()));
        }
        drop(active_tiles_map);
        
        let mut new_layer_tiles = new_layer.tiles.lock().unwrap();
        
        for ((tx, ty), tile_arc) in tiles_to_process {
            let mut tile = tile_arc.lock().unwrap();
            if let Some(data) = &mut tile.data {
                let mut new_tile_data = vec![Color32::TRANSPARENT; self.tile_size * self.tile_size];
                let mut has_content = false;

                for y in 0..self.tile_size {
                    for x in 0..self.tile_size {
                        let px = tx * (self.tile_size as i32) + (x as i32);
                        let py = ty * (self.tile_size as i32) + (y as i32);
                        
                        if selection.contains(Vec2::new(px as f32, py as f32)) {
                            let idx = y * self.tile_size + x;
                            let color = data[idx];
                            if color != Color32::TRANSPARENT {
                                new_tile_data[idx] = color;
                                data[idx] = Color32::TRANSPARENT;
                                has_content = true;
                            }
                        }
                    }
                }
                
                if has_content {
                    let new_tile = Arc::new(Mutex::new(TileCell { data: Some(new_tile_data) }));
                    new_layer_tiles.insert((tx, ty), new_tile);
                }
            }
        }
        
        drop(new_layer_tiles);
        
        self.layers.push(new_layer);
        self.active_layer_idx = self.layers.len() - 1;
        
        Some(self.active_layer_idx)
    }

    pub fn merge_layer_down(&mut self, layer_idx: usize) {
        if layer_idx == 0 || layer_idx >= self.layers.len() {
            return;
        }

        // Remove the top layer (source)
        let top_layer = self.layers.remove(layer_idx);
        
        {
            // Get the bottom layer (destination)
            // Note: indices shifted after remove, so the layer that was at layer_idx - 1 is still at layer_idx - 1
            let bottom_layer = &mut self.layers[layer_idx - 1];

            let top_tiles = top_layer.tiles.lock().unwrap();
            let mut bottom_tiles = bottom_layer.tiles.lock().unwrap();

            for ((tx, ty), top_tile_arc) in top_tiles.iter() {
                let top_guard = top_tile_arc.lock().unwrap();
                if let Some(top_data) = &top_guard.data {
                    // Ensure bottom tile exists
                    let bottom_tile_arc = bottom_tiles
                        .entry((*tx, *ty))
                        .or_insert_with(|| Arc::new(Mutex::new(TileCell { data: None })));
                    
                    let mut bottom_guard = bottom_tile_arc.lock().unwrap();
                    
                    // Initialize bottom data if missing
                    if bottom_guard.data.is_none() {
                         bottom_guard.data = Some(vec![Color32::TRANSPARENT; self.tile_size * self.tile_size]);
                    }

                    if let Some(bottom_data) = &mut bottom_guard.data {
                        for i in 0..bottom_data.len() {
                            let src_pixel = apply_opacity_scale(top_data[i], top_layer.opacity as u32);
                            bottom_data[i] = alpha_over(src_pixel, bottom_data[i]);
                        }
                    }
                }
            }
        }

        // Adjust active layer index if needed
        if self.active_layer_idx >= self.layers.len() {
            self.active_layer_idx = self.layers.len() - 1;
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
