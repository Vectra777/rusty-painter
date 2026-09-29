//! View → Reference Image: a floating window showing a picture to paint
//! from. Open one (or drop it on the window), scroll to zoom, drag to pan,
//! and click to paint with a colour from it. The picture isn't part of the
//! project; the last one opened comes back next session.

use crate::PainterApp;
use crate::ui::style::*;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};
use std::path::PathBuf;

/// Longest side kept of a reference picture (a GPU texture limit, and
/// plenty to pick colours from).
const MAX_SIDE: u32 = 4096;

#[derive(Default)]
pub struct ReferenceState {
    pub open: bool,
    /// The picture last opened from a file (kept between sessions).
    pub path: Option<PathBuf>,
    image: Option<Picture>,
    /// Decoded, waiting for the window to make its texture.
    pending: Option<(String, image::RgbaImage)>,
    view: RefView,
    /// Fit the picture to the window next time it's drawn.
    fit: bool,
    /// The saved picture was tried this session.
    reopened: bool,
    error: Option<String>,
    /// Where the window was last frame: a file dropped on it becomes the
    /// reference instead of a new layer.
    pub window_rect: Option<Rect>,
}

struct Picture {
    name: String,
    pixels: image::RgbaImage,
    texture: egui::TextureHandle,
}

/// Placement of the picture in the window: `centre` (picture pixels) shows
/// at the middle of the area, `zoom` screen points per picture pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RefView {
    pub centre: Vec2,
    pub zoom: f32,
}

