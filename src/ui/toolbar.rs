//! Vertical tool strip on the far left: tools on top, the brush and
//! secondary colors at the bottom.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::fill::FillMode;
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
    // The tools, and the brush presets and settings buttons.
    let buttons = 15.0;
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
    let width = size + m.toolbar_width - m.tool_button;
    let frame = egui::Frame::none()
        .fill(BG_PANEL)
        .inner_margin(egui::Margin::symmetric(5.0, MARGIN_Y));
    let area = ctx.available_rect();
    let overlay = m.touch && app.workspace.autohide_panels;
    let shown = if overlay {
        // The brush panel floats beside it (layout::show_panels).
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("toolbar_overlay_width"), width));
        crate::app::layout::left_reveal(app, ctx, area.left() + width)
    } else {
        1.0
    };
    let (offset, _) = crate::app::layout::left_slide(app, ctx, width);
    let mut strip = |ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, ITEM_SPACING);
        if fitting.is_some() {
            anchors = tool_buttons(app, ui, size, m.touch);
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                color_pair(app, ui, size);
                settings_button(app, ui, size);
                presets_button(app, ui, size, m.touch);
            });
        } else {
            // The colors stay pinned at the bottom; the tools scroll.
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                color_pair(app, ui, size);
                settings_button(app, ui, size);
                presets_button(app, ui, size, m.touch);
                separator(ui, size);
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    let out = egui::ScrollArea::vertical()
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
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
                            accent(),
                            Stroke::NONE,
                        ));
                    }
                });
            });
        }
    };
    if overlay {
        // Over the canvas, fading with the pen's distance: the canvas
        // keeps its size whether the strip shows or not.
        egui::Area::new(egui::Id::new("toolbar_overlay"))
            .fixed_pos(area.left_top() + egui::vec2(offset, 0.0))
            .order(egui::Order::Middle)
            .interactable(shown > 0.3)
            .show(ctx, |ui| {
                ui.set_opacity(shown);
                frame.show(ui, |ui| {
                    ui.set_width(width - 10.0);
                    ui.set_height(area.height() - 2.0 * MARGIN_Y);
                    strip(ui);
                });
            });
    } else {
        egui::SidePanel::left("toolbar")
            .exact_width(width)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| strip(ui));
    }
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
    // The Eraser, and Lasso delete (draw around an area to erase it).
    let lasso_delete =
        matches!(app.active_tool, Tool::Fill) && app.workspace.fill.mode == FillMode::LassoDelete;
    let groups = &mut app.workspace.tool_groups;
    if lasso_delete || eraser {
        groups.lasso_delete = lasso_delete;
    }
    let members = [
        Member::new(Icon::Eraser, "Eraser", "E", eraser),
        Member::new(Icon::LassoDelete, "Lasso delete", "", lasso_delete),
    ];
    match group_button(
        ui,
        "eraser",
        &members,
        groups.lasso_delete as usize,
        size,
        touch,
    ) {
        Some(Pick::Tool(0)) => app.set_brush_tool(true),
        Some(Pick::Tool(_)) => {
            app.active_tool = Tool::Fill;
            app.workspace.fill.mode = FillMode::LassoDelete;
        }
        Some(Pick::Again) => app.workspace.show_left_panel = !app.workspace.show_left_panel,
        None => {}
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
    let response = icon_button(
        ui,
        Icon::SelectRect,
        size,
        select_active,
        "Selection (M / L): click for types and modes",
    );
    more_dot(ui, response.rect, select_active);
    if response.clicked() {
        if !select_active {
            app.set_select_tool(app.workspace.select_type);
        }
        app.modal_state.select_menu_open = !app.modal_state.select_menu_open;
    }
    let select_anchor = Some(response.rect);
    separator(ui, size);

    // Transform, and Animate (moving the layer over time).
    let transform_active = matches!(app.active_tool, Tool::Transform(_));
    let animate_active = matches!(app.active_tool, Tool::Animate);
    let groups = &mut app.workspace.tool_groups;
    if transform_active || animate_active {
        groups.animate = animate_active;
    }
    let members = [
        Member::new(
            Icon::Transform,
            "Transform: move, scale, turn or distort the drawing itself",
            "V",
            transform_active,
        ),
        Member::new(
            Icon::Motion,
            "Animate: move the layer over time, a key at each drag",
            "A",
            animate_active,
        ),
    ];
    match group_button(
        ui,
        "transform",
        &members,
        groups.animate as usize,
        size,
        touch,
    ) {
        Some(Pick::Tool(0)) => app.set_transform_tool(),
        Some(Pick::Tool(_)) => {
            app.active_tool = Tool::Animate;
            app.workspace.animation.show_timeline = true;
        }
        Some(Pick::Again) => app.workspace.show_left_panel = !app.workspace.show_left_panel,
        None => {}
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
    let fill_active = matches!(app.active_tool, Tool::Fill) && !lasso_delete;
    if tool_button(
        ui,
        app,
        Icon::Bucket,
        fill_active,
        "Fill (G): bucket or enclose",
    ) {
        app.active_tool = Tool::Fill;
        if app.workspace.fill.mode == FillMode::LassoDelete {
            app.workspace.fill.mode = FillMode::Bucket;
        }
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
        "Shapes (U): line, rectangle, ellipse, polygon; click again for options",
    );
    more_dot(ui, response.rect, shape_active);
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

/// Opens and closes the brush presets, beside the brush settings (touch
/// mode has them on the top bar).
fn presets_button(app: &mut PainterApp, ui: &mut egui::Ui, size: f32, touch: bool) {
    let open = app.brush_state.show_presets;
    if !touch && icon_button(ui, Icon::Presets, size, open, "Brush presets (P)").clicked() {
        app.brush_state.show_presets = !open;
    }
}

/// One tool of a grouped button.
struct Member {
    icon: Icon,
    name: &'static str,
    key: &'static str,
    active: bool,
}

impl Member {
    fn new(icon: Icon, name: &'static str, key: &'static str, active: bool) -> Self {
        Self {
            icon,
            name,
            key,
            active,
        }
    }
}

/// What a grouped button was asked for.
enum Pick {
    /// This tool (clicked, or chosen in the flyout).
    Tool(usize),
    /// Touch: the tool already active was tapped (the settings panel slides).
    Again,
}

/// A button holding several tools, like Photoshop's: it shows `members
/// [shown]`, and a click picks that one; a double-click, right-click or
/// long press opens the others beside it. A dot in its corner says so.
fn group_button(
    ui: &mut egui::Ui,
    salt: &str,
    members: &[Member],
    shown: usize,
    size: f32,
    touch: bool,
) -> Option<Pick> {
    let any_active = members.iter().any(|m| m.active);
    let member = &members[shown.min(members.len() - 1)];
    let tip = if member.key.is_empty() {
        format!("{} (double-click for more)", member.name)
    } else {
        format!("{} ({}; double-click for more)", member.name, member.key)
    };
    let response = icon_button(ui, member.icon, size, any_active, &tip);
    more_dot(ui, response.rect, any_active);
    let open_id = ui.id().with(("tool_group", salt));
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let mut pick = None;
    if response.double_clicked() || response.secondary_clicked() {
        open = true;
    } else if response.clicked() {
        pick = Some(if touch && member.active {
            Pick::Again
        } else {
            Pick::Tool(shown)
        });
    }
    if open {
        let area = egui::Area::new(open_id.with("flyout"))
            .order(egui::Order::Foreground)
            .fixed_pos(response.rect.right_top() + egui::vec2(6.0, 0.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, m) in members.iter().enumerate() {
                        if flyout_row(ui, m) {
                            pick = Some(Pick::Tool(i));
                        }
                    }
                })
            });
        // Picked, or a press anywhere else: closed.
        let pressed_outside = ui.input(|i| i.pointer.any_pressed())
            && !area.response.contains_pointer()
            && !response.contains_pointer();
        if pick.is_some() || pressed_outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    pick
}

