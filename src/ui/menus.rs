//! The app menus (File, Edit, View, Help): the desktop menu bar, the
//! touch menu sheet, and the file/panel actions they and the shortcuts run.

use crate::PainterApp;
use crate::app::input::keymap::Action;
use crate::ui::bar_slider::BarSlider;
use crate::ui::style::*;
use crate::ui::widgets::bar_frame;
use eframe::egui::{self, Key, Modifiers, RichText};

/// As printed on this keyboard (see [`crate::app::input::keyboard`]).
fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    crate::app::input::keyboard::shortcut_label(ctx, modifiers, key)
}

/// The keys `action` has now (they can be changed), as printed on this
/// keyboard.
fn keys(app: &PainterApp, ctx: &egui::Context, action: Action) -> Option<String> {
    app.workspace.keymap.label(ctx, action)
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
    Image,
    Layer,
    Select,
    Filter,
    View,
    Help,
}

impl MenuSection {
    const ALL: [(MenuSection, &'static str); 8] = [
        (MenuSection::File, "File"),
        (MenuSection::Edit, "Edit"),
        (MenuSection::Image, "Image"),
        (MenuSection::Layer, "Layer"),
        (MenuSection::Select, "Select"),
        (MenuSection::Filter, "Filter"),
        (MenuSection::View, "View"),
        (MenuSection::Help, "Help"),
    ];

    fn items(self, app: &mut PainterApp, ui: &mut egui::Ui) {
        match self {
            MenuSection::File => file_menu(app, ui),
            MenuSection::Edit => edit_menu(app, ui),
            MenuSection::Image => {
                crate::ui::image_menu::image_menu(app, ui, |ui, label| menu_item(ui, label, None))
            }
            MenuSection::Layer => layer_menu(app, ui),
            MenuSection::Select => select_menu(app, ui),
            MenuSection::Filter => filter_menu(app, ui),
            MenuSection::View => view_menu(app, ui),
            MenuSection::Help => help_menu(app, ui),
        }
    }
}

/// The one bar above the canvas. Desktop: the menus, the active tool's
/// options, then the view controls on the right. Touch: the menu sheet
/// button and finger painting, then the view controls (tool options float
/// on the canvas instead).
pub fn top_bar(app: &mut PainterApp, ctx: &egui::Context) {
    let m = metrics(ctx);
    // A phone held upright can't fit it all on one row: the sliders and
    // the view toggles go on a second.
    let two_rows = m.touch && ctx.screen_rect().width() < TWO_ROW_WIDTH;
    // Too many tool options for one row (found the frame before): more.
    let rows_id = egui::Id::new("top_bar_option_rows");
    let rows = ctx.data(|d| d.get_temp::<u8>(rows_id)).unwrap_or(1);
    egui::TopBottomPanel::top("top_bar")
        .exact_height(m.menu_height * rows as f32)
        .frame(bar_frame(BG_PANEL))
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(7.0, 3.0);
                if m.touch {
                    crate::ui::status_bar::touch_buttons(app, ui, m.menu_height);
                } else {
                    // (Their commands need the canvas: they wait while
                    // strokes are still being painted.)
                    let settling = app.strokes_settling();
                    ui.add_enabled_ui(!settling, |ui| {
                        for (section, title) in MenuSection::ALL {
                            ui.menu_button(title, |ui| section.items(app, ui));
                        }
                    });
                }
                crate::ui::widgets::vdivider(ui);
                // The view controls go on the right; the tool options get
                // what's left (their width is measured the frame before).
                let right_id = ui.id().with("top_bar_right");
                let right: f32 = ui.data(|d| d.get_temp(right_id)).unwrap_or(0.0);
                let middle = (ui.available_width() - right - 8.0).max(0.0);
                let height = ui.available_height();
                ui.allocate_ui_with_layout(
                    egui::vec2(middle, height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if m.touch {
                            // On two rows the second has them.
                            if !two_rows {
                                let id = ui.id().with("top_bar_sliders");
                                crate::ui::bar_slider::fitted_row(
                                    ui,
                                    id,
                                    middle,
                                    m.menu_height,
                                    |ui| crate::ui::canvas_sliders::bar_sliders(app, ui, None),
                                );
                            }
                        } else {
                            let wanted = crate::ui::tool_options::options_inline(app, ui);
                            if wanted != rows {
                                ui.data_mut(|d| d.insert_temp(rows_id, wanted));
                                ui.ctx().request_repaint();
                            }
                        }
                    },
                );
                let right = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if m.touch {
                            let size = m.menu_height - 4.0;
                            crate::app::layout::dropdown_buttons(app, ui, size);
                            crate::ui::widgets::vdivider(ui);
                        }
                        crate::ui::frame_times::status_readout(app, ui);
                        if two_rows {
                            // Pinching zooms: Fit is the one view control
                            // a phone needs at hand.
                            crate::ui::status_bar::fit_button(app, ui);
                        } else {
                            crate::ui::status_bar::view_controls(app, ui);
                        }
                        ui.min_rect().width()
                    })
                    .inner;
                ui.data_mut(|d| d.insert_temp(right_id, right));
            });
        });
    if two_rows {
        egui::TopBottomPanel::top("top_bar_sliders")
            .exact_height(m.menu_height)
            .frame(bar_frame(BG_PANEL))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().button_padding = egui::vec2(7.0, 3.0);
                    // The two sliders share what the toggles and their
                    // number boxes leave.
                    let toggles = 2.0 * (m.menu_height + ui.spacing().item_spacing.x)
                        + if app.viewport.rotation.rem_euclid(std::f32::consts::TAU) > 1e-3 {
                            ROTATION_WIDTH
                        } else {
                            0.0
                        };
                    let numbers = 2.0 * (SLIDER_NUMBER_WIDTH + 2.0 * ui.spacing().item_spacing.x);
                    let width = ((ui.available_width() - toggles - numbers) / 2.0).max(40.0);
                    crate::ui::canvas_sliders::bar_sliders(app, ui, Some(width));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        crate::ui::status_bar::view_toggles(app, ui);
                        crate::ui::status_bar::rotation_reset(app, ui);
                    });
                });
            });
    }
}

