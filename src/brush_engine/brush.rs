use crate::canvas::{
    canvas::{Canvas, alpha_over, blend_erase},
    history::{TileSnapshot, UndoAction},
};
use crate::utils::{profiler::ScopeTimer, vector::Vec2};
use eframe::egui::Color32;
use rand::Rng;
use rayon::ThreadPool;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::collections::HashSet;

/// Available shapes for how a brush applies paint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BrushType {
    Soft,
    Pixel,
}

/// Blending strategy for how source color affects the destination.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Eraser,
}

/// Option for how the brush softness falloff is calculated.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SoftnessSelector {
    Gaussian,
    Curve,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

impl CurvePoint {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoftnessCurve {
    pub points: Vec<CurvePoint>,
}

impl Default for SoftnessCurve {
    fn default() -> Self {
        Self {
            points: vec![
                CurvePoint::new(0.0, 1.0),
                CurvePoint::new(1.0, 0.0),
            ],
        }
    }
}

impl SoftnessCurve {
    pub fn eval(&self, t: f32) -> f32 {
        if self.points.is_empty() {
            return 0.0;
        }
        // Clamp t to 0..1 just in case
        let t = t.clamp(0.0, 1.0);

        let len = self.points.len();
        if len == 1 {
            return self.points[0].y;
        }

        if t <= self.points[0].x {
            return self.points[0].y;
        }
        if t >= self.points[len - 1].x {
            return self.points[len - 1].y;
        }

        // Find the segment i such that points[i].x <= t <= points[i+1].x
        let mut i = 0;
        // Since points are sorted and N is small, linear scan is fine.
        // If N grows large, use binary search.
        for idx in 0..len - 1 {
            if t >= self.points[idx].x && t <= self.points[idx + 1].x {
                i = idx;
                break;
            }
        }

        // Monotone Cubic Hermite Interpolation
        // p0 = points[i], p1 = points[i+1]
        let p0 = &self.points[i];
        let p1 = &self.points[i + 1];

        let dx = p1.x - p0.x;
        if dx.abs() < 1e-6 {
            return p0.y;
        }

        // Calculate slopes (tangents)
        // m0 = slope at p0, m1 = slope at p1
        // Secants
        let secant0 = if i > 0 {
            let pm1 = &self.points[i - 1];
            (p0.y - pm1.y) / (p0.x - pm1.x)
        } else {
            (p1.y - p0.y) / dx // One-sided difference for start
        };

        let secant1 = (p1.y - p0.y) / dx;

        let secant2 = if i < len - 2 {
            let pp2 = &self.points[i + 2];
            (pp2.y - p1.y) / (pp2.x - p1.x)
        } else {
            secant1 // One-sided difference for end
        };

        // Tangents (using simple finite difference or centripetal)
        // Standard Monotone checks:
        // If secant k-1 and secant k have different signs, tangent is 0.
        // Else, tangent is arithmetic mean (simple) or harmonic mean (Fritsch-Butland).
        // Here we use a simple average of secants for smoothness, but clamped for monotonicity if needed.
        // For a general smooth curve (like Krita), Catmull-Rom is often better than strictly Monotone which can look "stiff".
        // But Monotone is safer for 0..1 range. Let's use Catmull-Rom style tangents (0.5 * (p[i+1]-p[i-1]))
        // but adapted for non-uniform spacing.

        let tangent = |k: usize, sec_prev: f32, sec_next: f32| -> f32 {
             if sec_prev * sec_next <= 0.0 {
                 // Local extrema, flat tangent for strict monotonicity
                 // But for "smooth" feel, maybe not?
                 // Let's try to be smooth.
                 0.0 
             } else {
                 // Harmonic mean is good for monotonicity
                 // 3.0 * sec_prev * sec_next / (sec_next + 2.0 * sec_prev) ... etc
                 // Let's just use average for simplicity and standard spline look
                 (sec_prev + sec_next) * 0.5
             }
        };
        
        // Re-calculating secants properly for the endpoints logic
        let m0 = if i == 0 {
             secant1 // Start point
        } else {
             (secant0 + secant1) * 0.5
        };
        
        let m1 = if i == len - 2 {
             secant1 // End point
        } else {
             (secant1 + secant2) * 0.5
        };

        // Evaluate cubic hermite
        let t_local = (t - p0.x) / dx;
        let t2 = t_local * t_local;
        let t3 = t2 * t_local;

        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t_local;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;

        p0.y * h00 + m0 * dx * h10 + p1.y * h01 + m1 * dx * h11
    }
}

/// Rectangular region inside a tile that needs to be touched by a dab.
#[derive(Clone, Copy, Debug)]
struct TileRegion {
    tx: usize,
    ty: usize,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
}

/// Cached soft mask used to avoid rebuilding the kernel for every dab.
#[derive(Clone, Debug)]
struct BrushMaskCache {
    diameter: f32,
    hardness: f32,
    softness_selector: SoftnessSelector,
    softness_curve: SoftnessCurve,
    size: usize,
    data: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PixelBrushShape {
    Circle,
    Square,
    Custom {
        width: usize,
        height: usize,
        data: Vec<u8>, // 0-255 mask
    },
}

/// User-facing brush configuration and scratch buffers.
#[derive(Clone, Debug)]
pub struct Brush {
    pub diameter: f32,
    pub hardness: f32, // 0..100
    pub softness_selector: SoftnessSelector,
    pub softness_curve: SoftnessCurve,
    pub pixel_shape: PixelBrushShape,
    pub color: Color32,
    pub spacing: f32, // Percentage of diameter (0..100+)
    pub flow: f32,    // 0..100
    pub opacity: f32, // 0..1
    pub blend_mode: BlendMode,
    pub is_changed: bool,

