use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use eframe::egui::{Color32, ColorImage, Rgba};

use crate::canvas::blend::{apply_opacity_scale, premultiply, rgba_to_color32_fast};
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;
use crate::utils::color::{Color, ColorManipulation};
use eframe::egui::Vec2;

const MAX_TRANSFORM_SOURCE_PIXELS: usize = 67_108_864;
type TileMap = HashMap<(i32, i32), Arc<Mutex<TileCell>>>;
type RowTileCache = Vec<Option<(i32, Arc<Mutex<TileCell>>, Option<Vec<Rgba>>, bool)>>;

pub use crate::canvas::blend::{
    LinearBrushColor, alpha_over, alpha_over_batch, alpha_over_brush, blend_erase,
};

/// Transform operation parameters
#[derive(Clone, Copy, Debug)]
pub struct TransformParams {
    pub offset: Vec2,
    pub rotation: f32,
    pub scale: Vec2,
    pub center: Vec2,
}

impl TransformParams {
    pub fn new(offset: Vec2, rotation: f32, scale: Vec2, center: Vec2) -> Self {
        Self {
            offset,
            rotation,
            scale,
            center,
        }
    }
}

fn source_bounds_and_tiles(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    tile_size: usize,
    selection: Option<&SelectionManager>,
) -> Option<(eframe::egui::Rect, HashSet<(i32, i32)>)> {
    let mut bounds = eframe::egui::Rect::NOTHING;
    let mut affected = HashSet::new();
    let mut found = false;

    for ((tx, ty), data) in src_tiles {
        let base_x = tx * tile_size as i32;
        let base_y = ty * tile_size as i32;
        let mut tile_has_content = false;

        for py in 0..tile_size {
            for px in 0..tile_size {
                let idx = py * tile_size + px;
                if data[idx].a() == 0 {
                    continue;
                }

                let gx = base_x + px as i32;
                let gy = base_y + py as i32;
                if let Some(sel) = selection
                    && !sel.contains_coords(gx as f32, gy as f32)
                {
                    continue;
                }

                tile_has_content = true;
                let pos = eframe::egui::pos2(gx as f32, gy as f32);
                if found {
                    bounds.extend_with(pos);
                } else {
                    bounds = eframe::egui::Rect::from_min_max(pos, pos);
                    found = true;
                }
            }
        }

        if tile_has_content {
            affected.insert((*tx, *ty));
        }
    }

    found.then_some((bounds, affected))
}

fn sample_source_tile(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    x: i32,
    y: i32,
    tile_size: usize,
    selection: Option<&SelectionManager>,
) -> Color32 {
    if let Some(sel) = selection
        && !sel.contains_coords(x as f32, y as f32)
    {
        return Color32::TRANSPARENT;
    }

    let tile_size_i32 = tile_size as i32;
    let tx = x.div_euclid(tile_size_i32);
    let ty = y.div_euclid(tile_size_i32);
    let px = (x - tx * tile_size_i32) as usize;
    let py = (y - ty * tile_size_i32) as usize;
    src_tiles
        .get(&(tx, ty))
        .map_or(Color32::TRANSPARENT, |data| data[py * tile_size + px])
}