/// Narrower touch screens (in points) get the top bar on two rows.
const TWO_ROW_WIDTH: f32 = 840.0;
/// Room for the rotation's reset button and angle.
const ROTATION_WIDTH: f32 = 100.0;
/// Room for a slider's number box ("100%", "1000 px").
const SLIDER_NUMBER_WIDTH: f32 = 64.0;

fn file_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    if menu_item(ui, "New Canvas…", keys(app, ctx, Action::NewCanvas)) {
        open_new_canvas_dialog(app);
    }
    if menu_item(ui, "Projects…", None) {
        app.open_library();
    }
    if menu_item(ui, "Open…", keys(app, ctx, Action::Open)) {
        open_project(app);
    }
    if menu_item(ui, "Save", keys(app, ctx, Action::Save)) {
        save_project(app);
    }
    if menu_item(ui, "Save As…", None) {
        save_project_as(app);
    }
    // Android's Save As goes to the library already.
    if !cfg!(target_os = "android") && menu_item(ui, "Save to Library…", None) {
        app.workspace
            .library
            .ask_save_name(&app.modal_state.new_canvas.name);
    }
    if cfg!(target_os = "android") && menu_item(ui, "Export Project File…", None) {
        export_project_file(app);
    }
    ui.separator();
    if menu_item(ui, "Import Image…", keys(app, ctx, Action::Import)) {
        crate::app::import::import_image_dialog(app);
    }
    if menu_item(ui, "Export Image…", keys(app, ctx, Action::Export)) {
        open_export_dialog(app);
    }
    let animated = app.canvas.is_animated() || app.canvas.layers.iter().any(|l| l.rig.is_some());
    if ui
        .add_enabled_ui(animated, |ui| menu_item(ui, "Export Animation…", None))
        .inner
    {
        crate::ui::timeline::pick_export(app);
    }
    #[cfg(not(target_os = "android"))]
    {
        if menu_item(ui, "Import Animation (Spine, DragonBones, Lottie)…", None) {
            crate::ui::timeline::pick_animation(app);
        }
        if menu_item(ui, "Import Video as Frames…", None) {
            crate::ui::timeline::pick_video(app);
        }
    }
    ui.separator();
    let recording = app.workspace.timelapse.recording;
    let title = if recording {
        "Time-lapse (recording)"
    } else {
        "Time-lapse"
    };
    ui.menu_button(title, |ui| {
        let mut on = recording;
        if ui
            .checkbox(&mut on, "Record")
            .on_hover_text("Keep a frame after each change, to export as a video of the painting")
            .changed()
        {
            app.set_timelapse_recording(on);
        }
        let frames = app.workspace.timelapse.frame_count();
        if ui
            .add_enabled_ui(frames > 1, |ui| {
                menu_item(ui, &format!("Export… ({frames} frames)"), None)
            })
            .inner
        {
            export_timelapse_dialog(app);
        }
        if ui
            .add_enabled_ui(frames > 0, |ui| menu_item(ui, "Clear", None))
            .inner
        {
            app.clear_timelapse();
        }
    });
    ui.separator();
    if menu_item(ui, "Settings…", None) {
        app.modal_state.show_general_settings = true;
    }
}

