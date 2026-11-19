//! The camera window (Figure tool → Hand → Hand Scanner, or Pose → Pose
//! from Camera): the camera's picture with the hand or body found drawn
//! over it, what to pose, Live and Capture; and Pose from Photo's window:
//! the photo with the body found over it, to apply (see
//! [`crate::app::hand_scanner`]).

use crate::PainterApp;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use eframe::egui::{self, RichText};

/// The preview's width, in points.
const PREVIEW_WIDTH: f32 = 320.0;
/// A photo's preview's tallest, in points.
#[cfg(feature = "pose-capture")]
const PHOTO_HEIGHT: f32 = 400.0;

/// The hand landmarks joined into the hand's bones (MediaPipe's numbering).
#[cfg(feature = "pose-capture")]
const HAND_BONES: [(usize, usize); 21] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 4),
    (0, 5),
    (5, 6),
    (6, 7),
    (7, 8),
    (5, 9),
    (9, 10),
    (10, 11),
    (11, 12),
    (9, 13),
    (13, 14),
    (14, 15),
    (15, 16),
    (13, 17),
    (17, 18),
    (18, 19),
    (19, 20),
    (0, 17),
];

/// The body landmarks joined into its bones (BlazePose's numbering).
#[cfg(feature = "pose-capture")]
const BODY_BONES: [(usize, usize); 27] = [
    (7, 2),
    (2, 0),
    (0, 5),
    (5, 8),
    (9, 10),
    (11, 12),
    (11, 13),
    (13, 15),
    (12, 14),
    (14, 16),
    (15, 17),
    (15, 19),
    (17, 19),
    (16, 18),
    (16, 20),
    (18, 20),
    (11, 23),
    (12, 24),
    (23, 24),
    (23, 25),
    (25, 27),
    (27, 29),
    (29, 31),
    (27, 31),
    (24, 26),
    (26, 28),
    (28, 30),
];

