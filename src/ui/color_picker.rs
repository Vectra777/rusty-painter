use crate::ColorModel;
use crate::brush_engine::brush::Brush;
use crate::utils::color::ColorManipulation;
use eframe::egui;
use egui::Color32;

const TRI_SIDE: f32 = 200.0;
const SLIDER_MIN: f32 = 160.0;
const SLIDER_MAX: f32 = 320.0;

#[derive(Clone, Copy, Debug)]
struct PickerState {
    hue: f32,
    last_color: Color32,
}

fn slider_width(ui: &egui::Ui) -> f32 {
    ui.available_width().clamp(SLIDER_MIN, SLIDER_MAX)
}

fn draw_checkerboard(painter: &egui::Painter, rect: egui::Rect, cell: f32) {
    let rows = ((rect.height() / cell).ceil() as i32).max(1);
    let cols = ((rect.width() / cell).ceil() as i32).max(1);
    for y in 0..rows {
        for x in 0..cols {
            let dark = (x + y) % 2 == 0;
            let min = egui::pos2(rect.left() + x as f32 * cell, rect.top() + y as f32 * cell);
            let max = min + egui::vec2(cell, cell);
            painter.rect_filled(
                egui::Rect::from_min_max(min, max.min(rect.max)),
                2.0,
                if dark {
                    Color32::from_gray(200)
                } else {
                    Color32::from_gray(240)
                },
            );
        }
    }
}

fn gradient_slider(
    ui: &mut egui::Ui,
    width: f32,
    value: &mut f32,
    label: &str,
    color_at: &dyn Fn(f32) -> Color32,
    checker: bool,
) -> bool {
    ui.label(label);
    let bar_height = 18.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, bar_height), egui::Sense::click_and_drag());
    let painter = ui.painter();
    let radius = bar_height * 0.5;
    if checker {
        draw_checkerboard(painter, rect, 8.0);
    }

    let steps = 64;
    let mut mesh = egui::Mesh::default();
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = egui::lerp(rect.x_range(), t);
        let color = color_at(t);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x, rect.top()),
            uv: egui::Pos2::ZERO,
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x, rect.bottom()),
            uv: egui::Pos2::ZERO,
            color,
        });
        if i > 0 {
            let base = (i * 2) as u32;
            mesh.indices
                .extend_from_slice(&[base - 2, base - 1, base, base - 1, base + 1, base]);
        }
    }
    painter.add(egui::Shape::mesh(mesh));

    painter.rect_stroke(
        rect.expand(0.25),
        radius,
        egui::Stroke::new(1.0, Color32::from_gray(80)),
    );

    let t = value.clamp(0.0, 1.0);
    let handle_x = egui::lerp(rect.x_range(), t);
    let handle_rect = egui::Rect::from_center_size(
        egui::pos2(handle_x, rect.center().y),
        egui::vec2(8.0, bar_height + 4.0),
    );
    painter.rect_filled(handle_rect, radius, Color32::from_white_alpha(180));
    painter.rect_stroke(
        handle_rect,
        radius,
        egui::Stroke::new(1.0, Color32::from_gray(40)),
    );

    let pointer_down = ui.input(|i| i.pointer.primary_down());
    if (response.hovered() || response.dragged()) && pointer_down {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            if (t - *value).abs() > f32::EPSILON {
                *value = t;
                return true;
            }
        }
    }
    false
}

