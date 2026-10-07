//! From tiles to screen: composite dirty tiles, upload them to the GPU
//! atlases, place the canvas quad, and map canvas to screen coordinates
//! ([`ScreenMap`]) for overlays.

use crate::PainterApp;
use crate::app::document::{ATLAS_SIZE, CanvasTile, TILE_SIZE};
use crate::app::state::{BelowCache, RenderCache};
use crate::app::view::gpu_canvas::{
    ATLAS_BORDER, ATLAS_TEXTURE_SIZE, AtlasQuad, CanvasPaint, MIP_LEVELS, TileUpload,
};
use crate::app::view::shader_gpu::ComposePlan;
use crate::canvas::storage::BelowComposite;
use eframe::egui::{self, Color32};
use eframe::egui_wgpu;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

pub struct CanvasView {
    pub origin: egui::Pos2,
    pub canvas_center: egui::Pos2,
    pub response: egui::Response,
    /// The canvas panel on screen.
    rect: egui::Rect,
    /// Paint-order slot reserved for the canvas, filled by [`paint_canvas`].
    slot: egui::layers::ShapeIdx,
}

/// While a stroke is in progress on a layer with 2+ layers below it, make sure
/// every dirty tile has the composite of those layers cached, so redraws only
/// composite the active layer and the ones above it.
fn refresh_below_cache(app: &mut PainterApp, visible: &impl Fn(&CanvasTile) -> bool) {
    let active = app.canvas.active_layer_idx;
    // The cache splits the stack at the active layer, which only works for a
    // plain stack: folders and masks composite as a tree.
    if !app.brush_state.is_drawing || active < 2 || app.canvas.needs_tree_compositing() {
        app.render_cache.below_cache = None;
        return;
    }
    let key = app.canvas.composite_below_key(active);
    let cache = match &mut app.render_cache.below_cache {
        Some(cache) if cache.key == key => cache,
        slot => slot.insert(BelowCache {
            key,
            tiles: Default::default(),
        }),
    };
    let missing: Vec<(usize, usize)> = app
        .render_cache
        .tiles
        .iter()
        .filter(|t| t.dirty && visible(t) && !cache.tiles.contains_key(&(t.tx, t.ty)))
        .map(|t| (t.tx, t.ty))
        .collect();
    let canvas = &app.canvas;
    let computed: Vec<_> = app.workspace.pool.install(|| {
        missing
            .par_iter()
            .map(|&(tx, ty)| {
                (
                    (tx, ty),
                    canvas.composite_below(active, tx as i32, ty as i32),
                )
            })
            .collect()
    });
    cache.tiles.extend(computed);
}

/// Mip level to upload strokes at: while painting zoomed out, only the level
/// on screen (and coarser) is updated; full
/// resolution follows when the stroke ends or the view zooms in.
///
/// The GPU picks mip levels per *physical* pixel, so the display scale must
/// be included: at 1.5x a 0.5 zoom shows ~0.75 texels per pixel, i.e. level 0,
/// and uploading level 1 would leave the (stale) finer level visible. Half a
/// level of margin keeps rounding in the hardware's LOD estimate safe.
fn preview_level(drawing: bool, zoom: f32, pixels_per_point: f32) -> u32 {
    if !drawing {
        return 0;
    }
    let texels_per_pixel_log2 = (1.0 / (zoom * pixels_per_point)).log2();
    ((texels_per_pixel_log2 - 0.5).floor().max(0.0) as u32).min(MIP_LEVELS - 1)
}

/// Canvas pixels per pixel of a live preview (a power of two): about one
/// per physical screen pixel, never finer than the level uploaded.
fn live_preview_block(zoom: f32, pixels_per_point: f32) -> usize {
    let lod = (1.0 / (zoom * pixels_per_point)).log2().floor().max(0.0) as u32;
    let level = preview_level(true, zoom, pixels_per_point);
    1 << lod.clamp(level, 6)
}

/// The part of the canvas on screen, and the resolution a live preview
/// (a filter or adjustment setting being dragged) is computed at there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewView {
    /// Tile columns and rows in view.
    pub xs: std::ops::Range<usize>,
    pub ys: std::ops::Range<usize>,
    /// See [`live_preview_block`].
    pub block: usize,
}

