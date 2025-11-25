use crate::brush_engine::brush::{BlendMode, Brush, BrushType};
use eframe::egui;

/// Panel for tweaking the currently selected brush properties.
pub fn brush_settings_window(ctx: &egui::Context, brush: &mut Brush) {
    egui::Window::new("Brush Settings")
        .default_width(200.0)
        .show(ctx, |ui| {
            ui.heading("Brush Properties");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Type:");
                ui.selectable_value(&mut brush.brush_type, BrushType::Soft, "Soft");
                ui.selectable_value(&mut brush.brush_type, BrushType::Pixel, "Pixel");
            });

            ui.horizontal(|ui| {
                ui.label("Mode:");
                ui.selectable_value(&mut brush.blend_mode, BlendMode::Normal, "Normal");
                ui.selectable_value(&mut brush.blend_mode, BlendMode::Eraser, "Eraser");
            });

            ui.add_space(5.0);

            ui.label("Size:");
            ui.add(egui::Slider::new(&mut brush.diameter, 1.0..=300.0).logarithmic(true));

            if brush.brush_type == BrushType::Soft {
                ui.label("Hardness:");
                ui.add(egui::Slider::new(&mut brush.hardness, 0.0..=100.0));
            }

            ui.label("Opacity:");
            ui.add(egui::Slider::new(&mut brush.opacity, 0.0..=1.0));

            ui.label("Flow:");
            ui.add(egui::Slider::new(&mut brush.flow, 0.0..=100.0));

            ui.label("Spacing (%):");
            ui.add(egui::Slider::new(&mut brush.spacing, 1.0..=200.0));

            ui.label("Jitter:");
            ui.add(egui::Slider::new(&mut brush.jitter, 0.0..=50.0));

            ui.label("Stabilizer:");
            ui.add(egui::Slider::new(&mut brush.stabilizer, 0.0..=1.0));

            ui.separator();
            ui.checkbox(&mut brush.pixel_perfect, "Pixel Perfect Mode");
            ui.checkbox(&mut brush.anti_alias, "Anti-Aliasing");
        });
}
