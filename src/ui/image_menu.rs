//! The Image menu (canvas size, image size, crop, rotate, flip) and its
//! Canvas Size and Image Size dialogs.

use crate::PainterApp;
use crate::canvas::geometry::ImageOp;
use crate::ui::widgets::FitScreen;
use crate::ui::widgets::{property_row, segmented};
use eframe::egui;

/// An open size dialog.
#[derive(Clone, Copy, Debug)]
pub enum SizeDialog {
    /// Canvas size: new size and which part of the canvas stays put
    /// (0..=2 across and down; 1, 1 = centred).
    Canvas {
        w: usize,
        h: usize,
        anchor: (u8, u8),
    },
    /// Image size: resample to this size.
    Image {
        w: usize,
        h: usize,
        keep_ratio: bool,
        smooth: bool,
    },
}

pub fn image_menu(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    item: impl Fn(&mut egui::Ui, &str) -> bool,
) {
    let (w, h) = (app.canvas.width(), app.canvas.height());
    if item(ui, "Canvas Size…") {
        app.modal_state.size_dialog = Some(SizeDialog::Canvas {
            w,
            h,
            anchor: (1, 1),
        });
    }
    if item(ui, "Image Size…") {
        app.modal_state.size_dialog = Some(SizeDialog::Image {
            w,
            h,
            keep_ratio: true,
            smooth: true,
        });
    }
    let crop = app
        .selection_manager
        .get_bounds()
        .filter(|_| app.selection_manager.has_selection());
    if ui
        .add_enabled_ui(crop.is_some(), |ui| item(ui, "Crop to Selection"))
        .inner
        && let Some(b) = crop
    {
        let [x0, y0, x1, y1] = [
            b.min.x.floor() as i32,
            b.min.y.floor() as i32,
            b.max.x.ceil() as i32,
            b.max.y.ceil() as i32,
        ];
        // Within the canvas: cropping never grows it.
        let (x0, y0) = (x0.max(0), y0.max(0));
        let (x1, y1) = (x1.min(w as i32), y1.min(h as i32));
        if x1 > x0 && y1 > y0 {
            app.apply_image_op(ImageOp::Reframe {
                x: x0,
                y: y0,
                w: (x1 - x0) as usize,
                h: (y1 - y0) as usize,
            });
        }
    }
    ui.separator();
    for (label, op) in [
        ("Rotate 90° Clockwise", ImageOp::RotateCw),
        ("Rotate 90° Counter-clockwise", ImageOp::RotateCcw),
        ("Rotate 180°", ImageOp::Rotate180),
        ("Flip Horizontally", ImageOp::FlipHorizontal),
        ("Flip Vertically", ImageOp::FlipVertical),
    ] {
        if item(ui, label) {
            app.apply_image_op(op);
        }
    }
    ui.separator();
    ui.menu_button("Colour Profile", |ui| profile_items(app, ui));
    ui.menu_button("Colour Depth", |ui| {
        let current = app.canvas.depth();
        for depth in crate::canvas::storage::Depth::ALL {
            if ui.radio(current == depth, depth.label()).clicked() {
                app.convert_depth(depth);
                ui.close_menu();
            }
        }
    });
}

