//! The Settings dialog (brush threads, colour blending, the brushes
//! folder, finger painting, pen pressure curve) and the keyboard
//! shortcuts window.

use crate::PainterApp;
use crate::app::input::keymap::{self, Action};
use crate::ui::bar_slider::BarSlider;
use crate::ui::widgets::FitScreen;
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
            |v| BarSlider::new(v, 1..=app.workspace.max_threads).text("Brush threads"),
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
    appearance_settings(app, ui);
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

/// The accent colour: a preset, or any colour picked.
fn appearance_settings(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::ui::style::{ACCENT_PRESETS, DEFAULT_ACCENT, RADIUS_SMALL, TEXT_DIM, TEXT_STRONG};
    ui.label(
        egui::RichText::new("APPEARANCE")
            .small()
            .strong()
            .color(TEXT_DIM),
    );
    let accent = &mut app.workspace.accent;
    ui.horizontal_wrapped(|ui| {
        ui.label("Accent");
        let side = ui.spacing().interact_size.y;
        for (name, color) in ACCENT_PRESETS {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            let painter = ui.painter();
            painter.rect_filled(rect.shrink(2.0), RADIUS_SMALL, color);
            if *accent == color {
                painter.rect_stroke(
                    rect,
                    RADIUS_SMALL + 2.0,
                    egui::Stroke::new(2.0_f32, TEXT_STRONG),
                );
            } else if response.hovered() {
                painter.rect_stroke(
                    rect,
                    RADIUS_SMALL + 2.0,
                    egui::Stroke::new(1.0_f32, TEXT_DIM),
                );
            }
            if response.on_hover_text(name).clicked() {
                *accent = color;
            }
        }
        let custom = !ACCENT_PRESETS.iter().any(|(_, c)| c == accent);
        let mut picked = *accent;
        let response = egui::color_picker::color_edit_button_srgba(
            ui,
            &mut picked,
            egui::color_picker::Alpha::Opaque,
        );
        if custom {
            ui.painter().rect_stroke(
                response.rect.expand(2.0),
                RADIUS_SMALL + 2.0,
                egui::Stroke::new(2.0_f32, TEXT_STRONG),
            );
        }
        if response.on_hover_text("Any colour").changed() {
            *accent = egui::Color32::from_rgb(picked.r(), picked.g(), picked.b());
        }
        if *accent != DEFAULT_ACCENT && ui.small_button("Reset").clicked() {
            *accent = DEFAULT_ACCENT;
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
        "Touch mode (larger controls, size and opacity on the top bar)",
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
    calibration_pad(ui, ws);
    ui.label(
        egui::RichText::new(
            "What pressure controls (size, opacity, flow) is set per brush in the Brush panel.",
        )
        .small()
        .color(crate::ui::style::TEXT_DIM),
    );
}

/// "Calibrate from test strokes": a pad to draw a few strokes on with the
/// pen, then the pressure curve fitted to them.
fn calibration_pad(ui: &mut egui::Ui, ws: &mut crate::app::state::WorkspaceState) {
    use crate::app::pressure_calibration::{MIN_SAMPLES, fit_pressure_curve};
    let Some(calibration) = &mut ws.calibration else {
        if ui
            .button("Calibrate from test strokes…")
            .on_hover_text("Draw a few strokes and the curve is fitted to your hand.")
            .clicked()
        {
            ws.calibration = Some(Default::default());
        }
        return;
    };
    ui.label("Draw a few strokes here with the pen, as you usually do, light to heavy:");
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(320.0), 110.0),
        egui::Sense::hover(),
    );
    calibration.pad = Some(rect);
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        crate::ui::style::RADIUS_WIDGET,
        ui.visuals().extreme_bg_color,
    );
    let ink = ui.visuals().strong_text_color();
    for stroke in &calibration.strokes {
        for w in stroke.windows(2) {
            let width = 0.5 + 5.0 * (w[0].1 + w[1].1) / 2.0;
            painter.line_segment([w[0].0, w[1].0], egui::Stroke::new(width, ink));
        }
    }
    // Ask again while drawing: the pad shows the strokes as they come.
    ui.ctx().request_repaint();
    let fitted = fit_pressure_curve(&calibration.samples);
    let count = calibration.samples.len();
    let (mut apply, mut clear, mut cancel) = (false, false, false);
    ui.horizontal(|ui| {
        apply = ui
            .add_enabled(fitted.is_some(), egui::Button::new("Apply"))
            .on_disabled_hover_text(format!(
                "Needs {MIN_SAMPLES} pen samples ({count} so far) with some range of pressure."
            ))
            .clicked();
        clear = ui.button("Clear").clicked();
        cancel = ui.button("Cancel").clicked();
    });
    if clear {
        *calibration = Default::default();
    }
    if let Some(curve) = fitted.filter(|_| apply) {
        ws.pressure_curve = curve;
    }
    if apply || cancel {
        ws.calibration = None;
    }
}

/// Modal window that captures focus for general settings.
pub fn general_settings_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_general_settings {
        return;
    }

    let mut open = app.modal_state.show_general_settings;
    egui::Window::new("Settings")
        .fit_screen(ctx)
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

