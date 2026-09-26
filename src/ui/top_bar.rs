use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::transform;
use crate::brush_engine::brush_options::PaintingMode;
use crate::selection::SelectionType;
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, percent_of_unit, segmented, vdivider};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, RichText};

fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    ctx.format_shortcut(&KeyboardShortcut::new(modifiers, key))
}

/// A menu entry with a right-aligned shortcut hint; closes the menu on click.
fn menu_item(ui: &mut egui::Ui, label: &str, hint: Option<String>) -> bool {
    let touch = metrics(ui.ctx()).touch;
    let min_height = if touch { 44.0 } else { 0.0 };
    let mut button = egui::Button::new(label).min_size(egui::vec2(220.0, min_height));
    // Keyboard shortcuts mean nothing without a keyboard.
    if let Some(hint) = hint.filter(|_| !touch) {
        button = button.shortcut_text(RichText::new(hint).color(TEXT_DIM));
    }
    let clicked = ui.add(button).clicked();
    if clicked {
        ui.close_menu();
    }
    clicked
}

fn bar_frame(fill: egui::Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(8.0, 0.0))
}

/// File / Edit / View / Help menus.
pub fn menu_bar(app: &mut PainterApp, ctx: &egui::Context) {
    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;

    egui::TopBottomPanel::top("menu_bar")
        .exact_height(metrics(ctx).menu_height)
        .frame(bar_frame(BG_CANVAS))
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);

                ui.menu_button("File", |ui| {
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
                });

                ui.menu_button("Edit", |ui| {
                    if menu_item(ui, "Undo", Some(shortcut(ctx, cmd, Key::Z))) {
                        app.apply_history(false);
                    }
                    if menu_item(ui, "Redo", Some(shortcut(ctx, cmd_shift, Key::Z))) {
                        app.apply_history(true);
                    }
                    ui.separator();
                    if menu_item(ui, "New Layer", Some(shortcut(ctx, cmd_shift, Key::N))) {
                        app.add_layer_and_select();
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
                    if menu_item(ui, "Deselect", Some(shortcut(ctx, cmd, Key::D))) {
                        app.selection_manager.clear_selection();
                    }
                    if menu_item(ui, "Swap Colors", Some("X".into())) {
                        app.swap_colors();
                    }
                    ui.separator();
                    if menu_item(ui, "Clear Canvas (not undoable)", None) {
                        app.clear_canvas();
                    }
                });

                ui.menu_button("View", |ui| {
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
                    ui.separator();
                    if menu_item(ui, "Show / Hide Panels", Some("Tab".into())) {
                        toggle_all_panels(app);
                    }
                    ui.checkbox(&mut app.workspace.show_left_panel, "Brush panel");
                    ui.checkbox(&mut app.workspace.show_right_panel, "Color & layers panel");
                    ui.checkbox(&mut app.workspace.touch_mode, "Touch mode");
                    ui.separator();
                    if menu_item(ui, "Reset Panel Layout", None) {
                        app.dock_left = crate::app::layout::default_left_dock();
                        app.dock_right = crate::app::layout::default_right_dock();
                    }
                });

                ui.menu_button("Help", |ui| {
                    let label = if app.workspace.touch_mode {
                        "Gestures & Shortcuts"
                    } else {
                        "Keyboard Shortcuts"
                    };
                    if menu_item(ui, label, None) {
                        app.modal_state.show_shortcuts = true;
                    }
                });

                // Show/hide the side panels (also Tab on desktop).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    panel_toggles(app, ui);
                });
            });
        });
}

