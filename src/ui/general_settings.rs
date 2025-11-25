use eframe::egui;
use rayon::ThreadPoolBuilder;
use crate::PainterApp;


/// Window with app-wide toggles that affect rendering performance and controls.
pub fn general_settings_ui(app: &mut PainterApp, ctx: &egui::Context) {
    egui::Window::new("General Settings").show(ctx, |ui| {
        ui.checkbox(&mut app.use_masked_brush, "Use masked brush (fast)");
        ui.checkbox(&mut app.disable_lod, "High quality zoom out (slower)");
        // self.brush.set_masked(self.use_masked_brush);
        let threads_changed = ui
            .add(
                egui::Slider::new(&mut app.thread_count, 1..=app.max_threads)
                    .text("Brush threads"),
            )
            .changed();
        if threads_changed {
            if let Ok(pool) = ThreadPoolBuilder::new()
                .num_threads(app.thread_count)
                .build()
            {
                app.pool = pool;
            }
        }
        ui.separator();
        ui.label("Controls:");
        ui.label("Left click: Paint");
        ui.label("C: Clear Canvas");
    });
}
