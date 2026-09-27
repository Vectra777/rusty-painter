use crate::selection::SelectionManager;
use eframe::egui::Vec2;
use rayon::ThreadPool;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::slice::ParallelSlice;
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug)]
pub(super) struct TileRegion {
    pub tx: usize,
    pub ty: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DabBounds {
    pub start_x: usize,
    pub start_y: usize,
    pub end_x: usize,
    pub end_y: usize,
    pub min_tx: usize,
    pub max_tx: usize,
    pub min_ty: usize,
    pub max_ty: usize,
}

pub(super) fn calc_dab_bounds(
    center: Vec2,
    radius: f32,
    canvas_w: i32,
    canvas_h: i32,
    tile_size: usize,
) -> Option<DabBounds> {
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

    Some(DabBounds {
        start_x,
        start_y,
        end_x,
        end_y,
        min_tx: start_x / tile_size,
        max_tx: end_x / tile_size,
        min_ty: start_y / tile_size,
        max_ty: end_y / tile_size,
    })
}

/// A dab position resolved against the canvas: clipped bounds plus the
/// quantized placement the Gaussian mask kernel uses.
#[derive(Clone, Copy, Debug)]
pub(super) struct PlacedDab {
    pub center: Vec2,
    pub bounds: DabBounds,
    /// Mask origin in canvas pixels (`floor(center) - r_ceil`).
    pub base_x: i32,
    pub base_y: i32,
    /// Sub-pixel center offset, quantized to 1/16 px.
    pub frac_x: f32,
    pub frac_y: f32,
    /// How a custom tip is turned for this dab (mirror copies): maps canvas
    /// offsets from the centre to tip offsets, row-major.
    pub orient: [f32; 4],
}

impl PlacedDab {
    pub fn new(center: Vec2, bounds: DabBounds, r_ceil: i32) -> Self {
        let quantize =
            |v: f32| ((v - v.floor()) * 16.0).floor().clamp(0.0, 15.0) as u8 as f32 / 16.0;
        Self {
            center,
            bounds,
            base_x: center.x.floor() as i32 - r_ceil,
            base_y: center.y.floor() as i32 - r_ceil,
            frac_x: quantize(center.x),
            frac_y: quantize(center.y),
            orient: [1.0, 0.0, 0.0, 1.0],
        }
    }

    /// `(dx, dy)` (canvas offset from the centre) in the tip's frame.
    #[inline]
    pub fn tip_offset(&self, dx: f32, dy: f32) -> (f32, f32) {
        let [a, b, c, d] = self.orient;
        (a * dx + b * dy, c * dx + d * dy)
    }
}

/// A tile plus the indices (in stroke order) of the dabs that touch it.
pub(super) type TileBucket = (TileRegion, Vec<usize>);

/// Group dabs by the tiles their bounds touch, like libmypaint's per-tile
/// operation queue. Each tile keeps its dabs in stroke order, so per-pixel
/// blending order is unchanged; tiles are listed in first-touch order.
pub(super) fn bucket_by_tile(dabs: &[PlacedDab]) -> Vec<TileBucket> {
    let mut slots: FxHashMap<(usize, usize), usize> = FxHashMap::default();
    let mut buckets: Vec<TileBucket> = Vec::new();
    for (i, dab) in dabs.iter().enumerate() {
        let b = &dab.bounds;
        for ty in b.min_ty..=b.max_ty {
            for tx in b.min_tx..=b.max_tx {
                let slot = *slots.entry((tx, ty)).or_insert_with(|| {
                    buckets.push((TileRegion { tx, ty }, Vec::new()));
                    buckets.len() - 1
                });
                buckets[slot].1.push(i);
            }
        }
    }
    buckets
}

/// Whether a dab of radius `r` at `center` can reach the tile at `(x0, y0)`.
pub(super) fn dab_reaches_tile(
    center: Vec2,
    r: f32,
    x0: usize,
    y0: usize,
    tile_size: usize,
) -> bool {
    !(center.x < x0 as f32 - r
        || center.x > (x0 + tile_size) as f32 + r
        || center.y < y0 as f32 - r
        || center.y > (y0 + tile_size) as f32 + r)
}

/// Pixel-range within a tile that a dab's bounds actually overlap.
#[derive(Clone, Copy, Debug)]
pub(super) struct TileOverlap {
    pub min_x: usize,
    pub max_x: usize,
    pub min_y: usize,
    pub max_y: usize,
}

/// Clip `bounds` to the pixel range of a single tile at `(tile_x0, tile_y0)`.
/// Identical computation was previously duplicated at every draw_tile call
/// site in brush.rs.
pub(super) fn tile_overlap(
    bounds: &DabBounds,
    tile_x0: usize,
    tile_y0: usize,
    tile_size: usize,
) -> TileOverlap {
    TileOverlap {
        min_x: bounds.start_x.max(tile_x0),
        max_x: bounds.end_x.min(tile_x0 + tile_size - 1),
        min_y: bounds.start_y.max(tile_y0),
        max_y: bounds.end_y.min(tile_y0 + tile_size - 1),
    }
}

/// Pixel work (dab area) that justifies one more thread. Below this per
/// thread, extra threads mostly hand off work and spin: measured on a
/// 16-thread pool, a 20 px brush used 3.5 cores to paint no faster than one.
const PIXELS_PER_THREAD: usize = 16384;

/// Run `draw_tile` over every bucket: serially for a small batch, otherwise
/// once through the thread pool (one dispatch per batch, not per dab).
pub(super) fn dispatch_over_buckets<F>(
    buckets: &[TileBucket],
    pool: &ThreadPool,
    work_pixels: usize,
    draw_tile: F,
) where
    F: Fn(&TileBucket) + Sync,
{
    // Give work to only as many threads as the batch justifies: split the
    // tiles into that many chunks, so rayon wakes that many workers instead
    // of spreading a small batch over the whole pool.
    let threads = (work_pixels / PIXELS_PER_THREAD)
        .min(pool.current_num_threads())
        .min(buckets.len());
    if threads <= 1 {
        buckets.iter().for_each(&draw_tile);
    } else if threads >= pool.current_num_threads() {
        // Enough work for the whole pool: rayon's dynamic splitting balances
        // uneven tiles better than fixed chunks.
        pool.install(|| buckets.par_iter().for_each(&draw_tile));
    } else {
        let chunk = buckets.len().div_ceil(threads);
        pool.install(|| {
            buckets
                .par_chunks(chunk)
                .for_each(|tiles| tiles.iter().for_each(&draw_tile))
        });
    }
}

pub(super) fn tile_overlaps_selection(
    selection: Option<&SelectionManager>,
    tile_x0: usize,
    tile_y0: usize,
    tile_size: usize,
) -> bool {
    let Some(sel) = selection else {
        return true;
    };
    let Some(sel_bounds) = sel.get_bounds() else {
        return true;
    };

    let tile_max_x = (tile_x0 + tile_size) as f32;
    let tile_max_y = (tile_y0 + tile_size) as f32;
    !(tile_x0 as f32 >= sel_bounds.max.x
        || tile_max_x <= sel_bounds.min.x
        || tile_y0 as f32 >= sel_bounds.max.y
        || tile_max_y <= sel_bounds.min.y)
}
