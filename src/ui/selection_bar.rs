//! The selection bar: a small floating bar under the selection (above it
//! when there's no room) with what's done to a selection most: deselect,
//! invert, transform, copy, crop, fill and clear. Hidden while painting or
//! drawing the selection, so it never gets in the way of a stroke.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::view::render::ScreenMap;
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::icon_button;
use eframe::egui::{self, Stroke};

/// Room between the selection and the bar.
const GAP: f32 = 12.0;

/// Whether the bar shows: a selection, and nothing else going on.
fn wanted(app: &PainterApp) -> bool {
    let m = &app.modal_state;
    app.selection_manager.has_selection()
        && !app.selection_manager.is_dragging
        && !app.viewport.is_primary_down
        && !app.brush_state.is_drawing
        && !matches!(app.active_tool, Tool::Transform(_))
        && !crate::app::tools::transform::transform_running(app)
        && app.workspace.select.magnetic.is_none()
        && app.workspace.filter.session.is_none()
        && app.workspace.text.session.is_none()
        && !(m.menu_sheet_open
            || m.select_menu_open
            || m.symmetry_menu_open
            || m.shape_menu_open
            || m.show_new_canvas_modal
            || m.show_general_settings
            || m.show_shortcuts)
}

/// Where the bar goes for a selection shown over `sel` on screen, in the
/// canvas `area`, the bar `height` tall: its top-centre (`true`) or
/// bottom-centre point.
fn place(sel: egui::Rect, area: egui::Rect, height: f32) -> (egui::Pos2, bool) {
    let x = sel.center().x.clamp(area.left() + 8.0, area.right() - 8.0);
    if sel.bottom() + GAP + height <= area.bottom() - 8.0 {
        (egui::pos2(x, sel.bottom().max(area.top()) + GAP), true)
    } else if sel.top() - GAP - height >= area.top() + 8.0 {
        (egui::pos2(x, sel.top().min(area.bottom()) - GAP), false)
    } else {
        // The selection fills the view: along its bottom edge.
        (egui::pos2(x, area.bottom() - 16.0), false)
    }
}

pub(crate) fn selection_bar(
    app: &mut PainterApp,
    ctx: &egui::Context,
    map: &ScreenMap,
    area: egui::Rect,
) {
    if !wanted(app) {
        return;
    }
    let Some(b) = app.selection_manager.get_bounds() else {
        return;
    };
    let sel = egui::Rect::from_points(
        &[
            b.left_top(),
            b.right_top(),
            b.left_bottom(),
            b.right_bottom(),
        ]
        .map(|p| map.to_screen(p.to_vec2())),
    );
    let m = metrics(ctx);
    let size = if m.touch { m.tool_button } else { 28.0 };
    let (pos, below) = place(sel, area, size + 10.0);
    egui::Area::new(egui::Id::new("selection_bar"))
        .fixed_pos(pos)
        .pivot(if below {
            egui::Align2::CENTER_TOP
        } else {
            egui::Align2::CENTER_BOTTOM
        })
        .constrain_to(area.shrink(4.0))
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                .rounding(RADIUS_WINDOW)
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 2.0),
                    blur: 10.0,
                    spread: 0.0,
                    color: egui::Color32::from_black_alpha(90),
                })
                .inner_margin(egui::Margin::same(4.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        buttons(app, ui, size);
                    });
                });
        });
}

fn buttons(app: &mut PainterApp, ui: &mut egui::Ui, size: f32) {
    use crate::app::input::keymap::Action;
    // "What (its keys)", with the keys as they're set now.
    let tip = |what: &str, action: Action| match app.workspace.keymap.label(ui.ctx(), action) {
        Some(keys) => format!("{what} ({keys})"),
        None => what.to_string(),
    };
    let [deselect, invert, transform, clear] = [
        tip("Deselect", Action::Deselect),
        tip("Invert", Action::InvertSelection),
        tip("Transform", Action::Transform),
        tip("Clear", Action::DeletePixels),
    ];
    let divider = |ui: &mut egui::Ui| {
        ui.add_space(3.0);
        crate::ui::widgets::vdivider(ui);
        ui.add_space(3.0);
    };
    if icon_button(ui, Icon::Deselect, size, false, &deselect).clicked() {
        app.deselect();
    }
    if icon_button(ui, Icon::SelectInvert, size, false, &invert).clicked() {
        app.invert_selection();
    }
    divider(ui);
    if icon_button(ui, Icon::Transform, size, false, &transform).clicked() {
        app.set_transform_tool();
    }
    if icon_button(ui, Icon::Duplicate, size, false, "Copy (Ctrl+C)").clicked() {
        app.copy_selection(false);
    }
    if icon_button(ui, Icon::Crop, size, false, "Crop the canvas to it").clicked() {
        crate::ui::image_menu::crop_to_selection(app);
    }
    divider(ui);
    let fill = icon_button(ui, Icon::Bucket, size, false, "Fill with the brush colour");
    // The colour it fills with, as a bar under the bucket.
    let color = app.brush_state.brush.brush_options.color;
    let r = fill.rect;
    let swatch = egui::Rect::from_min_max(
        egui::pos2(r.left() + size * 0.22, r.bottom() - size * 0.16),
        egui::pos2(r.right() - size * 0.22, r.bottom() - size * 0.08),
    );
    ui.painter().rect_filled(
        swatch,
        1.0,
        egui::Color32::from_rgb(color.r(), color.g(), color.b()),
    );
    if fill.clicked() {
        app.fill_selection();
    }
    if icon_button(ui, Icon::Trash, size, false, &clear).clicked() {
        app.delete_selection_contents();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_goes_under_the_selection_else_over_it_else_inside() {
        let area = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));
        let rect = |y0, y1| egui::Rect::from_min_max(egui::pos2(100.0, y0), egui::pos2(300.0, y1));
        assert_eq!(
            place(rect(100.0, 200.0), area, 40.0),
            (egui::pos2(200.0, 212.0), true)
        );
        assert_eq!(
            place(rect(300.0, 580.0), area, 40.0),
            (egui::pos2(200.0, 288.0), false)
        );
        assert_eq!(
            place(rect(-50.0, 700.0), area, 40.0),
            (egui::pos2(200.0, 584.0), false)
        );
    }
}
