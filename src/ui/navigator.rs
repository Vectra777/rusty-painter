//! View → Navigator: the whole picture, with a frame round the part the
//! canvas view shows; a second view to judge the picture by while painting
//! zoomed in, maybe mirrored or through a check view of its own. Click or
//! drag in it to move the view there; resize the window to make it bigger.
//!
//! It's drawn from the canvas view's GPU atlases, so it follows every stroke
//! as it's painted; tiles off screen are composited for it at its size
//! ([`crate::app::view::render::update_dirty_textures`]). Layers drawn sharp
//! are left out of the atlases: a small picture of them goes over the top.
//! While shader layers show live (the atlases then hold runs of layers), it
//! falls back to a point-sampled composite made on the CPU (a few hundred
//! pixels across), rebuilt at most twice a second and not mid-stroke.

use crate::PainterApp;
use crate::app::view::check::CheckMode;
use crate::app::view::viewport::{MAX_ZOOM, MIN_ZOOM};
use crate::canvas::storage::LayerId;
use crate::ui::bar_slider::BarSlider;
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest side of the CPU-made picture, in pixels.
const PICTURE_PX: usize = 256;
/// Starting width of the navigator's view, in points.
const WIDTH: f32 = 220.0;
const REBUILD_EVERY: Duration = Duration::from_millis(500);
/// Largest side of the sharp layers' picture, in pixels.
const OVERLAY_PX: usize = 1024;
/// The sharp layers' picture is made again at most this often while they
/// change.
const OVERLAY_EVERY: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct NavigatorState {
    pub open: bool,
    /// Shown mirrored left to right.
    pub mirror: bool,
    /// Its own check view.
    pub check: CheckMode,
    /// The canvas changed since the pictures were built.
    pub dirty: bool,
    /// The atlas mip level it was last drawn from on the GPU (`None`: not
    /// drawn from them last frame).
    pub(crate) level: Option<u32>,
    /// The CPU-made picture.
    texture: Option<egui::TextureHandle>,
    built_at: Option<Instant>,
    /// Canvas size, mirroring and check view the picture was built for.
    built_for: (usize, usize, bool, CheckMode, u32),
    overlay: Option<SharpOverlay>,
}

/// The layers drawn sharp (left out of the atlases), small.
struct SharpOverlay {
    ids: Vec<LayerId>,
    size: [usize; 2],
    pixels: Arc<Vec<u8>>,
    version: u64,
    built_at: Instant,
}

impl NavigatorState {
    /// The atlas mip level off-screen tiles are composited at for it, while
    /// it's drawn from them.
    pub(crate) fn gpu_level(&self) -> Option<u32> {
        self.level.filter(|_| self.open)
    }
}

/// Where a `canvas` sized picture sits, fitted and centred in `area`, and
/// its scale (navigator points per canvas pixel).
pub(crate) fn fit(canvas: Vec2, area: Rect) -> (Rect, f32) {
    let scale = (area.width() / canvas.x).min(area.height() / canvas.y);
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (Rect::from_center_size(area.center(), canvas * scale), scale)
}

/// Canvas point `p` in the navigator (`picture` and `scale` from [`fit`]),
/// mirrored about the middle if `mirror`.
pub(crate) fn to_navigator(p: Vec2, picture: Rect, scale: f32, mirror: bool) -> Pos2 {
    let q = picture.min + p * scale;
    if mirror {
        egui::pos2(picture.max.x - (q.x - picture.min.x), q.y)
    } else {
        q
    }
}

/// The canvas point at navigator point `s`.
pub(crate) fn from_navigator(s: Pos2, picture: Rect, scale: f32, mirror: bool) -> Vec2 {
    let s = if mirror {
        egui::pos2(picture.max.x - (s.x - picture.min.x), s.y)
    } else {
        s
    };
    (s - picture.min) / scale
}

