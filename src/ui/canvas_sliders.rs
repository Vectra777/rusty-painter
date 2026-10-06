//! Touch-mode controls: brush size / opacity sliders on the top bar, and
//! on the canvas a contextual action button (Deselect, Commit), since touch
//! mode shows no tool options in the top bar, and the brush palette button.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::selection::{SelectionMode, SelectionType};
use crate::ui::style::*;
use eframe::egui::{self, Stroke};

/// Touch target of the brush palette button.
const HIT_WIDTH: f32 = 36.0;
/// Each top-bar slider's track.
const SLIDER_WIDTH: f32 = 110.0;
const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 3000.0;

/// Touch mode, on the top bar: brush size and opacity, a thumb's reach
/// from the tools.
pub(crate) fn bar_sliders(app: &mut PainterApp, ui: &mut egui::Ui, slider_width: Option<f32>) {
    ui.spacing_mut().slider_width = slider_width.unwrap_or(SLIDER_WIDTH);
    let options = &mut app.brush_state.brush.brush_options;
    let size = ui
        .add(
            egui::Slider::new(&mut options.diameter, MIN_SIZE..=MAX_SIZE)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px"),
        )
        .on_hover_text("Brush size");
    if size.changed() {
        options.diameter = options.diameter.round().max(MIN_SIZE);
        app.brush_state.brush.is_changed = true;
    }
    let options = &mut app.brush_state.brush.brush_options;
    let opacity = ui
        .add(
            egui::Slider::new(&mut options.opacity, 0.0..=1.0)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .custom_parser(|s| {
                    s.trim_end_matches('%')
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .map(|v| v / 100.0)
                }),
        )
        .on_hover_text("Opacity");
    if opacity.changed() {
        app.brush_state.brush_preview.dirty = true;
    }
}

/// A floating button at the top of the canvas for the one action the
/// current tool needs (the desktop options bar has these).
fn context_action(app: &mut PainterApp, ctx: &egui::Context, area: egui::Rect) {
    match app.active_tool {
        Tool::Transform(_) => context_bar(ctx, area, |ui| {
            crate::ui::tool_options::transform_controls(app, ui)
        }),
        // No Shift/Alt on a touch screen: the modes and actions as buttons.
        Tool::Select(kind) => context_bar(ctx, area, |ui| {
            crate::ui::widgets::segmented(
                ui,
                &mut app.selection_manager.mode,
                &[
                    (SelectionMode::Replace, "Replace"),
                    (SelectionMode::Add, "Add"),
                    (SelectionMode::Subtract, "Erase"),
                    (SelectionMode::Intersect, "Intersect"),
                ],
                true,
            );
            if matches!(kind, SelectionType::Magnetic | SelectionType::Polygon)
                && app.workspace.select.magnetic.is_some()
            {
                crate::ui::widgets::vdivider(ui);
                if ui.button("Close").clicked() {
                    app.magnetic_close();
                }
                if ui.button("Undo point").clicked() {
                    app.magnetic_undo_anchor();
                }
            }
            crate::ui::widgets::vdivider(ui);
            if ui.button("Invert").clicked() {
                app.invert_selection();
            }
            let has = app.selection_manager.has_selection();
            if ui.add_enabled(has, egui::Button::new("Deselect")).clicked() {
                app.deselect();
            }
            if ui
                .add_enabled(
                    has && !app.patch_running(),
                    egui::Button::new("Fill from surroundings"),
                )
                .clicked()
            {
                app.content_aware_fill();
            }
        }),
        Tool::Shape(_) => context_bar(ctx, area, |ui| {
            crate::ui::shape_menu::shape_controls(app, ui, true);
            crate::ui::widgets::vdivider(ui);
            crate::ui::shape_menu::shape_actions(app, ui);
        }),
        _ => {}
    }
}

/// A floating bar at the top of the canvas for the current tool's actions
/// (the desktop options bar has these); wraps on a narrow screen.
fn context_bar(ctx: &egui::Context, area: egui::Rect, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(egui::Id::new("canvas_context_action"))
        .fixed_pos(egui::pos2(area.center().x, area.top() + 12.0))
        .pivot(egui::Align2::CENTER_TOP)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                .inner_margin(egui::Margin::same(6.0))
                .show(ui, |ui| {
                    ui.set_max_width(area.width() - 36.0);
                    ui.horizontal_wrapped(add_contents);
                });
        });
}

/// Touch-mode canvas overlays for the canvas `area`.
pub fn canvas_sliders(app: &mut PainterApp, ctx: &egui::Context, area: egui::Rect) {
    // They'd float over the menu sheet and dialogs.
    let m = &app.modal_state;
    let covered = m.menu_sheet_open
        || m.select_menu_open
        || m.symmetry_menu_open
        || m.shape_menu_open
        || m.show_new_canvas_modal
        || m.show_general_settings
        || m.show_shortcuts
        || app.export_state.show_modal
        // A panel floating over the canvas (a phone).
        || (ctx.screen_rect().width() < crate::app::layout::NARROW_WIDTH && app.any_panel_open());
    if !app.workspace.touch_mode || covered {
        return;
    }
    // Beside the side panels sliding over the canvas, not under them.
    let mut area = area;
    area.min.x = (area.min.x + crate::app::layout::left_cover(ctx)).min(area.max.x - 60.0);
    context_action(app, ctx, area);
    palette_button(app, ctx, area);
}

/// A button in the bottom-left corner that opens the pop-up brush palette
/// (a right click does on the desktop).
fn palette_button(app: &mut PainterApp, ctx: &egui::Context, area: egui::Rect) {
    let size = HIT_WIDTH;
    egui::Area::new(egui::Id::new("canvas_palette_button"))
        .fixed_pos(egui::pos2(area.left() + 6.0, area.bottom() - size - 12.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let icon = crate::ui::icons::Icon::Brush;
            let open = app.brush_state.library.radial.is_some();
            if crate::ui::widgets::icon_button(ui, icon, size, open, "Brush palette").clicked() {
                app.open_radial_palette(area.center(), false);
            }
        });
}
