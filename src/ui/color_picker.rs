use crate::brush_engine::brush::{Brush};
use eframe::egui;
use crate::utils::color::Color;
use egui::Color32;

/// Interactive HSVA picker that updates the active brush color.
pub fn color_picker_window(ctx: &egui::Context, brush: &mut Brush) {
    egui::Window::new("Color Picker")
        .default_size([220.0, 300.0])
        .show(ctx, |ui| {
            let (mut hue, mut sat, mut val, mut alpha) = brush.color.to_hsva();
            let mut color_changed = false;

            // Barycentric coordinate helper to locate cursor inside the triangle.
            fn barycentric(
                p: egui::Pos2,
                a: egui::Pos2,
                b: egui::Pos2,
                c: egui::Pos2,
            ) -> (f32, f32, f32) {
                let v0 = b - a;
                let v1 = c - a;
                let v2 = p - a;
                let denom = v0.x * v1.y - v1.x * v0.y;
                if denom.abs() < f32::EPSILON {
                    return (0.0, 0.0, 0.0);
                }
                let inv_denom = 1.0 / denom;
                let v = (v2.x * v1.y - v1.x * v2.y) * inv_denom;
                let w = (v0.x * v2.y - v2.x * v0.y) * inv_denom;
                let u = 1.0 - v - w;
                (u, v, w)
            }

            let side = 200.0;
            let tri_height = side * (3.0_f32).sqrt() * 0.5;
            let (rect, response) =
                ui.allocate_at_least(egui::vec2(side, tri_height), egui::Sense::click_and_drag());

            let base_x = rect.center().x - side * 0.5;
            let base_y = rect.top();
            let tri_top = egui::pos2(base_x + side * 0.5, base_y);
            let tri_left = egui::pos2(base_x, base_y + tri_height);
            let tri_right = egui::pos2(base_x + side, base_y + tri_height);

            let hue_color = Color::from_hsva(hue, 1.0, 1.0, 1.0).to_color32();

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
            if (response.hovered() || response.dragged()) && pointer_down {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let (w_top, w_left, w_right) =
                        barycentric(pointer, tri_top, tri_left, tri_right);
                    if w_top >= -0.01 && w_left >= -0.01 && w_right >= -0.01 {
                        let mut w_top = w_top.clamp(0.0, 1.0);
                        let w_left = w_left.clamp(0.0, 1.0);
                        let mut w_right = w_right.clamp(0.0, 1.0);
                        let total = w_top + w_left + w_right;
                        if total > 0.0 {
                            w_top /= total;
                            w_right /= total;
                        }
                        sat = w_right;
                        val = (w_top + w_right).clamp(0.0, 1.0);
                        color_changed = true;
                    }
                }
            }

            ui.add_space(8.0);
            fn draw_checkerboard(painter: &egui::Painter, rect: egui::Rect, cell: f32) {
                let rows = ((rect.height() / cell).ceil() as i32).max(1);
                let cols = ((rect.width() / cell).ceil() as i32).max(1);
                for y in 0..rows {
                    for x in 0..cols {
                        let dark = (x + y) % 2 == 0;
                        let min =
                            egui::pos2(rect.left() + x as f32 * cell, rect.top() + y as f32 * cell);
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

            let gradient_slider = |ui: &mut egui::Ui,
                                   value: &mut f32,
                                   label: &str,
                                   color_at: &dyn Fn(f32) -> Color32,
                                   checker: bool|
             -> bool {
                ui.label(label);
                let bar_height = 18.0;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(side, bar_height),
                    egui::Sense::click_and_drag(),
                );
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
                        mesh.indices.extend_from_slice(&[
                            base - 2,
                            base - 1,
                            base,
                            base - 1,
                            base + 1,
                            base,
                        ]);
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
            };

            color_changed |= gradient_slider(
                ui,
                &mut hue,
                "Hue:",
                &|t| Color::from_hsva(t, 1.0, 1.0, 1.0).to_color32(),
                false,
            );
            color_changed |= gradient_slider(
                ui,
                &mut val,
                "Brightness:",
                &|t| Color::from_hsva(hue, sat, t, 1.0).to_color32(),
                false,
            );
            color_changed |= gradient_slider(
                ui,
                &mut sat,
                "Saturation:",
                &|t| Color::from_hsva(hue, t, val, 1.0).to_color32(),
                false,
            );
            color_changed |= gradient_slider(
                ui,
                &mut alpha,
                "Opacity:",
                &|t| Color::from_hsva(hue, sat, val, t).to_color32(),
                true,
            );

            if color_changed {
                brush.color = Color::from_hsva(hue, sat, val, alpha);
            }
        });
}
