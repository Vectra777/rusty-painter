use crate::ColorModel;
use crate::app::state::BrushState;
use crate::canvas::color::ColorManipulation;
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use crate::ui::widgets::{color_swatch, draw_checkerboard, paint_swatch, section};
use eframe::egui::{self, Color32, Pos2, RichText, Sense, Stroke};
use std::f32::consts::TAU;

/// Ring thickness as a fraction of the wheel radius.
const RING_FRACTION: f32 = 0.16;

/// Picker state kept across frames: HSV survives round-trips through the
/// 8-bit brush color (hue is lost at zero saturation, saturation at black).
#[derive(Clone, Copy, Debug)]
struct PickerState {
    hue: f32,
    sat: f32,
    val: f32,
    last_color: Color32,
    drag: WheelDrag,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WheelDrag {
    None,
    Ring,
    Triangle,
}

/// Horizontal gradient bar with a handle; returns whether `value` changed.
fn gradient_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    color_at: &dyn Fn(f32) -> Color32,
    checker: bool,
) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized(
            egui::vec2(14.0, 18.0),
            egui::Label::new(RichText::new(label).small().color(TEXT_DIM)),
        );
        let value_width = 36.0;
        let width = (ui.available_width() - value_width - ui.spacing().item_spacing.x).max(40.0);
        let bar_height = if metrics(ui.ctx()).touch { 26.0 } else { 14.0 };
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, bar_height), Sense::click_and_drag());
        let painter = ui.painter();
        if checker {
            draw_checkerboard(painter, rect, 7.0);
        }

        let steps = 48;
        let mut mesh = egui::Mesh::default();
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = egui::lerp(rect.x_range(), t);
            let color = color_at(t);
            mesh.colored_vertex(egui::pos2(x, rect.top()), color);
            mesh.colored_vertex(egui::pos2(x, rect.bottom()), color);
            if i > 0 {
                let base = (i * 2) as u32;
                mesh.add_triangle(base - 2, base - 1, base);
                mesh.add_triangle(base - 1, base + 1, base);
            }
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, BORDER));

        let handle_x = egui::lerp(rect.x_range(), value.clamp(0.0, 1.0));
        let handle = egui::Rect::from_center_size(
            egui::pos2(handle_x, rect.center().y),
            egui::vec2(5.0, bar_height + 4.0),
        );
        painter.rect_filled(handle, 0.0, Color32::WHITE);
        painter.rect_stroke(handle, 0.0, Stroke::new(1.0_f32, Color32::BLACK));

        let mut changed = false;
        if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            if (t - *value).abs() > f32::EPSILON {
                *value = t;
                changed = true;
            }
        }

        let mut percent = (*value * 100.0).round();
        let drag = ui.add_sized(
            egui::vec2(value_width, 18.0),
            egui::DragValue::new(&mut percent)
                .range(0.0..=100.0)
                .speed(0.5)
                .max_decimals(0),
        );
        if drag.changed() {
            *value = percent / 100.0;
            changed = true;
        }
        changed
    })
    .inner
}