/// The view offset that puts canvas point `p` at the middle of a canvas
/// area of `area` size, for a canvas of `canvas` size seen at `zoom`,
/// turned by `rotation` and maybe flipped (the inverse of the canvas
/// placement: the canvas's corner at `offset`, turned about its centre).
pub(crate) fn offset_centring(
    p: Vec2,
    canvas: Vec2,
    zoom: f32,
    rotation: f32,
    flip: bool,
    area: Vec2,
) -> Vec2 {
    let q = if flip {
        Vec2::new(canvas.x - p.x, p.y)
    } else {
        p
    };
    let c = canvas * 0.5;
    let (sin, cos) = rotation.sin_cos();
    let d = (q - c) * zoom;
    let turned = Vec2::new(d.x * cos - d.y * sin, d.x * sin + d.y * cos);
    area * 0.5 - c * zoom - turned
}

impl PainterApp {
    /// The corners of the canvas area, in canvas coordinates: what the
    /// view shows (turned with it).
    fn view_quad(&self) -> Option<[Vec2; 4]> {
        let area = self.viewport.canvas_area?;
        let size = Vec2::new(self.canvas.width() as f32, self.canvas.height() as f32);
        let origin = area.min + self.viewport.offset;
        let centre = origin + size * self.viewport.zoom * 0.5;
        Some(
            [
                area.left_top(),
                area.right_top(),
                area.right_bottom(),
                area.left_bottom(),
            ]
            .map(|c| self.screen_to_canvas_raw(c, origin, centre)),
        )
    }

    /// Move the view so canvas point `p` is in the middle of it.
    pub(crate) fn centre_view_on(&mut self, p: Vec2) {
        let Some(area) = self.viewport.canvas_area else {
            return;
        };
        let size = Vec2::new(self.canvas.width() as f32, self.canvas.height() as f32);
        let v = &self.viewport;
        self.viewport.offset = offset_centring(p, size, v.zoom, v.rotation, v.flip_x, area.size());
        self.workspace.auto_fit = false;
    }
}

/// The small composite of the whole canvas, mirrored if `mirror`, through
/// `check` (with `values` steps).
fn picture(
    canvas: &crate::canvas::Canvas,
    mirror: bool,
    check: CheckMode,
    values: u32,
) -> egui::ColorImage {
    let (w, h) = (canvas.width(), canvas.height());
    let step = w.max(h).div_ceil(PICTURE_PX).max(1);
    let mut image = egui::ColorImage::new([0, 0], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, w, h, &mut image, step);
    if mirror {
        let width = image.size[0];
        for row in image.pixels.chunks_mut(width.max(1)) {
            row.reverse();
        }
    }
    if check != CheckMode::Colour {
        for p in &mut image.pixels {
            let [r, g, b, a] = check.apply(p.to_array(), values);
            *p = Color32::from_rgba_premultiplied(r, g, b, a);
        }
    }
    image
}

/// Layers `run` (bottom first) of `canvas`, point-sampled into a `size`
/// picture of the whole canvas, each over the last with its opacity: as
/// premultiplied sRGB RGBA8, like the atlases.
fn sharp_picture(canvas: &crate::canvas::Canvas, run: &[usize], size: [usize; 2]) -> Vec<u8> {
    let [w, h] = size;
    let tile_size = canvas.tile_size();
    let (cw, ch) = (canvas.width(), canvas.height());
    // Which pixels sample which tile, so each tile locks once a layer.
    let mut by_tile: std::collections::HashMap<(usize, usize), Vec<(usize, usize)>> =
        Default::default();
    for y in 0..h {
        let cy = ((y * 2 + 1) * ch / (h * 2)).min(ch - 1);
        for x in 0..w {
            let cx = ((x * 2 + 1) * cw / (w * 2)).min(cw - 1);
            by_tile
                .entry((cx / tile_size, cy / tile_size))
                .or_default()
                .push((y * w + x, (cy % tile_size) * tile_size + cx % tile_size));
        }
    }
    let mut out = vec![[0.0f32; 4]; w * h];
    for &i in run {
        let opacity = canvas.layers[i].opacity;
        for (&(tx, ty), samples) in &by_tile {
            let Some(cell) = canvas.lock_layer_tile_if_exists(i, tx, ty) else {
                continue;
            };
            let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
            let Some(data) = guard.data().filter(|_| !guard.is_empty) else {
                continue;
            };
            for &(dst, src) in samples {
                let p = data[src].to_array().map(|c| c as f32 / 255.0 * opacity);
                let o = &mut out[dst];
                for c in 0..4 {
                    o[c] = p[c] + o[c] * (1.0 - p[3]);
                }
            }
        }
    }
    out.iter()
        .flat_map(|p| p.map(|c| (c * 255.0).round() as u8))
        .collect()
}