pub fn size_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.modal_state.size_dialog else {
        return;
    };
    let (cw, ch) = (app.canvas.width(), app.canvas.height());
    let mut open = true;
    let (mut ok, mut cancel) = (false, false);
    let title = match dialog {
        SizeDialog::Canvas { .. } => "Canvas Size",
        SizeDialog::Image { .. } => "Image Size",
    };
    egui::Window::new(title)
        .fit_screen(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(format!("Now {cw} × {ch} px"));
            match &mut dialog {
                SizeDialog::Canvas { w, h, anchor } => {
                    size_fields(ui, w, h, None);
                    property_row(ui, "Anchor", |ui| anchor_grid(ui, anchor));
                }
                SizeDialog::Image {
                    w,
                    h,
                    keep_ratio,
                    smooth,
                } => {
                    let ratio = keep_ratio.then_some(cw as f64 / ch as f64);
                    size_fields(ui, w, h, ratio);
                    ui.checkbox(keep_ratio, "Keep proportions");
                    property_row(ui, "Resample", |ui| {
                        segmented(
                            ui,
                            smooth,
                            &[(true, "Smooth"), (false, "Hard pixels")],
                            true,
                        )
                    });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    app.modal_state.size_dialog = Some(dialog);
    if ok {
        app.modal_state.size_dialog = None;
        let op = match dialog {
            SizeDialog::Canvas { w, h, anchor } => {
                let place = |old: usize, new: usize, a: u8| -> i32 {
                    (old as i32 - new as i32) * a as i32 / 2
                };
                ImageOp::Reframe {
                    x: place(cw, w, anchor.0),
                    y: place(ch, h, anchor.1),
                    w,
                    h,
                }
            }
            SizeDialog::Image { w, h, smooth, .. } => ImageOp::Resize { w, h, smooth },
        };
        if op.new_size(cw, ch) != (cw, ch)
            || matches!(op, ImageOp::Reframe { x, y, .. } if x != 0 || y != 0)
        {
            app.apply_image_op(op);
        }
    } else if cancel || !open {
        app.modal_state.size_dialog = None;
    }
}

/// Width and height fields; with `ratio`, editing one updates the other.
fn size_fields(ui: &mut egui::Ui, w: &mut usize, h: &mut usize, ratio: Option<f64>) {
    let max = crate::app::document::MAX_CANVAS_DIMENSION;
    let changed_w = property_row(ui, "Width", |ui| {
        ui.add(egui::DragValue::new(w).range(1..=max).suffix(" px"))
            .changed()
    });
    let changed_h = property_row(ui, "Height", |ui| {
        ui.add(egui::DragValue::new(h).range(1..=max).suffix(" px"))
            .changed()
    });
    if let Some(r) = ratio {
        if changed_w {
            *h = ((*w as f64 / r).round() as usize).clamp(1, max);
        } else if changed_h {
            *w = ((*h as f64 * r).round() as usize).clamp(1, max);
        }
    }
}

/// A 3×3 grid of buttons choosing the anchor.
fn anchor_grid(ui: &mut egui::Ui, anchor: &mut (u8, u8)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
        for y in 0..3u8 {
            ui.horizontal(|ui| {
                for x in 0..3u8 {
                    let on = *anchor == (x, y);
                    let label = if on { "●" } else { "·" };
                    if ui
                        .add(
                            egui::Button::new(label)
                                .min_size(egui::vec2(22.0, 22.0))
                                .selected(on),
                        )
                        .clicked()
                    {
                        *anchor = (x, y);
                    }
                }
            });
        }
    });
}

/// Image → Colour Profile: which colours the document's numbers are.
fn profile_items(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::app::files::{OpenFor, ProfileUse};
    use crate::canvas::color_profile::{ColorProfile, RenderingIntent};
    ui.label(format!("Document: {}", app.canvas.profile.label()));
    ui.separator();
    ui.menu_button("Assign Profile", |ui| {
        ui.weak("The same numbers, read as this profile's colours.");
        for p in ColorProfile::BUILT_IN {
            if ui.radio(app.canvas.profile == p, p.label()).clicked() {
                app.assign_profile(p);
                ui.close_menu();
            }
        }
        if ui.button("From an ICC file…").clicked() {
            app.pick_open(OpenFor::Profile(ProfileUse::Assign));
            ui.close_menu();
        }
    });
    ui.menu_button("Convert to Profile", |ui| {
        ui.weak("The same colours, as this profile's numbers.");
        for p in ColorProfile::BUILT_IN {
            let current = app.canvas.profile == p;
            if ui
                .add_enabled(!current, egui::Button::new(p.label()))
                .clicked()
            {
                app.convert_profile(p);
                ui.close_menu();
            }
        }
        if ui.button("To an ICC file…").clicked() {
            app.pick_open(OpenFor::Profile(ProfileUse::Convert));
            ui.close_menu();
        }
    });
    ui.menu_button("Rendering Intent", |ui| {
        for intent in RenderingIntent::ALL {
            ui.radio_value(&mut app.workspace.color.intent, intent, intent.label());
        }
    });
}

/// View → Colour Management: the monitor's profile and proofing.
pub fn view_color_items(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::app::files::{OpenFor, ProfileUse};
    use crate::canvas::color_profile::ColorProfile;
    ui.menu_button(
        format!("Monitor: {}", app.workspace.color.monitor.label()),
        |ui| {
            for p in ColorProfile::BUILT_IN {
                let current = app.workspace.color.monitor == p;
                if ui.radio(current, p.label()).clicked() {
                    app.set_monitor_profile(p);
                    ui.close_menu();
                }
            }
            if ui.button("From an ICC file…").clicked() {
                app.pick_open(OpenFor::Profile(ProfileUse::Monitor));
                ui.close_menu();
            }
        },
    );
    ui.separator();
    let print = app
        .workspace
        .color
        .cmyk_profile()
        .map_or_else(|| "none found".to_string(), |p| p.name);
    ui.add_enabled_ui(app.workspace.color.cmyk.is_some(), |ui| {
        ui.checkbox(&mut app.workspace.color.proofing, "Proof Colours")
            .on_hover_text("Show the picture as it would print on the print profile.");
        ui.add_enabled(
            app.workspace.color.proofing,
            egui::Checkbox::new(&mut app.workspace.color.gamut_warning, "Gamut Warning"),
        )
        .on_hover_text("Colours that can't print show in grey.");
    });
    if ui.button(format!("Print Profile ({print})…")).clicked() {
        app.pick_open(OpenFor::Profile(ProfileUse::Print));
        ui.close_menu();
    }
}