/// Context bar under the menus showing the active tool's options.
pub fn options_bar(app: &mut PainterApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("tool_options")
        .exact_height(metrics(ctx).bar_height)
        .frame(bar_frame(BG_PANEL))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // Desktop only (tablets use the canvas faders); hints fill the right.
                let hint = match app.active_tool {
                    Tool::Brush => brush_options(app, ui),
                    Tool::Select(kind) => select_options(app, ui, kind),
                    Tool::Transform(_) => transform_options(app, ui),
                    Tool::Eyedropper => eyedropper_options(app, ui),
                    Tool::Fill => fill_options(app, ui, true),
                    Tool::Liquify => liquify_options(app, ui, true),
                    Tool::Smudge | Tool::Blur => blend_options(app, ui),
                };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Hints are optional: drop them rather than overlap the options.
                    let font = egui::TextStyle::Small.resolve(ui.style());
                    let width = ui
                        .painter()
                        .layout_no_wrap(hint.into(), font, TEXT_DIM)
                        .size()
                        .x;
                    if width + 12.0 < ui.available_width() {
                        ui.label(RichText::new(hint).small().color(TEXT_DIM));
                    }
                });
            });
        });
}

fn tool_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).strong().color(TEXT_STRONG));
    vdivider(ui);
}

fn bar_slider(ui: &mut egui::Ui, label: &str, width: f32, slider: egui::Slider) -> bool {
    ui.label(RichText::new(label).color(TEXT_DIM));
    let scale = if metrics(ui.ctx()).touch { 1.4 } else { 1.0 };
    ui.spacing_mut().slider_width = width * scale;
    ui.add(slider).changed()
}

