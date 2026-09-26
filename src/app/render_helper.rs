use crate::PainterApp;
use crate::app::gpu_canvas::{ATLAS_BORDER, ATLAS_TEXTURE_SIZE, AtlasQuad, CanvasPaint, MIP_LEVELS, TileUpload};
use crate::app::painter_state::{BelowCache, RenderCache};
use crate::app::state::{ATLAS_SIZE, CanvasTile, TILE_SIZE};
use crate::canvas::blend::{color32_to_linear, rgba_to_color32_fast};
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
    if !app.brush_state.is_drawing || active < 2 {
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
            .map(|&(tx, ty)| ((tx, ty), canvas.composite_below(active, tx as i32, ty as i32)))
            .collect()
    });
    cache.tiles.extend(computed);
}

/// Mip level to upload strokes at: while painting zoomed out, only the level
/// on screen (and coarser) is updated, like Krita's Instant Preview; full
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

/// `img` shrunk by `2^level`, each pixel the average of its block in linear
/// light (partial blocks at the canvas edge average the pixels they have).
fn downsample(img: &egui::ColorImage, level: u32) -> egui::ColorImage {
    if level == 0 {
        return img.clone();
    }
    let block = 1usize << level;
    let [w, h] = img.size;
    let (out_w, out_h) = (w.div_ceil(block), h.div_ceil(block));
    let mut out = egui::ColorImage::new([out_w, out_h], Color32::TRANSPARENT);
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = [0.0f32; 4];
            let mut count = 0.0;
            for y in oy * block..((oy + 1) * block).min(h) {
                for x in ox * block..((ox + 1) * block).min(w) {
                    let p = color32_to_linear(img.pixels[y * w + x]);
                    for (s, v) in sum.iter_mut().zip(p.to_array()) {
                        *s += v;
                    }
                    count += 1.0;
                }
            }
            let [r, g, b, a] = sum.map(|v| v / count);
            out.pixels[oy * out_w + ox] =
                rgba_to_color32_fast(egui::Rgba::from_rgba_premultiplied(r, g, b, a));
        }
    }
    out
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
fn tile_destinations(cache: &RenderCache, tx: usize, ty: usize, tile_size: [usize; 2]) -> Vec<Destination> {
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

/// The uploads for one composited tile at mip `level` (`img` already shrunk).
fn tile_uploads(cache: &RenderCache, tx: usize, ty: usize, full_size: [usize; 2], img: &egui::ColorImage, level: u32) -> Vec<TileUpload> {
    let scale = 1usize << level;
    tile_destinations(cache, tx, ty, full_size)
        .into_iter()
        .map(|dest| {
            let src = dest.src.map(|v| v / scale);
            let end = [0, 1].map(|a| ((dest.src[a] + dest.size[a]).div_ceil(scale)).min(img.size[a]));
            let [w, h] = [end[0] - src[0], end[1] - src[1]];
            let mut pixels = Vec::with_capacity(w * h * 4);
            for y in src[1]..end[1] {
                let row = &img.pixels[y * img.size[0] + src[0]..y * img.size[0] + end[0]];
                pixels.extend(row.iter().flat_map(|p| p.to_array()));
            }
            TileUpload {
                atlas: dest.atlas,
                level,
                x: (dest.dst[0] / scale) as u32,
                y: (dest.dst[1] / scale) as u32,
                width: w as u32,
                height: h as u32,
                pixels,
            }
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
    let corners = [clip.left_top(), clip.right_top(), clip.right_bottom(), clip.left_bottom()].map(&to_canvas);
    let bounds = egui::Rect::from_points(&corners);
    let tile = TILE_SIZE as f32;
    let span = |min: f32, max: f32, count: usize| {
        let start = (min / tile).floor().max(0.0) as usize;
        let end = ((max / tile).floor() + 1.0).clamp(0.0, count as f32) as usize;
        start.min(end)..end
    };
    (span(bounds.min.x, bounds.max.x, tiles_x), span(bounds.min.y, bounds.max.y, tiles_y))
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
    let level = preview_level(
        app.brush_state.is_drawing,
        app.viewport.zoom,
        view.response.ctx.pixels_per_point(),
    );
    // Tiles last uploaded at a coarser level than we now need go again.
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

    let (origin, center) = canvas_placement(app, view.rect);
    let (cos, sin) = (app.viewport.rotation.cos(), app.viewport.rotation.sin());
    let zoom = app.viewport.zoom;
    let (visible_x, visible_y) = visible_tile_range(
        clip.intersect(view.rect),
        |p| (PainterApp::rotate_point(p, center, cos, -sin) - origin).to_pos2() / zoom,
        app.render_cache.tiles_x,
        app.render_cache.tiles_y,
    );
    let visible = |t: &CanvasTile| visible_x.contains(&t.tx) && visible_y.contains(&t.ty);

    refresh_below_cache(app, &visible);

    let canvas = &app.canvas;
    let cache = &app.render_cache;
    let active = canvas.active_layer_idx;
    let below_cache = cache.below_cache.as_ref();
    let mut candidates = cache
        .tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.dirty && visible(t))
        .map(|(idx, _)| idx);
    let chosen: Vec<usize> = candidates.by_ref().take(MAX_TILES_PER_FRAME).collect();
    let more = candidates.next().is_some();
    let uploads: Vec<(usize, Vec<TileUpload>)> = app.workspace.pool.install(|| {
        chosen
            .par_iter()
            .map(|&idx| {
                let tile = &cache.tiles[idx];
                let below = below_cache
                    .and_then(|below| below.tiles.get(&(tile.tx, tile.ty)))
                    .map(|pixels| BelowComposite {
                        first_layer: active,
                        pixels,
                    });
                let mut img = egui::ColorImage::new([0, 0], Color32::TRANSPARENT);
                canvas.write_tile_to_color_image(tile.tx, tile.ty, &mut img, 1, below);
                let full_size = img.size;
                let img = downsample(&img, level);
                (idx, tile_uploads(cache, tile.tx, tile.ty, full_size, &img, level))
            })
            .collect()
    });

    let cache = &mut app.render_cache;
    for (idx, _) in &uploads {
        let tile = &mut cache.tiles[*idx];
        tile.dirty = false;
        if level > 0 {
            cache.preview_tiles.insert((tile.tx, tile.ty), level);
        } else {
            cache.preview_tiles.remove(&(tile.tx, tile.ty));
        }
    }
    (uploads.into_iter().flat_map(|(_, u)| u).collect(), more)
}

/// Screen position of the canvas's top-left corner and of its center (the
/// rotation pivot).
fn canvas_placement(app: &PainterApp, rect: egui::Rect) -> (egui::Pos2, egui::Pos2) {
    let canvas_size =
        egui::vec2(app.canvas.width() as f32, app.canvas.height() as f32) * app.viewport.zoom;
    let origin = rect.min + egui::vec2(app.viewport.offset.x, app.viewport.offset.y);
    (origin, origin + canvas_size * 0.5)
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
struct Placement {
    canvas_size: egui::Vec2,
    zoom: f32,
    origin: egui::Pos2,
    center: egui::Pos2,
    rotation: f32,
    /// The paint callback's area; quad corners are in its NDC.
    target: egui::Rect,
}

/// One quad per atlas, covering the canvas block that atlas holds.
fn atlas_quads(placement: &Placement, atlases_x: usize, atlases_y: usize) -> Vec<AtlasQuad> {
    let (cos, sin) = (placement.rotation.cos(), placement.rotation.sin());
    let atlas = ATLAS_SIZE as f32;
    let target = placement.target;
    let to_ndc = |canvas: egui::Pos2| {
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
    let mut quads = Vec::with_capacity(atlases_x * atlases_y);
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
            quads.push(AtlasQuad {
                atlas: ay * atlases_x + ax,
                corners: [
                    to_ndc(egui::pos2(x0, y0)),
                    to_ndc(egui::pos2(x0 + w, y0)),
                    to_ndc(egui::pos2(x0 + w, y0 + h)),
                    to_ndc(egui::pos2(x0, y0 + h)),
                ],
                uvs: [[border, border], [u, border], [u, v], [border, v]],
            });
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
        target,
    };
    let cache = &app.render_cache;
    let paint = CanvasPaint {
        generation: cache.texture_generation,
        atlas_count: cache.atlases_x * cache.atlases_y,
        uploads: std::sync::Mutex::new(uploads),
        quads: atlas_quads(&placement, cache.atlases_x, cache.atlases_y),
    };
    ui.painter()
        .set(view.slot, egui_wgpu::Callback::new_paint_callback(target, paint));
}

#[cfg(test)]
mod tests {
    use super::*;

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
            target,
        };
        let quads = atlas_quads(&placement, 2, 1);
        assert_eq!(quads.len(), 2);

        let split = 2.0 * ATLAS_SIZE as f32 / 3000.0 - 1.0;
        let close = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5;
        let left = quads[0];
        assert_eq!(left.atlas, 0);
        assert!(close(left.corners[0], [-1.0, 1.0]));
        assert!(close(left.corners[2], [split, -1.0]));
        let texture = ATLAS_TEXTURE_SIZE as f32;
        let border = ATLAS_BORDER as f32 / texture;
        assert_eq!(left.uvs[0], [border, border]);
        assert_eq!(left.uvs[2], [border + ATLAS_SIZE as f32 / texture, border + 1000.0 / texture]);

        let right = quads[1];
        assert_eq!(right.atlas, 1);
        assert!(close(right.corners[0], [split, 1.0]));
        assert!(close(right.corners[2], [1.0, -1.0]));
        assert_eq!(
            right.uvs[2],
            [border + (3000.0 - ATLAS_SIZE as f32) / texture, border + 1000.0 / texture]
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
                Destination { atlas: 0, dst: [b + edge, b + edge], src: [0, 0], size: [TILE_SIZE, TILE_SIZE] },
                Destination { atlas: 1, dst: [0, b + edge], src: [strip, 0], size: [b, TILE_SIZE] },
                Destination { atlas: 2, dst: [b + edge, 0], src: [0, strip], size: [TILE_SIZE, b] },
                Destination { atlas: 3, dst: [0, 0], src: [strip, strip], size: [b, b] },
            ]
        );

        // Left-edge tile of atlas 1 mirrors into atlas 0's right border only.
        let left = tile_destinations(&cache, 32, 0, [TILE_SIZE, TILE_SIZE]);
        assert_eq!(
            left,
            vec![
                Destination { atlas: 0, dst: [b + ATLAS_SIZE, b], src: [0, 0], size: [b, TILE_SIZE] },
                Destination { atlas: 1, dst: [b, b], src: [0, 0], size: [TILE_SIZE, TILE_SIZE] },
            ]
        );
    }

    #[test]
    fn preview_uploads_are_scaled_to_their_level() {
        let cache = RenderCache::new(4096, 4096);
        let full = [TILE_SIZE, TILE_SIZE];
        let img = egui::ColorImage::new([TILE_SIZE / 4, TILE_SIZE / 4], Color32::RED);
        let uploads = tile_uploads(&cache, 31, 31, full, &img, 2);
        let summary: Vec<_> = uploads.iter().map(|u| (u.atlas, u.level, u.x, u.y, u.width, u.height)).collect();
        let (slot, border) = ((ATLAS_BORDER + ATLAS_SIZE - TILE_SIZE) as u32 / 4, ATLAS_BORDER as u32 / 4);
        assert_eq!(
            summary,
            vec![(0, 2, slot, slot, 16, 16), (1, 2, 0, slot, border, 16), (2, 2, slot, 0, 16, border), (3, 2, 0, 0, border, border)]
        );
        assert!(uploads.iter().all(|u| u.pixels.len() == (u.width * u.height * 4) as usize));
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
        assert_eq!(preview_level(false, 0.1, 1.0), 0, "full resolution when not drawing");
        assert_eq!(preview_level(true, 1.0, 1.0), 0);
        assert_eq!(preview_level(true, 0.5, 1.0), 0, "exactly 2 texels/pixel keeps a margin");
        assert_eq!(preview_level(true, 0.3, 1.0), 1);
        // The user's case: a 1.375x display at 0.1 zoom samples ~log2(7.3) = 2.9,
        // so level 3 would leave level 2 stale.
        assert_eq!(preview_level(true, 0.1, 1.375), 2);
        assert_eq!(preview_level(true, 0.02, 1.0), MIP_LEVELS - 1);
        for zoom in [0.05f32, 0.1, 0.2, 0.33, 0.5, 0.7] {
            for ppp in [1.0f32, 1.25, 1.5, 2.0] {
                let sampled = (1.0 / (zoom * ppp)).log2().max(0.0);
                assert!(preview_level(true, zoom, ppp) as f32 <= sampled.floor(), "zoom {zoom} ppp {ppp}");
            }
        }
    }
}
