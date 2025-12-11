use crate::utils::vector::Vec2;
use eframe::egui::Rect;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformState {
    None,
    Moving,
    Rotating,
    Scaling(usize), // Index of the handle (0-7)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformInfo {
    pub start_pos: Option<Vec2>,
    pub offset: Vec2,
    pub rotation: f32,
    pub scale: Vec2,
    pub bounds: Option<Rect>,
    pub state: TransformState,
}

impl Default for TransformInfo {
    fn default() -> Self {
        Self {
            start_pos: None,
            offset: Vec2 { x: 0.0, y: 0.0 },
            rotation: 0.0,
            scale: Vec2 { x: 1.0, y: 1.0 },
            bounds: None,
            state: TransformState::None,
        }
    }
}

impl TransformInfo {
    pub fn hit_test(&self, pos: Vec2, zoom: f32) -> TransformState {
        if let Some(bounds) = self.bounds {
            let center = Vec2::new(bounds.center().x, bounds.center().y);

            // Transform the bounds corners
            let corners = [
                bounds.min, // Top-Left
                eframe::egui::pos2(bounds.center().x, bounds.min.y), // Top-Center
                eframe::egui::pos2(bounds.max.x, bounds.min.y), // Top-Right
                eframe::egui::pos2(bounds.max.x, bounds.center().y), // Right-Center
                bounds.max, // Bottom-Right
                eframe::egui::pos2(bounds.center().x, bounds.max.y), // Bottom-Center
                eframe::egui::pos2(bounds.min.x, bounds.max.y), // Bottom-Left
                eframe::egui::pos2(bounds.min.x, bounds.center().y), // Left-Center
            ];

            let (sin_r, cos_r) = self.rotation.sin_cos();
            let handle_radius = 10.0 / zoom; // Adjust handle size by zoom

            for (i, corner) in corners.iter().enumerate() {
                // Apply transform to corner
                let dx = corner.x - center.x;
                let dy = corner.y - center.y;

                let sx = dx * self.scale.x;
                let sy = dy * self.scale.y;

                let rx = sx * cos_r - sy * sin_r;
                let ry = sx * sin_r + sy * cos_r;

                let tx = rx + center.x + self.offset.x;
                let ty = ry + center.y + self.offset.y;

                let dist = ((pos.x - tx).powi(2) + (pos.y - ty).powi(2)).sqrt();
                if dist < handle_radius {
                    return TransformState::Scaling(i);
                }
            }

            // Check if inside for moving
            // Inverse transform the mouse pos to check against original AABB
            let dx = pos.x - (center.x + self.offset.x);
            let dy = pos.y - (center.y + self.offset.y);

            let rx = dx * cos_r + dy * sin_r; // Inverse rotate
            let ry = -dx * sin_r + dy * cos_r;

            let sx = rx / self.scale.x; // Inverse scale
            let sy = ry / self.scale.y;

            let lx = sx + center.x;
            let ly = sy + center.y;

            if bounds.contains(eframe::egui::pos2(lx, ly)) {
                return TransformState::Moving;
            }

            // Check for rotation (outside corners)
            for (i, corner) in corners.iter().enumerate() {
                // Only corners: 0, 2, 4, 6
                if i % 2 != 0 {
                    continue;
                }

                let dx = corner.x - center.x;
                let dy = corner.y - center.y;

                let sx = dx * self.scale.x;
                let sy = dy * self.scale.y;

                let rx = sx * cos_r - sy * sin_r;
                let ry = sx * sin_r + sy * cos_r;

                let tx = rx + center.x + self.offset.x;
                let ty = ry + center.y + self.offset.y;

                let dist = ((pos.x - tx).powi(2) + (pos.y - ty).powi(2)).sqrt();
                if dist < handle_radius * 3.0 {
                    // Larger radius for rotation
                    return TransformState::Rotating;
                }
            }
        }
        TransformState::None
    }
}
