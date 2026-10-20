//! The Palette window: choose, arrange and organise palettes, open and
//! save other apps' palette files, make ramps, turn the picture into a
//! palette, and recolour a layer with it. And the palette's grid in the
//! colour panel.

use crate::PainterApp;
use crate::app::tools::palette::RampKind;
use crate::app::tools::palettes::{Palettes, Slot, Swatch, Which};
use crate::canvas::palette::SortKey;
use crate::project::palette_files::Format;
use crate::ui::bar_slider::BarSlider;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use crate::ui::widgets::{color_swatch, paint_swatch, segmented};
use eframe::egui::{self, Color32, RichText, Sense, Stroke};

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(TEXT_DIM));
}

pub fn palette_window(app: &mut PainterApp, ctx: &egui::Context) {
    let mut open = app.workspace.palette.open;
    if !open {
        return;
    }
    let touch = metrics(ctx).touch;
    let swatch = if touch { 34.0 } else { 24.0 };
    let response = egui::Window::new("Palette")
        .fit_screen(ctx)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(300.0)
        // Tall enough for all of it, where the screen is (it scrolls
        // where not).
        .default_height(640.0)
        .show(ctx, |ui| {
            palettes_section(app, ui);
            ui.separator();
            picture_section(app, ui, swatch);
            ui.separator();
            ramp_section(app, ui, swatch);
            ui.separator();
            recolour_section(app, ui);
        });
    app.workspace.palette.window_rect = response.map(|r| r.response.rect);
    app.workspace.palette.open = open;
}

/// Which palette, and what to do with it.
fn palettes_section(app: &mut PainterApp, ui: &mut egui::Ui) {
    heading(ui, "PALETTE");
    let palettes = &mut app.brush_state.palettes;
    palette_choice(ui, palettes, ui.available_width());
    let which = palettes.active_which();
    let state = &mut app.workspace.palette;

    if let Some(name) = state.renaming.as_mut() {
        let (mut done, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(name).desired_width(180.0));
            r.request_focus();
            done = ui.button("OK").clicked()
                || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            cancel =
                ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        if cancel {
            state.renaming = None;
        }
        if done && let Some(name) = state.renaming.take() {
            let name = name.trim();
            if !name.is_empty()
                && let Some(p) = palettes.edit(which)
            {
                p.name = name.to_string();
            }
        }
    }

    ui.horizontal_wrapped(|ui| {
        if ui
            .button("New")
            .on_hover_text("A new, empty palette")
            .clicked()
        {
            palettes.add(
                crate::app::tools::palettes::Palette::new("Palette", &[]),
                false,
            );
        }
        ui.menu_button("Presets", |ui| {
            let presets = crate::project::palette_files::presets();
            let mut categories: Vec<&str> = Vec::new();
            for (c, _) in presets {
                if !categories.contains(&c.as_str()) {
                    categories.push(c);
                }
            }
            for category in categories {
                ui.menu_button(category, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(400.0)
                        .show(ui, |ui| {
                            for (_, p) in presets.iter().filter(|(c, _)| c == category) {
                                let label = format!("{} ({})", p.name, p.len());
                                if ui.button(label).clicked() {
                                    palettes.add(p.clone(), false);
                                    ui.close_menu();
                                }
                            }
                        });
                });
            }
        })
        .response
        .on_hover_text("Add a copy of a palette that comes with the app");
        if ui.button("Duplicate").clicked() {
            let mut copy = palettes.active().clone();
            copy.name = format!("{} copy", copy.name);
            palettes.add(copy, matches!(which, Which::Document(_)));
        }
        if ui.button("Rename").clicked() {
            state.renaming = Some(palettes.active().name.clone());
        }
        ui.menu_button("Delete", |ui| {
            ui.label(format!("Delete “{}”?", palettes.active().name));
            if ui.button("Delete it").clicked() {
                palettes.remove(which);
                ui.close_menu();
            }
        });
    });
    let mut in_document = matches!(which, Which::Document(_));
    if ui
        .checkbox(&mut in_document, "Keep in this document")
        .on_hover_text("Saved in the project file, instead of with your palettes")
        .changed()
    {
        palettes.move_to(which, in_document);
    }
    let which = palettes.active_which();

    let mut columns = palettes.active().columns();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Columns").color(TEXT_DIM));
        if ui
            .add(BarSlider::new(
                &mut columns,
                1..=crate::app::tools::palettes::MAX_COLUMNS,
            ))
            .changed()
            && let Some(p) = palettes.edit(which)
        {
            p.columns = columns;
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Sort").color(TEXT_DIM));
        let mut sort = |ui: &mut egui::Ui, label: &str, tip: &str, key: Option<SortKey>| {
            if ui.button(label).on_hover_text(tip).clicked()
                && let Some(p) = palettes.edit(which)
            {
                match key {
                    Some(key) => p.sort(key),
                    None => p.arrange_by_hue(),
                }
            }
        };
        sort(
            ui,
            "Value",
            "Dark to light, gaps closed",
            Some(SortKey::Lightness),
        );
        sort(
            ui,
            "Hue",
            "Greys, then round the colour wheel",
            Some(SortKey::Hue),
        );
        sort(
            ui,
            "Saturation",
            "Greyest to most vivid",
            Some(SortKey::Chroma),
        );
        sort(ui, "Rows by hue", "A row for each hue, dark to light", None);
    });

    // Groups: the named sections below the first rows.
    let groups = palettes.active().groups.len();
    let mut remove = None;
    for g in 1..groups {
        ui.horizontal(|ui| {
            let mut name = palettes.active().groups[g].name.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut name).desired_width(160.0))
                .changed()
                && let Some(p) = palettes.edit(which)
            {
                p.groups[g].name = name;
            }
            if ui
                .small_button("×")
                .on_hover_text("Remove the group (its colours join the rows above)")
                .clicked()
            {
                remove = Some(g);
            }
        });
    }
    if let Some(g) = remove
        && let Some(p) = palettes.edit(which)
    {
        p.remove_group(g);
    }
    match state.new_group.as_mut() {
        Some(name) => {
            let (mut done, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("Group name")
                        .desired_width(160.0),
                );
                r.request_focus();
                done = ui.button("Add").clicked()
                    || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                cancel = ui.button("Cancel").clicked();
            });
            if cancel {
                state.new_group = None;
            }
            if done
                && let Some(name) = state.new_group.take()
                && let Some(p) = palettes.edit(which)
            {
                let name = name.trim();
                p.add_group(if name.is_empty() { "Group" } else { name });
            }
        }
        None => {
            if ui
                .button("Add group")
                .on_hover_text("A named section below, as Krita's palettes have")
                .clicked()
            {
                state.new_group = Some(String::new());
            }
        }
    }

    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Open palette file…")
            .on_hover_text("GIMP (.gpl), Adobe (.ase, .aco) or Krita (.kpl) palettes")
            .clicked()
        {
            app.pick_open(crate::app::files::OpenFor::PaletteFiles);
        }
        ui.menu_button("Save as…", |ui| {
            for format in Format::ALL {
                if ui.button(format.label()).clicked() {
                    app.export_palette(format);
                    ui.close_menu();
                }
            }
        });
    });
}

