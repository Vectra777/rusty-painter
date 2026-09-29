//! The Select tool's slide-out menu (one toolbar button for every selection
//! type) and the selection controls it shares with the options bar.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::select::SampleSource;
use crate::selection::{SelectionMode, SelectionType};
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use crate::ui::widgets::{segmented, slider_row, vdivider};
use eframe::egui::{self, RichText, Sense, Stroke};

/// Every selection type with its icon, label and shortcut hint.
pub(crate) const TYPES: [(SelectionType, Icon, &str, &str); 8] = [
    (SelectionType::Rectangle, Icon::SelectRect, "Rectangle", "M"),
    (SelectionType::Circle, Icon::SelectEllipse, "Ellipse", "M"),
    (SelectionType::Lasso, Icon::Lasso, "Lasso", "L"),
    (
        SelectionType::Magnetic,
        Icon::Magnetic,
        "Magnetic lasso",
        "L",
    ),
    (SelectionType::Polygon, Icon::SelectPolygon, "Polygon", "L"),
    (SelectionType::Wand, Icon::Wand, "Magic wand", "Q"),
    (
        SelectionType::ColorRange,
        Icon::ColorRange,
        "Colour range",
        "Q",
    ),
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

/// Whether a selection type has settings (the menu stays open to show them).
fn has_settings(kind: SelectionType) -> bool {
    !matches!(kind, SelectionType::Rectangle | SelectionType::Circle)
}

/// A labelled slider: inline in the options bar, a property row in panels.
fn setting(ui: &mut egui::Ui, compact: bool, label: &str, slider: impl egui::Widget) -> bool {
    if compact {
        ui.label(RichText::new(label).color(TEXT_DIM));
        ui.add(slider).changed()
    } else {
        slider_row(ui, label, slider).changed()
    }
}

fn source_picker(ui: &mut egui::Ui, source: &mut SampleSource, compact: bool) -> bool {
    if compact {
        ui.label(RichText::new("Look at").color(TEXT_DIM));
    }
    segmented(
        ui,
        source,
        &[
            (SampleSource::Layer, "Layer"),
            (SampleSource::AllVisible, "All layers"),
            (SampleSource::Reference, "Reference"),
        ],
        compact,
    )
}

/// The combine mode (Replace / Add / Erase / Intersect), plus the active
/// selection type's settings.
pub(crate) fn mode_and_brush_controls(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) {
    segmented(
        ui,
        &mut app.selection_manager.mode,
        &[
            (SelectionMode::Replace, "Replace"),
            (SelectionMode::Add, "Add"),
            (SelectionMode::Subtract, "Erase"),
            (SelectionMode::Intersect, "Intersect"),
        ],
        compact,
    );
    let Tool::Select(kind) = app.active_tool else {
        return;
    };
    if compact {
        vdivider(ui);
    }
    let mut rerun = false;
    match kind {
        SelectionType::Brush => {
            let sel = &mut app.selection_manager;
            setting(
                ui,
                compact,
                "Size",
                crate::ui::widgets::reset(&mut sel.brush_radius, |v| {
                    egui::Slider::new(v, 1.0..=500.0)
                        .logarithmic(true)
                        .max_decimals(0)
                        .suffix(" px")
                }),
            );
            setting(
                ui,
                compact,
                "Hardness",
                crate::ui::widgets::reset(&mut sel.brush_hardness, |v| {
                    egui::Slider::new(v, 0.0..=1.0)
                }),
            );
            smoothing(app, ui, compact);
            ui.checkbox(&mut app.workspace.patch.smart_patch, "Smart patch")
                .on_hover_text("Paint over something to remove it: it's filled from its surroundings on release");
            if app.workspace.patch.smart_patch {
                ui.checkbox(&mut app.workspace.patch.sample_all, "Sample all layers");
            }
        }
        SelectionType::Lasso => smoothing(app, ui, compact),
        SelectionType::Wand => {
            let w = &mut app.workspace.select.wand;
            rerun |= setting(
                ui,
                compact,
                "Tolerance",
                crate::ui::widgets::reset(&mut w.tolerance, |v| egui::Slider::new(v, 0..=255)),
            );
            rerun |= ui
                .checkbox(&mut w.contiguous, "Contiguous")
                .on_hover_text("Only the connected area; off selects the colour everywhere")
                .changed();
            rerun |= source_picker(ui, &mut w.source, compact);
            rerun |= setting(
                ui,
                compact,
                "Close gaps",
                crate::ui::widgets::reset(&mut w.gap, |v| {
                    egui::Slider::new(v, 0..=40).suffix(" px")
                }),
            );
            rerun |= setting(
                ui,
                compact,
                "Grow",
                crate::ui::widgets::reset(&mut w.grow, |v| {
                    egui::Slider::new(v, -40..=40).suffix(" px")
                }),
            );
            rerun |= ui.checkbox(&mut w.antialias, "Smooth edge").changed();
        }
        SelectionType::ColorRange => {
            let c = &mut app.workspace.select.color;
            rerun |= setting(
                ui,
                compact,
                "Tolerance",
                crate::ui::widgets::reset(&mut c.tolerance, |v| {
                    egui::Slider::new(v, 0.0..=60.0).max_decimals(1)
                }),
            );
            rerun |= setting(
                ui,
                compact,
                "Softness",
                crate::ui::widgets::reset(&mut c.softness, |v| {
                    egui::Slider::new(v, 0.0..=40.0).max_decimals(1)
                }),
            );
            rerun |= source_picker(ui, &mut c.source, compact);
        }
        SelectionType::Magnetic | SelectionType::Polygon => {
            let session = app.workspace.select.magnetic.is_some();
            if ui
                .add_enabled(session, egui::Button::new("Close"))
                .on_hover_text("Close the outline (Enter, or double-click)")
                .clicked()
            {
                app.magnetic_close();
            }
            if ui
                .add_enabled(session, egui::Button::new("Undo point"))
                .on_hover_text("Remove the last point (Backspace)")
                .clicked()
            {
                app.magnetic_undo_anchor();
            }
            if !compact {
                ui.label(
                    RichText::new(if kind == SelectionType::Polygon {
                        "Click each corner. Click the start point, double-click or \
                         press Enter to close."
                    } else {
                        "Click along an edge, or drag with a pen. Click the start point \
                         or double-click to close."
                    })
                    .small()
                    .color(TEXT_DIM),
                );
            }
        }
        SelectionType::Rectangle | SelectionType::Circle => {}
    }
    if rerun {
        app.rerun_last_pick();
    }
}

fn smoothing(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) {
    setting(
        ui,
        compact,
        "Smoothing",
        crate::ui::widgets::reset(&mut app.selection_manager.smoothing, |v| {
            egui::Slider::new(v, 0.0..=1.0)
        }),
    );
}

/// Select all / Invert / Deselect.
pub(crate) fn selection_actions(app: &mut PainterApp, ui: &mut egui::Ui) {
    let has = app.selection_manager.has_selection();
    if ui
        .button("All")
        .on_hover_text("Select all (Ctrl+A)")
        .clicked()
    {
        app.select_all();
    }
    if ui
        .button("Invert")
        .on_hover_text("Invert selection (Ctrl+Shift+I)")
        .clicked()
    {
        app.invert_selection();
    }
    if ui
        .add_enabled(has, egui::Button::new("Deselect"))
        .on_hover_text("Ctrl+D")
        .clicked()
    {
        app.deselect();
    }
    if ui
        .add_enabled(
            has && !app.patch_running(),
            egui::Button::new("Fill from surroundings"),
        )
        .on_hover_text(
            "Content-aware fill: replace what's selected with texture from around it (Shift+F5)",
        )
        .clicked()
    {
        app.content_aware_fill();
    }
    ui.checkbox(&mut app.workspace.patch.sample_all, "All layers")
        .on_hover_text("Fill from everything visible, not just this layer");
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
                    // Scrolls when taller than the screen below the button.
                    let max_height = ctx.screen_rect().bottom() - anchor.top() - 24.0;
                    egui::ScrollArea::vertical()
                        .max_height(max_height.max(120.0))
                        .show(ui, |ui| menu_contents(app, ui, m.touch));
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

fn menu_contents(app: &mut PainterApp, ui: &mut egui::Ui, touch: bool) {
    ui.label(RichText::new("SELECTION").small().strong().color(TEXT_DIM));
    let current = match app.active_tool {
        Tool::Select(kind) => Some(kind),
        _ => None,
    };
    for (kind, icon, label, key) in TYPES {
        let row_h = if touch { 44.0 } else { 30.0 };
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::click());
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
        if !key.is_empty() && !touch {
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
            // Settings live in this menu; keep it open for them.
            if !has_settings(kind) {
                app.modal_state.select_menu_open = false;
            }
        }
    }
    ui.separator();
    ui.label(
        RichText::new("Mode  (Shift add · Alt erase)")
            .small()
            .color(TEXT_DIM),
    );
    mode_and_brush_controls(app, ui, false);
    ui.separator();
    ui.horizontal(|ui| selection_actions(app, ui));
}
