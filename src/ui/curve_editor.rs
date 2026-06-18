use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
use eframe::egui;

pub(crate) fn curve_editor(ui: &mut egui::Ui, curve: &mut SoftnessCurve) -> bool {
    let mut changed = false;
    let size = egui::Vec2::new(ui.available_width(), 150.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
    let rect = response.rect;

    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(30));

    let to_screen = |p: &CurvePoint| -> egui::Pos2 {
        egui::Pos2::new(
            rect.min.x + p.x * rect.width(),
            rect.max.y - p.y * rect.height(),
        )
    };
    let from_screen = |pos: egui::Pos2| -> CurvePoint {
        CurvePoint {
            x: ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
            y: ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0),
        }
    };

    for y in [0.0, 1.0] {
        painter.line_segment(
            [
                to_screen(&CurvePoint { x: 0.0, y }),
                to_screen(&CurvePoint { x: 1.0, y }),
            ],
            egui::Stroke::new(1.0, egui::Color32::GRAY),
        );
    }

    if curve.points.len() >= 2 {
        let mut points = Vec::with_capacity(101);
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            points.push(to_screen(&CurvePoint {
                x: t,
                y: curve.eval(t),
            }));
        }
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE),
        ));
    }

    let dragged_point_id = ui.make_persistent_id("curve_dragged_point");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(dragged_point_id));

    if dragging.is_none()
        && response.drag_started()
        && let Some(pointer_pos) = response.interact_pointer_pos().or(response.hover_pos())
    {
        dragging = curve
            .points
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let dist = to_screen(p).distance(pointer_pos);
                (dist < 15.0).then_some((i, dist))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        ui.data_mut(|d| d.insert_temp(dragged_point_id, dragging));
    }

    if let Some(idx) = dragging {
        if ui.input(|i| i.pointer.primary_down()) {
            if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                let new_p = from_screen(pointer_pos);
                let len = curve.points.len();
                if idx < len {
                    if idx == 0 || idx == len - 1 {
                        curve.points[idx].y = new_p.y;
                    } else {
                        let prev_x = curve.points[idx - 1].x;
                        let next_x = curve.points[idx + 1].x;
                        let p = &mut curve.points[idx];
                        p.x = new_p.x.clamp(prev_x + 0.01, next_x - 0.01);
                        p.y = new_p.y;
                    }
                    changed = true;
                }
            }
            ui.data_mut(|d| d.insert_temp(dragged_point_id, dragging));
            ui.ctx().request_repaint();
        } else {
            ui.data_mut(|d| d.remove_temp::<Option<usize>>(dragged_point_id));
        }
    }

    if response.double_clicked()
        && let Some(pointer_pos) = response.interact_pointer_pos().or(response.hover_pos())
    {
        let new_p = from_screen(pointer_pos);
        let clicked_point_idx = curve
            .points
            .iter()
            .enumerate()
            .find_map(|(i, p)| (to_screen(p).distance(pointer_pos) < 10.0).then_some(i));

        if let Some(idx) = clicked_point_idx {
            if idx > 0 && idx < curve.points.len() - 1 {
                curve.points.remove(idx);
                changed = true;
            }
        } else if let Some(insert_idx) = curve.points.iter().position(|p| new_p.x < p.x)
            && insert_idx > 0
        {
            curve.points.insert(insert_idx, new_p);
            changed = true;
        }
    }

    for (i, p) in curve.points.iter().enumerate() {
        let center = to_screen(p);
        let is_being_dragged = Some(i) == dragging;
        let radius = if is_being_dragged { 6.0 } else { 4.0 };
        let color = if is_being_dragged {
            egui::Color32::WHITE
        } else {
            egui::Color32::YELLOW
        };
        painter.circle_filled(center, radius, color);
        painter.circle_stroke(center, radius, egui::Stroke::new(1.0, egui::Color32::BLACK));
    }

    changed
}
