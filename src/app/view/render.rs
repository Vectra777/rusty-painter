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
    let live = app.workspace.filter.live();
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
                        canvas, cache, tile, level, live_block, below, quick_mask, filter,
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
        let layer = filter.preview_pixels(canvas, tile.tx, tile.ty, block);
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
mod tests {
    /// Drives the real screen-update path (`update_dirty_textures`) over a
    /// few frames and applies its uploads to an in-memory atlas, then checks
    /// the atlas shows exactly the layers' composite.
    struct ScreenSim {
        ctx: egui::Context,
        atlas: std::collections::HashMap<usize, Vec<[u8; 4]>>,
        /// The pointer is down (a slider being dragged).
        dragging: bool,
    }

    impl ScreenSim {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                atlas: Default::default(),
                dragging: false,
            }
        }

        /// Run frames until no dirty tiles are left.
        fn settle(&mut self, app: &mut PainterApp) {
            self.run_frames(app, true);
        }

        /// Run frames until no dirty tiles are left, without keeping the
        /// uploads; returns how many it took and the time spent updating.
        fn settle_counting(&mut self, app: &mut PainterApp) -> (usize, std::time::Duration) {
            self.run_frames(app, false)
        }

        fn run_frames(&mut self, app: &mut PainterApp, keep: bool) -> (usize, std::time::Duration) {
            use crate::app::view::gpu_canvas::ATLAS_TEXTURE_SIZE;
            let mut spent = std::time::Duration::ZERO;
            for frame in 1..=50 {
                let mut more = false;
                let mut uploads = Vec::new();
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    ..Default::default()
                };
                let _ = self.ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let view = draw_canvas(app, ui);
                        let started = std::time::Instant::now();
                        // As the frame does: a filter's run, then the screen.
                        let screen = preview_view(app, &view, view.rect);
                        app.filter_update(self.dragging.then_some(&screen));
                        let (u, m) = update_dirty_textures(app, &view, view.rect);
                        spent += started.elapsed();
                        uploads = u;
                        more = m;
                    });
                });
                if !keep {
                    uploads.clear();
                }
                for up in uploads {
                    if up.level > 0 {
                        // (Only level 0 is simulated.)
                        assert!(self.dragging, "not drawing: full resolution");
                        continue;
                    }
                    let tex = self
                        .atlas
                        .entry(up.atlas)
                        .or_insert_with(|| vec![[0; 4]; ATLAS_TEXTURE_SIZE * ATLAS_TEXTURE_SIZE]);
                    for row in 0..up.height as usize {
                        for col in 0..up.width as usize {
                            let s = (row * up.width as usize + col) * 4;
                            let d =
                                (up.y as usize + row) * ATLAS_TEXTURE_SIZE + up.x as usize + col;
                            tex[d].copy_from_slice(&up.pixels[s..s + 4]);
                        }
                    }
                }
                if !more && !app.render_cache.tiles.iter().any(|t| t.dirty) {
                    return (frame, spent);
                }
            }
            panic!("screen never settled");
        }

        /// Canvas pixels whose on-screen texel differs from the composite.
        fn mismatches(&self, app: &PainterApp) -> Vec<(usize, usize)> {
            use crate::app::view::gpu_canvas::{ATLAS_BORDER, ATLAS_TEXTURE_SIZE};
            let img = app.canvas.flatten();
            let (w, h) = (app.canvas.width(), app.canvas.height());
            let mut bad = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let (atlas, lx, ly) = app.render_cache.atlas_slot(x / TILE_SIZE, y / TILE_SIZE);
                    let texel = (ATLAS_BORDER + ly + y % TILE_SIZE) * ATLAS_TEXTURE_SIZE
                        + ATLAS_BORDER
                        + lx
                        + x % TILE_SIZE;
                    let shown = self.atlas.get(&atlas).map_or([0; 4], |t| t[texel]);
                    if shown != img.pixels[y * w + x].to_array() {
                        bad.push((x, y));
                    }
                }
            }
            bad
        }
    }

    /// The paint layer's pixels, tile by tile.
    fn layer_pixels(app: &PainterApp, idx: usize) -> Vec<Vec<Color32>> {
        let mut keys = app.canvas.layer_tile_keys(idx);
        keys.sort_unstable();
        keys.into_iter()
            .filter_map(|(tx, ty)| app.canvas.get_layer_tile_data(idx, tx, ty))
            .collect()
    }

    #[test]
    fn a_dragged_filter_previews_on_screen_and_ends_exactly_as_before() {
        use crate::canvas::filters::Filter;
        use crate::selection::{SelectionMode, SelectionShape};
        for filter in [
            Filter::HueSaturation {
                hue: 70.0,
                saturation: 0.3,
                lightness: 0.1,
            },
            Filter::GaussianBlur { radius: 6.0 },
        ] {
            let setup = || {
                let mut app = painted(512);
                app.viewport.zoom = 0.25;
                app.viewport.offset = eframe::egui::Vec2::ZERO;
                // A soft selection: only part changes, its edge blends.
                app.selection_manager.apply_shape(
                    SelectionShape::Circle {
                        center: eframe::egui::Vec2::new(250.0, 240.0),
                        radius: 180.0,
                    },
                    SelectionMode::Replace,
                );
                app
            };
            // The result as a filter always gave it: run once, then kept.
            let mut direct = setup();
            direct.filter_open(filter);
            direct.filter_commit();
            let expected = layer_pixels(&direct, 1);

            let mut app = setup();
            let mut screen = ScreenSim::new();
            screen.settle(&mut app);
            let before = layer_pixels(&app, 1);
            app.filter_open(Filter::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            });
            screen.settle(&mut app);
            app.workspace.filter.session.as_mut().unwrap().set_slow();
            screen.dragging = true;
            for step in [0.2_f32, 0.6, 1.0] {
                // Part way there, then the value itself.
                let session = app.workspace.filter.session.as_mut().unwrap();
                session.filter = if step < 1.0 {
                    Filter::GaussianBlur { radius: step }
                } else {
                    filter
                };
                session.dirty = true;
                let (frames, _) = screen.settle_counting(&mut app);
                assert_eq!(frames, 1, "{filter:?}: all on screen in one frame");
                assert!(app.workspace.filter.live());
            }
            assert!(
                layer_pixels(&app, 1) == before,
                "{filter:?}: the preview leaves the layer as it was"
            );
            screen.dragging = false;
            screen.settle(&mut app);
            assert!(!app.workspace.filter.live());
            assert!(
                screen.mismatches(&app).is_empty(),
                "{filter:?}: let go, the screen shows the layer exactly"
            );
            app.filter_commit();
            assert!(
                layer_pixels(&app, 1) == expected,
                "{filter:?}: kept as before"
            );
            assert_eq!(app.layer_state.history.stacks().0.len(), 1, "one step");
            app.apply_history(false);
            assert!(layer_pixels(&app, 1) == before, "{filter:?}: undone");
        }
    }

    #[test]
    fn a_dragged_adjustment_previews_then_shows_exactly() {
        use crate::canvas::filters::Filter;
        let mut app = painted(512);
        app.viewport.zoom = 0.25;
        app.viewport.offset = eframe::egui::Vec2::ZERO;
        let mut screen = ScreenSim::new();
        app.add_adjustment_layer(Filter::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
        });
        app.add_mask_to_active();
        screen.settle(&mut app);
        let idx = app
            .canvas
            .layer_index_of(app.workspace.filter.editing.unwrap());
        screen.dragging = true;
        for gamma in [1.5, 2.0, 3.0] {
            app.workspace.filter.adjusting = true;
            app.canvas_mut().layers[idx.unwrap()].adjustment = Some(Filter::Levels {
                black: 0.1,
                white: 0.9,
                gamma,
            });
            app.mark_all_tiles_dirty();
            let (frames, _) = screen.settle_counting(&mut app);
            assert_eq!(frames, 1, "all on screen in one frame");
        }
        screen.dragging = false;
        screen.settle(&mut app);
        assert!(!app.workspace.filter.adjusting);
        assert!(screen.mismatches(&app).is_empty(), "let go: exact");
    }

    #[test]
    fn screen_matches_the_layers_after_liquify_undo_redo() {
        use crate::app::tools::Tool;
        use crate::canvas::liquify::LiquifyMode;
        let canvas = crate::canvas::Canvas::new(512, 384, Color32::WHITE, TILE_SIZE);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.recreate_render_cache(512, 384);
        app.canvas_mut().active_layer_idx = 1;
        app.viewport.zoom = 1.0;
        app.viewport.offset = eframe::egui::Vec2::ZERO;
        app.workspace.auto_fit = false;
        let mut screen = ScreenSim::new();
        screen.settle(&mut app);
        assert!(screen.mismatches(&app).is_empty(), "blank canvas");

        for (color, y) in [
            (Color32::from_rgb(210, 40, 40), 150.0),
            (Color32::from_rgb(210, 100, 40), 200.0),
        ] {
            app.brush_state.brush.brush_options.color = color;
            app.brush_state.brush.brush_options.diameter = 70.0;
            app.start_stroke_with_pressure(eframe::egui::Vec2::new(30.0, y), 1.0);
            for i in 1..=30 {
                app.add_stroke_point(eframe::egui::Vec2::new(30.0 + i as f32 * 14.0, y), 1.0);
                app.stroke_worker.wait_idle();
                app.sync_stroke_worker();
                screen.settle(&mut app);
            }
            app.finish_stroke();
            app.settle_strokes();
            screen.settle(&mut app);
        }
        assert!(screen.mismatches(&app).is_empty(), "after strokes");

        app.active_tool = Tool::Liquify;
        app.workspace.liquify.radius = 80.0;
        for (mode, y) in [
            (LiquifyMode::Push, 240.0),
            (LiquifyMode::TwirlCw, 220.0),
            (LiquifyMode::Bloat, 200.0),
        ] {
            app.workspace.liquify.mode = mode;
            app.liquify_press(eframe::egui::Vec2::new(150.0, y));
            screen.settle(&mut app);
            for i in 1..=12 {
                app.liquify_drag(eframe::egui::Vec2::new(
                    150.0 + i as f32 * 9.0,
                    y - i as f32 * 3.0,
                ));
                app.liquify_hold(1.0 / 60.0);
                screen.settle(&mut app);
            }
            app.liquify_release();
        }
        assert!(screen.mismatches(&app).is_empty(), "after liquify");
        for (step, redo) in [
            ("undo", false),
            ("redo", true),
            ("undo again", false),
            ("undo stroke", false),
        ] {
            app.apply_history(redo);
            screen.settle(&mut app);
            let bad = screen.mismatches(&app);
            assert!(
                bad.is_empty(),
                "{step}: {} stale pixels, e.g. {:?}",
                bad.len(),
                &bad[..bad.len().min(5)]
            );
        }
    }

    use super::*;
    use crate::canvas::blend::downsample;

    #[test]
    fn with_a_live_shader_layer_each_run_fills_its_own_atlases() {
        use crate::app::view::gpu_canvas::{ATLAS_BORDER, ATLAS_TEXTURE_SIZE};
        use crate::canvas::shader::{self, LiveStatus};
        let mut app = painted(160);
        app.viewport.zoom = 1.0;
        app.viewport.offset = eframe::egui::Vec2::ZERO;
        let (name, source) = shader::TEMPLATES[0];
        app.add_shader_layer(name, source).unwrap();
        // A translucent paint layer above the shader layer.
        let above = app
            .add_layer_with_tiles("Above".into(), Vec::new(), |_| {})
            .unwrap();
        let dot: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
            .map(|i| {
                if (i % TILE_SIZE) < 20 {
                    Color32::from_rgba_unmultiplied(200, 30, 30, 120)
                } else {
                    Color32::TRANSPARENT
                }
            })
            .collect();
        app.canvas.set_layer_tile_data(above, 1, 1, dot);
        let LiveStatus::Live(layout) = shader::live_layout(&app.canvas) else {
            panic!("expected a live layout");
        };
        assert_eq!(layout.runs.len(), 2);
        app.render_cache.live = Some(std::sync::Arc::new(layout.clone()));
        app.mark_all_tiles_dirty();

        let mut screen = ScreenSim::new();
        screen.settle(&mut app);
        let per_run = app.render_cache.atlases_x * app.render_cache.atlases_y;
        let (w, h) = (app.canvas.width(), app.canvas.height());
        for (run, shown) in layout.runs.iter().enumerate() {
            let img = shader::run_view(&app.canvas, shown).flatten();
            for y in 0..h {
                for x in 0..w {
                    let (atlas, lx, ly) = app.render_cache.atlas_slot(x / TILE_SIZE, y / TILE_SIZE);
                    let texel = (ATLAS_BORDER + ly + y % TILE_SIZE) * ATLAS_TEXTURE_SIZE
                        + ATLAS_BORDER
                        + lx
                        + x % TILE_SIZE;
                    let shown = screen
                        .atlas
                        .get(&(atlas + run * per_run))
                        .map_or([0; 4], |t| t[texel]);
                    assert_eq!(
                        shown,
                        img.pixels[y * w + x].to_array(),
                        "run {run} at {x},{y}"
                    );
                }
            }
        }
        // The run above holds only the translucent layer: the shader
        // layer's own (baked) pixels are in neither run.
        let top = shader::run_view(&app.canvas, &layout.runs[1]).flatten();
        assert_eq!(top.pixels[0], Color32::TRANSPARENT);
    }

    #[test]
    fn atlas_quads_cover_the_canvas_in_target_ndc() {
        // A 3000x1000 canvas (two atlases wide) drawn 1:1 filling the target.
        let target = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(3000.0, 1000.0));
        let placement = Placement {
            canvas_size: egui::vec2(3000.0, 1000.0),
            zoom: 1.0,
            origin: target.min,
            center: target.center(),
            rotation: 0.0,
            flip: false,
            target,
            wrap: false,
        };
        let quads = atlas_quads(&placement, 2, 1);
        assert_eq!(quads.len(), 2);
        // Wrap-around: the canvas repeated round itself, the first repeat
        // one canvas to the right of it.
        let wrapped = atlas_quads(
            &Placement {
                wrap: true,
                ..placement
            },
            2,
            1,
        );
        assert_eq!(wrapped.len(), 18);
        let right_of = &wrapped[2 * 5];
        assert!((right_of.corners[0][0] - quads[0].corners[0][0] - 2.0).abs() < 1e-4);

        let split = 2.0 * ATLAS_SIZE as f32 / 3000.0 - 1.0;
        let close =
            |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5;
        let left = quads[0];
        assert_eq!(left.atlas, 0);
        assert!(close(left.corners[0], [-1.0, 1.0]));
        assert!(close(left.corners[2], [split, -1.0]));
        let texture = ATLAS_TEXTURE_SIZE as f32;
        let border = ATLAS_BORDER as f32 / texture;
        assert_eq!(left.uvs[0], [border, border]);
        assert_eq!(
            left.uvs[2],
            [
                border + ATLAS_SIZE as f32 / texture,
                border + 1000.0 / texture
            ]
        );

        let right = quads[1];
        assert_eq!(right.atlas, 1);
        assert!(close(right.corners[0], [split, 1.0]));
        assert!(close(right.corners[2], [1.0, -1.0]));
        assert_eq!(
            right.uvs[2],
            [
                border + (3000.0 - ATLAS_SIZE as f32) / texture,
                border + 1000.0 / texture
            ]
        );
    }

    #[test]
    fn tile_destinations_mirror_edges_into_neighbouring_atlas_borders() {
        let cache = RenderCache::new(4096, 4096); // 2x2 atlases
        let b = ATLAS_BORDER;
        let edge = ATLAS_SIZE - TILE_SIZE;

        // Bottom-right tile of atlas 0: its slot, plus the right, bottom and
        // diagonal neighbours' borders.
        let corner = tile_destinations(&cache, 31, 31, [TILE_SIZE, TILE_SIZE]);
        let strip = TILE_SIZE - b;
        assert_eq!(
            corner,
            vec![
                Destination {
                    atlas: 0,
                    dst: [b + edge, b + edge],
                    src: [0, 0],
                    size: [TILE_SIZE, TILE_SIZE]
                },
                Destination {
                    atlas: 1,
                    dst: [0, b + edge],
                    src: [strip, 0],
                    size: [b, TILE_SIZE]
                },
                Destination {
                    atlas: 2,
                    dst: [b + edge, 0],
                    src: [0, strip],
                    size: [TILE_SIZE, b]
                },
                Destination {
                    atlas: 3,
                    dst: [0, 0],
                    src: [strip, strip],
                    size: [b, b]
                },
            ]
        );

        // Left-edge tile of atlas 1 mirrors into atlas 0's right border only.
        let left = tile_destinations(&cache, 32, 0, [TILE_SIZE, TILE_SIZE]);
        assert_eq!(
            left,
            vec![
                Destination {
                    atlas: 0,
                    dst: [b + ATLAS_SIZE, b],
                    src: [0, 0],
                    size: [b, TILE_SIZE]
                },
                Destination {
                    atlas: 1,
                    dst: [b, b],
                    src: [0, 0],
                    size: [TILE_SIZE, TILE_SIZE]
                },
            ]
        );
    }

    /// Every atlas texel an upload set writes, keyed by (atlas, level, x, y).
    fn texels(
        uploads: &[TileUpload],
    ) -> std::collections::HashMap<(usize, u32, u32, u32), [u8; 4]> {
        let mut out = std::collections::HashMap::new();
        for u in uploads {
            for y in 0..u.height {
                for x in 0..u.width {
                    let i = ((y * u.width + x) * 4) as usize;
                    let px = [
                        u.pixels[i],
                        u.pixels[i + 1],
                        u.pixels[i + 2],
                        u.pixels[i + 3],
                    ];
                    out.insert((u.atlas, u.level, u.x + x, u.y + y), px);
                }
            }
        }
        out
    }

    #[test]
    fn damaged_rect_uploads_match_the_full_tile_upload() {
        // A corner tile mirrored into three neighbouring atlases' borders.
        let cache = RenderCache::new(4096, 4096);
        let full = [TILE_SIZE, TILE_SIZE];
        let mut tile = egui::ColorImage::new(full, Color32::TRANSPARENT);
        for (i, px) in tile.pixels.iter_mut().enumerate() {
            *px = Color32::from_rgba_premultiplied((i % 251) as u8, (i / 251) as u8, 7, 255);
        }
        for level in [0u32, 1] {
            let full_img = downsample(&tile, level);
            let whole = texels(&tile_uploads(
                &cache,
                31,
                31,
                full,
                [0, 0, TILE_SIZE, TILE_SIZE],
                &full_img,
                level,
            ));
            for rect in [
                [8, 16, 24, 40],
                [TILE_SIZE - 6, 0, TILE_SIZE, 10],
                [0, TILE_SIZE - 4, TILE_SIZE, TILE_SIZE],
            ] {
                // Crop the damaged part, as the redraw does.
                let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
                let mut part = egui::ColorImage::new([w, h], Color32::TRANSPARENT);
                for y in 0..h {
                    for x in 0..w {
                        part.pixels[y * w + x] =
                            tile.pixels[(rect[1] + y) * TILE_SIZE + rect[0] + x];
                    }
                }
                let part_img = downsample(&part, level);
                let partial = texels(&tile_uploads(&cache, 31, 31, full, rect, &part_img, level));
                assert!(!partial.is_empty(), "rect {rect:?} level {level}");
                for (key, px) in &partial {
                    assert_eq!(
                        whole.get(key),
                        Some(px),
                        "rect {rect:?} level {level} texel {key:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn preview_uploads_are_scaled_to_their_level() {
        let cache = RenderCache::new(4096, 4096);
        let full = [TILE_SIZE, TILE_SIZE];
        let img = egui::ColorImage::new([TILE_SIZE / 4, TILE_SIZE / 4], Color32::RED);
        let uploads = tile_uploads(&cache, 31, 31, full, [0, 0, TILE_SIZE, TILE_SIZE], &img, 2);
        let summary: Vec<_> = uploads
            .iter()
            .map(|u| (u.atlas, u.level, u.x, u.y, u.width, u.height))
            .collect();
        let (slot, border) = (
            (ATLAS_BORDER + ATLAS_SIZE - TILE_SIZE) as u32 / 4,
            ATLAS_BORDER as u32 / 4,
        );
        assert_eq!(
            summary,
            vec![
                (0, 2, slot, slot, 16, 16),
                (1, 2, 0, slot, border, 16),
                (2, 2, slot, 0, 16, border),
                (3, 2, 0, 0, border, border)
            ]
        );
        assert!(
            uploads
                .iter()
                .all(|u| u.pixels.len() == (u.width * u.height * 4) as usize)
        );
    }

    #[test]
    fn downsample_averages_in_linear_light() {
        let mut img = egui::ColorImage::new([2, 2], Color32::BLACK);
        img.pixels[0] = Color32::WHITE;
        img.pixels[3] = Color32::WHITE;
        let half = downsample(&img, 1);
        assert_eq!(half.size, [1, 1]);
        // Half white in linear light is sRGB ~188, not the naive 128.
        let [r, g, b, a] = half.pixels[0].to_array();
        assert!((186..=189).contains(&r) && r == g && g == b, "{r}");
        assert_eq!(a, 255);
    }

    #[test]
    fn visible_tile_range_covers_only_tiles_under_clip() {
        let identity = |p: egui::Pos2| p;
        let clip = egui::Rect::from_min_max(egui::pos2(70.0, 10.0), egui::pos2(130.0, 20.0));
        assert_eq!(visible_tile_range(clip, identity, 4, 3), (1..3, 0..1));

        let offscreen = egui::Rect::from_min_max(egui::pos2(-50.0, -50.0), egui::pos2(-1.0, -1.0));
        let (xs, ys) = visible_tile_range(offscreen, identity, 4, 3);
        assert!(xs.is_empty() && ys.is_empty());

        let whole = egui::Rect::from_min_max(egui::pos2(-1e4, -1e4), egui::pos2(1e4, 1e4));
        assert_eq!(visible_tile_range(whole, identity, 4, 3), (0..4, 0..3));
    }

    #[test]
    fn preview_level_never_exceeds_the_level_the_gpu_samples() {
        assert_eq!(
            preview_level(false, 0.1, 1.0),
            0,
            "full resolution when not drawing"
        );
        assert_eq!(preview_level(true, 1.0, 1.0), 0);
        assert_eq!(
            preview_level(true, 0.5, 1.0),
            0,
            "exactly 2 texels/pixel keeps a margin"
        );
        assert_eq!(preview_level(true, 0.3, 1.0), 1);
        // The user's case: a 1.375x display at 0.1 zoom samples ~log2(7.3) = 2.9,
        // so level 3 would leave level 2 stale.
        assert_eq!(preview_level(true, 0.1, 1.375), 2);
        assert_eq!(preview_level(true, 0.02, 1.0), MIP_LEVELS - 1);
        for zoom in [0.05f32, 0.1, 0.2, 0.33, 0.5, 0.7] {
            for ppp in [1.0f32, 1.25, 1.5, 2.0] {
                let sampled = (1.0 / (zoom * ppp)).log2().max(0.0);
                assert!(
                    preview_level(true, zoom, ppp) as f32 <= sampled.floor(),
                    "zoom {zoom} ppp {ppp}"
                );
            }
        }
    }

    #[test]
    fn flipped_view_maps_to_screen_and_back() {
        use super::ScreenMap;
        use crate::canvas::Canvas;
        use eframe::egui::{Color32, Vec2, pos2, vec2};
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(300, 200, Color32::WHITE, 64));
        app.viewport.zoom = 1.7;
        app.viewport.rotation = 0.6;
        app.viewport.flip_x = true;
        let origin = pos2(40.0, 25.0);
        let center = origin + vec2(300.0, 200.0) * 1.7 * 0.5;
        let (sin, cos) = 0.6f32.sin_cos();
        let map = ScreenMap {
            origin,
            center,
            zoom: 1.7,
            cos,
            sin,
            flip: Some(300.0),
        };
        for p in [
            Vec2::new(0.0, 0.0),
            Vec2::new(250.0, 30.0),
            Vec2::new(12.5, 199.0),
        ] {
            let back = app.screen_to_canvas_raw(map.to_screen(p), origin, center);
            assert!((back - p).length() < 1e-3, "{p:?} came back as {back:?}");
            let back = map.to_canvas(map.to_screen(p));
            assert!((back - p).length() < 1e-3, "{p:?} came back as {back:?}");
        }
        // Flipped: the canvas's left edge shows on the right.
        let left = map.to_screen(Vec2::new(0.0, 100.0));
        let right = map.to_screen(Vec2::new(300.0, 100.0));
        let unrotated = |q: eframe::egui::Pos2| (q - center).x * cos + (q - center).y * sin;
        assert!(unrotated(left) > unrotated(right));
    }

    /// A `size`² app whose layer 1 is painted all over (a colour field with
    /// black lines, as `bench_api`'s `painted_app`), seen whole in a
    /// 1200×900 window.
    fn painted(size: usize) -> PainterApp {
        let canvas = crate::canvas::Canvas::new(size, size, Color32::WHITE, TILE_SIZE);
        let tiles = size.div_ceil(TILE_SIZE);
        for ty in 0..tiles {
            for tx in 0..tiles {
                let tile: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
                    .map(|i| {
                        let (x, y) = (
                            tx * TILE_SIZE + i % TILE_SIZE,
                            ty * TILE_SIZE + i / TILE_SIZE,
                        );
                        if x % 50 < 3 || y % 50 < 3 {
                            Color32::BLACK
                        } else {
                            Color32::from_rgb((x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8)
                        }
                    })
                    .collect();
                canvas.set_layer_tile_data(1, tx as i32, ty as i32, tile);
            }
        }
        let mut app = crate::project::tests::test_app_pub(canvas);
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        app.workspace.pool = std::sync::Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap(),
        );
        app.recreate_render_cache(size, size);
        app.canvas_mut().active_layer_idx = 1;
        app.viewport.zoom = 880.0 / size as f32;
        app.viewport.offset = eframe::egui::Vec2::new(150.0, 10.0);
        app.workspace.auto_fit = false;
        app
    }

    #[test]
    #[ignore = "timing: cargo test --release -- --ignored --nocapture"]
    fn adjustment_preview_timing_4k() {
        use crate::canvas::filters::Filter;
        let mut app = painted(4000);
        let mut screen = ScreenSim::new();
        screen.settle(&mut app);
        let hue = |h: f32| Filter::HueSaturation {
            hue: h,
            saturation: 0.1,
            lightness: 0.0,
        };
        let report = |what: &str, ticks: &[(usize, std::time::Duration)]| {
            let frames: usize = ticks.iter().map(|t| t.0).sum();
            let worst = ticks.iter().map(|t| t.1).max().unwrap_or_default();
            let mean = ticks.iter().map(|t| t.1).sum::<std::time::Duration>() / ticks.len() as u32;
            eprintln!(
                "{what}: {:.1} frames a step, {mean:?} a step (worst {worst:?})",
                frames as f32 / ticks.len() as f32
            );
        };
        // A filter's slider dragged: each step, the frames until the
        // screen shows it.
        app.filter_open(hue(10.0));
        screen.settle(&mut app);
        screen.dragging = true;
        let ticks: Vec<_> = (0..10)
            .map(|step| {
                let session = app.workspace.filter.session.as_mut().unwrap();
                session.filter = hue(20.0 + step as f32);
                session.dirty = true;
                screen.settle_counting(&mut app)
            })
            .collect();
        report("filter slider step", &ticks);
        screen.dragging = false;
        let (frames, spent) = screen.settle_counting(&mut app);
        eprintln!("filter slider let go: exact in {frames} frames, {spent:?}");
        app.filter_cancel();
        screen.settle(&mut app);

        // An adjustment layer's slider dragged.
        app.add_adjustment_layer(hue(10.0));
        screen.settle(&mut app);
        let idx = app.canvas.active_layer_idx;
        screen.dragging = true;
        let ticks: Vec<_> = (0..10)
            .map(|step| {
                app.workspace.filter.adjusting = true;
                app.canvas_mut().layers[idx].adjustment = Some(hue(20.0 + step as f32));
                app.mark_all_tiles_dirty();
                screen.settle_counting(&mut app)
            })
            .collect();
        report("adjustment slider step", &ticks);
        screen.dragging = false;
        let (frames, spent) = screen.settle_counting(&mut app);
        eprintln!("adjustment slider let go: exact in {frames} frames, {spent:?}");
    }
}