fn brush_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    let eraser = app.is_eraser_active();
    tool_title(ui, if eraser { "Eraser" } else { "Brush" });

    let brush = &mut app.brush_state.brush;
    let mut changed = false;
    let size_changed = bar_slider(
        ui,
        "Size",
        140.0,
        egui::Slider::new(&mut brush.brush_options.diameter, 1.0..=3000.0)
            .logarithmic(true)
            .max_decimals(0)
            .suffix(" px"),
    );
    ui.add_space(6.0);
    changed |= bar_slider(
        ui,
        "Opacity",
        100.0,
        percent_of_unit(egui::Slider::new(
            &mut brush.brush_options.opacity,
            0.0..=1.0,
        )),
    );
    ui.add_space(6.0);
    changed |= bar_slider(
        ui,
        "Flow",
        100.0,
        egui::Slider::new(&mut brush.brush_options.flow, 0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    vdivider(ui);
    changed |= segmented(
        ui,
        &mut brush.brush_options.painting_mode,
        &[
            (PaintingMode::BuildUp, "Build-up"),
            (PaintingMode::Wash, "Wash"),
        ],
        true,
    );

    if size_changed {
        brush.is_changed = true;
    }
    // Size doesn't change the (fixed-size) preview.
    if changed {
        app.brush_state.brush_preview.dirty = true;
    }

    "[ ] size  ·  Alt+click pick color  ·  Space drag to pan"
}

/// Smudge / Blur: the brush's size, strength (flow) and softness, plus the
/// tool's own setting. The rest (spacing, pressure...) is in the brush panel.
fn blend_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    let smudge = matches!(app.active_tool, Tool::Smudge);
    tool_title(ui, if smudge { "Smudge" } else { "Blur" });
    let o = &mut app.brush_state.brush.brush_options;
    let size_changed = bar_slider(
        ui,
        "Size",
        140.0,
        egui::Slider::new(&mut o.diameter, 1.0..=3000.0)
            .logarithmic(true)
            .max_decimals(0)
            .suffix(" px"),
    );
    ui.add_space(6.0);
    bar_slider(
        ui,
        "Strength",
        100.0,
        egui::Slider::new(&mut o.flow, 0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    ui.add_space(6.0);
    bar_slider(
        ui,
        "Hardness",
        100.0,
        egui::Slider::new(&mut o.hardness, 0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    vdivider(ui);
    let b = &mut app.workspace.blend;
    if smudge {
        bar_slider(
            ui,
            "Length",
            100.0,
            percent_of_unit(egui::Slider::new(&mut b.smudge_length, 0.0..=1.0)),
        );
    } else {
        bar_slider(
            ui,
            "Blur size",
            100.0,
            percent_of_unit(egui::Slider::new(&mut b.blur_size, 0.05..=1.0)),
        );
    }
    if size_changed {
        app.brush_state.brush.is_changed = true;
    }
    if smudge {
        "Drag to smear the paint  ·  Length: how far colour is carried  ·  uses the brush's spacing & pressure"
    } else {
        "Paint over edges to soften them  ·  uses the brush's spacing & pressure"
    }
}

fn select_options(app: &mut PainterApp, ui: &mut egui::Ui, kind: SelectionType) -> &'static str {
    tool_title(ui, "Selection");
    let mut kind_value = kind;
    if segmented(
        ui,
        &mut kind_value,
        &[
            (SelectionType::Rectangle, "Rectangle"),
            (SelectionType::Circle, "Ellipse"),
            (SelectionType::Lasso, "Lasso"),
            (SelectionType::Brush, "Brush"),
        ],
        true,
    ) {
        app.set_select_tool(kind_value);
    }
    vdivider(ui);
    crate::ui::select_menu::mode_and_brush_controls(app, ui, true);
    vdivider(ui);
    crate::ui::select_menu::selection_actions(app, ui);
    "Shift add  ·  Alt subtract  ·  Ctrl+A all  ·  Ctrl+Shift+I invert"
}

fn transform_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    tool_title(ui, "Transform");
    transform_controls(app, ui);
    "Drag inside to move  ·  outside to rotate  ·  Shift keeps proportions  ·  Enter apply  ·  Esc cancel"
}

/// Mode, flips, quarter turns, apply and cancel. Shared with the touch bar.
pub(crate) fn transform_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let distort = matches!(app.active_tool, Tool::Transform(info) if info.corners.is_some());
    let mut mode = distort;
    segmented(ui, &mut mode, &[(false, "Free"), (true, "Distort")], true);
    if mode != distort {
        transform::set_distort(app, mode);
    }
    vdivider(ui);
    if ui
        .button("Flip H")
        .on_hover_text("Mirror horizontally")
        .clicked()
    {
        transform::flip(app, true);
    }
    if ui
        .button("Flip V")
        .on_hover_text("Mirror vertically")
        .clicked()
    {
        transform::flip(app, false);
    }
    if ui
        .button("−90°")
        .on_hover_text("Rotate 90° counter-clockwise")
        .clicked()
    {
        transform::rotate_quarter(app, false);
    }
    if ui
        .button("+90°")
        .on_hover_text("Rotate 90° clockwise")
        .clicked()
    {
        transform::rotate_quarter(app, true);
    }
    vdivider(ui);
    ui.checkbox(&mut app.workspace.transform_pick_layer, "Pick layer")
        .on_hover_text(
            "Click an image or drawing on another (unlocked) layer to select and transform it",
        );
    vdivider(ui);
    let floating = app.layer_state.floating_layer_idx.is_some();
    if ui
        .add_enabled(floating, egui::Button::new("Apply"))
        .on_hover_text("Apply the transform (Enter)")
        .clicked()
    {
        transform::commit_floating_layer(app);
    }
    if ui
        .add_enabled(floating, egui::Button::new("Cancel"))
        .on_hover_text("Put everything back (Esc)")
        .clicked()
    {
        transform::cancel_floating_layer(app);
    }
}

/// Fill mode, reference and line handling. `compact` lays it out in a row
/// (options bar); otherwise stacked (tool panel).
pub(crate) fn fill_options(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) -> &'static str {
    use crate::app::fill_tool::{FillMode, FillSource};
    if compact {
        tool_title(ui, "Fill");
    }
    let f = &mut app.workspace.fill;
    segmented(
        ui,
        &mut f.mode,
        &[(FillMode::Bucket, "Bucket"), (FillMode::Enclose, "Enclose")],
        compact,
    );
    if compact {
        vdivider(ui);
        ui.label(RichText::new("Look at").color(TEXT_DIM));
    }
    segmented(
        ui,
        &mut f.source,
        &[
            (FillSource::CurrentLayer, "Layer"),
            (FillSource::LayerBelow, "Below"),
            (FillSource::AllVisible, "All"),
        ],
        compact,
    );
    let s = &mut f.settings;
    let rows: [(&str, &mut u8, std::ops::RangeInclusive<u8>, &str); 3] = [
        (
            "Tolerance",
            &mut s.tolerance,
            0..=255,
            "How different a colour can be and still count as the same area",
        ),
        (
            "Close gaps",
            &mut s.gap,
            0..=40,
            "Don't leak through openings in the lines up to this wide (px)",
        ),
        (
            "Under lines",
            &mut s.expand,
            0..=12,
            "Grow the fill under the line art (px), so no halo is left",
        ),
    ];
    for (label, value, range, tip) in rows {
        if compact {
            vdivider(ui);
            ui.label(RichText::new(label).color(TEXT_DIM))
                .on_hover_text(tip);
            ui.add(egui::Slider::new(value, range).show_value(true))
                .on_hover_text(tip);
        } else {
            crate::ui::widgets::slider_row(ui, label, egui::Slider::new(value, range))
                .on_hover_text(tip);
        }
    }
    if compact {
        vdivider(ui);
    }
    ui.checkbox(&mut s.antialias, "Smooth edge");
    match f.mode {
        FillMode::Bucket => "Click an area to fill it  ·  G toggles Enclose",
        FillMode::Enclose => {
            "Draw around the areas to fill, even across lines  ·  G toggles Bucket"
        }
    }
}

