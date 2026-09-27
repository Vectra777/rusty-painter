//! The Settings dialog (brush threads, colour blending, the brushes
//! folder, finger painting, pen pressure curve) and the keyboard
//! shortcuts window.

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
        .add(crate::ui::widgets::reset(
            &mut app.workspace.thread_count,
            |v| egui::Slider::new(v, 1..=app.workspace.max_threads).text("Brush threads"),
        ))
        .changed();
    if threads_changed
        && let Ok(pool) = ThreadPoolBuilder::new()
            .num_threads(app.workspace.thread_count)
            .build()
    {
        app.workspace.pool = std::sync::Arc::new(pool);
    }
    keyboard_layout_row(app, ui);
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
    ui.label("Pen pressure").on_hover_text(
        "How hard you press (across) to the pressure every brush gets (up). \
             Bowed up (Firm): light touches count more. Bowed down (Soft): needs \
             more force.",
    );
    crate::ui::curve_editor::curve_editor_with(
        ui,
        &mut ws.pressure_curve,
        &crate::ui::curve_editor::PRESSURE_PRESETS,
    );
    // Live readout: press with the pen to see what it sends.
    let (raw, mapped) = ws.last_pressure.unwrap_or((0.0, 0.0));
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Pen")
                .small()
                .color(crate::ui::style::TEXT_DIM),
        );
        ui.add(
            egui::ProgressBar::new(raw)
                .desired_width(90.0)
                .text(format!("{:.0}%", raw * 100.0)),
        );
        ui.label(
            egui::RichText::new("→ brush")
                .small()
                .color(crate::ui::style::TEXT_DIM),
        );
        ui.add(
            egui::ProgressBar::new(mapped)
                .desired_width(90.0)
                .text(format!("{:.0}%", mapped * 100.0)),
        );
    });
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

/// Which keyboard the shortcuts are placed and labelled for.
fn keyboard_layout_row(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::app::input::keyboard::KeyboardLayout;
    let keyboard = &mut app.workspace.keyboard;
    let auto = format!("Automatic ({})", keyboard.detected().label());
    ui.horizontal(|ui| {
        ui.label("Keyboard");
        egui::ComboBox::from_id_salt("keyboard_layout")
            .selected_text(
                keyboard
                    .choice
                    .map_or(auto.clone(), |l| l.label().to_string()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut keyboard.choice, None, auto);
                for layout in KeyboardLayout::ALL {
                    ui.selectable_value(&mut keyboard.choice, Some(layout), layout.label());
                }
            })
            .response
            .on_hover_text(
                "Letter shortcuts follow the letter on the key; digit and symbol ones \
                 (brush size, fit, zoom) follow the key's place, and the menus show \
                 your keyboard's own keys.",
            );
    });
}

/// A titled group of `(keys, action)` rows.
type ShortcutGroup = (&'static str, &'static [(&'static str, &'static str)]);

