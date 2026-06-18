use crate::PainterApp;
use crate::app::state::{ATLAS_SIZE, TILE_SIZE};
use eframe::egui::{self, Color32, TextureOptions};
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};

pub struct CanvasView {
    pub origin: egui::Pos2,
    pub canvas_center: egui::Pos2,
    pub _cos: f32,
    pub _sin: f32,
    pub response: egui::Response,
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

    let canvas_ref = &app.canvas;
    let dirty_images: Vec<(usize, egui::ColorImage)> = app.workspace.pool.install(|| {
        app.render_cache
            .tiles
            .par_iter()
            .enumerate()
            .filter(|(_, t)| t.dirty)
            .map(|(idx, tile)| {
                let x = tile.tx * TILE_SIZE;
                let y = tile.ty * TILE_SIZE;
                let w = TILE_SIZE.min(canvas_ref.width() - x);
                let h = TILE_SIZE.min(canvas_ref.height() - y);

                let out_w = w.div_ceil(lod_step);
                let out_h = h.div_ceil(lod_step);
                let mut img = egui::ColorImage::new([out_w, out_h], Color32::TRANSPARENT);
                canvas_ref.write_region_to_color_image(x, y, w, h, &mut img, lod_step);
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

    for tile in &app.render_cache.tiles {
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
        _cos: cos,
        _sin: sin,
        response,
    }
}
