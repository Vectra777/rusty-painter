use crate::PainterApp;
use eframe::egui;
use rayon::ThreadPoolBuilder;

/// Panel with app-wide toggles that affect rendering performance and controls.
pub fn general_settings_panel(app: &mut PainterApp, ui: &mut egui::Ui) {
    ui.checkbox(&mut app.use_masked_brush, "Use masked brush (fast)");
    ui.checkbox(&mut app.disable_lod, "High quality zoom out (slower)");
    let threads_changed = ui
        .add(egui::Slider::new(&mut app.thread_count, 1..=app.max_threads).text("Brush threads"))
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
    
    ui.separator();
    if ui.button("Open Brush Folder").clicked() {
        let _ = app.brushes_path.canonicalize().map(|path| {
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
    if !app.show_general_settings {
        return;
    }

    let mut open = app.show_general_settings;
    egui::Window::new("General Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            general_settings_panel(app, ui);
        });
    app.show_general_settings = open;
}