/// Keys and gestures that aren't commands to rebind: `(keys, what)` rows
/// shown under each group of the shortcuts window.
const FIXED: &[(&str, &[(&str, &str)])] = &[
    ("Tools", &[("Alt + click", "Pick color while painting")]),
    (
        "Brush & colour",
        &[(
            "Right-click, pen side button",
            "Pop-up palette (right drag pans)",
        )],
    ),
    (
        "View",
        &[
            ("Space + drag, right drag", "Pan"),
            ("Middle drag", "Rotate"),
            ("Mouse wheel", "Zoom at cursor"),
            ("Two fingers", "Pan, pinch to zoom, twist to rotate"),
            (
                "Ctrl + drag a guide",
                "Move it (from beside the canvas: a new one)",
            ),
        ],
    ),
    (
        "Edit",
        &[
            ("Ctrl + X / C / V", "Cut / copy / paste (as a new layer)"),
            ("Ctrl + Shift + C", "Copy everything visible (merged)"),
            ("Double-click a layer", "Rename it (folders too)"),
            ("Two-finger tap", "Undo"),
            ("Three-finger tap", "Redo"),
        ],
    ),
    (
        "Selection",
        &[
            ("Esc", "Cancel what's in progress, else deselect"),
            ("Shift / Alt + drag", "Add to / erase from the selection"),
            ("Ctrl + click a thumbnail", "Select the layer's paint"),
            (
                "Ctrl + Shift / Alt + click",
                "Add it to / take it from the selection",
            ),
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
    ("File", &[("Drop a file", "Import image as a layer")]),
];

/// The groups, in order.
fn groups() -> Vec<&'static str> {
    let mut groups: Vec<&str> = Vec::new();
    for info in keymap::ACTIONS {
        if !groups.contains(&info.group) {
            groups.push(info.group);
        }
    }
    groups
}

fn fixed_rows(group: &str) -> &'static [(&'static str, &'static str)] {
    FIXED
        .iter()
        .find(|(g, _)| *g == group)
        .map_or(&[], |(_, rows)| rows)
}

/// How many rows a group takes (its header counts as about two).
fn group_rows(group: &str) -> usize {
    keymap::ACTIONS.iter().filter(|i| i.group == group).count() + fixed_rows(group).len() + 2
}

/// Width of one column of shortcuts, and of the names in it.
const SHORTCUT_COLUMN_WIDTH: f32 = 400.0;
const SHORTCUT_NAME_WIDTH: f32 = 170.0;

/// Help → Keyboard Shortcuts: every command and its keys, which a click
/// changes, and the gestures that can't be changed; the groups in as many
/// columns as the screen fits (up to three), scrolling if still too tall.
pub fn shortcuts_window(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_shortcuts {
        app.workspace.recording_shortcut = None;
        return;
    }
    record_shortcut(app, ctx);
    let screen = ctx.screen_rect();
    let columns = ((screen.width() - 48.0) / SHORTCUT_COLUMN_WIDTH)
        .floor()
        .clamp(1.0, 3.0) as usize;
    let columns = shortcut_columns(columns);
    let mut open = true;
    egui::Window::new("Keyboard Shortcuts")
        .fit_screen(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(
                        "Click a shortcut, then press the new keys (Esc: cancel). \
                         + adds another key, × leaves it without one.",
                    )
                    .small()
                    .color(crate::ui::style::TEXT_DIM),
                );
                let changed = keymap::ACTIONS
                    .iter()
                    .any(|i| !app.workspace.keymap.is_default(i.action));
                if ui
                    .add_enabled(changed, egui::Button::new("Reset All"))
                    .clicked()
                {
                    app.workspace.keymap.reset_all();
                    set_notice(ctx, None);
                }
            });
            if let Some(notice) = notice(ctx) {
                ui.label(egui::RichText::new(notice).color(crate::ui::style::TEXT_STRONG));
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height((screen.height() - 150.0).max(200.0))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for (c, column) in columns.iter().enumerate() {
                            if c > 0 {
                                ui.separator();
                            }
                            ui.vertical(|ui| {
                                ui.set_width(SHORTCUT_COLUMN_WIDTH - 24.0);
                                for group in column {
                                    shortcut_group(app, ui, group);
                                }
                            });
                        }
                    });
                });
        });
    app.modal_state.show_shortcuts = open;
}

/// The groups split into `n` columns of about the same height, in order.
fn shortcut_columns(n: usize) -> Vec<Vec<&'static str>> {
    let groups = groups();
    let total: usize = groups.iter().map(|g| group_rows(g)).sum();
    let target = total.div_ceil(n.max(1));
    let mut columns = vec![Vec::new()];
    let mut height = 0;
    for group in groups {
        let h = group_rows(group);
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
        columns.last_mut().expect("one column").push(group);
        height += h;
    }
    columns
}

