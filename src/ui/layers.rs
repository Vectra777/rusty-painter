use crate::canvas::canvas::Canvas;
use eframe::egui;

/// Sidebar that manages the canvas layer stack.
pub fn layers_window(ctx: &egui::Context, canvas: &mut Canvas) {
    egui::Window::new("Layers")
        .default_width(200.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("New Layer").clicked() {
                    canvas.add_layer();
                }
            });
            ui.separator();

            // Iterate in reverse so top layers are at the top of the list
            let mut to_delete = None;
            let mut active_idx = canvas.active_layer_idx;

            for i in (0..canvas.layers.len()).rev() {
                ui.horizontal(|ui| {
                    let layer = &mut canvas.layers[i];
                    ui.checkbox(&mut layer.visible, "");

                    let is_active = i == active_idx;
                    if ui.selectable_label(is_active, &layer.name).clicked() {
                        active_idx = i;
                    }

                    // Opacity slider
                    ui.add(egui::Slider::new(&mut layer.opacity, 0.0..=1.0).show_value(false));

                    if canvas.layers.len() > 1 {
                        if ui.button("X").clicked() {
                            to_delete = Some(i);
                        }
                    }
                });
            }

            canvas.active_layer_idx = active_idx;

            if let Some(idx) = to_delete {
                canvas.layers.remove(idx);
                if canvas.active_layer_idx >= canvas.layers.len() {
                    canvas.active_layer_idx = canvas.layers.len().saturating_sub(1);
                }
            }
        });
}
