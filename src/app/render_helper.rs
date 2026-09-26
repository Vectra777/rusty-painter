use crate::PainterApp;
use crate::app::painter_state::BelowCache;
use crate::app::state::{ATLAS_SIZE, TILE_SIZE};
use crate::canvas::storage::BelowComposite;
use eframe::egui::{self, Color32, TextureOptions};
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};

pub struct CanvasView {
    pub origin: egui::Pos2,
    pub canvas_center: egui::Pos2,
    pub response: egui::Response,
}

/// While a stroke is in progress on a layer with 2+ layers below it, make sure
/// every dirty tile has the composite of those layers cached, so redraws only
/// composite the active layer and the ones above it.
fn refresh_below_cache(app: &mut PainterApp) {
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
        .filter(|t| t.dirty && !cache.tiles.contains_key(&(t.tx, t.ty)))
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

pub fn update_dirty_textures(app: &mut PainterApp) {
    let lod_step = if app.render_cache.disable_lod {
        1
    } else if app.viewport.zoom < 1.0 {
        (1.0 / app.viewport.zoom).ceil() as usize
    } else {
        1
    }
    .clamp(1, TILE_SIZE);

    refresh_below_cache(app);

    let canvas_ref = &app.canvas;
    let active = canvas_ref.active_layer_idx;
    let below_cache = app.render_cache.below_cache.as_ref();
    let dirty_images: Vec<(usize, egui::ColorImage)> = app.workspace.pool.install(|| {
        app.render_cache
            .tiles
            .par_iter()
            .enumerate()
            .filter(|(_, t)| t.dirty)
            .map(|(idx, tile)| {
                let below = below_cache
                    .and_then(|cache| cache.tiles.get(&(tile.tx, tile.ty)))
                    .map(|pixels| BelowComposite {
                        first_layer: active,
                        pixels,
                    });
                let mut img = egui::ColorImage::new([0, 0], Color32::TRANSPARENT);
                canvas_ref.write_tile_to_color_image(tile.tx, tile.ty, &mut img, lod_step, below);
                (idx, img)
            })
            .collect()
    });

    for (idx, img) in dirty_images {
        if let Some(tile) = app.render_cache.tiles.get_mut(idx) {
            let img_w = img.size[0];
            let img_h = img.size[1];
            if let Some(atlas) = app.render_cache.atlases.get_mut(tile.atlas_idx) {
                atlas.texture.set_partial(
                    [tile.atlas_x, tile.atlas_y],
                    img,
                    TextureOptions::NEAREST,
                );
            }
            tile.pixel_w = img_w;
            tile.pixel_h = img_h;
            tile.dirty = false;
        }
    }
}

/// Range of tile columns/rows whose canvas area can overlap `clip` (screen space).
/// `to_canvas` maps a screen point to canvas pixels; the four clip corners are
/// mapped and their bounding box taken, which stays conservative under rotation.
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

pub fn draw_canvas(app: &mut PainterApp, ui: &mut egui::Ui) -> CanvasView {
    let desired_size = egui::vec2(app.canvas.width() as f32, app.canvas.height() as f32);
    let canvas_size = desired_size * app.viewport.zoom;
    let (rect, response) = ui.allocate_at_least(ui.available_size(), egui::Sense::click_and_drag());

    let origin = rect.min + egui::vec2(app.viewport.offset.x, app.viewport.offset.y);
    let canvas_center = origin + canvas_size * 0.5;
    let cos = app.viewport.rotation.cos();
    let sin = app.viewport.rotation.sin();

    let mut meshes: Vec<egui::Mesh> = app
        .render_cache
        .atlases
        .iter()
        .map(|atlas| egui::Mesh::with_texture(atlas.texture.id()))
        .collect();

    let half_texel = 0.5 / ATLAS_SIZE as f32;
    let clip_rect = ui.clip_rect();

    let zoom = app.viewport.zoom;
    let (tx_range, ty_range) = visible_tile_range(
        clip_rect,
        |p| {
            let unrotated = PainterApp::rotate_point(p, canvas_center, cos, -sin);
            ((unrotated - origin) / zoom).to_pos2()
        },
        app.render_cache.tiles_x,
        app.render_cache.tiles_y,
    );
    let tiles_x = app.render_cache.tiles_x;
    let all_tiles = &app.render_cache.tiles;
    let visible_tiles = ty_range.flat_map(|ty| {
        tx_range
            .clone()
            .filter_map(move |tx| all_tiles.get(ty * tiles_x + tx))
    });

    for tile in visible_tiles {
        let x = (tile.tx * TILE_SIZE) as f32 * app.viewport.zoom;
        let y = (tile.ty * TILE_SIZE) as f32 * app.viewport.zoom;

        let tile_w =
            (TILE_SIZE.min(app.canvas.width() - tile.tx * TILE_SIZE)) as f32 * app.viewport.zoom;
        let tile_h =
            (TILE_SIZE.min(app.canvas.height() - tile.ty * TILE_SIZE)) as f32 * app.viewport.zoom;

        let tile_rect =
            egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(tile_w, tile_h));

        let corners = [
            PainterApp::rotate_point(tile_rect.left_top(), canvas_center, cos, sin),
            PainterApp::rotate_point(tile_rect.right_top(), canvas_center, cos, sin),
            PainterApp::rotate_point(tile_rect.right_bottom(), canvas_center, cos, sin),
            PainterApp::rotate_point(tile_rect.left_bottom(), canvas_center, cos, sin),
        ];
        let min_x = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let min_y = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let max_x = corners
            .iter()
            .map(|p| p.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = corners
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let screen_bounds =
            egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y));
        if !screen_bounds.intersects(clip_rect) {
            continue;
        }

        let u0 = (tile.atlas_x as f32 + half_texel) / ATLAS_SIZE as f32;
        let v0 = (tile.atlas_y as f32 + half_texel) / ATLAS_SIZE as f32;
        let u1 = (tile.atlas_x as f32 + tile.pixel_w as f32 - half_texel) / ATLAS_SIZE as f32;
        let v1 = (tile.atlas_y as f32 + tile.pixel_h as f32 - half_texel) / ATLAS_SIZE as f32;

        let uv_coords = [
            egui::Pos2::new(u0, v0),
            egui::Pos2::new(u1, v0),
            egui::Pos2::new(u1, v1),
            egui::Pos2::new(u0, v1),
        ];

        if let Some(mesh) = meshes.get_mut(tile.atlas_idx) {
            let base = mesh.vertices.len() as u32;
            for (corner, uv) in corners.iter().zip(uv_coords.iter()) {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: *corner,
                    uv: *uv,
                    color: Color32::WHITE,
                });
            }
            mesh.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    for mesh in meshes {
        if !mesh.vertices.is_empty() {
            ui.painter().add(mesh);
        }
    }

    CanvasView {
        origin,
        canvas_center,
        response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
