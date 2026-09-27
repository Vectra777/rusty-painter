//! The Palette window: turn the picture into a palette, keep it as swatches
//! and recolour a layer with it.

use crate::PainterApp;
use crate::ui::style::*;
use crate::ui::widgets::{color_swatch, segmented};
use eframe::egui::{self, RichText};

pub fn palette_window(app: &mut PainterApp, ctx: &egui::Context) {
    let mut open = app.workspace.palette.open;
    if !open {
        return;
    }
    let touch = metrics(ctx).touch;
    let swatch = if touch { 34.0 } else { 24.0 };
    let response = egui::Window::new("Palette")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(300.0)
        .show(ctx, |ui| {
            let p = &mut app.workspace.palette;
            ui.label(RichText::new("TAKE COLOURS FROM").small().strong().color(TEXT_DIM));
            segmented(ui, &mut p.from_layer, &[(false, "Whole picture"), (true, "Selected layer")], false);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Colours").color(TEXT_DIM));
                ui.add(crate::ui::widgets::reset(&mut p.count, |v| egui::Slider::new(v, 2..=64)));
            });
            ui.horizontal(|ui| {
                if ui.button("Extract palette").clicked() {
                    app.extract_palette();
                }
                #[cfg(not(target_os = "android"))]
                if ui
                    .button("From an image…")
                    .on_hover_text("Take the colours of a picture file")
                    .clicked()
                {
                    palette_from_image_dialog(app);
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
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                    for c in &extracted {
                        let [r, g, b, _] = c.to_srgba_unmultiplied();
                        let tip = format!("#{r:02X}{g:02X}{b:02X} — click to paint with it");
                        if color_swatch(ui, *c, egui::vec2(swatch, swatch)).on_hover_text(tip).clicked() {
                            app.brush_state.brush.brush_options.color = *c;
                            app.brush_state.brush_preview.dirty = true;
                        }
                    }
                });
                ui.add_space(4.0);
                if ui.button("Add to swatches").clicked() {
                    app.add_swatches(&extracted);
                }
            }

            ui.separator();
            ui.label(RichText::new("RECOLOUR THE SELECTED LAYER").small().strong().color(TEXT_DIM));
            let palette = if extracted.is_empty() {
                app.brush_state.swatches.clone()
            } else {
                extracted
            };
            ui.checkbox(&mut app.workspace.palette.dither, "Dither (mix neighbouring colours)");
            let label = if app.workspace.palette.extracted.is_empty() {
                "Recolour to swatches"
            } else {
                "Recolour to this palette"
            };
            let enabled = !palette.is_empty();
            if ui
                .add_enabled(enabled, egui::Button::new(label))
                .on_hover_text("Every pixel takes the nearest palette colour (inside the selection, if any). Undo restores it.")
                .clicked()
            {
                app.recolor_layer(&palette);
            }
        });
    app.workspace.palette.window_rect = response.map(|r| r.response.rect);
    app.workspace.palette.open = open;
}

#[cfg(not(target_os = "android"))]
fn palette_from_image_dialog(app: &mut PainterApp) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "tif", "tiff"])
        .pick_file()
    else {
        return;
    };
    let name = path
        .file_stem()
        .map_or_else(|| "Image".to_string(), |s| s.to_string_lossy().into_owned());
    let result = std::fs::read(&path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))
        .and_then(|bytes| app.extract_palette_from_image(&name, &bytes));
    if let Err(err) = result {
        app.export_state.message = Some(err);
    }
}

/// Swatches in the colour panel: click to pick, right-click or long-press
/// to remove, + to add the brush colour. Returns the picked colour, and
/// whether the swatches changed (to save them).
pub(crate) fn swatches(
    app_swatches: &mut Vec<eframe::egui::Color32>,
    current: egui::Color32,
    ui: &mut egui::Ui,
) -> (Option<egui::Color32>, bool) {
    let mut changed = false;
    let size = metrics(ui.ctx()).recent_swatch;
    let mut picked = None;
    let mut remove = None;
    ui.label(RichText::new("SWATCHES").small().strong().color(TEXT_DIM));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
        for (i, &c) in app_swatches.iter().enumerate() {
            let r = color_swatch(ui, c, egui::vec2(size, size))
                .on_hover_text("Click to use · right-click or long-press to remove");
            // A long press (pen or finger) removes rather than picks.
            if r.secondary_clicked() || r.long_touched() {
                remove = Some(i);
            } else if r.clicked() {
                picked = Some(c);
            }
        }
        if ui
            .add_sized([size, size], egui::Button::new("+"))
            .on_hover_text("Add the brush colour")
            .clicked()
            && !app_swatches.contains(&current)
        {
            app_swatches.push(current);
            changed = true;
        }
    });
    if let Some(i) = remove {
        app_swatches.remove(i);
        changed = true;
    }
    (picked, changed)
}
