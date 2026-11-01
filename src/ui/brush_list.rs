//! Floating brush presets window (toolbar button or `P`): one column of wide
//! stroke previews with the preset name inside, like Clip Studio's brush
//! list. Picking a preset loads it into the tool it belongs to: eraser
//! presets switch to the eraser, the others to the brush.
//!
//! Above the list, a search box (name or tag) and chips: all presets, the
//! favourites (starred on their tile or from their menu), the recent ones,
//! or one tag. A preset's right-click menu stars it and edits its tags.

use crate::PainterApp;
use crate::app::brush_library::{FolderId, LibraryDrag, Shelf, filtered_presets, shown_presets};
use crate::app::state::BrushState;
use crate::brush_engine::brush::BrushPreset;
use crate::ui::icons::Icon;
use crate::ui::preview_worker::PreviewLook;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use crate::ui::widgets::icon_button;
use eframe::egui::{self, Color32, RichText, Stroke};
use rayon::ThreadPool;
use std::sync::Arc;

/// Temp-memory id used to flag a duplicate preset name in the save modal.
const DUPLICATE_NAME_WARNING_ID: &str = "brush_preset_duplicate_name";
/// Preview texture size; drawn stretched to the tile width.
const PREVIEW_PX: [usize; 2] = [480, 96];
const PREVIEW_DIAMETER: f32 = 26.0;
/// Starting width; the window can be resized both ways.
const WINDOW_WIDTH: f32 = 320.0;

pub fn presets_window(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.brush_state.show_presets {
        return;
    }
    let m = metrics(ctx);
    let mut open = true;
    // Touch mode docks the list in the right panel (layout::show_panels).
    if !m.touch {
        let default_pos = egui::pos2(m.toolbar_width + 8.0, m.menu_height + 8.0);
        egui::Window::new("Brush Presets")
            .fit_screen_size(ctx)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_pos(default_pos)
            .default_size([WINDOW_WIDTH, 520.0])
            .min_size([200.0, 160.0])
            .show(ctx, |ui| presets_contents(app, ui));
    }
    if !open {
        app.brush_state.show_presets = false;
    }
    save_preset_modal(app, ctx);
    import_report_window(app, ctx);
}

/// The presets list and what its actions do (in the window, or docked).
pub(crate) fn presets_contents(app: &mut PainterApp, ui: &mut egui::Ui) {
    let touch = metrics(ui.ctx()).touch;
    match presets_list(app, ui) {
        Some(PresetAction::Pick(index)) => {
            app.apply_preset(index);
            // On a tablet the list is in the way once a preset is picked.
            if touch {
                app.brush_state.show_presets = false;
            }
        }
        Some(PresetAction::Delete(index)) => app.delete_user_preset(index),
        Some(PresetAction::Reset(index)) => app.reset_preset(index),
        Some(PresetAction::RestoreDefaults) => app.restore_default_presets(),
        Some(PresetAction::ToggleFavourite(index)) => {
            let name = app.brush_state.presets[index].name.clone();
            app.edit_library(|lib| lib.toggle_favourite(&name));
        }
        Some(PresetAction::AddTag(index, tag)) => {
            let name = app.brush_state.presets[index].name.clone();
            app.edit_library(|lib| {
                lib.add_tag(&name, &tag);
            });
            app.brush_state.library.new_tag.clear();
        }
        Some(PresetAction::RemoveTag(index, tag)) => {
            let name = app.brush_state.presets[index].name.clone();
            app.edit_library(|lib| lib.remove_tag(&name, &tag));
        }
        Some(PresetAction::Export(index)) => {
            let name = app.brush_state.presets[index].name.clone();
            crate::app::brush_io::export_presets_dialog(app, &[index], &name);
        }
        Some(PresetAction::ExportMine) => {
            let mine: Vec<usize> = (app.brush_state.presets.iter().enumerate())
                .filter(|(_, p)| !PainterApp::is_default_preset(&p.name))
                .map(|(i, _)| i)
                .collect();
            crate::app::brush_io::export_presets_dialog(app, &mine, "My brushes");
        }
        Some(PresetAction::ExportAll) => {
            let all: Vec<usize> = (0..app.brush_state.presets.len()).collect();
            crate::app::brush_io::export_presets_dialog(app, &all, "Brushes");
        }
        Some(PresetAction::Import) => crate::app::brush_io::import_presets_dialog(app),
        None => {}
    }
}

