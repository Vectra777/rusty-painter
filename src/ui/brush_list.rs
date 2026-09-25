use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::preview::stroke_preview_image;
use crate::ui::style::{PANEL_BG, PRESET_PREVIEW_SIZE};
use eframe::egui;
use eframe::egui::{Color32, TextureOptions};
use rayon::ThreadPool;
use std::collections::HashMap;

/// Temp-memory id used to flag a duplicate preset name in the save modal.
const DUPLICATE_NAME_WARNING_ID: &str = "brush_preset_duplicate_name";

/// Displays available presets and lets the user apply one to the active brush.
pub fn brush_list_panel(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    presets: &mut Vec<BrushPreset>,
    previews: &mut HashMap<String, egui::TextureHandle>,
    pool: &ThreadPool,
    show_modal: &mut bool,
    new_preset_name: &mut String,
) {
    ui.set_min_width(200.0);
    let ctx = ui.ctx().clone();

    ui.horizontal(|ui| {
        ui.heading("Presets");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("+").clicked() {
                *show_modal = true;
                *new_preset_name = "New Preset".to_string();
                ctx.data_mut(|d| d.remove::<bool>(egui::Id::new(DUPLICATE_NAME_WARNING_ID)));
            }
        });
    });
    ui.separator();

    // Modal for new preset
    if *show_modal {
        egui::Window::new("Save Brush Preset")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(&ctx, |ui| {
                ui.label("Preset Name:");
                ui.text_edit_singleline(new_preset_name);

                let warning_id = egui::Id::new(DUPLICATE_NAME_WARNING_ID);
                if ctx.data(|d| d.get_temp::<bool>(warning_id).unwrap_or(false)) {
                    ui.colored_label(
                        Color32::from_rgb(220, 80, 80),
                        "A preset with this name already exists.",
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        *show_modal = false;
                        ctx.data_mut(|d| d.remove::<bool>(warning_id));
                    }
                    if ui.button("Save").clicked() {
                        let name = if new_preset_name.trim().is_empty() {
                            "Untitled Brush".to_string()
                        } else {
                            new_preset_name.trim().to_string()
                        };

                        if presets.iter().any(|p| p.name == name) {
                            ctx.data_mut(|d| d.insert_temp(warning_id, true));
                        } else {
                            presets.push(BrushPreset {
                                name,
                                brush: brush.clone(),
                            });
                            ctx.data_mut(|d| d.remove::<bool>(warning_id));
                            *show_modal = false;
                        }
                    }
                });
            });
    }

    egui::ScrollArea::vertical()
        .id_salt("brush_presets_scroll")
        .show(ui, |ui| {
            ui.columns(3, |col| {
                let mut idx = 0;
                for preset in presets {
                    let column = &mut col[idx];
                    column.vertical(|ui| {
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(PRESET_PREVIEW_SIZE, PRESET_PREVIEW_SIZE),
                            egui::Sense::click(),
                        );

                        // Ensure preview exists
                        let texture_id = if let Some(tex) = previews.get(&preset.name) {
                            tex.id()
                        } else {
                            // Generate preview
                            let tex = generate_preset_preview(&preset.brush, pool, &ctx);
                            let id = tex.id();
                            previews.insert(preset.name.clone(), tex);
                            id
                        };

                        // Draw background
                        ui.painter().rect_filled(rect, 2.0, PANEL_BG);

                        // Draw texture
                        let uv =
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        ui.painter().image(texture_id, rect, uv, Color32::WHITE);

                        // Selection highlight
                        // We don't strictly track which preset is "selected" in PainterApp yet,
                        // but we could highlight if active brush matches preset?
                        // For now just hover effect
                        if response.hovered() {
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, Color32::WHITE),
                            );
                        } else {
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, Color32::GRAY),
                            );
                        }

                        let response = response.on_hover_text(&preset.name);
                        if response.clicked() {
                            let current_color = brush.brush_options.color;
                            *brush = preset.brush.clone();
                            brush.brush_options.color = current_color;
                        }

                        ui.label(egui::RichText::new(&preset.name).size(10.0).weak());
                    });
                    column.add_space(8.0);
                    idx += 1;
                    idx %= 3;
                }
            });
        });
}

fn generate_preset_preview(
    brush_template: &Brush,
    pool: &ThreadPool,
    ctx: &egui::Context,
) -> egui::TextureHandle {
    let mut brush = brush_template.clone();
    let image = stroke_preview_image(&mut brush, pool, [128, 128], 32, Color32::WHITE, 20.0);
    ctx.load_texture("preset_preview", image, TextureOptions::LINEAR)
}