/// A row of a grouped button's flyout: icon, name, key. Returns whether
/// it was clicked.
fn flyout_row(ui: &mut egui::Ui, m: &Member) -> bool {
    let height = ui.spacing().interact_size.y.max(26.0);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let name = m.name.split(':').next().unwrap_or(m.name);
    let text = ui
        .painter()
        .layout_no_wrap(name.to_string(), font.clone(), TEXT);
    let key = ui
        .painter()
        .layout_no_wrap(m.key.to_string(), font, TEXT_DIM);
    let width = (height + 8.0 + text.size().x + 24.0 + key.size().x).max(160.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
    let painter = ui.painter();
    let (bg, fg) = if m.active {
        (accent(), accent_text())
    } else if response.hovered() {
        (WIDGET_HOVER, TEXT_STRONG)
    } else {
        (egui::Color32::TRANSPARENT, TEXT)
    };
    painter.rect_filled(rect, RADIUS_WIDGET, bg);
    let icon = egui::Rect::from_min_size(
        rect.min + egui::vec2(6.0, height * 0.2),
        egui::Vec2::splat(height * 0.6),
    );
    paint_icon(painter, icon, m.icon, fg);
    painter.galley_with_override_text_color(
        egui::pos2(icon.right() + 8.0, rect.center().y - text.size().y * 0.5),
        text,
        fg,
    );
    painter.galley_with_override_text_color(
        egui::pos2(
            rect.right() - 8.0 - key.size().x,
            rect.center().y - key.size().y * 0.5,
        ),
        key,
        if m.active { fg } else { TEXT_DIM },
    );
    response.clicked()
}

/// The tiny dot in a button's lower right corner: it holds more than one
/// option.
fn more_dot(ui: &egui::Ui, rect: egui::Rect, selected: bool) {
    let r = (rect.width() * 0.05).clamp(1.5, 2.5);
    let c = rect.right_bottom() - egui::vec2(r + 3.0, r + 3.0);
    let color = if selected { accent_text() } else { TEXT_DIM };
    ui.painter().circle_filled(c, r, color);
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
    painter.rect_stroke(
        secondary.expand(1.0),
        RADIUS_SMALL,
        Stroke::new(1.0_f32, BG_PANEL),
    );
    paint_swatch(painter, primary, app.brush_state.brush.brush_options.color);
    painter.rect_stroke(
        primary.expand(1.0),
        RADIUS_SMALL,
        Stroke::new(1.0_f32, BG_PANEL),
    );
    painter.rect_stroke(primary, RADIUS_SMALL, Stroke::new(1.0_f32, BORDER_LIGHT));
    let swap_color = if swap_resp.hovered() {
        TEXT_STRONG
    } else {
        TEXT_DIM
    };
    paint_icon(painter, swap_rect, Icon::Swap, swap_color);

    // On a touch screen the brush color opens/closes the colour panel.
    if touch {
        if primary_resp
            .on_hover_text("Brush color: tap to show/hide the colour panel")
            .clicked()
        {
            app.workspace.show_color = !app.workspace.show_color;
        }
    } else {
        primary_resp.on_hover_text("Brush color");
    }
    let swap_clicked = swap_resp.on_hover_text("Swap colors (X)").clicked();
    let secondary_clicked = secondary_resp
        .on_hover_text("Secondary color: click to swap (X)")
        .clicked();
    if swap_clicked || secondary_clicked {
        app.swap_colors();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Frames of a grouped button; returns its rect and what was picked.
    struct Harness {
        ctx: egui::Context,
        t: f64,
    }

    impl Harness {
        fn frame(&mut self, events: Vec<egui::Event>) -> (egui::Rect, Option<usize>) {
            self.t += 0.05;
            let (mut rect, mut picked) = (egui::Rect::NOTHING, None);
            let _ = self.ctx.run(
                egui::RawInput {
                    events,
                    time: Some(self.t),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let members = [
                            Member::new(Icon::Transform, "Transform", "V", true),
                            Member::new(Icon::Motion, "Animate", "A", false),
                        ];
                        let top = ui.cursor().min;
                        if let Some(Pick::Tool(i)) = group_button(ui, "t", &members, 0, 34.0, false)
                        {
                            picked = Some(i);
                        }
                        rect = egui::Rect::from_min_size(top, egui::Vec2::splat(34.0));
                    });
                },
            );
            (rect, picked)
        }

        /// Moved there and pressed at once: no tooltip shows up under the
        /// pointer first (egui's tooltips take the pointer).
        fn click(&mut self, at: egui::Pos2) -> Option<usize> {
            let a = self
                .frame(vec![egui::Event::PointerMoved(at), button(at, true)])
                .1;
            let b = self.frame(vec![button(at, false)]).1;
            a.or(b)
        }
    }

    #[test]
    fn a_double_click_opens_the_group_and_picks_from_it() {
        let mut h = Harness {
            ctx: egui::Context::default(),
            t: 0.0,
        };
        let (rect, _) = h.frame(vec![]);
        assert_eq!(
            h.click(rect.center()),
            Some(0),
            "a click picks the one shown"
        );
        h.t += 1.0;
        h.click(rect.center());
        h.click(rect.center());
        // The flyout sits to the button's right; its second row (Animate)
        // under the first, inside the popup's margin.
        let margin = h.ctx.style().spacing.menu_margin.top;
        let row = rect.right_top() + egui::vec2(6.0 + 40.0, margin + 26.0 + 2.0 + 13.0);
        // (Its first frame only sizes it.)
        h.frame(vec![]);
        assert_eq!(h.click(row), Some(1), "picked from the flyout");
        // Picked: closed, a click there picks nothing.
        assert_eq!(h.click(row), None);
    }
}