fn edit_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    if menu_item(ui, "Undo", keys(app, ctx, Action::Undo)) {
        app.apply_history(false);
    }
    if menu_item(ui, "Redo", keys(app, ctx, Action::Redo)) {
        app.apply_history(true);
    }
    if menu_item(ui, "History…", keys(app, ctx, Action::History)) {
        app.modal_state.show_history = true;
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
    if menu_item(
        ui,
        "Delete Selected Pixels",
        keys(app, ctx, Action::DeletePixels),
    ) {
        app.delete_selection_contents();
    }
    if menu_item(
        ui,
        "Content-Aware Fill",
        keys(app, ctx, Action::ContentFill),
    ) {
        app.content_aware_fill();
    }
    ui.separator();
    if menu_item(ui, "Define Brush Tip from Selection", None) {
        let ctx = ui.ctx().clone();
        app.export_state.message = Some(match app.define_tip_from_selection(&ctx) {
            Ok(name) => format!("New brush tip: {name} (the brush uses it now)"),
            Err(err) => err,
        });
    }
    if menu_item(ui, "Palette…", None) {
        app.workspace.palette.open = true;
    }
    if menu_item(ui, "Swap Colors", Some("X".into())) {
        app.swap_colors();
    }
    ui.separator();
    if menu_item(ui, "Clear Canvas (not undoable)", None) {
        app.clear_canvas();
    }
}

