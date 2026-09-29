//! Picking an image to import on Android, where there's no file dialog: a
//! gallery of the device's photo library (MediaStore). Listing, thumbnails
//! and decoding run on background threads.

use crate::PainterApp;
use crate::ui::style::*;
use eframe::egui::{self, RichText};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

#[cfg(target_os = "android")]
use crate::android as photos;

/// Desktop builds import through the file dialog; these keep the gallery
/// compiling (and checked) there.
#[cfg(not(target_os = "android"))]
mod photos {
    #[derive(Clone, Debug)]
    pub struct GalleryImage {
        pub id: i64,
        pub name: String,
    }
    const NONE: &str = "The photo library is only available on Android";
    pub fn has_image_access() -> bool {
        false
    }
    pub fn request_image_access() -> Result<(), String> {
        Err(NONE.into())
    }
    pub fn list_images(_limit: usize) -> Result<Vec<GalleryImage>, String> {
        Err(NONE.into())
    }
    pub fn load_thumbnail(_id: i64, _size: i32) -> Result<(usize, usize, Vec<u8>), String> {
        Err(NONE.into())
    }
    pub fn read_image(_id: i64) -> Result<Vec<u8>, String> {
        Err(NONE.into())
    }
}

/// Newest images shown.
const MAX_IMAGES: usize = 600;
const THUMB_PX: i32 = 192;

enum Msg {
    Listed(Result<Vec<photos::GalleryImage>, String>),
    Thumb(i64, egui::ColorImage),
    Decoded(Result<(String, image::RgbaImage), String>),
}

#[derive(PartialEq)]
enum Phase {
    Closed,
    /// Waiting for the user to allow photo access.
    NeedAccess,
    Listing,
    Ready,
    Importing,
}

struct Item {
    image: photos::GalleryImage,
    texture: Option<egui::TextureHandle>,
}

pub struct GalleryState {
    phase: Phase,
    items: Vec<Item>,
    error: Option<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Tells the thumbnail thread to stop (gallery closed).
    cancel: Arc<AtomicBool>,
    last_access_check: f64,
    /// The picked image becomes the reference image, not a new layer.
    for_reference: bool,
}

impl Default for GalleryState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            phase: Phase::Closed,
            items: Vec::new(),
            error: None,
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            last_access_check: 0.0,
            for_reference: false,
        }
    }
}

impl GalleryState {
    // Desktop imports through the file dialog instead.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn open(&mut self) {
        if self.phase != Phase::Closed {
            return;
        }
        self.error = None;
        self.for_reference = false;
        if photos::has_image_access() {
            self.start_listing();
        } else {
            if let Err(e) = photos::request_image_access() {
                self.error = Some(e);
            }
            self.phase = Phase::NeedAccess;
        }
    }

    /// Open the gallery to pick the reference image (View → Reference
    /// Image).
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn open_for_reference(&mut self) {
        self.open();
        self.for_reference = true;
    }

    fn close(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.phase = Phase::Closed;
        // Drop textures; the next opening lists the library again.
        self.items.clear();
    }

    fn start_listing(&mut self) {
        self.phase = Phase::Listing;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Listed(photos::list_images(MAX_IMAGES)));
        });
    }

    fn start_thumbnails(&self, ctx: &egui::Context) {
        let ids: Vec<i64> = self.items.iter().map(|i| i.image.id).collect();
        let (tx, cancel, ctx) = (self.tx.clone(), self.cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            for id in ids {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Ok((w, h, rgba)) = photos::load_thumbnail(id, THUMB_PX) {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                    if tx.send(Msg::Thumb(id, img)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            }
        });
    }

    fn start_import(&mut self, ctx: &egui::Context, image: photos::GalleryImage) {
        self.phase = Phase::Importing;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = photos::read_image(image.id).and_then(|bytes| {
                let img = image::load_from_memory(&bytes)
                    .map_err(|e| format!("Couldn't open {}: {e}", image.name))?
                    .to_rgba8();
                let name = std::path::Path::new(&image.name)
                    .file_stem()
                    .map_or_else(|| "Image".to_string(), |s| s.to_string_lossy().into_owned());
                Ok((name, img))
            });
            let _ = tx.send(Msg::Decoded(result));
            ctx.request_repaint();
        });
    }
}