/// The sharp layers' picture for a navigator `px` pixels big, made again if
/// they or the canvas changed (at most every [`OVERLAY_EVERY`]); `None`
/// when no layer is drawn sharp.
fn refresh_overlay(
    app: &mut PainterApp,
    ctx: &egui::Context,
    px: Vec2,
) -> Option<([u32; 2], u64, Arc<Vec<u8>>)> {
    let ids = app.workspace.vector.sharp.hidden.clone();
    if ids.is_empty() {
        app.workspace.view_aids.navigator.overlay = None;
        return None;
    }
    let (cw, ch) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let scale = (px.x / cw)
        .min(px.y / ch)
        .min(OVERLAY_PX as f32 / cw.max(ch))
        .min(1.0);
    let size = [
        ((cw * scale).round() as usize).max(1),
        ((ch * scale).round() as usize).max(1),
    ];
    let n = &app.workspace.view_aids.navigator;
    let rebuild = match &n.overlay {
        None => true,
        Some(o) if o.ids != ids || o.size != size => true,
        Some(o) if n.dirty => {
            let wait = OVERLAY_EVERY.saturating_sub(o.built_at.elapsed());
            if !wait.is_zero() {
                ctx.request_repaint_after(wait);
            }
            wait.is_zero()
        }
        Some(_) => false,
    };
    if rebuild {
        let run: Vec<usize> = ids
            .iter()
            .filter_map(|&id| app.canvas.layer_index_of(id))
            .collect();
        let pixels = Arc::new(sharp_picture(&app.canvas, &run, size));
        let n = &mut app.workspace.view_aids.navigator;
        let version = n.overlay.as_ref().map_or(0, |o| o.version + 1);
        n.overlay = Some(SharpOverlay {
            ids,
            size,
            pixels,
            version,
            built_at: Instant::now(),
        });
        n.dirty = false;
    }
    let o = app.workspace.view_aids.navigator.overlay.as_ref()?;
    Some((
        [o.size[0] as u32, o.size[1] as u32],
        o.version,
        o.pixels.clone(),
    ))
}

/// Rebuild the CPU-made picture if the canvas changed (at most every
/// [`REBUILD_EVERY`], and after the stroke being painted).
fn refresh(app: &mut PainterApp, ctx: &egui::Context) {
    let aids = &app.workspace.view_aids;
    let n = &aids.navigator;
    let size = (
        app.canvas.width(),
        app.canvas.height(),
        n.mirror,
        n.check,
        aids.check_values,
    );
    let stale = n.texture.is_none() || n.built_for != size;
    if !stale && (!n.dirty || app.brush_state.is_drawing) {
        return;
    }
    if !stale
        && let Some(built) = n.built_at
        && built.elapsed() < REBUILD_EVERY
    {
        ctx.request_repaint_after(REBUILD_EVERY - built.elapsed());
        return;
    }
    let image = picture(&app.canvas, size.2, size.3, size.4);
    let n = &mut app.workspace.view_aids.navigator;
    match &mut n.texture {
        Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
        None => {
            n.texture = Some(ctx.load_texture("navigator", image, egui::TextureOptions::LINEAR));
        }
    }
    n.dirty = false;
    n.built_at = Some(Instant::now());
    n.built_for = size;
}

