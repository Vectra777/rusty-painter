//! The brush: its tip (Gaussian, custom image, pixel) and rendering a
//! batch of dabs into canvas tiles in parallel.

use super::brush_options::{BrushOptions, ColorSource};
use crate::{
    brush_engine::{
        brush_options::{BlendMode, PaintingMode, PixelBrushShape},
        dab::{
            PIXELS_PER_THREAD, PlacedDab, TileBucket, TileRegion, bucket_by_tile, calc_dab_bounds,
            dab_reaches_tile, dispatch_over_buckets, tile_overlap, tile_overlaps_selection,
        },
        hardness::SoftnessSelector,
        stroke::{StrokeBuffer, StrokeTiles},
    },
    canvas::{
        Canvas,
        blend::{
            StrokeColor, resolve_stroke_erase, resolve_stroke_general, resolve_stroke_normal,
            resolve_stroke_normal_gamma, resolve_stroke_normal_simd,
        },
        blend_modes::{BlendSpace, LayerBlend},
        history::{TileSnapshot, UndoAction},
    },
    selection::SelectionManager,
};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPool;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::sync::Mutex;

/// Available shapes for how a brush applies paint.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrushType {
    Soft,
    Pixel,
    /// A row of hairs, each painting its own line (see
    /// [`crate::brush_engine::bristle`]).
    Bristle,
    /// Its line, and fine lines to earlier points of the stroke nearby (see
    /// [`crate::brush_engine::sketch`]).
    Sketch,
    /// Parallel lines pinned to the canvas wherever it passes (see
    /// [`crate::brush_engine::hatching`]).
    Hatching,
    /// Each dab a cloud of small particles (see
    /// [`crate::brush_engine::engines::Spray`]).
    Spray,
    /// The tip broken up by a grain, filled more by pressing harder.
    Chalk,
    /// Curves swinging from points a while back to the pen.
    Curve,
    /// One shape in each cell of a grid it passes over.
    Grid,
    /// A normal map: the pen's tilt as the colour.
    TangentNormal,
    /// A swarm pulled along after the pen, each drawing its path.
    Particle,
}

impl BrushType {
    /// Every type, in the order the brush panel lists them.
    pub const ALL: [BrushType; 11] = [
        BrushType::Soft,
        BrushType::Pixel,
        BrushType::Bristle,
        BrushType::Sketch,
        BrushType::Hatching,
        BrushType::Spray,
        BrushType::Chalk,
        BrushType::Curve,
        BrushType::Grid,
        BrushType::TangentNormal,
        BrushType::Particle,
    ];