/// Show the gallery while it's open; importing the tapped image.
pub fn image_gallery(app: &mut PainterApp, ctx: &egui::Context) {
    if app.workspace.gallery.phase == Phase::Closed {
        return;
    }

    // Results from the background threads.
    let mut decoded = None;
    while let Ok(msg) = app.workspace.gallery.rx.try_recv() {
        let g = &mut app.workspace.gallery;
        match msg {
            Msg::Listed(Ok(images)) => {
                g.items = images
                    .into_iter()
                    .map(|image| Item {
                        image,
                        texture: None,
                    })
                    .collect();
                g.phase = Phase::Ready;
                g.start_thumbnails(ctx);
            }
            Msg::Listed(Err(e)) => {
                g.error = Some(e);
                g.phase = Phase::Ready;
            }
            Msg::Thumb(id, img) => {
                if let Some(item) = g.items.iter_mut().find(|i| i.image.id == id) {
                    item.texture = Some(ctx.load_texture(
                        format!("gallery-{id}"),
                        img,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            Msg::Decoded(result) => decoded = Some(result),
        }
    }
    match decoded {
        Some(Ok((name, img))) => {
            app.workspace.gallery.close();
            if std::mem::take(&mut app.workspace.gallery.for_reference) {
                app.set_reference_image(&name, img);
            } else {
                app.import_rgba(&name, img);
            }
            return;
        }
        Some(Err(e)) => {
            let g = &mut app.workspace.gallery;
            g.error = Some(e);
            g.phase = Phase::Ready;
        }
        None => {}
    }

    // The permission answer isn't reported back: check now and then.
    let now = ctx.input(|i| i.time);
    let g = &mut app.workspace.gallery;
    if g.phase == Phase::NeedAccess {
        if now - g.last_access_check > 0.5 {
            g.last_access_check = now;
            if photos::has_image_access() {
                g.start_listing();
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(300));
    }

    let screen = ctx.screen_rect();
    let mut open = true;
    let mut pick = None;
    egui::Window::new("Import image")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(screen.size() * egui::vec2(0.8, 0.75))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            let g = &mut app.workspace.gallery;
            if let Some(e) = &g.error {
                ui.label(RichText::new(e).color(egui::Color32::from_rgb(230, 110, 110)));
            }
            match g.phase {
                Phase::NeedAccess => {
                    ui.label("Allow access to your photos to import one.");
                    if ui.button("Ask again").clicked() {
                        g.error = photos::request_image_access().err();
                    }
                }
                Phase::Listing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Looking for images…");
                    });
                }
                Phase::Importing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Opening the image…");
                    });
                }
                Phase::Ready if g.items.is_empty() && g.error.is_none() => {
                    ui.label("No images found on this device.");
                }
                _ => {}
            }
            if g.phase != Phase::Ready || g.items.is_empty() {
                return;
            }
            let cell = if metrics(ui.ctx()).touch {
                132.0
            } else {
                110.0
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for item in &g.items {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(cell, cell), egui::Sense::click());
                            if !ui.is_rect_visible(rect) {
                                continue;
                            }
                            let painter = ui.painter();
                            painter.rect_filled(rect, 0.0, BG_RAISED);
                            if let Some(tex) = &item.texture {
                                // Fit inside the square, keeping proportions.
                                let size = tex.size_vec2();
                                let k = (cell / size.x).min(cell / size.y);
                                let r = egui::Rect::from_center_size(rect.center(), size * k);
                                let uv = egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                );
                                painter.image(tex.id(), r, uv, egui::Color32::WHITE);
                            }
                            if response.hovered() {
                                painter.rect_stroke(rect, 0.0, egui::Stroke::new(2.0_f32, ACCENT));
                            }
                            if response.on_hover_text(&item.image.name).clicked() {
                                pick = Some(item.image.clone());
                            }
                        }
                    });
                });
        });
    let g = &mut app.workspace.gallery;
    if let Some(image) = pick {
        g.error = None;
        g.start_import(ctx, image);
    }
    if !open {
        g.close();
    }
}