/// Barycentric weights of `p` in triangle `(a, b, c)`.
fn barycentric(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> (f32, f32, f32) {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let denom = v0.x * v1.y - v1.x * v0.y;
    if denom.abs() <= f32::EPSILON {
        return (1.0, 0.0, 0.0);
    }
    let wb = (v2.x * v1.y - v1.x * v2.y) / denom;
    let wc = (v0.x * v2.y - v2.x * v0.y) / denom;
    (1.0 - wb - wc, wb, wc)
}

/// Hue ring with a saturation/value triangle inside. The triangle rotates
/// so its pure-hue corner points at the selected hue on the ring.
fn hue_wheel(ui: &mut egui::Ui, state: &mut PickerState) -> bool {
    let side = ui.available_width().min(metrics(ui.ctx()).wheel_max);
    let (outer_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), side), Sense::hover());
    let rect = egui::Rect::from_center_size(outer_rect.center(), egui::vec2(side, side));
    let response = ui.interact(rect, ui.id().with("hue_wheel"), Sense::click_and_drag());

    let center = rect.center();
    let r_out = side * 0.5 - 1.0;
    let r_in = r_out * (1.0 - RING_FRACTION);
    let r_tri = r_in - 4.0;

    let painter = ui.painter();

    // Ring.
    let segments = 96;
    let mut ring = egui::Mesh::default();
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let (sin, cos) = (t * TAU).sin_cos();
        let dir = egui::vec2(cos, sin);
        let color = Color32::from_hsva(t, 1.0, 1.0, 1.0);
        ring.colored_vertex(center + dir * r_in, color);
        ring.colored_vertex(center + dir * r_out, color);
        if i > 0 {
            let base = (i * 2) as u32;
            ring.add_triangle(base - 2, base - 1, base);
            ring.add_triangle(base - 1, base + 1, base);
        }
    }
    painter.add(egui::Shape::mesh(ring));

    // Triangle corners: pure hue, white, black.
    let angle = state.hue * TAU;
    let corner = |offset: f32| {
        let (sin, cos) = (angle + offset).sin_cos();
        center + egui::vec2(cos, sin) * r_tri
    };
    let p_hue = corner(0.0);
    let p_white = corner(TAU / 3.0);
    let p_black = corner(2.0 * TAU / 3.0);
    let mut tri = egui::Mesh::default();
    tri.colored_vertex(p_hue, Color32::from_hsva(state.hue, 1.0, 1.0, 1.0));
    tri.colored_vertex(p_white, Color32::WHITE);
    tri.colored_vertex(p_black, Color32::BLACK);
    tri.add_triangle(0, 1, 2);
    painter.add(egui::Shape::mesh(tri));

    // Interaction: the press position decides whether the drag edits hue
    // (ring) or saturation/value (triangle) for the whole gesture.
    let mut changed = false;
    if (response.drag_started() || response.clicked() || response.is_pointer_button_down_on())
        && let Some(pos) = response.interact_pointer_pos()
    {
        if state.drag == WheelDrag::None || response.drag_started() || response.clicked() {
            let d = (pos - center).length();
            let (wh, ww, wb) = barycentric(pos, p_hue, p_white, p_black);
            state.drag = if d >= r_in - 2.0 && d <= r_out + 4.0 {
                WheelDrag::Ring
            } else if wh >= -0.02 && ww >= -0.02 && wb >= -0.02 {
                WheelDrag::Triangle
            } else {
                WheelDrag::None
            };
        }
        match state.drag {
            WheelDrag::Ring => {
                let v = pos - center;
                let hue = (v.y.atan2(v.x) / TAU).rem_euclid(1.0);
                if (hue - state.hue).abs() > f32::EPSILON {
                    state.hue = hue;
                    changed = true;
                }
            }
            WheelDrag::Triangle => {
                let (wh, ww, wb) = barycentric(pos, p_hue, p_white, p_black);
                let (wh, ww, wb) = (wh.max(0.0), ww.max(0.0), wb.max(0.0));
                let sum = (wh + ww + wb).max(f32::EPSILON);
                let (wh, ww) = (wh / sum, ww / sum);
                // color = wh·hue + ww·white + wb·black, so
                // value = wh + ww and saturation = wh / value.
                let val = (wh + ww).clamp(0.0, 1.0);
                let sat = if val > f32::EPSILON {
                    (wh / val).clamp(0.0, 1.0)
                } else {
                    state.sat
                };
                state.sat = sat;
                state.val = val;
                changed = true;
            }
            WheelDrag::None => {}
        }
    }
    if !response.is_pointer_button_down_on() && !response.dragged() {
        state.drag = WheelDrag::None;
    }

    // Markers.
    let (sin, cos) = angle.sin_cos();
    let ring_mid = center + egui::vec2(cos, sin) * (r_in + r_out) * 0.5;
    let ring_half = (r_out - r_in) * 0.5 + 1.0;
    let tangent = egui::vec2(-sin, cos) * 2.5;
    let radial = egui::vec2(cos, sin) * ring_half;
    let marker = vec![
        ring_mid - radial - tangent,
        ring_mid + radial - tangent,
        ring_mid + radial + tangent,
        ring_mid - radial + tangent,
    ];
    painter.add(egui::Shape::closed_line(
        marker.clone(),
        Stroke::new(3.0_f32, Color32::BLACK),
    ));
    painter.add(egui::Shape::closed_line(
        marker,
        Stroke::new(1.5_f32, Color32::WHITE),
    ));

    let wh = state.sat * state.val;
    let ww = state.val - wh;
    let wb = 1.0 - state.val;
    let p = Pos2::new(
        p_hue.x * wh + p_white.x * ww + p_black.x * wb,
        p_hue.y * wh + p_white.y * ww + p_black.y * wb,
    );
    painter.circle_stroke(p, 5.0, Stroke::new(3.0_f32, Color32::BLACK));
    painter.circle_stroke(p, 5.0, Stroke::new(1.5_f32, Color32::WHITE));

    changed
}

