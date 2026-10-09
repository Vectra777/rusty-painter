//! Painting a batch of placed dabs into the canvas's tiles, in parallel
//! bands, and resolving the stroke into the layer.

use super::*;
use crate::canvas::blend::{DeepStroke, resolve_stroke_deep};
use crate::canvas::history::SnapshotPixels;

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
pub(super) fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
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
        if ctx.defer {
            merge_spans(&mut buffer.pending, &spans, tile_size);
        } else {
            // (With what was left unresolved here.)
            let mut spans = spans;
            if let Some(pending) = buffer.pending.take() {
                for (s, p) in spans.iter_mut().zip(pending) {
                    *s = (s.0.min(p.0), s.1.max(p.1));
                }
            }
            resolve_spans(ctx, *region, &mut buffer, &spans);
        }
    };
    dispatch_over_buckets(buckets, pool, work_pixels, draw_tile);
}

/// Rows' columns `spans` added to those left unresolved in `pending`.
fn merge_spans(pending: &mut Option<Vec<(usize, usize)>>, spans: &[(usize, usize)], side: usize) {
    let pending = pending.get_or_insert_with(|| vec![(usize::MAX, 0); side]);
    for (p, s) in pending.iter_mut().zip(spans) {
        *p = (p.0.min(s.0), p.1.max(s.1));
    }
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

/// Impasto: put a tile's newly laid heights (`rows`: where each starts in
/// the tile, and its heights) on the layer's heights.
fn write_heights(ctx: &BatchCtx<'_>, region: TileRegion, rows: Vec<(usize, Vec<u16>)>) {
    if rows.is_empty() {
        return;
    }
    let Some(map) = ctx
        .canvas
        .layers
        .get(ctx.canvas.active_layer_idx)
        .and_then(|l| l.height.as_deref())
    else {
        return;
    };
    let side = ctx.canvas.tile_size();
    map.edit_tile((region.tx as i32, region.ty as i32), side, |tile| {
        for (start, laid) in rows {
            tile[start..start + laid.len()].copy_from_slice(&laid);
        }
    });
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
    if tile.data().is_none() {
        return;
    }
    let side = ctx.canvas.tile_size();
    let (data, deep) = tile.deep_parts(ctx.canvas.depth(), side * side);
    resolve_spans_in(ctx, region, buffer, spans, data, deep);
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
    mut deep: Option<&mut crate::canvas::storage::DeepTile>,
) {
    let tile_size = ctx.canvas.tile_size();
    let (tile_x0, tile_y0) = (region.tx * tile_size, region.ty * tile_size);
    // The stroke and its tail together: dabs combine the same in any order.
    let mut combined = Vec::new();
    let mut combined_colors: Vec<[f32; 3]> = Vec::new();
    // The same for the whole tile: decided once, not per row.
    let has_tail = buffer.tail.iter().any(Option::is_some);
    let behind = ctx.blend_mode == BlendMode::Behind;
    let general = (ctx.general && ctx.blend_mode == BlendMode::Normal) || behind;
    // Impasto: the rows' heights, written to the layer's heights at once.
    let mut height_rows: Vec<(usize, Vec<u16>)> = Vec::new();
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
        if let (Some((impasto, erase)), Some(before)) = (ctx.impasto, &buffer.heights) {
            let laid: Vec<u16> = (before[range.clone()].iter().zip(coverage))
                .map(|(&h, &c)| impasto.height(h, c, erase))
                .collect();
            height_rows.push((range.start, laid));
        }
        // Canvas position of the span, for the alpha dither.
        let origin = [(tile_x0 + min_x) as u32, (tile_y0 + row) as u32];
        // A deeper document: the stroke at full depth, rounded to 8 bits.
        if let (Some(deep), Some(original)) = (deep.as_deref_mut(), &buffer.original_deep) {
            let stroke = match ctx.blend_mode {
                BlendMode::Eraser | BlendMode::Behind if ctx.alpha_lock => continue,
                BlendMode::Eraser => DeepStroke::Erase,
                BlendMode::Behind => DeepStroke::Behind {
                    colors: ctx.colored.then(|| {
                        stroke_colors(buffer, range.clone(), ctx.tail_newer, &mut combined_colors);
                        &combined_colors[..]
                    }),
                },
                BlendMode::Normal => DeepStroke::Paint {
                    mode: if general {
                        ctx.mode
                    } else {
                        LayerBlend::Normal
                    },
                    colors: (general && ctx.colored).then(|| {
                        stroke_colors(buffer, range.clone(), ctx.tail_newer, &mut combined_colors);
                        &combined_colors[..]
                    }),
                },
            };
            resolve_stroke_deep(
                original,
                range.start,
                coverage,
                deep,
                &mut data[range],
                ctx.color,
                ctx.cap,
                stroke,
                ctx.space,
                ctx.alpha_lock,
                origin,
            );
            continue;
        }
        if general {
            // Under the pixels, where their alpha is locked: nothing shows.
            if behind && ctx.alpha_lock {
                continue;
            }
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
                behind,
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
            // (Painting behind always goes the general way.)
            BlendMode::Eraser | BlendMode::Behind if ctx.alpha_lock => {}
            BlendMode::Behind => {}
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
    write_heights(ctx, region, height_rows);

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
                if tile.data().is_some() {
                    let (data, mut deep) = tile.deep_parts(canvas.depth(), tile_size * tile_size);
                    for row in y0..y1 {
                        let range = row * tile_size + x0..row * tile_size + x1;
                        data[range.clone()].copy_from_slice(&buffer.original[range]);
                    }
                    if let (Some(deep), Some(original)) =
                        (deep.as_deref_mut(), &buffer.original_deep)
                    {
                        let block = original.block(tile_size, (x0, y0, x1 - x0, y1 - y0));
                        deep.put_block(tile_size, (x0, y0, x1 - x0), &block);
                    }
                    let region = TileRegion {
                        tx: key.0,
                        ty: key.1,
                    };
                    resolve_spans_in(&ctx, region, &mut buffer, &spans, data, deep);
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
    /// and show the result. (Wet paint darkens its own edges instead.)
    pub(crate) fn apply_wet_edges(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let keys: Vec<(usize, usize)> = stroke_tiles.buffers.keys().copied().collect();
        self.wet_edge_tiles(pool, canvas, selection, stroke_tiles, &keys, true);
    }

    /// Watercolour edges while the pen is down: the tiles just painted
    /// (and those next to them, whose edges they change) shown as they'll
    /// be when it lifts; the stroke's own coverage is kept as it is.
    pub(crate) fn wet_edges_live(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        stroke_tiles: &mut StrokeTiles,
    ) {
        if self.wet_edge <= 0.0 || self.wet.is_some() || stroke_tiles.dirty.is_empty() {
            return;
        }
        let mut keys: Vec<(usize, usize)> = Vec::new();
        for &(tx, ty) in &stroke_tiles.dirty {
            for dy in -1..=1_isize {
                for dx in -1..=1_isize {
                    let key = (tx.wrapping_add_signed(dx), ty.wrapping_add_signed(dy));
                    if stroke_tiles.buffers.contains_key(&key) && !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
        }
        self.wet_edge_tiles(pool, canvas, selection, stroke_tiles, &keys, false);
    }

    /// Watercolour edges on tiles `keys`, from the stroke's coverage; for
    /// good (`last`: the coverage becomes the thinned one) or only shown.
    fn wet_edge_tiles(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        stroke_tiles: &mut StrokeTiles,
        keys: &[(usize, usize)],
        last: bool,
    ) {
        use rayon::prelude::*;
        if self.wet_edge <= 0.0 || self.wet.is_some() || keys.is_empty() {
            return;
        }
        let ts = canvas.tile_size();
        let radius = (self.wet_edge_width.round() as usize).clamp(1, ts);
        let pad = radius;
        let side = ts + 2 * pad;
        let buffers = &stroke_tiles.buffers;
        let strength = self.wet_edge;
        // Each tile's thinned coverage, read from the stroke's coverage
        // around it (before any of it changes).
        let after: Vec<((usize, usize), Vec<f32>)> = pool.install(|| {
            keys.par_iter()
                .map(|&(tx, ty)| {
                    let mut patch = vec![0.0f32; side * side];
                    // The tiles under the patch (itself and its neighbours),
                    // each locked once.
                    for sty in ty.saturating_sub(1)..=ty + 1 {
                        for stx in tx.saturating_sub(1)..=tx + 1 {
                            let Some(buffer) = buffers.get(&(stx, sty)) else {
                                continue;
                            };
                            let buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
                            let c = &buffer.coverage;
                            // This tile's part of the patch, in patch rows
                            // and columns.
                            let (ox, oy) = (
                                (stx * ts) as isize - (tx * ts) as isize + pad as isize,
                                (sty * ts) as isize - (ty * ts) as isize + pad as isize,
                            );
                            let x0 = ox.max(0) as usize;
                            let x1 = (ox + ts as isize).min(side as isize).max(0) as usize;
                            let y0 = oy.max(0) as usize;
                            let y1 = (oy + ts as isize).min(side as isize).max(0) as usize;
                            for py in y0..y1 {
                                let ly = (py as isize - oy) as usize;
                                let lx0 = (x0 as isize - ox) as usize;
                                patch[py * side + x0..py * side + x1]
                                    .copy_from_slice(&c[ly * ts + lx0..ly * ts + lx0 + (x1 - x0)]);
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
            let raw = std::mem::replace(&mut buffer.coverage, coverage);
            let region = TileRegion {
                tx: key.0,
                ty: key.1,
            };
            resolve_spans(&ctx, region, &mut buffer, &spans);
            if !last {
                buffer.coverage = raw;
            }
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
            impasto: self.paints_heights(canvas),
            sharpness: self.sharpness,
            sharpness_softness: self.sharpness_softness.clamp(0.0, 1.0),
            strength: 1.0,
            wash: None,
            defer: false,
        }
    }

    /// Resolve what was left unresolved (see [`StrokeTiles::defer_resolve`])
    /// into the tiles' pixels, and mark them for redrawing.
    pub(crate) fn flush_resolves(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        stroke_tiles: &mut StrokeTiles,
    ) {
        use rayon::prelude::*;
        let keys: Vec<(usize, usize)> = (stroke_tiles.buffers.iter())
            .filter(|(_, b)| {
                b.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending
                    .is_some()
            })
            .map(|(&k, _)| k)
            .collect();
        if keys.is_empty() {
            return;
        }
        let ctx = self.batch_ctx(
            canvas,
            selection,
            &[],
            &stroke_tiles.buffers,
            Target::Stroke,
            stroke_tiles.tail_newer,
            stroke_tiles.grain,
        );
        pool.install(|| {
            keys.par_iter().for_each(|&(tx, ty)| {
                let Some(buffer) = ctx.buffers.get(&(tx, ty)) else {
                    return;
                };
                let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(spans) = buffer.pending.take() {
                    resolve_spans(&ctx, TileRegion { tx, ty }, &mut buffer, &spans);
                }
            })
        });
        stroke_tiles.dirty.extend(keys);
    }

    /// Snapshot and paint dabs already placed on the canvas.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_placed(
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
        let heights = self.paints_heights(canvas).is_some();
        Self::snapshot_tiles(canvas, &regions, undo_action, stroke_tiles, heights);

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
        // (A wash's cap goes by the pressure as it's painted: resolved now.)
        ctx.defer = stroke_tiles.defer_resolve && !wash && target != Target::Mask;
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
    pub(super) fn snapshot_tiles(
        canvas: &Canvas,
        regions: &[TileRegion],
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
        heights: bool,
    ) {
        let height_map = canvas
            .layers
            .get(canvas.active_layer_idx)
            .and_then(|l| l.height.as_deref())
            .filter(|_| heights);
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
            let Some(data) = tile.data() else {
                continue;
            };
            // A deeper document's pixels at full depth (widened from 8 bits
            // if the tile has none yet).
            let original_deep = (tile.deep().cloned())
                .or_else(|| crate::canvas::storage::DeepTile::widen(canvas.depth(), data));
            undo_action.tiles.push(TileSnapshot {
                tx: region.tx as i32,
                ty: region.ty as i32,
                layer_id,
                x0: 0,
                y0: 0,
                width: tile_size,
                height: tile_size,
                data: match &original_deep {
                    Some(deep) => SnapshotPixels::Deep(deep.clone()),
                    None => data.clone().into(),
                },
            });
            // Impasto: the tile's heights as they were, for the stroke and
            // its undo step.
            let heights = height_map.map(|map| {
                let tile_key = (region.tx as i32, region.ty as i32);
                let before = map.tile(tile_key);
                crate::canvas::impasto::record_undo(
                    undo_action,
                    layer_id,
                    tile_key,
                    before.clone(),
                );
                before.unwrap_or_else(|| vec![0; tile_size * tile_size])
            });
            stroke_tiles.buffers.insert(
                key,
                Mutex::new(StrokeBuffer {
                    heights,
                    original: data.clone(),
                    original_deep,
                    coverage: vec![0.0; tile_size * tile_size],
                    selection: None,
                    damage: None,
                    tail: [None, None],
                    tail_rect: [None, None],
                    colors: None,
                    tail_colors: [None, None],
                    mask: None,
                    mask_dirty: None,
                    pending: None,
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
            let edge = self.antialias_width.max(0.5);
            let tip_for = |r, hardness| GaussianTip::new(r, hardness, edge);
            let batch_tip = tip_for(ctx.r, hardness_val);
            let stamp = |dab: &PlacedDab, gy: usize, x0: usize, out: &mut [f32]| {
                // Turning a round tip changes nothing; squashing it does.
                // Small tips are supersampled.
                if !dab.rigid || crate::brush_engine::masks::supersamples(dab.r, true) > 1 {
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
                            let h = crate::brush_engine::brush_options::hash3(
                                seed,
                                (x0 + i) as u32,
                                gy as u32,
                            );
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
    curve_lut: Option<std::sync::Arc<crate::brush_engine::hardness::CurveLut>>,
    /// Softer dabs (a Softness input): a falloff for each level used.
    soft_luts: Vec<Option<std::sync::Arc<crate::brush_engine::hardness::CurveLut>>>,
    tips: std::borrow::Cow<'a, [PixelBrushShape]>,
    anti_aliasing: bool,
    /// How many pixels an anti-aliased round or square tip's edge fades
    /// over.
    edge: f32,
    custom: bool,
    auto: crate::brush_engine::brush_options::AutoTip,
    auto_on: bool,
    grainy: bool,
    spikes: Option<crate::brush_engine::brush_options::Spikes>,
    /// Small supersampled dabs' coverage, worked out once for each
    /// size, hardness and place within a pixel (see [`SmallKey`]).
    small: FxHashMap<SmallKey, Box<[f32]>>,
}

/// A small round or square dab, as [`SoftTip`] keeps its coverage: its
/// tip, radius, hardness and softness level, and where its centre sits in
/// its pixel (in 16ths).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SmallKey {
    tip: u8,
    r: u32,
    hardness: u32,
    soft: u8,
    at: (u8, u8),
}

/// Small dabs' centres snap to this fraction of a pixel (well under what
/// shows), so a stroke's thousands share a few coverages.
const SMALL_STEPS: f32 = 16.0;

impl<'a> SoftTip<'a> {
    /// For `brush`'s tips, painting `dabs`.
    pub(crate) fn new(brush: &'a Brush, dabs: &[PlacedDab]) -> Self {
        let o = &brush.brush_options;
        let anti_aliasing_flag = brush.anti_aliasing;
        let hardness_val = (o.hardness / 100.0).clamp(0.0, 1.0);
        let softness_selector = o.softness_selector;
        let curve_lut = (softness_selector == SoftnessSelector::Curve)
            .then(|| crate::brush_engine::hardness::CurveLut::cached(&o.softness_curve));
        // Softer dabs (a Softness input): a falloff for each level used.
        let full = crate::brush_engine::dab::SOFT_LEVELS;
        let soft_luts: Vec<Option<std::sync::Arc<crate::brush_engine::hardness::CurveLut>>> =
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
                            crate::brush_engine::hardness::CurveLut::cached(
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
            edge: brush.antialias_width.max(0.5),
            custom,
            auto,
            auto_on,
            grainy,
            spikes,
            small: FxHashMap::default(),
        }
        .with_small(dabs)
    }

    /// The key a small dab's coverage is kept under, if it's one that's
    /// worth keeping: a round or square tip taking several samples a pixel,
    /// upright, its coverage the same wherever it is (no spikes, fades or
    /// grain).
    fn small_key(&self, dab: &PlacedDab) -> Option<SmallKey> {
        let fade = self.auto_on && self.auto.has_fade();
        if self.custom
            || self.grainy
            || fade
            || self.spikes.is_some()
            || !dab.upright()
            || crate::brush_engine::masks::supersamples(dab.r, self.anti_aliasing) == 1
        {
            return None;
        }
        let hardness = (self.hardness_val + dab.hardness).clamp(0.0, 1.0) * dab.softness();
        let q = |v: f32| {
            ((v - v.floor()) * SMALL_STEPS)
                .round()
                .min(SMALL_STEPS - 1.0) as u8
        };
        Some(SmallKey {
            tip: dab.tip,
            r: dab.r.to_bits(),
            hardness: hardness.to_bits(),
            soft: dab.soft,
            at: (q(dab.center.x), q(dab.center.y)),
        })
    }

    /// The side of a small dab's kept square, and where it starts relative
    /// to the pixel holding its centre.
    fn small_extent(r: f32) -> (usize, i32) {
        let reach = r.ceil() as i32 + 1;
        ((2 * reach + 1) as usize, -reach)
    }

    /// Work out the coverage of `dabs`' small dabs, once each.
    fn with_small(mut self, dabs: &[PlacedDab]) -> Self {
        for dab in dabs {
            let Some(key) = self.small_key(dab) else {
                continue;
            };
            if self.small.contains_key(&key) {
                continue;
            }
            let (side, start) = Self::small_extent(dab.r);
            // The dab with its centre where the key has it, far enough in
            // that the square starts on the canvas.
            let base = (side + 1) as f32;
            let mut at = *dab;
            at.center = Vec2::new(
                base + key.at.0 as f32 / SMALL_STEPS,
                base + key.at.1 as f32 / SMALL_STEPS,
            );
            at.strength = 1.0;
            let origin = (base as i32 + start) as usize;
            let mut block = vec![0.0f32; side * side];
            for (ly, row) in block.chunks_mut(side).enumerate() {
                self.row_exact(&at, origin + ly, origin, row, 1.0);
            }
            self.small.insert(key, block.into_boxed_slice());
        }
        self
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
        if let Some(block) = self.small_key(dab).and_then(|k| self.small.get(&k)) {
            let (side, start) = Self::small_extent(dab.r);
            let ly = gy as i32 - (dab.center.y.floor() as i32 + start);
            let bx = dab.center.x.floor() as i32 + start;
            let strength = (strength * dab.strength).min(1.0);
            out.fill(0.0);
            if (0..side as i32).contains(&ly) {
                let row = &block[ly as usize * side..(ly as usize + 1) * side];
                for (i, slot) in out.iter_mut().enumerate() {
                    let lx = (x0 + i) as i32 - bx;
                    if (0..side as i32).contains(&lx) {
                        *slot = row[lx as usize] * strength;
                    }
                }
            }
            return nonzero_span(out);
        }
        self.row_exact(dab, gy, x0, out, strength)
    }

    /// [`Self::row`] for any round or square tip, worked out pixel by pixel.
    fn row_exact(
        &self,
        dab: &PlacedDab,
        gy: usize,
        x0: usize,
        out: &mut [f32],
        strength: f32,
    ) -> Range<usize> {
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
            SoftnessSelector::Gaussian => {
                crate::brush_engine::masks::gaussian_falloff(t, hardness_val)
            }
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
                    let a = crate::brush_engine::masks::auto_tip_alpha(
                        (pdx, pdy),
                        r,
                        matches!(shape, PixelBrushShape::Square),
                        softness_selector,
                        falloff,
                        if anti_aliasing { self.edge } else { 0.0 },
                    );
                    if fade && a > 0.0 {
                        a * crate::brush_engine::brush_options::AutoTip::fade_with(
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
            crate::brush_engine::masks::supersamples(r, anti_aliasing)
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

#[cfg(test)]
mod small_dab_tests {
    use super::*;
    use crate::brush_engine::dab::calc_dab_bounds;

    /// A small soft dab's rows from its kept coverage, against working it
    /// out pixel by pixel: the same on the 16ths it snaps to, all but the
    /// same between them.
    #[test]
    fn small_dabs_paint_as_worked_out_pixel_by_pixel() {
        let brush = Brush::new(3.0, 70.0, Color32::BLACK, 10.0);
        for (center, tolerance) in [
            (Vec2::new(10.0 + 5.0 / 16.0, 7.0 + 11.0 / 16.0), 1e-6),
            (Vec2::new(10.37, 7.71), 0.08),
        ] {
            let bounds = calc_dab_bounds(center, 1.5, 64, 64, 64).unwrap();
            let dab = PlacedDab::new(center, bounds, 1.5);
            let tip = SoftTip::new(&brush, std::slice::from_ref(&dab));
            assert_eq!(tip.small.len(), 1, "kept");
            for gy in 4..12 {
                let (mut kept, mut exact) = ([0.0f32; 12], [0.0f32; 12]);
                tip.row(&dab, gy, 5, &mut kept, 1.0);
                tip.row_exact(&dab, gy, 5, &mut exact, 1.0);
                for (k, e) in kept.iter().zip(&exact) {
                    assert!(
                        (k - e).abs() <= tolerance,
                        "{center:?} row {gy}: {kept:?} {exact:?}"
                    );
                }
            }
        }
    }
}
