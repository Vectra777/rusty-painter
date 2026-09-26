use crate::PainterApp;
use eframe::egui;
use rayon::ThreadPoolBuilder;

/// Panel with app-wide toggles that affect rendering performance and controls.
pub fn general_settings_panel(app: &mut PainterApp, ui: &mut egui::Ui) {
    ui.checkbox(
        &mut app.brush_state.use_masked_brush,
        "Use masked brush (fast)",
    );
    let threads_changed = ui
        .add(
            egui::Slider::new(
                &mut app.workspace.thread_count,
                1..=app.workspace.max_threads,
            )
            .text("Brush threads"),
        )
        .changed();
    if threads_changed
        && let Ok(pool) = ThreadPoolBuilder::new()
            .num_threads(app.workspace.thread_count)
            .build()
    {
        app.workspace.pool = std::sync::Arc::new(pool);
    }
    ui.separator();
    ui.label("Controls:");
    ui.label("Left click: Paint");
    ui.label("C: Clear Canvas");

    ui.separator();
    if ui.button("Open Brush Folder").clicked() {
        let _ = app.brush_state.brushes_path.canonicalize().map(|path| {
            #[cfg(target_os = "linux")]
            let _ = std::process::Command::new("xdg-open").arg(path).spawn();
            #[cfg(target_os = "windows")]
            let _ = std::process::Command::new("explorer").arg(path).spawn();
            #[cfg(target_os = "macos")]
            let _ = std::process::Command::new("open").arg(path).spawn();
        });
    }
    if ui.button("Refresh Brushes").clicked() {
        let ctx = ui.ctx().clone();
        app.load_brush_tips(ctx);
    }
}

/// Modal window that captures focus for general settings.
pub fn general_settings_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_general_settings {
        return;
    }

    let mut open = app.modal_state.show_general_settings;
    egui::Window::new("General Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            general_settings_panel(app, ui);
        });
    app.modal_state.show_general_settings = open;
}