fn to_hex(color: Color32) -> String {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    format!("{r:02X}{g:02X}{b:02X}")
}

fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    let hex = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => hex.to_string(),
        _ => return None,
    };
    let v = u32::from_str_radix(&hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Primary + secondary swatches, swap arrow, and a hex field.
/// Returns a new brush color if the hex field was committed.
fn swatch_row(ui: &mut egui::Ui, brush_state: &mut BrushState) -> Option<Color32> {
    let mut new_color = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let color = brush_state.brush.brush_options.color;
        color_swatch(ui, color, egui::vec2(44.0, 22.0)).on_hover_text("Brush color");
        let (swap_rect, swap) = ui.allocate_exact_size(egui::vec2(16.0, 22.0), Sense::click());
        let swap_color = if swap.hovered() {
            TEXT_STRONG
        } else {
            TEXT_DIM
        };
        paint_icon(
            ui.painter(),
            swap_rect.shrink2(egui::vec2(1.0, 4.0)),
            Icon::Swap,
            swap_color,
        );
        let swap_clicked = swap.on_hover_text("Swap colors (X)").clicked();
        let secondary_clicked =
            color_swatch(ui, brush_state.secondary_color, egui::vec2(28.0, 22.0))
                .on_hover_text("Secondary color — click to swap")
                .clicked();
        if swap_clicked || secondary_clicked {
            std::mem::swap(
                &mut brush_state.brush.brush_options.color,
                &mut brush_state.secondary_color,
            );
            brush_state.brush_preview.dirty = true;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let id = ui.id().with("hex_edit");
            let mut text = ui
                .data(|d| d.get_temp::<String>(id))
                .unwrap_or_else(|| to_hex(color));
            let response = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(64.0)
                    .font(egui::TextStyle::Monospace)
                    .char_limit(7),
            );
            ui.label(RichText::new("#").monospace().color(TEXT_DIM));
            if response.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, text.clone()));
            } else {
                if response.lost_focus()
                    && let Some([r, g, b]) = parse_hex(&text)
                {
                    new_color = Some(Color32::from_rgba_unmultiplied(r, g, b, color.a()));
                }
                ui.data_mut(|d| d.remove::<String>(id));
            }
        });
    });
    new_color
}

/// Grid of recently used colors; returns the clicked one.
fn recent_colors(ui: &mut egui::Ui, colors: &[Color32]) -> Option<Color32> {
    let mut picked = None;
    let swatch = metrics(ui.ctx()).recent_swatch;
    section(ui, "Recent", true, |ui| {
        if colors.is_empty() {
            ui.label(
                RichText::new("Colors you paint with appear here.")
                    .small()
                    .color(TEXT_DIM),
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
            for &color in colors {
                if color_swatch(ui, color, egui::vec2(swatch, swatch))
                    .on_hover_text(format!("#{}", to_hex(color)))
                    .clicked()
                {
                    picked = Some(color);
                }
            }
        });
    });
    picked
}

/// Adopt `color`'s HSV into the picker, keeping components it doesn't
/// define: hue at zero saturation, saturation at black, and everything
/// when fully transparent (its premultiplied RGB is then all zero).
fn sync_state_from_color(state: &mut PickerState, color: Color32) {
    if color.a() == 0 {
        return;
    }
    let (hue, sat, val, _) = color.to_hsva();
    if sat > 0.0 && val > 0.0 {
        state.hue = hue;
    }
    if val > 0.0 {
        state.sat = sat;
    }
    state.val = val;
}

