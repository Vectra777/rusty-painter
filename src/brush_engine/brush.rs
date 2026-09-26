use super::brush_options::BrushOptions;
use crate::{
    brush_engine::{
        brush_options::{BlendMode, PaintingMode, PixelBrushShape},
        dab::{
            PlacedDab, TileBucket, TileRegion, bucket_by_tile, calc_dab_bounds, dab_reaches_tile,
            dispatch_over_buckets, tile_overlap, tile_overlaps_selection,
        },
        hardness::SoftnessSelector,
        masks::{calc_soft_brush_alpha, sample_custom_mask_nn},
        stroke::{StrokeBuffer, StrokeTiles},
    },
    canvas::{
        Canvas,
        blend::{
            StrokeColor, resolve_stroke_erase, resolve_stroke_normal, resolve_stroke_normal_gamma,
            resolve_stroke_normal_simd,
        },
        blend_modes::BlendSpace,
        history::{TileSnapshot, UndoAction},
    },
    selection::SelectionManager,
};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPool;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::sync::Mutex;

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
    /// Scalar reference for one pixel; [`row_kernel`] must match it bit for bit.
    #[cfg(test)]
    fn alpha(&self, dist_sq: f32, dist: f32, t: f32) -> f32 {
        if dist_sq >= self.r_sq {
            return 0.0;
        }
        let mut alpha_factor = super::masks::gaussian_falloff(t, self.hardness);
        if dist > self.fade_start {
            alpha_factor *= 1.0 - (dist - self.fade_start) * self.inv_fade_width;
        }
        alpha_factor.clamp(0.0, 1.0)
    }

    /// Tip alphas for one dab row, for tip columns `mx0..mx0 + out.len()`.
    /// `pdy` is the row's vertical offset from the dab center.
    ///
    /// Runs the AVX2 build of [`row_kernel`] when the CPU has it (8 lanes),
    /// else the baseline build (4 lanes); both are bit-identical.
    /// Columns `0..len` of a row (starting at mask column `mx0`) that can be
    /// inside the circle, i.e. where `pdx² < r² - pdy²`. Widened by a pixel
    /// on each side so float rounding can never cut off a covered pixel.
    fn chord(&self, pdy: f32, frac_x: f32, mx0: usize, len: usize) -> Range<usize> {
        let room = self.r_sq - pdy * pdy;
        if room <= 0.0 {
            return 0..0;
        }
        let half = room.sqrt();
        // pdx(i) = i + offset, as in `row_kernel`.
        let offset = mx0 as f32 - self.r_ceil as f32 + 0.5 - frac_x;
        let lo = (-half - offset).floor() - 1.0;
        let hi = (half - offset).ceil() + 1.0;
        let start = lo.max(0.0) as usize;
        let end = (hi.max(0.0) as usize).min(len);
        start.min(end)..end
    }

    fn row(&self, pdy: f32, frac_x: f32, mx0: usize, out: &mut [f32]) {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2, checked just above.
            unsafe { row_kernel_avx2(self, pdy, frac_x, mx0, out) };
            return;
        }
        row_kernel(self, pdy, frac_x, mx0, out);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn row_kernel_avx2(tip: &GaussianTip, pdy: f32, frac_x: f32, mx0: usize, out: &mut [f32]) {
    row_kernel(tip, pdy, frac_x, mx0, out);
}

/// [`GaussianTip::alpha`] for a run of columns, written as a straight loop
/// with branch-free selects so the compiler vectorizes it to whatever width
/// the enclosing function's target features allow. Each select mirrors the
/// scalar branch (`x > 0 ? x : 0` is exactly SSE `maxps(x, 0)`, etc.), so
/// every lane is bit-identical to [`GaussianTip::alpha`].
#[inline(always)]
fn row_kernel(tip: &GaussianTip, pdy: f32, frac_x: f32, mx0: usize, out: &mut [f32]) {
    let r_ceil = tip.r_ceil as f32;
    let pdy_sq = pdy * pdy;
    let hardness = tip.hardness;
    let soft = hardness < 1.0;
    let soft_span = 1.0 - hardness;
    let base = mx0 as i32;
    for (i, slot) in out.iter_mut().enumerate() {
        let pdx = (base + i as i32) as f32 - r_ceil + 0.5 - frac_x;
        let dist_sq = pdx * pdx + pdy_sq;
        let dist = dist_sq.sqrt();
        let t = dist * tip.inv_radius;
        let mut alpha = 1.0;
        if soft {
            let v = (t - hardness) / soft_span;
            let v = if v > 0.0 { v } else { 0.0 };
            let v = if v < 1.0 { v } else { 1.0 };
            let falloff = 1.0 - v;
            let smooth = falloff * falloff * (3.0 - 2.0 * falloff);
            alpha = if t < hardness { 1.0 } else { smooth };
        }
        let faded = alpha * (1.0 - (dist - tip.fade_start) * tip.inv_fade_width);
        alpha = if dist > tip.fade_start { faded } else { alpha };
        alpha = if alpha > 0.0 { alpha } else { 0.0 };
        alpha = if alpha < 1.0 { alpha } else { 1.0 };
        *slot = if dist_sq < tip.r_sq { alpha } else { 0.0 };
    }
}

