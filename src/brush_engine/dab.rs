use crate::selection::SelectionManager;
use eframe::egui::Vec2;
use rayon::ThreadPool;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

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

pub(super) fn build_tile_regions(bounds: &DabBounds) -> Vec<TileRegion> {
    (bounds.min_ty..=bounds.max_ty)
        .flat_map(|ty| (bounds.min_tx..=bounds.max_tx).map(move |tx| TileRegion { tx, ty }))
        .collect()
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

/// Run `draw_tile` over every tile, serially for a small/cheap dab and via
/// the thread pool otherwise. The exact same threshold
/// (`tiles.len() == 1 || (tiles.len() <= 4 && diameter <= 24.0)`) was
/// previously duplicated at every dab call site in brush.rs.
pub(super) fn dispatch_over_tiles<F>(tiles: &[TileRegion], pool: &ThreadPool, diameter: f32, draw_tile: F)
where
    F: Fn(&TileRegion) + Sync,
{
    if tiles.len() == 1 || (tiles.len() <= 4 && diameter <= 24.0) {
        tiles.iter().for_each(&draw_tile);
    } else {
        pool.install(|| tiles.par_iter().for_each(&draw_tile));
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
