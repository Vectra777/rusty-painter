//! The Select menu's selection changes (Modify, From Layer Paint, saved
//! selections, Quick Mask) and their dialogs.

use crate::PainterApp;
use crate::app::tools::select::SelectionModify;
use crate::selection::SelectionMode;
use crate::ui::widgets::{property_row, slider_row};
use eframe::egui::{self, Key, Modifiers};

/// An open Modify or Save Selection dialog.
#[derive(Clone, Debug)]
pub enum SelectDialog {
    Modify { op: SelectionModify, radius: u32 },
    Save { name: String },
}

/// What a saved selection's submenu picked.
enum SavedAction {
    Load(usize, SelectionMode),
    Delete(usize),
}

/// The Select menu entries after Select All / Deselect / Invert.
pub fn select_menu_items(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    item: impl Fn(&mut egui::Ui, &str, Option<String>) -> bool,
) {
    let quick_mask = app.workspace.select.quick_mask.is_some();
    let has_selection = app.selection_manager.has_selection() && !quick_mask;
    ui.separator();
    ui.add_enabled_ui(has_selection, |ui| {
        ui.menu_button("Modify", |ui| {
            for op in SelectionModify::ALL {
                if item(ui, &format!("{}…", op.name()), None) {
                    let radius = app.workspace.select.modify.get(op);
                    app.workspace.select.dialog = Some(SelectDialog::Modify { op, radius });
                }
            }
        });
    });
    if ui
        .add_enabled_ui(!quick_mask, |ui| item(ui, "From Layer Paint", None))
        .inner
    {
        let active = app.canvas.active_layer_idx;
        app.select_layer_paint(active, SelectionMode::Replace);
    }
    ui.separator();
    if ui
        .add_enabled_ui(has_selection, |ui| item(ui, "Save Selection…", None))
        .inner
    {
        let name = app.next_saved_selection_name();
        app.workspace.select.dialog = Some(SelectDialog::Save { name });
    }
    let names: Vec<String> = app
        .workspace
        .select
        .saved
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let mut action = None;
    ui.add_enabled_ui(!names.is_empty() && !quick_mask, |ui| {
        ui.menu_button("Load Selection", |ui| {
            for (i, name) in names.iter().enumerate() {
                ui.menu_button(name, |ui| {
                    for (mode, label) in [
                        (SelectionMode::Replace, "Replace"),
                        (SelectionMode::Add, "Add"),
                        (SelectionMode::Subtract, "Subtract"),
                        (SelectionMode::Intersect, "Intersect"),
                    ] {
                        if item(ui, label, None) {
                            action = Some(SavedAction::Load(i, mode));
                        }
                    }
                    ui.separator();
                    if item(ui, "Delete", None) {
                        action = Some(SavedAction::Delete(i));
                    }
                });
            }
        });
    });
    match action {
        Some(SavedAction::Load(i, mode)) => app.load_selection(i, mode),
        Some(SavedAction::Delete(i)) => app.delete_saved_selection(i),
        None => {}
    }
    ui.separator();
    let label = if quick_mask {
        "Leave Quick Mask"
    } else {
        "Quick Mask"
    };
    let hint = crate::app::input::keyboard::shortcut_label(ui.ctx(), Modifiers::SHIFT, Key::Q);
    if item(ui, label, Some(hint)) {
        app.toggle_quick_mask();
    }
}

/// The Modify (Grow, Shrink, Feather, Border) or Save Selection dialog.
pub fn select_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.workspace.select.dialog.take() else {
        return;
    };
    let mut open = true;
    let (mut ok, mut cancel) = (false, false);
    let title = match &dialog {
        SelectDialog::Modify { op, .. } => format!("{} Selection", op.name()),
        SelectDialog::Save { .. } => "Save Selection".to_string(),
    };
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            match &mut dialog {
                SelectDialog::Modify { op, radius } => {
                    slider_row(
                        ui,
                        op.radius_label(),
                        egui::Slider::new(radius, 1..=500)
                            .logarithmic(true)
                            .suffix(" px"),
                    );
                }
                SelectDialog::Save { name } => {
                    property_row(ui, "Name", |ui| {
                        let response = ui.text_edit_singleline(name);
                        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            ok = true;
                        }
                    });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ok |= ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if ok {
        match dialog {
            SelectDialog::Modify { op, radius } => app.modify_selection(op, radius),
            SelectDialog::Save { name } => {
                let name = name.trim();
                if !name.is_empty() {
                    app.save_selection(name.to_string());
                }
            }
        }
    } else if !(cancel || !open) {
        app.workspace.select.dialog = Some(dialog);
    }
}
