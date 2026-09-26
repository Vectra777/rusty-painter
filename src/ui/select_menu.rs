//! The Select tool's slide-out menu (one toolbar button for every selection
//! type) and the selection controls it shares with the options bar.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::selection::{SelectionMode, SelectionType};
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use crate::ui::widgets::{segmented, slider_row};
use eframe::egui::{self, RichText, Sense, Stroke};

/// Every selection type with its icon, label and shortcut hint.
pub(crate) const TYPES: [(SelectionType, Icon, &str, &str); 4] = [
    (SelectionType::Rectangle, Icon::SelectRect, "Rectangle", "M"),
    (SelectionType::Circle, Icon::SelectEllipse, "Ellipse", "M"),
    (SelectionType::Lasso, Icon::Lasso, "Lasso", "L"),
    (
        SelectionType::Brush,
        Icon::SelectBrush,
        "Selection brush",
        "",
    ),
];

pub(crate) fn icon_for(kind: SelectionType) -> Icon {
    TYPES
        .iter()
        .find(|t| t.0 == kind)
        .map_or(Icon::SelectRect, |t| t.1)
}

/// Replace / Add / Subtract, plus the brush settings when painting.
pub(crate) fn mode_and_brush_controls(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) {
    let sel = &mut app.selection_manager;
    segmented(
        ui,
        &mut sel.mode,
        &[
            (SelectionMode::Replace, "Replace"),
            (SelectionMode::Add, "Add"),
            (SelectionMode::Subtract, "Subtract"),
        ],
        compact,
    );
    if matches!(app.active_tool, Tool::Select(SelectionType::Brush)) {
        let sel = &mut app.selection_manager;
        if compact {
            ui.label(RichText::new("Size").color(TEXT_DIM));
            ui.add(
                egui::Slider::new(&mut sel.brush_radius, 1.0..=500.0)
                    .logarithmic(true)
                    .suffix(" px"),
            );
            ui.label(RichText::new("Hardness").color(TEXT_DIM));
            ui.add(egui::Slider::new(&mut sel.brush_hardness, 0.0..=1.0));
        } else {
            slider_row(
                ui,
                "Size",
                egui::Slider::new(&mut sel.brush_radius, 1.0..=500.0)
                    .logarithmic(true)
                    .max_decimals(0)
                    .suffix(" px"),
            );
            slider_row(
                ui,
                "Hardness",
                egui::Slider::new(&mut sel.brush_hardness, 0.0..=1.0),
            );
        }
    }
}

/// Select all / Invert / Deselect.
pub(crate) fn selection_actions(app: &mut PainterApp, ui: &mut egui::Ui) {
    let has = app.selection_manager.has_selection();
    if ui
        .button("All")
        .on_hover_text("Select all (Ctrl+A)")
        .clicked()
    {
        app.selection_manager.select_all();
    }
    if ui
        .button("Invert")
        .on_hover_text("Invert selection (Ctrl+Shift+I)")
        .clicked()
    {
        app.selection_manager.invert();
    }
    if ui
        .add_enabled(has, egui::Button::new("Deselect"))
        .on_hover_text("Ctrl+D")
        .clicked()
    {
        app.selection_manager.clear_selection();
    }
}

/// The menu sliding out from the toolbar's Select button (`anchor`).
pub fn show(app: &mut PainterApp, ctx: &egui::Context, anchor: egui::Rect) {
    let open = app.modal_state.select_menu_open;
    let t = ctx.animate_bool_with_time(egui::Id::new("select_menu_anim"), open, 0.12);
    if t <= 0.0 {
        return;
    }
    let m = metrics(ctx);
    let width = if m.touch { 280.0 } else { 230.0 };
    // Slides out from under the toolbar edge.
    let x = anchor.right() + 4.0 - (1.0 - t) * width;
    let response = egui::Area::new(egui::Id::new("select_menu"))
        .fixed_pos(egui::pos2(x, anchor.top()))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    ui.label(RichText::new("SELECTION").small().strong().color(TEXT_DIM));
                    let current = match app.active_tool {
                        Tool::Select(kind) => Some(kind),
                        _ => None,
                    };
                    for (kind, icon, label, key) in TYPES {
                        let row_h = if m.touch { 44.0 } else { 30.0 };
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_h),
                            Sense::click(),
                        );
                        let selected = current == Some(kind);
                        let bg = if selected {
                            ACCENT
                        } else if resp.hovered() {
                            WIDGET_HOVER
                        } else {
                            BG_PANEL
                        };
                        ui.painter().rect_filled(rect, 0.0, bg);
                        let icon_rect = egui::Rect::from_min_size(
                            rect.min + egui::vec2(6.0, (row_h - 18.0) / 2.0),
                            egui::vec2(18.0, 18.0),
                        );
                        paint_icon(ui.painter(), icon_rect, icon, TEXT_STRONG);
                        ui.painter().text(
                            rect.left_center() + egui::vec2(32.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            label,
                            egui::TextStyle::Body.resolve(ui.style()),
                            TEXT_STRONG,
                        );
                        if !key.is_empty() && !m.touch {
                            ui.painter().text(
                                rect.right_center() - egui::vec2(8.0, 0.0),
                                egui::Align2::RIGHT_CENTER,
                                key,
                                egui::TextStyle::Small.resolve(ui.style()),
                                TEXT_DIM,
                            );
                        }
                        if resp.clicked() {
                            app.set_select_tool(kind);
                            // Brush settings live in this menu; keep it open for them.
                            if kind != SelectionType::Brush {
                                app.modal_state.select_menu_open = false;
                            }
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("Mode  (Shift add · Alt subtract)")
                            .small()
                            .color(TEXT_DIM),
                    );
                    mode_and_brush_controls(app, ui, false);
                    ui.separator();
                    ui.horizontal(|ui| selection_actions(app, ui));
                });
        })
        .response;

    // Click anywhere else closes it (the toolbar button toggles it itself).
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !response.rect.contains(p) && !anchor.contains(p));
    if open && clicked_outside {
        app.modal_state.select_menu_open = false;
    }
}
