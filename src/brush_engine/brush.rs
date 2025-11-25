use crate::canvas::{history::{TileSnapshot, UndoAction}, canvas::{Canvas, alpha_over, blend_erase}};
use crate::utils::{vector::Vec2, profiler::ScopeTimer, color::Color};
use rand::Rng;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::ThreadPool;
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
    size: usize,
    data: Vec<f32>,
}

/// User-facing brush configuration and scratch buffers.
#[derive(Clone, Debug)]
pub struct Brush {
    pub diameter: f32,
    pub hardness: f32, // 0..100
    pub color: Color,
    pub spacing: f32, // Percentage of diameter (0..100+)
    pub flow: f32,    // 0..100
    pub opacity: f32, // 0..1
    pub blend_mode: BlendMode,

    pub brush_type: BrushType,
    pub pixel_perfect: bool,
    pub jitter: f32,
    pub anti_alias: bool,
    pub stabilizer: f32, // 0..1 (0 = off, 1 = max smoothing)
    mask_cache: Option<BrushMaskCache>,
}

impl Brush {
    /// Create a standard soft brush with the given radius, hardness, base color and spacing.
    pub fn new(diameter: f32, hardness: f32, color: Color, spacing: f32) -> Self {
        Self {
            diameter,
            hardness,
            color,
            spacing,
            flow: 100.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            brush_type: BrushType::Soft,
            pixel_perfect: false,
            jitter: 0.0,
            anti_alias: true,
            stabilizer: 0.0,
            mask_cache: None,
        }
    }

    #[allow(dead_code)]
    /// Convenience constructor for a pixel-perfect pen.
    pub fn new_pixel(diameter: f32, color: Color) -> Self {
        Self {
            diameter,
            hardness: 100.0,
            color,
            spacing: 10.0,
            flow: 100.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            brush_type: BrushType::Pixel,
            pixel_perfect: true,
            jitter: 0.0,
            anti_alias: false,
            stabilizer: 0.0,
            mask_cache: None,
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
        let need_new = match &self.mask_cache {
            Some(cache) => (cache.diameter - self.diameter).abs() > f32::EPSILON
                || (cache.hardness - self.hardness).abs() > f32::EPSILON,
            None => true,
        };

        if need_new {
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
                    let alpha_factor = if t < hardness {
                        1.0
                    } else {
                        let v = (t - hardness) / (1.0 - hardness);
                        let falloff = (1.0 - v.clamp(0.0, 1.0)).max(0.0);
                        falloff.powf(1.5)
                    };
                    data.push(alpha_factor);
                }
            }

            self.mask_cache = Some(BrushMaskCache {
                diameter: self.diameter,
                hardness: self.hardness,
                size,
                data,
            });
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

        let color = self.color;
        let alpha = color.a * self.opacity * (self.flow / 100.0);
        let src_color = Color { a: alpha, ..color };

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

                        // Aliased check
                        if dx * dx + dy * dy <= r_sq {
                            let local_y = gy - tile_y0;
                            let local_x = gx - tile_x0;
                            let idx = local_y * tile_size + local_x;

                            let dst = Color::from_color32(data[idx]);
                            let blended = match self.blend_mode {
                                BlendMode::Normal => alpha_over(src_color, dst),
                                BlendMode::Eraser => blend_erase(src_color, dst),
                            };
                            data[idx] = blended.to_color32();
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
        let flow_alpha = self.opacity * (self.flow / 100.0);
        let blend_mode = self.blend_mode;
        let mask = self.ensure_mask();
        let mask_size = mask.size as isize;
        let center_x = center.x;
        let center_y = center.y;
        let start_x = start_x;
        let start_y = start_y;
        let end_x = end_x;
        let end_y = end_y;

        _pool.install(|| {
            tiles.par_iter().for_each(|(tx, ty)| {
                let tile_x0 = tx * tile_size;
                let tile_y0 = ty * tile_size;
                let tile_x1 = tile_x0 + tile_size;
                let tile_y1 = tile_y0 + tile_size;

                let dx = if center_x < tile_x0 as f32 {
                    tile_x0 as f32 - center_x
                } else if center_x > tile_x1 as f32 {
                    center_x - tile_x1 as f32
                } else {
                    0.0
                };
                let dy = if center_y < tile_y0 as f32 {
                    tile_y0 as f32 - center_y
                } else if center_y > tile_y1 as f32 {
                    center_y - tile_y1 as f32
                } else {
                    0.0
                };

                if dx * dx + dy * dy > r_sq {
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
                        let mask_y = ((gy as f32 + 0.5 - center_y + r).floor()) as isize;
                        if mask_y < 0 || mask_y >= mask_size {
                            continue;
                        }
                        let mask_row = (mask_y as usize) * mask.size;
                        for gx in overlap_min_x..=overlap_max_x {
                            let mask_x = ((gx as f32 + 0.5 - center_x + r).floor()) as isize;
                            if mask_x < 0 || mask_x >= mask_size {
                                continue;
                            }

                            let alpha_factor = mask.data[mask_row + mask_x as usize];
                            if alpha_factor <= 0.0 {
                                continue;
                            }

                            let src = Color {
                                r: base_color.r,
                                g: base_color.g,
                                b: base_color.b,
                                a: base_color.a * alpha_factor * flow_alpha,
                            };

                            let local_y = gy - tile_y0;
                            let local_x = gx - tile_x0;
                            let idx = local_y * tile_size + local_x;

                            let dst = Color::from_color32(data[idx]);
                            let blended = match blend_mode {
                                BlendMode::Normal => alpha_over(src, dst),
                                BlendMode::Eraser => blend_erase(src, dst),
                            };
                            data[idx] = blended.to_color32();
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
                    let jx = rng.random_range(-brush.jitter..=brush.jitter);
                    let jy = rng.random_range(-brush.jitter..=brush.jitter);
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
