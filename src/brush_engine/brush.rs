use super::brush_options::BrushOptions;
use crate::{
    brush_engine::{
        brush_options::{BlendMode, PixelBrushShape},
        dab::{
            TileRegion, build_tile_regions, calc_dab_bounds, tile_coords, tile_overlaps_selection,
        },
        hardness::SoftnessSelector,
        masks::{calc_soft_brush_alpha, sample_custom_mask_nn},
    },
    canvas::{
        Canvas,
        history::{TileSnapshot, UndoAction},
        storage::{LinearBrushColor, alpha_over_brush, blend_erase},
    },
    selection::SelectionManager,
};
use eframe::egui::Color32;
use eframe::egui::Vec2;
use rayon::ThreadPool;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::collections::HashSet;

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

#[derive(Clone, Debug)]
struct SoftMaskCache {
    diameter_bits: u32,
    hardness_bits: u32,
    frac_x: u8,
    frac_y: u8,
    r_ceil: i32,
    size: usize,
    alpha: Vec<u8>,
    spans: Vec<MaskSpan>,
}

#[derive(Clone, Copy, Debug)]
struct MaskSpan {
    y: usize,
    x_start: usize,
    x_end: usize,
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
    soft_mask_cache: Vec<SoftMaskCache>,
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
            soft_mask_cache: Vec::new(),
        }
    }

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
            soft_mask_cache: Vec::new(),
        }
    }

    fn gaussian_circle_mask(
        &mut self,
        center: Vec2,
        r: f32,
        hardness_val: f32,
        fade_start: f32,
        inv_fade_width: f32,
    ) -> &SoftMaskCache {
        let frac_x = ((center.x - center.x.floor()) * 16.0)
            .floor()
            .clamp(0.0, 15.0) as u8;
        let frac_y = ((center.y - center.y.floor()) * 16.0)
            .floor()
            .clamp(0.0, 15.0) as u8;
        let diameter_bits = self.brush_options.diameter.to_bits();
        let hardness_bits = hardness_val.to_bits();
        let r_ceil = r.ceil() as i32;
        if let Some(index) = self.soft_mask_cache.iter().position(|cache| {
            cache.diameter_bits == diameter_bits
                && cache.hardness_bits == hardness_bits
                && cache.frac_x == frac_x
                && cache.frac_y == frac_y
        }) {
            return &self.soft_mask_cache[index];
        }

        if self.soft_mask_cache.len() > 32 {
            self.soft_mask_cache.clear();
        }

        {
            let size = (r_ceil * 2 + 1).max(1) as usize;
            let r_sq = r * r;
            let inv_radius = if r > 0.0 { 1.0 / r } else { 0.0 };
            let frac_x = frac_x as f32 / 16.0;
            let frac_y = frac_y as f32 / 16.0;
            let mut alpha = vec![0; size * size];
            let mut spans = Vec::new();

            for my in 0..size {
                let pdy = my as f32 - r_ceil as f32 + 0.5 - frac_y;
                let mut span_start = None;
                for mx in 0..size {
                    let pdx = mx as f32 - r_ceil as f32 + 0.5 - frac_x;
                    let dist_sq = pdx * pdx + pdy * pdy;
                    if dist_sq >= r_sq {
                        if let Some(x_start) = span_start.take() {
                            spans.push(MaskSpan {
                                y: my,
                                x_start,
                                x_end: mx,
                            });
                        }
                        continue;
                    }

                    let dist = dist_sq.sqrt();
                    let t = dist * inv_radius;
                    let mut alpha_factor = if t < hardness_val || hardness_val >= 1.0 {
                        1.0
                    } else {
                        let v = (t - hardness_val) / (1.0 - hardness_val);
                        let falloff = 1.0 - v.clamp(0.0, 1.0);
                        let f2 = falloff * falloff;
                        f2 * (3.0 - 2.0 * falloff)
                    };

                    if dist > fade_start {
                        alpha_factor *= 1.0 - (dist - fade_start) * inv_fade_width;
                    }
                    let alpha_u8 = (alpha_factor.clamp(0.0, 1.0) * 255.0) as u8;
                    alpha[my * size + mx] = alpha_u8;
                    if alpha_u8 == 0 {
                        if let Some(x_start) = span_start.take() {
                            spans.push(MaskSpan {
                                y: my,
                                x_start,
                                x_end: mx,
                            });
                        }
                    } else if span_start.is_none() {
                        span_start = Some(mx);
                    }
                }
                if let Some(x_start) = span_start {
                    spans.push(MaskSpan {
                        y: my,
                        x_start,
                        x_end: size,
                    });
                }
            }

            self.soft_mask_cache.push(SoftMaskCache {
                diameter_bits,
                hardness_bits,
                frac_x: (frac_x * 16.0) as u8,
                frac_y: (frac_y * 16.0) as u8,
                r_ceil,
                size,
                alpha,
                spans,
            });
        }

        &self.soft_mask_cache[self.soft_mask_cache.len() - 1]
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
            BrushType::Soft => {
                self.soft_dab(pool, canvas, selection, center, undo_action, modified_tiles)
            }
            BrushType::Pixel => {
                self.pixel_dab(pool, canvas, selection, center, undo_action, modified_tiles)
            }
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
                let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                let Some(data) = tile.data.as_mut() else {
                    continue;
                };

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

        let Some(bounds) = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size) else {
            return;
        };
        let regions = build_tile_regions(&bounds);

        self.snapshot_tiles(canvas, &regions, undo_action, modified_tiles);

        let src_base = self.brush_options.color;
        let base_alpha = self.brush_options.color.a() as f32
            * self.brush_options.opacity
            * (self.brush_options.flow / 100.0);
        let src_r = src_base.r();
        let src_g = src_base.g();
        let src_b = src_base.b();
        let linear_brush = LinearBrushColor::new(src_r, src_g, src_b);

        // Pre-compute common shape data
        let r_sq = r * r;
        let custom_data_ref = match &self.brush_options.pixel_shape {
            PixelBrushShape::Custom {
                width,
                height,
                data,
            } => Some((width, height, data)),
            _ => None,
        };
        let blend_mode = self.brush_options.blend_mode;
        let center_x = center.x;
        let center_y = center.y;
        let pixel_shape = &self.brush_options.pixel_shape;
        let diameter = self.brush_options.diameter;

        // Parallel execution for pixel dab
        let tiles = tile_coords(&bounds);

        let draw_tile = |(tx, ty): &(usize, usize)| {
            if let Some(tile_arc) = canvas.lock_tile(*tx, *ty) {
                let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
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

                if !tile_overlaps_selection(selection, tile_x0, tile_y0, tile_size) {
                    return; // Tile completely outside selection, skip
                }

                for gy in overlap_min_y..=overlap_max_y {
                    let dy = gy as f32 + 0.5 - center_y;
                    let py = gy as f32 + 0.5;
                    for gx in overlap_min_x..=overlap_max_x {
                        let dx = gx as f32 + 0.5 - center_x;
                        let px = gx as f32 + 0.5;

                        if let Some(sel) = selection
                            && !sel.contains_coords(px, py)
                        {
                            continue;
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

                            match blend_mode {
                                BlendMode::Normal => {
                                    data[idx] =
                                        alpha_over_brush(&linear_brush, alpha_u8, data[idx]);
                                }
                                BlendMode::Eraser => {
                                    let src_color = Color32::from_rgba_unmultiplied(
                                        src_r, src_g, src_b, alpha_u8,
                                    );
                                    data[idx] = blend_erase(src_color, data[idx]);
                                }
                            }
                        }
                    }
                }

                // Mark tile as dirty (not empty) after modifications
                tile.is_empty = false;
            }
        };

        if tiles.len() == 1 || (tiles.len() <= 4 && diameter <= 24.0) {
            tiles.iter().for_each(draw_tile);
        } else {
            pool.install(|| tiles.par_iter().for_each(draw_tile));
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
        let tile_size = canvas.tile_size();
        let canvas_w = canvas.width() as i32;
        let canvas_h = canvas.height() as i32;

        let Some(bounds) = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size) else {
            return;
        };
        let regions = build_tile_regions(&bounds);

        self.snapshot_tiles(canvas, &regions, undo_action, modified_tiles);

        // Pre-compute values outside loops
        let base_color = self.brush_options.color;
        let sr = base_color.r();
        let sg = base_color.g();
        let sb = base_color.b();
        let linear_brush = LinearBrushColor::new(sr, sg, sb);
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
        let inv_fade_width = if fade_width > 0.0 {
            1.0 / fade_width
        } else {
            0.0
        };

        let tiles = tile_coords(&bounds);

        if selection.is_none()
            && blend_mode == BlendMode::Normal
            && anti_aliasing
            && softness_selector == SoftnessSelector::Gaussian
            && matches!(pixel_shape, PixelBrushShape::Circle)
        {
            let mask =
                self.gaussian_circle_mask(center, r, hardness_val, fade_start, inv_fade_width);
            let mask_base_x = center_x.floor() as i32 - mask.r_ceil;
            let mask_base_y = center_y.floor() as i32 - mask.r_ceil;
            let opacity_scale = base_alpha * flow_alpha;
            let draw_tile = |(tx, ty): &(usize, usize)| {
                let tile_x0 = tx * tile_size;
                let tile_y0 = ty * tile_size;
                let tile_x1 = tile_x0 + tile_size;
                let tile_y1 = tile_y0 + tile_size;

                if center_x < (tile_x0 as f32 - r)
                    || center_x > (tile_x1 as f32 + r)
                    || center_y < (tile_y0 as f32 - r)
                    || center_y > (tile_y1 as f32 + r)
                {
                    return;
                }

                if let Some(tile_arc) = canvas.lock_tile(*tx, *ty) {
                    let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                    let data = match tile.data.as_mut() {
                        Some(d) => d,
                        None => return,
                    };

                    let overlap_min_x = bounds.start_x.max(tile_x0);
                    let overlap_max_x = bounds.end_x.min(tile_x0 + tile_size - 1);
                    let overlap_min_y = bounds.start_y.max(tile_y0);
                    let overlap_max_y = bounds.end_y.min(tile_y0 + tile_size - 1);

                    for span in &mask.spans {
                        let gy = mask_base_y + span.y as i32;
                        if gy < overlap_min_y as i32 || gy > overlap_max_y as i32 {
                            continue;
                        }
                        let x_start = (mask_base_x + span.x_start as i32).max(overlap_min_x as i32);
                        let x_end = (mask_base_x + span.x_end as i32).min(overlap_max_x as i32 + 1);

                        for gx in x_start..x_end {
                            let mx = (gx - mask_base_x) as usize;
                            let mask_alpha = mask.alpha[span.y * mask.size + mx];
                            let local_y = gy as usize - tile_y0;
                            let local_x = gx as usize - tile_x0;
                            let idx = local_y * tile_size + local_x;
                            let alpha_u8 =
                                (mask_alpha as f32 * opacity_scale).clamp(0.0, 255.0) as u8;
                            data[idx] = alpha_over_brush(&linear_brush, alpha_u8, data[idx]);
                        }
                    }

                    tile.is_empty = false;
                }
            };

            if tiles.len() == 1 || (tiles.len() <= 4 && diameter <= 24.0) {
                tiles.iter().for_each(draw_tile);
            } else {
                _pool.install(|| tiles.par_iter().for_each(draw_tile));
            }
            return;
        }

        let draw_tile = |(tx, ty): &(usize, usize)| {
            let tile_x0 = tx * tile_size;
            let tile_y0 = ty * tile_size;
            let tile_x1 = tile_x0 + tile_size;
            let tile_y1 = tile_y0 + tile_size;

            // Check if tile is reasonably close to center (bounding box check)
            if center_x < (tile_x0 as f32 - r)
                || center_x > (tile_x1 as f32 + r)
                || center_y < (tile_y0 as f32 - r)
                || center_y > (tile_y1 as f32 + r)
            {
                return;
            }

            if let Some(tile_arc) = canvas.lock_tile(*tx, *ty) {
                let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                let data = match tile.data.as_mut() {
                    Some(d) => d,
                    None => return,
                };

                let overlap_min_x = bounds.start_x.max(tile_x0);
                let overlap_max_x = bounds.end_x.min(tile_x0 + tile_size - 1);
                let overlap_min_y = bounds.start_y.max(tile_y0);
                let overlap_max_y = bounds.end_y.min(tile_y0 + tile_size - 1);

                if !tile_overlaps_selection(selection, tile_x0, tile_y0, tile_size) {
                    return; // Tile completely outside selection
                }

                for gy in overlap_min_y..=overlap_max_y {
                    let py = gy as f32 + 0.5;
                    for gx in overlap_min_x..=overlap_max_x {
                        let pdx = gx as f32 + 0.5 - center_x;
                        let pdy = py - center_y;
                        let px = gx as f32 + 0.5;

                        if let Some(sel) = selection
                            && !sel.contains_coords(px, py)
                        {
                            continue;
                        }

                        let alpha_factor = if anti_aliasing {
                            // Anti-aliased path (smooth, uses calc_soft_brush_alpha and AA fade)
                            let (base_alpha_at_pixel, dist_sq) = calc_soft_brush_alpha(
                                pdx,
                                pdy,
                                r,
                                pixel_shape,
                                hardness_val,
                                softness_selector,
                                softness_curve,
                            );

                            if base_alpha_at_pixel <= 0.0 {
                                // Early exit if inner shape is transparent
                                0.0
                            } else {
                                // Apply the 1.5 pixel outer fade using pre-computed distance
                                let dist_for_aa = match pixel_shape {
                                    PixelBrushShape::Circle => dist_sq.sqrt(),
                                    PixelBrushShape::Square => pdx.abs().max(pdy.abs()),
                                    PixelBrushShape::Custom { .. } => pdx.abs().max(pdy.abs()),
                                };

                                if dist_for_aa >= r {
                                    // Beyond brush radius, fully transparent
                                    0.0
                                } else if dist_for_aa > fade_start {
                                    // Within AA fade zone
                                    let fraction = (dist_for_aa - fade_start) * inv_fade_width;
                                    base_alpha_at_pixel * (1.0 - fraction) // Blend base alpha with fade
                                } else {
                                    // Solid interior
                                    base_alpha_at_pixel
                                }
                            }
                        } else {
                            // Non-anti-aliased path (hard edges)
                            let (in_shape, alpha_mod) = match &pixel_shape {
                                PixelBrushShape::Circle => ((pdx * pdx + pdy * pdy) <= r_sq, 1.0),
                                PixelBrushShape::Square => (pdx.abs() <= r && pdy.abs() <= r, 1.0),
                                PixelBrushShape::Custom {
                                    width,
                                    height,
                                    data,
                                } => {
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
                        let local_y = gy - tile_y0;
                        let local_x = gx - tile_x0;
                        let idx = local_y * tile_size + local_x;

                        match blend_mode {
                            BlendMode::Normal => {
                                data[idx] = alpha_over_brush(&linear_brush, alpha_u8, data[idx]);
                            }
                            BlendMode::Eraser => {
                                let src = Color32::from_rgba_unmultiplied(sr, sg, sb, alpha_u8);
                                data[idx] = blend_erase(src, data[idx]);
                            }
                        }
                    }
                }

                // Mark tile as dirty (not empty) after modifications
                tile.is_empty = false;
            }
        };

        if tiles.len() == 1 || (tiles.len() <= 4 && diameter <= 24.0) {
            tiles.iter().for_each(draw_tile);
        } else {
            _pool.install(|| tiles.par_iter().for_each(draw_tile));
        }
    }
}

/// Named preset that can be displayed in the UI and cloned into the active brush.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub brush: Brush,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_gaussian_circle_mask_matches_formula() {
        let mut brush = Brush::new(24.0, 20.0, Color32::BLACK, 25.0);
        let r = 12.0;
        let hardness = 0.2;
        let fade_start = 10.5;
        let inv_fade_width = 1.0 / 1.5;
        let center = Vec2::new(10.25, 8.5);

        let mask = brush.gaussian_circle_mask(center, r, hardness, fade_start, inv_fade_width);
        let mx = mask.r_ceil as usize;
        let my = mask.r_ceil as usize;
        let cached = mask.alpha[my * mask.size + mx] as f32 / 255.0;
        assert!(
            mask.spans
                .iter()
                .any(|span| span.y == my && span.x_start <= mx && mx < span.x_end)
        );

        let pdx = mx as f32 - mask.r_ceil as f32 + 0.5 - 0.25;
        let pdy = my as f32 - mask.r_ceil as f32 + 0.5 - 0.5;
        let (base, dist_sq) = calc_soft_brush_alpha(
            pdx,
            pdy,
            r,
            &PixelBrushShape::Circle,
            hardness,
            SoftnessSelector::Gaussian,
            &brush.brush_options.softness_curve,
        );
        let dist = dist_sq.sqrt();
        let expected = if dist > fade_start {
            base * (1.0 - (dist - fade_start) * inv_fade_width)
        } else {
            base
        };

        assert!((cached - expected).abs() <= 1.0 / 255.0);
    }
}
