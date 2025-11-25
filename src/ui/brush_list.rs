use crate::brush_engine::brush::{Brush, BrushPreset};
use eframe::egui;
use egui::Color32;

/// Displays available presets and lets the user apply one to the active brush.
pub fn brush_list_window(ctx: &egui::Context, brush: &mut Brush, presets: &Vec<BrushPreset>) {
    egui::Window::new("Brush Presets")
        .default_width(400.0)
        .show(ctx, |ui| {
            ui.heading("Presets");
            ui.separator();

            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    for preset in presets {
                        ui.vertical(|ui| {
                            // Create a small preview image
                            let preview_size = 32.0;
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(preview_size, preview_size),
                                egui::Sense::click(),
                            );

                            // Draw a simple circle preview
                            let center = rect.center();
                            let radius = (preset.brush.diameter * 0.5).min(preview_size * 0.4);
                            ui.painter().circle_filled(
                                center,
                                radius,
                                preset.brush.color.to_color32(),
                            );

                            // Border if selected (but we don't have selection state)
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, Color32::GRAY),
                            );

                            // Name below
                            ui.label(&preset.name);

                            if response.clicked() {
                                // Keep the current color, but copy other properties
                                let current_color = brush.color;
                                *brush = preset.brush.clone();
                                brush.color = current_color;
                            }
                        });
                        ui.add_space(10.0);
                    }
                });
            });
        });
}