/// User-facing brush configuration.
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

/// Shared inputs for painting one batch of dabs into the stroke buffers.
struct BatchCtx<'a> {
    canvas: &'a Canvas,
    selection: Option<&'a SelectionManager>,
    dabs: &'a [PlacedDab],
    buffers: &'a FxHashMap<(usize, usize), Mutex<StrokeBuffer>>,
    r: f32,
    blend_mode: BlendMode,
    /// The document's blending space (gamma documents mix stored values).
    space: BlendSpace,
    color: StrokeColor,
    /// Coverage multiplier when resolving: the stroke opacity in wash mode.
    cap: f32,
    /// Soft brushes use anti-aliased selection edges; pixel brushes keep
    /// hard, pixel-center edges (pixel art).
    antialiased_selection: bool,
    /// The layer's transparency is locked: paint only recolours.
    alpha_lock: bool,
}

/// A whole tile's selection coverage, row by row.
fn tile_selection_coverage(
    selection: &SelectionManager,
    tile_x0: usize,
    tile_y0: usize,
    tile_size: usize,
    antialiased: bool,
) -> Vec<f32> {
    let mut coverage = vec![0.0; tile_size * tile_size];
    let mut inside = vec![false; tile_size];
    for (ly, row) in coverage.chunks_exact_mut(tile_size).enumerate() {
        if antialiased {
            selection.row_coverage(tile_y0 + ly, tile_x0, row);
        } else {
            selection.row_mask(tile_y0 + ly, tile_x0, &mut inside);
            for (c, &i) in row.iter_mut().zip(&inside) {
                *c = if i { 1.0 } else { 0.0 };
            }
        }
    }
    coverage
}

/// Shortest row span worth resolving with [`resolve_stroke_normal_simd`].
const SIMD_RESOLVE_MIN: usize = 16;

/// The range of `out` holding non-zero values (empty if none).
fn nonzero_span(out: &[f32]) -> Range<usize> {
    match out.iter().position(|&a| a > 0.0) {
        Some(first) => first..out.iter().rposition(|&a| a > 0.0).unwrap_or(first) + 1,
        None => 0..0,
    }
}

