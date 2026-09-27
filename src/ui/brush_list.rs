//! Floating brush presets window (toolbar button or `P`): one column of wide
//! stroke previews with the preset name inside, like Clip Studio's brush
//! list. Picking a preset loads it into the tool it belongs to: eraser
//! presets switch to the eraser, the others to the brush.

use crate::PainterApp;
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::brush_options::BlendMode;
use crate::brush_engine::preview::stroke_preview_image;
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::icon_button;
use eframe::egui::{self, Color32, RichText, Stroke, TextureOptions};
use rayon::ThreadPool;

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
    let default_pos = egui::pos2(m.toolbar_width + 8.0, m.menu_height + m.bar_height + 8.0);
    egui::Window::new("Brush Presets")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_pos(default_pos)
        .default_size([WINDOW_WIDTH, 520.0])
        .min_size([200.0, 160.0])
        .show(ctx, |ui| {
            if let Some(index) = presets_list(app, ui) {
                app.apply_preset(index);
                // On a tablet the window is in the way once a preset is picked.
                if m.touch {
                    app.brush_state.show_presets = false;
                }
            }
        });
    if !open {
        app.brush_state.show_presets = false;
    }
    save_preset_modal(app, ctx);
}

/// The list; returns the index of the preset the user picked.
fn presets_list(app: &mut PainterApp, ui: &mut egui::Ui) -> Option<usize> {
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
        });
    });

    let tile_height = if m.touch { 76.0 } else { 58.0 };
    let pool = app.workspace.pool.clone();
    let eraser_first = app.brush_state.eraser_active;
    egui::ScrollArea::vertical()
        .id_salt("brush_presets_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // The active tool's presets first.
            let sections = if eraser_first {
                [true, false]
            } else {
                [false, true]
            };
            for erasers in sections {
                ui.label(
                    RichText::new(if erasers { "ERASERS" } else { "BRUSHES" })
                        .small()
                        .strong()
                        .color(TEXT_DIM),
                );
                let bs = &mut app.brush_state;
                for (index, preset) in bs.presets.iter().enumerate() {
                    let is_eraser = preset.brush.brush_options.blend_mode == BlendMode::Eraser;
                    if is_eraser != erasers {
                        continue;
                    }
                    let texture = bs
                        .preset_previews
                        .entry(preset.name.clone())
                        .or_insert_with(|| preview_texture(&preset.brush, &pool, ui.ctx()))
                        .id();
                    let active = bs.active_preset.as_deref() == Some(preset.name.as_str())
                        && bs.eraser_active == is_eraser;
                    if preset_tile(ui, &preset.name, texture, tile_height, active).clicked() {
                        picked = Some(index);
                    }
                }
                ui.add_space(6.0);
            }
        });
    picked
}

/// A wide preview tile with the name drawn inside it.
fn preset_tile(
    ui: &mut egui::Ui,
    name: &str,
    texture: egui::TextureId,
    height: f32,
    active: bool,
) -> egui::Response {
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
    painter.image(
        texture,
        egui::Rect::from_center_size(inner.center(), size),
        uv,
        Color32::WHITE,
    );

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
    response.on_hover_text(name)
}

fn preview_texture(brush: &Brush, pool: &ThreadPool, ctx: &egui::Context) -> egui::TextureHandle {
    let mut brush = brush.clone();
    let image = stroke_preview_image(
        &mut brush,
        pool,
        PREVIEW_PX,
        64,
        PREVIEW_INK,
        PREVIEW_DIAMETER,
    );
    ctx.load_texture("preset_preview", image, TextureOptions::LINEAR)
}

/// Name prompt for saving the active tool's settings as a new preset.
fn save_preset_modal(app: &mut PainterApp, ctx: &egui::Context) {
    let bs = &mut app.brush_state;
    if !bs.show_new_preset_modal {
        return;
    }
    let warning_id = egui::Id::new(DUPLICATE_NAME_WARNING_ID);
    egui::Window::new("Save Brush Preset")
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
                        bs.presets.push(BrushPreset {
                            name: name.clone(),
                            brush: bs.brush.clone(),
                        });
                        bs.active_preset = Some(name);
                        ctx.data_mut(|d| d.remove::<bool>(warning_id));
                        bs.show_new_preset_modal = false;
                    }
                }
            });
        });
}