impl PreviewView {
    pub(crate) fn contains(&self, tx: usize, ty: usize) -> bool {
        self.xs.contains(&tx) && self.ys.contains(&ty)
    }
}

/// What of the canvas the screen shows in `clip`.
pub(crate) fn preview_view(app: &PainterApp, view: &CanvasView, clip: egui::Rect) -> PreviewView {
    let (origin, center) = canvas_placement(app, view.rect);
    let (cos, sin) = (app.viewport.rotation.cos(), app.viewport.rotation.sin());
    let zoom = app.viewport.zoom;
    let flip = app.viewport.flip_x.then_some(app.canvas.width() as f32);
    let (xs, ys) = if app.workspace.wrap_around {
        // Every tile may show in one of the repeats.
        (0..app.render_cache.tiles_x, 0..app.render_cache.tiles_y)
    } else {
        visible_tile_range(
            clip.intersect(view.rect),
            |p| {
                let c = (PainterApp::rotate_point(p, center, cos, -sin) - origin).to_pos2() / zoom;
                flip.map_or(c, |w| egui::pos2(w - c.x, c.y))
            },
            app.render_cache.tiles_x,
            app.render_cache.tiles_y,
        )
    };
    PreviewView {
        xs,
        ys,
        block: live_preview_block(zoom, view.response.ctx.pixels_per_point()),
    }
}

/// `small` (a tile composited `block` times smaller) scaled up to mip
/// `level` of a `tile_size` tile, each of its pixels repeated.
fn preview_at_level(
    small: &egui::ColorImage,
    block: usize,
    tile_size: [usize; 2],
    level: u32,
) -> egui::ColorImage {
    let factor = (block >> level).max(1);
    let size = [
        tile_size[0].div_ceil(1 << level),
        tile_size[1].div_ceil(1 << level),
    ];
    if factor == 1 {
        return small.clone();
    }
    let mut img = egui::ColorImage::new(size, Color32::TRANSPARENT);
    for y in 0..size[1] {
        let src = &small.pixels[(y / factor) * small.size[0]..][..small.size[0]];
        for (x, px) in img.pixels[y * size[0]..(y + 1) * size[0]]
            .iter_mut()
            .enumerate()
        {
            *px = src[x / factor];
        }
    }
    img
}

/// Part of a tile to copy into an atlas texture, in level-0 texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Destination {
    atlas: usize,
    dst: [usize; 2],
    src: [usize; 2],
    size: [usize; 2],
}

/// Where tile `(tx, ty)`'s pixels go: its own slot, plus the border strips of
/// any neighbouring atlas (sides and diagonals) that mirror its edge pixels.
fn tile_destinations(
    cache: &RenderCache,
    tx: usize,
    ty: usize,
    tile_size: [usize; 2],
) -> Vec<Destination> {
    let (atlas, lx, ly) = cache.atlas_slot(tx, ty);
    let (ax, ay) = (atlas % cache.atlases_x, atlas / cache.atlases_x);
    let local = [lx, ly];
    let mut out = Vec::with_capacity(4);
    for dy in -1i64..=1 {
        for dx in -1i64..=1 {
            let (nx, ny) = (ax as i64 + dx, ay as i64 + dy);
            if nx < 0 || ny < 0 || nx >= cache.atlases_x as i64 || ny >= cache.atlases_y as i64 {
                continue;
            }
            // Per axis: the slice of the tile inside the strip that neighbour
            // mirrors ([0, BORDER) of the block for -1, its last BORDER px for
            // +1, everything for 0).
            let mut src = [0usize; 2];
            let mut size = [0usize; 2];
            let mut dst = [0usize; 2];
            let mut inside = true;
            for axis in 0..2 {
                let d = [dx, dy][axis];
                let (lo, hi) = match d {
                    -1 => (0, ATLAS_BORDER),
                    1 => (ATLAS_SIZE - ATLAS_BORDER, ATLAS_SIZE),
                    _ => (0, ATLAS_SIZE),
                };
                let start = lo.max(local[axis]);
                let end = hi.min(local[axis] + tile_size[axis]);
                if start >= end {
                    inside = false;
                    break;
                }
                src[axis] = start - local[axis];
                size[axis] = end - start;
                dst[axis] = (ATLAS_BORDER as i64 + start as i64 - d * ATLAS_SIZE as i64) as usize;
            }
            if inside {
                out.push(Destination {
                    atlas: (ny as usize) * cache.atlases_x + nx as usize,
                    dst,
                    src,
                    size,
                });
            }
        }
    }
    out
}

