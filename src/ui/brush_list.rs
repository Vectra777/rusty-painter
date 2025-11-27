use crate::brush_engine::brush::{Brush, BrushPreset};
use eframe::{egui, epaint::color};
use egui::Color32;

/// Displays available presets and lets the user apply one to the active brush.
pub fn brush_list_window(ctx: &egui::Context, brush: &mut Brush, presets: &Vec<BrushPreset>) {
    egui::Window::new("Brushes")
    .default_width(32.+3.+2.)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.columns(3, |col| {
                    let mut idx = 0;
                    for preset in presets {
                        // borrow the specific column once as a mutable reference
                        let column = &mut col[idx];
                        column.vertical(|ui| {
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
                                Color32::WHITE,
                            );

                            // Border if selected (but we don't have selection state)
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, Color32::GRAY),
                            );

                            let response = response.on_hover_text(&preset.name);
                            if response.clicked() {
                                // Keep the current color, but copy other properties
                                let current_color = brush.color;
                                *brush = preset.brush.clone();
                                brush.color = current_color;
                            }
                        });
                        column.add_space(1.0);
                        idx += 1;
                        idx = idx % 3;
                    }
                });
            });
        });
}