/// What the last import of other apps' brushes brought in, and what it
/// approximated or left out.
fn import_report_window(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(report) = &app.brush_state.import_report else {
        return;
    };
    let mut open = true;
    let mut close = false;
    egui::Window::new("Imported Brushes")
        .fit_screen_size(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([420.0, 300.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    for (file, count, notes) in report {
                        ui.label(RichText::new(format!("{file}: {count} brushes")).strong());
                        for note in notes {
                            ui.label(RichText::new(format!("• {note}")).small().color(TEXT_DIM));
                        }
                        ui.add_space(6.0);
                    }
                });
            ui.label(
                RichText::new(
                    "Other apps' brushes can do things this one doesn't: what has a \
                     counterpart here came across. They're in the presets list.",
                )
                .small()
                .color(TEXT_DIM),
            );
            if ui.button("OK").clicked() {
                close = true;
            }
        });
    if !open || close {
        app.brush_state.import_report = None;
    }
}

/// What the user did in the presets list.
enum PresetAction {
    Pick(usize),
    Delete(usize),
    Reset(usize),
    RestoreDefaults,
    ToggleFavourite(usize),
    AddTag(usize, String),
    RemoveTag(usize, String),
    Export(usize),
    ExportMine,
    ExportAll,
    Import,
}

/// The list, with a menu on each preset (export, delete) and in the header
/// (import, export several).
fn presets_list(app: &mut PainterApp, ui: &mut egui::Ui) -> Option<PresetAction> {
    let m = metrics(ui.ctx());
    let mut picked = None;
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.brush_state.library.drag = None;
    }

    ui.horizontal(|ui| {
        let tool = if app.brush_state.eraser_active() {
            "eraser"
        } else {
            "brush"
        };
        ui.label(
            RichText::new(format!("Active {tool}"))
                .small()
                .color(TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(
                ui,
                Icon::Plus,
                m.header_button,
                false,
                "Save current settings as a preset",
            )
            .clicked()
            {
                app.brush_state.show_new_preset_modal = true;
                app.brush_state.new_preset_name = "New Preset".to_string();
                ui.ctx()
                    .data_mut(|d| d.remove::<bool>(egui::Id::new(DUPLICATE_NAME_WARNING_ID)));
            }
            {
                let menu = icon_button(ui, Icon::Menu, m.header_button, false, "Import and export");
                let id = ui.make_persistent_id("brush_presets_menu");
                if menu.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(id));
                }
                egui::popup_below_widget(
                    ui,
                    id,
                    &menu,
                    egui::PopupCloseBehavior::CloseOnClick,
                    |ui| {
                        ui.set_min_width(170.0);
                        if ui.button("Import brushes…").clicked() {
                            picked = Some(PresetAction::Import);
                        }
                        let any_mine = (app.brush_state.presets.iter())
                            .any(|p| !PainterApp::is_default_preset(&p.name));
                        if ui
                            .add_enabled(any_mine, egui::Button::new("Export my brushes…"))
                            .clicked()
                        {
                            picked = Some(PresetAction::ExportMine);
                        }
                        if ui.button("Export all brushes…").clicked() {
                            picked = Some(PresetAction::ExportAll);
                        }
                        ui.separator();
                        if ui.button("Restore default brushes").clicked() {
                            picked = Some(PresetAction::RestoreDefaults);
                        }
                    },
                );
            }
        });
    });

    app.brush_state
        .library
        .file
        .reconcile(&app.brush_state.presets);
    folder_navigation(&mut app.brush_state, ui);
    let all_tags = library_filters(&mut app.brush_state, ui);
    picked = preset_list_body(app, ui, &all_tags).or(picked.take());
    picked
}

