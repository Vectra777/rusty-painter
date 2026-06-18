use super::PainterApp;
use eframe::egui::{self, Vec2};

impl PainterApp {
    pub(crate) fn rotate_point(
        point: egui::Pos2,
        center: egui::Pos2,
        cos: f32,
        sin: f32,
    ) -> egui::Pos2 {
        let delta = point - center;
        egui::Pos2::new(
            center.x + delta.x * cos - delta.y * sin,
            center.y + delta.x * sin + delta.y * cos,
        )
    }

    pub(crate) fn screen_to_canvas(
        &self,
        pos: egui::Pos2,
        origin: egui::Pos2,
        canvas_center: egui::Pos2,
    ) -> (Vec2, bool) {
        let unrotated = self.unrotate_point_around_center(pos, canvas_center);
        let world_point = canvas_center + unrotated;
        let canvas_point = self.world_to_canvas_coords(world_point, origin);
        let clamped = self.clamp_to_canvas_bounds(canvas_point);
        let is_inside = self.is_point_in_canvas(canvas_point);
        (clamped, is_inside)
    }

    fn unrotate_point_around_center(&self, pos: egui::Pos2, center: egui::Pos2) -> egui::Vec2 {
        let cos = self.viewport.rotation.cos();
        let sin = self.viewport.rotation.sin();
        let delta = pos - center;
        egui::Vec2::new(
            delta.x * cos + delta.y * sin,
            -delta.x * sin + delta.y * cos,
        )
    }

    fn world_to_canvas_coords(&self, world: egui::Pos2, origin: egui::Pos2) -> egui::Pos2 {
        let delta = (world - origin) / self.viewport.zoom;
        egui::Pos2::new(delta.x, delta.y)
    }

    fn clamp_to_canvas_bounds(&self, point: egui::Pos2) -> Vec2 {
        Vec2 {
            x: point.x.clamp(0.0, self.canvas.width() as f32),
            y: point.y.clamp(0.0, self.canvas.height() as f32),
        }
    }

    fn is_point_in_canvas(&self, point: egui::Pos2) -> bool {
        point.x >= 0.0
            && point.y >= 0.0
            && point.x <= self.canvas.width() as f32
            && point.y <= self.canvas.height() as f32
    }

    pub fn draw_transform_overlay(&mut self, painter: &egui::Painter, origin: egui::Pos2) {
        if let super::tools::Tool::Transform(ref mut info) = self.active_tool {
            if info.bounds.is_none() {
                info.bounds = self.canvas.get_content_bounds(
                    self.canvas.active_layer_idx,
                    if self.selection_manager.has_selection() {
                        Some(&self.selection_manager)
                    } else {
                        None
                    },
                );
            }

            if let Some(bounds) = info.bounds {
                let center = Vec2::new(bounds.center().x, bounds.center().y);
                let (sin_r, cos_r) = info.rotation.sin_cos();

                let transform_point = |p: egui::Pos2| -> egui::Pos2 {
                    let dx = p.x - center.x;
                    let dy = p.y - center.y;
                    let sx = dx * info.scale.x;
                    let sy = dy * info.scale.y;
                    let rx = sx * cos_r - sy * sin_r;
                    let ry = sx * sin_r + sy * cos_r;
                    egui::pos2(
                        origin.x + (rx + center.x + info.offset.x) * self.viewport.zoom,
                        origin.y + (ry + center.y + info.offset.y) * self.viewport.zoom,
                    )
                };

                let corners = [
                    bounds.min,
                    eframe::egui::pos2(bounds.max.x, bounds.min.y),
                    bounds.max,
                    eframe::egui::pos2(bounds.min.x, bounds.max.y),
                ];
                let t_corners = [
                    transform_point(corners[0]),
                    transform_point(corners[1]),
                    transform_point(corners[2]),
                    transform_point(corners[3]),
                ];

                let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 120, 255));
                painter.line_segment([t_corners[0], t_corners[1]], stroke);
                painter.line_segment([t_corners[1], t_corners[2]], stroke);
                painter.line_segment([t_corners[2], t_corners[3]], stroke);
                painter.line_segment([t_corners[3], t_corners[0]], stroke);

                let handle_points = [
                    bounds.min,
                    eframe::egui::pos2(bounds.center().x, bounds.min.y),
                    eframe::egui::pos2(bounds.max.x, bounds.min.y),
                    eframe::egui::pos2(bounds.max.x, bounds.center().y),
                    bounds.max,
                    eframe::egui::pos2(bounds.center().x, bounds.max.y),
                    eframe::egui::pos2(bounds.min.x, bounds.max.y),
                    eframe::egui::pos2(bounds.min.x, bounds.center().y),
                ];

                for p in handle_points {
                    let tp = transform_point(p);
                    painter.circle_filled(tp, 4.0, egui::Color32::WHITE);
                    painter.circle_stroke(tp, 4.0, stroke);
                }
            }
        }
    }
}
