//! Floating brush presets window (toolbar button or `P`): one column of wide
//! stroke previews with the preset name inside, like Clip Studio's brush
//! list. Picking a preset loads it into the tool it belongs to: eraser
//! presets switch to the eraser, the others to the brush.
//!
//! Above the list, a search box (name or tag) and chips: all presets, the
//! favourites (starred on their tile or from their menu), the recent ones,
//! or one tag. A preset's right-click menu stars it and edits its tags.

use crate::PainterApp;
use crate::app::brush_library::{Shelf, shown_presets};
use crate::app::state::BrushState;
use crate::brush_engine::brush::BrushPreset;
use crate::brush_engine::brush_options::BlendMode;
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
const WINDOW_WIDTH: f32 = 280.0;

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

    ui.horizontal(|ui| {
        let tool = if app.brush_state.eraser_active {
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

    let all_tags = library_filters(&mut app.brush_state, ui);

    let tile_height = if m.touch { 76.0 } else { 58.0 };
    let pool = app.workspace.pool.clone();
    collect_preset_previews(app, ui.ctx());
    let eraser_first = app.brush_state.eraser_active;
    let bs = &mut app.brush_state;
    let lib = &bs.library;
    let shown = shown_presets(&bs.presets, &lib.file, &lib.shelf, &lib.search);
    let recent = lib.shelf == Shelf::Recent;
    egui::ScrollArea::vertical()
        .id_salt("brush_presets_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if shown.is_empty() {
                ui.label(RichText::new("No presets match").small().color(TEXT_DIM));
                return;
            }
            // The recent ones newest first, in one list.
            if recent {
                for &index in &shown {
                    let row = preset_row(ui, bs, index, &all_tags, &pool, tile_height);
                    picked = row.or(picked.take());
                }
                return;
            }
            // The active tool's presets first.
            let sections = if eraser_first {
                [true, false]
            } else {
                [false, true]
            };
            for erasers in sections {
                let section: Vec<usize> = (shown.iter().copied())
                    .filter(|&i| {
                        let mode = bs.presets[i].brush.brush_options.blend_mode;
                        (mode == BlendMode::Eraser) == erasers
                    })
                    .collect();
                if section.is_empty() {
                    continue;
                }
                ui.label(
                    RichText::new(if erasers { "ERASERS" } else { "BRUSHES" })
                        .small()
                        .strong()
                        .color(TEXT_DIM),
                );
                for index in section {
                    let row = preset_row(ui, bs, index, &all_tags, &pool, tile_height);
                    picked = row.or(picked.take());
                }
                ui.add_space(6.0);
            }
        });
    picked
}

/// The search box and the shelf chips. Returns every tag in use.
fn library_filters(bs: &mut BrushState, ui: &mut egui::Ui) -> Vec<String> {
    let lib = &mut bs.library;
    let all_tags = lib.file.all_tags();
    // A tag no preset has any more.
    if let Shelf::Tag(tag) = &lib.shelf
        && !all_tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    {
        lib.shelf = Shelf::All;
    }
    ui.add(
        egui::TextEdit::singleline(&mut lib.search)
            .hint_text("Search names and tags")
            .desired_width(f32::INFINITY),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        let mut chip = |ui: &mut egui::Ui, shelf: Shelf, text: &str| {
            let on = lib.shelf == shelf;
            if ui
                .selectable_label(on, RichText::new(text).small())
                .clicked()
            {
                lib.shelf = if on { Shelf::All } else { shelf };
            }
        };
        chip(ui, Shelf::All, "All");
        chip(ui, Shelf::Favourites, "Favourites");
        chip(ui, Shelf::Recent, "Recent");
        for tag in &all_tags {
            chip(ui, Shelf::Tag(tag.clone()), tag);
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
    let preset = &bs.presets[index];
    // Any brush can be the eraser's, so the name alone says which is in use.
    let active = bs.active_preset.as_deref() == Some(preset.name.as_str());
    let favourite = bs.library.file.is_favourite(&preset.name);
    let (tile, star) = preset_tile(ui, &preset.name, texture, tile_height, active, favourite);
    if star {
        picked = Some(PresetAction::ToggleFavourite(index));
    } else if tile.clicked() {
        picked = Some(PresetAction::Pick(index));
    }
    let default = PainterApp::is_default_preset(&preset.name);
    let tags = bs.library.file.tags(&preset.name);
    let new_tag = &mut bs.library.new_tag;
    tile.context_menu(|ui| {
        let star_text = if favourite {
            "Remove from favourites"
        } else {
            "Add to favourites"
        };
        if ui.button(star_text).clicked() {
            picked = Some(PresetAction::ToggleFavourite(index));
            ui.close_menu();
        }
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
        if default && ui.button("Reset to default").clicked() {
            picked = Some(PresetAction::Reset(index));
        }
        if ui.button("Delete").clicked() {
            picked = Some(PresetAction::Delete(index));
        }
    });
    picked
}

/// A wide preview tile with the name drawn inside it, and a star in the
/// top-right corner. Returns the tile and whether the star was clicked.
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
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        0.0,
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
        rect.min + egui::vec2(4.0, 4.0),
        galley.size() + egui::vec2(8.0, 4.0),
    );
    painter.rect_filled(chip, 0.0, Color32::from_black_alpha(150));
    painter.galley(chip.min + egui::vec2(4.0, 2.0), galley, TEXT_STRONG);

    let stroke = if active {
        Stroke::new(2.0_f32, ACCENT)
    } else if response.hovered() {
        Stroke::new(1.0_f32, TEXT_DIM)
    } else {
        Stroke::new(1.0_f32, BORDER)
    };
    painter.rect_stroke(rect, 0.0, stroke);

    // The star: always shown on a favourite, on hover otherwise.
    let star_size = (height * 0.3).clamp(14.0, 24.0);
    let star_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - star_size - 4.0, rect.top() + 4.0),
        egui::vec2(star_size, star_size),
    );
    let pointer = response.hover_pos();
    let over_star = pointer.is_some_and(|p| star_rect.contains(p));
    if favourite || response.hovered() {
        let colour = if favourite || over_star {
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
    let star_clicked = response.clicked()
        && (response.interact_pointer_pos()).is_some_and(|p| star_rect.contains(p));
    let tip = if over_star {
        if favourite {
            "Remove from favourites"
        } else {
            "Add to favourites"
        }
    } else {
        name
    };
    (response.on_hover_text(tip), star_clicked)
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
        // Convex pieces: the middle pentagon and the five points.
        let inner: Vec<egui::Pos2> = (0..5).map(|k| points[2 * k + 1]).collect();
        painter.add(egui::Shape::convex_polygon(inner, colour, Stroke::NONE));
        for k in 0..5 {
            let tip = vec![points[(2 * k + 9) % 10], points[2 * k], points[2 * k + 1]];
            painter.add(egui::Shape::convex_polygon(tip, colour, Stroke::NONE));
        }
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
        .fit_screen(ctx)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
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
