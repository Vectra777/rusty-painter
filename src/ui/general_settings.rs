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
    ui.label(
        egui::RichText::new("DOCUMENT")
            .small()
            .strong()
            .color(crate::ui::style::TEXT_DIM),
    );
    ui.horizontal(|ui| {
        ui.label("Color blending");
        let mut space = app.canvas.blend_space;
        if crate::ui::canvas_creation::blend_space_picker(ui, &mut space) {
            app.canvas_mut().blend_space = space;
            app.mark_all_tiles_dirty();
        }
    });
    ui.label(
        egui::RichText::new(crate::ui::canvas_creation::blend_space_hint(
            app.canvas.blend_space,
        ))
        .small()
        .color(crate::ui::style::TEXT_DIM),
    );
    ui.separator();
    touch_and_pen_settings(app, ui);
    ui.separator();
    ui.horizontal(|ui| {
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
    });
}

/// Touch-screen and pen pressure options.
fn touch_and_pen_settings(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ws = &mut app.workspace;
    ui.label(
        egui::RichText::new("TOUCH & PEN")
            .small()
            .strong()
            .color(crate::ui::style::TEXT_DIM),
    );
    ui.checkbox(
        &mut ws.touch_mode,
        "Touch mode (larger controls, canvas faders)",
    );
    ui.checkbox(&mut ws.finger_painting, "Paint with one finger")
        .on_hover_text("When off, only a stylus paints and one finger pans the canvas.");
    ui.add(
        egui::Slider::new(&mut ws.pressure_curve, 0.3..=3.0)
            .logarithmic(true)
            .text("Pen pressure curve"),
    )
    .on_hover_text(
        "Below 1: light touches count more (soft pen). Above 1: needs more force (firm pen).",
    );
    ui.label(
        egui::RichText::new(
            "What pressure controls (size, opacity, flow) is set per brush in the Brush panel.",
        )
        .small()
        .color(crate::ui::style::TEXT_DIM),
    );
}

/// Modal window that captures focus for general settings.
pub fn general_settings_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_general_settings {
        return;
    }

    let mut open = app.modal_state.show_general_settings;
    egui::Window::new("Settings")
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            general_settings_panel(app, ui);
        });
    app.modal_state.show_general_settings = open;
}

/// Every shortcut, grouped, for Help → Keyboard Shortcuts.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Tools",
        &[
            ("B", "Brush"),
            ("E", "Eraser"),
            ("M", "Rectangle / ellipse select"),
            ("L", "Lasso select (again: magnetic lasso)"),
            ("Q", "Magic wand (again: colour range)"),
            ("V or T", "Transform"),
            ("I", "Eyedropper"),
            ("G", "Fill (again: bucket / enclose)"),
            ("W", "Liquify"),
            ("S", "Smudge (again: blur)"),
            ("Alt + click", "Pick color while painting"),
        ],
    ),
    (
        "Brush & color",
        &[
            ("[  /  ]", "Smaller / larger brush"),
            ("P", "Brush presets window"),
            ("X", "Swap brush and secondary color"),
        ],
    ),
    (
        "View",
        &[
            ("Space + drag, right drag", "Pan"),
            ("Middle drag", "Rotate"),
            ("Mouse wheel", "Zoom at cursor"),
            ("Two fingers", "Pan, pinch to zoom, twist to rotate"),
            ("Tab", "Show / hide panels"),
            ("Ctrl + = / Ctrl + -", "Zoom in / out"),
            ("Ctrl + 0", "Fit to window"),
            ("Ctrl + 1", "Actual pixels"),
            ("H", "Flip the view horizontally"),
        ],
    ),
    (
        "Edit",
        &[
            ("Ctrl + Z", "Undo"),
            ("Ctrl + Shift + Z, Ctrl + Y", "Redo"),
            ("Ctrl + Shift + N", "New layer"),
            ("Ctrl + G", "New folder"),
            ("Two-finger tap", "Undo"),
            ("Three-finger tap", "Redo"),
            ("Ctrl + D, Esc", "Deselect"),
            ("Ctrl + A", "Select all"),
            ("Ctrl + Shift + I", "Invert selection"),
            ("Shift / Alt + drag", "Add to / erase from the selection"),
            ("Backspace", "Remove the last magnetic lasso point"),
            ("Enter, double-click", "Close the magnetic lasso"),
            ("Enter / Esc", "Apply / cancel transform"),
            ("Shift + drag corner", "Scale proportionally"),
            ("/", "Lock layer transparency"),
        ],
    ),
    (
        "File",
        &[
            ("Ctrl + N", "New canvas"),
            ("Ctrl + Shift + O, drop a file", "Import image as a layer"),
            ("Ctrl + O", "Open project"),
            ("Ctrl + S", "Save project"),
            ("Ctrl + E", "Export image"),
        ],
    ),
];

/// Help window listing every keyboard shortcut.
pub fn shortcuts_window(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_shortcuts {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard Shortcuts")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            for (group, entries) in SHORTCUTS {
                ui.label(
                    egui::RichText::new(group.to_uppercase())
                        .small()
                        .strong()
                        .color(crate::ui::style::TEXT_DIM),
                );
                egui::Grid::new(group)
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        for (keys, action) in *entries {
                            ui.label(egui::RichText::new(*keys).monospace());
                            ui.label(*action);
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
            }
        });
    app.modal_state.show_shortcuts = open;
}
