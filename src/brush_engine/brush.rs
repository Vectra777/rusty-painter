use super::brush_options::BrushOptions;
use crate::{
    brush_engine::{
        brush_options::{BlendMode, PixelBrushShape},
        dab::{
            PlacedDab, TileBucket, TileRegion, bucket_by_tile, calc_dab_bounds, dab_reaches_tile,
            dispatch_over_buckets, tile_overlap, tile_overlaps_selection,
        },
        hardness::SoftnessSelector,
        masks::{calc_soft_brush_alpha, gaussian_falloff, sample_custom_mask_nn},
        stroke::StrokeTiles,
    },
    canvas::{
        Canvas,
        history::{TileSnapshot, UndoAction},
        storage::{LinearBrushColor, alpha_over_brush, alpha_over_brush_batch, blend_erase},
    },
    selection::SelectionManager,
};
use eframe::egui::Color32;
use eframe::egui::Vec2;
use rayon::ThreadPool;
use std::sync::Arc;
use wide::{CmpGt, CmpLt, f32x4};

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

/// Gaussian circle brush tip: per-pixel alpha as a function of distance from
/// the (sub-pixel quantized) dab center.
struct GaussianTip {
    r_ceil: i32,
    r_sq: f32,
    inv_radius: f32,
    hardness: f32,
    fade_start: f32,
    inv_fade_width: f32,
}

impl GaussianTip {
    #[inline]
    fn alpha(&self, dist_sq: f32, dist: f32, t: f32) -> u8 {
        if dist_sq >= self.r_sq {
            return 0;
        }
        let mut alpha_factor = gaussian_falloff(t, self.hardness);
        if dist > self.fade_start {
            alpha_factor *= 1.0 - (dist - self.fade_start) * self.inv_fade_width;
        }
        (alpha_factor.clamp(0.0, 1.0) * 255.0) as u8
    }

    /// Four lanes of [`Self::alpha`], branch-free: both sides of each branch
    /// are computed and selected per lane, with the same float operations in
    /// the same order as the scalar version, so every lane is bit-identical.
    /// `fast_max`/`fast_min` are safe here: no input can be NaN.
    #[inline]
    fn alpha_x4(&self, dist_sq: f32x4, dist: f32x4, t: f32x4) -> wide::i32x4 {
        let zero = f32x4::splat(0.0);
        let one = f32x4::splat(1.0);
        let mut alpha = if self.hardness >= 1.0 {
            one
        } else {
            let hardness = f32x4::splat(self.hardness);
            let v = (t - hardness) / f32x4::splat(1.0 - self.hardness);
            let falloff = one - v.fast_max(zero).fast_min(one);
            let smooth = falloff * falloff * (f32x4::splat(3.0) - f32x4::splat(2.0) * falloff);
            t.cmp_lt(hardness).blend(one, smooth)
        };
        let fade_start = f32x4::splat(self.fade_start);
        let faded = alpha * (one - (dist - fade_start) * f32x4::splat(self.inv_fade_width));
        alpha = dist.cmp_gt(fade_start).blend(faded, alpha);
        let alpha = dist_sq
            .cmp_lt(f32x4::splat(self.r_sq))
            .blend(alpha.fast_max(zero).fast_min(one) * f32x4::splat(255.0), zero);
        alpha.fast_trunc_int()
    }

