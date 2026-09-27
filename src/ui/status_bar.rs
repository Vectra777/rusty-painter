//! Bottom status bar: document info on the left, view controls on the right.

use crate::app::viewport::{MAX_ZOOM, MIN_ZOOM};
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, vdivider};
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

/// Below this bar width the view controls drop the buttons pinch-zoom and
/// the menus already cover.
const COMPACT_WIDTH: f32 = 640.0;

pub fn status_bar(app: &mut PainterApp, ctx: &egui::Context) {
    let m = metrics(ctx);
    egui::TopBottomPanel::bottom("status_bar")
        .exact_height(m.status_height)
        .frame(
            egui::Frame::none()
                .fill(BG_CANVAS)
                .inner_margin(egui::Margin::symmetric(
                    if m.touch { 4.0 } else { 10.0 },
                    0.0,
                )),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if !m.touch {
                    ui.spacing_mut().interact_size.y = 18.0;
                }
                let compact = ui.available_width() < COMPACT_WIDTH;

                if m.touch {
                    touch_buttons(app, ui, m.status_height);
                    vdivider(ui);
                }

                // The view controls go on the right; the info gets what's
                // left (their width is measured the frame before).
                let right_id = ui.id().with("status_right_width");
                let right_width: f32 = ui.data(|d| d.get_temp(right_id)).unwrap_or(0.0);
                let info_width = (ui.available_width() - right_width - 8.0).max(0.0);
                let height = ui.available_height();
                ui.allocate_ui_with_layout(
                    egui::vec2(info_width, height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                        document_info(app, ui, info_width, compact);
                    },
                );

                let right = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if m.touch {
                            let size = m.status_height - 4.0;
                            crate::ui::top_bar::panel_toggles(app, ui, size);
                            vdivider(ui);
                        }
                        view_controls(app, ui, compact);
                        // Report only what the controls use, not the free space.
                        ui.min_rect().width()
                    })
                    .inner;
                ui.data_mut(|d| d.insert_temp(right_id, right));
            });
        });
}

/// Touch mode: the menu sheet toggle (the menus live there instead of a top
/// bar) and one-finger painting on/off.
fn touch_buttons(app: &mut PainterApp, ui: &mut egui::Ui, bar_height: f32) {
    let size = bar_height - 4.0;
    let open = app.modal_state.menu_sheet_open;
    if icon_button(ui, Icon::Menu, size, open, "File, Edit, View and Help").clicked() {
        app.modal_state.menu_sheet_open = !open;
    }
    let ws = &mut app.workspace;
    let tip = if ws.finger_painting {
        "Finger painting on: one finger paints. Tap to paint with the stylus only."
    } else {
        "Finger painting off: only the stylus paints, one finger pans. Tap to turn on."
    };
    if icon_button(ui, Icon::Finger, size, ws.finger_painting, tip).clicked() {
        ws.finger_painting = !ws.finger_painting;
    }
}

/// Canvas size, color model, layer, cursor position and the last message,
/// dropping the least useful parts first so the rest fits in `width`.
fn document_info(app: &mut PainterApp, ui: &mut egui::Ui, width: f32, compact: bool) {
    let model = match app.workspace.color_model {
        ColorModel::Rgba => "RGBA",
        ColorModel::Grayscale => "Grayscale",
    };
    let size = format!("{} × {} px", app.canvas.width(), app.canvas.height());
    let layer = app
        .canvas
        .layers
        .get(app.canvas.active_layer_idx)
        .map(|l| l.name.clone())
        .unwrap_or_default();
    let cursor = app
        .viewport
        .cursor_canvas
        .map(|p| format!("{:.0}, {:.0}", p.x, p.y))
        .unwrap_or_else(|| "–".to_string());

    // A message (export done, errors) matters more than the rest.
    if let Some(message) = app.export_state.message.clone() {
        // Dismiss first: a long message truncates instead of hiding it.
        if small_button(ui, "✕", "Dismiss") {
            app.export_state.message = None;
        }
        ui.add(egui::Label::new(RichText::new(message).small().color(TEXT)).truncate());
        return;
    }

    let text_width = |ui: &egui::Ui, text: &str| {
        let font = egui::TextStyle::Small.resolve(ui.style());
        ui.painter()
            .layout_no_wrap(text.to_string(), font, TEXT_DIM)
            .size()
            .x
    };
    // Each divider costs about this much.
    let gap = 13.0;
    let layer_text = format!("Layer: {layer}");
    let full_size = format!("{size}  ·  {model}");
    let mut parts: Vec<String> = if compact {
        vec![size.clone(), layer.clone()]
    } else {
        vec![full_size, layer_text, cursor]
    };
    // Drop from the end until it fits; the layer name truncates last.
    while parts.len() > 1 {
        let total: f32 =
            parts.iter().map(|p| text_width(ui, p)).sum::<f32>() + gap * (parts.len() - 1) as f32;
        if total <= width {
            break;
        }
        parts.pop();
    }
    let last = parts.len().saturating_sub(1);
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            vdivider(ui);
        }
        let label = egui::Label::new(dim(part));
        if i == last {
            ui.add(label.truncate());
        } else {
            ui.add(label);
        }
    }
}

/// Right-to-left: [flip] [1:1] [Fit] [+] [zoom%] [−]  rotation
fn view_controls(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) {
    let flipped = app.viewport.flip_x;
    let size = (ui.available_height() - 2.0).clamp(18.0, 36.0);
    if icon_button(
        ui,
        Icon::Flip,
        size,
        flipped,
        "Flip the view horizontally (H) — the picture itself is unchanged",
    )
    .clicked()
    {
        app.viewport.flip_x = !flipped;
    }
    if !compact && small_button(ui, "1:1", "Actual pixels (Ctrl+1)") {
        app.set_zoom_from_center(1.0);
    }
    if small_button(ui, "Fit", "Fit to window (Ctrl+0)") {
        app.fit_view();
    }
    if !compact && small_button(ui, "+", "Zoom in (Ctrl+=)") {
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

    if !compact && small_button(ui, "−", "Zoom out (Ctrl+-)") {
        app.zoom_by_from_center(0.8);
    }

    let degrees = app.viewport.rotation.to_degrees().rem_euclid(360.0);
    if degrees > 0.05 && degrees < 359.95 {
        vdivider(ui);
        if small_button(ui, "Reset", "Reset rotation") {
            app.viewport.rotation = 0.0;
        }
        let label = if compact {
            format!("{degrees:.0}°")
        } else {
            format!("Rotation {degrees:.0}°")
        };
        ui.label(dim(label));
    }
}
