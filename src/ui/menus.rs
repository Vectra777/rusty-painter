//! The app menus (File, Edit, View, Help): the desktop menu bar, the
//! touch menu sheet, and the file/panel actions they and the shortcuts run.

use crate::PainterApp;
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::{bar_frame, icon_button};
use eframe::egui::{self, Key, Modifiers, RichText};

/// As printed on this keyboard (see [`crate::app::input::keyboard`]).
fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    crate::app::input::keyboard::shortcut_label(ctx, modifiers, key)
}

fn menu_action_id() -> egui::Id {
    egui::Id::new("rusty_painter_menu_action")
}

/// A menu entry with a right-aligned shortcut hint; closes the menu on click.
fn menu_item(ui: &mut egui::Ui, label: &str, hint: Option<String>) -> bool {
    let touch = metrics(ui.ctx()).touch;
    // Touch menus live in the sheet, where items span its column.
    let (min_width, min_height) = if touch {
        (ui.available_width().max(220.0), 44.0)
    } else {
        (220.0, 0.0)
    };
    let mut button = egui::Button::new(label).min_size(egui::vec2(min_width, min_height));
    // Keyboard shortcuts mean nothing without a keyboard.
    if let Some(hint) = hint.filter(|_| !touch) {
        button = button.shortcut_text(RichText::new(hint).color(TEXT_DIM));
    }
    let clicked = ui.add(button).clicked();
    if clicked {
        ui.close_menu();
        // Lets the touch menu sheet close too.
        ui.ctx().data_mut(|d| d.insert_temp(menu_action_id(), true));
    }
    clicked
}

/// Whether a menu item was picked since the last call.
fn take_menu_action(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(menu_action_id()))
        .unwrap_or(false)
}

/// The top-level menus, shared by the desktop menu bar and the touch sheet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MenuSection {
    #[default]
    File,
    Edit,
    View,
    Help,
}

impl MenuSection {
    const ALL: [(MenuSection, &'static str); 4] = [
        (MenuSection::File, "File"),
        (MenuSection::Edit, "Edit"),
        (MenuSection::View, "View"),
        (MenuSection::Help, "Help"),
    ];

    fn items(self, app: &mut PainterApp, ui: &mut egui::Ui) {
        match self {
            MenuSection::File => file_menu(app, ui),
            MenuSection::Edit => edit_menu(app, ui),
            MenuSection::View => view_menu(app, ui),
            MenuSection::Help => help_menu(app, ui),
        }
    }
}

/// File / Edit / View / Help menus. Touch mode has the bottom menu sheet
/// instead (see [`menu_sheet`]).
pub fn menu_bar(app: &mut PainterApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("menu_bar")
        .exact_height(metrics(ctx).menu_height)
        .frame(bar_frame(BG_CANVAS))
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
                for (section, title) in MenuSection::ALL {
                    ui.menu_button(title, |ui| section.items(app, ui));
                }

                // Show/hide the side panels (also Tab on desktop).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let size = (metrics(ctx).menu_height - 4.0).min(40.0);
                    panel_toggles(app, ui, size);
                });
            });
        });
}

fn file_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    if menu_item(ui, "New Canvas…", Some(shortcut(ctx, cmd, Key::N))) {
        open_new_canvas_dialog(app);
    }
    if menu_item(ui, "Open…", Some(shortcut(ctx, cmd, Key::O))) {
        open_project(app);
    }
    if menu_item(ui, "Save…", Some(shortcut(ctx, cmd, Key::S))) {
        save_project(app);
    }
    ui.separator();
    if menu_item(ui, "Import Image…", Some(shortcut(ctx, cmd_shift, Key::O))) {
        crate::app::import::import_image_dialog(app);
    }
    if menu_item(ui, "Export Image…", Some(shortcut(ctx, cmd, Key::E))) {
        open_export_dialog(app);
    }
    ui.separator();
    if menu_item(ui, "Settings…", None) {
        app.modal_state.show_general_settings = true;
    }
}