    /// Draws its own lines or shapes in place of the stroke's dabs.
    pub fn replaces_dabs(self) -> bool {
        matches!(
            self,
            BrushType::Curve | BrushType::Grid | BrushType::Particle
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StabilizerAlgorithm {
    None,
    Simple,
    Dynamic,
    /// Pulled string (lazy mouse): the brush trails the pen on a string.
    String,
    /// The path is smoothed when the pen lifts, and the stroke repainted.
    PostCorrection,
    /// Jitter filtered out by speed: smooth when slow, direct when fast.
    MotionFilter,
}

/// Gaussian circle brush tip: per-pixel alpha as a function of distance from
/// the (sub-pixel quantized) dab center.
struct GaussianTip {
    r_ceil: i32,
    radius: f32,
    r_sq: f32,
    inv_radius: f32,
    hardness: f32,
    /// Krita's anti-aliased edge (see `masks::auto_tip_alpha`): from here
    /// out the falloff's value here fades linearly to nothing at the edge.
    fade_start: f32,
    fade_base: f32,
    inv_fade_width: f32,
}

impl GaussianTip {
    fn new(r: f32, hardness: f32) -> Self {
        let fade_start = (r - 1.0).max(0.0);
        let inv_radius = if r > 0.0 { 1.0 / r } else { 0.0 };
        Self {
            r_ceil: r.ceil() as i32,
            radius: r,
            r_sq: r * r,
            inv_radius,
            hardness,
            fade_start,
            fade_base: super::masks::gaussian_falloff(fade_start * inv_radius, hardness),
            inv_fade_width: if r > fade_start {
                1.0 / (r - fade_start)
            } else {
                0.0
            },
        }
    }

    /// Scalar reference for one pixel; [`row_kernel`] must match it bit for bit.
    #[cfg(test)]
    fn alpha(&self, dist_sq: f32, dist: f32, t: f32) -> f32 {
        if dist_sq >= self.r_sq {
            return 0.0;
        }
        let alpha_factor = if dist > self.fade_start {
            self.fade_base * ((self.radius - dist) * self.inv_fade_width)
        } else {
            super::masks::gaussian_falloff(t, self.hardness)
        };
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

/// `GaussianTip::alpha` for a run of columns, written as a straight loop
/// with branch-free selects so the compiler vectorizes it to whatever width
/// the enclosing function's target features allow. Each select mirrors the
/// scalar branch (`x > 0 ? x : 0` is exactly SSE `maxps(x, 0)`, etc.), so
/// every lane is bit-identical to `GaussianTip::alpha`.
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
        let faded = tip.fade_base * ((tip.radius - dist) * tip.inv_fade_width);
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
    /// The other stabiliser modes' settings.
    pub stabilizer_modes: crate::brush_engine::stabilizer::StabilizerModes,
    /// What changes from dab to dab besides pressure: tip angle and squash,
    /// tapers, speed, randomness. All off by default.
    pub dynamics: crate::brush_engine::dynamics::BrushDynamics,
    /// Inputs driving dab settings, each through its own curve.
    pub inputs: Vec<crate::brush_engine::dynamics::InputMapping>,
    /// Paper grain taking paint away from each dab; `None` for none.
    pub texture: Option<crate::brush_engine::texture::BrushTexture>,
    /// How the paint blends onto the layer (multiply, screen, add…), like a
    /// layer's blend mode but per stroke.
    pub paint_blend: LayerBlend,
    /// Airbrush: dabs per second added where the pen is while it's down,
    /// so paint builds up when it's held still (0 = off).
    pub airbrush_rate: f32,
    /// Dual brush: a second tip that masks this one; `None` for none.
    pub dual: Option<crate::brush_engine::dual::DualTip>,
    /// Watercolour edges: how much the middle of a stroke thins when the
    /// pen lifts, its paint pooling at the rim (0 = off, up to 0.95).
    pub wet_edge: f32,
    /// How wide the pooled rim is, in canvas pixels.
    pub wet_edge_width: f32,
    /// The hairs of a [`BrushType::Bristle`] brush.
    pub bristles: crate::brush_engine::bristle::Bristles,
    /// The joining lines of a [`BrushType::Sketch`] brush.
    pub sketch: crate::brush_engine::sketch::Sketch,
    /// The lines of a [`BrushType::Hatching`] brush.
    pub hatching: crate::brush_engine::hatching::Hatching,
    /// The settings of the spray, chalk, curve, grid, tangent normal and
    /// particle types.
    pub engines: crate::brush_engine::engines::Engines,
    /// Hard edges: tip coverage below this share
    /// (0..1) is dropped and the rest painted at full strength (0 = off).
    pub sharpness: f32,
    /// Hard edges' soft band (0..1): coverage
    /// down to this share below the cut keeps its own strength.
    pub sharpness_softness: f32,
    /// Colour mixing: the brush smudges the paint under it, mixing in its
    /// colour (it then paints through the Smudge tool's engine); `None` for
    /// a plain brush.
    pub mixing: Option<crate::brush_engine::brush_options::Mixing>,
    /// The secondary colour, for input mappings that mix it in (set when
    /// a stroke starts; not part of the brush's settings).
    pub second_color: Color32,
    /// Wash mode: the opacity pen pressure gives the dabs being painted
    /// (set with the pressure while painting; not a setting).
    pub wash_opacity: f32,
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
    /// Where the dabs accumulate.
    target: Target,
    /// The brush's texture, applied to every dab.
    texture: Option<&'a crate::brush_engine::texture::BrushTexture>,
    /// The dabs differ in colour: their colours are accumulated per pixel.
    colored: bool,
    /// How the stroke blends onto the layer.
    mode: LayerBlend,
    /// Resolve with [`resolve_stroke_general`] (a blend mode or per-pixel
    /// colours) rather than the fast single-colour resolves.
    general: bool,
    /// Which tail segment is the newer.
    tail_newer: usize,
    /// The stroke's grain, when the texture is placed (moved, turned...).
    grain: Option<crate::brush_engine::texture::StrokeGrain>,
    /// A dual brush: how its mask combines with the coverage.
    dual: Option<crate::brush_engine::dual::DualMode>,
    /// The dabs paint their tips' own colours.
    tip_colors: bool,
    /// A hatching brush: its lines, over every dab.
    hatch: Option<&'a crate::brush_engine::hatching::Hatching>,
    /// A chalk brush: its grain, over every dab.
    chalk: Option<&'a crate::brush_engine::engines::Chalk>,
    /// Hard edges: the brush's [`Brush::sharpness`] (0 = off) and its
    /// soft band.
    sharpness: f32,
    sharpness_softness: f32,
    /// The batch's stroke strength (before each dab's own), which a dab's
    /// stamped alphas are scaled by.
    strength: f32,
    /// Wash mode (alpha darken): the flow each dab moves the
    /// coverage toward its opacity with; its stamped alphas are then the
    /// tip's coverage alone.
    wash: Option<f32>,
}

/// Where a batch of dabs accumulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// The stroke's coverage, for good.
    Stroke,
    /// A segment (0 or 1) of the redrawable tail (an end taper still to
    /// come): merged into the stroke once it's far enough behind the pen,
    /// or cleared and drawn again, tapered, when the pen lifts.
    Tail(usize),
    /// A dual brush's mask: the second tip's coverage, which the stroke's
    /// is combined with when resolving (nothing is resolved on its own).
    Mask,
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

/// An sRGB value (0..1) in linear light.
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// `a` and `b` mixed, `t` (0..1) of the way to `b`, in unmultiplied sRGB.
fn mix_colors(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (a.to_srgba_unmultiplied(), b.to_srgba_unmultiplied());
    let ch = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
    Color32::from_rgb(ch(0), ch(1), ch(2))
}

/// Whether `m` only turns or mirrors (keeps a circle a circle).
#[inline]
fn is_rigid(m: [f32; 4]) -> bool {
    let [a, b, c, d] = m;
    ((a * a + c * c) - 1.0).abs() < 1e-4
        && ((b * b + d * d) - 1.0).abs() < 1e-4
        && (a * b + c * d).abs() < 1e-4
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
/// Rows a heavy tile is split into to paint in parallel.
const BAND_ROWS: usize = 16;

fn paint_batch(
    pool: &ThreadPool,
    ctx: &BatchCtx<'_>,
    buckets: &[TileBucket],
    work_pixels: usize,
    stamp: &(impl Fn(&PlacedDab, usize, usize, &mut [f32]) -> Range<usize> + Sync),
    color_stamp: Option<&ColorStamp<'_>>,
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
            tail,
            colors,
            tail_colors,
            mask,
            ..
        } = &mut *buffer;
        let mut no_colors = None;
        let (coverage, colors) = match ctx.target {
            Target::Stroke => (coverage, colors),
            Target::Tail(k) => (
                tail[k].get_or_insert_with(|| vec![0.0; tile_size * tile_size]),
                &mut tail_colors[k],
            ),
            Target::Mask => (
                mask.get_or_insert_with(|| vec![0.0; tile_size * tile_size]),
                &mut no_colors,
            ),
        };
        // The stroke's coverage takes the selection; its mask needn't too.
        let selection_coverage = selection_coverage
            .as_deref()
            .filter(|_| ctx.target != Target::Mask);
        let colors = ctx
            .colored
            .then(|| colors.get_or_insert_with(|| vec![[0.0; 3]; tile_size * tile_size]));
        // Per tile row, the columns [min, max] any dab of this batch actually
        // reached (non-zero alpha). Resolving only those, rather than the
        // union of dab rectangles, skips the rectangles' empty corners and the
        // stroke trail earlier batches already resolved: unchanged pixels
        // that were most of the resolve cost.
        let mut spans = vec![(usize::MAX, 0usize); tile_size];
        // What changes each row's alphas after stamping (hard edges,
        // hatching, texture), read once per tile rather than per row.
        let sharpness = ctx.sharpness;
        let sharp = sharpness > 0.0;
        let post_row = sharp || ctx.hatch.is_some() || ctx.chalk.is_some() || ctx.texture.is_some();
        // An imported texture (textures the shape before the strength) and
        // wash (alpha darken), likewise once per tile.
        let krita_texture = ctx.texture.is_some_and(|t| t.krita.is_some());
        let wash = ctx.wash;

        // The dabs over canvas rows `rows` of the tile: `coverage`,
        // `colors` and `spans` are those rows'. Whether any pixel took paint.
        let paint_rows = |rows: Range<usize>,
                          coverage: &mut [f32],
                          mut colors: Option<&mut [[f32; 3]]>,
                          spans: &mut [(usize, usize)]| {
            let mut alpha_row = vec![0.0f32; tile_size];
            let mut color_row = if color_stamp.is_some() {
                vec![[0.0f32; 3]; tile_size]
            } else {
                Vec::new()
            };
            let mut touched = false;
            for &i in dab_ids {
                let dab = &ctx.dabs[i];
                if !dab_reaches_tile(dab.center, dab.reach, tile_x0, tile_y0, tile_size) {
                    continue;
                }
                let overlap = tile_overlap(&dab.bounds, tile_x0, tile_y0, tile_size);
                let width = overlap.max_x - overlap.min_x + 1;
                for gy in overlap.min_y.max(rows.start)..=overlap.max_y.min(rows.end - 1) {
                    let alphas = &mut alpha_row[..width];
                    // The round dab only covers part of its rectangle's row.
                    let span = stamp(dab, gy, overlap.min_x, alphas);
                    if span.is_empty() {
                        continue;
                    }
                    let (first, last) = (span.start, span.end - 1);
                    // In the tile (the selection), and in the band's rows.
                    let in_tile = (gy - tile_y0) * tile_size + (overlap.min_x - tile_x0);
                    let start = in_tile - (rows.start - tile_y0) * tile_size;
                    if let Some(color_stamp) = color_stamp {
                        // From the tip's own alpha, before texture and selection.
                        color_stamp(
                            dab,
                            gy,
                            overlap.min_x + first,
                            &alphas[span.clone()],
                            &mut color_row[..span.len()],
                        );
                    }
                    // One test per row for a plain brush, whatever it skips.
                    if post_row {
                        if sharp {
                            // The tip's coverage (the alpha over the dab's
                            // strength) cut at the threshold, the rest full:
                            // its threshold (1 - the cut) scaled by the dab's
                            // inputs, and a soft band below the cut kept.
                            let full = (ctx.strength * dab.strength).min(1.0);
                            let cut = (1.0 - (1.0 - sharpness) * dab.sharp).clamp(0.0, 1.0) * full;
                            let low = cut * (1.0 - ctx.sharpness_softness);
                            for a in &mut alphas[span.clone()] {
                                *a = if *a > 0.0 && *a >= cut {
                                    full
                                } else if *a <= low {
                                    0.0
                                } else {
                                    *a
                                };
                            }
                        }
                        if let Some(hatch) = ctx.hatch {
                            hatch.apply_row(
                                gy,
                                overlap.min_x + first,
                                dab.hatch,
                                &mut alphas[span.clone()],
                            );
                        }
                        if let Some(chalk) = ctx.chalk {
                            chalk.apply_row(
                                gy,
                                overlap.min_x + first,
                                dab.seed(),
                                &mut alphas[span.clone()],
                            );
                        }
                        if let Some(texture) = ctx.texture {
                            let (x, row) = (overlap.min_x + first, &mut alphas[span.clone()]);
                            // An imported texture textures the tip's shape, before flow and
                            // opacity (a wash's stamps are the shape already).
                            let full = if krita_texture {
                                (ctx.strength * dab.strength).min(1.0)
                            } else {
                                1.0
                            };
                            let unscale = full > 0.0 && full < 1.0;
                            if unscale {
                                row.iter_mut().for_each(|a| *a /= full);
                            }
                            match &ctx.grain {
                                Some(grain) => texture.apply_row_placed(
                                    gy,
                                    x,
                                    row,
                                    dab.texture,
                                    grain,
                                    [dab.center.x, dab.center.y],
                                ),
                                None => texture.apply_row_scaled(gy, x, row, dab.texture),
                            }
                            if unscale {
                                row.iter_mut().for_each(|a| *a *= full);
                            }
                        }
                    }
                    if let Some(sel) = selection_coverage {
                        for (alpha, &s) in alphas[span.clone()]
                            .iter_mut()
                            .zip(&sel[in_tile + first..=in_tile + last])
                        {
                            *alpha *= s;
                        }
                    }
                    // Wash: the colour goes on at the dab's opacity over its
                    // coverage (alpha darken), not at the coverage.
                    if let Some(colors) = colors.as_deref_mut() {
                        // Each dab's colour laid over what's there, like paint.
                        let dst = &mut colors[start + first..=start + last];
                        if wash.is_some() {
                            for a in &mut alphas[span.clone()] {
                                *a *= dab.opacity;
                            }
                        }
                        let alphas = &alphas[span.clone()];
                        let mix = |dst: &mut [f32; 3], c: [f32; 3], alpha: f32| {
                            let keep = 1.0 - alpha;
                            *dst = [
                                c[0] * alpha + dst[0] * keep,
                                c[1] * alpha + dst[1] * keep,
                                c[2] * alpha + dst[2] * keep,
                            ];
                        };
                        if color_stamp.is_some() {
                            for ((dst, &alpha), &c) in dst.iter_mut().zip(alphas).zip(&color_row) {
                                mix(dst, c, alpha);
                            }
                        } else {
                            for (dst, &alpha) in dst.iter_mut().zip(alphas) {
                                mix(dst, dab.color, alpha);
                            }
                        }
                    }
                    let covered = coverage[start + first..=start + last].iter_mut();
                    match wash {
                        Some(flow) => {
                            // The alphas carry the opacity already when the
                            // dabs differ in colour (above).
                            let carried = colors.is_some();
                            alpha_darken(covered, &alphas[span], dab, flow * dab.flow, carried);
                        }
                        None => {
                            for (cov, &alpha) in covered.zip(&alphas[span]) {
                                *cov += alpha * (1.0 - *cov);
                            }
                        }
                    }
                    let local_x = overlap.min_x - tile_x0;
                    let span = &mut spans[gy - rows.start];
                    span.0 = span.0.min(local_x + first);
                    span.1 = span.1.max(local_x + last);
                    touched = true;
                }
            }
            touched
        };
        // A tile under many large dabs is most of a batch's work: its rows
        // in bands, in parallel (each pixel still takes the dabs in order).
        let work: usize = dab_ids
            .iter()
            .map(|&i| {
                let o = tile_overlap(&ctx.dabs[i].bounds, tile_x0, tile_y0, tile_size);
                (o.max_x + 1).saturating_sub(o.min_x) * (o.max_y + 1).saturating_sub(o.min_y)
            })
            .sum();
        let colors = colors.map(|c| c.as_mut_slice());
        let touched = if work >= 2 * PIXELS_PER_THREAD && tile_size >= 2 * BAND_ROWS {
            let mut bands = Vec::new();
            let (mut coverage, mut colors, mut spans) = (&mut coverage[..], colors, &mut spans[..]);
            let mut y = tile_y0;
            while !spans.is_empty() {
                let n = BAND_ROWS.min(spans.len());
                let (cov, rest) = std::mem::take(&mut coverage).split_at_mut(n * tile_size);
                coverage = rest;
                let col = colors.take().map(|c| {
                    let (band, rest) = c.split_at_mut(n * tile_size);
                    colors = Some(rest);
                    band
                });
                let (sp, rest) = std::mem::take(&mut spans).split_at_mut(n);
                spans = rest;
                bands.push((y..y + n, cov, col, sp));
                y += n;
            }
            bands
                .into_par_iter()
                .map(|(rows, cov, col, sp)| paint_rows(rows, cov, col, sp))
                .reduce(|| false, |a, b| a || b)
        } else {
            paint_rows(tile_y0..tile_y0 + tile_size, coverage, colors, &mut spans)
        };

        if !touched {
            return;
        }
        if ctx.target == Target::Mask {
            // Resolved later, with the brush the mask belongs to.
            for (row, &(lo, hi)) in spans.iter().enumerate() {
                if lo <= hi {
                    grow_rect(&mut buffer.mask_dirty, [lo, row, hi + 1, row + 1]);
                }
            }
            return;
        }
        if let Target::Tail(k) = ctx.target {
            // Remember where the segment is, to merge or clear it later.
            for (row, &(lo, hi)) in spans.iter().enumerate() {
                if lo <= hi {
                    grow_rect(&mut buffer.tail_rect[k], [lo, row, hi + 1, row + 1]);
                }
            }
        }
        resolve_spans(ctx, *region, &mut buffer, &spans);
    };
    dispatch_over_buckets(buckets, pool, work_pixels, draw_tile);
}

/// Wash: one dab's row of tip coverage `alphas` into `coverage` as an
/// alpha darken (the "creamy" variant): the coverage moves toward the
/// dab's opacity by the tip's coverage, at `flow`, and never past it; the
/// stroke's average opacity holds it up while pressure eases. With
/// `carried` the alphas carry the dab's opacity already. Out of line: the
/// usual build-up loop stays small.
#[inline(never)]
fn alpha_darken<'a>(
    coverage: impl Iterator<Item = &'a mut f32>,
    alphas: &[f32],
    dab: &PlacedDab,
    flow: f32,
    carried: bool,
) {
    let (op, avg) = (dab.opacity, dab.average);
    let inv_op = if carried { 1.0 / op.max(1e-6) } else { 1.0 };
    for (cov, &a) in coverage.zip(alphas) {
        let m = (a * inv_op).min(1.0);
        let dst = *cov;
        let full = if avg > op {
            if avg > dst {
                let src = m * op;
                src + (avg - src) * (dst / avg)
            } else {
                dst
            }
        } else if op > dst {
            dst + (op - dst) * m
        } else {
            dst
        };
        *cov = dst + (full - dst) * flow;
    }
}

/// The colours of a dab painting its tip's own: `(dab, gy, x0, alphas,
/// out)` writes the colours (in the document's blend space, unmultiplied)
/// of the pixels whose stamped alphas are `alphas`, from column `x0`.
type ColorStamp<'a> = dyn Fn(&PlacedDab, usize, usize, &[f32], &mut [[f32; 3]]) + Sync + 'a;

/// One stretch of a ribbon brush's stroke: from `p0` to `p1`, with the unit
/// normals there (`n0`, `n1`, towards the bottom of the picture), the
/// half-widths (`w0`, `w1`) and the position along the repeating picture
/// (`u0`, `u1`, in picture lengths).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RibbonSeg {
    pub p0: Vec2,
    pub p1: Vec2,
    pub n0: Vec2,
    pub n1: Vec2,
    pub w0: f32,
    pub w1: f32,
    pub u0: f32,
    pub u1: f32,
}

/// Grow `rect` (`[x0, y0, x1, y1)`) to include `r`.
fn grow_rect(rect: &mut Option<[usize; 4]>, r: [usize; 4]) {
    *rect = Some(match *rect {
        Some(d) => [
            d[0].min(r[0]),
            d[1].min(r[1]),
            d[2].max(r[2]),
            d[3].max(r[3]),
        ],
        None => r,
    });
}

/// The unmultiplied colour of each pixel of `range` in a stroke whose dabs
/// differ in colour: the stroke's, then the older and the newer tail
/// segment's laid over it, in painting order.
fn stroke_colors(
    buffer: &StrokeBuffer,
    range: Range<usize>,
    tail_newer: usize,
    out: &mut Vec<[f32; 3]>,
) {
    out.clear();
    for i in range {
        let mut a = buffer.coverage[i];
        let mut c = buffer.colors.as_ref().map_or([0.0; 3], |c| c[i]);
        for k in [1 - tail_newer, tail_newer] {
            let (Some(tail), Some(tc)) = (&buffer.tail[k], &buffer.tail_colors[k]) else {
                continue;
            };
            let (ta, t) = (tail[i], tc[i]);
            let keep = 1.0 - ta;
            c = [t[0] + c[0] * keep, t[1] + c[1] * keep, t[2] + c[2] * keep];
            a = ta + a * keep;
        }
        let inv = if a > 0.0 { 1.0 / a } else { 0.0 };
        out.push(c.map(|v| (v * inv).clamp(0.0, 1.0)));
    }
}

/// Re-resolve a tile's pixels on `spans` (per row, the columns `[min,
/// max]`; `min > max` for none) from its original pixels and the stroke's
/// coverage (with the tail's, if there is one), and record the damage.
#[inline]
fn resolve_spans(
    ctx: &BatchCtx<'_>,
    region: TileRegion,
    buffer: &mut StrokeBuffer,
    spans: &[(usize, usize)],
) {
    let Some(tile_arc) = ctx.canvas.lock_tile(region.tx, region.ty) else {
        return;
    };
    let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(data) = tile.data.as_mut() else {
        return;
    };
    resolve_spans_in(ctx, region, buffer, spans, data);
    tile.is_empty = false;
}

/// [`resolve_spans`] into a tile's pixels the caller holds locked.
#[inline]
fn resolve_spans_in(
    ctx: &BatchCtx<'_>,
    region: TileRegion,
    buffer: &mut StrokeBuffer,
    spans: &[(usize, usize)],
    data: &mut [Color32],
) {
    let tile_size = ctx.canvas.tile_size();
    let (tile_x0, tile_y0) = (region.tx * tile_size, region.ty * tile_size);
    // The stroke and its tail together: dabs combine the same in any order.
    let mut combined = Vec::new();
    let mut combined_colors: Vec<[f32; 3]> = Vec::new();
    // The same for the whole tile: decided once, not per row.
    let has_tail = buffer.tail.iter().any(Option::is_some);
    let general = ctx.general && ctx.blend_mode == BlendMode::Normal;
    for (row, &(min_x, max_x)) in spans.iter().enumerate() {
        if min_x > max_x {
            continue;
        }
        let range = row * tile_size + min_x..row * tile_size + max_x + 1;
        let original = &buffer.original[range.clone()];
        let coverage = if has_tail {
            combined.clear();
            combined.extend_from_slice(&buffer.coverage[range.clone()]);
            for tail in buffer.tail.iter().flatten() {
                for (c, &t) in combined.iter_mut().zip(&tail[range.clone()]) {
                    *c += t * (1.0 - *c);
                }
            }
            &combined[..]
        } else {
            &buffer.coverage[range.clone()]
        };
        // A dual brush: paint only where its second tip reached too.
        let masked;
        let coverage = match ctx.dual {
            Some(mode) => {
                let mask = buffer.mask.as_ref().map(|m| &m[range.clone()]);
                masked = coverage
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| mode.apply(c, mask.map_or(0.0, |m| m[i])))
                    .collect::<Vec<f32>>();
                &masked[..]
            }
            None => coverage,
        };
        // Canvas position of the span, for the alpha dither.
        let origin = [(tile_x0 + min_x) as u32, (tile_y0 + row) as u32];
        if general {
            let colors = ctx.colored.then(|| {
                stroke_colors(buffer, range.clone(), ctx.tail_newer, &mut combined_colors);
                &combined_colors[..]
            });
            resolve_stroke_general(
                original,
                coverage,
                colors,
                &mut data[range.clone()],
                ctx.color,
                ctx.cap,
                ctx.mode,
                ctx.space,
                origin,
            );
            if ctx.alpha_lock {
                for (out, orig) in data[range].iter_mut().zip(original) {
                    *out = crate::canvas::blend::with_alpha_of(*out, orig.a());
                }
            }
            continue;
        }
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

    // Report exactly what changed, so the display redraws only that.
    let mut rect: Option<[usize; 4]> = None;
    for (row, &(lo, hi)) in spans.iter().enumerate() {
        if lo <= hi {
            grow_rect(&mut rect, [lo, row, hi + 1, row + 1]);
        }
    }
    if let Some(r) = rect {
        grow_rect(&mut buffer.damage, r);
    }
}

impl Brush {
    /// Whether any dynamics are on (fixed ones or input mappings).
    pub fn has_dynamics(&self) -> bool {
        self.dynamics.is_active() || !self.inputs.is_empty()
    }