/// Liquify mode, size, strength, apply and cancel.
pub(crate) fn liquify_options(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    compact: bool,
) -> &'static str {
    use crate::canvas::liquify::LiquifyMode;
    if compact {
        tool_title(ui, "Liquify");
    }
    let s = &mut app.workspace.liquify;
    if compact {
        let label = LiquifyMode::ALL
            .iter()
            .find(|m| m.0 == s.mode)
            .map_or("", |m| m.1);
        egui::ComboBox::from_id_salt("liquify_mode")
            .selected_text(label)
            .show_ui(ui, |ui| {
                for (mode, name) in LiquifyMode::ALL {
                    ui.selectable_value(&mut s.mode, mode, name);
                }
            });
        vdivider(ui);
        ui.label(RichText::new("Size").color(TEXT_DIM));
        ui.add(
            egui::Slider::new(&mut s.radius, 4.0..=600.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px"),
        );
        ui.label(RichText::new("Strength").color(TEXT_DIM));
        ui.add(percent_of_unit(egui::Slider::new(
            &mut s.strength,
            0.02..=1.0,
        )));
        vdivider(ui);
    } else {
        ui.horizontal_wrapped(|ui| {
            for (mode, name) in LiquifyMode::ALL {
                ui.selectable_value(&mut s.mode, mode, name);
            }
        });
        crate::ui::widgets::slider_row(
            ui,
            "Size",
            egui::Slider::new(&mut s.radius, 4.0..=600.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px"),
        );
        crate::ui::widgets::slider_row(
            ui,
            "Strength",
            percent_of_unit(egui::Slider::new(&mut s.strength, 0.02..=1.0)),
        );
    }
    let active = app.layer_state.liquify.is_some();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(active, egui::Button::new("Apply"))
            .on_hover_text("Keep the result (Enter)")
            .clicked()
        {
            app.liquify_commit();
        }
        if ui
            .add_enabled(active, egui::Button::new("Cancel"))
            .on_hover_text("Put the layer back (Esc)")
            .clicked()
        {
            app.liquify_cancel();
        }
    });
    "Drag to push  ·  hold still to twirl / pinch / bloat  ·  [ ] size  ·  Enter apply  ·  Esc cancel"
}

fn eyedropper_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    tool_title(ui, "Eyedropper");
    let color = app.brush_state.brush.brush_options.color;
    crate::ui::widgets::color_swatch(ui, color, egui::vec2(36.0, 18.0));
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    ui.label(
        RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
            .monospace()
            .color(TEXT),
    );
    "Click or drag on the canvas to sample the visible color"
}

fn toggle_all_panels(app: &mut PainterApp) {
    let ws = &mut app.workspace;
    let show = !(ws.show_left_panel || ws.show_right_panel);
    ws.show_left_panel = show;
    ws.show_right_panel = show;
}

/// Buttons that show/hide the side docks (more canvas on small screens).
fn panel_toggles(app: &mut PainterApp, ui: &mut egui::Ui) {
    let size = (metrics(ui.ctx()).menu_height - 4.0).min(40.0);
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