pub fn hand_scanner_window(app: &mut PainterApp, ctx: &egui::Context) {
    #[cfg(feature = "pose-capture")]
    crate::app::hand_scanner::session::free_retired();
    pose_photo_window(app, ctx);
    if !app.workspace.figure.scanner.open {
        return;
    }
    #[cfg(feature = "pose-capture")]
    {
        // The camera stops while the app is in the background.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        app.scanner_poll(focused);
    }
    let mut open = true;
    let title = match app.workspace.figure.scanner.mode {
        crate::app::hand_scanner::ScanMode::Hand => "Hand Scanner",
        crate::app::hand_scanner::ScanMode::Body => "Pose from Camera",
    };
    egui::Window::new(title)
        .id(egui::Id::new("pose-camera-window"))
        .fit_screen_size(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(PREVIEW_WIDTH)
        .show(ctx, |ui| scanner_body(app, ui));
    if !open {
        app.close_hand_scanner();
    } else if cfg!(feature = "pose-capture") {
        // New frames come without input: keep drawing while the camera
        // runs (and look for it now and then while it doesn't).
        #[cfg(feature = "pose-capture")]
        let running = (app.workspace.figure.scanner.session.as_ref()).is_some_and(|s| s.running());
        #[cfg(not(feature = "pose-capture"))]
        let running = false;
        let wait = if running { 15 } else { 250 };
        ctx.request_repaint_after(std::time::Duration::from_millis(wait));
    }
}

fn privacy_note(ui: &mut egui::Ui) {
    ui.label(
        RichText::new("Runs on this device: nothing is recorded or sent.")
            .small()
            .color(TEXT_DIM),
    );
}

#[cfg(not(feature = "pose-capture"))]
fn scanner_body(_app: &mut PainterApp, ui: &mut egui::Ui) {
    ui.label("This build has no camera support (it was built without `pose-capture`).");
    privacy_note(ui);
}

#[cfg(not(feature = "pose-capture"))]
fn pose_photo_window(_app: &mut PainterApp, _ctx: &egui::Context) {}

#[cfg(feature = "pose-capture")]
fn scanner_body(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::app::hand_scanner::{ScanMode, TargetHand};
    use crate::pose_track::Handedness;

    // What to pose.
    ui.horizontal(|ui| {
        let mut mode = app.workspace.figure.scanner.mode;
        ui.selectable_value(&mut mode, ScanMode::Hand, "Hand")
            .on_hover_text("Pose a hand from yours");
        ui.selectable_value(&mut mode, ScanMode::Body, "Body")
            .on_hover_text("Pose the whole figure from you");
        app.scanner_set_mode(mode);
    });
    let mode = app.workspace.figure.scanner.mode;
    if app.active_figure().is_none() {
        ui.label(
            RichText::new(match mode {
                ScanMode::Hand => "Select a 3D figure layer: the scanner poses its figure's hand.",
                ScanMode::Body => "Select a 3D figure layer: the camera poses its figure.",
            })
            .color(TEXT_DIM),
        );
    }
    // The camera.
    let mut pick = None;
    if let Some(session) = app.workspace.figure.scanner.session.as_ref()
        && !session.cameras.is_empty()
    {
        let current = session
            .index
            .or(Some(session.wanted))
            .and_then(|i| session.cameras.get(i))
            .map_or("", |c| c.name.as_str());
        egui::ComboBox::from_label("Camera")
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (i, camera) in session.cameras.iter().enumerate() {
                    if ui
                        .selectable_label(session.index == Some(i), &camera.name)
                        .clicked()
                    {
                        pick = Some(i);
                    }
                }
            });
    }
    if let Some(i) = pick {
        app.scanner_pick_camera(i);
    }

    preview(app, ui);

    match mode {
        ScanMode::Hand => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Pose").color(TEXT_DIM));
                let target = &mut app.workspace.figure.scanner.target;
                let before = *target;
                ui.selectable_value(target, TargetHand::Auto, "Auto")
                    .on_hover_text("The hand the camera sees");
                ui.selectable_value(target, TargetHand::Left, "Left hand");
                ui.selectable_value(target, TargetHand::Right, "Right hand");
                if *target != before && app.workspace.figure.scanner.live {
                    // A new hand is a new undo step.
                    app.figure_edit_done();
                }
            });
        }
        ScanMode::Body => {
            ui.horizontal(|ui| {
                let scanner = &mut app.workspace.figure.scanner;
                ui.checkbox(&mut scanner.include_hands, "Hands")
                    .on_hover_text("Read the hands too: the fingers and wrists follow yours");
                ui.checkbox(&mut scanner.turn_body, "Turn the figure")
                    .on_hover_text("Turn the whole figure as you're turned to the camera");
            });
        }
    }
    let found = (app.workspace.figure.scanner.session.as_ref())
        .and_then(|s| s.tracked.as_ref())
        .is_some_and(|t| t.hand.is_some() || t.body.is_some());
    ui.horizontal(|ui| {
        let live = app.workspace.figure.scanner.live;
        let what = match mode {
            ScanMode::Hand => "the hand",
            ScanMode::Body => "the figure",
        };
        if ui
            .selectable_label(live, "Live")
            .on_hover_text(format!(
                "Pose {what} as you move (one undo step until turned off)"
            ))
            .clicked()
        {
            app.scanner_set_live(!live);
        }
        if ui
            .add_enabled(!live && found, egui::Button::new("Capture"))
            .on_hover_text(format!("Pose {what} as it is now"))
            .clicked()
        {
            app.scanner_capture();
        }
        ui.checkbox(&mut app.workspace.figure.scanner.mirror, "Mirror")
            .on_hover_text(match mode {
                ScanMode::Hand => "Show the camera's picture as in a mirror",
                ScanMode::Body => {
                    "Show the camera's picture as in a mirror, and pose the figure as your mirror image"
                }
            });
    });

    // How it's going.
    let Some(session) = app.workspace.figure.scanner.session.as_mut() else {
        return;
    };
    let status = if let Some(text) = downloading_models() {
        RichText::new(text).color(TEXT_DIM)
    } else if let Some(err) = &session.error {
        RichText::new(err.as_str()).color(ui.visuals().warn_fg_color)
    } else if session.needs_permission() {
        RichText::new(if session.asked {
            "Allow the camera to pose from it (if it was refused, allow it in the system's settings)"
        } else {
            "The app needs permission to use the camera"
        })
        .color(ui.visuals().warn_fg_color)
    } else if !session.running() {
        RichText::new("No camera").color(TEXT_DIM)
    } else {
        match &session.tracked {
            Some(t) => match (&t.hand, &t.body) {
                (Some(hand), _) => RichText::new(format!(
                    "{} hand, {:.0} ms",
                    match hand.handedness {
                        Handedness::Left => "Left",
                        Handedness::Right => "Right",
                    },
                    t.millis
                ))
                .color(TEXT_DIM),
                (None, Some(body)) => {
                    let hands = body.hands.iter().flatten().count();
                    RichText::new(match hands {
                        0 => format!("Body, {:.0} ms", t.millis),
                        1 => format!("Body and a hand, {:.0} ms", t.millis),
                        _ => format!("Body and hands, {:.0} ms", t.millis),
                    })
                    .color(TEXT_DIM)
                }
                (None, None) => RichText::new(match mode {
                    ScanMode::Hand => "Hold a hand up to the camera",
                    ScanMode::Body => "Step back so the camera sees you",
                })
                .color(TEXT_DIM),
            },
            None => RichText::new("Starting…").color(TEXT_DIM),
        }
    };
    ui.label(status);
    if session.needs_permission() && ui.button("Allow camera").clicked() {
        session.asked = true;
        session.camera.request_permission();
    }
    privacy_note(ui);
}