/// A row of colours to paint with (click).
fn strip(app: &mut PainterApp, ui: &mut egui::Ui, colors: &[Color32], size: f32) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
        for c in colors {
            let [r, g, b, _] = c.to_srgba_unmultiplied();
            let tip = format!("#{r:02X}{g:02X}{b:02X}: click to paint with it");
            if color_swatch(ui, *c, egui::vec2(size, size))
                .on_hover_text(tip)
                .clicked()
            {
                app.brush_state.brush.brush_options.color = *c;
                app.brush_state.brush_preview.dirty = true;
            }
        }
    });
}

fn picture_section(app: &mut PainterApp, ui: &mut egui::Ui, swatch: f32) {
    heading(ui, "TAKE COLOURS FROM");
    let p = &mut app.workspace.palette;
    segmented(
        ui,
        &mut p.from_layer,
        &[(false, "Whole picture"), (true, "Selected layer")],
        false,
    );
    ui.horizontal(|ui| {
        ui.label(RichText::new("Colours").color(TEXT_DIM));
        ui.add(crate::ui::widgets::reset(&mut p.count, |v| {
            BarSlider::new(v, 2..=64)
        }));
    });
    ui.horizontal(|ui| {
        if ui.button("Extract palette").clicked() {
            app.extract_palette();
        }
        if ui
            .button("From an image…")
            .on_hover_text("Take the colours of a picture file")
            .clicked()
        {
            app.pick_open(crate::app::files::OpenFor::Palette);
        }
    });
    let hint = match &app.workspace.palette.source_image {
        Some(name) => format!("Colours of {name}"),
        None => "Or drop a picture on this window".to_string(),
    };
    ui.label(RichText::new(hint).small().color(TEXT_DIM));

    let extracted = app.workspace.palette.extracted.clone();
    if !extracted.is_empty() {
        ui.add_space(6.0);
        strip(app, ui, &extracted, swatch);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Add to palette")
                .on_hover_text("As new rows of the palette shown (leaving out colours it has)")
                .clicked()
            {
                app.add_to_palette(&extracted);
            }
            if ui
                .button("New palette")
                .on_hover_text("A palette of these, a row for each hue")
                .clicked()
            {
                let name = (app.workspace.palette.source_image.clone())
                    .unwrap_or_else(|| app.workspace.tabs.title.clone());
                app.new_palette_from(&name, &extracted);
            }
        });
    }
}

