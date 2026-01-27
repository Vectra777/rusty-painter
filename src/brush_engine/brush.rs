use crate::{brush_engine::{brush_options::{BlendMode, PixelBrushShape}, hardness::SoftnessSelector}, canvas::{
    canvas::{Canvas, alpha_over_batch, blend_erase},
    history::{TileSnapshot, UndoAction},
}, selection::SelectionManager};
use crate::utils::vector::Vec2;
use eframe::egui::Color32;
use rayon::ThreadPool;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::collections::HashSet;
use super::brush_options::BrushOptions;

/// Sample custom mask with nearest neighbor interpolation
#[inline]
fn sample_custom_mask_nn(dx: f32, dy: f32, diameter: f32, width: usize, height: usize, mask: &[u8]) -> (bool, f32) {
    let r = diameter / 2.0;
    let nx = (dx + r) / diameter;
    let ny = (dy + r) / diameter;
    
    if nx >= 0.0 && nx < 1.0 && ny >= 0.0 && ny < 1.0 {
        let ix = (nx * width as f32).floor() as usize;
        let iy = (ny * height as f32).floor() as usize;
        let idx = iy * width + ix;
        if idx < mask.len() {
            let val = mask[idx];
            return (val > 0, val as f32 / 255.0);
        }
    }
    (false, 0.0)
}

/// Sample custom mask with bilinear interpolation for smooth soft brushes
#[inline]
fn sample_custom_mask_bilinear(dx: f32, dy: f32, radius: f32, width: usize, height: usize, data: &[u8]) -> f32 {
    let nx = (dx + radius) / (radius * 2.0);
    let ny = (dy + radius) / (radius * 2.0);

    if nx >= 0.0 && nx < 1.0 && ny >= 0.0 && ny < 1.0 {
        let tx = nx * (width as f32);
        let ty = ny * (height as f32);

        let x0 = tx.floor() as usize;
        let y0 = ty.floor() as usize;
        let x1 = (x0 + 1).min(width - 1);
        let y1 = (y0 + 1).min(height - 1);

        let fx = tx - x0 as f32;
        let fy = ty - y0 as f32;
        
        let get_pixel = |x: usize, y: usize| -> f32 {
            if x < width && y < height {
                data[y * width + x] as f32 / 255.0
            } else {
                0.0
            }
        };

        let c00 = get_pixel(x0, y0);
        let c10 = get_pixel(x1, y0);
        let c01 = get_pixel(x0, y1);
        let c11 = get_pixel(x1, y1);

        c00 * (1.0 - fx) * (1.0 - fy) +
         c10 * fx * (1.0 - fy) +
         c01 * (1.0 - fx) * fy +
         c11 * fx * fy
    } else {
        0.0
    }
}

/// Calculate base alpha value for soft brush at given offset from center.
/// Returns (alpha, distance_squared) to avoid redundant sqrt calculations.
fn calc_soft_brush_alpha(
    dx: f32,
    dy: f32,
    radius: f32,
    shape: &PixelBrushShape,
    hardness_val: f32,
    softness_selector: SoftnessSelector,
    softness_curve: &crate::brush_engine::hardness::SoftnessCurve,
) -> (f32, f32) {
    match shape {
        PixelBrushShape::Circle => {
            let dist_sq = dx * dx + dy * dy;
            let r_sq = radius * radius;
            if dist_sq >= r_sq {
                (0.0, dist_sq)
            } else {
                let dist = dist_sq.sqrt();
                let t = dist / radius;
                let alpha = match softness_selector {
                    SoftnessSelector::Gaussian => {
                        if t < hardness_val {
                            1.0
                        } else if hardness_val >= 1.0 {
                            // 100% hardness: hard edge (outer AA fade handles smoothing)
                            1.0
                        } else {
                            let v = (t - hardness_val) / (1.0 - hardness_val);
                            let falloff = 1.0 - v.clamp(0.0, 1.0);
                            let f2 = falloff * falloff;
                            f2 * (3.0 - 2.0 * falloff)
                        }
                    }
                    SoftnessSelector::Curve => softness_curve.eval(t),
                };
                (alpha, dist_sq)
            }
        }
        PixelBrushShape::Square => {
            let dist_x = dx.abs();
            let dist_y = dy.abs();
            let dist = dist_x.max(dist_y);
            let dist_sq = dist * dist;
            let t = dist / radius;
            if dist >= radius {
                (0.0, dist_sq)
            } else {
                let alpha = match softness_selector {
                    SoftnessSelector::Gaussian => {
                        if t < hardness_val {
                            1.0
                        } else if hardness_val >= 0.999 {
                            // Very high hardness (>99.9%): hard edge (outer AA fade handles smoothing)
                            1.0
                        } else {
                            let v = (t - hardness_val) / (1.0 - hardness_val);
                            let falloff = 1.0 - v.clamp(0.0, 1.0);
                            let f2 = falloff * falloff;
                            f2 * (3.0 - 2.0 * falloff)
                        }
                    }
                    SoftnessSelector::Curve => softness_curve.eval(t),
                };
                (alpha, dist_sq)
            }
        }
        PixelBrushShape::Custom { width, height, data } => {
            let alpha = sample_custom_mask_bilinear(dx, dy, radius, *width, *height, data);
            let dist_sq = dx * dx + dy * dy;
            (alpha, dist_sq)
        }
    }
}

