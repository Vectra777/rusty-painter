//! Small reusable widgets that give every panel the same flat, square look:
//! segmented controls, labeled property rows, section headers, icon buttons
//! and color swatches.

use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use eframe::egui::{self, Color32, Rect, Response, RichText, Sense, Stroke};

/// Paints a transparency checkerboard into `rect`.
pub(crate) fn draw_checkerboard(painter: &egui::Painter, rect: Rect, cell: f32) {
    painter.rect_filled(rect, 0.0, CHECKERBOARD_LIGHT);
    let rows = ((rect.height() / cell).ceil() as i32).max(1);
    let cols = ((rect.width() / cell).ceil() as i32).max(1);
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                let min = egui::pos2(rect.left() + x as f32 * cell, rect.top() + y as f32 * cell);
                let max = (min + egui::vec2(cell, cell)).min(rect.max);
                painter.rect_filled(Rect::from_min_max(min, max), 0.0, CHECKERBOARD_DARK);
            }
        }
    }
}

/// Paints a color over a checkerboard with a hairline border.
pub(crate) fn paint_swatch(painter: &egui::Painter, rect: Rect, color: Color32) {
    if color.a() < 255 {
        draw_checkerboard(painter, rect, (rect.height() / 3.0).clamp(3.0, 8.0));
    }
    painter.rect_filled(rect, 0.0, color);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, BORDER));
}

/// A clickable color swatch of the given size.
pub(crate) fn color_swatch(ui: &mut egui::Ui, color: Color32, size: egui::Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    paint_swatch(ui.painter(), rect, color);
    if response.hovered() {
        ui.painter()
            .rect_stroke(rect.expand(1.0), 0.0, Stroke::new(1.0_f32, TEXT_STRONG));
    }
    response
}

/// Square button showing an icon. `selected` fills it with the accent.
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    selected: bool,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    let (bg, fg) = if selected {
        (ACCENT, TEXT_STRONG)
    } else if response.is_pointer_button_down_on() {
        (WIDGET_ACTIVE, TEXT_STRONG)
    } else if response.hovered() {
        (WIDGET_HOVER, TEXT_STRONG)
    } else {
        (Color32::TRANSPARENT, TEXT)
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    let inset = (size * 0.24).round();
    paint_icon(ui.painter(), rect.shrink(inset), icon, fg);
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Small borderless icon toggle used in list rows (visibility, lock).
pub(crate) fn icon_toggle(
    ui: &mut egui::Ui,
    on: &mut bool,
    icon_on: Icon,
    icon_off: Icon,
    tooltip: &str,
) -> bool {
    let size = metrics(ui.ctx()).row_toggle;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, WIDGET_HOVER);
    }
    let (icon, color) = if *on {
        (icon_on, TEXT)
    } else {
        (icon_off, TEXT_DIM)
    };
    paint_icon(ui.painter(), rect.shrink(size * 0.16), icon, color);
    let clicked = response.on_hover_text(tooltip).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// Joined row of mutually exclusive options. Fills the available width