/// The uploads for one composited tile at mip `level`. `img` holds the tile's
/// `rect` (tile-local pixels, start aligned to `2^level`), already shrunk;
/// `tile_size` is the whole tile's (edge-clipped) size.
fn tile_uploads(
    cache: &RenderCache,
    tx: usize,
    ty: usize,
    tile_size: [usize; 2],
    rect: [usize; 4],
    img: &egui::ColorImage,
    level: u32,
) -> Vec<TileUpload> {
    let scale = 1usize << level;
    let origin = [rect[0], rect[1]];
    let rect_end = [rect[2], rect[3]];
    tile_destinations(cache, tx, ty, tile_size)
        .into_iter()
        .filter_map(|dest| {
            // Part of this destination's slice of the tile that was redrawn.
            let mut src = [0usize; 2];
            let mut end = [0usize; 2];
            let mut dst = [0usize; 2];
            for a in 0..2 {
                let lo = dest.src[a].max(origin[a]);
                let hi = (dest.src[a] + dest.size[a]).min(rect_end[a]);
                if lo >= hi {
                    return None;
                }
                src[a] = (lo - origin[a]) / scale;
                end[a] = (hi - origin[a]).div_ceil(scale).min(img.size[a]);
                dst[a] = (dest.dst[a] + (lo - dest.src[a])) / scale;
            }
            let [w, h] = [end[0] - src[0], end[1] - src[1]];
            if w == 0 || h == 0 {
                return None;
            }
            let mut pixels = Vec::with_capacity(w * h * 4);
            for y in src[1]..end[1] {
                let row = &img.pixels[y * img.size[0] + src[0]..y * img.size[0] + end[0]];
                pixels.extend(row.iter().flat_map(|p| p.to_array()));
            }
            Some(TileUpload {
                atlas: dest.atlas,
                level,
                x: dst[0] as u32,
                y: dst[1] as u32,
                width: w as u32,
                height: h as u32,
                pixels,
            })
        })
        .collect()
}

/// Range of tile columns/rows whose canvas area can overlap `clip` (screen
/// space). The clip's corners are mapped into canvas pixels and their bounding
/// box taken, which stays conservative under rotation.
fn visible_tile_range(
    clip: egui::Rect,
    to_canvas: impl Fn(egui::Pos2) -> egui::Pos2,
    tiles_x: usize,
    tiles_y: usize,
) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let corners = [
        clip.left_top(),
        clip.right_top(),
        clip.right_bottom(),
        clip.left_bottom(),
    ]
    .map(&to_canvas);
    let bounds = egui::Rect::from_points(&corners);
    let tile = TILE_SIZE as f32;
    let span = |min: f32, max: f32, count: usize| {
        let start = (min / tile).floor().max(0.0) as usize;
        let end = ((max / tile).floor() + 1.0).clamp(0.0, count as f32) as usize;
        start.min(end)..end
    };
    (
        span(bounds.min.x, bounds.max.x, tiles_x),
        span(bounds.min.y, bounds.max.y, tiles_y),
    )
}

/// Most tiles composited and uploaded per frame (16 MiB at full resolution),
/// so a full refresh of a huge canvas fills in over a few frames instead of
/// stalling one.
const MAX_TILES_PER_FRAME: usize = 1024;