fn preset_list_body(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    all_tags: &[String],
) -> Option<PresetAction> {
    let m = metrics(ui.ctx());
    let mut picked = None;

    let tile_height = if m.touch { 76.0 } else { 58.0 };
    let pool = app.workspace.pool.clone();
    collect_preset_previews(app, ui.ctx());
    let bs = &mut app.brush_state;
    let lib = &bs.library;
    let mut shown = filtered_presets(
        &bs.presets,
        &lib.file,
        &lib.shelf,
        &lib.search,
        lib.tag_filter.as_deref(),
    );
    if lib.shelf != Shelf::Recent {
        shown.sort_by_key(|&i| !lib.file.is_favourite(&bs.presets[i].name));
    }
    let folder = if let Shelf::Folder(id) = lib.shelf {
        Some(id)
    } else {
        None
    };
    let children: Vec<_> = lib
        .file
        .folders
        .iter()
        .filter(|f| f.parent == folder && folder.is_some())
        .filter(|_| lib.search.trim().is_empty() && lib.tag_filter.is_none())
        .cloned()
        .collect();
    egui::ScrollArea::vertical()
        .id_salt(("brush_presets_scroll", format!("{:?}", bs.library.shelf)))
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            drag_scroll(ui, bs.library.drag.is_some());
            ui.spacing_mut().item_spacing.y = 4.0;
            for &index in shown
                .iter()
                .filter(|&&i| {
                    bs.library.shelf != Shelf::Recent
                        && bs.library.file.is_favourite(&bs.presets[i].name)
                })
                .collect::<Vec<_>>()
            {
                picked = preset_row(ui, bs, index, all_tags, &pool, tile_height).or(picked.take());
            }
            for child in &children {
                folder_row(&mut bs.library, ui, child);
            }
            for index in shown
                .iter()
                .copied()
                .filter(|&i| {
                    bs.library.shelf == Shelf::Recent
                        || !bs.library.file.is_favourite(&bs.presets[i].name)
                })
                .collect::<Vec<_>>()
            {
                picked = preset_row(ui, bs, index, all_tags, &pool, tile_height).or(picked.take());
            }
            if shown.is_empty() && children.is_empty() {
                ui.label(RichText::new("No brushes here").small().color(TEXT_DIM));
            }
            if let Some(folder) = folder
                && bs.library.drag.is_some()
            {
                let response = ui.add_sized(
                    [ui.available_width(), 28.0],
                    egui::Label::new("Drop here to place last"),
                );
                if response.contains_pointer() && ui.input(|i| i.pointer.any_released()) {
                    match bs.library.drag.take() {
                        Some(LibraryDrag::Brush(id)) => {
                            bs.library.file.move_brush(id, folder, None)
                        }
                        Some(LibraryDrag::Folder(id)) => {
                            bs.library.file.move_folder(id, Some(folder), None);
                        }
                        None => {}
                    }
                    bs.library.dirty = true;
                }
            }
        });
    if bs.library.drag.is_some() && ui.input(|i| i.pointer.any_released()) {
        bs.library.drag = None;
    }
    picked
}

fn paint_grip(ui: &egui::Ui, rect: egui::Rect) {
    for x in [-2.5, 2.5] {
        for y in [-5.0, 0.0, 5.0] {
            ui.painter()
                .circle_filled(rect.center() + egui::vec2(x, y), 1.1, TEXT_DIM);
        }
    }
}

fn library_drag_started(ui: &egui::Ui, response: &egui::Response) -> bool {
    if !metrics(ui.ctx()).touch {
        return response.drag_started();
    }
    let id = response.id.with("hold_to_move");
    let now = ui.input(|i| i.time);
    if !response.is_pointer_button_down_on() {
        ui.data_mut(|d| d.remove::<(f64, bool)>(id));
        return false;
    }
    let (started, lifted) = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || (now, false)));
    if !lifted && now - started >= 0.45 {
        ui.data_mut(|d| d.insert_temp(id, (started, true)));
        return true;
    }
    ui.ctx().request_repaint();
    false
}

fn drag_scroll(ui: &egui::Ui, dragging: bool) {
    if !dragging {
        return;
    }
    if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
        let rect = ui.clip_rect();
        if rect.contains(pointer) {
            if pointer.y < rect.top() + 24.0 {
                ui.scroll_with_delta(egui::vec2(0.0, 12.0));
            } else if pointer.y > rect.bottom() - 24.0 {
                ui.scroll_with_delta(egui::vec2(0.0, -12.0));
            }
        }
    }
    ui.ctx().request_repaint();
}

fn choose_shelf(lib: &mut crate::app::brush_library::BrushLibrary, shelf: Shelf) {
    lib.file.selected_folder = if let Shelf::Folder(id) = shelf {
        Some(id)
    } else {
        None
    };
    lib.shelf = shelf;
    lib.tag_filter = None;
    lib.dirty = true;
}