    pub brush_type: BrushType,
    pub pixel_perfect: bool,
    pub anti_aliasing: bool,
    pub jitter: f32,
    pub stabilizer: f32, // 0..1 (0 = off, 1 = max smoothing)
    mask_cache: Option<BrushMaskCache>,
}

impl Brush {
    /// Create a standard soft brush with the given radius, hardness, base color and spacing.
    pub fn new(diameter: f32, hardness: f32, color: Color32, spacing: f32) -> Self {
        Self {
            diameter,
            hardness,
            softness_selector: SoftnessSelector::Gaussian,
            softness_curve: SoftnessCurve::default(),
            pixel_shape: PixelBrushShape::Circle,
            color,
            spacing,
            flow: 100.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            brush_type: BrushType::Soft,
            pixel_perfect: false,
            anti_aliasing: true,
            jitter: 0.0,
            stabilizer: 0.0,
            mask_cache: None,
            is_changed: false,
        }
    }

    #[allow(dead_code)]
    /// Convenience constructor for a pixel-perfect pen.
    pub fn new_pixel(diameter: f32, color: Color32) -> Self {
        Self {
            diameter,
            hardness: 100.0,
            softness_selector: SoftnessSelector::Gaussian,
            softness_curve: SoftnessCurve::default(),
            pixel_shape: PixelBrushShape::Square, // Default to Square for pixel art
            color,
            spacing: 10.0,
            flow: 100.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            brush_type: BrushType::Pixel,
            pixel_perfect: true,
            anti_aliasing: false,
            jitter: 0.0,
            stabilizer: 0.0,
            mask_cache: None,
            is_changed: false,
        }
    }

    /// Paint a single dab with the currently selected brush type.
    fn dab(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        match self.brush_type {
            BrushType::Soft => self.soft_dab(pool, canvas, center, undo_action, modified_tiles),
            BrushType::Pixel => self.pixel_dab(pool, canvas, center, undo_action, modified_tiles),
        }
    }

