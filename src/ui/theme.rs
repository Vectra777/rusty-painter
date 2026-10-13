//! Applies the tokens from [`crate::ui::style`] to egui and the dock: the
//! one place the app theme is built (desktop or touch).

use crate::ui::style::*;
use eframe::egui::{self, Color32, FontId, Margin, Rounding, Shadow, Stroke, TextStyle};

/// Apply the app theme: rounded widgets and popups in square regions,
/// neutral grays, a single accent and compact spacing, so the UI stays
/// quiet and dense around the canvas.
pub fn apply_global_style(ctx: &egui::Context) {
    apply_style(ctx, false);
}

/// Build the theme for desktop or touch (larger text and hit targets,
/// always-visible scroll bars).
pub fn apply_style(ctx: &egui::Context, touch: bool) {
    // Ctrl +/-/0 zoom the canvas; don't let egui also zoom the whole UI.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);

    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = BG_INSET;
    visuals.faint_bg_color = BG_RAISED;
    visuals.code_bg_color = BG_INSET;
    visuals.window_stroke = Stroke::new(1.0_f32, BORDER_LIGHT);
    visuals.window_rounding = Rounding::same(RADIUS_WINDOW);
    visuals.menu_rounding = Rounding::same(RADIUS_WINDOW);
    visuals.window_highlight_topmost = false;
    visuals.hyperlink_color = accent();
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Circle;
    visuals.indent_has_left_vline = false;
    visuals.collapsing_header_frame = false;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    visuals.override_text_color = None;

    let widget = |bg: Color32, stroke: Color32, fg: Color32| egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0_f32, stroke),
        rounding: Rounding::same(RADIUS_WIDGET),
        fg_stroke: Stroke::new(1.0_f32, fg),
        expansion: 0.0,
    };
    visuals.widgets.noninteractive = widget(BG_PANEL, BORDER, TEXT);
    visuals.widgets.inactive = widget(WIDGET, WIDGET, TEXT);
    visuals.widgets.hovered = widget(WIDGET_HOVER, BORDER_LIGHT, TEXT_STRONG);
    visuals.widgets.active = widget(WIDGET_ACTIVE, accent(), TEXT_STRONG);
    visuals.widgets.open = widget(WIDGET_HOVER, BORDER_LIGHT, TEXT_STRONG);

    visuals.selection.bg_fill = accent();
    visuals.selection.stroke = Stroke::new(1.0_f32, TEXT_STRONG);

    let shadow = Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(120),
    };
    visuals.popup_shadow = shadow;
    visuals.window_shadow = shadow;

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(14.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 5.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size = egui::vec2(36.0, 22.0);
    style.spacing.slider_rail_height = 4.0;
    style.spacing.window_margin = Margin::same(10.0);
    style.spacing.menu_margin = Margin::same(4.0);
    style.spacing.indent = 12.0;
    style.spacing.icon_width = 13.0;
    style.spacing.icon_width_inner = 7.0;
    // A bar beside the content, not floating over it: a floating bar hides
    // the right end of whatever is under it (sliders, number boxes).
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 6.0,
        bar_inner_margin: 2.0,
        ..egui::style::ScrollStyle::solid()
    };
    style.interaction.selectable_labels = false;
    style.animation_time = 0.12;

    if touch {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(17.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Small, FontId::proportional(12.5)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size = egui::vec2(44.0, 34.0);
        style.spacing.slider_rail_height = 6.0;
        style.spacing.icon_width = 20.0;
        style.spacing.icon_width_inner = 11.0;
        style.spacing.menu_margin = Margin::same(6.0);
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.spacing.scroll.bar_width = 10.0;
        // No hover on touch screens: show tooltips only on long press.
        style.interaction.tooltip_delay = 0.6;
    }

    ctx.set_style(style);
}