fn hsva_triangle(ui: &mut egui::Ui, hue: f32, sat: &mut f32, val: &mut f32, side: f32) -> bool {
    let tri_height = side * (3.0_f32).sqrt() * 0.5;
    let (rect, response) =
        ui.allocate_at_least(egui::vec2(side, tri_height), egui::Sense::click_and_drag());

    let base_x = rect.center().x - side * 0.5;
    let base_y = rect.top();
    let tri_top = egui::pos2(base_x + side * 0.5, base_y);
    let tri_left = egui::pos2(base_x, base_y + tri_height);
    let tri_right = egui::pos2(base_x + side, base_y + tri_height);

    let hue_color = Color32::from_hsva(hue, 1.0, 1.0, 1.0);

    let mut mesh = egui::Mesh::default();
    let top_idx = mesh.vertices.len();
    mesh.vertices.push(egui::epaint::Vertex {
        pos: tri_top,
        uv: egui::Pos2::ZERO,
        color: Color32::WHITE,
    });
    let left_idx = mesh.vertices.len();
    mesh.vertices.push(egui::epaint::Vertex {
        pos: tri_left,
        uv: egui::Pos2::ZERO,
        color: Color32::BLACK,
    });
    let right_idx = mesh.vertices.len();
    mesh.vertices.push(egui::epaint::Vertex {
        pos: tri_right,
        uv: egui::Pos2::ZERO,
        color: hue_color,
    });
    mesh.indices
        .extend_from_slice(&[top_idx as u32, left_idx as u32, right_idx as u32]);
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().add(egui::Shape::line(
        vec![tri_top, tri_right, tri_left, tri_top],
        egui::Stroke::new(1.5, Color32::from_gray(80)),
    ));

    let tri_sat = sat.clamp(0.0, 1.0);
    let tri_val = val.clamp(0.0, 1.0);
    let mut w_hue = tri_sat.min(tri_val);
    let mut w_white = (tri_val - w_hue).max(0.0);
    let mut w_black = (1.0 - w_hue - w_white).max(0.0);
    let sum = w_hue + w_white + w_black;
    if sum > 0.0 {
        w_hue /= sum;
        w_white /= sum;
        w_black /= sum;
    }
    let indicator = egui::pos2(
        tri_top.x * w_white + tri_left.x * w_black + tri_right.x * w_hue,
        tri_top.y * w_white + tri_left.y * w_black + tri_right.y * w_hue,
    );
    ui.painter()
        .circle_filled(indicator, 7.0, Color32::from_white_alpha(32));
    ui.painter().circle_stroke(
        indicator,
        7.0,
        egui::Stroke::new(2.0, Color32::from_gray(30)),
    );

    let pointer_down = ui.input(|i| i.pointer.primary_down());
    let mut changed = false;
    if (response.hovered() || response.dragged()) && pointer_down {
        if let Some(pointer) = response.interact_pointer_pos() {
            let v0 = tri_right - tri_top;
            let v1 = tri_left - tri_top;
            let v2 = pointer - tri_top;
            let denom = v0.x * v1.y - v1.x * v0.y;
            if denom.abs() > f32::EPSILON {
                let inv_denom = 1.0 / denom;
                let w_right = (v2.x * v1.y - v1.x * v2.y) * inv_denom;
                let w_left = (v0.x * v2.y - v2.x * v0.y) * inv_denom;
                let w_top = 1.0 - w_left - w_right;
                if w_top >= -0.01 && w_left >= -0.01 && w_right >= -0.01 {
                    let mut w_top = w_top.clamp(0.0, 1.0);
                    let w_left = w_left.clamp(0.0, 1.0);
                    let mut w_right = w_right.clamp(0.0, 1.0);
                    let total = w_top + w_left + w_right;
                    if total > 0.0 {
                        w_top /= total;
                        w_right /= total;
                    }
                    *sat = w_right;
                    *val = (w_top + w_right).clamp(0.0, 1.0);
                    changed = true;
                }
            }
        }
    }

    ui.add_space(8.0);
    changed
}

/// Interactive HSVA picker that updates the active brush color.
pub fn color_picker_panel(ui: &mut egui::Ui, brush: &mut Brush, color_model: ColorModel) {
    let min_width = slider_width(ui);
    ui.set_min_width(min_width);

    let id = ui.id().with("color_picker_state");
    let (mut hue, mut sat, mut val, mut alpha) = brush.color.to_hsva();
    let mut state = ui.ctx().data_mut(|d| {
        d.get_temp::<PickerState>(id).unwrap_or(PickerState {
            hue,
            last_color: brush.color,
        })
    });

    if state.last_color != brush.color {
        let (nh, _, _, _) = brush.color.to_hsva();
        state.hue = nh;
        state.last_color = brush.color;
    }

    hue = state.hue;

    let mut apply_color = false;

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| match color_model {
            ColorModel::Rgba => {
                apply_color = rgba_picker(ui, &mut hue, &mut sat, &mut val, &mut alpha);
            }
            ColorModel::Grayscale => {
                if grayscale_picker(ui, brush) {
                    let (h, _, _, _) = brush.color.to_hsva();
                    state.hue = h;
                    state.last_color = brush.color;
                    ui.ctx().data_mut(|d| d.insert_temp(id, state));
                }
            }
        });

    if apply_color {
        brush.color = Color32::from_hsva(hue, sat, val, alpha);
        state.hue = hue;
        state.last_color = brush.color;
        ui.ctx().data_mut(|d| d.insert_temp(id, state));
    }
}