/// Every shortcut, grouped, for Help → Keyboard Shortcuts.
const SHORTCUTS: &[ShortcutGroup] = &[
    (
        "Tools",
        &[
            ("B", "Brush"),
            ("E", "Eraser"),
            ("M", "Rectangle / ellipse select"),
            ("L", "Lasso select (again: magnetic lasso)"),
            ("Q", "Magic wand (again: colour range)"),
            ("U", "Shapes (again: next shape)"),
            ("Shift + G", "Gradient"),
            ("R", "Show / hide the ruler"),
            ("V or T", "Transform"),
            ("I", "Eyedropper"),
            ("G", "Fill (again: bucket / enclose / lasso delete)"),
            ("W", "Liquify"),
            ("S", "Smudge (again: blur)"),
            ("Alt + click", "Pick color while painting"),
        ],
    ),
    (
        "Brush & color",
        &[
            ("{[}  /  {]}", "Smaller / larger brush"),
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
            ("Ctrl + {=} / Ctrl + {-}", "Zoom in / out"),
            ("Ctrl + {0}", "Fit to window"),
            ("Ctrl + {1}", "Actual pixels"),
            ("H", "Flip the view horizontally"),
        ],
    ),
    (
        "Edit",
        &[
            ("Ctrl + Z", "Undo"),
            ("Ctrl + Shift + Z, Ctrl + Y", "Redo"),
            ("Ctrl + X / C / V", "Cut / copy / paste (as a new layer)"),
            ("Ctrl + Shift + C", "Copy everything visible (merged)"),
            ("Ctrl + Shift + N", "New layer"),
            ("Ctrl + J", "Duplicate layer"),
            ("Ctrl + G", "New folder"),
            ("Double-click a layer", "Rename it (folders too)"),
            ("Two-finger tap", "Undo"),
            ("Three-finger tap", "Redo"),
            ("{/}", "Lock layer transparency"),
        ],
    ),
    (
        "Selection & transform",
        &[
            ("Ctrl + A", "Select all"),
            ("Ctrl + D, Esc", "Deselect"),
            ("Ctrl + Shift + I", "Invert selection"),
            ("Delete, Backspace", "Delete the selected pixels"),
            ("Shift + F5", "Fill the selection from its surroundings"),
            ("Shift / Alt + drag", "Add to / erase from the selection"),
            ("Backspace", "Remove the last magnetic lasso point"),
            (
                "Enter, double-click",
                "Close the magnetic lasso, finish a polygon",
            ),
            (
                "Shift / Alt + drag shape",
                "Keep proportions / draw from the centre",
            ),
            ("Enter / Esc", "Apply / cancel transform"),
            ("Shift + drag corner", "Scale proportionally"),
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

/// Width of one column of shortcuts.
const SHORTCUT_COLUMN_WIDTH: f32 = 330.0;

/// Help window listing every keyboard shortcut: the groups in as many
/// columns as the screen fits (up to three), scrolling if still too tall.
pub fn shortcuts_window(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_shortcuts {
        return;
    }
    let screen = ctx.screen_rect();
    let columns = ((screen.width() - 48.0) / SHORTCUT_COLUMN_WIDTH)
        .floor()
        .clamp(1.0, 3.0) as usize;
    let groups = shortcut_columns(columns);
    let mut open = true;
    egui::Window::new("Keyboard Shortcuts")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height((screen.height() - 120.0).max(200.0))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for (c, column) in groups.iter().enumerate() {
                            if c > 0 {
                                ui.separator();
                            }
                            ui.vertical(|ui| {
                                ui.set_width(SHORTCUT_COLUMN_WIDTH - 24.0);
                                for &(group, entries) in column {
                                    shortcut_group(ui, group, entries);
                                }
                            });
                        }
                    });
                });
        });
    app.modal_state.show_shortcuts = open;
}

/// The groups split into `n` columns of about the same height, in order.
fn shortcut_columns(n: usize) -> Vec<Vec<ShortcutGroup>> {
    // A group's header counts as about two rows.
    let rows = |entries: &[(&str, &str)]| entries.len() + 2;
    let total: usize = SHORTCUTS.iter().map(|(_, e)| rows(e)).sum();
    let target = total.div_ceil(n.max(1));
    let mut columns = vec![Vec::new()];
    let mut height = 0;
    for &(group, entries) in SHORTCUTS {
        let h = rows(entries);
        // Start a new column when this group would overshoot more than
        // stopping short does.
        if height > 0
            && columns.len() < n
            && height + h > target
            && height + h - target > target.saturating_sub(height)
        {
            columns.push(Vec::new());
            height = 0;
        }
        columns
            .last_mut()
            .expect("one column")
            .push((group, entries));
        height += h;
    }
    columns
}

fn shortcut_group(ui: &mut egui::Ui, group: &str, entries: &[(&str, &str)]) {
    ui.label(
        egui::RichText::new(group.to_uppercase())
            .small()
            .strong()
            .color(crate::ui::style::TEXT_DIM),
    );
    egui::Grid::new(group)
        .num_columns(2)
        .spacing([16.0, 3.0])
        .show(ui, |ui| {
            for (keys, action) in entries {
                let keys = crate::app::input::keyboard::with_keycaps(ui.ctx(), keys);
                ui.label(egui::RichText::new(keys).monospace());
                ui.add(egui::Label::new(*action).wrap());
                ui.end_row();
            }
        });
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_columns_keep_every_group_in_order_and_balance() {
        for n in 1..=3 {
            let columns = shortcut_columns(n);
            assert!(columns.len() <= n);
            let flat: Vec<&str> = columns.iter().flatten().map(|g| g.0).collect();
            let all: Vec<&str> = SHORTCUTS.iter().map(|g| g.0).collect();
            assert_eq!(flat, all, "{n} columns");
            let heights: Vec<usize> = columns
                .iter()
                .map(|c| c.iter().map(|g| g.1.len() + 2).sum())
                .collect();
            let (lo, hi) = (heights.iter().min().unwrap(), heights.iter().max().unwrap());
            assert!(hi - lo <= 12, "{n} columns: {heights:?}");
        }
    }
}
