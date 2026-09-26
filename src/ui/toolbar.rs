//! Vertical tool strip on the far left: tools on top, the brush and
//! secondary colors at the bottom.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, paint_swatch};
use eframe::egui::{self, Sense, Stroke};

pub fn toolbar(app: &mut PainterApp, ctx: &egui::Context) {
    let m = metrics(ctx);
    let mut select_anchor = None;
    egui::SidePanel::left("toolbar")
        .exact_width(m.toolbar_width)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(BG_PANEL)
                .inner_margin(egui::Margin::symmetric(5.0, 6.0)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
            let size = m.tool_button;
            let eraser = app.is_eraser_active();
            // On a touch screen, tapping the tool that's already active
            // slides the tool settings panel in or out.
            let tool_button = |ui: &mut egui::Ui, app: &mut PainterApp, icon, selected, tip| {
                let clicked = icon_button(ui, icon, size, selected, tip).clicked();
                if clicked && selected && m.touch {
                    app.workspace.show_left_panel = !app.workspace.show_left_panel;
                    return false;
                }
                clicked
            };

            let brush_active = matches!(app.active_tool, Tool::Brush) && !eraser;
            if tool_button(ui, app, Icon::Brush, brush_active, "Brush (B)") {
                app.set_brush_tool(false);
            }
            if tool_button(ui, app, Icon::Eraser, eraser, "Eraser (E)") {
                app.set_brush_tool(true);
            }
            let smudge_active = matches!(app.active_tool, Tool::Smudge);
            if tool_button(
                ui,
                app,
                Icon::Smudge,
                smudge_active,
                "Smudge (S): smear paint with the brush's settings",
            ) {
                app.set_blend_tool(true);
            }
            let blur_active = matches!(app.active_tool, Tool::Blur);
            if tool_button(
                ui,
                app,
                Icon::Blur,
                blur_active,
                "Blur (S again): soften paint with the brush's settings",
            ) {
                app.set_blend_tool(false);
            }
            separator(ui, size);

            // One Select button: it shows the current type, and clicking it
            // slides out the menu to pick another type, the mode and more.
            let select_active = matches!(app.active_tool, Tool::Select(_));
            let icon = crate::ui::select_menu::icon_for(app.workspace.select_type);
            let response = icon_button(
                ui,
                icon,
                size,
                select_active,
                "Selection (M / L) — click for types and modes",
            );
            if response.clicked() {
                if !select_active {
                    app.set_select_tool(app.workspace.select_type);
                }
                app.modal_state.select_menu_open = !app.modal_state.select_menu_open;
            }
            select_anchor = Some(response.rect);
            separator(ui, size);

            let transform_active = matches!(app.active_tool, Tool::Transform(_));
            if tool_button(ui, app, Icon::Transform, transform_active, "Transform (V)") {
                app.set_transform_tool();
            }
            let picker_active = matches!(app.active_tool, Tool::Eyedropper);
            if tool_button(
                ui,
                app,
                Icon::Eyedropper,
                picker_active,
                "Eyedropper (I, or Alt+click)",
            ) {
                app.active_tool = Tool::Eyedropper;
            }
            let fill_active = matches!(app.active_tool, Tool::Fill);
            if tool_button(
                ui,
                app,
                Icon::Bucket,
                fill_active,
                "Fill (G): bucket or enclose",
            ) {
                app.active_tool = Tool::Fill;
            }
            let liquify_active = matches!(app.active_tool, Tool::Liquify);
            if tool_button(ui, app, Icon::Liquify, liquify_active, "Liquify (W)") {
                app.active_tool = Tool::Liquify;
            }
            separator(ui, size);
            let presets_open = app.brush_state.show_presets;
            if icon_button(ui, Icon::Presets, size, presets_open, "Brush presets (P)").clicked() {
                app.brush_state.show_presets = !presets_open;
            }
            let palette_open = app.workspace.palette.open;
            if icon_button(
                ui,
                Icon::Palette,
                size,
                palette_open,
                "Palette: extract colours, recolour a layer",
            )
            .clicked()
            {
                app.workspace.palette.open = !palette_open;
            }

            // No keyboard on a touch screen: undo/redo need buttons.
            if m.touch {
                separator(ui, size);
                if icon_button(ui, Icon::Undo, size, false, "Undo (two-finger tap)").clicked() {
                    app.apply_history(false);
                }
                if icon_button(ui, Icon::Redo, size, false, "Redo (three-finger tap)").clicked() {
                    app.apply_history(true);
                }
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                color_pair(app, ui, size);
            });
        });
    if let Some(anchor) = select_anchor {
        crate::ui::select_menu::show(app, ctx, anchor);
    }
}

fn separator(ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(4.0),
        rect.center().y,
        Stroke::new(1.0_f32, BORDER_LIGHT),
    );
}

/// Overlapping primary/secondary swatches with a swap control, like most
/// painting apps. Clicking the secondary swatch or the arrow swaps them.
fn color_pair(app: &mut PainterApp, ui: &mut egui::Ui, width: f32) {
    let swatch = (width * 0.64).round();
    let (area, _) = ui.allocate_exact_size(egui::vec2(width, swatch * 1.9), Sense::hover());
    let primary =
        egui::Rect::from_min_size(area.min + egui::vec2(0.0, 6.0), egui::vec2(swatch, swatch));
    let secondary = egui::Rect::from_min_size(
        area.min + egui::vec2(width - swatch, 6.0 + swatch * 0.55),
        egui::vec2(swatch, swatch),
    );
    let swap_size = (swatch * 0.55).round();
    let swap_rect = egui::Rect::from_min_size(
        area.min + egui::vec2(width - swap_size, 0.0),
        egui::vec2(swap_size, swap_size),
    );

    let secondary_resp = ui.interact(secondary, ui.id().with("secondary_color"), Sense::click());
    let swap_resp = ui.interact(swap_rect, ui.id().with("swap_colors"), Sense::click());
    let touch = metrics(ui.ctx()).touch;
    let primary_sense = if touch {
        Sense::click()
    } else {
        Sense::hover()
    };
    let primary_resp = ui.interact(primary, ui.id().with("primary_color"), primary_sense);

    let painter = ui.painter();
    paint_swatch(painter, secondary, app.brush_state.secondary_color);
    painter.rect_stroke(secondary.expand(1.0), 0.0, Stroke::new(1.0_f32, BG_PANEL));
    paint_swatch(painter, primary, app.brush_state.brush.brush_options.color);
    painter.rect_stroke(primary.expand(1.0), 0.0, Stroke::new(1.0_f32, BG_PANEL));
    painter.rect_stroke(primary, 0.0, Stroke::new(1.0_f32, BORDER_LIGHT));
    let swap_color = if swap_resp.hovered() {
        TEXT_STRONG
    } else {
        TEXT_DIM
    };
    paint_icon(painter, swap_rect, Icon::Swap, swap_color);

    // On a touch screen the brush color opens/closes the color & layers panel.
    if touch {
        if primary_resp
            .on_hover_text("Brush color — tap to show/hide colors & layers")
            .clicked()
        {
            app.workspace.show_right_panel = !app.workspace.show_right_panel;
        }
    } else {
        primary_resp.on_hover_text("Brush color");
    }
    let swap_clicked = swap_resp.on_hover_text("Swap colors (X)").clicked();
    let secondary_clicked = secondary_resp
        .on_hover_text("Secondary color — click to swap (X)")
        .clicked();
    if swap_clicked || secondary_clicked {
        app.swap_colors();
    }
}