impl Default for RefView {
    fn default() -> Self {
        Self {
            centre: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl RefView {
    /// The whole `size` picture fitted in `area`.
    pub fn fit(size: Vec2, area: Rect) -> Self {
        let zoom = (area.width() / size.x).min(area.height() / size.y);
        Self {
            centre: size * 0.5,
            zoom: if zoom.is_finite() && zoom > 0.0 {
                zoom
            } else {
                1.0
            },
        }
    }

    pub fn to_screen(self, p: Vec2, area: Rect) -> Pos2 {
        area.center() + (p - self.centre) * self.zoom
    }

    pub fn to_picture(self, s: Pos2, area: Rect) -> Vec2 {
        self.centre + (s - area.center()) / self.zoom
    }

    /// Zoom by `factor`, keeping the picture point under `anchor` still.
    pub fn zoom_about(&mut self, anchor: Pos2, area: Rect, factor: f32) {
        let under = self.to_picture(anchor, area);
        self.zoom = (self.zoom * factor).clamp(0.01, 64.0);
        self.centre = under - (anchor - area.center()) / self.zoom;
    }
}

impl PainterApp {
    /// Use the picture at `path` as the reference image.
    pub(crate) fn open_reference_path(&mut self, path: &std::path::Path) -> Result<(), String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        let name = path.file_name().map_or_else(
            || "Reference".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        self.open_reference_bytes(&name, &bytes)?;
        self.workspace.view_aids.reference.path = Some(path.to_path_buf());
        Ok(())
    }

    pub(crate) fn open_reference_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| format!("Couldn't open the image: {e}"))?
            .to_rgba8();
        self.set_reference_image(name, img);
        Ok(())
    }

    /// Show `img` in the reference window (shrunk if it's huge).
    pub(crate) fn set_reference_image(&mut self, name: &str, mut img: image::RgbaImage) {
        let longest = img.width().max(img.height());
        if longest > MAX_SIDE {
            let k = MAX_SIDE as f32 / longest as f32;
            let w = ((img.width() as f32 * k).round() as u32).max(1);
            let h = ((img.height() as f32 * k).round() as u32).max(1);
            img = crate::app::import::downscale(&img, w, h);
        }
        let r = &mut self.workspace.view_aids.reference;
        r.pending = Some((name.to_string(), img));
        r.error = None;
        r.open = true;
    }

    /// Paint with the reference picture's colour at pixel (`x`, `y`),
    /// keeping the brush's own opacity (like the eyedropper).
    pub(crate) fn pick_reference_color(&mut self, x: u32, y: u32) {
        let r = &self.workspace.view_aids.reference;
        let pixels = match (&r.image, &r.pending) {
            (_, Some((_, img))) | (Some(Picture { pixels: img, .. }), None) => img,
            (None, None) => return,
        };
        let Some(&image::Rgba([r, g, b, a])) = pixels.get_pixel_checked(x, y) else {
            return;
        };
        if a == 0 {
            return;
        }
        let alpha = self.brush_state.brush.brush_options.color.a();
        self.brush_state.brush.brush_options.color =
            Color32::from_rgba_unmultiplied(r, g, b, alpha);
        self.brush_state.brush_preview.dirty = true;
    }

    /// Whether a file dropped now lands on the reference window.
    pub(crate) fn drop_is_on_reference(&self, ctx: &egui::Context) -> bool {
        let r = &self.workspace.view_aids.reference;
        r.open
            && r.window_rect.is_some_and(|rect| {
                ctx.input(|i| i.pointer.latest_pos())
                    .is_some_and(|p| rect.contains(p))
            })
    }
}

#[cfg(not(target_os = "android"))]
fn open_dialog(app: &mut PainterApp) {
    let Some(path) = crate::app::settings::file_dialog()
        .add_filter(
            "Images",
            &["png", "jpg", "jpeg", "bmp", "tif", "tiff", "gif"],
        )
        .pick_file()
        .inspect(|p| crate::app::settings::remember_dir(p))
    else {
        return;
    };
    if let Err(err) = app.open_reference_path(&path) {
        log::error!("{err}");
        app.workspace.view_aids.reference.error = Some(err);
    }
}

/// No file dialog on Android: pick from the photo library.
#[cfg(target_os = "android")]
fn open_dialog(app: &mut PainterApp) {
    app.workspace.gallery.open_for_reference();
}

pub fn reference_window(app: &mut PainterApp, ctx: &egui::Context) {
    let r = &mut app.workspace.view_aids.reference;
    if !r.open {
        r.window_rect = None;
        return;
    }
    // The picture from last session, once.
    if !std::mem::replace(&mut r.reopened, true)
        && r.image.is_none()
        && r.pending.is_none()
        && let Some(path) = r.path.clone()
        && let Err(err) = app.open_reference_path(&path)
    {
        log::warn!("{err}");
    }
    let r = &mut app.workspace.view_aids.reference;
    if let Some((name, img)) = r.pending.take() {
        let size = [img.width() as usize, img.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
        let texture = ctx.load_texture("reference-image", color, egui::TextureOptions::LINEAR);
        if let Some(old) = r.image.replace(Picture {
            name,
            pixels: img,
            texture,
        }) {
            app.workspace.retired_textures.push(old.texture);
        }
        app.workspace.view_aids.reference.fit = true;
    }

    let mut open = true;
    let mut picked = None;
    let mut browse = false;
    let response = egui::Window::new("Reference")
        .open(&mut open)
        .resizable(true)
        .collapsible(true)
        .default_size([320.0, 300.0])
        .min_size([160.0, 120.0])
        .show(ctx, |ui| {
            let r = &mut app.workspace.view_aids.reference;
            ui.horizontal(|ui| {
                if ui
                    .button("Open…")
                    .on_hover_text("Or drop a picture on this window")
                    .clicked()
                {
                    browse = true;
                }
                if ui
                    .add_enabled(r.image.is_some(), egui::Button::new("Fit"))
                    .clicked()
                {
                    r.fit = true;
                }
                if let Some(pic) = &r.image {
                    ui.label(
                        RichText::new(format!("{} · {:.0}%", pic.name, r.view.zoom * 100.0))
                            .small()
                            .color(TEXT_DIM),
                    );
                }
            });
            if let Some(err) = &r.error {
                ui.label(
                    RichText::new(err)
                        .small()
                        .color(Color32::from_rgb(255, 120, 120)),
                );
            }
            let size = ui.available_size().max(egui::vec2(140.0, 90.0));
            let (area, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
            let painter = ui.painter_at(area);
            painter.rect_filled(area, 0.0, BG_INSET);
            let Some(pic) = &r.image else {
                painter.text(
                    area.center(),
                    egui::Align2::CENTER_CENTER,
                    "Open or drop a picture",
                    egui::FontId::proportional(13.0),
                    TEXT_DIM,
                );
                return;
            };
            let pic_size = pic.texture.size_vec2();
            if std::mem::take(&mut r.fit) {
                r.view = RefView::fit(pic_size, area.shrink(4.0));
            }
            // Scroll or pinch to zoom at the pointer; drag to pan.
            if let Some(hover) = response.hover_pos() {
                let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
                let factor = pinch * (scroll / 200.0).exp();
                if factor != 1.0 {
                    r.view.zoom_about(hover, area, factor);
                }
            }
            if response.dragged() {
                r.view.centre -= response.drag_delta() / r.view.zoom;
            }
            let rect = Rect::from_min_max(
                r.view.to_screen(Vec2::ZERO, area),
                r.view.to_screen(pic_size, area),
            );
            let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
            painter.image(pic.texture.id(), rect, uv, Color32::WHITE);
            if response.clicked()
                && let Some(at) = response.interact_pointer_pos()
            {
                let p = r.view.to_picture(at, area);
                if p.x >= 0.0 && p.y >= 0.0 && p.x < pic_size.x && p.y < pic_size.y {
                    picked = Some((p.x as u32, p.y as u32));
                }
            }
            if response.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            response.on_hover_text_at_pointer("Click: paint with this colour");
        });
    let r = &mut app.workspace.view_aids.reference;
    r.window_rect = response.map(|w| w.response.rect);
    r.open = open;
    if let Some((x, y)) = picked {
        app.pick_reference_color(x, y);
    }
    if browse {
        open_dialog(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    #[test]
    fn the_view_maps_there_and_back_and_zooms_about_the_pointer() {
        let area = Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(200.0, 100.0));
        let mut view = RefView::fit(egui::vec2(400.0, 100.0), area);
        assert_eq!(view.zoom, 0.5);
        assert_eq!(view.to_screen(Vec2::ZERO, area), egui::pos2(10.0, 45.0));
        let anchor = egui::pos2(60.0, 50.0);
        let under = view.to_picture(anchor, area);
        view.zoom_about(anchor, area, 3.0);
        assert!((view.to_picture(anchor, area) - under).length() < 1e-3);
    }

    #[test]
    fn clicking_the_reference_sets_the_brush_colour() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        let mut img = image::RgbaImage::new(4, 2);
        img.put_pixel(3, 1, image::Rgba([200, 100, 50, 255]));
        app.set_reference_image("test", img);
        assert!(app.workspace.view_aids.reference.open);
        app.brush_state.brush.brush_options.color = Color32::from_rgba_unmultiplied(0, 0, 0, 128);
        app.pick_reference_color(3, 1);
        assert_eq!(
            app.brush_state.brush.brush_options.color,
            Color32::from_rgba_unmultiplied(200, 100, 50, 128)
        );
        // A transparent pixel or one outside the picture: no change.
        app.pick_reference_color(0, 0);
        app.pick_reference_color(9, 9);
        assert_eq!(
            app.brush_state.brush.brush_options.color,
            Color32::from_rgba_unmultiplied(200, 100, 50, 128)
        );
        assert!(app.layer_state.history.stacks().0.is_empty());
    }
}