fn folder_navigation(bs: &mut BrushState, ui: &mut egui::Ui) {
    let lib = &mut bs.library;
    let current = match &lib.shelf {
        Shelf::Folder(id) => lib.file.folder_path(*id),
        Shelf::Favourites => "Favorites".into(),
        Shelf::Recent => "Recent".into(),
        Shelf::All => "All brushes".into(),
    };
    let parent = if let Shelf::Folder(id) = lib.shelf {
        lib.file
            .folders
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| f.parent)
    } else {
        None
    };
    ui.horizontal(|ui| {
        if ui
            .add_enabled(parent.is_some(), egui::Button::new("↑"))
            .on_hover_text("Parent folder")
            .clicked()
        {
            choose_shelf(lib, Shelf::Folder(parent.unwrap()));
        }
        egui::ComboBox::from_id_salt("brush_folder_picker")
            .width(ui.available_width())
            .selected_text(current)
            .truncate()
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(lib.shelf == Shelf::Favourites, "Favorites")
                    .clicked()
                {
                    choose_shelf(lib, Shelf::Favourites);
                }
                ui.separator();
                let roots: Vec<_> = lib
                    .file
                    .folders
                    .iter()
                    .filter(|f| f.parent.is_none())
                    .cloned()
                    .collect();
                for root in roots {
                    if ui
                        .selectable_label(lib.shelf == Shelf::Folder(root.id), &root.name)
                        .clicked()
                    {
                        choose_shelf(lib, Shelf::Folder(root.id));
                    }
                }
                ui.separator();
                if ui
                    .selectable_label(lib.shelf == Shelf::All, "All brushes")
                    .clicked()
                {
                    choose_shelf(lib, Shelf::All);
                }
                if ui
                    .selectable_label(lib.shelf == Shelf::Recent, "Recent")
                    .clicked()
                {
                    choose_shelf(lib, Shelf::Recent);
                }
            });
    });
    ui.menu_button("Manage folders", |ui| {
        ui.add(egui::TextEdit::singleline(&mut lib.folder_name).hint_text("Folder name"));
        if ui
            .add_enabled(
                !lib.folder_name.trim().is_empty(),
                egui::Button::new("Create folder here"),
            )
            .clicked()
        {
            let parent = if let Shelf::Folder(id) = lib.shelf {
                Some(id)
            } else {
                None
            };
            lib.file.create_folder(lib.folder_name.trim(), parent);
            lib.folder_name.clear();
            lib.dirty = true;
            ui.close_menu();
        }
        if let Shelf::Folder(id) = lib.shelf {
            ui.separator();
            folder_actions(lib, ui, id, false);
        }
    });
}

fn folder_actions(
    lib: &mut crate::app::brush_library::BrushLibrary,
    ui: &mut egui::Ui,
    id: FolderId,
    name_field: bool,
) {
    if name_field {
        ui.add(egui::TextEdit::singleline(&mut lib.folder_name).hint_text("New name"));
    }
    if ui
        .add_enabled(
            !lib.folder_name.trim().is_empty(),
            egui::Button::new("Rename folder"),
        )
        .clicked()
    {
        if let Some(f) = lib.file.folders.iter_mut().find(|f| f.id == id) {
            f.name = lib.folder_name.trim().into();
        }
        lib.folder_name.clear();
        lib.dirty = true;
        ui.close_menu();
    }
    ui.menu_button("Move folder to", |ui| {
        if ui.button("Top level").clicked() {
            lib.file.move_folder(id, None, None);
            lib.dirty = true;
            ui.close_menu();
        }
        for f in lib.file.folders.clone() {
            if f.id != id && ui.button(lib.file.folder_path(f.id)).clicked() {
                lib.dirty |= lib.file.move_folder(id, Some(f.id), None);
                ui.close_menu();
            }
        }
    });
    if ui.button("Delete folder (keep brushes)").clicked() {
        let parent = lib
            .file
            .folders
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| f.parent);
        lib.file.delete_folder(id);
        if lib.shelf == Shelf::Folder(id) {
            choose_shelf(lib, parent.map_or(Shelf::All, Shelf::Folder));
        }
        lib.dirty = true;
        ui.close_menu();
    }
}

