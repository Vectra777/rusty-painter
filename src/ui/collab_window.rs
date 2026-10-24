//! File → Paint Together: host the document or join someone's, and who's
//! painting (see [`crate::app::collab`]).

use crate::PainterApp;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use eframe::egui::{self, RichText};

pub fn collab_window(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.workspace.collab_form.open {
        return;
    }
    let mut open = true;
    let mut action = None;
    egui::Window::new("Paint Together")
        .fit_screen_size(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            if let Some(c) = &app.workspace.collab {
                ui.label(&c.status);
                if let Some(invite) = &c.invite {
                    let codes = [
                        ("Invite code:", Some(invite)),
                        ("Through Tailscale:", c.tailnet_invite.as_ref()),
                    ];
                    for (title, code) in codes {
                        let Some(code) = code else { continue };
                        ui.separator();
                        ui.label(title);
                        let mut shown = code.clone();
                        ui.add(egui::TextEdit::multiline(&mut shown).desired_rows(2));
                        let qr_id = ui.id().with((title, "qr"));
                        let mut qr = ui.data(|d| d.get_temp(qr_id)).unwrap_or(false);
                        ui.push_id(title, |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Copy").clicked() {
                                    ctx.copy_text(code.clone());
                                }
                                qr ^= ui.selectable_label(qr, "QR code").clicked();
                            });
                        });
                        ui.data_mut(|d| d.insert_temp(qr_id, qr));
                        if qr {
                            qr_code(ui, code);
                        }
                    }
                    ui.label(
                        RichText::new(
                            "Guests outside this network need the port forwarded \
                             (or a VPN such as Tailscale), or host on a relay.",
                        )
                        .small()
                        .color(TEXT_DIM),
                    );
                }
                ui.separator();
                for (_, name) in &c.users {
                    ui.label(name);
                }
                ui.separator();
                let stop = if c.is_host() { "Stop Hosting" } else { "Leave" };
                if ui.button(stop).clicked() {
                    action = Some(Action::Stop);
                }
                return;
            }
            let form = &mut app.workspace.collab_form;
            egui::Grid::new("collab_form")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Your name");
                    ui.text_edit_singleline(&mut form.name);
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut form.password).password(true));
                    ui.end_row();
                });
            ui.separator();
            // Hosting shares the canvas showing: none from the library.
            let hosting = !app.workspace.library.open;
            if !hosting {
                ui.label(
                    RichText::new(
                        "To host, open the canvas to share and use File → Paint Together.",
                    )
                    .small()
                    .color(TEXT_DIM),
                );
            }
            let form = &mut app.workspace.collab_form;
            if hosting {
                ui.label(RichText::new("Host this document").strong());
                egui::Grid::new("collab_host")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut form.port).range(1..=65535));
                        ui.end_row();
                        ui.label("Relay");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.relay)
                                .hint_text("optional: host:port"),
                        );
                        ui.end_row();
                    });
                if ui.button("Host").clicked() {
                    action = Some(Action::Host);
                }
                ui.separator();
            }
            ui.label(RichText::new("Join").strong());
            ui.add(egui::TextEdit::singleline(&mut form.invite).hint_text("rp://…"));
            // A phone's camera reads the host's QR code and copies it.
            #[cfg(target_os = "android")]
            if ui.button("Paste").clicked() {
                match crate::android::clipboard_text() {
                    Ok(text) => form.invite = text.trim().to_string(),
                    Err(err) => form.error = Some(err),
                }
            }
            ui.label(
                RichText::new("The open document stays in its own tab.")
                    .small()
                    .color(TEXT_DIM),
            );
            if ui.button("Join").clicked() {
                action = Some(Action::Join);
            }
            if let Some(err) = &form.error {
                ui.colored_label(egui::Color32::from_rgb(230, 90, 80), err);
            }
        });
    let result = match action {
        Some(Action::Host) => app.collab_host(ctx),
        Some(Action::Join) => app.collab_join(ctx),
        Some(Action::Stop) => {
            app.workspace.collab = None;
            Ok(())
        }
        None => return app.workspace.collab_form.open = open,
    };
    app.workspace.collab_form.error = result.err();
    app.workspace.collab_form.open = open;
}

/// `text` as a QR code, dark on a light square with its quiet zone, as
/// big as the window allows (a camera reads it off the screen).
fn qr_code(ui: &mut egui::Ui, text: &str) {
    use qrcodegen::{QrCode, QrCodeEcc};
    let Ok(qr) = QrCode::encode_text(text, QrCodeEcc::Low) else {
        return;
    };
    let modules = qr.size() + 8;
    let side = ui.available_width().min(320.0);
    let cell = (side / modules as f32).floor().max(1.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::Vec2::splat(cell * modules as f32),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                let min = rect.min + egui::vec2((x + 4) as f32, (y + 4) as f32) * cell;
                painter.rect_filled(
                    egui::Rect::from_min_size(min, egui::Vec2::splat(cell)),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
}

enum Action {
    Host,
    Join,
    Stop,
}