/// Stamp every tile's dabs into its stroke coverage (`cov += a * (1 - cov)`,
/// in stroke order), then re-resolve the pixels those dabs touched, once per
/// tile. `stamp(dab, gy, x0, out)` writes the dab's alpha, already scaled by
/// the stroke strength, for canvas row `gy` and columns `x0..x0 + out.len()`,
/// and returns the range of `out` that may be non-zero (the rest is zero).
fn paint_batch(
    pool: &ThreadPool,
    ctx: &BatchCtx<'_>,
    buckets: &[TileBucket],
    work_pixels: usize,
    stamp: &(impl Fn(&PlacedDab, usize, usize, &mut [f32]) -> Range<usize> + Sync),
) {
    let tile_size = ctx.canvas.tile_size();
    let draw_tile = |(region, dab_ids): &TileBucket| {
        let tile_x0 = region.tx * tile_size;
        let tile_y0 = region.ty * tile_size;
        if !tile_overlaps_selection(ctx.selection, tile_x0, tile_y0, tile_size) {
            return;
        }
        let Some(buffer) = ctx.buffers.get(&(region.tx, region.ty)) else {
            return;
        };
        let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sel) = ctx.selection
            && buffer.selection.is_none()
        {
            buffer.selection = Some(tile_selection_coverage(
                sel,
                tile_x0,
                tile_y0,
                tile_size,
                ctx.antialiased_selection,
            ));
        }
        let StrokeBuffer {
            coverage,
            selection: selection_coverage,
            ..
        } = &mut *buffer;
        let mut alpha_row = vec![0.0f32; tile_size];
        // Per tile row, the columns [min, max] any dab of this batch actually
        // reached (non-zero alpha). Resolving only those, rather than the
        // union of dab rectangles, skips the rectangles' empty corners and the
        // stroke trail earlier batches already resolved: unchanged pixels
        // that were most of the resolve cost.
        let mut spans = vec![(usize::MAX, 0usize); tile_size];
        let mut touched = false;

        for &i in dab_ids {
            let dab = &ctx.dabs[i];
            if !dab_reaches_tile(dab.center, ctx.r, tile_x0, tile_y0, tile_size) {
                continue;
            }
            let overlap = tile_overlap(&dab.bounds, tile_x0, tile_y0, tile_size);
            let width = overlap.max_x - overlap.min_x + 1;
            for gy in overlap.min_y..=overlap.max_y {
                let alphas = &mut alpha_row[..width];
                // The round dab only covers part of its rectangle's row.
                let span = stamp(dab, gy, overlap.min_x, alphas);
                if span.is_empty() {
                    continue;
                }
                let (first, last) = (span.start, span.end - 1);
                let start = (gy - tile_y0) * tile_size + (overlap.min_x - tile_x0);
                if let Some(sel) = selection_coverage {
                    for (alpha, &s) in alphas[span.clone()]
                        .iter_mut()
                        .zip(&sel[start + first..=start + last])
                    {
                        *alpha *= s;
                    }
                }
                for (cov, &alpha) in coverage[start + first..=start + last]
                    .iter_mut()
                    .zip(&alphas[span])
                {
                    *cov += alpha * (1.0 - *cov);
                }
                let local_x = overlap.min_x - tile_x0;
                let span = &mut spans[gy - tile_y0];
                span.0 = span.0.min(local_x + first);
                span.1 = span.1.max(local_x + last);
                touched = true;
            }
        }

        if !touched {
            return;
        }
        let Some(tile_arc) = ctx.canvas.lock_tile(region.tx, region.ty) else {
            return;
        };
        let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
        let Some(data) = tile.data.as_mut() else {
            return;
        };
        for (row, &(min_x, max_x)) in spans.iter().enumerate() {
            if min_x > max_x {
                continue;
            }
            let range = row * tile_size + min_x..row * tile_size + max_x + 1;
            let (original, coverage) = (
                &buffer.original[range.clone()],
                &buffer.coverage[range.clone()],
            );
            // Canvas position of the span, for the alpha dither.
            let origin = [(tile_x0 + min_x) as u32, (tile_y0 + row) as u32];
            match ctx.blend_mode {
                BlendMode::Normal if ctx.space == BlendSpace::Gamma => resolve_stroke_normal_gamma(
                    original,
                    coverage,
                    &mut data[range.clone()],
                    ctx.color,
                    ctx.cap,
                ),
                BlendMode::Normal => {
                    // SIMD pays off on longer spans; short ones (small dabs)
                    // are cheaper through the plain loop.
                    if range.len() >= SIMD_RESOLVE_MIN {
                        resolve_stroke_normal_simd(
                            original,
                            coverage,
                            &mut data[range.clone()],
                            ctx.color,
                            ctx.cap,
                            origin,
                        )
                    } else {
                        resolve_stroke_normal(
                            original,
                            coverage,
                            &mut data[range.clone()],
                            ctx.color,
                            ctx.cap,
                            origin,
                        )
                    }
                }
                BlendMode::Eraser if ctx.alpha_lock => {}
                BlendMode::Eraser => resolve_stroke_erase(
                    original,
                    coverage,
                    &mut data[range.clone()],
                    ctx.cap,
                    origin,
                ),
            }
            if ctx.alpha_lock {
                for (out, orig) in data[range].iter_mut().zip(original) {
                    *out = crate::canvas::blend::with_alpha_of(*out, orig.a());
                }
            }
        }
        tile.is_empty = false;

        // Report exactly what changed, so the display redraws only that.
        let mut rect: Option<[usize; 4]> = None;
        for (row, &(lo, hi)) in spans.iter().enumerate() {
            if lo > hi {
                continue;
            }
            let r = rect.get_or_insert([lo, row, hi + 1, row + 1]);
            *r = [
                r[0].min(lo),
                r[1].min(row),
                r[2].max(hi + 1),
                r[3].max(row + 1),
            ];
        }
        if let Some(r) = rect {
            buffer.damage = Some(match buffer.damage {
                Some(d) => [
                    d[0].min(r[0]),
                    d[1].min(r[1]),
                    d[2].max(r[2]),
                    d[3].max(r[3]),
                ],
                None => r,
            });
        }
    };
    dispatch_over_buckets(buckets, pool, work_pixels, draw_tile);
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

    /// Paint a batch of dabs (in stroke order) with the current brush.
    ///
    /// Dabs are grouped per tile; each tile accumulates its dabs into the
    /// stroke's coverage buffer in order, then its touched pixels are
    /// resolved once. The result is identical to painting the dabs one by
    /// one, with each tile locked once per batch.
    pub(crate) fn dabs(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let o = &self.brush_options;
        let r = o.diameter / 2.0;
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
        Self::snapshot_tiles(canvas, &regions, undo_action, stroke_tiles);

        let wash = o.painting_mode == PaintingMode::Wash;
        // Build-up scales every dab by opacity; wash instead caps the whole
        // stroke at opacity when resolving.
        let strength =
            o.color.a() as f32 / 255.0 * (o.flow / 100.0) * if wash { 1.0 } else { o.opacity };
        let ctx = BatchCtx {
            canvas,
            selection,
            dabs: &dabs,
            buffers: &stroke_tiles.buffers,
            r,
            blend_mode: o.blend_mode,
            space: canvas.blend_space,
            color: StrokeColor::new(o.color),
            cap: if wash { o.opacity } else { 1.0 },
            antialiased_selection: self.brush_type == BrushType::Soft,
            alpha_lock: canvas
                .layers
                .get(canvas.active_layer_idx)
                .is_some_and(|l| l.alpha_locked),
        };
        let side = (2 * r_ceil + 1).max(1) as usize;
        let work_pixels = dabs.len() * side * side;
        match self.brush_type {
            BrushType::Soft => self.paint_soft(pool, &ctx, &buckets, work_pixels, strength),
            BrushType::Pixel => self.paint_pixel(pool, &ctx, &buckets, work_pixels, strength),
        }
    }

    /// On a tile's first touch this stroke: snapshot it for undo and create
    /// its stroke buffer from the same pre-stroke pixels.
    fn snapshot_tiles(
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
            let key = (region.tx, region.ty);
            stroke_tiles.dirty.insert(key);
            if stroke_tiles.buffers.contains_key(&key) {
                continue;
            }
            let Some(tile_arc) = canvas.lock_layer_tile(layer_idx, region.tx, region.ty) else {
                continue;
            };
            let tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(data) = tile.data.as_ref() else {
                continue;
            };
            undo_action.tiles.push(TileSnapshot {
                tx: region.tx as i32,
                ty: region.ty as i32,
                layer_id,
                x0: 0,
                y0: 0,
                width: tile_size,
                height: tile_size,
                data: data.clone().into(),
            });
            stroke_tiles.buffers.insert(
                key,
                Mutex::new(StrokeBuffer {
                    original: data.clone(),
                    coverage: vec![0.0; tile_size * tile_size],
                    selection: None,
                    damage: None,
                }),
            );
        }
    }

    /// Hard, pixel-aligned dabs.
    fn paint_pixel(
        &self,
        pool: &ThreadPool,
        ctx: &BatchCtx<'_>,
        buckets: &[TileBucket],
        work_pixels: usize,
        strength: f32,
    ) {
        let r = ctx.r;
        let r_sq = r * r;
        let pixel_shape = &self.brush_options.pixel_shape;
        let diameter = self.brush_options.diameter;
        let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
            let dy = gy as f32 + 0.5 - dab.center.y;
            for (i, slot) in out.iter_mut().enumerate() {
                let dx = (x0 + i) as f32 + 0.5 - dab.center.x;
                let (in_shape, alpha_mod) = match pixel_shape {
                    PixelBrushShape::Circle => (dx * dx + dy * dy <= r_sq, 1.0),
                    PixelBrushShape::Square => (dx.abs() <= r && dy.abs() <= r, 1.0),
                    PixelBrushShape::Custom {
                        width,
                        height,
                        data,
                    } => sample_custom_mask_nn(dx, dy, diameter, *width, *height, data),
                };
                *slot = if in_shape {
                    (strength * alpha_mod).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
            nonzero_span(out)
        };
        paint_batch(pool, ctx, buckets, work_pixels, &stamp);
    }

    /// Soft, anti-aliased dabs.
    fn paint_soft(
        &self,
        pool: &ThreadPool,
        ctx: &BatchCtx<'_>,
        buckets: &[TileBucket],
        work_pixels: usize,
        strength: f32,
    ) {
        let o = &self.brush_options;
        let r = ctx.r;
        let r_sq = r * r;
        let hardness_val = (o.hardness / 100.0).clamp(0.0, 1.0);
        let softness_selector = o.softness_selector;
        let softness_curve = &o.softness_curve;
        let pixel_shape = &o.pixel_shape;
        let diameter = o.diameter;
        let anti_aliasing = self.anti_aliasing;

        // 1.5 pixel outer anti-aliasing fade
        let fade_start = (r - 1.5).max(0.0);
        let fade_width = 1.5_f32.min(r);
        let inv_fade_width = if fade_width > 0.0 {
            1.0 / fade_width
        } else {
            0.0
        };

        if anti_aliasing
            && softness_selector == SoftnessSelector::Gaussian
            && matches!(pixel_shape, PixelBrushShape::Circle)
        {
            let tip = GaussianTip {
                r_ceil: r.ceil() as i32,
                r_sq,
                inv_radius: if r > 0.0 { 1.0 / r } else { 0.0 },
                hardness: hardness_val,
                fade_start,
                inv_fade_width,
            };
            let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
                let my = gy as i32 - dab.base_y;
                let pdy = my as f32 - tip.r_ceil as f32 + 0.5 - dab.frac_y;
                let mx0 = (x0 as i32 - dab.base_x) as usize;
                // Only the circle's chord on this row can be non-zero; run the
                // kernel there (bounds widened a pixel, the kernel itself
                // zeroes anything outside the circle) and zero the rest.
                let chord = tip.chord(pdy, dab.frac_x, mx0, out.len());
                out[..chord.start].fill(0.0);
                out[chord.end..].fill(0.0);
                if chord.is_empty() {
                    return chord;
                }
                tip.row(pdy, dab.frac_x, mx0 + chord.start, &mut out[chord.clone()]);
                for alpha in &mut out[chord.clone()] {
                    *alpha *= strength;
                }
                chord
            };
            paint_batch(pool, ctx, buckets, work_pixels, &stamp);
            return;
        }

        let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
            let pdy = gy as f32 + 0.5 - dab.center.y;
            for (i, slot) in out.iter_mut().enumerate() {
                let pdx = (x0 + i) as f32 + 0.5 - dab.center.x;
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
                        let dist_for_aa = match pixel_shape {
                            PixelBrushShape::Circle => dist_sq.sqrt(),
                            PixelBrushShape::Square | PixelBrushShape::Custom { .. } => {
                                pdx.abs().max(pdy.abs())
                            }
                        };
                        if dist_for_aa >= r {
                            0.0
                        } else if dist_for_aa > fade_start {
                            base_alpha_at_pixel
                                * (1.0 - (dist_for_aa - fade_start) * inv_fade_width)
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
                *slot = if alpha_factor <= 0.0 {
                    0.0
                } else {
                    (strength * alpha_factor).clamp(0.0, 1.0)
                };
            }
            nonzero_span(out)
        };
        paint_batch(pool, ctx, buckets, work_pixels, &stamp);
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

    #[test]
    fn pressure_can_drive_opacity_and_restores_the_brush() {
        use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
        use crate::canvas::history::UndoAction;
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let alpha_at = |pressure: f32| {
            let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
            canvas.active_layer_idx = 1;
            let mut brush = Brush::new(12.0, 100.0, Color32::BLACK, 10.0);
            brush.brush_options.pressure_size = false;
            brush.brush_options.pressure_opacity = true;
            let mut undo = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut tiles = StrokeTiles::default();
            let mut stroke = StrokeState::new();
            let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
            stroke.add_point(&mut brush, Vec2::new(32.0, 32.0), pressure, &mut ctx);
            assert_eq!(
                brush.brush_options.opacity, 1.0,
                "opacity restored after the sample"
            );
            assert_eq!(brush.brush_options.diameter, 12.0, "diameter restored");
            canvas.get_layer_tile_data(1, 0, 0).unwrap()[32 * 64 + 32].a()
        };
        let (light, full) = (alpha_at(0.25), alpha_at(1.0));
        assert!(
            light < full,
            "light pressure {light} should paint lighter than full {full}"
        );
        assert!(
            (60..=68).contains(&light),
            "about a quarter opacity, got {light}"
        );
    }

    #[test]
    fn gamma_documents_mix_strokes_as_stored_values() {
        use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
        use crate::canvas::blend_modes::BlendSpace;
        use crate::canvas::history::UndoAction;
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let stroke_on = |space: BlendSpace| {
            // Paint on the (white) background layer itself so the stroke mixes
            // with white.
            let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
            canvas.blend_space = space;
            canvas.active_layer_idx = 0;
            let mut brush = Brush::new(12.0, 100.0, Color32::BLACK, 10.0);
            brush.brush_options.opacity = 0.5;
            let mut undo = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut tiles = StrokeTiles::default();
            let mut stroke = StrokeState::new();
            let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
            stroke.add_point(&mut brush, Vec2::new(32.0, 32.0), 1.0, &mut ctx);
            canvas.get_layer_tile_data(0, 0, 0).unwrap()[32 * 64 + 32].r()
        };
        assert_eq!(stroke_on(BlendSpace::Linear), 188);
        assert_eq!(stroke_on(BlendSpace::Gamma), 128);
    }

    #[test]
    fn stroke_leaving_the_canvas_paints_only_inside() {
        use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
        use crate::canvas::history::UndoAction;
        let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let mut canvas = Canvas::new(256, 256, Color32::WHITE, 64);
        canvas.active_layer_idx = 1;
        let mut brush = Brush::new(10.0, 100.0, Color32::BLACK, 10.0);
        let mut undo = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut tiles = StrokeTiles::default();
        let mut stroke = StrokeState::new();
        let mut ctx = StrokeContext::new(&pool, &canvas, None, &mut undo, &mut tiles);
        // Up through the top edge at x=64, along outside, back in at x=192.
        for p in [
            (64.0, 128.0),
            (64.0, -100.0),
            (192.0, -100.0),
            (192.0, 128.0),
        ] {
            stroke.add_point(&mut brush, Vec2::new(p.0, p.1), 1.0, &mut ctx);
        }
        let painted = |x: i32, y: i32| {
            let data = canvas
                .get_layer_tile_data(1, x / 64, y / 64)
                .unwrap_or_default();
            data.get(((y % 64) * 64 + x % 64) as usize)
                .is_some_and(|p| p.a() > 0)
        };
        assert!(
            painted(64, 2) && painted(192, 2),
            "both crossings reach the edge"
        );
        for x in 80..176 {
            assert!(
                !painted(x, 0),
                "top edge at x={x} painted: stroke was clamped"
            );
        }
    }

    #[test]
    fn chord_never_cuts_off_covered_pixels() {
        // Full-row kernel vs chord-restricted kernel, over many radii,
        // sub-pixel offsets and rows: every non-zero pixel must be inside the
        // chord, and the values inside must be identical.
        for &r in &[0.6_f32, 1.0, 2.3, 7.5, 31.0, 120.25] {
            let tip = GaussianTip {
                r_ceil: r.ceil() as i32,
                r_sq: r * r,
                inv_radius: 1.0 / r,
                hardness: 0.4,
                fade_start: (r - 1.5).max(0.0),
                inv_fade_width: 1.0 / 1.5_f32.min(r),
            };
            let len = (2 * tip.r_ceil + 3) as usize;
            for step in 0..7 {
                let frac = step as f32 / 7.0;
                for my in 0..len {
                    let pdy = my as f32 - tip.r_ceil as f32 + 0.5 - frac;
                    for mx0 in [0usize, 1, 3] {
                        let n = len.saturating_sub(mx0);
                        let mut full = vec![0.0; n];
                        tip.row(pdy, frac, mx0, &mut full);
                        let chord = tip.chord(pdy, frac, mx0, n);
                        let mut part = vec![0.0; n];
                        if !chord.is_empty() {
                            tip.row(pdy, frac, mx0 + chord.start, &mut part[chord.clone()]);
                        }
                        assert_eq!(full, part, "r {r}, frac {frac}, row {my}, mx0 {mx0}");
                    }
                }
            }
        }
    }
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
        SelectionManager::with_shape(Some(crate::selection::new_lasso_shape(star)))
    }

    fn paint_and_checksum_in(
        steps: &[(Brush, Vec<Vec2>)],
        selection: Option<&SelectionManager>,
    ) -> u64 {
        fnv1a(&paint_bytes_in(steps, selection))
    }

    fn paint_bytes_in(
        steps: &[(Brush, Vec<Vec2>)],
        selection: Option<&SelectionManager>,
    ) -> Vec<u8> {
        let canvas = Canvas::new(96, 96, Color32::TRANSPARENT, TILE_SIZE_FOR_TEST);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();

        for (brush, centers) in steps {
            let brush = brush.clone();
            let mut undo_action = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            let mut stroke_tiles = StrokeTiles::default();
            // One batch per step: the goldens were captured painting one dab
            // at a time, so matching them proves batching changes nothing.
            brush.dabs(
                &pool,
                &canvas,
                selection,
                centers,
                &mut undo_action,
                &mut stroke_tiles,
            );
        }

        let mut bytes = Vec::new();
        for ty in 0..2 {
            for tx in 0..2 {
                let data = canvas.get_layer_tile_data(1, tx, ty).unwrap_or_else(|| {
                    vec![Color32::TRANSPARENT; TILE_SIZE_FOR_TEST * TILE_SIZE_FOR_TEST]
                });
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
        assert_eq!(
            checksum, 0xc540cd961ef28f05,
            "GOLDEN_PLACEHOLDER:pixel_dab_output_is_stable"
        );
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
    fn dirty_tiles_track_only_dabs_since_last_drain() {
        let canvas = Canvas::new(96, 96, Color32::TRANSPARENT, TILE_SIZE_FOR_TEST);
        let pool = ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let brush = Brush::new(8.0, 50.0, Color32::BLACK, 25.0);
        let mut undo_action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut stroke_tiles = StrokeTiles::default();

        let first = Vec2::new(16.0, 16.0);
        brush.dabs(
            &pool,
            &canvas,
            None,
            &[first],
            &mut undo_action,
            &mut stroke_tiles,
        );
        stroke_tiles.dirty.clear();

        let second = Vec2::new(80.0, 80.0);
        brush.dabs(
            &pool,
            &canvas,
            None,
            &[second],
            &mut undo_action,
            &mut stroke_tiles,
        );

        assert_eq!(stroke_tiles.dirty, HashSet::from([(1, 1)]));
        let buffered: HashSet<_> = stroke_tiles.buffers.keys().copied().collect();
        assert_eq!(buffered, HashSet::from([(0, 0), (1, 1)]));
    }

    /// Golden-master check for the Soft brush's fast Gaussian-circle path
    /// (anti_aliasing + Gaussian + Circle + Normal blend + no selection).
    #[test]
    fn soft_dab_gaussian_fast_path_output_is_stable() {
        let brush = Brush::new(
            24.0,
            40.0,
            Color32::from_rgba_unmultiplied(30, 30, 200, 255),
            15.0,
        );
        assert!(brush.anti_aliasing);
        assert_eq!(
            brush.brush_options.softness_selector,
            SoftnessSelector::Gaussian
        );
        assert_eq!(brush.brush_options.pixel_shape, PixelBrushShape::Circle);
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0xfcf6cc6ce4d2d650,
            "GOLDEN_PLACEHOLDER:soft_dab_gaussian_fast_path_output_is_stable"
        );
    }

    /// Golden-master check for the Soft brush's general (non-fast-path)
    /// path, forced by a Square tip.
    #[test]
    fn soft_dab_general_path_square_output_is_stable() {
        let mut brush = Brush::new(
            24.0,
            40.0,
            Color32::from_rgba_unmultiplied(200, 200, 30, 255),
            15.0,
        );
        brush.brush_options.pixel_shape = PixelBrushShape::Square;
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0x3be4088e1a6b28ad,
            "GOLDEN_PLACEHOLDER:soft_dab_general_path_square_output_is_stable"
        );
    }

    /// Golden-master check for the Soft brush's general path without
    /// anti-aliasing (hard edges, still goes through calc_soft_brush_alpha).
    #[test]
    fn soft_dab_general_path_no_aa_output_is_stable() {
        let mut brush = Brush::new(
            24.0,
            40.0,
            Color32::from_rgba_unmultiplied(200, 30, 200, 255),
            15.0,
        );
        brush.anti_aliasing = false;
        let checksum = paint_and_checksum(&[(brush, stroke_centers())]);
        assert_eq!(
            checksum, 0x80c2a24b62e25d05,
            "GOLDEN_PLACEHOLDER:soft_dab_general_path_no_aa_output_is_stable"
        );
    }

    /// Golden-master check for the Eraser blend mode, erasing into a
    /// pre-painted solid fill.
    #[test]
    fn soft_dab_eraser_output_is_stable() {
        let fill_brush = Brush::new(
            60.0,
            0.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 255),
            15.0,
        );
        let mut eraser_brush = Brush::new(
            24.0,
            40.0,
            Color32::from_rgba_unmultiplied(0, 0, 0, 255),
            15.0,
        );
        eraser_brush.brush_options.blend_mode = BlendMode::Eraser;

        let checksum = paint_and_checksum(&[
            (fill_brush, vec![Vec2::new(48.0, 48.0)]),
            (eraser_brush, stroke_centers()),
        ]);
        assert_eq!(
            checksum, 0x9ec526431d3a5f6,
            "GOLDEN_PLACEHOLDER:soft_dab_eraser_output_is_stable"
        );
    }

    #[test]
    fn soft_dab_selection_output_is_stable() {
        let brush = Brush::new(
            24.0,
            40.0,
            Color32::from_rgba_unmultiplied(30, 150, 90, 255),
            15.0,
        );
        let checksum = paint_and_checksum_in(&[(brush, stroke_centers())], Some(&star_selection()));
        assert_eq!(
            checksum, 0x43e95c8b28ef7b5c,
            "GOLDEN_PLACEHOLDER:soft_dab_selection_output_is_stable"
        );
    }

    #[test]
    fn selection_only_changes_pixels_on_its_antialiased_edge() {
        let brush = Brush::new(
            24.0,
            40.0,
            Color32::from_rgba_unmultiplied(30, 150, 90, 255),
            15.0,
        );
        let steps = [(brush, stroke_centers())];
        let free = paint_bytes_in(&steps, None);
        let selection = star_selection();
        let masked = paint_bytes_in(&steps, Some(&selection));

        // paint_bytes_in lays tiles out (0,0), (1,0), (0,1), (1,1), 64x64 each.
        let mut checked_inside = 0;
        for tile in 0..4 {
            let (tx, ty) = (tile % 2, tile / 2);
            for ly in 0..64 {
                let mut row = [0.0f32; 64];
                selection.row_coverage(ty * 64 + ly, tx * 64, &mut row);
                for (lx, &cov) in row.iter().enumerate() {
                    let i = (tile * 64 * 64 + ly * 64 + lx) * 4;
                    if cov == 1.0 {
                        assert_eq!(masked[i..i + 4], free[i..i + 4]);
                        checked_inside += 1;
                    } else if cov == 0.0 {
                        assert_eq!(masked[i..i + 4], [0, 0, 0, 0]);
                    }
                }
            }
        }
        assert!(checked_inside > 500);
    }

    #[test]
    fn pixel_dab_selection_output_is_stable() {
        let brush = Brush::new_pixel(18.0, Color32::from_rgba_unmultiplied(90, 30, 150, 255));
        let checksum = paint_and_checksum_in(&[(brush, stroke_centers())], Some(&star_selection()));
        assert_eq!(
            checksum, 0x322bba2014492c15,
            "GOLDEN_PLACEHOLDER:pixel_dab_selection_output_is_stable"
        );
    }

    /// Dab centers off the pixel grid, so sub-pixel placement and rounding
    /// both matter.
    fn fractional_centers() -> Vec<Vec2> {
        (0..30)
            .map(|i| Vec2::new(14.3 + i as f32 * 2.37, 20.7 + (i as f32 * 0.4).sin() * 25.1))
            .collect()
    }

    #[test]
    fn soft_partial_flow_fractional_output_is_stable() {
        let mut brush = Brush::new(
            30.0,
            30.0,
            Color32::from_rgba_unmultiplied(220, 120, 40, 255),
            10.0,
        );
        brush.brush_options.flow = 50.0;
        let checksum = paint_and_checksum(&[(brush, fractional_centers())]);
        assert_eq!(
            checksum, 0x38d00942f73404f9,
            "GOLDEN_PLACEHOLDER:soft_partial_flow_fractional_output_is_stable"
        );
    }

    #[test]
    fn eraser_partial_flow_fractional_output_is_stable() {
        let fill = Brush::new(
            60.0,
            0.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 255),
            15.0,
        );
        let mut eraser = Brush::new(24.0, 40.0, Color32::BLACK, 15.0);
        eraser.brush_options.blend_mode = BlendMode::Eraser;
        eraser.brush_options.flow = 40.0;
        let checksum = paint_and_checksum(&[
            (fill, vec![Vec2::new(48.0, 48.0)]),
            (eraser, fractional_centers()),
        ]);
        assert_eq!(
            checksum, 0x4a01292a66855c9d,
            "GOLDEN_PLACEHOLDER:eraser_partial_flow_fractional_output_is_stable"
        );
    }

    fn max_alpha(bytes: &[u8]) -> u8 {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3])
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn wash_mode_caps_a_stroke_at_its_opacity() {
        let dense: Vec<Vec2> = (0..40)
            .map(|i| Vec2::new(40.0 + i as f32 * 0.5, 48.0))
            .collect();
        let mut brush = Brush::new(30.0, 80.0, Color32::from_rgb(20, 90, 200), 5.0);
        brush.brush_options.opacity = 0.5;
        brush.brush_options.flow = 60.0;

        let build_up = max_alpha(&paint_bytes_in(&[(brush.clone(), dense.clone())], None));
        brush.brush_options.painting_mode = PaintingMode::Wash;
        let wash = max_alpha(&paint_bytes_in(&[(brush, dense)], None));

        assert!(
            build_up > 250,
            "build-up keeps accumulating past opacity: {build_up}"
        );
        assert!(
            (127..=128).contains(&wash),
            "wash tops out at opacity: {wash}"
        );
    }

    #[test]
    fn pixel_brush_honors_opacity_and_flow() {
        let mut brush = Brush::new_pixel(9.0, Color32::from_rgb(200, 40, 40));
        brush.brush_options.opacity = 0.5;
        let half = max_alpha(&paint_bytes_in(
            &[(brush.clone(), vec![Vec2::new(20.5, 20.5)])],
            None,
        ));
        brush.brush_options.opacity = 1.0;
        brush.brush_options.flow = 25.0;
        let quarter = max_alpha(&paint_bytes_in(
            &[(brush, vec![Vec2::new(20.5, 20.5)])],
            None,
        ));
        assert!((127..=128).contains(&half), "{half}");
        assert!((63..=64).contains(&quarter), "{quarter}");
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
        let mut row = [0.0f32; 25];
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
            assert!((alpha - expected).abs() <= 1e-5, "mx={mx}");
        }
    }

    #[test]
    fn gaussian_tip_kernels_match_scalar_reference() {
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
                let side = (2 * tip.r_ceil + 1) as usize;
                for (frac_x, frac_y) in [(0.0, 0.0), (0.3125, 0.9375), (0.5, 0.0625)] {
                    for my in 0..side {
                        let pdy = my as f32 - tip.r_ceil as f32 + 0.5 - frac_y;
                        // Odd starts and lengths exercise vector bodies and tails.
                        let (mx0, len) = (my % 3, side - my % 3);
                        let mut baseline = vec![0.0f32; len];
                        let mut dispatched = vec![0.0f32; len];
                        row_kernel(&tip, pdy, frac_x, mx0, &mut baseline);
                        tip.row(pdy, frac_x, mx0, &mut dispatched);
                        for i in 0..len {
                            let pdx = (mx0 + i) as f32 - tip.r_ceil as f32 + 0.5 - frac_x;
                            let dist_sq = pdx * pdx + pdy * pdy;
                            let dist = dist_sq.sqrt();
                            let expected =
                                tip.alpha(dist_sq, dist, dist * tip.inv_radius).to_bits();
                            assert_eq!(
                                baseline[i].to_bits(),
                                expected,
                                "h={hardness} r={r} my={my} i={i}"
                            );
                            assert_eq!(
                                dispatched[i].to_bits(),
                                expected,
                                "h={hardness} r={r} my={my} i={i}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn batched_dabs_match_one_at_a_time() {
        let centers: Vec<Vec2> = (0..40)
            .map(|i| Vec2::new(30.0 + i as f32 * 1.7, 34.0 + (i as f32 * 0.9).sin() * 20.0))
            .collect();
        let brushes = [
            Brush::new(
                40.0,
                30.0,
                Color32::from_rgba_unmultiplied(20, 90, 200, 180),
                10.0,
            ),
            Brush::new_pixel(9.0, Color32::from_rgba_unmultiplied(200, 40, 40, 255)),
        ];
        for brush in brushes {
            let paint = |batched: bool| {
                let canvas = Canvas::new(128, 128, Color32::TRANSPARENT, 32);
                let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
                let brush = brush.clone();
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