fn edit_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    if menu_item(ui, "Undo", Some(shortcut(ctx, cmd, Key::Z))) {
        app.apply_history(false);
    }
    if menu_item(ui, "Redo", Some(shortcut(ctx, cmd_shift, Key::Z))) {
        app.apply_history(true);
    }
    ui.separator();
    if menu_item(ui, "Cut", Some(shortcut(ctx, cmd, Key::X))) {
        app.cut_selection();
    }
    if menu_item(ui, "Copy", Some(shortcut(ctx, cmd, Key::C))) {
        app.copy_selection(false);
    }
    if menu_item(ui, "Copy Merged", Some(shortcut(ctx, cmd_shift, Key::C))) {
        app.copy_selection(true);
    }
    if menu_item(ui, "Paste", Some(shortcut(ctx, cmd, Key::V))) {
        app.paste();
    }
    ui.separator();
    if menu_item(ui, "New Layer", Some(shortcut(ctx, cmd_shift, Key::N))) {
        app.add_layer_and_select();
    }
    if menu_item(ui, "Duplicate Layer", Some(shortcut(ctx, cmd, Key::J))) {
        app.duplicate_layer();
    }
    if menu_item(ui, "Palette…", None) {
        app.workspace.palette.open = true;
    }
    if menu_item(ui, "New Folder", Some(shortcut(ctx, cmd, Key::G))) {
        app.add_folder();
    }
    if menu_item(ui, "Add Layer Mask", None) {
        app.add_mask_to_active();
    }
    if menu_item(ui, "Select All", Some(shortcut(ctx, cmd, Key::A))) {
        app.select_all();
    }
    if menu_item(
        ui,
        "Invert Selection",
        Some(shortcut(ctx, cmd_shift, Key::I)),
    ) {
        app.invert_selection();
    }
    if menu_item(
        ui,
        "Content-Aware Fill",
        Some(shortcut(ctx, Modifiers::SHIFT, Key::F5)),
    ) {
        app.content_aware_fill();
    }
    if menu_item(ui, "Delete Selected Pixels", Some("Delete".into())) {
        app.delete_selection_contents();
    }
    if menu_item(ui, "Deselect", Some(shortcut(ctx, cmd, Key::D))) {
        app.deselect();
    }
    if menu_item(ui, "Swap Colors", Some("X".into())) {
        app.swap_colors();
    }
    ui.separator();
    if menu_item(ui, "Clear Canvas (not undoable)", None) {
        app.clear_canvas();
    }
}

fn view_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    let cmd = Modifiers::COMMAND;
    if menu_item(ui, "Zoom In", Some(shortcut(ctx, cmd, Key::Equals))) {
        app.zoom_by_from_center(1.25);
    }
    if menu_item(ui, "Zoom Out", Some(shortcut(ctx, cmd, Key::Minus))) {
        app.zoom_by_from_center(0.8);
    }
    if menu_item(ui, "Fit to Window", Some(shortcut(ctx, cmd, Key::Num0))) {
        app.fit_view();
    }
    if menu_item(ui, "Actual Pixels", Some(shortcut(ctx, cmd, Key::Num1))) {
        app.set_zoom_from_center(1.0);
    }
    if menu_item(ui, "Reset Rotation", None) {
        app.viewport.rotation = 0.0;
    }
    if menu_item(ui, "Flip Canvas Horizontally", Some("H".into())) {
        app.viewport.flip_x = !app.viewport.flip_x;
    }
    ui.separator();
    ui.menu_button("Mirror Painting", |ui| {
        crate::ui::symmetry_menu::symmetry_controls(app, ui);
    });
    crate::ui::shape_menu::ruler_controls(app, ui);
    ui.menu_button("Assistants", |ui| {
        crate::ui::shape_menu::assistant_controls(app, ui);
    });
    if ui
        .checkbox(&mut app.workspace.wrap_around, "Wrap Around")
        .on_hover_text(
            "Paint past an edge and it comes in at the other; the canvas shows repeated. For \
             seamless tiles and patterns.",
        )
        .changed()
    {
        app.mark_all_tiles_dirty();
    }
    ui.separator();
    if menu_item(ui, "Show / Hide Panels", Some("Tab".into())) {
        toggle_all_panels(app);
    }
    ui.checkbox(&mut app.workspace.show_left_panel, "Brush panel");
    ui.checkbox(&mut app.workspace.show_right_panel, "Color & layers panel");
    ui.checkbox(&mut app.workspace.touch_mode, "Touch mode");
    if app.workspace.touch_mode {
        ui.checkbox(&mut app.workspace.finger_painting, "Paint with one finger")
            .on_hover_text("When off, only a stylus paints and one finger pans the canvas.");
    }
    let stats = &mut app.workspace.frame_stats;
    if ui
        .checkbox(&mut stats.enabled, "Frame times")
        .on_hover_text("Frames per second and time per frame in the status bar")
        .changed()
    {
        stats.window_open = stats.enabled;
    }
    ui.separator();
    if menu_item(ui, "Reset Panel Layout", None) {
        app.dock_left = crate::app::layout::default_left_dock();
        app.dock_right = crate::app::layout::default_right_dock();
    }
}

fn help_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let label = if app.workspace.touch_mode {
        "Gestures & Shortcuts"
    } else {
        "Keyboard Shortcuts"
    };
    if menu_item(ui, label, None) {
        app.modal_state.show_shortcuts = true;
    }
}

/// Width of one menu column in the touch sheet.
const SHEET_COLUMN_WIDTH: f32 = 240.0;