/// Available shapes for how a brush applies paint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BrushType {
    Soft,
    Pixel,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum StabilizerAlgorithm {
    None,
    Simple,
    Dynamic,
}

/// Rectangular region inside a tile that needs to be touched by a dab.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
struct TileRegion {
    tx: usize,
    ty: usize,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
}

/// Pixel bounds for a dab operation
#[derive(Clone, Copy, Debug)]
struct DabBounds {
    start_x: usize,
    start_y: usize,
    end_x: usize,
    end_y: usize,
    min_tx: usize,
    max_tx: usize,
    min_ty: usize,
    max_ty: usize,
}

/// User-facing brush configuration and scratch buffers.
#[derive(Clone, Debug)]
pub struct Brush {
    pub brush_options: BrushOptions,
    pub is_changed: bool,
    pub brush_type: BrushType,
    pub pixel_perfect: bool,
    pub anti_aliasing: bool,
    pub jitter: f32,
    pub stabilizer: f32, // 0..1 (0 = off, 1 = max smoothing) - Used for Simple
    pub stabilizer_algorithm: StabilizerAlgorithm,
    pub stabilizer_mass: f32, // 0.01..1.0
    pub stabilizer_drag: f32, // 0.0..1.0
}

/// Calculate pixel and tile bounds for a dab centered at the given position.
fn calc_dab_bounds(center: Vec2, radius: f32, canvas_w: i32, canvas_h: i32, tile_size: usize) -> Option<DabBounds> {
    let r_ceil = radius.ceil() as i32;
    let min_x = (center.x.floor() as i32) - r_ceil;
    let max_x = (center.x.floor() as i32) + r_ceil;
    let min_y = (center.y.floor() as i32) - r_ceil;
    let max_y = (center.y.floor() as i32) + r_ceil;
    
    if max_x < 0 || max_y < 0 || min_x >= canvas_w || min_y >= canvas_h {
        return None;
    }
    
    let start_x = min_x.max(0) as usize;
    let start_y = min_y.max(0) as usize;
    let end_x = max_x.min(canvas_w - 1) as usize;
    let end_y = max_y.min(canvas_h - 1) as usize;
    
    if start_x > end_x || start_y > end_y {
        return None;
    }
    
    let min_tx = start_x / tile_size;
    let max_tx = end_x / tile_size;
    let min_ty = start_y / tile_size;
    let max_ty = end_y / tile_size;
    
    Some(DabBounds { start_x, start_y, end_x, end_y, min_tx, max_tx, min_ty, max_ty })
}

/// Build tile regions from dab bounds.
fn build_tile_regions(bounds: &DabBounds, tile_size: usize) -> Vec<TileRegion> {
    (bounds.min_ty..=bounds.max_ty)
        .flat_map(|ty| {
            (bounds.min_tx..=bounds.max_tx).map(move |tx| {
                let tile_x0 = tx * tile_size;
                let tile_y0 = ty * tile_size;
                let overlap_min_x = bounds.start_x.max(tile_x0);
                let overlap_max_x = bounds.end_x.min(tile_x0 + tile_size - 1);
                let overlap_min_y = bounds.start_y.max(tile_y0);
                let overlap_max_y = bounds.end_y.min(tile_y0 + tile_size - 1);
                TileRegion {
                    tx,
                    ty,
                    x0: overlap_min_x - tile_x0,
                    y0: overlap_min_y - tile_y0,
                    width: overlap_max_x - overlap_min_x + 1,
                    height: overlap_max_y - overlap_min_y + 1,
                }
            })
        })
        .collect()
}