fn ramp_section(app: &mut PainterApp, ui: &mut egui::Ui, swatch: f32) {
    heading(ui, "MAKE COLOURS");
    let p = &mut app.workspace.palette;
    egui::ComboBox::from_id_salt("palette_maker_kind")
        .selected_text(p.ramp_kind.label())
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for kind in RampKind::ALL {
                ui.selectable_value(&mut p.ramp_kind, kind, kind.label());
            }
        });
    let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(TEXT_DIM));
            add(ui);
        });
    };
    match p.ramp_kind {
        RampKind::Shades | RampKind::Blend | RampKind::Gradient => {
            row(ui, "Steps", &mut |ui| {
                ui.add(BarSlider::new(&mut p.ramp_steps, 2..=32));
            });
        }
        RampKind::Grid | RampKind::Variations => {
            row(ui, "Columns", &mut |ui| {
                ui.add(BarSlider::new(&mut p.grid_cols, 1..=16));
            });
            row(ui, "Rows", &mut |ui| {
                ui.add(BarSlider::new(&mut p.grid_rows, 1..=16));
            });
        }
    }
    match p.ramp_kind {
        RampKind::Shades => row(ui, "Hue shift", &mut |ui| {
            ui.add(
                BarSlider::new(&mut p.hue_shift, 0.0..=60.0)
                    .suffix("°")
                    .max_decimals(0),
            )
            .on_hover_text("Shadows turn toward blue and lights toward yellow");
        }),
        RampKind::Variations => {
            row(ui, "Hue", &mut |ui| {
                ui.add(
                    BarSlider::new(&mut p.vary_hue, 0.0..=180.0)
                        .suffix("°")
                        .max_decimals(0),
                )
                .on_hover_text("How far the hue turns either way across a row");
            });
            row(ui, "Lightness", &mut |ui| {
                ui.add(BarSlider::new(&mut p.vary_light, 0.0..=0.5).max_decimals(2))
                    .on_hover_text("How much lighter the top row and darker the bottom");
            });
        }
        RampKind::Grid => {
            // The corners, as the grid lays them out; a click puts the brush
            // colour in one.
            let brush = app.brush_state.brush.brush_options.color;
            let corners = &mut app.workspace.palette.grid_corners;
            ui.label(
                RichText::new("Corners (click one to put the brush colour there):")
                    .small()
                    .color(TEXT_DIM),
            );
            egui::Grid::new("grid_corners").show(ui, |ui| {
                for (i, c) in corners.iter_mut().enumerate() {
                    if color_swatch(ui, *c, egui::vec2(swatch, swatch))
                        .on_hover_text("Click: the brush colour goes here")
                        .clicked()
                    {
                        *c = brush;
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        }
        RampKind::Gradient => {
            let library = &app.workspace.gradient.library;
            let current = app.workspace.palette.gradient;
            let mut picked = current;
            egui::ComboBox::from_id_salt("palette_maker_gradient")
                .selected_text(library.name(current).to_string())
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for g in library.choices() {
                        ui.selectable_value(&mut picked, g, library.name(g));
                    }
                });
            app.workspace.palette.gradient = picked;
        }
        RampKind::Blend => {}
    }
    let (colors, cols) = app.palette_ramp();
    for r in colors.chunks(cols.max(1)) {
        strip(app, ui, r, swatch * 0.8);
    }
    ui.horizontal(|ui| {
        if ui
            .button("Add as rows")
            .on_hover_text("At the end of the palette shown")
            .clicked()
        {
            app.add_rows_to_palette(&colors, cols);
        }
        if ui.button("New palette").clicked() {
            let name = app.workspace.palette.ramp_kind.label().to_string();
            app.new_palette_of_rows(&name, &colors, cols);
        }
    });
}

fn recolour_section(app: &mut PainterApp, ui: &mut egui::Ui) {
    heading(ui, "RECOLOUR THE SELECTED LAYER");
    let extracted = !app.workspace.palette.extracted.is_empty();
    let palette: Vec<Color32> = if extracted {
        app.workspace.palette.extracted.clone()
    } else {
        app.brush_state.palettes.active().colors().collect()
    };
    ui.checkbox(
        &mut app.workspace.palette.dither,
        "Dither (mix neighbouring colours)",
    );
    let label = if extracted {
        "Recolour to the extracted colours"
    } else {
        "Recolour to the palette"
    };
    if ui
        .add_enabled(!palette.is_empty(), egui::Button::new(label))
        .on_hover_text("Every pixel takes the nearest palette colour (inside the selection, if any). Undo restores it.")
        .clicked()
    {
        app.recolor_layer(&palette);
    }
}

/// A drop-down of every palette; picking one shows it.
fn palette_choice(ui: &mut egui::Ui, palettes: &mut Palettes, width: f32) {
    let label = |which: Which, name: &str| match which {
        Which::Mine(_) => name.to_string(),
        Which::Document(_) => format!("{name} (document)"),
    };
    let active = palettes.active_which();
    let mut picked = None;
    egui::ComboBox::from_id_salt(ui.id().with("palette_choice"))
        .selected_text(label(active, &palettes.active().name))
        .width(width)
        .show_ui(ui, |ui| {
            for (which, p) in palettes.all() {
                if ui
                    .selectable_label(which == active, label(which, &p.name))
                    .clicked()
                {
                    picked = Some(which);
                }
            }
        });
    if let Some(which) = picked {
        palettes.set_active(which);
    }
}

/// What a press on the palette grid asks for.
enum GridAction {
    Pick(Color32),
    Add(Color32),
    Put(Slot, Color32),
    Remove(Slot),
    Rename(Slot, String),
    Swap(Slot, Slot),
}

/// A swatch's menu: its name, replace and remove.
fn swatch_menu(
    ui: &mut egui::Ui,
    slot: Slot,
    swatch: &Swatch,
    current: Color32,
) -> Option<GridAction> {
    ui.set_min_width(170.0);
    let mut action = None;
    let mut name = swatch.name.clone();
    if ui
        .add(egui::TextEdit::singleline(&mut name).hint_text("Name"))
        .changed()
    {
        action = Some(GridAction::Rename(slot, name));
    }
    if ui.button("Replace with the brush colour").clicked() {
        action = Some(GridAction::Put(slot, current));
        ui.close_menu();
    }
    if ui.button("Remove").clicked() {
        action = Some(GridAction::Remove(slot));
        ui.close_menu();
    }
    action
}

/// The palette shown, in the colour panel: its grid of swatches (click to
/// paint with one, drag to move it, right-click or long-press for its
/// menu), a click on a gap puts the brush colour there, and + adds it at
/// the end. Returns the colour picked.
pub(crate) fn palette_grid(
    palettes: &mut Palettes,
    current: Color32,
    ui: &mut egui::Ui,
) -> Option<Color32> {
    let m = metrics(ui.ctx());
    ui.horizontal(|ui| {
        heading(ui, "PALETTE");
        palette_choice(ui, palettes, ui.available_width() - 4.0);
    });
    let palette = palettes.active();
    let columns = palette.columns();
    let gap = 2.0;
    let side = ((ui.available_width() - gap * (columns - 1) as f32) / columns as f32)
        .clamp(8.0, m.recent_swatch.max(24.0));
    let dragging = egui::DragAndDrop::has_payload_of_type::<Slot>(ui.ctx());
    let mut action = None;
    for (g, group) in palette.groups.iter().enumerate() {
        if g > 0 {
            ui.label(RichText::new(&group.name).small().color(TEXT_DIM));
        }
        // The first rows end with +; while dragging, every group has an
        // empty row more to drop into.
        let plus = (g == 0).then_some(group.slots.len());
        let used = group.slots.len() + usize::from(plus.is_some());
        let rows = used.div_ceil(columns).max(1) + usize::from(dragging);
        let size = egui::vec2(
            columns as f32 * (side + gap) - gap,
            rows as f32 * (side + gap) - gap,
        );
        let (area, _) = ui.allocate_exact_size(size, Sense::hover());
        for i in 0..rows * columns {
            let min =
                area.min + egui::vec2((i % columns) as f32, (i / columns) as f32) * (side + gap);
            let rect = egui::Rect::from_min_size(min, egui::vec2(side, side));
            let slot = (g, i);
            let id = ui.id().with(("palette_slot", g, i));
            let swatch = group.slots.get(i).and_then(Option::as_ref);
            let r = ui.interact(
                rect,
                id,
                if swatch.is_some() {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            if r.dnd_hover_payload::<Slot>().is_some() {
                ui.painter().rect_stroke(
                    rect.expand(1.0),
                    RADIUS_SMALL,
                    Stroke::new(2.0, accent()),
                );
            }
            if let Some(from) = r.dnd_release_payload::<Slot>() {
                action = Some(GridAction::Swap(*from, slot));
            }
            let painter = ui.painter();
            match swatch {
                Some(s) => {
                    paint_swatch(painter, rect, s.color);
                    let [cr, cg, cb, _] = s.color.to_srgba_unmultiplied();
                    let hex = format!("#{cr:02X}{cg:02X}{cb:02X}");
                    let tip = if s.name.is_empty() {
                        hex
                    } else {
                        format!("{} ({hex})", s.name)
                    };
                    let r = r.on_hover_text(format!(
                        "{tip}\nClick to use · drag to move · right-click or long-press for more"
                    ));
                    if r.drag_started() {
                        r.dnd_set_drag_payload(slot);
                    }
                    if r.dragged()
                        && let Some(p) = ui.ctx().pointer_interact_pos()
                    {
                        let layer = egui::LayerId::new(egui::Order::Tooltip, id.with("ghost"));
                        let ghost = egui::Rect::from_center_size(p, rect.size());
                        paint_swatch(&ui.ctx().layer_painter(layer), ghost, s.color);
                    }
                    if r.clicked() {
                        action = Some(GridAction::Pick(s.color));
                    }
                    if m.touch {
                        let menu = id.with("menu");
                        if r.long_touched() {
                            ui.memory_mut(|mem| mem.open_popup(menu));
                        }
                        let picked = egui::popup_below_widget(
                            ui,
                            menu,
                            &r,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| swatch_menu(ui, slot, s, current),
                        );
                        if let Some(Some(a)) = picked {
                            if !matches!(a, GridAction::Rename(..)) {
                                ui.memory_mut(|mem| mem.close_popup());
                            }
                            action = Some(a);
                        }
                    } else {
                        r.context_menu(|ui| {
                            if let Some(a) = swatch_menu(ui, slot, s, current) {
                                action = Some(a);
                            }
                        });
                    }
                    if r.hovered() && !dragging {
                        painter.rect_stroke(
                            rect.expand(1.0),
                            RADIUS_SMALL,
                            Stroke::new(1.0, TEXT_STRONG),
                        );
                    }
                }
                None => {
                    let hovered = r.hovered();
                    if plus == Some(i) {
                        let fill = if hovered { WIDGET_HOVER } else { WIDGET };
                        painter.rect_filled(rect, RADIUS_SMALL, fill);
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "+",
                            egui::FontId::proportional(side * 0.7),
                            TEXT,
                        );
                    } else {
                        let line = if hovered { BORDER_LIGHT } else { WIDGET };
                        painter.rect_stroke(rect.shrink(0.5), RADIUS_SMALL, Stroke::new(1.0, line));
                    }
                    let tip = if plus == Some(i) {
                        "Add the brush colour"
                    } else {
                        "Click to put the brush colour here"
                    };
                    if r.on_hover_text(tip).clicked() {
                        action = Some(if plus == Some(i) {
                            GridAction::Add(current)
                        } else {
                            GridAction::Put(slot, current)
                        });
                    }
                }
            }
        }
    }

    let action = action?;
    if let GridAction::Pick(c) = action {
        return Some(c);
    }
    let palette = palettes.edit_active();
    match action {
        GridAction::Pick(_) => {}
        GridAction::Add(c) => {
            palette.push(c);
        }
        GridAction::Put(slot, c) => {
            let name = palette
                .get(slot)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            palette.set(slot, Some(Swatch { color: c, name }));
        }
        GridAction::Remove(slot) => palette.set(slot, None),
        GridAction::Rename(slot, name) => {
            if let Some(s) = palette.get(slot).cloned() {
                palette.set(slot, Some(Swatch { name, ..s }));
            }
        }
        GridAction::Swap(a, b) => palette.swap(a, b),
    }
    None
}