    /// Mask alphas for one dab row, for mask columns `mx0..mx0 + out.len()`.
    /// Computed on the fly per tile instead of building (and caching) a full
    /// per-dab mask: pressure changes the diameter every sample, so such a
    /// cache almost never hit.
    fn row(&self, pdy: f32, frac_x: f32, mx0: usize, out: &mut [u8]) {
        let r_ceil = self.r_ceil as f32;
        let pdy_sq = pdy * pdy;
        let mut k = 0;
        // 4-wide distance/sqrt; per-lane operation order matches the scalar
        // tail, so results are bit-identical either way.
        while k + 4 <= out.len() {
            let mx_f = (mx0 + k) as f32;
            let pdx = f32x4::new([mx_f, mx_f + 1.0, mx_f + 2.0, mx_f + 3.0])
                - f32x4::splat(r_ceil)
                + f32x4::splat(0.5)
                - f32x4::splat(frac_x);
            let dist_sq = pdx * pdx + f32x4::splat(pdy_sq);
            let dist = dist_sq.sqrt();
            let t = dist * f32x4::splat(self.inv_radius);
            let alphas = self.alpha_x4(dist_sq, dist, t).to_array();
            for (slot, alpha) in out[k..k + 4].iter_mut().zip(alphas) {
                *slot = alpha as u8;
            }
            k += 4;
        }
        for (i, slot) in out.iter_mut().enumerate().skip(k) {
            let pdx = (mx0 + i) as f32 - r_ceil + 0.5 - frac_x;
            let dist_sq = pdx * pdx + pdy_sq;
            let dist = dist_sq.sqrt();
            *slot = self.alpha(dist_sq, dist, dist * self.inv_radius);
        }
    }
}

/// Blend `pixels` wherever `mask` is non-zero, one contiguous run at a time,
/// in chunks of 64 through `blend`.
fn blend_mask_runs(
    mask: &[u8],
    opacity_scale: f32,
    pixels: &mut [Color32],
    blend: &impl Fn(&[u8], &mut [Color32]),
) {
    const CHUNK: usize = 64;
    let mut alphas = [0u8; CHUNK];
    let mut x = 0;
    while x < mask.len() {
        if mask[x] == 0 {
            x += 1;
            continue;
        }
        let run_start = x;
        while x < mask.len() && mask[x] != 0 {
            x += 1;
        }
        let mut c = run_start;
        while c < x {
            let n = CHUNK.min(x - c);
            for (a, &m) in alphas[..n].iter_mut().zip(&mask[c..c + n]) {
                *a = (m as f32 * opacity_scale).clamp(0.0, 255.0) as u8;
            }
            blend(&alphas[..n], &mut pixels[c..c + n]);
            c += n;
        }
    }
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
    /// Linear-light table for the current brush RGB, rebuilt only when the color changes.
    linear_color: Option<([u8; 3], Arc<LinearBrushColor>)>,
}