fn layer_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    if menu_item(ui, "New Layer", keys(app, ctx, Action::NewLayer)) {
        app.add_layer_and_select();
    }
    if menu_item(ui, "New Folder", keys(app, ctx, Action::NewFolder)) {
        app.add_folder();
    }
    if menu_item(ui, "New Vector Layer", None) {
        app.add_vector_layer();
    }
    ui.menu_button("New Adjustment Layer", |ui| {
        for filter in crate::canvas::filters::Filter::ADJUSTMENTS {
            if menu_item(ui, filter.name(), None) {
                app.add_adjustment_layer(filter);
            }
        }
    });
    ui.menu_button("New Fill Layer", |ui| {
        if menu_item(ui, "Colour", None) {
            app.add_colour_fill_layer();
        }
        if menu_item(ui, "Gradient", None) {
            app.add_gradient_fill_layer();
        }
    });
    let active = app.canvas.layers.get(app.canvas.active_layer_idx);
    let can_border = active.is_some_and(|l| {
        l.kind == crate::canvas::storage::LayerKind::Paint
            && l.adjustment.is_none()
            && l.style.fill.is_none()
    });
    if ui
        .add_enabled_ui(can_border, |ui| menu_item(ui, "Border…", None))
        .inner
    {
        app.workspace.filter.border_editing = app.canvas.layer_id_at(app.canvas.active_layer_idx);
    }
    let active = app.canvas.active_layer_idx;
    ui.add_enabled_ui(app.is_vector_layer(active), |ui| {
        ui.menu_button("Vector", |ui| {
            if menu_item(ui, "Edit Lines", None) {
                app.active_tool = crate::app::tools::Tool::VectorEdit;
            }
            if menu_item(ui, "Line Width…", None) {
                app.line_width_open(active);
            }
            if menu_item(ui, "Recolour Lines (brush colour)", None) {
                app.recolour_vector_lines(active);
            }
            if menu_item(ui, "Rasterise Vector Layer", None) {
                app.rasterise_vector_layer(active);
            }
        })
    })
    .response
    .on_disabled_hover_text("For vector layers (Layer → New Vector Layer)");
    let shown = app.canvas.layers.get(active).map(|l| l.shown_style());
    let lit = shown.and_then(|s| s.impasto);
    // A lightness map (Krita's colour smudge with a lightness tip): no
    // light to set, but it bakes the same.
    let relief = shown.is_some_and(|s| s.lightness_map);
    ui.add_enabled_ui(lit.is_some() || relief, |ui| {
        ui.menu_button("Impasto", |ui| {
            if let Some(mut light) = lit {
                let mut changed = false;
                for (label, value, range) in [
                    ("Light from", &mut light.angle, 0.0..=360.0),
                    ("Light height", &mut light.elevation, 5.0..=90.0),
                ] {
                    changed |= ui
                        .add(BarSlider::new(value, range).suffix("°").text(label))
                        .changed();
                }
                changed |= ui
                    .add(BarSlider::new(&mut light.strength, 0.0..=2.0).text("Strength"))
                    .changed();
                changed |= ui
                    .add(BarSlider::new(&mut light.gloss, 0.0..=1.0).text("Gloss"))
                    .changed();
                if changed {
                    app.canvas_mut().layers[active].style.impasto = Some(light);
                    app.mark_all_tiles_dirty();
                }
            }
            if menu_item(ui, "Bake Impasto", None) {
                app.bake_impasto(active);
            }
        })
    })
    .response
    .on_disabled_hover_text("For layers painted with impasto (Brush → Impasto)");
    ui.menu_button("Wet Paint", |ui| {
        let wet = app
            .canvas
            .layers
            .get(active)
            .and_then(|l| l.wet.as_ref())
            .is_some_and(|w| !w.is_empty());
        if ui
            .add_enabled_ui(wet, |ui| menu_item(ui, "Dry Paint Now", None))
            .inner
        {
            app.dry_wet_paint(active);
        }
        // Which way drips run: 0° down the canvas.
        let [x, y] = app.workspace.wet_gravity;
        let mut angle = x.atan2(y).to_degrees();
        if ui
            .add(
                BarSlider::new(&mut angle, -180.0..=180.0)
                    .suffix("°")
                    .text("Drips run"),
            )
            .on_hover_text("Which way pooled wet paint runs: 0° straight down the canvas.")
            .changed()
        {
            let (s, c) = angle.to_radians().sin_cos();
            app.workspace.wet_gravity = [s, c];
        }
    });
    if menu_item(
        ui,
        "Duplicate Layer",
        keys(app, ctx, Action::DuplicateLayer),
    ) {
        app.duplicate_layer();
    }
    ui.separator();
    if menu_item(ui, "Add Layer Mask", None) {
        app.add_mask_to_active();
    }
    if menu_item(
        ui,
        "Clip to Layer Below",
        keys(app, ctx, Action::ClipToBelow),
    ) {
        app.toggle_clip_active();
    }
    let active = app.canvas.active_layer_idx;
    let is_text = app.is_text_layer(active);
    if ui
        .add_enabled_ui(is_text, |ui| menu_item(ui, "Rasterise Text", None))
        .inner
    {
        app.rasterise_text_layer(active);
    }
    ui.separator();
    if menu_item(ui, "Merge Down", keys(app, ctx, Action::MergeDown)) {
        app.merge_down();
    }
    if menu_item(ui, "Merge Visible", keys(app, ctx, Action::MergeVisible)) {
        app.merge_visible();
    }
    if menu_item(ui, "Flatten Image", None) {
        app.flatten_image();
    }
}

fn select_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    if menu_item(ui, "Select All", keys(app, ctx, Action::SelectAll)) {
        app.select_all();
    }
    if menu_item(ui, "Deselect", keys(app, ctx, Action::Deselect)) {
        app.deselect();
    }
    if menu_item(
        ui,
        "Invert Selection",
        keys(app, ctx, Action::InvertSelection),
    ) {
        app.invert_selection();
    }
    crate::ui::select_dialog::select_menu_items(app, ui, menu_item);
}

