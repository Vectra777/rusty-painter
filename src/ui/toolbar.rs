//! Vertical tool strip on the far left: tools on top, the brush and
//! secondary colors at the bottom.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, paint_swatch};
use eframe::egui::{self, Sense, Stroke};

/// Smallest a tool button shrinks to on short screens before the strip scrolls.
const MIN_TOOL_BUTTON: f32 = 28.0;
const MIN_TOOL_BUTTON_TOUCH: f32 = 34.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const ITEM_SPACING: f32 = 2.0;
const MARGIN_Y: f32 = 6.0;

/// Button side that fits every tool (and the colors) in `height`, if any.
fn fitting_button_size(height: f32, touch: bool, preferred: f32) -> Option<f32> {
    // The tools and the brush-settings button.
    let buttons = 16.0;
    let separators = 3.0;
    // The color pair is about 1.22 buttons tall.
    let fixed = 2.0 * MARGIN_Y
        + separators * SEPARATOR_HEIGHT
        + (buttons + separators + 1.0) * ITEM_SPACING;
    let size = ((height - fixed) / (buttons + 1.22)).floor().min(preferred);
    let min = if touch {
        MIN_TOOL_BUTTON_TOUCH
    } else {
        MIN_TOOL_BUTTON
    };
    (size >= min).then_some(size)
}

pub fn toolbar(app: &mut PainterApp, ctx: &egui::Context) {
    let m = metrics(ctx);
    let fitting = fitting_button_size(ctx.available_rect().height(), m.touch, m.tool_button);
    // Too short even for the smallest buttons: keep them usable and scroll.
    let size = fitting.unwrap_or(if m.touch {
        MIN_TOOL_BUTTON_TOUCH
    } else {
        MIN_TOOL_BUTTON
    });
    let mut anchors = Anchors::default();
    egui::SidePanel::left("toolbar")
        .exact_width(size + m.toolbar_width - m.tool_button)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(BG_PANEL)
                .inner_margin(egui::Margin::symmetric(5.0, MARGIN_Y)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, ITEM_SPACING);
            if fitting.is_some() {
                anchors = tool_buttons(app, ui, size, m.touch);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    color_pair(app, ui, size);
                    settings_button(app, ui, size);
                });
            } else {
                // The colors stay pinned at the bottom; the tools scroll.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    color_pair(app, ui, size);
                    settings_button(app, ui, size);
                    separator(ui, size);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        let out = egui::ScrollArea::vertical()
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| {
                                anchors = tool_buttons(app, ui, size, m.touch);
                            });
                        // More tools below: say so (the strip scrolls by drag
                        // or wheel, with no bar to see).
                        let r = out.inner_rect;
                        let hidden = out.content_size.y - out.state.offset.y - r.height();
                        if hidden > 2.0 {
                            let band = egui::Rect::from_min_max(
                                egui::pos2(r.left(), r.bottom() - 12.0),
                                r.right_bottom(),
                            );
                            ui.painter().rect_filled(band, 0.0, BG_PANEL);
                            let c = egui::pos2(r.center().x, r.bottom() - 6.0);
                            ui.painter().add(egui::Shape::convex_polygon(
                                vec![
                                    c + egui::vec2(-6.0, -4.0),
                                    c + egui::vec2(6.0, -4.0),
                                    c + egui::vec2(0.0, 3.0),
                                ],
                                ACCENT,
                                Stroke::NONE,
                            ));
                        }
                    });
                });
            }
        });
    if let Some(anchor) = anchors.select {
        crate::ui::select_menu::show(app, ctx, anchor);
    }
    if let Some(anchor) = anchors.symmetry {
        crate::ui::symmetry_menu::show(app, ctx, anchor);
    }
    if let Some(anchor) = anchors.shape {
        crate::ui::shape_menu::show(app, ctx, anchor);
    }
}

/// Buttons that menus slide out from.
#[derive(Default)]
struct Anchors {
    select: Option<egui::Rect>,
    symmetry: Option<egui::Rect>,
    shape: Option<egui::Rect>,
}

/// The tool buttons, top to bottom.
fn tool_buttons(app: &mut PainterApp, ui: &mut egui::Ui, size: f32, touch: bool) -> Anchors {
    let eraser = app.is_eraser_active();
    // On a touch screen, tapping the tool that's already active
    // slides the tool settings panel in or out.
    let tool_button = |ui: &mut egui::Ui, app: &mut PainterApp, icon, selected, tip| {
        let clicked = icon_button(ui, icon, size, selected, tip).clicked();
        if clicked && selected && touch {
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
    let select_anchor = Some(response.rect);
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
    let gradient_active = matches!(app.active_tool, Tool::Gradient);
    if tool_button(
        ui,
        app,
        Icon::Gradient,
        gradient_active,
        "Gradient (Shift+G): linear, radial, reflected, angle",
    ) {
        app.active_tool = Tool::Gradient;
    }
    let text_active = matches!(app.active_tool, Tool::Text);
    if tool_button(
        ui,
        app,
        Icon::Text,
        text_active,
        "Text: click the canvas to type",
    ) {
        app.active_tool = Tool::Text;
    }
    // One Shape button: it shows the current shape; clicking it picks the
    // tool, and again opens the menu of shapes.
    let shape_active = matches!(app.active_tool, Tool::Shape(_));
    let kind = app.workspace.shapes.last_kind;
    let response = icon_button(
        ui,
        crate::ui::shape_menu::icon_for(kind),
        size,
        shape_active,
        "Shapes (U): line, rectangle, ellipse, polygon — click again for options",
    );
    if response.clicked() {
        if shape_active {
            app.modal_state.shape_menu_open = !app.modal_state.shape_menu_open;
        } else {
            app.set_shape_tool(kind);
        }
    }
    let shape_anchor = Some(response.rect);
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
    let mirroring = app.workspace.symmetry.is_active();
    let response = icon_button(
        ui,
        Icon::Symmetry,
        size,
        mirroring || app.modal_state.symmetry_menu_open,
        "Mirror painting: axes, mandala",
    );
    if response.clicked() {
        app.modal_state.symmetry_menu_open = !app.modal_state.symmetry_menu_open;
    }
    let symmetry_anchor = Some(response.rect);

    Anchors {
        select: select_anchor,
        symmetry: symmetry_anchor,
        shape: shape_anchor,
    }
}

/// Opens and closes the brush (tool settings) panel.
fn settings_button(app: &mut PainterApp, ui: &mut egui::Ui, size: f32) {
    let open = app.workspace.show_left_panel;
    if icon_button(ui, Icon::Sliders, size, open, "Brush settings").clicked() {
        app.workspace.show_left_panel = !open;
    }
}

fn separator(ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, SEPARATOR_HEIGHT), Sense::hover());
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

    // On a touch screen the brush color opens/closes the colour panel.
    if touch {
        if primary_resp
            .on_hover_text("Brush color — tap to show/hide the colour panel")
            .clicked()
        {
            app.workspace.show_color = !app.workspace.show_color;
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
