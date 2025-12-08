use crate::{brush_engine::{brush_options::{BlendMode, PixelBrushShape}, hardness::SoftnessSelector}, canvas::{
    canvas::{Canvas, alpha_over, blend_erase},
    history::{TileSnapshot, UndoAction},
}, selection::SelectionManager};
use crate::utils::vector::Vec2;
use eframe::egui::Color32;
use rayon::ThreadPool;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::collections::HashSet;
use super::brush_options::BrushOptions;

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
            if modified_tiles.contains(&(region.tx, region.ty)) {
                continue;
            }

            canvas.ensure_layer_tile_exists(layer_idx, region.tx, region.ty);

            if let Some(mut tile) = canvas.lock_layer_tile(layer_idx, region.tx, region.ty) {
                let data = tile.data.as_mut().unwrap();

                // Snapshot the ENTIRE tile to avoid artifacts if we draw on other parts of it later
                let patch = data.clone();

                undo_action.tiles.push(TileSnapshot {
                    tx: region.tx,
                    ty: region.ty,
                    layer_idx,
                    x0: 0,
                    y0: 0,
                    width: tile_size,
                    height: tile_size,
                    data: patch,
                });
                modified_tiles.insert((region.tx, region.ty));
            }
        }
    }

    /// Render a hard, pixel-aligned dab.
    fn pixel_dab(
        &mut self,
        _pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let r_ceil = r.ceil() as i32;

        let min_x = (center.x.floor() as i32) - r_ceil;
        let max_x = (center.x.floor() as i32) + r_ceil;
        let min_y = (center.y.floor() as i32) - r_ceil;
        let max_y = (center.y.floor() as i32) + r_ceil;

        let tile_size = canvas.tile_size();
        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;

        if max_x < 0 || max_y < 0 || min_x >= canvas_w || min_y >= canvas_h {
            return;
        }

        let start_x = min_x.max(0) as usize;
        let start_y = min_y.max(0) as usize;
        let end_x = max_x.min(canvas_w - 1) as usize;
        let end_y = max_y.min(canvas_h - 1) as usize;

        if start_x > end_x || start_y > end_y {
            return;
        }

        let min_tx = start_x / tile_size;
        let max_tx = end_x / tile_size;
        let min_ty = start_y / tile_size;
        let max_ty = end_y / tile_size;

        let tiles: Vec<(usize, usize)> = (min_ty..=max_ty)
            .flat_map(|ty| (min_tx..=max_tx).map(move |tx| (tx, ty)))
            .collect();

        let mut regions = Vec::with_capacity(tiles.len());
        for (tx, ty) in &tiles {
            let tile_x0 = tx * tile_size;
            let tile_y0 = ty * tile_size;
            let overlap_min_x = start_x.max(tile_x0);
            let overlap_max_x = end_x.min(tile_x0 + tile_size - 1);
            let overlap_min_y = start_y.max(tile_y0);
            let overlap_max_y = end_y.min(tile_y0 + tile_size - 1);
            regions.push(TileRegion {
                tx: *tx,
                ty: *ty,
                x0: overlap_min_x - tile_x0,
                y0: overlap_min_y - tile_y0,
                width: overlap_max_x - overlap_min_x + 1,
                height: overlap_max_y - overlap_min_y + 1,
            });
        }

        self.snapshot_tiles(canvas, &regions, undo_action, modified_tiles);

        let src_base = self.brush_options.color;
        let src_alpha =
            (self.brush_options.color.a() as f32 * self.brush_options.opacity * (self.brush_options.flow / 100.0)).clamp(0.0, 1.0);
        
        // Pre-compute common shape data
        let r_sq = r * r;
        let custom_data_ref = match &self.brush_options.pixel_shape {
            PixelBrushShape::Custom { width, height, data } => Some((width, height, data)),
            _ => None,
        };

        // Serial execution for pixel dab
        for (tx, ty) in tiles {
            if let Some(mut tile) = canvas.lock_tile(tx, ty) {
                let data = match tile.data.as_mut() {
                    Some(d) => d,
                    None => continue,
                };

                let tile_x0 = tx * tile_size;
                let tile_y0 = ty * tile_size;
                let overlap_min_x = start_x.max(tile_x0);
                let overlap_max_x = end_x.min(tile_x0 + tile_size - 1);
                let overlap_min_y = start_y.max(tile_y0);
                let overlap_max_y = end_y.min(tile_y0 + tile_size - 1);

                for gy in overlap_min_y..=overlap_max_y {
                    let dy = gy as f32 + 0.5 - center.y;
                    for gx in overlap_min_x..=overlap_max_x {
                        let dx = gx as f32 + 0.5 - center.x;

                        if let Some(sel) = selection {
                            if !sel.contains(Vec2 { x: gx as f32 + 0.5, y: gy as f32 + 0.5 }) {
                                continue;
                            }
                        }

                        let (in_shape, alpha_mod) = match self.brush_options.pixel_shape {
                            PixelBrushShape::Circle => (dx * dx + dy * dy <= r_sq, 1.0),
                            PixelBrushShape::Square => (dx.abs() <= r && dy.abs() <= r, 1.0),
                            PixelBrushShape::Custom { .. } => {
                                if let Some((w, h, mask)) = custom_data_ref {
                                    // Nearest neighbor sampling of the custom mask
                                    // Map (dx, dy) from [-r, r] to [0, w] and [0, h]
                                    // Normalized coords 0..1
                                    let nx = (dx + r) / self.brush_options.diameter;
                                    let ny = (dy + r) / self.brush_options.diameter;
                                    
                                    if nx >= 0.0 && nx < 1.0 && ny >= 0.0 && ny < 1.0 {
                                        let ix = (nx * *w as f32).floor() as usize;
                                        let iy = (ny * *h as f32).floor() as usize;
                                        let idx = iy * w + ix;
                                        if idx < mask.len() {
                                            let val = mask[idx];
                                            (val > 0, val as f32 / 255.0)
                                        } else {
                                            (false, 0.0)
                                        }
                                    } else {
                                        (false, 0.0)
                                    }
                                } else {
                                    (false, 0.0)
                                }
                            }
                        };

                        if in_shape {
                            let local_y = gy - tile_y0;
                            let local_x = gx - tile_x0;
                            let idx = local_y * tile_size + local_x;

                            let dst = data[idx];
                            
                            // Combine base alpha with shape alpha (if any)
                            let final_alpha = src_alpha * alpha_mod;
                            
                            let src_color = Color32::from_rgba_unmultiplied(
                                src_base.r(),
                                src_base.g(),
                                src_base.b(),
                                (final_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                            );

                            let blended = match self.brush_options.blend_mode {
                                BlendMode::Normal => alpha_over(src_color, dst),
                                BlendMode::Eraser => blend_erase(src_color, dst),
                            };
                            data[idx] = blended;
                        }
                    }
                }
            }
        }
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
        let r_ceil = r.ceil() as i32;

        let min_x = (center.x.floor() as i32) - r_ceil;
        let max_x = (center.x.floor() as i32) + r_ceil;
        let min_y = (center.y.floor() as i32) - r_ceil;
        let max_y = (center.y.floor() as i32) + r_ceil;

        let tile_size = canvas.tile_size();
        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;

        if max_x < 0 || max_y < 0 || min_x >= canvas_w || min_y >= canvas_h {
            return;
        }

        let start_x = min_x.max(0) as usize;
        let start_y = min_y.max(0) as usize;
        let end_x = max_x.min(canvas_w - 1) as usize;
        let end_y = max_y.min(canvas_h - 1) as usize;

        if start_x > end_x || start_y > end_y {
            return;
        }

        let min_tx = start_x / tile_size;
        let max_tx = end_x / tile_size;
        let min_ty = start_y / tile_size;
        let max_ty = end_y / tile_size;

        let tiles: Vec<(usize, usize)> = (min_ty..=max_ty)
            .flat_map(|ty| (min_tx..=max_tx).map(move |tx| (tx, ty)))
            .collect();

        let mut regions = Vec::with_capacity(tiles.len());
        for (tx, ty) in &tiles {
            let tile_x0 = tx * tile_size;
            let tile_y0 = ty * tile_size;
            let overlap_min_x = start_x.max(tile_x0);
            let overlap_max_x = end_x.min(tile_x0 + tile_size - 1);
            let overlap_min_y = start_y.max(tile_y0);
            let overlap_max_y = end_y.min(tile_y0 + tile_size - 1);
            regions.push(TileRegion {
                tx: *tx,
                ty: *ty,
                x0: overlap_min_x - tile_x0,
                y0: overlap_min_y - tile_y0,
                width: overlap_max_x - overlap_min_x + 1,
                height: overlap_max_y - overlap_min_y + 1,
            });
        }

        self.snapshot_tiles(canvas, &regions, undo_action, modified_tiles);

        let base_color = self.brush_options.color;
        let (sr, sg, sb) = if base_color.a() == 0 {
            (0, 0, 0)
        } else {
            let a = base_color.a() as f32;
            (
                (base_color.r() as f32 * 255.0 / a).round().clamp(0.0, 255.0) as u8,
                (base_color.g() as f32 * 255.0 / a).round().clamp(0.0, 255.0) as u8,
                (base_color.b() as f32 * 255.0 / a).round().clamp(0.0, 255.0) as u8,
            )
        };
        let flow_alpha = self.brush_options.opacity * (self.brush_options.flow / 100.0);
        let blend_mode = self.brush_options.blend_mode;
        let anti_aliasing = self.anti_aliasing;
        let hardness_val = (self.brush_options.hardness / 100.0).clamp(0.0, 0.999);
        let softness_selector = self.brush_options.softness_selector;
        let softness_curve = self.brush_options.softness_curve.clone();
        let pixel_shape = self.brush_options.pixel_shape.clone(); // Clone for use in closure

        let center_x = center.x;
        let center_y = center.y;
        let start_x = start_x;
        let start_y = start_y;
        let end_x = end_x;
        let end_y = end_y;

        let fade_start = (r - 1.0).max(0.0);
        let fade_width = r - fade_start;

        // Helper to get base alpha factor for a given point (dx, dy) and radius r
        let get_base_alpha = |dx: f32, dy: f32, radius: f32, shape: &PixelBrushShape| -> f32 {
            match shape {
                PixelBrushShape::Circle => {
                    let dist = (dx * dx + dy * dy).sqrt();
                    let t = dist / radius;
                    if dist >= radius {
                        0.0
                    } else {
                        match softness_selector {
                            SoftnessSelector::Gaussian => {
                                if t < hardness_val {
                                    1.0
                                } else {
                                    let v = (t - hardness_val) / (1.0 - hardness_val);
                                    let falloff = 1.0 - v.clamp(0.0, 1.0);
                                    let f2 = falloff * falloff;
                                    f2 * (3.0 - 2.0 * falloff)
                                }
                            }
                            SoftnessSelector::Curve => softness_curve.eval(t),
                        }
                    }
                }
                PixelBrushShape::Square => {
                    let dist_x = dx.abs();
                    let dist_y = dy.abs();
                    let dist = dist_x.max(dist_y);
                    let t = dist / radius;
                    if dist >= radius {
                        0.0
                    } else {
                        match softness_selector {
                            SoftnessSelector::Gaussian => {
                                if t < hardness_val {
                                    1.0
                                } else {
                                    let v = (t - hardness_val) / (1.0 - hardness_val);
                                    let falloff = 1.0 - v.clamp(0.0, 1.0);
                                    let f2 = falloff * falloff;
                                    f2 * (3.0 - 2.0 * falloff)
                                }
                            }
                            SoftnessSelector::Curve => softness_curve.eval(t),
                        }
                    }
                }
                PixelBrushShape::Custom { width, height, data } => {
                    // Map (dx, dy) from [-r, r] to [0, w] and [0, h]
                    let nx = (dx + radius) / (radius * 2.0); // Normalized x in 0..1
                    let ny = (dy + radius) / (radius * 2.0); // Normalized y in 0..1

                    if nx >= 0.0 && nx < 1.0 && ny >= 0.0 && ny < 1.0 {
                        let w_f32 = *width as f32;
                        let h_f32 = *height as f32;

                        let tx = nx * w_f32;
                        let ty = ny * h_f32;

                        let x0 = tx.floor() as usize;
                        let y0 = ty.floor() as usize;
                        let x1 = (x0 + 1).min(width - 1);
                        let y1 = (y0 + 1).min(height - 1);

                        let fx = tx - x0 as f32;
                        let fy = ty - y0 as f32;
                        
                        let get_pixel = |x, y| -> f32 {
                            if x < *width && y < *height {
                                data[y * width + x] as f32 / 255.0
                            } else {
                                0.0
                            }
                        };

                        // Bilinear interpolation
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
            }
        };

        // Pre-calculate alpha at the fade start boundary
        let _alpha_at_fade_start = get_base_alpha(fade_start, 0.0, r, &pixel_shape);

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

                if let Some(mut tile) = canvas.lock_tile(*tx, *ty) {
                    let data = match tile.data.as_mut() {
                        Some(d) => d,
                        None => return,
                    };

                    let overlap_min_x = start_x.max(tile_x0);
                    let overlap_max_x = end_x.min(tile_x0 + tile_size - 1);
                    let overlap_min_y = start_y.max(tile_y0);
                    let overlap_max_y = end_y.min(tile_y0 + tile_size - 1);

                    for gy in overlap_min_y..=overlap_max_y {
                        for gx in overlap_min_x..=overlap_max_x {
                            let pdx = gx as f32 + 0.5 - center_x;
                            let pdy = gy as f32 + 0.5 - center_y;

                            if let Some(sel) = selection {
                                if !sel.contains(Vec2 { x: gx as f32 + 0.5, y: gy as f32 + 0.5 }) {
                                    continue;
                                }
                            }
                            
                            let alpha_factor = if anti_aliasing {
                                // Anti-aliased path (smooth, uses get_base_alpha and AA fade)
                                let base_alpha_at_pixel = get_base_alpha(pdx, pdy, r, &pixel_shape);
                                
                                if base_alpha_at_pixel <= 0.0 { // Early exit if inner shape is transparent
                                    0.0
                                } else {
                                    // Apply the 1.5 pixel outer fade to the base alpha
                                    let dist_for_aa = match pixel_shape { // Distance metric for the AA fade
                                        PixelBrushShape::Circle => (pdx * pdx + pdy * pdy).sqrt(),
                                        PixelBrushShape::Square => pdx.abs().max(pdy.abs()),
                                        PixelBrushShape::Custom { .. } => pdx.abs().max(pdy.abs()), // Use square for AA distance for custom
                                    };
                                    
                                    if dist_for_aa >= r { // Beyond brush radius, fully transparent
                                        0.0
                                    } else if dist_for_aa > fade_start { // Within AA fade zone
                                        let fraction = (dist_for_aa - fade_start) / fade_width;
                                        base_alpha_at_pixel * (1.0 - fraction) // Blend base alpha with fade
                                    } else { // Solid interior
                                        base_alpha_at_pixel
                                    }
                                }
                            } else {
                                // Non-anti-aliased path (hard edges)
                                let (mut in_shape, mut alpha_mod) = (false, 0.0);
                                match &pixel_shape {
                                    PixelBrushShape::Circle => {
                                        in_shape = (pdx * pdx + pdy * pdy) <= r_sq;
                                        alpha_mod = 1.0;
                                    },
                                    PixelBrushShape::Square => {
                                        in_shape = pdx.abs() <= r && pdy.abs() <= r;
                                        alpha_mod = 1.0;
                                    },
                                    PixelBrushShape::Custom { width, height, data } => {
                                        // Nearest neighbor sampling of the custom mask (no AA)
                                        let nx = (pdx + r) / self.brush_options.diameter;
                                        let ny = (pdy + r) / self.brush_options.diameter;
                                        
                                        if nx >= 0.0 && nx < 1.0 && ny >= 0.0 && ny < 1.0 {
                                            let ix = (nx * *width as f32).floor() as usize;
                                            let iy = (ny * *height as f32).floor() as usize;
                                            let idx = iy * width + ix;
                                            if idx < data.len() {
                                                let val = data[idx];
                                                in_shape = val > 0;
                                                alpha_mod = val as f32 / 255.0;
                                            }
                                        }
                                    },
                                };
                                if in_shape { alpha_mod } else { 0.0 }
                            };

                            if alpha_factor <= 0.0 {
                                continue;
                            }

                            let src_a =
                                ((base_color.a() as f32 / 255.0) * flow_alpha * alpha_factor)
                                    .clamp(0.0, 1.0);
                            if src_a <= 0.0 {
                                continue;
                            }
                            let src = Color32::from_rgba_unmultiplied(
                                sr,
                                sg,
                                sb,
                                (src_a * 255.0).round().clamp(0.0, 255.0) as u8,
                            );

                            let local_y = gy - tile_y0;
                            let local_x = gx - tile_x0;
                            let idx = local_y * tile_size + local_x;

                            let dst = data[idx];
                            let blended = match blend_mode {
                                BlendMode::Normal => alpha_over(src, dst),
                                BlendMode::Eraser => blend_erase(src, dst),
                            };
                            data[idx] = blended;
                        }
                    }
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