fn folder_row(
    lib: &mut crate::app::brush_library::BrushLibrary,
    ui: &mut egui::Ui,
    folder: &crate::app::brush_library::BrushFolder,
) {
    let height = if metrics(ui.ctx()).touch { 48.0 } else { 36.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    ui.painter().rect_filled(
        rect,
        RADIUS_CARD,
        if response.hovered() {
            BG_RAISED
        } else {
            BG_INSET
        },
    );
    crate::ui::icons::paint_icon(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 20.0, rect.center().y),
            egui::vec2(20.0, 20.0),
        ),
        Icon::Folder,
        TEXT_DIM,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 38.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &folder.name,
        egui::TextStyle::Body.resolve(ui.style()),
        TEXT_STRONG,
    );
    let grip = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 15.0, rect.center().y),
        egui::vec2(24.0, 24.0),
    );
    paint_grip(ui, grip);
    let handle = ui.interact(
        grip,
        ui.id().with(("folder_grip", folder.id)),
        egui::Sense::drag(),
    );
    if library_drag_started(ui, &handle) || (!metrics(ui.ctx()).touch && response.drag_started()) {
        lib.drag = Some(LibraryDrag::Folder(folder.id));
    }
    if response.clicked() {
        choose_shelf(lib, Shelf::Folder(folder.id));
    }
    if response.contains_pointer() && lib.drag.is_some() {
        let insert = matches!(lib.drag, Some(LibraryDrag::Folder(_)))
            && ui.input(|i| {
                i.pointer
                    .hover_pos()
                    .is_some_and(|p| p.y < rect.top() + rect.height() * 0.25)
            });
        ui.painter()
            .rect_stroke(rect, RADIUS_CARD, Stroke::new(1.0_f32, accent()));
        if ui.input(|i| i.pointer.any_released()) {
            match lib.drag.take() {
                Some(LibraryDrag::Brush(id)) => lib.file.move_brush(id, folder.id, None),
                Some(LibraryDrag::Folder(id)) => {
                    lib.file.move_folder(
                        id,
                        if insert {
                            folder.parent
                        } else {
                            Some(folder.id)
                        },
                        insert.then_some(folder.id),
                    );
                }
                None => {}
            }
            lib.dirty = true;
        }
    }
    if lib.drag.is_none() {
        response.context_menu(|ui| folder_actions(lib, ui, folder.id, true));
    }
}

/// Tags filter the current folder (including descendants), without navigating away.
fn library_filters(bs: &mut BrushState, ui: &mut egui::Ui) -> Vec<String> {
    let lib = &mut bs.library;
    let all_tags = lib.file.all_tags();
    let scope = match lib.shelf {
        Shelf::Folder(id) => lib.file.folder_indices(&bs.presets, id, true),
        _ => shown_presets(&bs.presets, &lib.file, &lib.shelf, ""),
    };
    let mut scoped_tags: Vec<_> = scope
        .into_iter()
        .flat_map(|i| lib.file.tags(&bs.presets[i].name).iter().cloned())
        .collect();
    scoped_tags.sort_by_key(|t| t.to_lowercase());
    scoped_tags.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    ui.add(
        egui::TextEdit::singleline(&mut lib.search)
            .hint_text("Search names and tags")
            .desired_width(f32::INFINITY),
    );
    egui::ComboBox::from_id_salt("brush_tag_filter")
        .selected_text(lib.tag_filter.as_deref().unwrap_or("All tags"))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut lib.tag_filter, None, "All tags");
            for tag in &scoped_tags {
                ui.selectable_value(&mut lib.tag_filter, Some(tag.clone()), tag);
            }
        });
    all_tags
}

