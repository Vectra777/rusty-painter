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
                    ui.separator();
                    ui.label("Invite code:");
                    let mut code = invite.clone();
                    ui.add(egui::TextEdit::multiline(&mut code).desired_rows(2));
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(invite.clone());
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

enum Action {
    Host,
    Join,
    Stop,
}