/// Filters on the active layer (inside the selection, if any).
fn filter_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::canvas::filters::Filter;
    // One submenu a group: all of them in one list runs off the screen.
    let items = |app: &mut PainterApp, ui: &mut egui::Ui, group: &[Filter]| {
        for filter in group {
            let label = if filter.has_settings() {
                format!("{}…", filter.name())
            } else {
                filter.name().to_string()
            };
            if menu_item(ui, &label, None) {
                app.filter_open(*filter);
            }
        }
    };
    for (group, name) in Filter::MENU.iter().zip(Filter::MENU_GROUPS) {
        if group.len() == 1 {
            ui.separator();
            items(app, ui, group);
        } else {
            ui.menu_button(name, |ui| items(app, ui, group));
        }
    }
}

fn view_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let ctx = &ui.ctx().clone();
    if menu_item(ui, "Zoom In", keys(app, ctx, Action::ZoomIn)) {
        app.zoom_by_from_center(1.25);
    }
    if menu_item(ui, "Zoom Out", keys(app, ctx, Action::ZoomOut)) {
        app.zoom_by_from_center(0.8);
    }
    if menu_item(ui, "Fit to Window", keys(app, ctx, Action::FitView)) {
        app.fit_view();
    }
    if menu_item(ui, "Actual Pixels", keys(app, ctx, Action::ActualPixels)) {
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
    ui.checkbox(&mut app.workspace.quickshape.enabled, "QuickShape")
        .on_hover_text(
            "Hold the pen still at the end of a stroke to turn it into a clean line, \
             ellipse, rectangle or polygon, editable until you press elsewhere.",
        );
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
    ui.checkbox(&mut app.workspace.animation.show_timeline, "Timeline")
        .on_hover_text("Frames, playback and animated layers, under the canvas.");
    ui.menu_button("Colour Management", |ui| {
        crate::ui::image_menu::view_color_items(app, ui);
    });
    ui.separator();
    crate::ui::view_aids_menu::view_aids_items(app, ui, menu_item);
    ui.separator();
    if menu_item(ui, "Show / Hide Panels", Some("Tab".into())) {
        app.toggle_all_panels();
    }
    ui.checkbox(&mut app.workspace.show_left_panel, "Brush panel");
    ui.checkbox(&mut app.workspace.show_color, "Colour panel");
    ui.checkbox(&mut app.workspace.show_layers, "Layers panel");
    ui.checkbox(&mut app.workspace.touch_mode, "Touch mode");
    if app.workspace.touch_mode {
        ui.checkbox(&mut app.workspace.finger_painting, "Paint with one finger")
            .on_hover_text("When off, only a stylus paints and one finger pans the canvas.");
        ui.checkbox(
            &mut app.workspace.autohide_panels,
            "Hide side panels while painting",
        )
        .on_hover_text(
            "The tools and brush panel fade away; bring the pen near the left edge to show them.",
        );
    }
    let stats = &mut app.workspace.frame_stats;
    if ui
        .checkbox(&mut stats.enabled, "Frame times")
        .on_hover_text("Frames per second and time per frame, top right")
        .changed()
    {
        stats.window_open = stats.enabled;
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
    // Everything below the top bar.
    let area = ctx.available_rect();
    let width = area.width();
    let columns = (width - 16.0) >= SHEET_COLUMN_WIDTH * MenuSection::ALL.len() as f32;
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
                .fill(if selected { accent() } else { BG_RAISED })
                .min_size(egui::vec2(width, 40.0));
            if ui.add(button).clicked() {
                app.modal_state.menu_sheet_section = section;
            }
        }
    });
}

pub(crate) fn toggle_panels(app: &mut PainterApp) {
    app.toggle_all_panels();
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
    app.pick_open(crate::app::files::OpenFor::Document);
}

/// Save back to the document's file (in the background), or ask for one.
pub(crate) fn save_project(app: &mut PainterApp) {
    match app.workspace.library.project.clone() {
        Some(path) => app.save_project_in_background(path),
        None => save_project_as(app),
    }
}

