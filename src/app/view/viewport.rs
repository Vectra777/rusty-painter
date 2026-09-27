use crate::app::PainterApp;
use eframe::egui::{self, Vec2};

pub(crate) const MIN_ZOOM: f32 = 0.02;
pub(crate) const MAX_ZOOM: f32 = 32.0;

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

    /// Canvas position of screen point `pos`, clamped to the canvas, and
    /// whether it was inside.
    pub(crate) fn screen_to_canvas(
        &self,
        pos: egui::Pos2,
        origin: egui::Pos2,
        canvas_center: egui::Pos2,
    ) -> (Vec2, bool) {
        let raw = self.screen_to_canvas_raw(pos, origin, canvas_center);
        let canvas_point = egui::pos2(raw.x, raw.y);
        let clamped = self.clamp_to_canvas_bounds(canvas_point);
        let is_inside = self.is_point_in_canvas(canvas_point);
        (clamped, is_inside)
    }

    /// Canvas position of screen point `pos`, not clamped: brush strokes
    /// keep their real path outside the canvas and dabs are clipped to it,
    /// so a stroke that leaves and comes back paints only where it's inside.
    pub(crate) fn screen_to_canvas_raw(
        &self,
        pos: egui::Pos2,
        origin: egui::Pos2,
        canvas_center: egui::Pos2,
    ) -> Vec2 {
        let unrotated = self.unrotate_point_around_center(pos, canvas_center);
        let world_point = canvas_center + unrotated;
        let p = self.world_to_canvas_coords(world_point, origin);
        // A flipped view shows the canvas mirrored about its centre.
        let x = if self.viewport.flip_x {
            self.canvas.width() as f32 - p.x
        } else {
            p.x
        };
        Vec2::new(x, p.y)
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

    /// Set the zoom to `new_zoom`, keeping the canvas point under `anchor`
    /// (a screen position) fixed. `area_min` is the canvas area's top-left.
    pub(crate) fn zoom_about(&mut self, anchor: egui::Pos2, area_min: egui::Pos2, new_zoom: f32) {
        let new_zoom = new_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let old_zoom = self.viewport.zoom;
        let half = egui::vec2(self.canvas.width() as f32, self.canvas.height() as f32) * 0.5;
        let origin = area_min + self.viewport.offset;
        // Rotated offset of the anchored canvas point from the canvas center,
        // in canvas units; rotation is about the center, so it is unchanged
        // by zooming.
        let v = (anchor - origin) / old_zoom - half;
        let new_origin = anchor - (half + v) * new_zoom;
        self.viewport.offset = new_origin - area_min;
        self.viewport.zoom = new_zoom;
        self.workspace.auto_fit = false;
    }

    /// Rotate the view by `delta` radians about the screen point `anchor`,
    /// keeping the canvas point under it fixed.
    pub(crate) fn rotate_about(&mut self, anchor: egui::Pos2, area_min: egui::Pos2, delta: f32) {
        let size = egui::vec2(self.canvas.width() as f32, self.canvas.height() as f32)
            * self.viewport.zoom;
        let center = area_min + self.viewport.offset + size * 0.5;
        // Rotating about the canvas center moves the anchored content to
        // `center + R·v`; shifting the view by the difference puts it back.
        let v = anchor - center;
        let (sin, cos) = delta.sin_cos();
        let moved = center + egui::vec2(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
        self.viewport.rotation += delta;
        self.viewport.offset += anchor - moved;
    }

    /// Zoom by `factor` about the center of the canvas area.
    pub(crate) fn zoom_by_from_center(&mut self, factor: f32) {
        if let Some(area) = self.viewport.canvas_area {
            self.zoom_about(area.center(), area.min, self.viewport.zoom * factor);
        }
    }

    /// Zoom to exactly `zoom` about the center of the canvas area.
    pub(crate) fn set_zoom_from_center(&mut self, zoom: f32) {
        if let Some(area) = self.viewport.canvas_area {
            self.zoom_about(area.center(), area.min, zoom);
        }
    }

    /// Fit the whole canvas in view, unrotated.
    pub(crate) fn fit_view(&mut self) {
        self.viewport.rotation = 0.0;
        self.workspace.auto_fit = true;
        self.workspace.fitted_to = None;
    }

    /// Set the brush color to the composited canvas color at `pos`, keeping
    /// the brush's own alpha.
    pub(crate) fn pick_color(&mut self, pos: Vec2) {
        let x = (pos.x.max(0.0) as usize).min(self.canvas.width().saturating_sub(1));
        let y = (pos.y.max(0.0) as usize).min(self.canvas.height().saturating_sub(1));
        let mut image = egui::ColorImage::new([1, 1], egui::Color32::TRANSPARENT);
        self.canvas
            .write_region_to_color_image(x, y, 1, 1, &mut image, 1);
        let Some(&sample) = image.pixels.first() else {
            return;
        };
        let [r, g, b, _] = sample.to_srgba_unmultiplied();
        let alpha = self.brush_state.brush.brush_options.color.a();
        self.brush_state.brush.brush_options.color =
            egui::Color32::from_rgba_unmultiplied(r, g, b, alpha);
        self.brush_state.brush_preview.dirty = true;
    }

    pub fn draw_transform_overlay(
        &mut self,
        painter: &egui::Painter,
        map: &crate::app::view::render::ScreenMap,
    ) {
        let crate::app::tools::Tool::Transform(ref mut info) = self.active_tool else {
            return;
        };
        if info.bounds.is_none() {
            let selection = self
                .selection_manager
                .has_selection()
                .then_some(&self.selection_manager);
            info.reset_to(
                self.canvas
                    .get_content_bounds(self.canvas.active_layer_idx, selection),
            );
        }
        let Some(quad) = info.quad() else {
            return;
        };
        let to_screen = |p: Vec2| map.to_screen(p);
        let pts: Vec<egui::Pos2> = quad.iter().map(|&p| to_screen(p)).collect();

        // Black under white reads on any artwork.
        let dark = egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(200));
        let light = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
        for stroke in [dark, light] {
            for i in 0..4 {
                painter.line_segment([pts[i], pts[(i + 1) % 4]], stroke);
            }
        }
        if info.corners.is_some() {
            // Distort: diagonals hint at the perspective.
            let faint = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(70));
            painter.line_segment([pts[0], pts[2]], faint);
            painter.line_segment([pts[1], pts[3]], faint);
        }

        let half = if self.workspace.touch_mode { 7.0 } else { 4.5 };
        for h in info.handles() {
            let r = egui::Rect::from_center_size(to_screen(h), egui::vec2(half * 2.0, half * 2.0));
            painter.rect_filled(r.expand(1.0), 0.0, egui::Color32::BLACK);
            painter.rect_filled(r, 0.0, egui::Color32::WHITE);
        }
    }
}