/// Composite the dirty tiles visible in `clip` (off-screen ones stay dirty
/// until scrolled into view), at most [`MAX_TILES_PER_FRAME`] of them, and hand
/// them back as atlas uploads: at full resolution normally, or at
/// [`preview_level`] mid-stroke when zoomed out. Also returns whether visible
/// dirty tiles remain for the next frame.
pub fn update_dirty_textures(
    app: &mut PainterApp,
    view: &CanvasView,
    clip: egui::Rect,
) -> (Vec<TileUpload>, bool) {
    // A filter or adjustment setting being dragged: a quick preview at
    // about the screen's resolution, made exact when it's let go.
    let liquify_live = app
        .layer_state
        .liquify
        .as_ref()
        .is_some_and(|s| s.previewing());
    let live = app.workspace.filter.live() || liquify_live;
    let ppp = view.response.ctx.pixels_per_point();
    let level = preview_level(app.brush_state.is_drawing || live, app.viewport.zoom, ppp);
    let screen = preview_view(app, view, clip);
    let live_block = live.then_some(screen.block);
    // Tiles last uploaded at a coarser level than we now need go again
    // (and live previews, once the drag is over).
    if !live {
        let stale: Vec<(usize, usize)> = app
            .render_cache
            .preview_tiles
            .iter()
            .filter(|&(_, &uploaded)| uploaded > level)
            .map(|(&tile, _)| tile)
            .collect();
        for (tx, ty) in stale {
            app.mark_tile_dirty(tx, ty);
        }
    }
    let visible = |t: &CanvasTile| screen.contains(t.tx, t.ty);

    refresh_below_cache(app, &visible);

    let canvas = &app.canvas;
    let cache = &app.render_cache;
    let active = canvas.active_layer_idx;
    let below_cache = cache.below_cache.as_ref();
    let quick_mask = app.quick_mask_layer();
    let mut candidates = cache
        .tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.dirty && visible(t))
        .map(|(idx, _)| idx);
    // A preview tile costs about a block's area less.
    let max_tiles = MAX_TILES_PER_FRAME * live_block.map_or(1, |b| b * b);
    let chosen: Vec<usize> = candidates.by_ref().take(max_tiles).collect();
    let filter = &app.workspace.filter;
    let liquify = app.layer_state.liquify.as_ref().filter(|s| s.previewing());
    let more = candidates.next().is_some();
    // Shader layers showing live: each run of layers between them is
    // composited on its own (the others hidden), into its own atlases.
    let shader_live = cache.live.as_deref();
    let views: Vec<Option<crate::canvas::Canvas>> = match shader_live {
        Some(layout) if !chosen.is_empty() => layout
            .runs
            .iter()
            .map(|shown| Some(crate::canvas::shader::run_view(canvas, shown)))
            .collect(),
        Some(_) => Vec::new(),
        None => vec![None],
    };
    let per_run = cache.atlases_x * cache.atlases_y;
    // The below-cache holds everything under the active layer: only the
    // first run may start from it, and only if it has all of that.
    let below_ok = shader_live.is_none_or(|layout| {
        matches!(
            layout.steps.first(),
            Some(crate::canvas::shader::LiveStep::Run { run: 0, .. })
        ) && layout.runs.first().is_some_and(|run| {
            run.get(active) == Some(&true)
                && (0..active).all(|i| run[i] || !canvas.layers[i].visible)
        })
    });
    let uploads: Vec<(usize, Vec<TileUpload>)> = app.workspace.pool.install(|| {
        chosen
            .par_iter()
            .map(|&idx| {
                let tile = &cache.tiles[idx];
                let mut uploads = Vec::new();
                for (run, view) in views.iter().enumerate() {
                    let canvas = view.as_ref().unwrap_or(canvas);
                    let below = below_cache
                        .filter(|_| run == 0 && below_ok)
                        .and_then(|below| below.tiles.get(&(tile.tx, tile.ty)))
                        .map(|pixels| BelowComposite {
                            first_layer: active,
                            pixels,
                        });
                    let start = uploads.len();
                    uploads.extend(tile_run_uploads(
                        canvas, cache, tile, level, live_block, below, quick_mask, filter, liquify,
                    ));
                    for upload in &mut uploads[start..] {
                        upload.atlas += run * per_run;
                    }
                }
                (idx, uploads)
            })
            .collect()
    });

    let cache = &mut app.render_cache;
    for (idx, _) in &uploads {
        let tile = &mut cache.tiles[*idx];
        tile.clear();
        if live {
            // Approximate: redrawn exactly once the drag is over.
            cache.preview_tiles.insert((tile.tx, tile.ty), u32::MAX);
        } else if level > 0 {
            cache.preview_tiles.insert((tile.tx, tile.ty), level);
        } else {
            cache.preview_tiles.remove(&(tile.tx, tile.ty));
        }
    }
    (uploads.into_iter().flat_map(|(_, u)| u).collect(), more)
}