/// with equal segments unless `compact`, where each segment fits its label.
pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
    compact: bool,
) -> bool {
    let mut changed = false;
    let height = ui.spacing().interact_size.y;
    let font = egui::TextStyle::Button.resolve(ui.style());
    let pad = ui.spacing().button_padding.x * 1.5;
    let equal_width = (ui.available_width() / options.len().max(1) as f32).floor();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        for (option, label) in options {
            let galley = ui
                .painter()
                .layout_no_wrap(label.to_string(), font.clone(), TEXT);
            let width = if compact {
                galley.size().x + pad * 2.0
            } else {
                (equal_width - 1.0).max(galley.size().x + 8.0)
            };
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
            let selected = *value == *option;
            let (bg, fg) = if selected {
                (ACCENT, TEXT_STRONG)
            } else if response.hovered() {
                (WIDGET_HOVER, TEXT_STRONG)
            } else {
                (WIDGET, TEXT)
            };
            ui.painter().rect_filled(rect, 0.0, bg);
            ui.painter().galley(
                rect.center() - galley.size() * 0.5,
                ui.painter()
                    .layout_no_wrap(label.to_string(), font.clone(), fg),
                fg,
            );
            if response.clicked() && !selected {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

/// Dim fixed-width label that starts a property row.
fn row_label(ui: &mut egui::Ui, label: &str) {
    let height = ui.spacing().interact_size.y;
    let width = metrics(ui.ctx()).label_width;
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.label(RichText::new(label).color(TEXT_DIM));
        },
    );
}

/// `label  [slider ----------] [value]` on one line, the slider filling
/// the space left by the label and value box.
pub(crate) fn slider_row(ui: &mut egui::Ui, label: &str, slider: egui::Slider) -> Response {
    ui.horizontal(|ui| {
        row_label(ui, label);
        let spacing = ui.spacing().item_spacing.x;
        let value_box = metrics(ui.ctx()).value_box_width;
        ui.spacing_mut().slider_width = (ui.available_width() - value_box - spacing).max(40.0);
        ui.add(slider)
    })
    .inner
}

/// `label  [contents]` on one line.
pub(crate) fn property_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        row_label(ui, label);
        add_contents(ui)
    })
    .inner
}

/// Formats a 0..=1 value as a whole percentage and parses it back.
pub(crate) fn percent_of_unit(slider: egui::Slider) -> egui::Slider {
    slider
        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
        .custom_parser(|s| {
            s.trim()
                .trim_end_matches('%')
                .trim()
                .parse::<f64>()
                .ok()
                .map(|v| v / 100.0)
        })
}

/// Collapsible section: a hairline, a small uppercase title with a
/// disclosure triangle, and an unindented body.
pub(crate) fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let id = ui.id().with(("section", title));
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(default_open);

    ui.add_space(4.0);
    let header_height = if metrics(ui.ctx()).touch { 34.0 } else { 22.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), header_height),
        Sense::click(),
    );
    let painter = ui.painter();
    painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0_f32, BORDER));
    let color = if response.hovered() { TEXT } else { TEXT_DIM };
    let c = egui::pos2(rect.left() + 4.0, rect.center().y + 1.0);
    let tri = if open {
        vec![
            c + egui::vec2(-3.5, -2.0),
            c + egui::vec2(3.5, -2.0),
            c + egui::vec2(0.0, 2.5),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -3.5),
            c + egui::vec2(2.5, 0.0),
            c + egui::vec2(-2.0, 3.5),
        ]
    };
    painter.add(egui::Shape::convex_polygon(tri, color, Stroke::NONE));
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y + 1.0),
        egui::Align2::LEFT_CENTER,
        title.to_uppercase(),
        egui::FontId::proportional(11.0),
        color,
    );
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }

    open.then(|| add_contents(ui))
}

/// Thin vertical divider for horizontal bars.
pub(crate) fn vdivider(ui: &mut egui::Ui) {
    let height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, height), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range().shrink(2.0),
        Stroke::new(1.0_f32, BORDER_LIGHT),
    );
}

/// A menu sliding out from a toolbar button (`anchor`) while `*open`;
/// scrolls when taller than the screen, and closes on a click elsewhere.
pub(crate) fn flyout(
    ctx: &egui::Context,
    id: &str,
    open: &mut bool,
    anchor: Rect,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let t = ctx.animate_bool_with_time(egui::Id::new((id, "anim")), *open, 0.12);
    if t <= 0.0 {
        return;
    }
    let x = anchor.right() + 4.0 - (1.0 - t) * width;
    let screen = ctx.screen_rect();
    // Low buttons open upward enough to fit.
    let top = anchor
        .top()
        .min((screen.bottom() - 260.0).max(screen.top()));
    let response = egui::Area::new(egui::Id::new(id))
        .fixed_pos(egui::pos2(x, top))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    egui::ScrollArea::vertical()
                        .max_height((screen.bottom() - top - 24.0).max(120.0))
                        .show(ui, add_contents);
                });
        })
        .response;
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !response.rect.contains(p) && !anchor.contains(p));
    if *open && clicked_outside {
        *open = false;
    }
}