/// One preset's tile, with its menu (favourite, tags, export, delete).
fn preset_row(
    ui: &mut egui::Ui,
    bs: &mut BrushState,
    index: usize,
    all_tags: &[String],
    pool: &Arc<ThreadPool>,
    tile_height: f32,
) -> Option<PresetAction> {
    let mut picked = None;
    // Only the tiles on screen ask for their preview.
    let tile = egui::Rect::from_min_size(
        ui.next_widget_position(),
        egui::vec2(ui.available_width(), tile_height),
    );
    let texture = if ui.is_rect_visible(tile) {
        preset_preview(bs, index, pool, ui.ctx())
    } else {
        None
    };
    let name = bs.presets[index].name.clone();
    let id = bs.library.file.preset_id(&name);
    let preset = &bs.presets[index];
    // Any brush can be the eraser's, so the name alone says which is in use.
    let active = bs.active_preset.as_deref() == Some(preset.name.as_str());
    let favourite = bs.library.file.is_favourite(&preset.name);
    let (tile, star) = preset_tile(ui, &preset.name, texture, tile_height, active, favourite);
    let grip = egui::Rect::from_min_size(
        tile.rect.left_bottom() - egui::vec2(0.0, 22.0),
        egui::vec2(24.0, 22.0),
    );
    paint_grip(ui, grip);
    let handle = ui
        .interact(grip, ui.id().with(("move_brush", id)), egui::Sense::drag())
        .on_hover_text("Drag to reorder or move to a folder");
    if library_drag_started(ui, &handle) || (!metrics(ui.ctx()).touch && tile.drag_started()) {
        bs.library.drag = Some(LibraryDrag::Brush(id));
    }

    if star {
        picked = Some(PresetAction::ToggleFavourite(index));
    } else if tile.clicked() {
        picked = Some(PresetAction::Pick(index));
    }
    if matches!(bs.library.shelf, Shelf::Folder(_))
        && bs.library.search.trim().is_empty()
        && tile.contains_pointer()
        && bs.library.drag.is_some()
    {
        ui.painter().line_segment(
            [tile.rect.left_top(), tile.rect.right_top()],
            Stroke::new(2.0_f32, accent()),
        );
        if ui.input(|i| i.pointer.any_released())
            && let Some(LibraryDrag::Brush(from)) = bs.library.drag.take()
            && let Shelf::Folder(folder) = bs.library.shelf
        {
            bs.library.file.move_brush(from, folder, Some(id));
            bs.library.dirty = true;
        }
    }
    // (Searches of the library: only for the tile under the pointer, as
    // every tile is laid out each frame.)
    if tile.hovered()
        && let Some(folder) = bs
            .library
            .file
            .folders
            .iter()
            .find(|f| f.brushes.contains(&id))
    {
        tile.clone()
            .on_hover_text(bs.library.file.folder_path(folder.id));
    }
    let tags = bs.library.file.tags(&preset.name).to_vec();
    let new_tag = &mut bs.library.new_tag;
    if bs.library.drag.is_none() {
        tile.context_menu(|ui| {
            let star_text = if favourite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            };
            if ui.button(star_text).clicked() {
                picked = Some(PresetAction::ToggleFavourite(index));
                ui.close_menu();
            }
            if let Shelf::Folder(folder) = bs.library.shelf {
                let positions = bs
                    .library
                    .file
                    .folders
                    .iter()
                    .find(|f| f.id == folder)
                    .map(|f| f.brushes.clone())
                    .unwrap_or_default();
                let at = positions.iter().position(|&v| v == id).unwrap_or(0);
                if ui
                    .add_enabled(at > 0, egui::Button::new("Move up"))
                    .clicked()
                {
                    bs.library.file.move_brush(
                        id,
                        folder,
                        positions.get(at.wrapping_sub(1)).copied(),
                    );
                    bs.library.dirty = true;
                    ui.close_menu();
                }
                if ui
                    .add_enabled(at + 1 < positions.len(), egui::Button::new("Move down"))
                    .clicked()
                {
                    bs.library
                        .file
                        .move_brush(id, folder, positions.get(at + 2).copied());
                    bs.library.dirty = true;
                    ui.close_menu();
                }
            }
            ui.menu_button("Move to folder", |ui| {
                let folders = bs.library.file.folders.clone();
                for folder in folders {
                    if ui.button(bs.library.file.folder_path(folder.id)).clicked() {
                        bs.library.file.move_brush(id, folder.id, None);
                        bs.library.dirty = true;
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Tags", |ui| {
                for tag in all_tags {
                    let has = tags.iter().any(|t| t.eq_ignore_ascii_case(tag));
                    let mut on = has;
                    if ui.checkbox(&mut on, tag.as_str()).changed() {
                        picked = Some(if on {
                            PresetAction::AddTag(index, tag.clone())
                        } else {
                            let own = tags.iter().find(|t| t.eq_ignore_ascii_case(tag));
                            PresetAction::RemoveTag(index, own.unwrap_or(tag).clone())
                        });
                    }
                }
                if !all_tags.is_empty() {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(new_tag)
                            .hint_text("New tag")
                            .desired_width(110.0),
                    );
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let add = ui
                        .add_enabled(!new_tag.trim().is_empty(), egui::Button::new("Add"))
                        .clicked();
                    if (add || enter) && !new_tag.trim().is_empty() {
                        picked = Some(PresetAction::AddTag(index, new_tag.trim().to_string()));
                        ui.close_menu();
                    }
                });
            });
            ui.separator();
            if ui.button("Export…").clicked() {
                picked = Some(PresetAction::Export(index));
            }
            let default = PainterApp::is_default_preset(&name)
                || bs.library.file.pack_presets.values().any(|n| *n == name);
            if default && ui.button("Reset to default").clicked() {
                picked = Some(PresetAction::Reset(index));
            }
            if ui.button("Delete").clicked() {
                picked = Some(PresetAction::Delete(index));
            }
        });
    }
    picked
}