/// One tile of `canvas` (the whole stack, or one run's view of it)
/// composited and cut into its atlas uploads.
#[allow(clippy::too_many_arguments)]
fn tile_run_uploads(
    canvas: &crate::canvas::Canvas,
    cache: &RenderCache,
    tile: &CanvasTile,
    level: u32,
    live_block: Option<usize>,
    below: Option<BelowComposite<'_>>,
    quick_mask: Option<usize>,
    filter: &crate::app::tools::filter::FilterState,
    liquify: Option<&crate::app::tools::liquify::LiquifySession>,
) -> Vec<TileUpload> {
    // Only the part a stroke changed, when that's all that did;
    // aligned to the mip block so downsampling stays exact.
    let tile_size = [
        TILE_SIZE.min(canvas.width() - tile.tx * TILE_SIZE),
        TILE_SIZE.min(canvas.height() - tile.ty * TILE_SIZE),
    ];
    let rect = tile.damage.map_or([0, 0, tile_size[0], tile_size[1]], |d| {
        let block = 1usize << level;
        [
            d[0] / block * block,
            d[1] / block * block,
            d[2].div_ceil(block).saturating_mul(block).min(tile_size[0]),
            d[3].div_ceil(block).saturating_mul(block).min(tile_size[1]),
        ]
    });
    let mut img = egui::ColorImage::new([0, 0], Color32::TRANSPARENT);
    if let Some(block) = live_block {
        let layer = filter
            .preview_pixels(canvas, tile.tx, tile.ty, block)
            .or_else(|| liquify?.preview_pixels(canvas, tile.tx, tile.ty, block));
        canvas.write_tile_preview(tile.tx, tile.ty, block, &mut img, layer);
        let img = preview_at_level(&img, block, tile_size, level);
        let full = [0, 0, tile_size[0], tile_size[1]];
        return tile_uploads(cache, tile.tx, tile.ty, tile_size, full, &img, level);
    }
    if level == 0 {
        canvas.write_tile_rect_to_color_image(tile.tx, tile.ty, rect, &mut img, below);
    } else {
        // Zoomed-out stroke preview: composite and average in one
        // pass, straight at the uploaded mip level.
        let block = 1usize << level;
        canvas.write_tile_rect_downsampled(tile.tx, tile.ty, rect, block, &mut img, below);
    }
    if let Some(mask) = quick_mask {
        crate::app::tools::quick_mask::tint_tile(
            canvas,
            mask,
            tile.tx,
            tile.ty,
            rect,
            1 << level,
            &mut img,
        );
    }
    tile_uploads(cache, tile.tx, tile.ty, tile_size, rect, &img, level)
}

/// Screen position of the canvas's top-left corner and of its center (the
/// rotation pivot).
fn canvas_placement(app: &PainterApp, rect: egui::Rect) -> (egui::Pos2, egui::Pos2) {
    let canvas_size =
        egui::vec2(app.canvas.width() as f32, app.canvas.height() as f32) * app.viewport.zoom;
    let origin = rect.min + egui::vec2(app.viewport.offset.x, app.viewport.offset.y);
    (origin, origin + canvas_size * 0.5)
}

/// Canvas → screen mapping for overlays (selection, transform box, lasso),
/// matching exactly how the canvas itself is drawn: zoom, pan and rotation.
#[derive(Clone, Copy)]
pub struct ScreenMap {
    origin: egui::Pos2,
    center: egui::Pos2,
    zoom: f32,
    cos: f32,
    sin: f32,
    /// Canvas width when the view is flipped (mirrored about the centre).
    flip: Option<f32>,
}

impl ScreenMap {
    pub fn to_screen(self, p: eframe::egui::Vec2) -> egui::Pos2 {
        let p = self.flip.map_or(p, |w| eframe::egui::vec2(w - p.x, p.y));
        PainterApp::rotate_point(self.origin + p * self.zoom, self.center, self.cos, self.sin)
    }

    pub fn zoom(self) -> f32 {
        self.zoom
    }