pub fn navigator_window(app: &mut PainterApp, ctx: &egui::Context) {
    app.workspace.view_aids.navigator.level = None;
    if !app.workspace.view_aids.navigator.open {
        app.workspace.view_aids.navigator.overlay = None;
        return;
    }
    // Shader layers showing live: the atlases hold runs of layers, not the
    // picture.
    let gpu = app.render_cache.live.is_none();
    if gpu {
        app.workspace.view_aids.navigator.texture = None;
    } else {
        app.workspace.view_aids.navigator.overlay = None;
        refresh(app, ctx);
    }
    let canvas = Vec2::new(app.canvas.width() as f32, app.canvas.height() as f32);
    let mut open = true;
    egui::Window::new("Navigator")
        .fit_screen_size(ctx)
        .open(&mut open)
        .resizable(true)
        .collapsible(true)
        .default_size([
            WIDTH,
            (WIDTH * canvas.y / canvas.x).clamp(60.0, WIDTH * 1.5) + 70.0,
        ])
        .min_size([140.0, 120.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let mut percent = app.viewport.zoom * 100.0;
                let slider = BarSlider::new(&mut percent, MIN_ZOOM * 100.0..=MAX_ZOOM * 100.0)
                    .logarithmic(true)
                    .suffix("%")
                    .max_decimals(0);
                if ui.add(slider).changed() {
                    app.set_zoom_from_center(percent / 100.0);
                }
                if ui.small_button("Fit").clicked() {
                    app.fit_view();
                }
            });
            check_controls(app, ui);
            let size = ui.available_size().max(egui::vec2(100.0, 60.0));
            let (area, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
            let painter = ui.painter_at(area);
            painter.rect_filled(area, 0.0, BG_INSET);
            let (pic, scale) = fit(canvas, area.shrink(2.0));
            crate::ui::widgets::draw_checkerboard(&painter, pic, 6.0);
            let mirror = app.workspace.view_aids.navigator.mirror;
            if gpu {
                let ppp = ctx.pixels_per_point();
                let overlay = refresh_overlay(app, ctx, pic.size() * ppp);
                let aids = &app.workspace.view_aids;
                let check = aids.navigator.check.uniform(aids.check_values);
                crate::app::view::render::paint_navigator(
                    app, &painter, pic, mirror, check, overlay,
                );
                let level = crate::app::view::render::preview_level(true, scale, ppp);
                let n = &mut app.workspace.view_aids.navigator;
                n.level = Some(level);
                if n.overlay.is_none() {
                    n.dirty = false;
                }
            } else if let Some(texture) = &app.workspace.view_aids.navigator.texture {
                let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
                painter.image(texture.id(), pic, uv, Color32::WHITE);
            }
            if let Some(quad) = app.view_quad() {
                let mut points: Vec<Pos2> = quad
                    .iter()
                    .map(|&p| to_navigator(p, pic, scale, mirror))
                    .collect();
                points.push(points[0]);
                painter.add(egui::Shape::line(
                    points.clone(),
                    Stroke::new(3.0_f32, Color32::from_black_alpha(160)),
                ));
                painter.add(egui::Shape::line(
                    points,
                    Stroke::new(1.5_f32, Color32::from_rgb(255, 80, 80)),
                ));
            }
            if (response.clicked() || response.dragged())
                && let Some(at) = response.interact_pointer_pos()
            {
                let p = from_navigator(at, pic, scale, mirror);
                app.centre_view_on(Vec2::new(
                    p.x.clamp(0.0, canvas.x),
                    p.y.clamp(0.0, canvas.y),
                ));
                ctx.request_repaint();
            }
        });
    app.workspace.view_aids.navigator.open = open;
}