/// A wide preview tile with the name drawn inside it, and a star in the
/// top-left corner. Returns the tile and whether the star was clicked.
fn preset_tile(
    ui: &mut egui::Ui,
    name: &str,
    texture: Option<egui::TextureId>,
    height: f32,
    active: bool,
    favourite: bool,
) -> (egui::Response, bool) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        if metrics(ui.ctx()).touch {
            egui::Sense::click()
        } else {
            egui::Sense::click_and_drag()
        },
    );
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        RADIUS_CARD,
        if response.hovered() {
            BG_RAISED
        } else {
            BG_INSET
        },
    );
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    // At the preview's own shape, centred, however wide the window is.
    let inner = rect.shrink2(egui::vec2(8.0, 6.0));
    let aspect = PREVIEW_PX[0] as f32 / PREVIEW_PX[1] as f32;
    let size = egui::vec2(inner.width().min(inner.height() * aspect), inner.height());
    // (Not drawn yet: just the name.)
    if let Some(texture) = texture {
        painter.image(
            texture,
            egui::Rect::from_center_size(inner.center(), size),
            uv,
            Color32::WHITE,
        );
    }

    // Name in the top-left corner on a dark chip so it reads over the stroke.
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = painter.layout_no_wrap(name.to_string(), font, TEXT_STRONG);
    let chip = egui::Rect::from_min_size(
        rect.min + egui::vec2(if metrics(ui.ctx()).touch { 40.0 } else { 30.0 }, 4.0),
        galley.size() + egui::vec2(8.0, 4.0),
    );
    painter.rect_filled(chip, RADIUS_SMALL, Color32::from_black_alpha(150));
    painter.galley(chip.min + egui::vec2(4.0, 2.0), galley, TEXT_STRONG);

    let stroke = if active {
        Stroke::new(2.0_f32, accent())
    } else if response.hovered() {
        Stroke::new(1.0_f32, TEXT_DIM)
    } else {
        Stroke::new(1.0_f32, BORDER)
    };
    painter.rect_stroke(rect, RADIUS_CARD, stroke);

    // Always visible, including on touch: the star is distinct from picking a brush.
    let star_size = if metrics(ui.ctx()).touch { 30.0 } else { 22.0 };
    let star_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(4.0, 4.0),
        egui::vec2(star_size, star_size),
    );
    let star_response = ui.interact(
        star_rect,
        response.id.with("favorite"),
        egui::Sense::click(),
    );
    let over_star = star_response.hovered();
    {
        let colour = if favourite {
            Color32::from_rgb(245, 190, 65)
        } else if over_star {
            TEXT_STRONG
        } else {
            TEXT_DIM
        };
        draw_star(
            painter,
            star_rect.center(),
            star_size * 0.5,
            favourite,
            colour,
        );
    }
    let star_clicked = star_response.clicked();
    star_response.on_hover_text(if favourite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    });
    (response.on_hover_text(name), star_clicked)
}

/// A five-pointed star, filled or outlined.
fn draw_star(
    painter: &egui::Painter,
    centre: egui::Pos2,
    radius: f32,
    filled: bool,
    colour: Color32,
) {
    let point = |k: usize| {
        let r = if k.is_multiple_of(2) {
            radius
        } else {
            radius * 0.45
        };
        let a = k as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
        centre + egui::vec2(a.cos(), a.sin()) * r
    };
    let points: Vec<egui::Pos2> = (0..10).map(point).collect();
    if filled {
        // One mesh avoids antialiasing seams between the star's points.
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(centre, colour);
        for &point in &points {
            mesh.colored_vertex(point, colour);
        }
        for k in 0..10u32 {
            mesh.add_triangle(0, k + 1, (k + 1) % 10 + 1);
        }
        painter.add(mesh);
        painter.add(egui::Shape::closed_line(
            points,
            Stroke::new(0.8_f32, colour),
        ));
    } else {
        painter.add(egui::Shape::closed_line(
            points,
            Stroke::new(1.2_f32, colour),
        ));
    }
}

/// The presets' previews drawn since last frame, kept as textures (any
/// they replace are freed next frame).
pub(crate) fn collect_preset_previews(app: &mut PainterApp, ctx: &egui::Context) {
    let bs = &mut app.brush_state;
    for (name, texture) in bs.preset_preview_worker.collect(ctx) {
        if let Some(old) = bs.preset_previews.insert(name, texture) {
            app.workspace.retired_textures.push(old);
        }
    }
}

/// Preset `index`'s preview, once drawn: the first time it's wanted, it's
/// asked for (drawn off the UI thread).
pub(crate) fn preset_preview(
    bs: &mut BrushState,
    index: usize,
    pool: &Arc<ThreadPool>,
    ctx: &egui::Context,
) -> Option<egui::TextureId> {
    let preset = bs.presets.get(index)?;
    if let Some(texture) = bs.preset_previews.get(&preset.name) {
        return Some(texture.id());
    }
    if !bs.preset_preview_worker.is_requested(&preset.name) {
        let look = PreviewLook {
            size: PREVIEW_PX,
            diameter: PREVIEW_DIAMETER,
            ink: PREVIEW_INK,
        };
        bs.preset_preview_worker
            .request(&preset.name, &preset.brush, look, pool, ctx);
    }
    None
}