    /// The canvas point shown at screen point `p` (the inverse of
    /// [`Self::to_screen`]).
    pub fn to_canvas(self, p: egui::Pos2) -> eframe::egui::Vec2 {
        let unrotated = PainterApp::rotate_point(p, self.center, self.cos, -self.sin);
        let q = (unrotated - self.origin) / self.zoom.max(1e-6);
        self.flip.map_or(q, |w| eframe::egui::vec2(w - q.x, q.y))
    }
}

/// The overlay mapping for this frame. Call it after input handling: it
/// uses the zoom and pan input just changed, as the canvas paint does, so
/// overlays don't lag a frame behind the picture (a flicker while zooming).
pub fn screen_map(app: &PainterApp, view: &CanvasView) -> ScreenMap {
    let (origin, center) = canvas_placement(app, view.rect);
    let (sin, cos) = app.viewport.rotation.sin_cos();
    ScreenMap {
        origin,
        center,
        zoom: app.viewport.zoom,
        cos,
        sin,
        flip: app.viewport.flip_x.then_some(app.canvas.width() as f32),
    }
}

/// Allocate the canvas area and reserve its place in the paint order (under
/// overlays drawn later this frame). The GPU paint itself is added by
/// [`paint_canvas`] after input handling.
pub fn draw_canvas(app: &mut PainterApp, ui: &mut egui::Ui) -> CanvasView {
    let (rect, response) = ui.allocate_at_least(ui.available_size(), egui::Sense::click_and_drag());
    let (origin, canvas_center) = canvas_placement(app, rect);
    let slot = ui.painter().add(egui::Shape::Noop);
    CanvasView {
        origin,
        canvas_center,
        response,
        rect,
        slot,
    }
}

/// Everything needed to place the canvas atlases on screen.
#[derive(Clone, Copy)]
struct Placement {
    canvas_size: egui::Vec2,
    zoom: f32,
    origin: egui::Pos2,
    center: egui::Pos2,
    rotation: f32,
    /// Mirror the canvas left to right about its centre.
    flip: bool,
    /// The paint callback's area; quad corners are in its NDC.
    target: egui::Rect,
    /// Wrap-around: the canvas repeated all round.
    wrap: bool,
}

/// One quad per atlas, covering the canvas block that atlas holds.
fn atlas_quads(placement: &Placement, atlases_x: usize, atlases_y: usize) -> Vec<AtlasQuad> {
    let (cos, sin) = (placement.rotation.cos(), placement.rotation.sin());
    let atlas = ATLAS_SIZE as f32;
    let target = placement.target;
    let to_ndc = |canvas: egui::Pos2| {
        let canvas = if placement.flip {
            egui::pos2(placement.canvas_size.x - canvas.x, canvas.y)
        } else {
            canvas
        };
        let screen = PainterApp::rotate_point(
            placement.origin + canvas.to_vec2() * placement.zoom,
            placement.center,
            cos,
            sin,
        );
        [
            2.0 * (screen.x - target.min.x) / target.width() - 1.0,
            1.0 - 2.0 * (screen.y - target.min.y) / target.height(),
        ]
    };
    // Wrap-around: the canvas and its eight neighbours.
    let repeats: &[(f32, f32)] = if placement.wrap {
        &[
            (0.0, 0.0),
            (-1.0, -1.0),
            (0.0, -1.0),
            (1.0, -1.0),
            (-1.0, 0.0),
            (1.0, 0.0),
            (-1.0, 1.0),
            (0.0, 1.0),
            (1.0, 1.0),
        ]
    } else {
        &[(0.0, 0.0)]
    };
    let mut quads = Vec::with_capacity(atlases_x * atlases_y * repeats.len());
    for &(rx, ry) in repeats {
        let shift = egui::vec2(rx * placement.canvas_size.x, ry * placement.canvas_size.y);
        for ay in 0..atlases_y {
            for ax in 0..atlases_x {
                let (x0, y0) = (ax as f32 * atlas, ay as f32 * atlas);
                let w = atlas.min(placement.canvas_size.x - x0);
                let h = atlas.min(placement.canvas_size.y - y0);
                if w <= 0.0 || h <= 0.0 {
                    continue;
                }
                let texture = ATLAS_TEXTURE_SIZE as f32;
                let border = ATLAS_BORDER as f32 / texture;
                let (u, v) = (border + w / texture, border + h / texture);
                let corner = |x: f32, y: f32| to_ndc(egui::pos2(x, y) + shift);
                quads.push(AtlasQuad {
                    atlas: ay * atlases_x + ax,
                    corners: [
                        corner(x0, y0),
                        corner(x0 + w, y0),
                        corner(x0 + w, y0 + h),
                        corner(x0, y0 + h),
                    ],
                    uvs: [[border, border], [u, border], [u, v], [border, v]],
                });
            }
        }
    }
    quads
}