/// The camera's picture, the hand or body found drawn over it.
#[cfg(feature = "pose-capture")]
fn preview(app: &mut PainterApp, ui: &mut egui::Ui) {
    let mirror = app.workspace.figure.scanner.mirror;
    let Some(session) = app.workspace.figure.scanner.session.as_mut() else {
        return;
    };
    let Some(frame) = &session.frame else {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(PREVIEW_WIDTH, PREVIEW_WIDTH * 0.75),
            egui::Sense::hover(),
        );
        ui.painter()
            .rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        return;
    };
    let (w, h) = frame.image.dimensions();
    if session.shown != frame.seq || session.texture.is_none() {
        let pixels = egui::ColorImage::from_rgb([w as usize, h as usize], frame.image.as_raw());
        set_texture(ui, &mut session.texture, "pose-camera", pixels);
        session.shown = frame.seq;
    }
    let Some(texture) = &session.texture else {
        return;
    };
    let size = egui::vec2(PREVIEW_WIDTH, PREVIEW_WIDTH * h as f32 / w.max(1) as f32);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let uv = if mirror {
        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
    } else {
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
    };
    let painter = ui.painter_at(rect);
    painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
    let Some(tracked) = &session.tracked else {
        return;
    };
    let view = Overlay {
        rect,
        size: egui::vec2(w as f32, h as f32),
        mirror,
    };
    if let Some(hand) = &tracked.hand {
        view.hand(&painter, hand);
    }
    if let Some(body) = &tracked.body {
        view.body(&painter, body, true);
    }
}

/// What to say while the tracking models download (the first time
/// they're wanted, in a build without them).
#[cfg(feature = "pose-capture")]
fn downloading_models() -> Option<String> {
    let bytes = crate::pose_track::downloading()?;
    Some(format!(
        "Downloading the tracking models (first use only): {:.1} of ~24 MB",
        bytes as f64 / 1e6
    ))
}

#[cfg(feature = "pose-capture")]
fn set_texture(
    ui: &egui::Ui,
    texture: &mut Option<egui::TextureHandle>,
    name: &str,
    pixels: egui::ColorImage,
) {
    match texture {
        Some(texture) => texture.set(pixels, egui::TextureOptions::LINEAR),
        None => {
            *texture = Some(
                ui.ctx()
                    .load_texture(name, pixels, egui::TextureOptions::LINEAR),
            )
        }
    }
}

/// Landmarks drawn over a picture shown in `rect` (the picture `size`
/// pixels, maybe shown mirrored).
#[cfg(feature = "pose-capture")]
struct Overlay {
    rect: egui::Rect,
    size: egui::Vec2,
    mirror: bool,
}

#[cfg(feature = "pose-capture")]
impl Overlay {
    fn at(&self, p: glam::Vec3) -> egui::Pos2 {
        let x = p.x / self.size.x;
        let x = if self.mirror { 1.0 - x } else { x };
        self.rect.min
            + egui::vec2(
                x * self.rect.width(),
                p.y / self.size.y * self.rect.height(),
            )
    }

    fn hand(&self, painter: &egui::Painter, hand: &crate::pose_track::HandLandmarks) {
        let bone = egui::Stroke::new(2.0_f32, accent());
        for (a, b) in HAND_BONES {
            painter.line_segment([self.at(hand.image[a]), self.at(hand.image[b])], bone);
        }
        for p in hand.image {
            painter.circle_filled(self.at(p), 2.5, egui::Color32::WHITE);
        }
    }

    /// The body: bones between landmarks seen solid, others faint (all
    /// faint and grey if not `chosen`).
    fn body(&self, painter: &egui::Painter, body: &crate::pose_track::BodyLandmarks, chosen: bool) {
        use crate::pose_track::body::VISIBLE;
        let seen = |i: usize| body.visibility[i] >= VISIBLE;
        let color = if chosen {
            accent()
        } else {
            egui::Color32::from_gray(150)
        };
        let solid = egui::Stroke::new(if chosen { 2.5_f32 } else { 1.5 }, color);
        let faint = egui::Stroke::new(1.5_f32, color.gamma_multiply(0.35));
        for (a, b) in BODY_BONES {
            let stroke = if seen(a) && seen(b) { solid } else { faint };
            painter.line_segment([self.at(body.image[a]), self.at(body.image[b])], stroke);
        }
        for (i, p) in body.image.iter().enumerate() {
            if seen(i) {
                painter.circle_filled(self.at(*p), 2.5, egui::Color32::WHITE);
            }
        }
        for hand in body.hands.iter().flatten() {
            self.hand(painter, hand);
        }
    }
}