    /// Whether the dabs' colours can differ (colour randomness, colour
    /// tips, inputs driving the colour).
    pub fn varies_color(&self) -> bool {
        matches!(self.brush_type, BrushType::TangentNormal | BrushType::Grid)
            || self.dynamics.random.has_color()
            || self.paints_tip_colors()
            || self.brush_options.color_source != ColorSource::Plain
            || self.inputs.iter().any(|m| m.setting.is_color())
    }

    /// Whether dabs can differ from one another (dynamics, several tips),
    /// so each is planned on its own.
    pub fn varies_per_dab(&self) -> bool {
        self.has_dynamics()
            || self.brush_options.tip_count() > 1
            || !matches!(self.brush_type, BrushType::Soft | BrushType::Pixel)
            || self.paints_tip_colors()
            || self.brush_options.color_source != ColorSource::Plain
    }

    /// How far from its centre a dab can paint, generously (for copying it
    /// across the canvas's edges with wrap-around): turned tips' corners,
    /// dynamics growing it, a bristle brush's hairs.
    pub fn wrap_reach(&self) -> f32 {
        let spread = match self.brush_type {
            BrushType::Bristle => self.bristles.spread.max(1.0) + self.bristles.thickness,
            BrushType::Sketch => self.sketch.reach,
            BrushType::Curve => 6.0,
            BrushType::Particle => 8.0,
            BrushType::Grid => 1.0 + self.engines.grid.cell / self.brush_options.diameter.max(1.0),
            _ => 1.0,
        };
        self.brush_options.diameter * 1.5 * spread + 4.0
    }