fn transform_tiles(
    src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
    mut src_bounds: eframe::egui::Rect,
    params: TransformParams,
    tile_size: usize,
    canvas_width: usize,
    canvas_height: usize,
    selection: Option<&SelectionManager>,
) -> HashMap<(i32, i32), Vec<Color32>> {
    if params.scale.x.abs() < f32::EPSILON
        || params.scale.y.abs() < f32::EPSILON
        || !params.offset.is_finite()
        || !params.scale.is_finite()
        || !params.center.is_finite()
        || !params.rotation.is_finite()
    {
        return HashMap::new();
    }

    src_bounds.max.x += 1.0;
    src_bounds.max.y += 1.0;

    let corners = [
        src_bounds.min,
        eframe::egui::pos2(src_bounds.max.x, src_bounds.min.y),
        src_bounds.max,
        eframe::egui::pos2(src_bounds.min.x, src_bounds.max.y),
    ];

    let (sin_r, cos_r) = params.rotation.sin_cos();
    let transform = |p: eframe::egui::Pos2| -> eframe::egui::Pos2 {
        let dx = p.x - params.center.x;
        let dy = p.y - params.center.y;
        let sx = dx * params.scale.x;
        let sy = dy * params.scale.y;
        let rx = sx * cos_r - sy * sin_r;
        let ry = sx * sin_r + sy * cos_r;
        eframe::egui::pos2(
            rx + params.center.x + params.offset.x,
            ry + params.center.y + params.offset.y,
        )
    };

    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for corner in corners.map(transform) {
        min_x = min_x.min(corner.x);
        min_y = min_y.min(corner.y);
        max_x = max_x.max(corner.x);
        max_y = max_y.max(corner.y);
    }

    let dst_min_x = min_x.floor() as i32;
    let dst_min_y = min_y.floor() as i32;
    let dst_max_x = max_x.ceil() as i32;
    let dst_max_y = max_y.ceil() as i32;
    let tile_size_i32 = tile_size as i32;
    let center_offset_x = params.center.x + params.offset.x;
    let center_offset_y = params.center.y + params.offset.y;
    let inv_scale_x = 1.0 / params.scale.x;
    let inv_scale_y = 1.0 / params.scale.y;
    let estimated_dst_tiles =
        ((dst_max_x - dst_min_x) * (dst_max_y - dst_min_y)) / (tile_size_i32 * tile_size_i32) + 4;
    let mut dst_tiles = HashMap::with_capacity(estimated_dst_tiles.max(0) as usize);

    for y in dst_min_y..dst_max_y {
        if y < 0 || y >= canvas_height as i32 {
            continue;
        }
        for x in dst_min_x..dst_max_x {
            if x < 0 || x >= canvas_width as i32 {
                continue;
            }
            let dx = x as f32 - center_offset_x;
            let dy = y as f32 - center_offset_y;
            let rx = dx * cos_r + dy * sin_r;
            let ry = -dx * sin_r + dy * cos_r;
            let src_x = (rx * inv_scale_x + params.center.x).round() as i32;
            let src_y = (ry * inv_scale_y + params.center.y).round() as i32;

            if src_x < src_bounds.min.x.floor() as i32
                || src_x >= src_bounds.max.x.ceil() as i32
                || src_y < src_bounds.min.y.floor() as i32
                || src_y >= src_bounds.max.y.ceil() as i32
            {
                continue;
            }

            let pixel = sample_source_tile(src_tiles, src_x, src_y, tile_size, selection);
            if pixel == Color32::TRANSPARENT {
                continue;
            }

            let ntx = x.div_euclid(tile_size_i32);
            let nty = y.div_euclid(tile_size_i32);
            let npx = (x - ntx * tile_size_i32) as usize;
            let npy = (y - nty * tile_size_i32) as usize;
            let dst_data = dst_tiles
                .entry((ntx, nty))
                .or_insert_with(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
            dst_data[npy * tile_size + npx] = pixel;
        }
    }

    dst_tiles
}

fn write_transformed_tiles(
    tiles: &mut TileMap,
    dst_tiles: HashMap<(i32, i32), Vec<Color32>>,
    tile_size: usize,
) {
    for ((tx, ty), data) in dst_tiles {
        let tile_arc = tiles.entry((tx, ty)).or_insert_with(|| {
            Arc::new(Mutex::new(TileCell {
                data: Some(vec![Color32::TRANSPARENT; tile_size * tile_size]),
                is_empty: true,
            }))
        });
        let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
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

#[derive(Debug)]
/// Single painting layer with its own opacity, visibility and tile storage.
pub struct Layer {
    pub name: String,
    pub visible: bool,
    pub opacity: f32, // 0..1
    pub locked: bool,
    tiles: Mutex<TileMap>,
}

#[derive(Clone)]
pub struct CanvasTileSnapshot {
    pub tx: i32,
    pub ty: i32,
    pub data: Vec<Color32>,
}

#[derive(Clone)]
pub struct CanvasLayerSnapshot {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub locked: bool,
    pub tiles: Vec<CanvasTileSnapshot>,
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

    fn from_snapshot(snapshot: CanvasLayerSnapshot) -> Self {
        let mut tiles = HashMap::new();
        for tile in snapshot.tiles {
            tiles.insert(
                (tile.tx, tile.ty),
                Arc::new(Mutex::new(TileCell {
                    is_empty: tile.data.iter().all(|&p| p == Color32::TRANSPARENT),
                    data: Some(tile.data),
                })),
            );
        }
        Self {
            name: snapshot.name,
            visible: snapshot.visible,
            opacity: snapshot.opacity.clamp(0.0, 1.0),
            locked: snapshot.locked,
            tiles: Mutex::new(tiles),
        }
    }
}

/// Main drawing surface that owns tile grids and blending rules across layers.
pub struct Canvas {
    width: usize,
    height: usize,
    tile_size: usize,
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

    pub fn layer_snapshots(&self) -> Vec<CanvasLayerSnapshot> {
        self.layers
            .iter()
            .map(|layer| {
                let mut tiles: Vec<_> = layer
                    .tiles
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .filter_map(|(&(tx, ty), cell)| {
                        let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                        if guard.is_empty {
                            return None;
                        }
                        guard
                            .data
                            .clone()
                            .map(|data| CanvasTileSnapshot { tx, ty, data })
                    })
                    .collect();
                tiles.sort_by_key(|tile| (tile.ty, tile.tx));
                CanvasLayerSnapshot {
                    name: layer.name.clone(),
                    visible: layer.visible,
                    opacity: layer.opacity,
                    locked: layer.locked,
                    tiles,
                }
            })
            .collect()
    }

    pub fn replace_layers_from_snapshots(
        &mut self,
        layers: Vec<CanvasLayerSnapshot>,
        active_layer_idx: usize,
    ) {
        self.layers = layers.into_iter().map(Layer::from_snapshot).collect();
        if self.layers.is_empty() {
            self.layers.push(Layer::new(
                "Background".to_string(),
                self.width,
                self.height,
                self.tile_size,
            ));
        }
        self.active_layer_idx = active_layer_idx.min(self.layers.len().saturating_sub(1));
    }

    /// Size of a tile edge in pixels.
    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    /// Access a specific layer's tile by index (used for compositing).
    fn layer_tile_cell(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            tiles
                .entry((tx, ty))
                .or_insert_with(|| {
                    Arc::new(Mutex::new(TileCell {
                        data: None,
                        is_empty: true,
                    }))
                })
                .clone()
        };

        {
            let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
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
    /// Guarantee a tile exists on the specified layer.
    pub fn ensure_layer_tile_exists(&self, layer_idx: usize, tx: usize, ty: usize) {
        let _ = self.ensure_layer_tile(layer_idx, tx as i32, ty as i32);
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
    pub fn get_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Vec<Color32>> {
        let cell = self.layer_tile_cell(layer_idx, tx, ty)?;
        let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
        guard.data.clone()
    }

    /// Overwrite a tile's pixel buffer for a given layer.
    pub fn set_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32, data: Vec<Color32>) {
        // Ensure tile exists
        if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
            let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
            let is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
            guard.is_empty = is_empty;
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
        if w == 0 || h == 0 {
            out.size = [0, 0];
            out.pixels.clear();
            return;
        }

        let step = step.max(1);
        let dst_w = w.div_ceil(step);
        let dst_h = h.div_ceil(step);

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        // Optimization: Check if the region is within a single tile
        let start_tx = x / self.tile_size;
        let start_ty = y / self.tile_size;
        let end_tx = (x.saturating_add(w).saturating_sub(1)) / self.tile_size;
        let end_ty = (y.saturating_add(h).saturating_sub(1)) / self.tile_size;

        if start_tx == end_tx
            && start_ty == end_ty
            && step == 1
            && self.try_write_single_tile_fast(
                start_tx as i32,
                start_ty as i32,
                x..x + w,
                y..y + h,
                out,
            )
        {
            return;
        }

        if start_tx == end_tx && start_ty == end_ty {
            // Fast path: Single tile access
            let tx = start_tx as i32;
            let ty = start_ty as i32;

            // 1. Get Arcs (Locking the map briefly)
            let layer_arcs: Vec<Option<Arc<Mutex<TileCell>>>> = self
                .layers
                .iter()
                .map(|layer| {
                    let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
                    tiles.get(&(tx, ty)).cloned()
                })
                .collect();

            // 2. Lock the Tiles (Holding locks for the render duration)
            let layer_guards: Vec<Option<std::sync::MutexGuard<'_, TileCell>>> = layer_arcs
                .iter()
                .map(|opt| {
                    opt.as_ref()
                        .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()))
                })
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
            let layer_props: Vec<(bool, f32, usize, bool, bool)> = layer_guards
                .iter()
                .enumerate()
                .map(|(i, opt_guard)| {
                    let is_visible = self.layers[i].visible && self.layers[i].opacity > 0.0;
                    let is_empty = opt_guard.as_ref().map_or(i != 0, |g| g.is_empty);
                    (is_visible, self.layers[i].opacity, i, i == 0, is_empty)
                })
                .collect();

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

                            for (i, (visible, opacity, _, is_bg, is_empty)) in
                                layer_props.iter().enumerate()
                            {
                                if !visible || *is_empty {
                                    continue;
                                }

                                // Get pixel in linear space (already converted)
                                let src = if let Some(linear_data) = &linear_tiles[i] {
                                    linear_data[src_idx]
                                } else if *is_bg {
                                    clear_color_linear
                                } else {
                                    Rgba::TRANSPARENT
                                };

                                if src.a() == 0.0 {
                                    continue;
                                }

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

                                    // Calculate the color for this sub-pixel using Linear Math
                                    let mut sub_composite =
                                        Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                                    for (i, (visible, opacity, _, is_bg, is_empty)) in
                                        layer_props.iter().enumerate()
                                    {
                                        if !visible || *is_empty {
                                            continue;
                                        }

                                        // Get pixel in linear space (already converted)
                                        let src = if let Some(linear_data) = &linear_tiles[i] {
                                            linear_data[src_idx]
                                        } else if *is_bg {
                                            clear_color_linear
                                        } else {
                                            Rgba::TRANSPARENT
                                        };

                                        if src.a() == 0.0 {
                                            continue;
                                        }

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
                                out.pixels[row_start + dst_x] =
                                    rgba_to_color32_fast(Rgba::from_rgba_premultiplied(
                                        r_acc * inv,
                                        g_acc * inv,
                                        b_acc * inv,
                                        a_acc * inv,
                                    ));
                            }
                        }
                    }
                }
            }
            return;
        }

        // --- FALLBACK (Multi-tile / Optimized Path) ---
        // Optimization: Cache tiles and pre-convert to linear space
        for dst_y in 0..dst_h {
            let global_y = y + dst_y * step;
            let ty = (global_y / self.tile_size) as i32;
            let local_y = global_y % self.tile_size;

            // Cache tile Arc and converted linear data for this row
            // Tuple: (cached_tx, tile_arc, linear_tile_data, is_empty)
            let mut row_tile_cache: RowTileCache = Vec::with_capacity(self.layers.len());

            // Initialize cache with None for each layer
            for _ in 0..self.layers.len() {
                row_tile_cache.push(None);
            }

            let mut dst_x = 0;
            while dst_x < dst_w {
                let global_x = x + dst_x * step;
                let tx = (global_x / self.tile_size) as i32;
                let local_x = global_x % self.tile_size;

                let dst_start = dst_y * dst_w + dst_x;

                let mut composite = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, 0.0);

                for (layer_idx, layer) in self.layers.iter().enumerate() {
                    if !layer.visible || layer.opacity <= 0.0 {
                        continue;
                    }

                    // Check if we need to fetch a different tile
                    let needs_lookup = row_tile_cache[layer_idx]
                        .as_ref()
                        .is_none_or(|(cached_tx, _, _, _)| *cached_tx != tx);

                    if needs_lookup {
                        // Drop old cache entry
                        row_tile_cache[layer_idx] = None;

                        // Fetch new tile and pre-convert to linear
                        if let Some(tile_arc) = self.layer_tile_cell(layer_idx, tx, ty) {
                            // Lock temporarily to read data
                            let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                            let is_empty = guard.is_empty;

                            // Pre-convert entire tile to linear space for efficiency
                            let linear_data = if let Some(data) = &guard.data {
                                let mut linear_tile = Vec::with_capacity(data.len());
                                for &pixel in data.iter() {
                                    linear_tile.push(Rgba::from(pixel));
                                }
                                Some(linear_tile)
                            } else {
                                None
                            };

                            // Release lock and cache the Arc with converted data
                            drop(guard);
                            row_tile_cache[layer_idx] = Some((tx, tile_arc, linear_data, is_empty));
                        }
                    }

                    // Skip if tile is empty or missing
                    let (is_empty, linear_data) =
                        if let Some((_, _, linear_data, is_empty)) = &row_tile_cache[layer_idx] {
                            (*is_empty, linear_data.as_ref())
                        } else if layer_idx == 0 {
                            (false, None) // Background uses clear_color
                        } else {
                            continue; // Non-background layer with no tile
                        };

                    if is_empty {
                        continue;
                    }

                    // Resolve Pixel in linear space
                    let src = if let Some(linear_tile) = linear_data {
                        let src_idx = local_y * self.tile_size + local_x;
                        linear_tile[src_idx]
                    } else if layer_idx == 0 {
                        Rgba::from(self.clear_color)
                    } else {
                        Rgba::TRANSPARENT
                    };

                    if src.a() == 0.0 {
                        continue;
                    }

                    // Apply opacity and blend (already in linear space)
                    let src = if layer.opacity < 1.0 {
                        src * layer.opacity
                    } else {
                        src
                    };
                    composite = src + composite * (1.0 - src.a());
                }

                out.pixels[dst_start] = rgba_to_color32_fast(composite);
                dst_x += 1;
            }
        }
    }

    pub fn write_thumbnail_nearest(&self, max_edge: usize, out: &mut ColorImage) {
        let max_edge = max_edge.max(1);
        let longest = self.width.max(self.height).max(1);
        let scale = max_edge as f32 / longest as f32;
        let dst_w = ((self.width as f32 * scale).round() as usize).max(1);
        let dst_h = ((self.height as f32 * scale).round() as usize).max(1);

        if out.size != [dst_w, dst_h] {
            out.size = [dst_w, dst_h];
            out.pixels.resize(dst_w * dst_h, Color32::TRANSPARENT);
        }

        let mut pixel = ColorImage::new([1, 1], Color32::TRANSPARENT);
        for y in 0..dst_h {
            let src_y = (y * self.height / dst_h).min(self.height.saturating_sub(1));
            for x in 0..dst_w {
                let src_x = (x * self.width / dst_w).min(self.width.saturating_sub(1));
                self.write_region_to_color_image(src_x, src_y, 1, 1, &mut pixel, 1);
                out.pixels[y * dst_w + x] = pixel.pixels[0];
            }
        }
    }

    fn try_write_single_tile_fast(
        &self,
        tx: i32,
        ty: i32,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
        out: &mut ColorImage,
    ) -> bool {
        let mut bg_visible = false;
        let mut paint_layer = None;
        for (idx, layer) in self.layers.iter().enumerate() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            if idx == 0 {
                bg_visible = true;
            } else if paint_layer.is_none() && layer.opacity >= 1.0 {
                paint_layer = Some(idx);
            } else {
                return false;
            }
        }

        let bg_arc = bg_visible
            .then(|| self.layer_tile_cell(0, tx, ty))
            .flatten();
        let bg_guard = bg_arc
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let bg_data = bg_guard.as_ref().and_then(|guard| guard.data.as_ref());

        let paint_arc = paint_layer.and_then(|idx| self.layer_tile_cell(idx, tx, ty));
        let paint_guard = paint_arc
            .as_ref()
            .map(|arc| arc.lock().unwrap_or_else(|e| e.into_inner()));
        let paint_data = paint_guard
            .as_ref()
            .filter(|guard| !guard.is_empty)
            .and_then(|guard| guard.data.as_ref());

        let w = x_range.len();
        let local_x = x_range.start % self.tile_size;
        let local_y = y_range.start % self.tile_size;
        for row in 0..y_range.len() {
            for col in 0..w {
                let src_idx = (local_y + row) * self.tile_size + local_x + col;
                let bg = if bg_visible {
                    bg_data.map_or(self.clear_color, |data| data[src_idx])
                } else {
                    Color32::TRANSPARENT
                };
                let pixel = match paint_data.map(|data| data[src_idx]) {
                    Some(src) if src.a() == 255 => src,
                    Some(src) if src.a() > 0 => alpha_over(src, bg),
                    _ => bg,
                };
                out.pixels[row * w + col] = pixel;
            }
        }
        true
    }

    /// Clear the active layer to the provided color (or transparent for non-background).
    pub fn clear(&mut self, color: Color) {
        self.clear_color = premultiply(color.to_color32());
        if let Some(layer) = self.layers.get(self.active_layer_idx) {
            let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                cell.data = None;
                cell.is_empty = true;
            }
        }
    }

    pub fn capture_layer_pixels(&self, layer_idx: usize) -> HashMap<(i32, i32), Vec<Color32>> {
        let mut pixels = HashMap::new();
        if let Some(layer) = self.layers.get(layer_idx) {
            let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for ((tx, ty), tile_arc) in tiles.iter() {
                let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(data) = &guard.data {
                    pixels.insert((*tx, *ty), data.clone());
                }
            }
        }
        pixels
    }

    pub fn preview_transform(
        &mut self,
        layer_idx: usize,
        src_tiles: &HashMap<(i32, i32), Vec<Color32>>,
        params: TransformParams,
    ) {
        let tile_size = self.tile_size;
        if src_tiles.len().saturating_mul(tile_size * tile_size) > MAX_TRANSFORM_SOURCE_PIXELS {
            log::warn!("Skipping transform preview: source is too large");
            return;
        }
        let Some((src_bounds, _)) = source_bounds_and_tiles(src_tiles, tile_size, None) else {
            return;
        };

        let dst_tiles = transform_tiles(
            src_tiles,
            src_bounds,
            params,
            tile_size,
            self.width,
            self.height,
            None,
        );

        if let Some(layer) = self.layers.get(layer_idx) {
            let mut tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                cell.data = None;
                cell.is_empty = true;
            }
            write_transformed_tiles(&mut tiles, dst_tiles, tile_size);
        }
    }

    pub fn apply_transform(
        &mut self,
        params: TransformParams,
        selection: Option<&crate::selection::SelectionManager>,
        history: Option<&mut UndoAction>,
    ) {
        let layer_idx = self.active_layer_idx;
        let tile_size = self.tile_size;
        let src_tiles = self.capture_layer_pixels(layer_idx);

        if src_tiles.len().saturating_mul(tile_size * tile_size) > MAX_TRANSFORM_SOURCE_PIXELS {
            log::warn!("Skipping transform: source layer is too large");
            return;
        }

        let Some((src_bounds, affected_src_tiles)) =
            source_bounds_and_tiles(&src_tiles, tile_size, selection)
        else {
            return;
        };

        let dst_tiles = transform_tiles(
            &src_tiles,
            src_bounds,
            params,
            tile_size,
            self.width,
            self.height,
            selection,
        );
        let tile_size_i32 = tile_size as i32;

        if let Some(layer) = self.layers.get(layer_idx) {
            let mut tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(action) = history {
                let mut affected_tiles = affected_src_tiles.clone();
                affected_tiles.extend(dst_tiles.keys().copied());

                for (tx, ty) in affected_tiles {
                    let data = if let Some(tile_arc) = tiles.get(&(tx, ty)) {
                        let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                        guard
                            .data
                            .clone()
                            .unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_size * tile_size])
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

            for ((tx, ty), source_data) in &src_tiles {
                if !affected_src_tiles.contains(&(*tx, *ty)) {
                    continue;
                }
                if let Some(tile_arc) = tiles.get(&(*tx, *ty)) {
                    let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(data) = &mut guard.data {
                        let base_x = *tx * tile_size_i32;
                        let base_y = *ty * tile_size_i32;
                        for py in 0..tile_size {
                            for px in 0..tile_size {
                                let idx = py * tile_size + px;
                                if source_data[idx].a() == 0 {
                                    continue;
                                }

                                let gx = base_x + px as i32;
                                let gy = base_y + py as i32;
                                if selection
                                    .is_none_or(|sel| sel.contains_coords(gx as f32, gy as f32))
                                {
                                    data[idx] = Color32::TRANSPARENT;
                                }
                            }
                        }
                        guard.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                    }
                }
            }

            write_transformed_tiles(&mut tiles, dst_tiles, tile_size);
        }
    }

    pub fn get_content_bounds(
        &self,
        layer_idx: usize,
        selection: Option<&crate::selection::SelectionManager>,
    ) -> Option<eframe::egui::Rect> {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        let mut found = false;

        if let Some(layer) = self.layers.get(layer_idx) {
            let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for ((tx, ty), tile_arc) in tiles.iter() {
                let guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(data) = &guard.data {
                    for py in 0..self.tile_size {
                        for px in 0..self.tile_size {
                            let idx = py * self.tile_size + px;
                            if data[idx].a() > 0 {
                                let gx = *tx * self.tile_size as i32 + px as i32;
                                let gy = *ty * self.tile_size as i32 + py as i32;

                                if let Some(sel) = selection
                                    && !sel.contains_coords(gx as f32, gy as f32)
                                {
                                    continue;
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
        let new_layer = Layer::new(
            "Floating Selection".to_string(),
            self.width,
            self.height,
            self.tile_size,
        );

        let active_layer = &self.layers[active_idx];
        let active_tiles_map = active_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());

        let mut tiles_to_process = Vec::new();
        for (&(tx, ty), tile_arc) in active_tiles_map.iter() {
            tiles_to_process.push(((tx, ty), tile_arc.clone()));
        }
        drop(active_tiles_map);

        let mut new_layer_tiles = new_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());

        for ((tx, ty), tile_arc) in tiles_to_process {
            let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
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
                    let new_tile = Arc::new(Mutex::new(TileCell {
                        data: Some(new_tile_data),
                        is_empty: false,
                    }));
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

            let top_tiles = top_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut bottom_tiles = bottom_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut src_with_opacity = Vec::new();
            let mut blended = Vec::new();

            for ((tx, ty), top_tile_arc) in top_tiles.iter() {
                let top_guard = top_tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(top_data) = &top_guard.data {
                    // Skip empty top tiles
                    if top_guard.is_empty {
                        continue;
                    }

                    // Ensure bottom tile exists
                    let bottom_tile_arc = bottom_tiles.entry((*tx, *ty)).or_insert_with(|| {
                        Arc::new(Mutex::new(TileCell {
                            data: None,
                            is_empty: true,
                        }))
                    });

                    let mut bottom_guard =
                        bottom_tile_arc.lock().unwrap_or_else(|e| e.into_inner());

                    // Initialize bottom data if missing
                    if bottom_guard.data.is_none() {
                        bottom_guard.data =
                            Some(vec![Color32::TRANSPARENT; self.tile_size * self.tile_size]);
                    }

                    if let Some(bottom_data) = &mut bottom_guard.data {
                        // Use SIMD batch processing for better performance
                        let tile_len = bottom_data.len();

                        // Apply opacity to source pixels and prepare for batch blend
                        src_with_opacity.resize(tile_len, Color32::TRANSPARENT);
                        for i in 0..tile_len {
                            src_with_opacity[i] =
                                apply_opacity_scale(top_data[i], top_layer.opacity);
                        }

                        // Create temporary output buffer
                        blended.resize(tile_len, Color32::TRANSPARENT);

                        // Batch blend using SIMD
                        alpha_over_batch(&src_with_opacity, bottom_data, &mut blended);

                        // Copy result back
                        bottom_data.copy_from_slice(&blended);

                        // Update is_empty flag
                        bottom_guard.is_empty =
                            bottom_data.iter().all(|&p| p == Color32::TRANSPARENT);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_sized_region_clears_output() {
        let canvas = Canvas::new(8, 8, Color32::WHITE, 4);
        let mut image = ColorImage::new([2, 2], Color32::BLACK);

        canvas.write_region_to_color_image(0, 0, 0, 8, &mut image, 1);

        assert_eq!(image.size, [0, 0]);
        assert!(image.pixels.is_empty());
    }

    #[test]
    fn transform_output_is_clipped_to_canvas() {
        let mut canvas = Canvas::new(8, 8, Color32::WHITE, 4);
        let mut data = vec![Color32::TRANSPARENT; 16];
        data[0] = Color32::BLACK;
        canvas.set_layer_tile_data(1, 0, 0, data);

        canvas.apply_transform(
            TransformParams::new(Vec2::new(-20.0, -20.0), 0.0, Vec2::splat(1.0), Vec2::ZERO),
            None,
            None,
        );

        let tiles = canvas.layers[1]
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(tiles.keys().all(|(tx, ty)| *tx >= 0 && *ty >= 0));
    }
}
