//! The top bar's right end: zoom and view controls (and, on touch
//! screens, the menu sheet and finger-painting buttons at its left).

use crate::PainterApp;
use crate::app::view::viewport::{MAX_ZOOM, MIN_ZOOM};
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, vdivider};
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

/// Touch mode: the menu sheet toggle (the menus live there instead of a top
/// bar) and one-finger painting on/off.
pub(crate) fn touch_buttons(app: &mut PainterApp, ui: &mut egui::Ui, bar_height: f32) {
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
    // No keyboard on a touch screen: undo/redo need buttons.
    crate::ui::widgets::vdivider(ui);
    if icon_button(ui, Icon::Undo, size, false, "Undo (two-finger tap)").clicked() {
        app.apply_history(false);
    }
    if icon_button(ui, Icon::Redo, size, false, "Redo (three-finger tap)").clicked() {
        app.apply_history(true);
    }
}

/// Right-to-left: `[flip] [ruler] [Fit] [zoom%]`, then the rotation if
/// the view is turned. The zoom's tooltip tells the canvas size.
pub(crate) fn view_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
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
    let ruler = app.workspace.guides.ruler.enabled;
    if icon_button(
        ui,
        Icon::Ruler,
        size,
        ruler,
        "Ruler (R): strokes run along it, or parallel to it",
    )
    .clicked()
    {
        app.set_ruler(!ruler);
    }
    let fit_tip = crate::app::input::keyboard::with_keycaps(
        ui.ctx(),
        "Fit to window (Ctrl+{0}); actual pixels: Ctrl+{1}",
    );
    if small_button(ui, "Fit", &fit_tip) {
        app.fit_view();
    }

    let mut percent = app.viewport.zoom * 100.0;
    let size = format!(
        "Zoom (drag or type). Canvas {} × {} px",
        app.canvas.width(),
        app.canvas.height()
    );
    let response = ui
        .add(
            egui::DragValue::new(&mut percent)
                .range(MIN_ZOOM * 100.0..=MAX_ZOOM * 100.0)
                .speed(1.0)
                .max_decimals(0)
                .suffix("%"),
        )
        .on_hover_text(size);
    if response.changed() {
        app.set_zoom_from_center(percent / 100.0);
    }

    let degrees = app.viewport.rotation.to_degrees().rem_euclid(360.0);
    if degrees > 0.05 && degrees < 359.95 {
        vdivider(ui);
        if small_button(ui, "Reset", "Reset rotation") {
            app.viewport.rotation = 0.0;
        }
        ui.label(dim(format!("{degrees:.0}°")));
    }
}