/// Fill the canvas's reserved slot with this frame's GPU paint: the tile
/// uploads plus the atlas quads, placed with the viewport as it is *after*
/// input handling, so a pan or zoom shows up in the same frame.
pub fn paint_canvas(app: &PainterApp, ui: &egui::Ui, view: &CanvasView, uploads: Vec<TileUpload>) {
    let target = ui.clip_rect().intersect(view.rect);
    if !target.is_positive() {
        return;
    }
    let (origin, center) = canvas_placement(app, view.rect);
    let placement = Placement {
        canvas_size: egui::vec2(app.canvas.width() as f32, app.canvas.height() as f32),
        zoom: app.viewport.zoom,
        origin,
        center,
        rotation: app.viewport.rotation,
        flip: app.viewport.flip_x,
        target,
        wrap: app.workspace.wrap_around,
    };
    let cache = &app.render_cache;
    let per_run = cache.atlases_x * cache.atlases_y;
    let runs = cache
        .live
        .as_ref()
        .map_or(1, |layout| layout.runs.len().max(1));
    let run_quads = atlas_quads(&placement, cache.atlases_x, cache.atlases_y);
    let quads = (0..runs)
        .flat_map(|run| {
            run_quads.iter().map(move |quad| AtlasQuad {
                atlas: quad.atlas + run * per_run,
                ..*quad
            })
        })
        .collect();
    let compose = cache.live.as_ref().map(|layout| {
        let ppp = ui.ctx().pixels_per_point();
        ComposePlan {
            size: [
                (target.width() * ppp).round().max(1.0) as u32,
                (target.height() * ppp).round().max(1.0) as u32,
            ],
            gamma: app.canvas.blend_space == crate::canvas::blend_modes::BlendSpace::Gamma,
            per_run,
            steps: app.shader_compose_steps(layout),
            frame: shader_frame(app, view, target, ppp),
        }
    });
    let paint = CanvasPaint {
        generation: cache.texture_generation,
        atlas_count: per_run * runs,
        uploads: std::sync::Mutex::new(uploads),
        quads,
        compose,
    };
    ui.painter().set(
        view.slot,
        egui_wgpu::Callback::new_paint_callback(target, paint),
    );
}

/// The live shaders' uniforms for this frame: the render target (the
/// canvas's paint area, `ppp` pixels per point) mapped to canvas pixels and
/// back.
fn shader_frame(
    app: &PainterApp,
    view: &CanvasView,
    target: egui::Rect,
    ppp: f32,
) -> crate::canvas::shader::FrameUniforms {
    let map = screen_map(app, view);
    let mut frame = app.shader_frame_base();
    // Target pixel -> canvas pixel.
    let at = |px: f32, py: f32| map.to_canvas(target.min + egui::vec2(px, py) / ppp);
    let (c0, cx, cy) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0));
    let (dx, dy) = (cx - c0, cy - c0);
    frame.to_canvas_x = [dx.x, dy.x, c0.x, 0.0];
    frame.to_canvas_y = [dx.y, dy.y, c0.y, 0.0];
    // Canvas pixel -> texture coordinate in the target.
    let uv = |c: egui::Vec2| {
        let s = map.to_screen(c) - target.min;
        egui::vec2(s.x / target.width(), s.y / target.height())
    };
    let s0 = uv(egui::Vec2::ZERO);
    let (sx, sy) = (uv(egui::vec2(1.0, 0.0)) - s0, uv(egui::vec2(0.0, 1.0)) - s0);
    frame.to_channel_x = [sx.x, sy.x, s0.x, 0.0];
    frame.to_channel_y = [sx.y, sy.y, s0.y, 0.0];
    frame.flags[0] = if app.workspace.wrap_around { 1.0 } else { 0.0 };
    frame
}

#[cfg(test)]
mod tests;
