//! The mixing palette's panel: paint on it with the brush, smear with the
//! smudge (or a mixing brush), right-click or Alt+click to pick a colour.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::mixer::{Mode, SIZE};
use crate::ui::style::{BORDER, TEXT_DIM};
use eframe::egui::{self, Color32, RichText, Stroke};

pub fn mixer_panel(app: &mut PainterApp, ui: &mut egui::Ui) {
    let width = ui.available_width();
    let scale = width / SIZE[0] as f32;
    let size = egui::vec2(width, SIZE[1] as f32 * scale);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let at = response
        .interact_pointer_pos()
        .map(|p| ((p - rect.min) / scale).max(egui::Vec2::ZERO));
    let alt = ui.input(|i| i.modifiers.alt);
    let ws = &mut app.workspace;
    let pressure = ws.last_pressure.map_or(1.0, |(_, mapped)| mapped);
    let pick = response.secondary_clicked() || (alt && response.clicked());
    if pick && let Some(at) = at {
        let bs = &mut app.brush_state;
        let alpha = bs.brush.brush_options.color.a();
        let [r, g, b, _] = ws.mixer.color_at(at).to_array();
        bs.brush.brush_options.color = Color32::from_rgba_unmultiplied(r, g, b, alpha);
        bs.brush_preview.dirty = true;
    } else if let Some(at) = at
        && !alt
        && response.is_pointer_button_down_on()
        && ui.input(|i| i.pointer.primary_down())
    {
        if ws.mixer.is_painting() {
            ws.mixer.drag(&ws.pool, at, pressure);
        } else {
            let brush = &app.brush_state.brush;
            let mode = match (app.active_tool, brush.mixing) {
                (Tool::Smudge, _) => Mode::Smudge {
                    length: ws.blend.smudge_length,
                    color_rate: ws.blend.color_rate,
                },
                (_, Some(m)) => Mode::Smudge {
                    length: m.smudge_length,
                    color_rate: m.color_rate,
                },
                _ => Mode::Paint,
            };
            ws.mixer.press(brush, &ws.pool, mode, at, pressure);
        }
        ui.ctx().request_repaint();
    } else if ws.mixer.is_painting() {
        ws.mixer.release(&ws.pool);
    }
    let texture = ws.mixer.texture(ui.ctx()).id();
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
    ui.painter().image(texture, rect, uv, Color32::WHITE);
    ui.painter()
        .rect_stroke(rect, 0.0, Stroke::new(1.0_f32, BORDER));
    ui.horizontal(|ui| {
        if ui.small_button("Clear").clicked() {
            ws.mixer.clear();
        }
        ui.label(
            RichText::new("Smudge to mix · right-click picks")
                .small()
                .color(TEXT_DIM),
        );
    });
}
