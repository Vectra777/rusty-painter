//! View → Navigator: the whole picture small, with a frame round the part
//! the canvas view shows. Click or drag in it to move the view there.
//!
//! The picture is a point-sampled composite (a few hundred pixels across,
//! whatever the canvas size), rebuilt at most twice a second while the
//! canvas changes, and not while a stroke is being painted.

use crate::PainterApp;
use crate::app::view::viewport::{MAX_ZOOM, MIN_ZOOM};
use crate::ui::bar_slider::BarSlider;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use std::time::{Duration, Instant};

/// Longest side of the navigator's picture, in pixels.
const PICTURE_PX: usize = 256;
/// Width of the navigator's view, in points.
const WIDTH: f32 = 220.0;
const REBUILD_EVERY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct NavigatorState {
    pub open: bool,
    /// The canvas changed since the picture was built.
    pub dirty: bool,
    texture: Option<egui::TextureHandle>,
    built_at: Option<Instant>,
    /// Canvas size the picture was built for.
    built_for: (usize, usize),
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

/// Canvas point `p` in the navigator (`picture` and `scale` from [`fit`]).
pub(crate) fn to_navigator(p: Vec2, picture: Rect, scale: f32) -> Pos2 {
    picture.min + p * scale
}

/// The canvas point at navigator point `s`.
pub(crate) fn from_navigator(s: Pos2, picture: Rect, scale: f32) -> Vec2 {
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

/// The small composite of the whole canvas.
fn picture(canvas: &crate::canvas::Canvas) -> egui::ColorImage {
    let (w, h) = (canvas.width(), canvas.height());
    let step = w.max(h).div_ceil(PICTURE_PX).max(1);
    let mut image = egui::ColorImage::new([0, 0], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, w, h, &mut image, step);
    image
}

/// Rebuild the picture if the canvas changed (at most every
/// [`REBUILD_EVERY`], and after the stroke being painted).
fn refresh(app: &mut PainterApp, ctx: &egui::Context) {
    let size = (app.canvas.width(), app.canvas.height());
    let n = &app.workspace.view_aids.navigator;
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
    let image = picture(&app.canvas);
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
    if !app.workspace.view_aids.navigator.open {
        return;
    }
    refresh(app, ctx);
    let mut open = true;
    egui::Window::new("Navigator")
        .fit_screen(ctx)
        .open(&mut open)
        .resizable(false)
        .collapsible(true)
        .default_width(WIDTH)
        .show(ctx, |ui| {
            let canvas = Vec2::new(app.canvas.width() as f32, app.canvas.height() as f32);
            let height = (WIDTH * canvas.y / canvas.x).clamp(60.0, WIDTH);
            let (area, response) =
                ui.allocate_exact_size(egui::vec2(WIDTH, height), egui::Sense::click_and_drag());
            let painter = ui.painter_at(area);
            painter.rect_filled(area, 0.0, BG_INSET);
            let (pic, scale) = fit(canvas, area);
            crate::ui::widgets::draw_checkerboard(&painter, pic, 6.0);
            if let Some(texture) = &app.workspace.view_aids.navigator.texture {
                let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
                painter.image(texture.id(), pic, uv, Color32::WHITE);
            }
            if let Some(quad) = app.view_quad() {
                let mut points: Vec<Pos2> =
                    quad.iter().map(|&p| to_navigator(p, pic, scale)).collect();
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
                let p = from_navigator(at, pic, scale);
                app.centre_view_on(Vec2::new(
                    p.x.clamp(0.0, canvas.x),
                    p.y.clamp(0.0, canvas.y),
                ));
                ctx.request_repaint();
            }
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
        });
    app.workspace.view_aids.navigator.open = open;
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
        let back = from_navigator(to_navigator(p, pic, scale), pic, scale);
        assert!((back - p).length() < 1e-2);
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
    fn the_picture_is_small_whatever_the_canvas() {
        let canvas = Canvas::new(4000, 3000, Color32::WHITE, 64);
        let image = picture(&canvas);
        assert!(image.size[0] <= PICTURE_PX && image.size[1] <= PICTURE_PX);
        assert!(image.size[0] >= PICTURE_PX - 16);
    }
}