impl Brush {
    /// Create a standard soft brush with the given radius, hardness, base color and spacing.
    pub fn new(diameter: f32, hardness: f32, color: Color32, spacing: f32) -> Self {
        Self {
            brush_options: BrushOptions::new(diameter, hardness, color, spacing),
            brush_type: BrushType::Soft,
            pixel_perfect: false,
            anti_aliasing: true,
            jitter: 0.0,
            stabilizer: 0.0,
            stabilizer_algorithm: StabilizerAlgorithm::None,
            stabilizer_mass: 0.1,
            stabilizer_drag: 0.5,
            is_changed: false,
        }
    }

    #[allow(dead_code)]
    /// Convenience constructor for a pixel-perfect pen.
    pub fn new_pixel(diameter: f32, color: Color32) -> Self {
        Self {
            brush_options: BrushOptions::new(diameter, 100.0, color, 10.0),
            brush_type: BrushType::Pixel,
            pixel_perfect: true,
            anti_aliasing: false,
            jitter: 0.0,
            stabilizer: 0.0,
            stabilizer_algorithm: StabilizerAlgorithm::None,
            stabilizer_mass: 0.1,
            stabilizer_drag: 0.5,
            is_changed: false,
        }
    }

    /// Paint a single dab with the currently selected brush type.
    pub(crate) fn dab(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        match self.brush_type {
            BrushType::Soft => self.soft_dab(pool, canvas, selection, center, undo_action, modified_tiles),
            BrushType::Pixel => self.pixel_dab(pool, canvas, selection, center, undo_action, modified_tiles),
        }
    }

    /// Snapshot tiles about to be modified so undo can restore them later.
    fn snapshot_tiles(
        &self,
        canvas: &Canvas,
        regions: &[TileRegion],
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let layer_idx = canvas.active_layer_idx;
        let tile_size = canvas.tile_size();

        for region in regions {
            // insert returns false if value was already present
            if !modified_tiles.insert((region.tx, region.ty)) {
                continue;
            }

            canvas.ensure_layer_tile_exists(layer_idx, region.tx, region.ty);

            if let Some(tile_arc) = canvas.lock_layer_tile(layer_idx, region.tx, region.ty) {
                let mut tile = tile_arc.lock().unwrap();
                let data = tile.data.as_mut().unwrap();

                // Snapshot the ENTIRE tile to avoid artifacts if we draw on other parts of it later
                let patch = data.clone();

                undo_action.tiles.push(TileSnapshot {
                    tx: region.tx as i32,
                    ty: region.ty as i32,
                    layer_idx,
                    x0: 0,
                    y0: 0,
                    width: tile_size,
                    height: tile_size,
                    data: patch,
                });
            }
        }
    }

    /// Render a hard, pixel-aligned dab.
    fn pixel_dab(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let tile_size = canvas.tile_size();
        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;
        
        let Some(bounds) = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size) else { return };
        let regions = build_tile_regions(&bounds, tile_size);

        self.snapshot_tiles(canvas, &regions, undo_action, modified_tiles);

        let src_base = self.brush_options.color;
        let base_alpha = self.brush_options.color.a() as f32 * self.brush_options.opacity * (self.brush_options.flow / 100.0);
        let src_r = src_base.r();
        let src_g = src_base.g();
        let src_b = src_base.b();
        
        // Pre-compute common shape data
        let r_sq = r * r;
        let custom_data_ref = match &self.brush_options.pixel_shape {
            PixelBrushShape::Custom { width, height, data } => Some((width, height, data)),
            _ => None,
        };
        let blend_mode = self.brush_options.blend_mode;
        let center_x = center.x;
        let center_y = center.y;
        let pixel_shape = &self.brush_options.pixel_shape;
        let diameter = self.brush_options.diameter;

        // Parallel execution for pixel dab
        let tiles: Vec<(usize, usize)> = (bounds.min_ty..=bounds.max_ty)
            .flat_map(|ty| (bounds.min_tx..=bounds.max_tx).map(move |tx| (tx, ty)))
            .collect();
        