/// Blend one pixel of paint into `data[idx]`. Identical in `pixel_dab` and
/// `soft_dab`'s general path (previously duplicated inline in both).
#[inline]
fn apply_dab_blend(
    blend_mode: BlendMode,
    linear_brush: &LinearBrushColor,
    r: u8,
    g: u8,
    b: u8,
    alpha_u8: u8,
    data: &mut [Color32],
    idx: usize,
) {
    match blend_mode {
        BlendMode::Normal => {
            data[idx] = alpha_over_brush(linear_brush, alpha_u8, data[idx]);
        }
        BlendMode::Eraser => {
            let src = Color32::from_rgba_unmultiplied(r, g, b, alpha_u8);
            data[idx] = blend_erase(src, data[idx]);
        }
    }
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
            linear_color: None,
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
            linear_color: None,
        }
    }

    fn linear_color(&mut self) -> Arc<LinearBrushColor> {
        let c = self.brush_options.color;
        let key = [c.r(), c.g(), c.b()];
        match &self.linear_color {
            Some((cached_key, color)) if *cached_key == key => color.clone(),
            _ => {
                let color = Arc::new(LinearBrushColor::new(key[0], key[1], key[2]));
                self.linear_color = Some((key, color.clone()));
                color
            }
        }
    }

    /// Paint a batch of dabs (in stroke order) with the current brush.
    ///
    /// Dabs are grouped per tile and each tile applies its dabs in order, so
    /// the result is identical to painting them one by one, but every tile is
    /// locked once and the thread pool is entered once per batch.
    pub(crate) fn dabs(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let r_ceil = r.ceil() as i32;
        let tile_size = canvas.tile_size();
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);
        let dabs: Vec<PlacedDab> = centers
            .iter()
            .filter_map(|&center| {
                let bounds = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size)?;
                Some(PlacedDab::new(center, bounds, r_ceil))
            })
            .collect();
        if dabs.is_empty() {
            return;
        }

        let buckets = bucket_by_tile(&dabs);
        let regions: Vec<TileRegion> = buckets.iter().map(|(region, _)| *region).collect();
        self.snapshot_tiles(canvas, &regions, undo_action, stroke_tiles);

        let side = (2 * r_ceil + 1).max(1) as usize;
        let work_pixels = dabs.len() * side * side;
        match self.brush_type {
            BrushType::Soft => {
                self.soft_dabs(pool, canvas, selection, &dabs, &buckets, work_pixels)
            }
            BrushType::Pixel => {
                self.pixel_dabs(pool, canvas, selection, &dabs, &buckets, work_pixels)
            }
        }
    }

    /// Snapshot tiles about to be modified so undo can restore them later.
    fn snapshot_tiles(
        &self,
        canvas: &Canvas,
        regions: &[TileRegion],
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let layer_idx = canvas.active_layer_idx;
        let tile_size = canvas.tile_size();
        let Some(layer_id) = canvas.layer_id_at(layer_idx) else {
            return;
        };

        for region in regions {
            stroke_tiles.dirty.insert((region.tx, region.ty));
            // insert returns false if value was already present
            if !stroke_tiles.snapshotted.insert((region.tx, region.ty)) {
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
                    layer_id,
                    x0: 0,
                    y0: 0,
                    width: tile_size,
                    height: tile_size,
                    data: patch,
                });
            }
        }
    }

    /// Render hard, pixel-aligned dabs.
    fn pixel_dabs(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        dabs: &[PlacedDab],
        buckets: &[TileBucket],
        work_pixels: usize,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let r_sq = r * r;
        let tile_size = canvas.tile_size();
        let src_base = self.brush_options.color;
        let base_alpha = self.brush_options.color.a() as f32
            * self.brush_options.opacity
            * (self.brush_options.flow / 100.0);
        let (src_r, src_g, src_b) = (src_base.r(), src_base.g(), src_base.b());
        let linear_brush = self.linear_color();
        let blend_mode = self.brush_options.blend_mode;
        let pixel_shape = &self.brush_options.pixel_shape;
        let diameter = self.brush_options.diameter;

        let draw_tile = |(region, dab_ids): &TileBucket| {
            let tile_x0 = region.tx * tile_size;
            let tile_y0 = region.ty * tile_size;
            if !tile_overlaps_selection(selection, tile_x0, tile_y0, tile_size) {
                return;
            }
            let Some(tile_arc) = canvas.lock_tile(region.tx, region.ty) else {
                return;
            };
            let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(data) = tile.data.as_mut() else {
                return;
            };

            let mut sel_row = vec![true; tile_size];
            for &i in dab_ids {
                let dab = &dabs[i];
                let overlap = tile_overlap(&dab.bounds, tile_x0, tile_y0, tile_size);
                for gy in overlap.min_y..=overlap.max_y {
                    let dy = gy as f32 + 0.5 - dab.center.y;
                    if let Some(sel) = selection {
                        sel.row_mask(gy, overlap.min_x, &mut sel_row[..=overlap.max_x - overlap.min_x]);
                    }
                    for gx in overlap.min_x..=overlap.max_x {
                        let dx = gx as f32 + 0.5 - dab.center.x;

                        if !sel_row[gx - overlap.min_x] {
                            continue;
                        }

                        let (in_shape, alpha_mod) = match pixel_shape {
                            PixelBrushShape::Circle => (dx * dx + dy * dy <= r_sq, 1.0),
                            PixelBrushShape::Square => (dx.abs() <= r && dy.abs() <= r, 1.0),
                            PixelBrushShape::Custom {
                                width,
                                height,
                                data,
                            } => sample_custom_mask_nn(dx, dy, diameter, *width, *height, data),
                        };

                        if in_shape {
                            let idx = (gy - tile_y0) * tile_size + (gx - tile_x0);
                            let final_alpha = (base_alpha * alpha_mod).clamp(0.0, 1.0);
                            let alpha_u8 = (final_alpha * 255.0) as u8;
                            apply_dab_blend(
                                blend_mode,
                                &linear_brush,
                                src_r,
                                src_g,
                                src_b,
                                alpha_u8,
                                data,
                                idx,
                            );
                        }
                    }
                }
            }
            tile.is_empty = false;
        };

        dispatch_over_buckets(buckets, pool, work_pixels, draw_tile);
    }

    /// Render soft, anti-aliased dabs.
    fn soft_dabs(
        &mut self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        dabs: &[PlacedDab],
        buckets: &[TileBucket],
        work_pixels: usize,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let r_sq = r * r;
        let tile_size = canvas.tile_size();
        let base_color = self.brush_options.color;
        let (sr, sg, sb) = (base_color.r(), base_color.g(), base_color.b());
        let linear_brush = self.linear_color();
        let base_alpha = base_color.a() as f32 / 255.0;
        let flow_alpha = self.brush_options.opacity * (self.brush_options.flow / 100.0);
        let blend_mode = self.brush_options.blend_mode;
        let anti_aliasing = self.anti_aliasing;
        let hardness_val = (self.brush_options.hardness / 100.0).clamp(0.0, 1.0);
        let softness_selector = self.brush_options.softness_selector;
        let softness_curve = &self.brush_options.softness_curve;
        let pixel_shape = &self.brush_options.pixel_shape;
        let diameter = self.brush_options.diameter;

        // Pre-compute fade values for anti-aliasing (1.5 pixel outer fade)
        let fade_start = (r - 1.5).max(0.0);
        let fade_width = 1.5_f32.min(r);
        let inv_fade_width = if fade_width > 0.0 { 1.0 / fade_width } else { 0.0 };

        let fast_path = anti_aliasing
            && softness_selector == SoftnessSelector::Gaussian
            && matches!(pixel_shape, PixelBrushShape::Circle);
        let tip = GaussianTip {
            r_ceil: r.ceil() as i32,
            r_sq,
            inv_radius: if r > 0.0 { 1.0 / r } else { 0.0 },
            hardness: hardness_val,
            fade_start,
            inv_fade_width,
        };
        let opacity_scale = base_alpha * flow_alpha;
        let blend_run = |alphas: &[u8], pixels: &mut [Color32]| match blend_mode {
            BlendMode::Normal => alpha_over_brush_batch(&linear_brush, alphas, pixels),
            BlendMode::Eraser => {
                for (pixel, &alpha) in pixels.iter_mut().zip(alphas) {
                    *pixel = blend_erase(Color32::from_rgba_premultiplied(0, 0, 0, alpha), *pixel);
                }
            }
        };

        let draw_tile = |(region, dab_ids): &TileBucket| {
            let tile_x0 = region.tx * tile_size;
            let tile_y0 = region.ty * tile_size;
            if !tile_overlaps_selection(selection, tile_x0, tile_y0, tile_size) {
                return;
            }
            let Some(tile_arc) = canvas.lock_tile(region.tx, region.ty) else {
                return;
            };
            let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(data) = tile.data.as_mut() else {
                return;
            };
            let mut mask_row = if fast_path { vec![0u8; tile_size] } else { Vec::new() };
            let mut sel_row = vec![true; tile_size];
            let mut painted = false;

            for &i in dab_ids {
                let dab = &dabs[i];
                if !dab_reaches_tile(dab.center, r, tile_x0, tile_y0, tile_size) {
                    continue;
                }
                painted = true;
                let overlap = tile_overlap(&dab.bounds, tile_x0, tile_y0, tile_size);

                if fast_path {
                    let width = overlap.max_x - overlap.min_x + 1;
                    let mx0 = (overlap.min_x as i32 - dab.base_x) as usize;
                    for gy in overlap.min_y..=overlap.max_y {
                        let my = gy as i32 - dab.base_y;
                        let pdy = my as f32 - tip.r_ceil as f32 + 0.5 - dab.frac_y;
                        let mask = &mut mask_row[..width];
                        tip.row(pdy, dab.frac_x, mx0, mask);
                        if let Some(sel) = selection {
                            let inside = &mut sel_row[..width];
                            sel.row_mask(gy, overlap.min_x, inside);
                            for (alpha, &inside) in mask.iter_mut().zip(inside.iter()) {
                                if !inside {
                                    *alpha = 0;
                                }
                            }
                        }
                        let start = (gy - tile_y0) * tile_size + (overlap.min_x - tile_x0);
                        blend_mask_runs(mask, opacity_scale, &mut data[start..start + width], &blend_run);
                    }
                    continue;
                }

                for gy in overlap.min_y..=overlap.max_y {
                    let py = gy as f32 + 0.5;
                    if let Some(sel) = selection {
                        sel.row_mask(gy, overlap.min_x, &mut sel_row[..=overlap.max_x - overlap.min_x]);
                    }
                    for gx in overlap.min_x..=overlap.max_x {
                        let pdx = gx as f32 + 0.5 - dab.center.x;
                        let pdy = py - dab.center.y;

                        if !sel_row[gx - overlap.min_x] {
                            continue;
                        }

                        let alpha_factor = if anti_aliasing {
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
                                0.0
                            } else {
                                // 1.5 pixel outer fade using the pre-computed distance
                                let dist_for_aa = match pixel_shape {
                                    PixelBrushShape::Circle => dist_sq.sqrt(),
                                    PixelBrushShape::Square => pdx.abs().max(pdy.abs()),
                                    PixelBrushShape::Custom { .. } => pdx.abs().max(pdy.abs()),
                                };

                                if dist_for_aa >= r {
                                    0.0
                                } else if dist_for_aa > fade_start {
                                    let fraction = (dist_for_aa - fade_start) * inv_fade_width;
                                    base_alpha_at_pixel * (1.0 - fraction)
                                } else {
                                    base_alpha_at_pixel
                                }
                            }
                        } else {
                            let (in_shape, alpha_mod) = match pixel_shape {
                                PixelBrushShape::Circle => ((pdx * pdx + pdy * pdy) <= r_sq, 1.0),
                                PixelBrushShape::Square => (pdx.abs() <= r && pdy.abs() <= r, 1.0),
                                PixelBrushShape::Custom {
                                    width,
                                    height,
                                    data,
                                } => sample_custom_mask_nn(pdx, pdy, diameter, *width, *height, data),
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
                        let idx = (gy - tile_y0) * tile_size + (gx - tile_x0);
                        apply_dab_blend(blend_mode, &linear_brush, sr, sg, sb, alpha_u8, data, idx);
                    }
                }
            }

            if painted {
                tile.is_empty = false;
            }
        };

        dispatch_over_buckets(buckets, pool, work_pixels, draw_tile);
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
    use rayon::ThreadPoolBuilder;
    use std::collections::HashSet;

    /// Deterministic, dependency-free hash (FNV-1a) over raw RGBA bytes.
    /// Used as a golden-master checksum: captured once from known-correct
    /// output, then asserted unchanged across refactors of the pixel-stamp
    /// hot path, which is otherwise very hard to regression-test without a
    /// display to visually compare rendered frames.
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    const TILE_SIZE_FOR_TEST: usize = 64;

    /// Paint each `(brush, centers)` step onto a shared fresh 2x2-tile
    /// canvas in order, then return an FNV-1a checksum of every tile's raw
    /// pixel bytes (layer 1, the paintable default layer). Spans multiple
    /// tiles so both the single-tile and multi-tile/parallel dispatch code
    /// paths run. Multiple steps let a scenario paint a base fill, then
    /// erase/blend on top of it within the same checksum.
    fn paint_and_checksum(steps: &[(Brush, Vec<Vec2>)]) -> u64 {
        paint_and_checksum_in(steps, None)
    }

    /// A concave lasso covering part of the stroke, for selection goldens.
    fn star_selection() -> SelectionManager {
        let star: Vec<Vec2> = (0..17)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 17.0;
                let r = if i % 2 == 0 { 38.0 } else { 17.3 };
                Vec2::new(50.2 + a.cos() * r, 46.7 + a.sin() * r)
            })
            .collect();
        SelectionManager {
            current_shape: Some(crate::selection::new_lasso_shape(star)),
            is_dragging: false,
        }
    }

    fn paint_and_checksum_in(steps: &[(Brush, Vec<Vec2>)], selection: Option<&SelectionManager>) -> u64 {
        fnv1a(&paint_bytes_in(steps, selection))
    }

    fn paint_bytes_in(steps: &[(Brush, Vec<Vec2>)], selection: Option<&SelectionManager>) -> Vec<u8> {
        let canvas = Canvas::new(96, 96, Color32::TRANSPARENT, TILE_SIZE_FOR_TEST);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();

        for (brush, centers) in steps {
            let mut brush = brush.clone();
            let mut undo_action = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut stroke_tiles = StrokeTiles::default();
            // One batch per step: the goldens were captured painting one dab
            // at a time, so matching them proves batching changes nothing.
            brush.dabs(&pool, &canvas, selection, centers, &mut undo_action, &mut stroke_tiles);
        }

        let mut bytes = Vec::new();
        for ty in 0..2 {
            for tx in 0..2 {
                let data = canvas
                    .get_layer_tile_data(1, tx, ty)
                    .unwrap_or_else(|| vec![Color32::TRANSPARENT; TILE_SIZE_FOR_TEST * TILE_SIZE_FOR_TEST]);
                for pixel in data {
                    bytes.extend_from_slice(&pixel.to_array());
                }
            }
        }
        bytes
    }

    fn stroke_centers() -> Vec<Vec2> {
        // A short diagonal stroke crossing all four tiles of the 2x2 grid.
        vec![
            Vec2::new(20.0, 20.0),
            Vec2::new(40.0, 40.0),
            Vec2::new(60.0, 60.0),
            Vec2::new(80.0, 80.0),
        ]
    }

    /// Golden-master check for the hard-edged Pixel brush path
    /// (`Brush::pixel_dab`). If this fails after a refactor, the refactor
    /// changed pixel output, not just structure.
    #[test]
    fn pixel_dab_output_is_stable() {
        let mut brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(200, 30, 30, 255));
        brush.brush_options.pixel_shape = PixelBrushShape::Circle;
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(checksum, 0xc540cd961ef28f05, "GOLDEN_PLACEHOLDER:pixel_dab_output_is_stable");
    }

    /// Golden-master check for the Pixel brush with a Square tip.
    #[test]
    fn pixel_dab_square_output_is_stable() {
        let mut brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(30, 200, 30, 255));
        brush.brush_options.pixel_shape = PixelBrushShape::Square;
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0x30fed29cce6020a5,
            "GOLDEN_PLACEHOLDER:pixel_dab_square_output_is_stable"
        );
    }

    #[test]
    fn linear_color_table_follows_brush_color_changes() {
        let mut brush = Brush::new(8.0, 100.0, Color32::RED, 25.0);
        let red = brush.linear_color();
        assert!(Arc::ptr_eq(&red, &brush.linear_color()), "same color reuses the table");

        brush.brush_options.color = Color32::BLUE;
        let blue = brush.linear_color();
        assert!(!Arc::ptr_eq(&red, &blue));
        assert_eq!(alpha_over_brush(&blue, 255, Color32::WHITE), Color32::BLUE);
    }

    #[test]
    fn dirty_tiles_track_only_dabs_since_last_drain() {
        let canvas = Canvas::new(96, 96, Color32::TRANSPARENT, TILE_SIZE_FOR_TEST);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let mut brush = Brush::new(8.0, 50.0, Color32::BLACK, 25.0);
        let mut undo_action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut stroke_tiles = StrokeTiles::default();

        let first = Vec2::new(16.0, 16.0);
        brush.dabs(&pool, &canvas, None, &[first], &mut undo_action, &mut stroke_tiles);
        stroke_tiles.dirty.clear();

        let second = Vec2::new(80.0, 80.0);
        brush.dabs(&pool, &canvas, None, &[second], &mut undo_action, &mut stroke_tiles);

        assert_eq!(stroke_tiles.dirty, HashSet::from([(1, 1)]));
        assert_eq!(stroke_tiles.snapshotted, HashSet::from([(0, 0), (1, 1)]));
    }

    /// Golden-master check for the Soft brush's fast Gaussian-circle path
    /// (anti_aliasing + Gaussian + Circle + Normal blend + no selection).
    #[test]
    fn soft_dab_gaussian_fast_path_output_is_stable() {
        let brush = Brush::new(24.0, 40.0, Color32::from_rgba_unmultiplied(30, 30, 200, 255), 15.0);
        assert!(brush.anti_aliasing);
        assert_eq!(brush.brush_options.softness_selector, SoftnessSelector::Gaussian);
        assert_eq!(brush.brush_options.pixel_shape, PixelBrushShape::Circle);
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0x3746e62f3bd2ff5d,
            "GOLDEN_PLACEHOLDER:soft_dab_gaussian_fast_path_output_is_stable"
        );
    }

    /// Golden-master check for the Soft brush's general (non-fast-path)
    /// path, forced by a Square tip.
    #[test]
    fn soft_dab_general_path_square_output_is_stable() {
        let mut brush =
            Brush::new(24.0, 40.0, Color32::from_rgba_unmultiplied(200, 200, 30, 255), 15.0);
        brush.brush_options.pixel_shape = PixelBrushShape::Square;
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0xf51081982ccff9b5,
            "GOLDEN_PLACEHOLDER:soft_dab_general_path_square_output_is_stable"
        );
    }

    /// Golden-master check for the Soft brush's general path without
    /// anti-aliasing (hard edges, still goes through calc_soft_brush_alpha).
    #[test]
    fn soft_dab_general_path_no_aa_output_is_stable() {
        let mut brush = Brush::new(24.0, 40.0, Color32::from_rgba_unmultiplied(200, 30, 200, 255), 15.0);
        brush.anti_aliasing = false;
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0x80c2a24b62e25d05,
            "GOLDEN_PLACEHOLDER:soft_dab_general_path_no_aa_output_is_stable"
        );
    }

    /// Golden-master check for the Soft brush's general path with the
    /// Eraser blend mode, erasing into a pre-painted solid fill.
    #[test]
    fn soft_dab_general_path_eraser_output_is_stable() {
        let fill_brush =
            Brush::new(60.0, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 255), 15.0);
        let mut eraser_brush = Brush::new(24.0, 40.0, Color32::from_rgba_unmultiplied(0, 0, 0, 255), 15.0);
        eraser_brush.brush_options.blend_mode = BlendMode::Eraser;

        let checksum = paint_and_checksum(&[
            (fill_brush, vec![Vec2::new(48.0, 48.0)]),
            (eraser_brush, stroke_centers()),
        ]);
        assert_eq!(
            checksum, 0x99e2b1f695633483,
            "GOLDEN_PLACEHOLDER:soft_dab_general_path_eraser_output_is_stable"
        );
    }

    #[test]
    fn soft_dab_selection_output_is_stable() {
        let brush = Brush::new(24.0, 40.0, Color32::from_rgba_unmultiplied(30, 150, 90, 255), 15.0);
        let checksum = paint_and_checksum_in(&[(brush, stroke_centers())], Some(&star_selection()));
        assert_eq!(checksum, 0xc6cc6a71eff28584, "GOLDEN_PLACEHOLDER:soft_dab_selection_output_is_stable");
    }

    #[test]
    fn pixel_dab_selection_output_is_stable() {
        let brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(90, 30, 150, 255));
        let checksum = paint_and_checksum_in(&[(brush, stroke_centers())], Some(&star_selection()));
        assert_eq!(checksum, 0x322bba2014492c15, "GOLDEN_PLACEHOLDER:pixel_dab_selection_output_is_stable");
    }

    #[test]
    fn gaussian_tip_matches_soft_brush_formula() {
        let brush = Brush::new(24.0, 20.0, Color32::BLACK, 25.0);
        let r = 12.0;
        let hardness = 0.2;
        let fade_start = 10.5;
        let inv_fade_width = 1.0 / 1.5;
        let tip = GaussianTip {
            r_ceil: 12,
            r_sq: r * r,
            inv_radius: 1.0 / r,
            hardness,
            fade_start,
            inv_fade_width,
        };
        let (frac_x, frac_y) = (0.25, 0.5);
        let my = 12usize;
        let pdy = my as f32 - 12.0 + 0.5 - frac_y;
        let mut row = [0u8; 25];
        tip.row(pdy, frac_x, 0, &mut row);

        for (mx, &alpha) in row.iter().enumerate() {
            let pdx = mx as f32 - 12.0 + 0.5 - frac_x;
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
            let expected = if dist >= r {
                0.0
            } else if dist > fade_start {
                base * (1.0 - (dist - fade_start) * inv_fade_width)
            } else {
                base
            };
            assert!((alpha as f32 / 255.0 - expected).abs() <= 1.0 / 255.0, "mx={mx}");
        }
    }

    #[test]
    fn gaussian_tip_simd_lanes_match_scalar() {
        for hardness in [0.0, 0.2, 0.5, 0.99, 1.0] {
            for r in [0.6_f32, 3.0, 12.0, 40.5] {
                let fade_start = (r - 1.5).max(0.0);
                let fade_width = 1.5_f32.min(r);
                let tip = GaussianTip {
                    r_ceil: r.ceil() as i32,
                    r_sq: r * r,
                    inv_radius: 1.0 / r,
                    hardness,
                    fade_start,
                    inv_fade_width: 1.0 / fade_width,
                };
                for step in 0..400 {
                    let base = step as f32 * (r + 2.0) / 100.0;
                    let dist = f32x4::new([base, base + 0.013, base + 0.37, base + 0.71]);
                    let dist_sq = dist * dist;
                    let t = dist * f32x4::splat(tip.inv_radius);
                    let simd = tip.alpha_x4(dist_sq, dist, t).to_array();
                    let (dist_sq, dist, t) = (dist_sq.to_array(), dist.to_array(), t.to_array());
                    for lane in 0..4 {
                        let scalar = tip.alpha(dist_sq[lane], dist[lane], t[lane]);
                        assert_eq!(simd[lane] as u8, scalar, "h={hardness} r={r} d={}", dist[lane]);
                    }
                }
            }
        }
    }

    /// Dense, overlapping dabs straddling tile corners on a multi-threaded
    /// pool: one batched call must match painting the dabs one at a time.
    #[test]
    fn batched_dabs_match_one_at_a_time() {
        let centers: Vec<Vec2> = (0..40)
            .map(|i| Vec2::new(30.0 + i as f32 * 1.7, 34.0 + (i as f32 * 0.9).sin() * 20.0))
            .collect();
        let brushes = [
            Brush::new(40.0, 30.0, Color32::from_rgba_unmultiplied(20, 90, 200, 180), 10.0),
            Brush::new_pixel(9.0, Color32::from_rgba_unmultiplied(200, 40, 40, 255)),
        ];
        for brush in brushes {
            let paint = |batched: bool| {
                let canvas = Canvas::new(128, 128, Color32::TRANSPARENT, 32);
                let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
                let mut brush = brush.clone();
                let mut undo = UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                };
                let mut tiles = StrokeTiles::default();
                if batched {
                    brush.dabs(&pool, &canvas, None, &centers, &mut undo, &mut tiles);
                } else {
                    for c in &centers {
                        brush.dabs(&pool, &canvas, None, &[*c], &mut undo, &mut tiles);
                    }
                }
                (0..4)
                    .flat_map(|ty| (0..4).map(move |tx| (tx, ty)))
                    .map(|(tx, ty)| canvas.get_layer_tile_data(1, tx, ty))
                    .collect::<Vec<_>>()
            };
            assert!(paint(true) == paint(false));
        }
    }
}
