//! Bottom status bar: document info on the left, view controls on the right.

use crate::app::viewport::{MAX_ZOOM, MIN_ZOOM};
use crate::ui::style::*;
use crate::ui::widgets::vdivider;
use crate::{ColorModel, PainterApp};
use eframe::egui::{self, RichText};

fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text).small().color(TEXT_DIM)
}

fn small_button(ui: &mut egui::Ui, text: &str, tooltip: &str) -> bool {
    let button = if metrics(ui.ctx()).touch {
        egui::Button::new(text).min_size(egui::vec2(40.0, 0.0))
    } else {
        egui::Button::new(RichText::new(text).small()).frame(false)
    };
    ui.add(button).on_hover_text(tooltip).clicked()
}

pub fn status_bar(app: &mut PainterApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status_bar")
        .exact_height(metrics(ctx).status_height)
        .frame(
            egui::Frame::none()
                .fill(BG_CANVAS)
                .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if !metrics(ctx).touch {
                    ui.spacing_mut().interact_size.y = 18.0;
                }

                let model = match app.workspace.color_model {
                    ColorModel::Rgba => "RGBA",
                    ColorModel::Grayscale => "Grayscale",
                };
                ui.label(dim(format!(
                    "{} × {} px  ·  {model}",
                    app.canvas.width(),
                    app.canvas.height()
                )));
                vdivider(ui);
                let layer = app
                    .canvas
                    .layers
                    .get(app.canvas.active_layer_idx)
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                ui.label(dim(format!("Layer: {layer}")));
                vdivider(ui);
                let cursor = app
                    .viewport
                    .cursor_canvas
                    .map(|p| format!("{:.0}, {:.0}", p.x, p.y))
                    .unwrap_or_else(|| "–".to_string());
                ui.label(dim(cursor));

                if let Some(message) = app.export_state.message.clone() {
                    vdivider(ui);
                    ui.label(RichText::new(message).small().color(TEXT));
                    if small_button(ui, "✕", "Dismiss") {
                        app.export_state.message = None;
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    view_controls(app, ui);
                });
            });
        });
}

/// Right-to-left: [1:1] [Fit] [+] [zoom%] [−]  rotation
fn view_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    if small_button(ui, "1:1", "Actual pixels (Ctrl+1)") {
        app.set_zoom_from_center(1.0);
    }
    if small_button(ui, "Fit", "Fit to window (Ctrl+0)") {
        app.fit_view();
    }
    if small_button(ui, "+", "Zoom in (Ctrl+=)") {
        app.zoom_by_from_center(1.25);
    }

    let mut percent = app.viewport.zoom * 100.0;
    let response = ui.add(
        egui::DragValue::new(&mut percent)
            .range(MIN_ZOOM * 100.0..=MAX_ZOOM * 100.0)
            .speed(1.0)
            .max_decimals(0)
            .suffix("%"),
    );
    if response.changed() {
        app.set_zoom_from_center(percent / 100.0);
    }

    if small_button(ui, "−", "Zoom out (Ctrl+-)") {
        app.zoom_by_from_center(0.8);
    }

    let degrees = app.viewport.rotation.to_degrees().rem_euclid(360.0);
    if degrees > 0.05 && degrees < 359.95 {
        vdivider(ui);
        if small_button(ui, "Reset", "Reset rotation") {
            app.viewport.rotation = 0.0;
        }
        ui.label(dim(format!("Rotation {degrees:.0}°")));
    }
}