/// Touch mode: the menus in a sheet that slides up from the bottom bar.
/// Wide screens show every menu side by side; narrow ones one at a time.
pub fn menu_sheet(app: &mut PainterApp, ctx: &egui::Context) {
    let open = app.modal_state.menu_sheet_open;
    let t = ctx.animate_bool_with_time(egui::Id::new("menu_sheet_anim"), open, 0.18);
    if t <= 0.0 {
        return;
    }
    // Everything above the bottom bar (the status bar is already placed).
    let area = ctx.available_rect();
    let width = area.width();
    let columns = (width - 16.0) >= SHEET_COLUMN_WIDTH * 4.0;
    let height_id = egui::Id::new("menu_sheet_height");
    let height: f32 = ctx
        .data(|d| d.get_temp(height_id))
        .unwrap_or(area.height() * 0.5);
    let height = height.min(area.height());
    let top = area.bottom() - height * t;

    // Dim the canvas and swallow taps on it: a tap outside closes the sheet
    // instead of painting.
    let backdrop = egui::Area::new(egui::Id::new("menu_sheet_backdrop"))
        .fixed_pos(area.min)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(area.size(), egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_black_alpha((90.0 * t) as u8));
            response
        })
        .inner;

    let response = egui::Area::new(egui::Id::new("menu_sheet"))
        .fixed_pos(egui::pos2(area.left(), top))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // Slide out from under the bottom bar, not over it.
            ui.set_clip_rect(area);
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(egui::Stroke::new(1.0_f32, BORDER_LIGHT))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    ui.set_max_height(area.height() - 16.0);
                    if columns {
                        ui.horizontal_top(|ui| {
                            for (section, title) in MenuSection::ALL {
                                ui.vertical(|ui| {
                                    ui.set_width(SHEET_COLUMN_WIDTH - 8.0);
                                    ui.label(RichText::new(title).strong().color(TEXT_STRONG));
                                    section.items(app, ui);
                                });
                            }
                        });
                    } else {
                        sheet_tabs(app, ui);
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let section = app.modal_state.menu_sheet_section;
                            section.items(app, ui);
                        });
                    }
                });
        })
        .response;
    ctx.data_mut(|d| d.insert_temp(height_id, response.rect.height()));

    // Close after picking an item, or on a tap outside the sheet.
    if take_menu_action(ctx) || backdrop.clicked() || backdrop.drag_started() {
        app.modal_state.menu_sheet_open = false;
    }
    if t < 1.0 {
        ctx.request_repaint();
    }
}

fn sheet_tabs(app: &mut PainterApp, ui: &mut egui::Ui) {
    let width = ui.available_width() / MenuSection::ALL.len() as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (section, title) in MenuSection::ALL {
            let selected = app.modal_state.menu_sheet_section == section;
            let text = RichText::new(title).color(if selected { TEXT_STRONG } else { TEXT });
            let button = egui::Button::new(text)
                .fill(if selected { ACCENT } else { BG_RAISED })
                .min_size(egui::vec2(width, 40.0));
            if ui.add(button).clicked() {
                app.modal_state.menu_sheet_section = section;
            }
        }
    });
}

fn toggle_all_panels(app: &mut PainterApp) {
    let ws = &mut app.workspace;
    let show = !(ws.show_left_panel || ws.show_right_panel);
    ws.show_left_panel = show;
    ws.show_right_panel = show;
}

/// Buttons that show/hide the side docks (more canvas on small screens).
pub(crate) fn panel_toggles(app: &mut PainterApp, ui: &mut egui::Ui, size: f32) {
    let ws = &mut app.workspace;
    if icon_button(
        ui,
        Icon::PanelRight,
        size,
        ws.show_right_panel,
        "Color & layers panel (Tab toggles both)",
    )
    .clicked()
    {
        ws.show_right_panel = !ws.show_right_panel;
    }
    if icon_button(
        ui,
        Icon::PanelLeft,
        size,
        ws.show_left_panel,
        "Brush panel (Tab toggles both)",
    )
    .clicked()
    {
        ws.show_left_panel = !ws.show_left_panel;
    }
}

pub(crate) fn toggle_panels(app: &mut PainterApp) {
    toggle_all_panels(app);
}

pub(crate) fn open_new_canvas_dialog(app: &mut PainterApp) {
    app.modal_state.new_canvas.sync_from_canvas(&app.canvas);
    app.modal_state.new_canvas.color_model = app.workspace.color_model;
    app.modal_state.show_new_canvas_modal = true;
}

pub(crate) fn open_export_dialog(app: &mut PainterApp) {
    app.export_state.settings.chosen_path = None;
    app.export_state.message = None;
    app.export_state.show_modal = true;
}

pub(crate) fn open_project(app: &mut PainterApp) {
    if let Some(path) = open_project_dialog()
        && let Err(err) = app.load_project_from_path(path)
    {
        log::error!("{err}");
        app.export_state.message = Some(err);
    }
}

pub(crate) fn save_project(app: &mut PainterApp) {
    if let Some(path) = save_project_dialog()
        && let Err(err) = app.save_project_to_path(path)
    {
        log::error!("{err}");
        app.export_state.message = Some(err);
    }
}

#[cfg(not(target_os = "android"))]
fn open_project_dialog() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Rusty Painter", &["rpainter"])
        .pick_file()
}

#[cfg(target_os = "android")]
fn open_project_dialog() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(target_os = "android"))]
fn save_project_dialog() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Rusty Painter", &["rpainter"])
        .set_file_name("project.rpainter")
        .save_file()
}

#[cfg(target_os = "android")]
fn save_project_dialog() -> Option<std::path::PathBuf> {
    None
}