    /// The brush lays its image tip along the stroke as a ribbon.
    pub fn is_ribbon(&self) -> bool {
        self.brush_options.placement == crate::brush_engine::brush_options::Placement::Ribbon
            && matches!(self.brush_options.pixel_shape, PixelBrushShape::Custom(_))
    }

    /// The dabs paint their tips' own colours (smooth image tips only).
    pub fn paints_tip_colors(&self) -> bool {
        self.brush_options.paints_tip_colors()
            && self.anti_aliasing
            && self.brush_type != BrushType::Pixel
    }

    /// The tip turns with the stroke's direction (a bristle brush's hairs
    /// always lie across it).
    pub fn follows_stroke(&self) -> bool {
        self.dynamics.tip.follow_stroke || self.brush_type == BrushType::Bristle
    }

    /// The stroke smoothing this brush asks for.
    pub fn stabilizer_settings(&self) -> crate::brush_engine::stabilizer::StabilizerSettings {
        crate::brush_engine::stabilizer::StabilizerSettings {
            algorithm: self.stabilizer_algorithm,
            strength: self.stabilizer,
            mass: self.stabilizer_mass,
            drag: self.stabilizer_drag,
            modes: self.stabilizer_modes,
            view_scale: 1.0,
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
            stabilizer_modes: Default::default(),
            is_changed: false,
            dynamics: Default::default(),
            inputs: Vec::new(),
            texture: None,
            paint_blend: LayerBlend::Normal,
            airbrush_rate: 0.0,
            dual: None,
            wet_edge: 0.0,
            wet_edge_width: 6.0,
            bristles: Default::default(),
            sketch: Default::default(),
            hatching: Default::default(),
            engines: Default::default(),
            sharpness: 0.0,
            sharpness_softness: 0.0,
            mixing: None,
            second_color: Color32::WHITE,
            wash_opacity: 1.0,
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
            stabilizer_modes: Default::default(),
            is_changed: false,
            dynamics: Default::default(),
            inputs: Vec::new(),
            texture: None,
            paint_blend: LayerBlend::Normal,
            airbrush_rate: 0.0,
            dual: None,
            wet_edge: 0.0,
            wet_edge_width: 6.0,
            bristles: Default::default(),
            sketch: Default::default(),
            hatching: Default::default(),
            engines: Default::default(),
            sharpness: 0.0,
            sharpness_softness: 0.0,
            mixing: None,
            second_color: Color32::WHITE,
            wash_opacity: 1.0,
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
        self.dabs_oriented(
            pool,
            canvas,
            selection,
            centers,
            None,
            undo_action,
            stroke_tiles,
        );
    }

    /// [`Self::dabs`] with a tip orientation per dab (mirror copies turn a
    /// custom tip with them).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dabs_oriented(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        orients: Option<&[[f32; 4]]>,
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);
        let tile_size = canvas.tile_size();
        let dabs: Vec<PlacedDab> = centers
            .iter()
            .enumerate()
            .filter_map(|(i, &center)| {
                let bounds = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size)?;
                let mut dab = PlacedDab::new(center, bounds, r);
                if let Some(o) = orients.and_then(|o| o.get(i)) {
                    dab.orient = *o;
                    dab.rigid = is_rigid(*o);
                }
                Some(dab)
            })
            .collect();
        self.paint_placed(
            pool,
            canvas,
            selection,
            dabs,
            Target::Stroke,
            undo_action,
            stroke_tiles,
        );
    }

    /// [`Self::dabs_oriented`] with each dab varied by its dynamics
    /// (`vars[i]` for dab `i`; mirror copies share their original's): its
    /// size, strength and the tip's turn and squash.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dabs_varied(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        vars: &[crate::brush_engine::dynamics::DabVar],
        orients: Option<&[[f32; 4]]>,
        target: Target,
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        use crate::brush_engine::dynamics::compose;
        let base_r = self.brush_options.diameter / 2.0;
        // A bristle brush: each dab is one small dab per hair.
        let hair_dabs;
        let (centers, vars, orients) = if self.brush_type == BrushType::Bristle {
            hair_dabs = self.hair_dabs(centers, vars, orients);
            (&hair_dabs.0[..], &hair_dabs.1[..], None)
        } else if self.brush_type == BrushType::Spray {
            hair_dabs = self.spray_dabs(centers, vars);
            (&hair_dabs.0[..], &hair_dabs.1[..], None)
        } else {
            (centers, vars, orients)
        };
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);
        let tile_size = canvas.tile_size();
        let colored = self.varies_color();
        let linear = canvas.blend_space == BlendSpace::Linear;
        // How far past its radius a turned tip reaches: a square's (or an
        // image's) corners.
        let tips = self.brush_options.tip_shapes();
        let corner_reach = tips
            .iter()
            .map(|shape| match shape {
                PixelBrushShape::Circle => 1.0,
                PixelBrushShape::Square => std::f32::consts::SQRT_2,
                PixelBrushShape::Custom(tip) => tip.corner_reach(),
            })
            .fold(1.0, f32::max);
        let last_tip = (tips.len() - 1) as u8;
        let dabs: Vec<PlacedDab> = centers
            .iter()
            .enumerate()
            .filter_map(|(i, &center)| {
                let var = vars.get(i % vars.len().max(1)).copied().unwrap_or_default();
                let r = (base_r * var.scale).max(0.25);
                if var.strength <= 0.0 || base_r * var.scale < 0.1 {
                    return None;
                }
                let mirror = orients.and_then(|o| o.get(i)).copied();
                let orient = match mirror {
                    Some(m) => compose(var.orient, m),
                    None => var.orient,
                };
                let upright = orient == crate::brush_engine::dynamics::IDENTITY;
                let reach = if upright { r } else { r * corner_reach };
                let bounds = calc_dab_bounds(center, reach, canvas_w, canvas_h, tile_size)?;
                let mut dab = PlacedDab::new(center, bounds, r);
                dab.orient = orient;
                dab.rigid = is_rigid(orient);
                dab.reach = reach;
                dab.strength = var.strength;
                dab.tip = var.tip.min(last_tip);
                dab.hatch = var.hatch;
                dab.hardness = var.hardness;
                dab.texture = var.texture;
                dab.sharp = var.sharpness;
                dab.soft = crate::brush_engine::dab::soft_level(var.softness);
                dab.flow = var.flow;
                dab.lightness = var.lightness;
                if colored {
                    let own = var.base.map_or(self.brush_options.color, |c| {
                        let [r, g, b] = c.map(|v| (v * 255.0).round() as u8);
                        Color32::from_rgb(r, g, b)
                    });
                    let base = if var.mix > 0.0 {
                        mix_colors(own, self.second_color, var.mix)
                    } else {
                        own
                    };
                    let srgb = crate::brush_engine::dynamics::shift_hsv(base, var.hsv)
                        .map(|c| c * var.darken.clamp(0.0, 1.0));
                    dab.color = if linear {
                        srgb.map(srgb_to_linear)
                    } else {
                        srgb
                    };
                }
                Some(dab)
            })
            .collect();
        self.paint_placed(
            pool,
            canvas,
            selection,
            dabs,
            target,
            undo_action,
            stroke_tiles,
        );
    }

    /// A spray brush's dabs: each of `centers` (with its variation) as a
    /// cloud of particles, the same for the same place.
    fn spray_dabs(
        &self,
        centers: &[Vec2],
        vars: &[crate::brush_engine::dynamics::DabVar],
    ) -> (Vec<Vec2>, Vec<crate::brush_engine::dynamics::DabVar>) {
        use crate::brush_engine::dynamics::{compose, tip_orientation};
        let base_r = self.brush_options.diameter * 0.5;
        let spray = &self.engines.spray;
        let mut out = (Vec::new(), Vec::new());
        for (i, &center) in centers.iter().enumerate() {
            let var = vars.get(i % vars.len().max(1)).copied().unwrap_or_default();
            let seed = center.x.to_bits() ^ center.y.to_bits().rotate_left(11) ^ i as u32;
            for p in spray.particles(seed, base_r * var.scale) {
                out.0.push(center + p.offset);
                let mut v = var;
                v.scale *= p.scale;
                if p.angle != 0.0 {
                    v.orient = compose(var.orient, tip_orientation(p.angle, 1.0));
                }
                out.1.push(v);
            }
        }
        out
    }

    /// A bristle brush's dabs, one per hair of each of `centers` (with its
    /// variation and mirror copy): the hairs' centres and variations.
    fn hair_dabs(
        &self,
        centers: &[Vec2],
        vars: &[crate::brush_engine::dynamics::DabVar],
        orients: Option<&[[f32; 4]]>,
    ) -> (Vec<Vec2>, Vec<crate::brush_engine::dynamics::DabVar>) {
        use crate::brush_engine::dynamics::{DabVar, IDENTITY, compose};
        let b = &self.bristles;
        let hairs = b.hairs();
        let base_r = (self.brush_options.diameter / 2.0).max(0.25);
        let mut out_centers = Vec::with_capacity(centers.len() * hairs.len());
        let mut out_vars = Vec::with_capacity(out_centers.capacity());
        for (i, &center) in centers.iter().enumerate() {
            let var = vars.get(i % vars.len().max(1)).copied().unwrap_or_default();
            let orient = match orients.and_then(|o| o.get(i)) {
                Some(&m) => compose(var.orient, m),
                None => var.orient,
            };
            // Tip frame → canvas: the inverse of `orient`.
            let [a, bb, c, d] = orient;
            let det = a * d - bb * c;
            let inv = if det.abs() > 1e-9 {
                [d / det, -bb / det, -c / det, a / det]
            } else {
                IDENTITY
            };
            let spread = base_r * var.scale * b.spread;
            for hair in &hairs {
                let ink = b.ink_left(hair, var.along);
                if ink <= 0.0 {
                    continue;
                }
                let (tx, ty) = (hair.offset.x * spread, hair.offset.y * spread);
                let offset = Vec2::new(inv[0] * tx + inv[1] * ty, inv[2] * tx + inv[3] * ty);
                out_centers.push(center + offset);
                let hair_r = (b.thickness * 0.5 * hair.thickness).max(0.3);
                out_vars.push(DabVar {
                    scale: hair_r / base_r,
                    strength: var.strength * hair.strength * ink,
                    orient: IDENTITY,
                    ..var
                });
            }
        }
        (out_centers, out_vars)
    }

    /// Merge tail segment `k` into the stroke for good. The pixels don't
    /// change (they already show it), so nothing is resolved.
    pub(crate) fn merge_tail(&self, canvas: &Canvas, stroke_tiles: &mut StrokeTiles, k: usize) {
        let tile_size = canvas.tile_size();
        for key in std::mem::take(&mut stroke_tiles.tail_tiles[k]) {
            let Some(buffer) = stroke_tiles.buffers.get(&key) else {
                continue;
            };
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            let (Some([x0, y0, x1, y1]), Some(tail)) =
                (buffer.tail_rect[k].take(), buffer.tail[k].take())
            else {
                continue;
            };
            let tail_colors = buffer.tail_colors[k].take();
            let buffer = &mut *buffer;
            if tail_colors.is_some() && buffer.colors.is_none() {
                // All the stroke's paint here came through the tail so far.
                buffer.colors = Some(vec![[0.0; 3]; tile_size * tile_size]);
            }
            for row in y0..y1 {
                let range = row * tile_size + x0..row * tile_size + x1;
                if let (Some(tc), Some(colors)) = (&tail_colors, buffer.colors.as_mut()) {
                    // The segment was painted over the stroke.
                    for i in range.clone() {
                        let keep = 1.0 - tail[i];
                        let (t, c) = (tc[i], &mut colors[i]);
                        *c = [t[0] + c[0] * keep, t[1] + c[1] * keep, t[2] + c[2] * keep];
                    }
                }
                for (c, &t) in buffer.coverage[range.clone()].iter_mut().zip(&tail[range]) {
                    *c += t * (1.0 - *c);
                }
            }
        }
    }

    /// Take both tail segments back off the canvas (see [`Target::Tail`]).
    pub(crate) fn clear_tails(&self, canvas: &Canvas, stroke_tiles: &mut StrokeTiles) {
        let keys: std::collections::HashSet<(usize, usize)> = stroke_tiles
            .tail_tiles
            .iter_mut()
            .flat_map(std::mem::take)
            .collect();
        if keys.is_empty() {
            return;
        }
        let ctx = self.batch_ctx(
            canvas,
            None,
            &[],
            &stroke_tiles.buffers,
            Target::Stroke,
            stroke_tiles.tail_newer,
            stroke_tiles.grain,
        );
        let tile_size = canvas.tile_size();
        for key in keys {
            let Some(buffer) = stroke_tiles.buffers.get(&key) else {
                continue;
            };
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            let mut rect: Option<[usize; 4]> = None;
            for k in 0..2 {
                if let Some(r) = buffer.tail_rect[k].take() {
                    grow_rect(&mut rect, r);
                }
            }
            buffer.tail = [None, None];
            buffer.tail_colors = [None, None];
            let Some([x0, y0, x1, y1]) = rect else {
                continue;
            };
            let mut spans = vec![(usize::MAX, 0usize); tile_size];
            for span in &mut spans[y0..y1] {
                *span = (x0, x1 - 1);
            }
            // The pixels as before the stroke (resolving skips pixels the
            // stroke doesn't cover, and the tail's may no longer be), then
            // the stroke again: under one lock, so the display never sees
            // the stroke missing in between.
            if let Some(tile) = canvas.lock_tile(key.0, key.1) {
                let mut tile = tile.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(data) = tile.data.as_mut() {
                    for row in y0..y1 {
                        let range = row * tile_size + x0..row * tile_size + x1;
                        data[range.clone()].copy_from_slice(&buffer.original[range]);
                    }
                    let region = TileRegion {
                        tx: key.0,
                        ty: key.1,
                    };
                    resolve_spans_in(&ctx, region, &mut buffer, &spans, data);
                }
            }
            stroke_tiles.dirty.insert(key);
        }
    }

    /// Re-resolve, with this (dual) brush, the pixels its mask's latest
    /// dabs reached: where the stroke already has paint, more of it shows.
    pub(crate) fn resolve_mask_changes(
        &self,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let tile_size = canvas.tile_size();
        let keys: Vec<_> = stroke_tiles.mask_tiles.drain().collect();
        let ctx = self.batch_ctx(
            canvas,
            selection,
            &[],
            &stroke_tiles.buffers,
            Target::Stroke,
            stroke_tiles.tail_newer,
            stroke_tiles.grain,
        );
        for key in keys {
            let Some(buffer) = stroke_tiles.buffers.get(&key) else {
                continue;
            };
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            let Some([x0, y0, x1, y1]) = buffer.mask_dirty.take() else {
                continue;
            };
            let mut spans = vec![(usize::MAX, 0usize); tile_size];
            for span in &mut spans[y0..y1] {
                *span = (x0, x1 - 1);
            }
            let region = TileRegion {
                tx: key.0,
                ty: key.1,
            };
            resolve_spans(&ctx, region, &mut buffer, &spans);
            stroke_tiles.dirty.insert(key);
        }
    }

    /// Watercolour edges, when the pen lifts: thin the middle of the whole
    /// stroke, keeping its rim (see [`crate::brush_engine::wet_edge`]),
    /// and show the result.
    pub(crate) fn apply_wet_edges(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        stroke_tiles: &mut StrokeTiles,
    ) {
        use rayon::prelude::*;
        if self.wet_edge <= 0.0 || stroke_tiles.buffers.is_empty() {
            return;
        }
        let ts = canvas.tile_size();
        let radius = (self.wet_edge_width.round() as usize).clamp(1, ts);
        let pad = radius;
        let side = ts + 2 * pad;
        // Every tile's coverage as the stroke left it, so each tile's blur
        // reads its neighbours unchanged.
        let before: FxHashMap<(usize, usize), Vec<f32>> = stroke_tiles
            .buffers
            .iter()
            .map(|(&k, b)| {
                (
                    k,
                    b.lock().unwrap_or_else(|e| e.into_inner()).coverage.clone(),
                )
            })
            .collect();
        let keys: Vec<_> = before.keys().copied().collect();
        let strength = self.wet_edge;
        let after: Vec<((usize, usize), Vec<f32>)> = pool.install(|| {
            keys.par_iter()
                .map(|&(tx, ty)| {
                    let mut patch = vec![0.0f32; side * side];
                    for py in 0..side {
                        // Canvas row of this patch row, and its tile.
                        let gy = (ty * ts + py) as isize - pad as isize;
                        if gy < 0 {
                            continue;
                        }
                        let (sty, ly) = (gy as usize / ts, gy as usize % ts);
                        for px in 0..side {
                            let gx = (tx * ts + px) as isize - pad as isize;
                            if gx < 0 {
                                continue;
                            }
                            let (stx, lx) = (gx as usize / ts, gx as usize % ts);
                            if let Some(c) = before.get(&(stx, sty)) {
                                patch[py * side + px] = c[ly * ts + lx];
                            }
                        }
                    }
                    let out = crate::brush_engine::wet_edge::wet_edge_tile(
                        &patch, side, pad, radius, strength,
                    );
                    ((tx, ty), out)
                })
                .collect()
        });
        let ctx = self.batch_ctx(
            canvas,
            selection,
            &[],
            &stroke_tiles.buffers,
            Target::Stroke,
            stroke_tiles.tail_newer,
            stroke_tiles.grain,
        );
        for (key, coverage) in after {
            let Some(buffer) = stroke_tiles.buffers.get(&key) else {
                continue;
            };
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            // Where the stroke has paint (the rest stays as it was).
            let spans: Vec<(usize, usize)> = coverage
                .chunks_exact(ts)
                .map(|row| {
                    let first = row.iter().position(|&c| c > 0.0);
                    let last = row.iter().rposition(|&c| c > 0.0);
                    match (first, last) {
                        (Some(a), Some(b)) => (a, b),
                        _ => (usize::MAX, 0),
                    }
                })
                .collect();
            buffer.coverage = coverage;
            let region = TileRegion {
                tx: key.0,
                ty: key.1,
            };
            resolve_spans(&ctx, region, &mut buffer, &spans);
            stroke_tiles.dirty.insert(key);
        }
    }

    /// What a batch needs to paint and resolve with this brush.
    #[allow(clippy::too_many_arguments)]
    fn batch_ctx<'a>(
        &'a self,
        canvas: &'a Canvas,
        selection: Option<&'a SelectionManager>,
        dabs: &'a [PlacedDab],
        buffers: &'a FxHashMap<(usize, usize), Mutex<StrokeBuffer>>,
        target: Target,
        tail_newer: usize,
        grain: crate::brush_engine::texture::StrokeGrain,
    ) -> BatchCtx<'a> {
        let o = &self.brush_options;
        let tip_colors = self.paints_tip_colors();
        let colored = self.varies_color();
        let wash = o.painting_mode == PaintingMode::Wash;
        BatchCtx {
            canvas,
            selection,
            dabs,
            buffers,
            r: o.diameter / 2.0,
            blend_mode: o.blend_mode,
            space: canvas.blend_space,
            color: StrokeColor::new(o.color),
            cap: if wash { o.opacity } else { 1.0 },
            antialiased_selection: self.brush_type != BrushType::Pixel,
            alpha_lock: canvas
                .layers
                .get(canvas.active_layer_idx)
                .is_some_and(|l| l.alpha_locked),
            target,
            texture: self.texture.as_ref(),
            colored,
            mode: self.paint_blend,
            general: colored || self.paint_blend != LayerBlend::Normal,
            tail_newer,
            grain: self
                .texture
                .as_ref()
                .is_some_and(|t| t.placement.is_active())
                .then_some(grain),
            dual: self.dual.as_ref().map(|d| d.mode),
            tip_colors,
            hatch: (self.brush_type == BrushType::Hatching).then_some(&self.hatching),
            chalk: (self.brush_type == BrushType::Chalk).then_some(&self.engines.chalk),
            sharpness: self.sharpness,
            sharpness_softness: self.sharpness_softness.clamp(0.0, 1.0),
            strength: 1.0,
            wash: None,
        }
    }

    /// Snapshot and paint dabs already placed on the canvas.
    #[allow(clippy::too_many_arguments)]
    fn paint_placed(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        dabs: Vec<PlacedDab>,
        target: Target,
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        self.paint_prepared(
            pool,
            canvas,
            selection,
            dabs,
            target,
            undo_action,
            stroke_tiles,
            |brush, ctx, buckets, work_pixels, strength| match brush.brush_type {
                BrushType::Pixel => brush.paint_pixel(pool, ctx, buckets, work_pixels, strength),
                _ => brush.paint_soft(pool, ctx, buckets, work_pixels, strength),
            },
        );
    }

    /// Snapshot the tiles `dabs` reach, then `paint(brush, ctx, buckets,
    /// work_pixels, strength)` them.
    #[allow(clippy::too_many_arguments)]
    fn paint_prepared(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        dabs: Vec<PlacedDab>,
        target: Target,
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
        paint: impl FnOnce(&Self, &BatchCtx<'_>, &[TileBucket], usize, f32),
    ) {
        let _ = pool;
        if dabs.is_empty() {
            return;
        }
        let o = &self.brush_options;
        if let Some(collect) = stroke_tiles.collect.as_mut() {
            // Laid down by the caller: the stroke's own dabs (a tail is
            // painted for good, a mask has no place there).
            if target != Target::Mask {
                let base = o.color.a() as f32 / 255.0 * (o.flow / 100.0) * o.opacity;
                collect.extend(dabs.into_iter().map(|dab| {
                    crate::brush_engine::stroke::CollectedDab {
                        strength: base * dab.strength * dab.flow,
                        dab,
                    }
                }));
            }
            return;
        }
        let mut dabs = dabs;
        // Wash, as an alpha darken: each dab's opacity (pressure,
        // colour alpha, dynamics) and the stroke's running average of it,
        // in stroke order; its stamp is then just the tip's coverage.
        let wash = o.painting_mode == PaintingMode::Wash && target != Target::Mask;
        if wash {
            let alpha = o.color.a() as f32 / 255.0;
            for dab in &mut dabs {
                let opacity = (self.wash_opacity * alpha * dab.strength).clamp(0.0, 1.0);
                let average = match stroke_tiles.wash_average {
                    Some(a) if a >= opacity => 0.1 * opacity + 0.9 * a,
                    // Rising, or the stroke's first dab (the average starts
                    // as the first opacity).
                    _ => opacity,
                };
                stroke_tiles.wash_average = Some(average);
                (dab.opacity, dab.average, dab.strength) = (opacity, average, 1.0);
            }
        } else {
            // Build-up: flow scales the dab like opacity.
            for dab in &mut dabs {
                dab.strength *= dab.flow;
            }
        }

        let buckets = bucket_by_tile(&dabs);
        let regions: Vec<TileRegion> = buckets.iter().map(|(region, _)| *region).collect();
        Self::snapshot_tiles(canvas, &regions, undo_action, stroke_tiles);

        // Build-up scales every dab by opacity; wash instead caps the whole
        // stroke at opacity when resolving, its dabs stamping their tip's
        // coverage alone (flow and opacity go into the alpha darken).
        let strength = if wash {
            1.0
        } else {
            o.color.a() as f32 / 255.0 * (o.flow / 100.0) * o.opacity
        };
        match target {
            Target::Tail(k) => {
                stroke_tiles.tail_tiles[k].extend(regions.iter().map(|r| (r.tx, r.ty)));
            }
            Target::Mask => stroke_tiles
                .mask_tiles
                .extend(regions.iter().map(|r| (r.tx, r.ty))),
            Target::Stroke => {}
        }
        let mut ctx = self.batch_ctx(
            canvas,
            selection,
            &dabs,
            &stroke_tiles.buffers,
            target,
            stroke_tiles.tail_newer,
            stroke_tiles.grain,
        );
        ctx.strength = strength;
        ctx.wash = wash.then_some((o.flow / 100.0).clamp(0.0, 1.0));
        let work_pixels: usize = dabs
            .iter()
            .map(|d| {
                let side = (2.0 * d.reach.ceil() + 1.0).max(1.0) as usize;
                side * side
            })
            .sum();
        paint(self, &ctx, &buckets, work_pixels, strength);
    }

    /// Lay the brush's image tip along `segs` (a ribbon), repeated along
    /// the stroke, its height across it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ribbon(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        segs: &[RibbonSeg],
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let PixelBrushShape::Custom(tip) = &self.brush_options.pixel_shape else {
            return;
        };
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);
        let tile_size = canvas.tile_size();
        let dabs: Vec<PlacedDab> = segs
            .iter()
            .enumerate()
            .filter_map(|(i, seg)| {
                let center = (seg.p0 + seg.p1) * 0.5;
                let reach = (seg.p1 - seg.p0).length() * 0.5 + seg.w0.max(seg.w1) + 1.5;
                let bounds = calc_dab_bounds(center, reach, canvas_w, canvas_h, tile_size)?;
                let mut dab = PlacedDab::new(center, bounds, reach);
                dab.seg = i as u32;
                Some(dab)
            })
            .collect();
        let tip = tip.clone();
        self.paint_prepared(
            pool,
            canvas,
            selection,
            dabs,
            Target::Stroke,
            undo_action,
            stroke_tiles,
            |_, ctx, buckets, work_pixels, strength| {
                let (tw, th) = (tip.width as f32, tip.height as f32);
                let sampler_of = |seg: &RibbonSeg| tip.ribbon_sampler(seg.w0 + seg.w1);
                // Where pixel (x, y) falls on the picture: texel coordinates,
                // or `None` off this segment.
                let texel = |seg: &RibbonSeg, x: f32, y: f32| {
                    let d = seg.p1 - seg.p0;
                    let len = d.length();
                    if len < 1e-4 {
                        return None;
                    }
                    let dir = d / len;
                    let rel = Vec2::new(x, y) - seg.p0;
                    let t = rel.dot(dir) / len;
                    if !(0.0..1.0).contains(&t) {
                        return None;
                    }
                    let n = (seg.n0 + (seg.n1 - seg.n0) * t).normalized();
                    let w = (seg.w0 + (seg.w1 - seg.w0) * t).max(0.25);
                    let v = (rel - dir * (t * len)).dot(n) / w;
                    let margin = 1.0 + 2.0 / th.max(1.0);
                    if v.abs() > margin {
                        return None;
                    }
                    let u = (seg.u0 + (seg.u1 - seg.u0) * t).rem_euclid(1.0);
                    Some((u * tw, (v * 0.5 + 0.5) * th))
                };
                let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
                    let seg = &segs[dab.seg as usize];
                    let sampler = sampler_of(seg);
                    let y = gy as f32 + 0.5;
                    for (i, slot) in out.iter_mut().enumerate() {
                        let x = (x0 + i) as f32 + 0.5;
                        *slot = texel(seg, x, y).map_or(0.0, |(tx, ty)| {
                            tip.sample_texel(&sampler, tx, ty) * strength
                        });
                    }
                    nonzero_span(out)
                };
                let linear = ctx.space == BlendSpace::Linear;
                let srgb =
                    crate::brush_engine::dynamics::shift_hsv(self.brush_options.color, [0.0; 3]);
                let brush_color = if linear {
                    srgb.map(srgb_to_linear)
                } else {
                    srgb
                };
                let color_stamp = |dab: &PlacedDab,
                                   gy: usize,
                                   x0: usize,
                                   alphas: &[f32],
                                   out: &mut [[f32; 3]]| {
                    let seg = &segs[dab.seg as usize];
                    let sampler = sampler_of(seg);
                    let y = gy as f32 + 0.5;
                    for (i, (o, &a)) in out.iter_mut().zip(alphas).enumerate() {
                        let x = (x0 + i) as f32 + 0.5;
                        *o = match texel(seg, x, y) {
                            Some((tx, ty)) if tip.has_colors() => {
                                let c = tip.color_texel(&sampler, tx, ty, a / strength.max(1e-6));
                                if linear { c.map(srgb_to_linear) } else { c }
                            }
                            _ => brush_color,
                        };
                    }
                };
                let color_stamp: Option<&ColorStamp<'_>> = if ctx.tip_colors {
                    Some(&color_stamp)
                } else {
                    None
                };
                paint_batch(pool, ctx, buckets, work_pixels, &stamp, color_stamp);
            },
        );
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
            stroke_tiles.save_for_checkpoint(key, canvas);
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
                    tail: [None, None],
                    tail_rect: [None, None],
                    colors: None,
                    tail_colors: [None, None],
                    mask: None,
                    mask_dirty: None,
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
        let tips = self.brush_options.tip_shapes();
        let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
            let pixel_shape = &tips[dab.tip as usize];
            let r = dab.r;
            let r_sq = r * r;
            let strength = (strength * dab.strength).min(1.0);
            let upright = dab.upright();
            let dy = gy as f32 + 0.5 - dab.center.y;
            for (i, slot) in out.iter_mut().enumerate() {
                let dx = (x0 + i) as f32 + 0.5 - dab.center.x;
                let (tx, ty) = if upright {
                    (dx, dy)
                } else {
                    dab.tip_offset(dx, dy)
                };
                let (in_shape, alpha_mod) = match pixel_shape {
                    PixelBrushShape::Circle => (tx * tx + ty * ty <= r_sq, 1.0),
                    PixelBrushShape::Square => (tx.abs() <= r && ty.abs() <= r, 1.0),
                    PixelBrushShape::Custom(tip) => {
                        let (tx, ty) = dab.tip_offset(dx, dy);
                        let v = tip.sample_nearest(tx, ty, r);
                        (v > 0.0, v)
                    }
                };
                *slot = if in_shape {
                    (strength * alpha_mod).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
            nonzero_span(out)
        };
        paint_batch(
            pool,
            ctx,
            buckets,
            work_pixels,
            &stamp,
            self.source_stamp(ctx).as_deref(),
        );
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
        let soft = SoftTip::new(self, ctx.dabs);
        let (hardness_val, softness_selector, anti_aliasing) = (
            soft.hardness_val,
            soft.softness_selector,
            soft.anti_aliasing,
        );
        let (custom, auto_on) = (soft.custom, soft.auto_on);
        let full = crate::brush_engine::dab::SOFT_LEVELS;
        let pixel_shape = &o.pixel_shape;
        let tips = &soft.tips;
        let source_stamp = self.source_stamp(ctx);

        // Any tip, any dab: per pixel, in the tip's (turned, squashed) frame.
        let general = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
            soft.row(dab, gy, x0, out, strength)
        };

        // Smooth image tips: a whole row at a time, the mip levels chosen
        // once per row rather than per pixel.
        if anti_aliasing && custom {
            let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
                let PixelBrushShape::Custom(tip) = &tips[dab.tip as usize] else {
                    unreachable!("extra tips go with an image tip");
                };
                let sampler = tip.sampler(dab.r);
                let strength = (strength * dab.strength).min(1.0);
                let pdy = gy as f32 + 0.5 - dab.center.y;
                let pdx = x0 as f32 + 0.5 - dab.center.x;
                let [a, b, c, d] = dab.orient;
                tip.row(
                    &sampler,
                    (a * pdx + b * pdy, c * pdx + d * pdy),
                    (a, c),
                    out,
                );
                for v in out.iter_mut() {
                    *v *= strength;
                }
                nonzero_span(out)
            };
            let linear = ctx.space == BlendSpace::Linear;
            // A lightness or gradient map paints from the brush colours.
            let mapping = o.tip_mapping;
            let srgb = |c: Color32| crate::brush_engine::dynamics::shift_hsv(c, [0.0; 3]);
            let (brush_srgb, second_srgb) = (srgb(o.color), srgb(self.second_color));
            let color_stamp =
                |dab: &PlacedDab, gy: usize, x0: usize, alphas: &[f32], out: &mut [[f32; 3]]| {
                    let PixelBrushShape::Custom(tip) = &tips[dab.tip as usize] else {
                        unreachable!("extra tips go with an image tip");
                    };
                    let sampler = tip.sampler(dab.r);
                    let strength = (strength * dab.strength).min(1.0);
                    let pdy = gy as f32 + 0.5 - dab.center.y;
                    let pdx = x0 as f32 + 0.5 - dab.center.x;
                    let [a, b, c, d] = dab.orient;
                    if tip.has_colors() {
                        tip.color_row(
                            &sampler,
                            (a * pdx + b * pdy, c * pdx + d * pdy),
                            (a, c),
                            alphas,
                            strength,
                            out,
                        );
                        if mapping != crate::brush_engine::brush_options::TipMapping::Colors {
                            // Lightness strength: the picture's grey pulled
                            // toward mid grey (the plain colour) at less.
                            let k = dab.lightness.clamp(0.0, 1.0);
                            let lightness = mapping
                                == crate::brush_engine::brush_options::TipMapping::Lightness;
                            for c in out.iter_mut() {
                                if lightness && k < 1.0 {
                                    *c = c.map(|v| 0.5 + (v - 0.5) * k);
                                }
                                *c = mapping.map(*c, brush_srgb, second_srgb);
                            }
                        }
                        if linear {
                            for c in out.iter_mut() {
                                *c = c.map(srgb_to_linear);
                            }
                        }
                    } else {
                        // A grey tip among colour ones paints the brush colour.
                        out.fill(dab.color);
                    }
                };
            let color_stamp: Option<&ColorStamp<'_>> = if ctx.tip_colors {
                Some(&color_stamp)
            } else {
                source_stamp.as_deref()
            };
            paint_batch(pool, ctx, buckets, work_pixels, &stamp, color_stamp);
            return;
        }

        if anti_aliasing
            && softness_selector == SoftnessSelector::Gaussian
            && matches!(pixel_shape, PixelBrushShape::Circle)
            && !auto_on
            && source_stamp.is_none()
        {
            let tip_for = GaussianTip::new;
            let batch_tip = tip_for(ctx.r, hardness_val);
            let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
                // Turning a round tip changes nothing; squashing it does.
                // Small tips are supersampled.
                if !dab.rigid || super::masks::supersamples(dab.r, true) > 1 {
                    return general(dab, gy, x0, out);
                }
                let own;
                let tip = if dab.r == ctx.r && dab.hardness == 0.0 && dab.soft == full {
                    &batch_tip
                } else {
                    own = tip_for(
                        dab.r,
                        (hardness_val + dab.hardness).clamp(0.0, 1.0) * dab.softness(),
                    );
                    &own
                };
                let strength = (strength * dab.strength).min(1.0);
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
            paint_batch(pool, ctx, buckets, work_pixels, &stamp, None);
            return;
        }
        paint_batch(
            pool,
            ctx,
            buckets,
            work_pixels,
            &general,
            source_stamp.as_deref(),
        );
    }

    /// Per-pixel colours from the colour source (random each pixel, a
    /// pattern), in the document's blend space; `None` for a source that
    /// colours whole dabs.
    fn source_stamp<'a>(&'a self, ctx: &BatchCtx<'_>) -> Option<Box<ColorStamp<'a>>> {
        if !self.brush_options.color_source.per_pixel() || !ctx.colored {
            return None;
        }
        let linear = ctx.space == BlendSpace::Linear;
        let to_space = |v: f32| if linear { srgb_to_linear(v) } else { v };
        Some(match &self.brush_options.color_source {
            ColorSource::Pattern { pattern, scale } => {
                // The brush colour to the secondary (in sRGB) by the
                // pattern, in RAMP steps, in the blend space.
                const RAMP: usize = 1024;
                let srgb = |c: Color32| crate::brush_engine::dynamics::shift_hsv(c, [0.0; 3]);
                let (brush, second) = (srgb(self.brush_options.color), srgb(self.second_color));
                let ramp: Vec<[f32; 3]> = (0..RAMP)
                    .map(|i| {
                        let v = i as f32 / (RAMP - 1) as f32;
                        std::array::from_fn(|k| to_space(brush[k] + (second[k] - brush[k]) * v))
                    })
                    .collect();
                let inv_scale = 1.0 / scale.max(0.01);
                Box::new(
                    move |_: &PlacedDab, gy: usize, x0: usize, _: &[f32], out: &mut [[f32; 3]]| {
                        let y = gy as f32 + 0.5;
                        for (i, c) in out.iter_mut().enumerate() {
                            let v = pattern.at((x0 + i) as f32 + 0.5, y, inv_scale);
                            *c = ramp[((v * (RAMP - 1) as f32 + 0.5) as usize).min(RAMP - 1)];
                        }
                    },
                )
            }
            _ => {
                // A random byte per channel, through its value in the
                // blend space.
                let levels: [f32; 256] = std::array::from_fn(|i| to_space(i as f32 / 255.0));
                Box::new(
                    move |dab: &PlacedDab,
                          gy: usize,
                          x0: usize,
                          _: &[f32],
                          out: &mut [[f32; 3]]| {
                        let seed = dab.seed();
                        for (i, c) in out.iter_mut().enumerate() {
                            let h = super::brush_options::hash3(seed, (x0 + i) as u32, gy as u32);
                            *c = [
                                levels[(h & 0xFF) as usize],
                                levels[((h >> 8) & 0xFF) as usize],
                                levels[((h >> 16) & 0xFF) as usize],
                            ];
                        }
                    },
                )
            }
        })
    }
}