fn grayscale_picker(ui: &mut egui::Ui, brush: &mut Brush) -> bool {
    let width = slider_width(ui);
    let mut value = (brush.color.r() as u16 + brush.color.g() as u16 + brush.color.b() as u16)
        as f32
        / (3.0 * 255.0);
    let mut alpha = brush.color.a() as f32 / 255.0;
    let mut changed = false;

    ui.label("Grayscale");
    changed |= gradient_slider(
        ui,
        width,
        &mut value,
        "Value",
        &|t| Color32::from_gray_alpha(t, 1.0),
        false,
    );
    changed |= gradient_slider(
        ui,
        width,
        &mut alpha,
        "Opacity",
        &|t| Color32::from_gray_alpha(value, t),
        true,
    );

    let mut preview = Color32::from_gray_alpha(value, alpha);
    ui.horizontal(|ui| {
        ui.label("Preview");
        ui.color_edit_button_srgba(&mut preview);
    });

    if changed {
        brush.color = Color32::from_gray_alpha(value, alpha);
    }

    changed
}

fn cmyk_picker(ui: &mut egui::Ui, brush: &mut Brush) {
    let width = slider_width(ui);
    let (mut c, mut m, mut y, mut k, mut a) = brush.color.to_cmyk();
    let mut changed = false;
    let mut color = Color32::from_cmyk(c, m, y, k, a);

    ui.label("CMYK");
    let (hue, mut sat, mut val, _) = color.to_hsva();
    let tri_side = ui.available_width().clamp(140.0, TRI_SIDE);
    changed |= hsva_triangle(ui, hue, &mut sat, &mut val, tri_side);
    if changed {
        color = Color32::from_hsva(hue, sat, val, a);
        let (nc, nm, ny, nk, _) = color.to_cmyk();
        c = nc;
        m = nm;
        y = ny;
        k = nk;
    }

    changed |= gradient_slider(
        ui,
        width,
        &mut c,
        "Cyan",
        &|t| Color32::from_cmyk(t, m, y, k, 1.0),
        false,
    );
    changed |= gradient_slider(
        ui,
        width,
        &mut m,
        "Magenta",
        &|t| Color32::from_cmyk(c, t, y, k, 1.0),
        false,
    );
    changed |= gradient_slider(
        ui,
        width,
        &mut y,
        "Yellow",
        &|t| Color32::from_cmyk(c, m, t, k, 1.0),
        false,
    );
    changed |= gradient_slider(
        ui,
        width,
        &mut k,
        "Key (Black)",
        &|t| Color32::from_cmyk(c, m, y, t, 1.0),
        false,
    );
    changed |= gradient_slider(
        ui,
        width,
        &mut a,
        "Opacity",
        &|t| Color32::from_cmyk(c, m, y, k, t),
        true,
    );

    let mut preview = Color32::from_cmyk(c, m, y, k, a);
    ui.horizontal(|ui| {
        ui.label("Preview");
        ui.color_edit_button_srgba(&mut preview);
    });

    if changed {
        brush.color = Color32::from_cmyk(c, m, y, k, a);
    }
}

fn rgba_picker(
    ui: &mut egui::Ui,
    hue: &mut f32,
    sat: &mut f32,
    val: &mut f32,
    alpha: &mut f32,
) -> bool {
    let width = slider_width(ui);
    let mut color_changed = false;

    let tri_side = ui.available_width().clamp(140.0, TRI_SIDE);
    color_changed |= hsva_triangle(ui, *hue, sat, val, tri_side);

    color_changed |= gradient_slider(
        ui,
        width,
        hue,
        "Hue:",
        &|t| Color32::from_hsva(t, 1.0, 1.0, 1.0),
        false,
    );
    color_changed |= gradient_slider(
        ui,
        width,
        val,
        "Brightness:",
        &|t| Color32::from_hsva(*hue, *sat, t, 1.0),
        false,
    );
    color_changed |= gradient_slider(
        ui,
        width,
        sat,
        "Saturation:",
        &|t| Color32::from_hsva(*hue, t, *val, 1.0),
        false,
    );
    color_changed |= gradient_slider(
        ui,
        width,
        alpha,
        "Opacity:",
        &|t| Color32::from_hsva(*hue, *sat, *val, t),
        true,
    );

    color_changed
}