/// Ask where to save (the dialog doesn't hold up the window), then save
/// there in the background.
#[cfg(not(target_os = "android"))]
pub(crate) fn save_project_as(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog()
        .add_filter("Rusty Painter", &["rpainter"])
        .set_file_name("project.rpainter");
    app.file_dialog_job(dialog, crate::app::jobs::Pick::Save, |app, paths| {
        let path = crate::project::with_project_extension(&paths[0]);
        app.save_project_in_background(&path);
        app.workspace.library.project = Some(path);
    });
}

/// No file system to save to on Android: into the library.
#[cfg(target_os = "android")]
pub(crate) fn save_project_as(app: &mut PainterApp) {
    let name = app.modal_state.new_canvas.name.clone();
    app.workspace.library.ask_save_name(&name);
}

/// The document as a project file, saved where the user picks (Android:
/// to share or back up outside the app).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn export_project_file(app: &mut PainterApp) {
    let name = app.workspace.library.project.as_ref().map_or_else(
        || format!("{}.rpainter", app.modal_state.new_canvas.name),
        |p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        },
    );
    match app.encode_document() {
        Ok(bytes) => app.pick_save(&name, "rpainter", "application/octet-stream", bytes),
        Err(err) => app.report(err),
    }
}

#[cfg(not(target_os = "android"))]
fn export_timelapse_dialog(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog()
        .add_filter("Video (MP4)", &["mp4"])
        .add_filter("Animated GIF", &["gif"])
        .set_file_name("timelapse.mp4");
    app.file_dialog_job(dialog, crate::app::jobs::Pick::Save, |app, paths| {
        app.export_timelapse(paths[0].clone());
    });
}

/// Android: a GIF (no ffmpeg there), moved to Pictures once written.
#[cfg(target_os = "android")]
fn export_timelapse_dialog(app: &mut PainterApp) {
    app.export_timelapse(crate::android::cache_dir().join("timelapse.gif"));
}

#[cfg(test)]
mod top_bar_tests {
    use super::*;
    use crate::app::tools::{Tool, shape::ShapeKind};
    use crate::selection::SelectionType;

    /// However many options a tool has, they stay left of the view controls
    /// and no bar sizes itself to the scroll area's endless width.
    #[test]
    fn tool_options_fit_the_top_bar() {
        let tools = [
            Tool::Brush,
            Tool::Smudge,
            Tool::Blur,
            Tool::Fill,
            Tool::Liquify,
            Tool::Gradient,
            Tool::Text,
            Tool::Eyedropper,
            Tool::Animate,
            Tool::VectorEdit,
            Tool::Select(SelectionType::Wand),
            Tool::Select(SelectionType::Brush),
            Tool::Select(SelectionType::Magnetic),
            Tool::Select(SelectionType::ColorRange),
            Tool::Shape(ShapeKind::Rectangle),
            Tool::Shape(ShapeKind::Curve),
        ];
        for width in [900.0, 1400.0] {
            for tool in tools {
                let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
                    64,
                    64,
                    egui::Color32::WHITE,
                    64,
                ));
                app.workspace.library.open = false;
                app.active_tool = tool;
                let ctx = egui::Context::default();
                crate::ui::theme::apply_global_style(&ctx);
                let mut widest = 0.0_f32;
                for _ in 0..8 {
                    crate::ui::bar_slider::DRAWN.with(|d| d.borrow_mut().clear());
                    let _ = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 800.0),
                            )),
                            ..Default::default()
                        },
                        |ctx| top_bar(&mut app, ctx),
                    );
                    // A bar cut off by the edge of the row it's in.
                    widest = crate::ui::bar_slider::DRAWN.with(|d| {
                        d.borrow()
                            .iter()
                            .filter(|(_, r, c)| r.right() > c.right() + 0.5)
                            .map(|(_, r, _)| r.right())
                            .fold(0.0, f32::max)
                    });
                }
                assert_eq!(
                    widest, 0.0,
                    "{tool:?} at {width}: a bar is cut off at {widest}"
                );
            }
        }
    }
}