/// What painting any tip takes, worked out once for a batch of dabs (see
/// [`SoftTip::row`]): soft round and square tips, auto tips, image tips.
pub(crate) struct SoftTip<'a> {
    hardness_val: f32,
    softness_selector: SoftnessSelector,
    curve_lut: Option<crate::brush_engine::hardness::CurveLut>,
    /// Softer dabs (a Softness input): a falloff for each level used.
    soft_luts: Vec<Option<crate::brush_engine::hardness::CurveLut>>,
    tips: std::borrow::Cow<'a, [PixelBrushShape]>,
    anti_aliasing: bool,
    custom: bool,
    auto: crate::brush_engine::brush_options::AutoTip,
    auto_on: bool,
    grainy: bool,
    spikes: Option<crate::brush_engine::brush_options::Spikes>,
}

impl<'a> SoftTip<'a> {
    /// For `brush`'s tips, painting `dabs`.
    pub(crate) fn new(brush: &'a Brush, dabs: &[PlacedDab]) -> Self {
        let o = &brush.brush_options;
        let anti_aliasing_flag = brush.anti_aliasing;
        let hardness_val = (o.hardness / 100.0).clamp(0.0, 1.0);
        let softness_selector = o.softness_selector;
        let curve_lut = (softness_selector == SoftnessSelector::Curve)
            .then(|| crate::brush_engine::hardness::CurveLut::new(&o.softness_curve));
        // Softer dabs (a Softness input): a falloff for each level used.
        let full = crate::brush_engine::dab::SOFT_LEVELS;
        let soft_luts: Vec<Option<crate::brush_engine::hardness::CurveLut>> =
            if softness_selector == SoftnessSelector::Curve && dabs.iter().any(|d| d.soft < full) {
                let mut used = [false; crate::brush_engine::dab::SOFT_LEVELS as usize];
                for d in dabs {
                    if let Some(u) = used.get_mut(d.soft as usize) {
                        *u = true;
                    }
                }
                used.iter()
                    .enumerate()
                    .map(|(level, &used)| {
                        used.then(|| {
                            let s = level as f32 / full as f32;
                            crate::brush_engine::hardness::CurveLut::new(
                                &o.softening.falloff(&o.softness_curve, s),
                            )
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
        let lut_for = |dab: &PlacedDab| {
            soft_luts
                .get(dab.soft as usize)
                .and_then(Option::as_ref)
                .or(curve_lut.as_ref())
        };
        let pixel_shape = &o.pixel_shape;
        let tips = o.tip_shapes();
        let anti_aliasing = anti_aliasing_flag;
        let custom = matches!(pixel_shape, PixelBrushShape::Custom(_));
        // Spikes, fades, density and randomness (round and square tips).
        let auto = o.auto_tip;
        let auto_on = auto.is_active() && !custom;
        let grainy = auto_on && (auto.density < 1.0 || auto.randomness > 0.0);
        let spikes = auto_on.then(|| auto.spikes()).flatten();

        let _ = (pixel_shape, &lut_for);
        Self {
            hardness_val,
            softness_selector,
            curve_lut,
            soft_luts,
            tips,
            anti_aliasing,
            custom,
            auto,
            auto_on,
            grainy,
            spikes,
        }
    }

    /// A plain round soft dab (a turn changes nothing, no squash, no auto
    /// tip options, the Gaussian falloff): its hardness, for a caller that
    /// works it out more cheaply itself.
    pub(crate) fn plain_round(&self, dab: &PlacedDab) -> Option<f32> {
        (matches!(self.tips[dab.tip as usize], PixelBrushShape::Circle)
            && self.softness_selector == SoftnessSelector::Gaussian
            && !self.auto_on
            && dab.rigid
            && dab.soft == crate::brush_engine::dab::SOFT_LEVELS)
            .then(|| (self.hardness_val + dab.hardness).clamp(0.0, 1.0))
    }

    /// One row of `dab`'s coverage (times `strength`), from canvas pixel
    /// `x0` on row `gy`, into `out`: any tip, in its turned and squashed
    /// frame. Returns where it's not zero.
    #[inline]
    pub(crate) fn row(
        &self,
        dab: &PlacedDab,
        gy: usize,
        x0: usize,
        out: &mut [f32],
        strength: f32,
    ) -> Range<usize> {
        // A smooth image tip: the row at once, its mip level chosen once.
        // (Painting takes its own path there: this is for other callers.)
        if self.custom
            && self.anti_aliasing
            && let PixelBrushShape::Custom(tip) = &self.tips[dab.tip as usize]
        {
            let sampler = tip.sampler(dab.r);
            let strength = (strength * dab.strength).min(1.0);
            let pdy = gy as f32 + 0.5 - dab.center.y;
            let pdx = x0 as f32 + 0.5 - dab.center.x;
            let [a, b, c, d] = dab.orient;
            tip.row(
                &sampler,
                (a * pdx + b * pdy, c * pdx + d * pdy),
                (a, c),
                out,
            );
            for v in out.iter_mut() {
                *v *= strength;
            }
            return nonzero_span(out);
        }
        let (hardness_val, softness_selector, anti_aliasing) = (
            self.hardness_val,
            self.softness_selector,
            self.anti_aliasing,
        );
        let (auto, auto_on, grainy, custom) = (self.auto, self.auto_on, self.grainy, self.custom);
        let spikes = &self.spikes;
        let lut_for = |dab: &PlacedDab| {
            self.soft_luts
                .get(dab.soft as usize)
                .and_then(Option::as_ref)
                .or(self.curve_lut.as_ref())
        };
        let pixel_shape = &self.tips[dab.tip as usize];
        let r = dab.r;
        let strength = (strength * dab.strength).min(1.0);
        // A fade takes the Softness input (as Krita's does); otherwise
        // the hardness does.
        let fade = auto_on && auto.has_fade();
        let hardness_val =
            (hardness_val + dab.hardness).clamp(0.0, 1.0) * if fade { 1.0 } else { dab.softness() };
        let ratio = if spikes.is_some() { dab.ratio() } else { 1.0 };
        let fade_k = auto.fade_coeffs(dab.softness());
        let inv_r = 1.0 / r;
        let seed = dab.seed();
        let softness_curve = lut_for(dab);
        let turned = custom || !dab.upright();
        let falloff = |t: f32| match softness_selector {
            SoftnessSelector::Gaussian => super::masks::gaussian_falloff(t, hardness_val),
            SoftnessSelector::Curve => softness_curve.map_or(1.0, |c| c.at(t)),
        };
        // A custom tip turns with its mirror copy; any tip with its
        // dynamics.
        let alpha_at = |pdx: f32, pdy: f32| {
            let (pdx, pdy) = if turned {
                dab.tip_offset(pdx, pdy)
            } else {
                (pdx, pdy)
            };
            match pixel_shape {
                // The mask's own edges are smooth already (sampled, not
                // clipped).
                PixelBrushShape::Custom(tip) if anti_aliasing => tip.sample(pdx, pdy, r),
                PixelBrushShape::Custom(tip) => tip.sample_nearest(pdx, pdy, r),
                shape => {
                    let (pdx, pdy) = match &spikes {
                        Some(spikes) => spikes.fold((pdx, pdy), ratio),
                        None => (pdx, pdy),
                    };
                    let a = super::masks::auto_tip_alpha(
                        (pdx, pdy),
                        r,
                        matches!(shape, PixelBrushShape::Square),
                        softness_selector,
                        falloff,
                        anti_aliasing,
                    );
                    if fade && a > 0.0 {
                        a * super::brush_options::AutoTip::fade_with(
                            pdx * inv_r,
                            pdy * inv_r,
                            fade_k,
                        )
                    } else {
                        a
                    }
                }
            }
        };
        // Small anti-aliased round and square tips: several samples a
        // pixel, as Krita takes them.
        let samples = if matches!(pixel_shape, PixelBrushShape::Custom(_)) {
            1
        } else {
            super::masks::supersamples(r, anti_aliasing)
        };
        let offset = |s: usize| (s as f32 + 0.5) / samples as f32 - 0.5;
        let pdy_canvas = gy as f32 + 0.5 - dab.center.y;
        for (i, slot) in out.iter_mut().enumerate() {
            let pdx_canvas = (x0 + i) as f32 + 0.5 - dab.center.x;
            let alpha_factor = if samples == 1 {
                alpha_at(pdx_canvas, pdy_canvas)
            } else {
                let mut sum = 0.0;
                for sy in 0..samples {
                    for sx in 0..samples {
                        sum += alpha_at(pdx_canvas + offset(sx), pdy_canvas + offset(sy));
                    }
                }
                sum / (samples * samples) as f32
            };
            let alpha_factor = if grainy && alpha_factor > 0.0 {
                alpha_factor * auto.grain(seed, x0 + i, gy)
            } else {
                alpha_factor
            };
            *slot = if alpha_factor <= 0.0 {
                0.0
            } else {
                (strength * alpha_factor).clamp(0.0, 1.0)
            };
        }
        nonzero_span(out)
    }
}

/// Named preset that can be displayed in the UI and cloned into the active brush.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub brush: Brush,
    /// Where a preset the user saved or imported is kept; `None` for the
    /// built-in ones.
    pub file: Option<std::path::PathBuf>,
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
            let tip = GaussianTip::new(r, 0.4);
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
            checksum, 0x2e19e339e81f9a29,
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
            checksum, 0x73e7076fcdc50db1,
            "GOLDEN_PLACEHOLDER:soft_dab_general_path_square_output_is_stable"
        );
    }

    /// Golden-master check for the Soft brush's general path without
    /// anti-aliasing (hard edges, the falloff kept inside, as in Krita).
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
            checksum, 0x3add87818d40766a,
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
            checksum, 0x26f16f7573c9fa96,
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
            checksum, 0x58c2a1b16f52a5a8,
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
            checksum, 0x6c7f6feffff7bc06,
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
            checksum, 0xc844f31153ed347a,
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
        let r = 12.0;
        let hardness = 0.2;
        let tip = GaussianTip::new(r, hardness);
        let (frac_x, frac_y) = (0.25, 0.5);
        let my = 12usize;
        let pdy = my as f32 - 12.0 + 0.5 - frac_y;
        let mut row = [0.0f32; 25];
        tip.row(pdy, frac_x, 0, &mut row);

        for (mx, &alpha) in row.iter().enumerate() {
            let pdx = mx as f32 - 12.0 + 0.5 - frac_x;
            let soft = |t| super::super::masks::gaussian_falloff(t, hardness);
            let expected = super::super::masks::auto_tip_alpha(
                (pdx, pdy),
                r,
                false,
                SoftnessSelector::Gaussian,
                soft,
                true,
            );
            assert!((alpha - expected).abs() <= 1e-5, "mx={mx}");
        }
    }

    #[test]
    fn gaussian_tip_kernels_match_scalar_reference() {
        for hardness in [0.0, 0.2, 0.5, 0.99, 1.0] {
            for r in [0.6_f32, 3.0, 12.0, 40.5] {
                let tip = GaussianTip::new(r, hardness);
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

    #[test]
    fn a_tile_painted_in_bands_matches_it_painted_whole() {
        // Big dabs close together: a batch puts far more than a thread's
        // share on each tile, so its rows are painted in bands; one dab at
        // a time never does.
        let centers: Vec<Vec2> = (0..30)
            .map(|i| Vec2::new(60.0 + i as f32 * 2.0, 64.0 + (i as f32 * 0.4).sin() * 8.0))
            .collect();
        let mut picture = Brush::new(
            110.0,
            50.0,
            Color32::from_rgba_unmultiplied(200, 120, 30, 160),
            2.0,
        );
        picture.brush_options.pixel_shape = PixelBrushShape::Custom(std::sync::Arc::clone(
            &crate::brush_engine::tip::builtin()[1].1,
        ));
        let brushes = [
            Brush::new(
                110.0,
                30.0,
                Color32::from_rgba_unmultiplied(20, 90, 200, 180),
                2.0,
            ),
            picture,
        ];
        for brush in brushes {
            let paint = |batched: bool| {
                let canvas = Canvas::new(192, 128, Color32::TRANSPARENT, 64);
                let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
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
                (0..2)
                    .flat_map(|ty| (0..3).map(move |tx| (tx, ty)))
                    .map(|(tx, ty)| canvas.get_layer_tile_data(1, tx, ty))
                    .collect::<Vec<_>>()
            };
            let whole = paint(true);
            assert!(whole.iter().any(|t| t.is_some()), "it painted");
            assert!(whole == paint(false));
        }
    }
}
