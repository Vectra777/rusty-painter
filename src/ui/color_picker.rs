use crate::ColorModel;
use crate::brush_engine::brush::Brush;
use crate::utils::color::Color;
use eframe::egui;
use egui::Color32;

const SLIDER_WIDTH: f32 = 200.0;
const TRI_SIDE: f32 = 200.0;

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

fn hsva_triangle(ui: &mut egui::Ui, hue: &mut f32, sat: &mut f32, val: &mut f32) -> bool {
    let tri_height = TRI_SIDE * (3.0_f32).sqrt() * 0.5;
    let (rect, response) = ui.allocate_at_least(
        egui::vec2(TRI_SIDE, tri_height),
        egui::Sense::click_and_drag(),
    );

    let base_x = rect.center().x - TRI_SIDE * 0.5;
    let base_y = rect.top();
    let tri_top = egui::pos2(base_x + TRI_SIDE * 0.5, base_y);
    let tri_left = egui::pos2(base_x, base_y + tri_height);
    let tri_right = egui::pos2(base_x + TRI_SIDE, base_y + tri_height);

    let hue_color = Color::from_hsva(*hue, 1.0, 1.0, 1.0).to_color32();

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
pub fn color_picker_window(ctx: &egui::Context, brush: &mut Brush, color_model: ColorModel) {
    egui::Window::new("Color Picker")
        .default_size([220.0, 300.0])
        .show(ctx, |ui| match color_model {
            ColorModel::Rgba => rgba_picker(ui, brush),
            ColorModel::Grayscale => grayscale_picker(ui, brush),
            ColorModel::Cmyk => cmyk_picker(ui, brush),
        });
}

fn grayscale_picker(ui: &mut egui::Ui, brush: &mut Brush) {
    let mut value = brush.color.to_grayscale_value();
    let mut alpha = brush.color.a;
    let mut changed = false;

    ui.label("Grayscale");
    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut value,
        "Value",
        &|t| Color::from_gray(t, 1.0).to_color32(),
        false,
    );
    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut alpha,
        "Opacity",
        &|t| Color::from_gray(value, t).to_color32(),
        true,
    );

    let mut preview = Color::from_gray(value, alpha).to_color32();
    ui.horizontal(|ui| {
        ui.label("Preview");
        ui.color_edit_button_srgba(&mut preview);
    });

    if changed {
        brush.color = Color::from_gray(value, alpha);
    }
}

fn cmyk_picker(ui: &mut egui::Ui, brush: &mut Brush) {
    let (mut c, mut m, mut y, mut k, mut a) = brush.color.to_cmyk();
    let mut changed = false;
    let mut color = Color::from_cmyk(c, m, y, k, a);

    ui.label("CMYK");
    let (mut hue, mut sat, mut val, _) = color.to_hsva();
    changed |= hsva_triangle(ui, &mut hue, &mut sat, &mut val);
    if changed {
        color = Color::from_hsva(hue, sat, val, a);
        let (nc, nm, ny, nk, _) = color.to_cmyk();
        c = nc;
        m = nm;
        y = ny;
        k = nk;
    }

    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut c,
        "Cyan",
        &|t| Color::from_cmyk(t, m, y, k, 1.0).to_color32(),
        false,
    );
    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut m,
        "Magenta",
        &|t| Color::from_cmyk(c, t, y, k, 1.0).to_color32(),
        false,
    );
    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut y,
        "Yellow",
        &|t| Color::from_cmyk(c, m, t, k, 1.0).to_color32(),
        false,
    );
    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut k,
        "Key (Black)",
        &|t| Color::from_cmyk(c, m, y, t, 1.0).to_color32(),
        false,
    );
    changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut a,
        "Opacity",
        &|t| Color::from_cmyk(c, m, y, k, t).to_color32(),
        true,
    );

    let mut preview = Color::from_cmyk(c, m, y, k, a).to_color32();
    ui.horizontal(|ui| {
        ui.label("Preview");
        ui.color_edit_button_srgba(&mut preview);
    });

    if changed {
        brush.color = Color::from_cmyk(c, m, y, k, a);
    }
}

fn rgba_picker(ui: &mut egui::Ui, brush: &mut Brush) {
    let (mut hue, mut sat, mut val, mut alpha) = brush.color.to_hsva();
    let mut color_changed = false;

    color_changed |= hsva_triangle(ui, &mut hue, &mut sat, &mut val);

    color_changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut hue,
        "Hue:",
        &|t| Color::from_hsva(t, 1.0, 1.0, 1.0).to_color32(),
        false,
    );
    color_changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut val,
        "Brightness:",
        &|t| Color::from_hsva(hue, sat, t, 1.0).to_color32(),
        false,
    );
    color_changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut sat,
        "Saturation:",
        &|t| Color::from_hsva(hue, t, val, 1.0).to_color32(),
        false,
    );
    color_changed |= gradient_slider(
        ui,
        SLIDER_WIDTH,
        &mut alpha,
        "Opacity:",
        &|t| Color::from_hsva(hue, sat, val, t).to_color32(),
        true,
    );

    if color_changed {
        brush.color = Color::from_hsva(hue, sat, val, alpha);
    }
}