/// Color panel: hue ring + triangle, swatches, sliders and recent colors.
pub fn color_picker_panel(
    ui: &mut egui::Ui,
    brush_state: &mut BrushState,
    color_model: ColorModel,
) {
    let id = ui.id().with("color_picker_state");
    let color = brush_state.brush.brush_options.color;
    let mut state = ui
        .ctx()
        .data_mut(|d| d.get_temp::<PickerState>(id))
        .unwrap_or_else(|| {
            let (hue, sat, val, _) = color.to_hsva();
            PickerState {
                hue,
                sat,
                val,
                last_color: color,
                drag: WheelDrag::None,
            }
        });

    // Resync when the color changed elsewhere (eyedropper, swap, preset).
    if state.last_color != color {
        sync_state_from_color(&mut state, color);
        state.last_color = color;
    }

    // The new brush color, and whether it came from outside the picker's own
    // HSV controls (hex field, recent colors) so HSV must be re-derived.
    let mut new_color: Option<(Color32, bool)> = None;

    egui::ScrollArea::vertical()
        .id_salt("color_picker_scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let mut alpha = color.a() as f32 / 255.0;
            match color_model {
                ColorModel::Rgba => {
                    if hue_wheel(ui, &mut state) {
                        new_color = Some((
                            Color32::from_hsva(state.hue, state.sat, state.val, alpha),
                            false,
                        ));
                    }
                    ui.add_space(6.0);
                    if let Some(c) = swatch_row(ui, brush_state) {
                        new_color = Some((c, true));
                    }
                    ui.add_space(4.0);
                    let (h, s, v) = (state.hue, state.sat, state.val);
                    let mut sliders_changed = false;
                    let mut hue = state.hue;
                    sliders_changed |= gradient_slider(
                        ui,
                        "H",
                        &mut hue,
                        &|t| Color32::from_hsva(t, 1.0, 1.0, 1.0),
                        false,
                    );
                    let mut sat = state.sat;
                    sliders_changed |= gradient_slider(
                        ui,
                        "S",
                        &mut sat,
                        &|t| Color32::from_hsva(h, t, v, 1.0),
                        false,
                    );
                    let mut val = state.val;
                    sliders_changed |= gradient_slider(
                        ui,
                        "V",
                        &mut val,
                        &|t| Color32::from_hsva(h, s, t, 1.0),
                        false,
                    );
                    sliders_changed |= gradient_slider(
                        ui,
                        "A",
                        &mut alpha,
                        &|t| Color32::from_hsva(h, s, v, t),
                        true,
                    );
                    if sliders_changed {
                        state.hue = hue;
                        state.sat = sat;
                        state.val = val;
                        new_color = Some((Color32::from_hsva(hue, sat, val, alpha), false));
                    }
                }
                ColorModel::Grayscale => {
                    // Gray level lives in the picker state: at low alpha the
                    // stored (premultiplied) color can't give it back exactly.
                    let mut value = state.val;
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 40.0),
                        Sense::hover(),
                    );
                    paint_swatch(ui.painter(), rect, color);
                    ui.add_space(6.0);
                    let mut changed = gradient_slider(
                        ui,
                        "V",
                        &mut value,
                        &|t| Color32::from_gray_alpha(t, 1.0),
                        false,
                    );
                    changed |= gradient_slider(
                        ui,
                        "A",
                        &mut alpha,
                        &|t| Color32::from_gray_alpha(value, t),
                        true,
                    );
                    if changed {
                        state.val = value;
                        state.sat = 0.0;
                        new_color = Some((Color32::from_gray_alpha(value, alpha), false));
                    }
                }
            }

            ui.add_space(4.0);
            if let Some(c) = recent_colors(ui, &brush_state.recent_colors) {
                new_color = Some((c, true));
            }
            ui.add_space(4.0);
            if let Some(c) =
                crate::ui::palette_window::swatches(&mut brush_state.swatches, color, ui)
            {
                new_color = Some((c, true));
            }
        });

    if let Some((c, external)) = new_color {
        brush_state.brush.brush_options.color = c;
        brush_state.brush_preview.dirty = true;
        // The picker's own controls already set the exact HSV; deriving it
        // back from the 8-bit premultiplied color would drift (and at zero
        // alpha lose the color entirely). Only outside picks re-derive it.
        if external {
            sync_state_from_color(&mut state, c);
        }
        state.last_color = c;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, state));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(hue: f32, sat: f32, val: f32) -> PickerState {
        PickerState {
            hue,
            sat,
            val,
            last_color: Color32::BLACK,
            drag: WheelDrag::None,
        }
    }

    #[test]
    fn transparent_color_keeps_the_picker_hsv() {
        let mut s = state(0.3, 0.8, 0.9);
        sync_state_from_color(&mut s, Color32::TRANSPARENT);
        assert_eq!((s.hue, s.sat, s.val), (0.3, 0.8, 0.9));
    }

    #[test]
    fn black_keeps_hue_and_saturation() {
        let mut s = state(0.3, 0.8, 0.9);
        sync_state_from_color(&mut s, Color32::BLACK);
        assert_eq!((s.hue, s.sat, s.val), (0.3, 0.8, 0.0));
    }
}
