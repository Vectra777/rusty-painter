//! Touch-mode overlays on the canvas: slim brush size / opacity faders on
//! the left edge (a thumb's reach away, like Procreate), and a contextual
//! action button (Deselect, Commit) since touch mode shows no tool options
//! in the top bar.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::selection::{SelectionMode, SelectionType};
use crate::ui::style::*;
use eframe::egui::{self, Color32, Sense, Stroke};

/// Touch target; the drawn track is much thinner.
const HIT_WIDTH: f32 = 36.0;
const FADER_HEIGHT: f32 = 170.0;
/// Faders shrink to this on short screens.
const MIN_FADER_HEIGHT: f32 = 60.0;
const TRACK_WIDTH: f32 = 4.0;
const HANDLE_SIZE: egui::Vec2 = egui::vec2(22.0, 8.0);
const GAP: f32 = 18.0;
const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 3000.0;

/// A slim vertical fader for `t` in 0..=1. Shows `value_text` beside the
/// handle only while it is being dragged.
fn fader(ui: &mut egui::Ui, height: f32, t: &mut f32, tooltip: &str, value_text: &str) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(HIT_WIDTH, height), Sense::click_and_drag());
    let active = response.dragged() || response.is_pointer_button_down_on();
    let response = response.on_hover_text(tooltip);

    let mut changed = false;
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let inner = rect.shrink2(egui::vec2(0.0, HANDLE_SIZE.y * 0.5));
        let new_t = ((inner.bottom() - pos.y) / inner.height()).clamp(0.0, 1.0);
        if (new_t - *t).abs() > f32::EPSILON {
            *t = new_t;
            changed = true;
        }
    }

    // Faint while idle so it doesn't compete with the artwork; the dark
    // backing keeps it legible over both a white canvas and the dark surround.
    let alpha = if active { 1.0 } else { 0.55 };
    let painter = ui.painter();
    let inner = rect.shrink2(egui::vec2(0.0, HANDLE_SIZE.y * 0.5));
    let x = rect.center().x;
    let track = egui::Rect::from_center_size(
        egui::pos2(x, inner.center().y),
        egui::vec2(TRACK_WIDTH + 4.0, inner.height() + 4.0),
    );
    painter.rect_filled(track, 0.0, Color32::from_black_alpha((110.0 * alpha) as u8));
    let handle_y = egui::lerp(inner.bottom()..=inner.top(), t.clamp(0.0, 1.0));
    let filled = egui::Rect::from_min_max(
        egui::pos2(x - TRACK_WIDTH * 0.5, handle_y),
        egui::pos2(x + TRACK_WIDTH * 0.5, inner.bottom()),
    );
    let fill = if active { ACCENT } else { TEXT };
    painter.rect_filled(filled, 0.0, fill.gamma_multiply(alpha));

    let handle = egui::Rect::from_center_size(egui::pos2(x, handle_y), HANDLE_SIZE);
    painter.rect_filled(handle, 0.0, TEXT_STRONG.gamma_multiply(alpha.max(0.8)));
    painter.rect_stroke(
        handle,
        0.0,
        Stroke::new(1.0_f32, Color32::from_black_alpha(160)),
    );

    if active {
        let galley = painter.layout_no_wrap(
            value_text.to_string(),
            egui::TextStyle::Body.resolve(ui.style()),
            TEXT_STRONG,
        );
        let bubble = egui::Rect::from_min_size(
            egui::pos2(rect.right() + 6.0, handle_y - galley.size().y * 0.5 - 4.0),
            galley.size() + egui::vec2(12.0, 8.0),
        );
        painter.rect_filled(bubble, 0.0, BG_PANEL);
        painter.rect_stroke(bubble, 0.0, Stroke::new(1.0_f32, BORDER));
        painter.galley(bubble.min + egui::vec2(6.0, 4.0), galley, TEXT_STRONG);
    }
    changed
}

fn faders(app: &mut PainterApp, ctx: &egui::Context, area: egui::Rect) {
    // Both faders fit the canvas height, with a margin top and bottom.
    let fader_height = ((area.height() - GAP - 24.0) * 0.5).clamp(MIN_FADER_HEIGHT, FADER_HEIGHT);
    let height = 2.0 * fader_height + GAP;
    let pos = egui::pos2(area.left() + 6.0, area.center().y - height * 0.5);

    egui::Area::new(egui::Id::new("canvas_faders"))
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = GAP;
            let options = &mut app.brush_state.brush.brush_options;

            // Size on a log scale, like the other size sliders.
            let span = (MAX_SIZE / MIN_SIZE).ln();
            let mut t = (options.diameter.max(MIN_SIZE) / MIN_SIZE).ln() / span;
            let size_text = format!("{:.0} px", options.diameter);
            let size_changed = fader(ui, fader_height, &mut t, "Brush size", &size_text);
            if size_changed {
                options.diameter = (MIN_SIZE * (t * span).exp()).round().max(MIN_SIZE);
            }

            let opacity_text = format!("{:.0}%", options.opacity * 100.0);
            let opacity_changed = fader(
                ui,
                fader_height,
                &mut options.opacity,
                "Opacity",
                &opacity_text,
            );

            if size_changed {
                app.brush_state.brush.is_changed = true;
            }
            if opacity_changed {
                app.brush_state.brush_preview.dirty = true;
            }
        });
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
        Tool::Gradient => context_bar(ctx, area, |ui| {
            crate::ui::tool_options::gradient_options(app, ui, true);
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
    faders(app, ctx, area);
    context_action(app, ctx, area);
}