/// Name prompt for saving the active tool's settings as a new preset.
fn save_preset_modal(app: &mut PainterApp, ctx: &egui::Context) {
    let bs = &mut app.brush_state;
    if !bs.show_new_preset_modal {
        return;
    }
    let warning_id = egui::Id::new(DUPLICATE_NAME_WARNING_ID);
    let mut save = None;
    egui::Window::new("Save Brush Preset")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show_fitted(ctx, |ui| {
            ui.label("Preset name");
            ui.text_edit_singleline(&mut bs.new_preset_name);
            if ctx.data(|d| d.get_temp::<bool>(warning_id).unwrap_or(false)) {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "A preset with this name already exists.",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    bs.show_new_preset_modal = false;
                    ctx.data_mut(|d| d.remove::<bool>(warning_id));
                }
                if ui.button("Save").clicked() {
                    let name = match bs.new_preset_name.trim() {
                        "" => "Untitled Brush".to_string(),
                        name => name.to_string(),
                    };
                    if bs.presets.iter().any(|p| p.name == name) {
                        ctx.data_mut(|d| d.insert_temp(warning_id, true));
                    } else {
                        save = Some(BrushPreset {
                            name,
                            brush: bs.brush.clone(),
                            file: None,
                        });
                        ctx.data_mut(|d| d.remove::<bool>(warning_id));
                        bs.show_new_preset_modal = false;
                    }
                }
            });
        });
    if let Some(preset) = save {
        let name = preset.name.clone();
        app.add_user_preset(preset);
        app.brush_state.active_preset = Some(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> PainterApp {
        let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
            64,
            64,
            Color32::WHITE,
            64,
        ));
        app.brush_state.presets = vec![BrushPreset {
            name: "Test brush".into(),
            brush: crate::brush_engine::brush::Brush::new(10.0, 80.0, Color32::BLACK, 10.0),
            file: None,
        }];
        app.brush_state
            .library
            .file
            .reconcile(&app.brush_state.presets);
        let folder = app.brush_state.library.file.folders[0].id;
        app.brush_state.library.shelf = Shelf::Folder(folder);
        app
    }

    fn frame(ctx: &egui::Context, app: &mut PainterApp, events: Vec<egui::Event>) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            rect = egui::Window::new("Brush Presets")
                .fit_screen_size(ctx)
                .default_pos(egui::pos2(20.0, 20.0))
                .default_size([320.0, 500.0])
                .min_size([200.0, 160.0])
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| presets_contents(app, ui))
                .unwrap()
                .response
                .rect;
        });
        rect
    }

    #[test]
    fn brush_window_can_shrink_vertically_and_keeps_the_requested_height() {
        let ctx = egui::Context::default();
        let mut app = app();
        frame(&ctx, &mut app, vec![]);
        let original = frame(&ctx, &mut app, vec![]);
        let from = original.center_bottom() - egui::vec2(0.0, 2.0);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(from)]);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
        );
        let to = from - egui::vec2(0.0, 170.0);
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(to)]);
        let resized = frame(
            &ctx,
            &mut app,
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert!(
            resized.height() < original.height() - 100.0,
            "before {original:?}, after {resized:?}"
        );
        for _ in 0..4 {
            let settled = frame(&ctx, &mut app, vec![]);
            assert!(
                (settled.height() - resized.height()).abs() < 2.0,
                "resized {resized:?}, settled {settled:?}"
            );
        }
    }

    #[test]
    fn favorite_star_has_its_own_hit_target_at_the_top_left() {
        let ctx = egui::Context::default();
        let mut rect = egui::Rect::NOTHING;
        let mut starred = false;
        for stage in 0..4 {
            let position = rect.min + egui::vec2(14.0, 14.0);
            let events = match stage {
                1 => vec![egui::Event::PointerMoved(position)],
                2 | 3 => vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: stage == 2,
                    modifiers: Default::default(),
                }],
                _ => vec![],
            };
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::Window::new("Star test")
                        .fixed_pos(egui::pos2(20.0, 20.0))
                        .default_width(300.0)
                        .collapsible(false)
                        .show(ctx, |ui| {
                            let (response, star) =
                                preset_tile(ui, "Test brush", None, 58.0, false, false);
                            rect = response.rect;
                            starred |= star;
                        });
                },
            );
        }
        assert!(
            starred,
            "clicking the top-left star toggles favorite status"
        );
    }
}