/// Pose from Photo's window: the photo with the body found drawn over it,
/// Apply (one undo step) and Cancel.
#[cfg(feature = "pose-capture")]
fn pose_photo_window(app: &mut PainterApp, ctx: &egui::Context) {
    if app.workspace.figure.scanner.photo.is_none() {
        return;
    }
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    let mut toggle = None;
    egui::Window::new("Pose from Photo")
        .id(egui::Id::new("pose-photo-window"))
        .fit_screen_size(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(PREVIEW_WIDTH)
        .show(ctx, |ui| {
            let has_figure = app.active_figure().is_some();
            let scanner = &mut app.workspace.figure.scanner;
            let Some(photo) = scanner.photo.as_mut() else {
                return;
            };
            ui.label(RichText::new(&photo.name).color(TEXT_DIM));
            if let Some(image) = photo.image.clone() {
                let (w, h) = image.dimensions();
                if photo.texture.is_none() {
                    let pixels =
                        egui::ColorImage::from_rgb([w as usize, h as usize], image.as_raw());
                    set_texture(ui, &mut photo.texture, "pose-photo", pixels);
                }
                if let Some(texture) = &photo.texture {
                    let k = (PREVIEW_WIDTH / w.max(1) as f32).min(PHOTO_HEIGHT / h.max(1) as f32);
                    let size = egui::vec2(w as f32 * k, h as f32 * k);
                    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                    let painter = ui.painter_at(rect);
                    let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    painter.image(texture.id(), rect, full, egui::Color32::WHITE);
                    let view = Overlay {
                        rect,
                        size: egui::vec2(w as f32, h as f32),
                        mirror: false,
                    };
                    for (body, &chosen) in photo.bodies.iter().zip(&photo.chosen) {
                        view.body(&painter, body, chosen);
                    }
                    // A click picks or unpicks the body nearest it.
                    if let Some(pos) = response.interact_pointer_pos().filter(|_| response.clicked()) {
                        let near = |b: &crate::pose_track::BodyLandmarks| {
                            (b.image.iter().zip(&b.visibility))
                                .filter(|(_, v)| **v >= crate::pose_track::body::VISIBLE)
                                .map(|(p, _)| view.at(*p).distance(pos))
                                .fold(f32::INFINITY, f32::min)
                        };
                        toggle = (photo.bodies.iter().enumerate())
                            .map(|(i, b)| (i, near(b)))
                            .filter(|(_, d)| *d < 40.0)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(i, _)| i);
                    }
                }
            }
            let status = if let Some(text) = downloading_models() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
                RichText::new(text).color(TEXT_DIM)
            } else if photo.reading {
                RichText::new("Reading the photo…").color(TEXT_DIM)
            } else if let Some(err) = &photo.error {
                RichText::new(err.as_str()).color(ui.visuals().warn_fg_color)
            } else if photo.bodies.is_empty() {
                RichText::new("No body found in the photo").color(ui.visuals().warn_fg_color)
            } else if !has_figure {
                RichText::new("Select a 3D figure layer to pose its figure").color(TEXT_DIM)
            } else if photo.bodies.len() > 1 {
                RichText::new(format!(
                    "{} people found: click them to pick whom to pose (one figure each)",
                    photo.bodies.len()
                ))
                .color(TEXT_DIM)
            } else {
                RichText::new("Pose the figure as the photo shows?").color(TEXT_DIM)
            };
            ui.label(status);
            let picked = photo.chosen.iter().filter(|c| **c).count();
            let found = picked > 0;
            ui.horizontal(|ui| {
                ui.checkbox(&mut scanner.include_hands, "Hands")
                    .on_hover_text(
                        "Pose the fingers and wrists from the hands, where they're clear",
                    );
                ui.checkbox(&mut scanner.turn_body, "Turn the figure")
                    .on_hover_text("Turn the whole figure as the body in the photo is turned");
                ui.add_enabled(picked < 2, egui::Checkbox::new(&mut scanner.place_body, "Place"))
                    .on_hover_text(
                        "Move the figure where the body stands to the photo's camera, seen through \
                         the layer's (always, picking several)",
                    );
            });
            ui.horizontal(|ui| {
                let label = if picked > 1 {
                    format!("Apply ({picked} figures)")
                } else {
                    "Apply".to_string()
                };
                apply = ui
                    .add_enabled(found && has_figure, egui::Button::new(label))
                    .on_hover_text(
                        "Pose the selected figure (and add one for each other body picked), one undo step",
                    )
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
            privacy_note(ui);
        });
    if let Some(i) = toggle {
        app.photo_toggle(i);
    }
    if apply {
        app.photo_apply();
    }
    if !open || cancel {
        app.workspace.figure.scanner.photo = None;
    }
}