/// The navigator's own mirror and check view.
fn check_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let aids = &mut app.workspace.view_aids;
    let n = &mut aids.navigator;
    ui.horizontal(|ui| {
        let size = ui.spacing().interact_size.y;
        if crate::ui::widgets::icon_button(
            ui,
            Icon::Flip,
            size,
            n.mirror,
            "Mirror the navigator left to right (the canvas stays as it is)",
        )
        .clicked()
        {
            n.mirror = !n.mirror;
        }
        egui::ComboBox::from_id_salt("navigator check")
            .selected_text(n.check.label())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for mode in CheckMode::ALL {
                    ui.selectable_value(&mut n.check, mode, mode.label())
                        .on_hover_text(mode.hint());
                }
            })
            .response
            .on_hover_text("How the navigator shows the picture");
    });
    if n.check == CheckMode::Values {
        crate::ui::view_aids_menu::values_slider(ui, &mut aids.check_values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    #[test]
    fn the_picture_fits_the_navigator_and_maps_back() {
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let (pic, scale) = fit(Vec2::new(4000.0, 2000.0), area);
        assert_eq!(scale, 0.05);
        assert_eq!(
            pic,
            Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(200.0, 150.0))
        );
        let p = Vec2::new(1234.0, 567.0);
        for mirror in [false, true] {
            let back = from_navigator(to_navigator(p, pic, scale, mirror), pic, scale, mirror);
            assert!((back - p).length() < 1e-2);
        }
        // Mirrored, the left edge shows on the right.
        let left = to_navigator(Vec2::new(0.0, 1000.0), pic, scale, true);
        assert_eq!(left, egui::pos2(200.0, 100.0));
    }

    #[test]
    fn centring_puts_the_point_in_the_middle_of_the_view() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(300, 200, Color32::WHITE, 64));
        let area = Rect::from_min_size(egui::pos2(50.0, 30.0), egui::vec2(640.0, 480.0));
        app.viewport.canvas_area = Some(area);
        for (zoom, rotation, flip) in [(1.0, 0.0, false), (2.5, 0.7, false), (0.4, -2.0, true)] {
            app.viewport.zoom = zoom;
            app.viewport.rotation = rotation;
            app.viewport.flip_x = flip;
            let p = Vec2::new(210.0, 45.0);
            app.centre_view_on(p);
            let quad = app.view_quad().unwrap();
            let middle = (quad[0] + quad[2]) * 0.5;
            assert!(
                (middle - p).length() < 1e-2,
                "{zoom} {rotation} {flip}: {middle:?}"
            );
            // The view's frame is the area's size, in canvas pixels.
            assert!(((quad[1] - quad[0]).length() - 640.0 / zoom).abs() < 1e-2);
        }
        assert!(!app.workspace.auto_fit);
    }

    #[test]
    fn the_sharp_layers_picture_stacks_them_with_their_opacity() {
        let mut canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        let low = canvas.add_layer();
        let low = canvas.layer_index_of(low).unwrap();
        let high = canvas.add_layer();
        let high = canvas.layer_index_of(high).unwrap();
        canvas.set_layer_tile_data(low, 0, 0, vec![Color32::from_rgb(200, 0, 0); 64 * 64]);
        canvas.set_layer_tile_data(high, 1, 0, vec![Color32::from_rgb(0, 0, 200); 64 * 64]);
        canvas.layers[high].opacity = 0.5;
        let pixels = sharp_picture(&canvas, &[low, high], [4, 2]);
        let px = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        // Left half: the lower layer alone; right half: the upper at half.
        assert_eq!(px(0, 0), [200, 0, 0, 255]);
        assert_eq!(px(3, 1), [0, 0, 100, 128]);
    }

    #[test]
    fn the_picture_is_small_whatever_the_canvas() {
        let canvas = Canvas::new(4000, 3000, Color32::WHITE, 64);
        let image = picture(&canvas, false, CheckMode::Colour, 3);
        assert!(image.size[0] <= PICTURE_PX && image.size[1] <= PICTURE_PX);
        assert!(image.size[0] >= PICTURE_PX - 16);
    }
}
