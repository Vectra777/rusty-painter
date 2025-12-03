use crate::brush_engine::brush::{Brush, BrushPreset, StrokeState};
use crate::canvas::canvas::Canvas;
use crate::canvas::history::UndoAction;
use crate::utils::vector::Vec2;
use eframe::egui;
use eframe::egui::{Color32, TextureOptions};
use rayon::ThreadPool;
use std::collections::{HashMap, HashSet};

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
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        *show_modal = false;
                    }
                    if ui.button("Save").clicked() {
                        let name = if new_preset_name.trim().is_empty() {
                            "Untitled Brush".to_string()
                        } else {
                            new_preset_name.trim().to_string()
                        };
                        
                        presets.push(BrushPreset {
                            name,
                            brush: brush.clone(),
                        });
                        *show_modal = false;
                    }
                });
            });
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.columns(3, |col| {
            let mut idx = 0;
            for preset in presets {
                let column = &mut col[idx];
                column.vertical(|ui| {
                    let preview_size = 64.0; // Increased size for better visibility
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(preview_size, preview_size),
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
                    ui.painter().rect_filled(rect, 2.0, Color32::from_gray(30));
                    
                    // Draw texture
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(texture_id, rect, uv, Color32::WHITE);

                    // Selection highlight
                    // We don't strictly track which preset is "selected" in PainterApp yet,
                    // but we could highlight if active brush matches preset?
                    // For now just hover effect
                    if response.hovered() {
                         ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, Color32::WHITE));
                    } else {
                         ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, Color32::GRAY));
                    }

                    let response = response.on_hover_text(&preset.name);
                    if response.clicked() {
                        let current_color = brush.color;
                        *brush = preset.brush.clone();
                        brush.color = current_color;
                    }
                    
                    ui.label(egui::RichText::new(&preset.name).size(10.0).weak());
                });
                column.add_space(8.0);
                idx += 1;
                idx = idx % 3;
            }
        });
    });
}

fn generate_preset_preview(brush_template: &Brush, pool: &ThreadPool, ctx: &egui::Context) -> egui::TextureHandle {
    let w = 128;
    let h = 128;
    let canvas = Canvas::new(w, h, Color32::TRANSPARENT, 32);
    
    let mut brush = brush_template.clone();
    // Normalize brush size for preview so huge brushes don't look weird
    brush.diameter = 20.0; 
    brush.color = Color32::WHITE;
    
    let mut stroke = StrokeState::new();
    let mut undo = UndoAction { tiles: Vec::new() };
    let mut modified = HashSet::new();

    // Draw S curve
    let steps = 80;
    let margin = 20.0;
    let width = w as f32;
    let height = h as f32;
    let effective_w = width - 2.0 * margin;

    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = margin + t * effective_w;
        let phase = t * std::f32::consts::PI * 2.0;
        let y = height * 0.5 + (phase.sin() * height * 0.3);
        
        let pressure = (t * std::f32::consts::PI).sin();
        brush.diameter = (20.0 * pressure).max(2.0);
        
        stroke.add_point(pool, &canvas, &mut brush, Vec2 { x, y }, &mut undo, &mut modified);
    }

    let mut image = egui::ColorImage::new([w, h], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, w, h, &mut image, 1);
    
    ctx.load_texture("preset_preview", image, TextureOptions::LINEAR)
}