fn shortcut_group(app: &mut PainterApp, ui: &mut egui::Ui, group: &str) {
    ui.label(
        egui::RichText::new(group.to_uppercase())
            .small()
            .strong()
            .color(crate::ui::style::TEXT_DIM),
    );
    egui::Grid::new(group)
        .num_columns(2)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            let name = |ui: &mut egui::Ui, text: &str| {
                ui.allocate_ui(egui::vec2(SHORTCUT_NAME_WIDTH, 0.0), |ui| {
                    ui.set_width(SHORTCUT_NAME_WIDTH);
                    ui.add(egui::Label::new(text).wrap());
                });
            };
            for info in keymap::ACTIONS.iter().filter(|i| i.group == group) {
                name(ui, info.label);
                action_keys(app, ui, info.action);
                ui.end_row();
            }
            for (keys, what) in fixed_rows(group) {
                name(ui, what);
                let keys = crate::app::input::keyboard::with_keycaps(ui.ctx(), keys);
                ui.label(
                    egui::RichText::new(keys)
                        .monospace()
                        .color(crate::ui::style::TEXT_DIM),
                );
                ui.end_row();
            }
        });
    ui.add_space(8.0);
}

/// An action's keys (click to record new ones) and its buttons.
fn action_keys(app: &mut PainterApp, ui: &mut egui::Ui, action: Action) {
    let ws = &mut app.workspace;
    let recording = ws.recording_shortcut.filter(|r| r.0 == action);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let text = match recording {
            Some(_) => "Press keys…".to_string(),
            None => ws.keymap.labels(ui.ctx(), action),
        };
        let keys = ui
            .selectable_label(recording.is_some(), egui::RichText::new(text).monospace())
            .on_hover_text("Click, then press the new keys");
        if keys.clicked() {
            ws.recording_shortcut = match recording {
                Some(_) => None,
                None => Some((action, false)),
            };
        }
        if ui
            .small_button("+")
            .on_hover_text("Add another key")
            .clicked()
        {
            ws.recording_shortcut = Some((action, true));
        }
        if !ws.keymap.bindings(action).is_empty()
            && ui.small_button("×").on_hover_text("No shortcut").clicked()
        {
            ws.keymap.set(action, Vec::new());
            ws.recording_shortcut = None;
        }
        if !ws.keymap.is_default(action)
            && ui
                .small_button("Default")
                .on_hover_text(format!(
                    "Back to {}",
                    keymap::Keymap::default().labels(ui.ctx(), action)
                ))
                .clicked()
        {
            ws.keymap.reset(action);
        }
    });
}

/// While a shortcut is being recorded, the next key press (with its
/// modifiers) becomes it; Esc cancels. Keys that are only modifiers wait.
fn record_shortcut(app: &mut PainterApp, ctx: &egui::Context) {
    let Some((action, add)) = app.workspace.recording_shortcut else {
        return;
    };
    let pressed = ctx.input_mut(|i| {
        let found = i.events.iter().position(|e| {
            matches!(
                e,
                egui::Event::Key {
                    pressed: true,
                    repeat: false,
                    ..
                }
            )
        })?;
        match i.events.remove(found) {
            egui::Event::Key {
                key,
                physical_key,
                modifiers,
                ..
            } => Some((key, physical_key, modifiers)),
            _ => None,
        }
    });
    let Some((key, physical_key, modifiers)) = pressed else {
        return;
    };
    app.workspace.recording_shortcut = None;
    if key == egui::Key::Escape && modifiers.is_none() {
        return;
    }
    let binding = keymap::Binding::recorded(key, physical_key, modifiers);
    let map = &mut app.workspace.keymap;
    if !add {
        map.set(action, Vec::new());
    }
    let taken = map.assign(action, binding);
    let label = binding.label(ctx);
    set_notice(
        ctx,
        (!taken.is_empty()).then(|| {
            let names: Vec<&str> = taken.iter().map(|a| a.info().label).collect();
            format!(
                "{label} was the shortcut for {}; now it's {}.",
                names.join(", "),
                action.info().label
            )
        }),
    );
}

fn notice_id() -> egui::Id {
    egui::Id::new("shortcut_notice")
}

fn notice(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp(notice_id()))
}

fn set_notice(ctx: &egui::Context, text: Option<String>) {
    ctx.data_mut(|d| match text {
        Some(t) => d.insert_temp(notice_id(), t),
        None => d.remove::<String>(notice_id()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_columns_keep_every_group_in_order_and_balance() {
        for n in 1..=3 {
            let columns = shortcut_columns(n);
            assert!(columns.len() <= n);
            let flat: Vec<&str> = columns.iter().flatten().copied().collect();
            assert_eq!(flat, groups(), "{n} columns");
            let heights: Vec<usize> = columns
                .iter()
                .map(|c| c.iter().map(|g| group_rows(g)).sum())
                .collect();
            let (lo, hi) = (heights.iter().min().unwrap(), heights.iter().max().unwrap());
            assert!(hi - lo <= 14, "{n} columns: {heights:?}");
        }
    }

    #[test]
    fn every_fixed_row_is_under_a_group_of_commands() {
        for (group, _) in FIXED {
            assert!(groups().contains(group), "{group}");
        }
    }
}