        pool.install(|| {
            tiles.par_iter().for_each(|(tx, ty)| {
                if let Some(tile_arc) = canvas.lock_tile(*tx, *ty) {
                    let mut tile = tile_arc.lock().unwrap();
                    let data = match tile.data.as_mut() {
                        Some(d) => d,
                        None => return,
                    };

                    let tile_x0 = tx * tile_size;
                    let tile_y0 = ty * tile_size;
                    let overlap_min_x = bounds.start_x.max(tile_x0);
                    let overlap_max_x = bounds.end_x.min(tile_x0 + tile_size - 1);
                    let overlap_min_y = bounds.start_y.max(tile_y0);
                    let overlap_max_y = bounds.end_y.min(tile_y0 + tile_size - 1);

                    // Pre-check if tile intersects selection bounds for early culling
                    let tile_overlaps_selection = if let Some(sel) = selection {
                        if let Some(sel_bounds) = sel.get_bounds() {
                            let tile_max_x = (tile_x0 + tile_size) as f32;
                            let tile_max_y = (tile_y0 + tile_size) as f32;
                            
                            // Check if tile AABB intersects selection AABB
                            !(tile_x0 as f32 >= sel_bounds.max.x || tile_max_x <= sel_bounds.min.x ||
                              tile_y0 as f32 >= sel_bounds.max.y || tile_max_y <= sel_bounds.min.y)
                        } else {
                            true
                        }
                    } else {
                        true
                    };
                    
                    if !tile_overlaps_selection {
                        return; // Tile completely outside selection, skip
                    }

                    // Batch pixels for SIMD blending
                    let mut src_batch = Vec::with_capacity(64);
                    let mut dst_batch = Vec::with_capacity(64);
                    let mut idx_batch = Vec::with_capacity(64);

                    for gy in overlap_min_y..=overlap_max_y {
                        let dy = gy as f32 + 0.5 - center_y;
                        let py = gy as f32 + 0.5;
                        for gx in overlap_min_x..=overlap_max_x {
                            let dx = gx as f32 + 0.5 - center_x;
                            let px = gx as f32 + 0.5;

                            if let Some(sel) = selection {
                                if !sel.contains_coords(px, py) {
                                    continue;
                                }
                            }

                            let (in_shape, alpha_mod) = match &pixel_shape {
                                PixelBrushShape::Circle => (dx * dx + dy * dy <= r_sq, 1.0),
                                PixelBrushShape::Square => (dx.abs() <= r && dy.abs() <= r, 1.0),
                                PixelBrushShape::Custom { .. } => {
                                    if let Some((w, h, mask)) = custom_data_ref {
                                        sample_custom_mask_nn(dx, dy, diameter, *w, *h, mask)
                                    } else {
                                        (false, 0.0)
                                    }
                                }
                            };

                            if in_shape {
                                let local_y = gy - tile_y0;
                                let local_x = gx - tile_x0;
                                let idx = local_y * tile_size + local_x;
                                
                                // Combine base alpha with shape alpha (if any)
                                let final_alpha = (base_alpha * alpha_mod).clamp(0.0, 1.0);
                                let alpha_u8 = (final_alpha * 255.0) as u8;
                                
                                let src_color = Color32::from_rgba_unmultiplied(src_r, src_g, src_b, alpha_u8);

                                match blend_mode {
                                    BlendMode::Normal => {
                                        // Batch for SIMD blending
                                        src_batch.push(src_color);
                                        dst_batch.push(data[idx]);
                                        idx_batch.push(idx);
                                        
                                        // Process batch when we have 4+ pixels
                                        if src_batch.len() >= 4 {
                                            let mut blended = vec![Color32::TRANSPARENT; src_batch.len()];
                                            alpha_over_batch(&src_batch, &dst_batch, &mut blended);
                                            for (i, &idx) in idx_batch.iter().enumerate() {
                                                data[idx] = blended[i];
                                            }
                                            src_batch.clear();
                                            dst_batch.clear();
                                            idx_batch.clear();
                                        }
                                    }
                                    BlendMode::Eraser => {
                                        data[idx] = blend_erase(src_color, data[idx]);
                                    }
                                }
                            }
                        }
                    }

                    // Process remaining pixels in batch (scalar fallback)
                    if !src_batch.is_empty() && blend_mode == BlendMode::Normal {
                        let mut blended = vec![Color32::TRANSPARENT; src_batch.len()];
                        alpha_over_batch(&src_batch, &dst_batch, &mut blended);
                        for (i, &idx) in idx_batch.iter().enumerate() {
                            data[idx] = blended[i];
                        }
                    }
                    // Mark tile as dirty (not empty) after modifications
                    tile.is_empty = false;
                }
            });
        });
    }

    /// Render a soft, anti-aliased dab using the cached mask and parallel tiling.
    fn soft_dab(
        &mut self,
        _pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let r_sq = r * r;
        let tile_size = canvas.tile_size();
        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;
        
        let Some(bounds) = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size) else { return };
        let regions = build_tile_regions(&bounds, tile_size);

        self.snapshot_tiles(canvas, &regions, undo_action, modified_tiles);

        // Pre-compute values outside loops
        let base_color = self.brush_options.color;
        let sr = base_color.r();
        let sg = base_color.g();
        let sb = base_color.b();
        let base_alpha = base_color.a() as f32 / 255.0;
        let flow_alpha = self.brush_options.opacity * (self.brush_options.flow / 100.0);
        let blend_mode = self.brush_options.blend_mode;
        let anti_aliasing = self.anti_aliasing;
        let hardness_val = (self.brush_options.hardness / 100.0).clamp(0.0, 1.0);
        let softness_selector = self.brush_options.softness_selector;
        let softness_curve = &self.brush_options.softness_curve;
        let pixel_shape = &self.brush_options.pixel_shape;
        let diameter = self.brush_options.diameter;

        let center_x = center.x;
        let center_y = center.y;

        // Pre-compute fade values for anti-aliasing (1.5 pixel outer fade)
        let fade_start = (r - 1.5).max(0.0);
        let fade_width = 1.5_f32.min(r);
        let inv_fade_width = if fade_width > 0.0 { 1.0 / fade_width } else { 0.0 };
        
        let tiles: Vec<(usize, usize)> = (bounds.min_ty..=bounds.max_ty)
            .flat_map(|ty| (bounds.min_tx..=bounds.max_tx).map(move |tx| (tx, ty)))
            .collect();

        _pool.install(|| {
            tiles.par_iter().for_each(|(tx, ty)| {
                let tile_x0 = tx * tile_size;
                let tile_y0 = ty * tile_size;
                let tile_x1 = tile_x0 + tile_size;
                let tile_y1 = tile_y0 + tile_size;

                // Check if tile is reasonably close to center (bounding box check)
                if center_x < (tile_x0 as f32 - r) || center_x > (tile_x1 as f32 + r) ||
                   center_y < (tile_y0 as f32 - r) || center_y > (tile_y1 as f32 + r) {
                    return;
                }

                if let Some(tile_arc) = canvas.lock_tile(*tx, *ty) {
                    let mut tile = tile_arc.lock().unwrap();
                    let data = match tile.data.as_mut() {
                        Some(d) => d,
                        None => return,
                    };

                    let overlap_min_x = bounds.start_x.max(tile_x0);
                    let overlap_max_x = bounds.end_x.min(tile_x0 + tile_size - 1);
                    let overlap_min_y = bounds.start_y.max(tile_y0);
                    let overlap_max_y = bounds.end_y.min(tile_y0 + tile_size - 1);

                    // Pre-check if tile intersects selection bounds for early culling
                    let tile_overlaps_selection = if let Some(sel) = selection {
                        if let Some(sel_bounds) = sel.get_bounds() {
                            let tile_max_x = (tile_x0 + tile_size) as f32;
                            let tile_max_y = (tile_y0 + tile_size) as f32;
                            
                            // Check if tile AABB intersects selection AABB
                            !(tile_x0 as f32 >= sel_bounds.max.x || tile_max_x <= sel_bounds.min.x ||
                              tile_y0 as f32 >= sel_bounds.max.y || tile_max_y <= sel_bounds.min.y)
                        } else {
                            true
                        }
                    } else {
                        true
                    };
                    
                    if !tile_overlaps_selection {
                        return; // Tile completely outside selection
                    }

                    // Batch pixels for SIMD blending
                    let mut src_batch = Vec::with_capacity(64);
                    let mut dst_batch = Vec::with_capacity(64);
                    let mut idx_batch = Vec::with_capacity(64);

                    for gy in overlap_min_y..=overlap_max_y {
                        let py = gy as f32 + 0.5;
                        for gx in overlap_min_x..=overlap_max_x {
                            let pdx = gx as f32 + 0.5 - center_x;
                            let pdy = py - center_y;
                            let px = gx as f32 + 0.5;

                            if let Some(sel) = selection {
                                if !sel.contains_coords(px, py) {
                                    continue;
                                }
                            }
                            
                            let alpha_factor = if anti_aliasing {
                                // Anti-aliased path (smooth, uses calc_soft_brush_alpha and AA fade)
                                let (base_alpha_at_pixel, dist_sq) = calc_soft_brush_alpha(
                                    pdx, pdy, r, &pixel_shape, hardness_val, softness_selector, softness_curve
                                );
                                
                                if base_alpha_at_pixel <= 0.0 { // Early exit if inner shape is transparent
                                    0.0
                                } else {
                                    // Apply the 1.5 pixel outer fade using pre-computed distance
                                    let dist_for_aa = match pixel_shape {
                                        PixelBrushShape::Circle => dist_sq.sqrt(),
                                        PixelBrushShape::Square => pdx.abs().max(pdy.abs()),
                                        PixelBrushShape::Custom { .. } => pdx.abs().max(pdy.abs()),
                                    };
                                    
                                    if dist_for_aa >= r { // Beyond brush radius, fully transparent
                                        0.0
                                    } else if dist_for_aa > fade_start { // Within AA fade zone
                                        let fraction = (dist_for_aa - fade_start) * inv_fade_width;
                                        base_alpha_at_pixel * (1.0 - fraction) // Blend base alpha with fade
                                    } else { // Solid interior
                                        base_alpha_at_pixel
                                    }
                                }
                            } else {
                                // Non-anti-aliased path (hard edges)
                                let (in_shape, alpha_mod) = match &pixel_shape {
                                    PixelBrushShape::Circle => {
                                        ((pdx * pdx + pdy * pdy) <= r_sq, 1.0)
                                    }
                                    PixelBrushShape::Square => {
                                        (pdx.abs() <= r && pdy.abs() <= r, 1.0)
                                    }
                                    PixelBrushShape::Custom { width, height, data } => {
                                        sample_custom_mask_nn(pdx, pdy, diameter, *width, *height, data)
                                    }
                                };
                                if in_shape { alpha_mod } else { 0.0 }
                            };

                            if alpha_factor <= 0.0 {
                                continue;
                            }

                            let src_a = (base_alpha * flow_alpha * alpha_factor).clamp(0.0, 1.0);
                            if src_a <= 0.0 {
                                continue;
                            }
                            let alpha_u8 = (src_a * 255.0) as u8;
                            let src = Color32::from_rgba_unmultiplied(sr, sg, sb, alpha_u8);

                            let local_y = gy - tile_y0;
                            let local_x = gx - tile_x0;
                            let idx = local_y * tile_size + local_x;

                            match blend_mode {
                                BlendMode::Normal => {
                                    // Batch for SIMD blending
                                    src_batch.push(src);
                                    dst_batch.push(data[idx]);
                                    idx_batch.push(idx);
                                    
                                    // Process batch when we have 4+ pixels
                                    if src_batch.len() >= 4 {
                                        let mut blended = vec![Color32::TRANSPARENT; src_batch.len()];
                                        alpha_over_batch(&src_batch, &dst_batch, &mut blended);
                                        for (i, &idx) in idx_batch.iter().enumerate() {
                                            data[idx] = blended[i];
                                        }
                                        src_batch.clear();
                                        dst_batch.clear();
                                        idx_batch.clear();
                                    }
                                }
                                BlendMode::Eraser => {
                                    data[idx] = blend_erase(src, data[idx]);
                                }
                            }
                        }
                    }

                    // Process remaining pixels in batch (scalar fallback)
                    if !src_batch.is_empty() && blend_mode == BlendMode::Normal {
                        let mut blended = vec![Color32::TRANSPARENT; src_batch.len()];
                        alpha_over_batch(&src_batch, &dst_batch, &mut blended);
                        for (i, &idx) in idx_batch.iter().enumerate() {
                            data[idx] = blended[i];
                        }
                    }
                    // Mark tile as dirty (not empty) after modifications
                    tile.is_empty = false;
                }
            });
        });
    }
}

/// Named preset that can be displayed in the UI and cloned into the active brush.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub brush: Brush,
}