    /// Ensure a soft brush mask exists for the current diameter/hardness and return it.
    fn ensure_mask(&mut self) -> &BrushMaskCache {
        let should_rebuild = self.is_changed || self.mask_cache.is_none();

        if should_rebuild {
            let r = self.diameter / 2.0;
            let r_sq = r * r;
            let r_ceil = r.ceil() as usize;
            let size = r_ceil * 2 + 2; // little padding for fractional centers
            let hardness = (self.hardness / 100.0).clamp(0.0, 0.999);

            let mut data = Vec::with_capacity(size * size);
            for y in 0..size {
                let dy = y as f32 + 0.5 - r;
                let dy2 = dy * dy;
                for x in 0..size {
                    let dx = x as f32 + 0.5 - r;
                    let dist2 = dx * dx + dy2;
                    if dist2 > r_sq {
                        data.push(0.0);
                        continue;
                    }
                    let dist = dist2.sqrt();
                    let t = dist / r;
                    let alpha_factor = match self.softness_selector {
                        SoftnessSelector::Gaussian => {
                            if t < hardness {
                                1.0
                            } else {
                                let v = (t - hardness) / (1.0 - hardness);
                                let falloff = 1.0 - v.clamp(0.0, 1.0);
                                let f2 = falloff * falloff;
                                f2 * (3.0 - 2.0 * falloff)
                            }
                        }
                        SoftnessSelector::Curve => self.softness_curve.eval(t),
                    };
                    data.push(alpha_factor);
                }
            }

            self.mask_cache = Some(BrushMaskCache {
                diameter: self.diameter,
                hardness: self.hardness,
                softness_selector: self.softness_selector,
                softness_curve: self.softness_curve.clone(),
                size,
                data,
            });
            self.is_changed = false;
        }

        self.mask_cache.as_ref().unwrap()
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

    /// Render a hard-edged dab for the pixel brush using a serial loop.
    fn pixel_dab(
        &self,
        _pool: &ThreadPool,
        canvas: &Canvas,
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let r = self.diameter / 2.0;
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

        let src_base = self.color;
        let src_alpha =
            (self.color.a() as f32 * self.opacity * (self.flow / 100.0)).clamp(0.0, 1.0);
        
        // Pre-compute common shape data
        let r_sq = r * r;
        let custom_data_ref = match &self.pixel_shape {
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

                        let (in_shape, alpha_mod) = match self.pixel_shape {
                            PixelBrushShape::Circle => (dx * dx + dy * dy <= r_sq, 1.0),
                            PixelBrushShape::Square => (dx.abs() <= r && dy.abs() <= r, 1.0),
                            PixelBrushShape::Custom { .. } => {
                                if let Some((w, h, mask)) = custom_data_ref {
                                    // Nearest neighbor sampling of the custom mask
                                    // Map (dx, dy) from [-r, r] to [0, w] and [0, h]
                                    // Normalized coords 0..1
                                    let nx = (dx + r) / self.diameter;
                                    let ny = (dy + r) / self.diameter;
                                    
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

                            let blended = match self.blend_mode {
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
        center: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let r = self.diameter / 2.0;
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

        let base_color = self.color;
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
        let flow_alpha = self.opacity * (self.flow / 100.0);
        let blend_mode = self.blend_mode;
        let anti_aliasing = self.anti_aliasing;
        let hardness_val = (self.hardness / 100.0).clamp(0.0, 0.999);
        let softness_selector = self.softness_selector;
        let softness_curve = self.softness_curve.clone();
        let pixel_shape = self.pixel_shape.clone(); // Clone for use in closure

        let mask = self.ensure_mask();
        let _mask_size = mask.size as isize;
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
        let alpha_at_fade_start = get_base_alpha(fade_start, 0.0, r, &pixel_shape);

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
                                        let nx = (pdx + r) / self.diameter;
                                        let ny = (pdy + r) / self.diameter;
                                        
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

/// Tracks per-stroke state like the last position and spacing accumulator.
pub struct StrokeState {
    pub last_pos: Option<Vec2>,
    dist_until_next_blit: f32,
    stroke_timer: Option<ScopeTimer>,
}

impl StrokeState {
    /// Create an empty stroke state and start the profiling timer.
    pub fn new() -> Self {
        Self {
            last_pos: None,
            dist_until_next_blit: 0.0,
            stroke_timer: Some(ScopeTimer::new("stroke")),
        }
    }

    /// Add a new sample to the stroke, interpolating dabs based on spacing and jitter.
    pub fn add_point(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        brush: &mut Brush,
        raw_pos: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        if brush.pixel_perfect {
            self.add_point_pixel_perfect(pool, canvas, brush, raw_pos, undo_action, modified_tiles);
            return;
        }

        let pos = if brush.stabilizer > 0.0 {
            if let Some(prev) = self.last_pos {
                let factor = 1.0 - (brush.stabilizer * 0.95);
                let diff = raw_pos - prev;
                prev + diff * factor
            } else {
                raw_pos
            }
        } else {
            raw_pos
        };

        let spacing_dist = (brush.spacing / 100.0) * brush.diameter;
        let spacing_dist = spacing_dist.max(0.5); // Avoid infinite loops

        if let Some(prev) = self.last_pos {
            let delta = pos - prev;
            let mut dist_left = delta.length();

            if dist_left == 0.0 {
                return;
            }

            let unit_step = delta / dist_left;
            let mut cur_pos = prev;

            while dist_left >= self.dist_until_next_blit {
                // Take a step to the next blit point.
                cur_pos = cur_pos + unit_step * self.dist_until_next_blit;
                dist_left -= self.dist_until_next_blit;

                // Blit.
                let mut p = cur_pos;
                if brush.jitter > 0.0 {
                    let mut rng = rand::rng();
                    let jitter_amount = (brush.jitter / 100.0) * brush.diameter;
                    let jx = rng.random_range(-jitter_amount..=jitter_amount);
                    let jy = rng.random_range(-jitter_amount..=jitter_amount);
                    p.x += jx;
                    p.y += jy;
                }
                brush.dab(pool, canvas, p, undo_action, modified_tiles);

                self.dist_until_next_blit = spacing_dist;
            }

            // Take the partial step to land at the sample.
            self.dist_until_next_blit -= dist_left;
        } else {
            // first point
            let mut p = pos;
            if brush.jitter > 0.0 {
                let mut rng = rand::rng();
                let jx = rng.random_range(-brush.jitter..=brush.jitter);
                let jy = rng.random_range(-brush.jitter..=brush.jitter);
                p.x += jx;
                p.y += jy;
            }
            brush.dab(pool, canvas, p, undo_action, modified_tiles);
            self.dist_until_next_blit = spacing_dist;
        }

        self.last_pos = Some(pos);
    }

    /// Pixel-perfect Bresenham line stepping to avoid gaps when snapping to pixels.
    fn add_point_pixel_perfect(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        brush: &mut Brush,
        pos: Vec2,
        undo_action: &mut UndoAction,
        modified_tiles: &mut HashSet<(usize, usize)>,
    ) {
        let x1 = pos.x.floor() as i32;
        let y1 = pos.y.floor() as i32;

        if let Some(prev) = self.last_pos {
            let x0 = prev.x.floor() as i32;
            let y0 = prev.y.floor() as i32;

            if x0 == x1 && y0 == y1 {
                return;
            }

            let dx = (x1 - x0).abs();
            let dy = -(y1 - y0).abs();
            let sx = if x0 < x1 { 1 } else { -1 };
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut err = dx + dy;

            let mut x = x0;
            let mut y = y0;

            loop {
                brush.dab(
                    pool,
                    canvas,
                    Vec2 {
                        x: x as f32 + 0.5,
                        y: y as f32 + 0.5,
                    },
                    undo_action,
                    modified_tiles,
                );

                if x == x1 && y == y1 {
                    break;
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y += sy;
                }
            }
        } else {
            brush.dab(
                pool,
                canvas,
                Vec2 {
                    x: x1 as f32 + 0.5,
                    y: y1 as f32 + 0.5,
                },
                undo_action,
                modified_tiles,
            );
        }
        self.last_pos = Some(pos);
    }

    /// Reset the stroke state and emit the profiling metric.
    pub fn end(&mut self) {
        self.last_pos = None;
        self.dist_until_next_blit = 0.0;
        // Drop the timer so stroke-level duration is reported when the stroke ends.
        self.stroke_timer.take();
    }
}

/// Named preset that can be displayed in the UI and cloned into the active brush.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub brush: Brush,
}
