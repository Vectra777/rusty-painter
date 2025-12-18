use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{Color32, ColorImage, Rgba};
use wide::f32x4;

use crate::utils::color::{Color, ColorManipulation};
use crate::utils::profiler::ScopeTimer;
use crate::utils::vector::Vec2;
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;

// Gamma correction lookup table (4096 entries for high precision)
static GAMMA_LUT: OnceLock<[u8; 4096]> = OnceLock::new();

fn gamma_lut() -> &'static [u8; 4096] {
    GAMMA_LUT.get_or_init(|| {
        let mut lut = [0u8; 4096];
        for i in 0..4096 {
            let linear = i as f32 / 4095.0;
            let srgb = if linear <= 0.0031308 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            lut[i] = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        lut
    })
}

/// Fast linear to sRGB conversion using lookup table (eliminates powf)
#[inline]
fn linear_to_srgb_u8(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let index = (clamped * 4095.0).round() as usize;
    gamma_lut()[index.min(4095)]
}

/// Fast Rgba (linear) to Color32 (sRGB) conversion without powf
#[inline]
fn rgba_to_color32_fast(rgba: Rgba) -> Color32 {
    Color32::from_rgba_premultiplied(
        linear_to_srgb_u8(rgba.r()),
        linear_to_srgb_u8(rgba.g()),
        linear_to_srgb_u8(rgba.b()),
        (rgba.a() * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

#[derive(Debug)]
/// Single painting layer with its own opacity, visibility and tile storage.
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: f32, // 0..1
    pub locked: bool,
    tiles: Mutex<HashMap<(i32, i32), Arc<Mutex<TileCell>>>>,
}

impl Layer {
    /// Allocate a new layer backing store but keep tile data lazy.
    fn new(name: String, _width: usize, _height: usize, _tile_size: usize) -> Self {
        Self {
            name,
            visible: true,
            opacity: 1.0,
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
    /// True if the tile contains only transparent pixels
    pub is_empty: bool,
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
                .or_insert_with(|| Arc::new(Mutex::new(TileCell { data: None, is_empty: true })))
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
                guard.is_empty = fill_color == Color32::TRANSPARENT;
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
            let is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
            guard.is_empty = is_empty;
            guard.data = Some(data);
        }
    }

    /// Mark a tile as having content (not empty). Called after brush operations.
    #[inline]
    pub(crate) fn mark_tile_dirty(&self, tx: usize, ty: usize) {
        if let Some(tile_arc) = self.tile_cell(tx as i32, ty as i32) {
            let mut guard = tile_arc.lock().unwrap();
            guard.is_empty = false;
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
            // Fast path: Single tile access
            let tx = start_tx as i32;
            let ty = start_ty as i32;

            // 1. Get Arcs (Locking the map briefly)
            let layer_arcs: Vec<Option<Arc<Mutex<TileCell>>>> = self
                .layers
                .iter()
                .map(|layer| {
                    let tiles = layer.tiles.lock().unwrap();
                    tiles.get(&(tx, ty)).cloned()
                })
                .collect();

            // 2. Lock the Tiles (Holding locks for the render duration)
            let layer_guards: Vec<Option<std::sync::MutexGuard<'_, TileCell>>> = layer_arcs
                .iter()
                .map(|opt| opt.as_ref().map(|arc| arc.lock().unwrap()))
                .collect();

            // 3. Pre-convert all tiles to linear space to avoid repeated conversions
            let tile_pixel_count = self.tile_size * self.tile_size;
            let mut linear_tiles: Vec<Option<Vec<Rgba>>> = Vec::with_capacity(self.layers.len());
            
            for opt_guard in layer_guards.iter() {
                if let Some(guard) = opt_guard {
                    if let Some(data) = &guard.data {
                        // Convert entire tile to linear space once
                        let mut linear_data = Vec::with_capacity(tile_pixel_count);
                        for &pixel in data.iter() {
                            linear_data.push(Rgba::from(pixel));
                        }
                        linear_tiles.push(Some(linear_data));
                    } else {
                        linear_tiles.push(None);
                    }
                } else {
                    linear_tiles.push(None);
                }
            }

            // 4. Pre-calculate layer visibility and opacity to avoid lookups in the pixel loop
            // Stores: (is_visible, opacity, has_data_guard_index, is_background, is_empty)
            let layer_props: Vec<(bool, f32, usize, bool, bool)> = layer_guards.iter().enumerate().map(|(i, opt_guard)| {
                let is_visible = self.layers[i].visible && self.layers[i].opacity > 0.0;
                let is_empty = opt_guard.as_ref().map_or(i != 0, |g| g.is_empty);
                (is_visible, self.layers[i].opacity, i, i == 0, is_empty)
            }).collect();
            
            // Pre-convert clear_color to linear space
            let clear_color_linear = Rgba::from(self.clear_color);

            if true { 
                for dst_y in 0..dst_h {
                    let global_y_start = y + dst_y * step;
                    let row_start = dst_y * dst_w;

                    for dst_x in 0..dst_w {
                        let global_x_start = x + dst_x * step;

                        if step == 1 {
                            // --- FAST PATH (1:1 Rendering) ---
                            let local_y = global_y_start % self.tile_size;
                            let local_x = global_x_start % self.tile_size;
                            let src_idx = local_y * self.tile_size + local_x;

                            // Linear Accumulator (starts transparent)
                            let mut composite = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                            for (i, (visible, opacity, _, is_bg, is_empty)) in layer_props.iter().enumerate() {
                                if !visible || *is_empty { continue; }

                                // Get pixel in linear space (already converted)
                                let src = if let Some(linear_data) = &linear_tiles[i] {
                                    linear_data[src_idx]
                                } else if *is_bg {
                                    clear_color_linear
                                } else {
                                    Rgba::TRANSPARENT
                                };

                                if src.a() == 0.0 { continue; }

                                // Apply Opacity and Blend (already in linear space)
                                let src = if *opacity < 1.0 { src * *opacity } else { src };
                                
                                // Linear Blend: Src Over Composite
                                composite = src + composite * (1.0 - src.a());
                            }
                            
                            // 4. Convert Linear Float -> sRGB (Once at the end) - Fast LUT-based
                            out.pixels[row_start + dst_x] = rgba_to_color32_fast(composite);

                        } else {
                            // --- DOWNSAMPLING PATH (High Quality) ---
                            let mut r_acc = 0.0;
                            let mut g_acc = 0.0;
                            let mut b_acc = 0.0;
                            let mut a_acc = 0.0;
                            let mut count = 0.0;

                            for sy in 0..step {
                                let global_y = global_y_start + sy;
                                if global_y >= y + h { continue; }
                                let local_y = global_y % self.tile_size;

                                for sx in 0..step {
                                    let global_x = global_x_start + sx;
                                    if global_x >= x + w { continue; }
                                    let local_x = global_x % self.tile_size;

                                    let src_idx = local_y * self.tile_size + local_x;
                                    
                                    // Calculate the color for this sub-pixel using Linear Math
                                    let mut sub_composite = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                                    for (i, (visible, opacity, _, is_bg, is_empty)) in layer_props.iter().enumerate() {
                                        if !visible || *is_empty { continue; }

                                        // Get pixel in linear space (already converted)
                                        let src = if let Some(linear_data) = &linear_tiles[i] {
                                            linear_data[src_idx]
                                        } else if *is_bg {
                                            clear_color_linear
                                        } else {
                                            Rgba::TRANSPARENT
                                        };

                                        if src.a() == 0.0 { continue; }

                                        // Apply Opacity and Blend (already in linear space)
                                        let src = if *opacity < 1.0 { src * *opacity } else { src };
                                        sub_composite = src + sub_composite * (1.0 - src.a());
                                    }

                                    r_acc += sub_composite.r();
                                    g_acc += sub_composite.g();
                                    b_acc += sub_composite.b();
                                    a_acc += sub_composite.a();
                                    count += 1.0;
                                }
                            }

                            if count > 0.0 {
                                let inv = 1.0 / count;
                                // Convert the averaged Linear result back to sRGB - Fast LUT-based
                                out.pixels[row_start + dst_x] = rgba_to_color32_fast(Rgba::from_rgba_premultiplied(
                                    r_acc * inv,
                                    g_acc * inv,
                                    b_acc * inv,
                                    a_acc * inv
                                ));
                            }
                        }
                    }
                }
            }
            return;
        }

        // --- FALLBACK (Multi-tile / Slow Path) ---
        // Optimization: Cache tiles per row to reduce HashMap lookups
        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let ty = (global_y / self.tile_size) as i32;
            let local_y = global_y % self.tile_size;
            
            // Cache tile references for this row across all layers
            // Tuple: (tile_arc, cached_tx, is_empty)
            let mut row_tile_cache: Vec<Option<(Arc<Mutex<TileCell>>, i32, bool)>> = vec![None; self.layers.len()];
            
            let mut dst_x = 0;
            while dst_x < dst_w {
                let global_x = x + dst_x * step;
                let tx = (global_x / self.tile_size) as i32;
                let local_x = global_x % self.tile_size;

                let dst_start = dst_y * dst_w + dst_x;

                let mut composite = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                for (layer_idx, layer) in self.layers.iter().enumerate() {
                    if !layer.visible || layer.opacity <= 0.0 { continue; }

                    // Check cache first
                    let needs_lookup = row_tile_cache[layer_idx]
                        .as_ref()
                        .map_or(true, |(_, cached_tx, _)| *cached_tx != tx);
                    
                    if needs_lookup {
                        row_tile_cache[layer_idx] = self.layer_tile_cell(layer_idx, tx, ty)
                            .map(|arc| {
                                let is_empty = arc.lock().unwrap().is_empty;
                                (arc, tx, is_empty)
                            });
                    }

                    // Skip if tile is empty
                    if let Some((_, _, is_empty)) = &row_tile_cache[layer_idx] {
                        if *is_empty { continue; }
                    } else if layer_idx != 0 {
                        continue; // Non-background layer with no tile
                    }

                    // Resolve Pixel from cache
                    let pixel_c32 = if let Some((cell, _, _)) = &row_tile_cache[layer_idx] {
                        let guard = cell.lock().unwrap();
                        if let Some(data) = guard.data.as_ref() {
                            let src_idx = local_y * self.tile_size + local_x;
                            data[src_idx]
                        } else if layer_idx == 0 {
                            self.clear_color
                        } else {
                            Color32::TRANSPARENT
                        }
                    } else if layer_idx == 0 {
                        self.clear_color
                    } else {
                        Color32::TRANSPARENT
                    };

                    if pixel_c32 == Color32::TRANSPARENT { continue; }

                    // Linear Blend
                    let mut src = Rgba::from(pixel_c32);
                    if layer.opacity < 1.0 {
                        src = src * layer.opacity;
                    }
                    composite = src + composite * (1.0 - src.a());
                }

                out.pixels[dst_start] = rgba_to_color32_fast(composite);
                dst_x += 1;
            }
        }
    }

    /// Clear the active layer to the provided color (or transparent for non-background).
    pub fn clear(&mut self, color: Color) {
        self.clear_color = premultiply(color.to_color32());
        if let Some(layer) = self.layers.get(self.active_layer_idx) {
            let tiles = layer.tiles.lock().unwrap();
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap();
                cell.data = None;
                cell.is_empty = true;
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
        // Optimization: Pre-allocate with estimated capacity
        let estimated_pixels = src_tiles.len() * tile_size * tile_size / 4; // Assume 25% fill
        let mut src_pixels: HashMap<(i32, i32), Color32> = HashMap::with_capacity(estimated_pixels);
        let mut src_bounds = eframe::egui::Rect::NOTHING;
        let mut first = true;

        for ((tx, ty), data) in src_tiles {
            let base_x = *tx * tile_size as i32;
            let base_y = *ty * tile_size as i32;
            
            for py in 0..tile_size {
                for px in 0..tile_size {
                    let idx = py * tile_size + px;
                    if data[idx].a() > 0 {
                        let gx = base_x + px as i32;
                        let gy = base_y + py as i32;
                        
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
                cell.is_empty = true;
            }

            // Write destination pixels
            for ((tx, ty), data) in dst_tiles {
                let tile_arc = tiles.entry((tx, ty)).or_insert_with(|| Arc::new(Mutex::new(TileCell { data: Some(vec![Color32::TRANSPARENT; tile_size * tile_size]), is_empty: true })));
                let mut guard = tile_arc.lock().unwrap();
                if guard.data.is_none() {
                    guard.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
                }
                
                let mut has_content = false;
                if let Some(target_data) = &mut guard.data {
                    for i in 0..data.len() {
                        if data[i].a() > 0 {
                            target_data[i] = data[i];
                            has_content = true;
                        }
                    }
                }
                guard.is_empty = !has_content;
            }
        }
    }

    pub fn apply_transform(&mut self, offset: Vec2, rotation: f32, scale: Vec2, center: Vec2, selection: Option<&crate::selection::SelectionManager>, history: Option<&mut UndoAction>) {
        let layer_idx = self.active_layer_idx;
        let tile_size = self.tile_size;
        
        // 1. Collect all source pixels
        let estimated_pixels = 1024; // Initial capacity
        let mut src_pixels: HashMap<(i32, i32), Color32> = HashMap::with_capacity(estimated_pixels);
        let mut src_bounds = eframe::egui::Rect::NOTHING;
        let mut first = true;

        if let Some(layer) = self.layers.get(layer_idx) {
            let tiles = layer.tiles.lock().unwrap();
            for ((tx, ty), tile_arc) in tiles.iter() {
                let guard = tile_arc.lock().unwrap();
                if let Some(data) = &guard.data {
                    let base_x = *tx * tile_size as i32;
                    let base_y = *ty * tile_size as i32;
                    
                    for py in 0..tile_size {
                        for px in 0..tile_size {
                            let idx = py * tile_size + px;
                            if data[idx].a() > 0 {
                                let gx = base_x + px as i32;
                                let gy = base_y + py as i32;
                                
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
        let estimated_dst_tiles = ((dst_max_x - dst_min_x) * (dst_max_y - dst_min_y)) / (tile_size as i32 * tile_size as i32) + 4;
        let mut dst_tiles: HashMap<(i32, i32), Vec<Color32>> = HashMap::with_capacity(estimated_dst_tiles as usize);
        let tile_size_i32 = tile_size as i32;
        let center_offset_x = center.x + offset.x;
        let center_offset_y = center.y + offset.y;
        let inv_scale_x = 1.0 / scale.x;
        let inv_scale_y = 1.0 / scale.y;
        
        for y in dst_min_y..dst_max_y {
            for x in dst_min_x..dst_max_x {
                // Inverse transform
                let dx = x as f32 - center_offset_x;
                let dy = y as f32 - center_offset_y;
                
                // Inverse Rotate
                let rx = dx * cos_r + dy * sin_r;
                let ry = -dx * sin_r + dy * cos_r;
                
                // Inverse Scale
                let sx = rx * inv_scale_x;
                let sy = ry * inv_scale_y;
                
                let src_x = (sx + center.x).round() as i32;
                let src_y = (sy + center.y).round() as i32;
                
                if let Some(pixel) = src_pixels.get(&(src_x, src_y)) {
                    let ntx = x.div_euclid(tile_size_i32);
                    let nty = y.div_euclid(tile_size_i32);
                    
                    let npx = (x - ntx * tile_size_i32) as usize;
                    let npy = (y - nty * tile_size_i32) as usize;

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
                
                // Source tiles - cache div_euclid results
                for ((gx, gy), _) in &src_pixels {
                     let tx = gx.div_euclid(tile_size_i32);
                     let ty = gy.div_euclid(tile_size_i32);
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
            
            // Clear source pixels - cache tile coordinates and batch by tile
            let mut clear_ops: HashMap<(i32, i32), Vec<(usize, usize)>> = HashMap::new();
            for ((gx, gy), _) in &src_pixels {
                 let tx = gx.div_euclid(tile_size_i32);
                 let ty = gy.div_euclid(tile_size_i32);
                 let px = (gx - tx * tile_size_i32) as usize;
                 let py = (gy - ty * tile_size_i32) as usize;
                 clear_ops.entry((tx, ty)).or_insert_with(Vec::new).push((px, py));
            }
            
            for ((tx, ty), pixel_coords) in clear_ops {
                if let Some(tile_arc) = tiles.get(&(tx, ty)) {
                    let mut guard = tile_arc.lock().unwrap();
                    if let Some(data) = &mut guard.data {
                        for (px, py) in pixel_coords {
                            let idx = py * tile_size + px;
                            data[idx] = Color32::TRANSPARENT;
                        }
                    }
                }
            }

            // Write destination pixels
            for ((tx, ty), data) in dst_tiles {
                let tile_arc = tiles.entry((tx, ty)).or_insert_with(|| Arc::new(Mutex::new(TileCell { data: Some(vec![Color32::TRANSPARENT; tile_size * tile_size]), is_empty: true })));
                let mut guard = tile_arc.lock().unwrap();
                if guard.data.is_none() {
                    guard.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
                }
                
                let mut has_content = false;
                if let Some(target_data) = &mut guard.data {
                    for i in 0..data.len() {
                        if data[i].a() > 0 {
                            target_data[i] = data[i];
                            has_content = true;
                        }
                    }
                }
                guard.is_empty = !has_content;
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
                    let new_tile = Arc::new(Mutex::new(TileCell { data: Some(new_tile_data), is_empty: false }));
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
                    // Skip empty top tiles
                    if top_guard.is_empty {
                        continue;
                    }
                    
                    // Ensure bottom tile exists
                    let bottom_tile_arc = bottom_tiles
                        .entry((*tx, *ty))
                        .or_insert_with(|| Arc::new(Mutex::new(TileCell { data: None, is_empty: true })));
                    
                    let mut bottom_guard = bottom_tile_arc.lock().unwrap();
                    
                    // Initialize bottom data if missing
                    if bottom_guard.data.is_none() {
                         bottom_guard.data = Some(vec![Color32::TRANSPARENT; self.tile_size * self.tile_size]);
                    }

                    if let Some(bottom_data) = &mut bottom_guard.data {
                        // Use SIMD batch processing for better performance
                        let tile_len = bottom_data.len();
                        
                        // Apply opacity to source pixels and prepare for batch blend
                        let mut src_with_opacity = vec![Color32::TRANSPARENT; tile_len];
                        for i in 0..tile_len {
                            src_with_opacity[i] = apply_opacity_scale(top_data[i], top_layer.opacity);
                        }
                        
                        // Create temporary output buffer
                        let mut blended = vec![Color32::TRANSPARENT; tile_len];
                        
                        // Batch blend using SIMD
                        alpha_over_batch(&src_with_opacity, bottom_data, &mut blended);
                        
                        // Copy result back
                        *bottom_data = blended;
                        
                        // Update is_empty flag
                        bottom_guard.is_empty = bottom_data.iter().all(|&p| p == Color32::TRANSPARENT);
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


/// SIMD-optimized alpha blending for 4 pixels at once
#[inline]
pub fn alpha_over_simd_x4(src: [Color32; 4], dst: [Color32; 4]) -> [Color32; 4] {
    // Convert 4 source pixels to linear space
    let s0 = Rgba::from(src[0]);
    let s1 = Rgba::from(src[1]);
    let s2 = Rgba::from(src[2]);
    let s3 = Rgba::from(src[3]);
    
    // Pack into SIMD vectors (Structure of Arrays layout for better vectorization)
    let sr = f32x4::new([s0.r(), s1.r(), s2.r(), s3.r()]);
    let sg = f32x4::new([s0.g(), s1.g(), s2.g(), s3.g()]);
    let sb = f32x4::new([s0.b(), s1.b(), s2.b(), s3.b()]);
    let sa = f32x4::new([s0.a(), s1.a(), s2.a(), s3.a()]);
    
    // Convert 4 destination pixels to linear space
    let d0 = Rgba::from(dst[0]);
    let d1 = Rgba::from(dst[1]);
    let d2 = Rgba::from(dst[2]);
    let d3 = Rgba::from(dst[3]);
    
    let dr = f32x4::new([d0.r(), d1.r(), d2.r(), d3.r()]);
    let dg = f32x4::new([d0.g(), d1.g(), d2.g(), d3.g()]);
    let db = f32x4::new([d0.b(), d1.b(), d2.b(), d3.b()]);
    let da = f32x4::new([d0.a(), d1.a(), d2.a(), d3.a()]);
    
    // Alpha over blend in SIMD: out = src + dst * (1 - src.a)
    let one = f32x4::splat(1.0);
    let inv_alpha = one - sa;
    
    let out_r = sr + dr * inv_alpha;
    let out_g = sg + dg * inv_alpha;
    let out_b = sb + db * inv_alpha;
    let out_a = sa + da * inv_alpha;
    
    // Convert back to Color32 (sRGB)
    let r = out_r.to_array();
    let g = out_g.to_array();
    let b = out_b.to_array();
    let a = out_a.to_array();
    
    [
        rgba_to_color32_fast(Rgba::from_rgba_premultiplied(r[0], g[0], b[0], a[0])),
        rgba_to_color32_fast(Rgba::from_rgba_premultiplied(r[1], g[1], b[1], a[1])),
        rgba_to_color32_fast(Rgba::from_rgba_premultiplied(r[2], g[2], b[2], a[2])),
        rgba_to_color32_fast(Rgba::from_rgba_premultiplied(r[3], g[3], b[3], a[3])),
    ]
}

/// Batch SIMD blend: process entire slices with SIMD acceleration
#[inline]
pub fn alpha_over_batch(src: &[Color32], dst: &[Color32], out: &mut [Color32]) {
    assert_eq!(src.len(), dst.len());
    assert_eq!(src.len(), out.len());
    
    let len = src.len();
    let simd_len = len / 4 * 4;
    
    // Process 4 pixels at a time with SIMD
    let mut i = 0;
    while i < simd_len {
        let src_chunk = [
            src[i],
            src[i + 1],
            src[i + 2],
            src[i + 3],
        ];
        let dst_chunk = [
            dst[i],
            dst[i + 1],
            dst[i + 2],
            dst[i + 3],
        ];
        
        let result = alpha_over_simd_x4(src_chunk, dst_chunk);
        
        out[i] = result[0];
        out[i + 1] = result[1];
        out[i + 2] = result[2];
        out[i + 3] = result[3];
        
        i += 4;
    }
    
    // Handle remaining pixels with scalar code
    for i in simd_len..len {
        out[i] = alpha_over(src[i], dst[i]);
    }
}

/// Scalar alpha blending (fallback and for single pixels)
#[inline]
pub fn alpha_over(src: Color32, dst: Color32) -> Color32 {
    // 1. Convert sRGB u8 to Linear f32
    let src_l = Rgba::from(src);
    let dst_l = Rgba::from(dst);

    // 2. Perform the blend in Linear space
    let inv_alpha = 1.0 - src_l.a();
    
    let out_r = src_l.r() + dst_l.r() * inv_alpha;
    let out_g = src_l.g() + dst_l.g() * inv_alpha;
    let out_b = src_l.b() + dst_l.b() * inv_alpha;
    let out_a = src_l.a() + dst_l.a() * inv_alpha;

    // 3. Construct the result and convert back to sRGB u8 - Fast LUT-based
    rgba_to_color32_fast(Rgba::from_rgba_premultiplied(out_r, out_g, out_b, out_a))
}

#[inline]
fn apply_opacity_scale(color: Color32, opacity_scale: f32) -> Color32 {
    if opacity_scale >= 1.0 {
        return color;
    }
    if opacity_scale <= 0.0 {
        return Color32::TRANSPARENT;
    }

    // Convert to linear, multiply all channels (RGBA) by scale, convert back
    let linear = Rgba::from(color) * opacity_scale;
    Color32::from(linear)
}

fn premultiply(color: Color32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    let linear = Rgba::from_rgba_unmultiplied(
        r as f32 / 255.0, 
        g as f32 / 255.0, 
        b as f32 / 255.0, 
        a as f32 / 255.0
    );
    Color32::from(linear)
}

fn unpremultiply(color: Color32) -> Color32 {
    let linear = Rgba::from(color);
    let a = linear.a();
    
    if a <= 0.0 || a >= 1.0 {
        return color;
    }

    // Divide RGB by Alpha to "un-stretch" the color
    let r = linear.r() / a;
    let g = linear.g() / a;
    let b = linear.b() / a;

    // Convert back to sRGB u8
    Color32::from(Rgba::from_rgba_premultiplied(r, g, b, a))
}
