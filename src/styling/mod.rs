use eframe::egui;

/// Apply a cohesive, modern dark theme with sharp accents and generous spacing.
pub fn apply_global_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(14, 16, 22);
    visuals.window_fill = egui::Color32::from_rgb(18, 21, 30);
    visuals.extreme_bg_color = egui::Color32::from_rgb(26, 30, 40);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(30, 34, 46);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(42, 64, 102);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(68, 118, 190);
    visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(220, 225, 235);
    visuals.widgets.hovered.fg_stroke.color = egui::Color32::from_rgb(240, 244, 255);
    visuals.selection.bg_fill = egui::Color32::from_rgb(90, 165, 255);
    visuals.selection.stroke.color = egui::Color32::from_rgb(255, 255, 255);
    visuals.window_rounding = egui::Rounding::same(12.0);
    visuals.widgets.inactive.rounding = egui::Rounding::same(10.0);
    visuals.widgets.hovered.rounding = egui::Rounding::same(10.0);
    visuals.widgets.active.rounding = egui::Rounding::same(10.0);
    visuals.popup_shadow = egui::Shadow {
        offset: egui::vec2(0.0, 6.0),
        blur: 24.0,
        spread: 0.0,
        color: egui::Color32::from_rgba_premultiplied(0, 0, 0, 110),
    };
    visuals.window_shadow = egui::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 30.0,
        spread: 4.0,
        color: egui::Color32::from_rgba_premultiplied(0, 0, 0, 140),
    };

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.interaction.selectable_labels = true;

    ctx.set_style(style);
}
