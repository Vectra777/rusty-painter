use crate::canvas::storage::{Distort, TransformParams};
use eframe::egui::Rect;
use eframe::egui::Vec2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformState {
    None,
    Moving,
    Rotating,
    Scaling(usize), // Index of the handle (0-7)
    /// Dragging one corner of the distort quad (0-3, clockwise from top-left).
    Corner(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformInfo {
    pub start_pos: Option<Vec2>,
    pub offset: Vec2,
    pub rotation: f32,
    pub scale: Vec2,
    pub bounds: Option<Rect>,
    pub state: TransformState,
    /// Distort mode: where each corner of `bounds` goes (top-left,
    /// top-right, bottom-right, bottom-left). Replaces offset/rotation/scale
    /// while set.
    pub corners: Option<[Vec2; 4]>,
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
            corners: None,
        }
    }
}

/// The 8 free-transform handles of `bounds`, clockwise from top-left
/// (corners at even indices, edge midpoints at odd ones).
fn handle_sources(bounds: Rect) -> [Vec2; 8] {
    let (min, max, c) = (bounds.min, bounds.max, bounds.center());
    [
        Vec2::new(min.x, min.y),
        Vec2::new(c.x, min.y),
        Vec2::new(max.x, min.y),
        Vec2::new(max.x, c.y),
        Vec2::new(max.x, max.y),
        Vec2::new(c.x, max.y),
        Vec2::new(min.x, max.y),
        Vec2::new(min.x, c.y),
    ]
}

impl TransformInfo {
    /// Whether anything moved since the session started.
    pub fn is_identity(&self) -> bool {
        if let (Some(c), Some(b)) = (self.corners, self.bounds) {
            let src = crate::canvas::storage::rect_corners(b);
            return c.iter().zip(src).all(|(a, b)| (*a - b).length() < 1e-3);
        }
        self.offset == Vec2::ZERO && self.rotation == 0.0 && self.scale == Vec2::new(1.0, 1.0)
    }

    /// The canvas transform this session describes.
    pub fn params(&self) -> TransformParams {
        let center = self.bounds.map_or(Vec2::ZERO, |b| b.center().to_vec2());
        match (self.corners, self.bounds) {
            (Some(dst), Some(src)) => TransformParams::distorted(Distort { src, dst }),
            _ => TransformParams::new(self.offset, self.rotation, self.scale, center),
        }
    }

    /// Where the corners of `bounds` currently are (top-left clockwise).
    pub fn quad(&self) -> Option<[Vec2; 4]> {
        let bounds = self.bounds?;
        if let Some(c) = self.corners {
            return Some(c);
        }
        let params = self.params();
        Some(crate::canvas::storage::rect_corners(bounds).map(|p| params.forward(p)))
    }

    /// Handle positions in canvas space: 8 in free mode, the 4 quad corners
    /// in distort mode.
    pub fn handles(&self) -> Vec<Vec2> {
        let Some(bounds) = self.bounds else {
            return Vec::new();
        };
        if let Some(c) = self.corners {
            return c.to_vec();
        }
        let params = self.params();
        handle_sources(bounds)
            .iter()
            .map(|&p| params.forward(p))
            .collect()
    }

    /// Start over with a new box, keeping the free/distort mode.
    pub fn reset_to(&mut self, bounds: Option<Rect>) {
        let distort = self.corners.is_some();
        *self = TransformInfo {
            bounds,
            ..TransformInfo::default()
        };
        if distort {
            // Without a box yet, placeholder corners just remember the mode.
            self.corners = Some(self.quad().unwrap_or([Vec2::ZERO; 4]));
        }
    }

    /// Enter distort mode, starting from the current free transform.
    pub fn begin_distort(&mut self) {
        if self.corners.is_none() {
            self.corners = self.quad();
        }
    }

    /// Back to free mode (drops the distortion).
    pub fn end_distort(&mut self) {
        self.corners = None;
    }

    pub fn hit_test(&self, pos: Vec2, zoom: f32) -> TransformState {
        if self.bounds.is_none() {
            return TransformState::None;
        }
        let handle_radius = 10.0 / zoom;
        let handles = self.handles();
        for (i, h) in handles.iter().enumerate() {
            if (pos - *h).length() < handle_radius {
                return if self.corners.is_some() {
                    TransformState::Corner(i)
                } else {
                    TransformState::Scaling(i)
                };
            }
        }

        if let Some(quad) = self.quad()
            && point_in_quad(pos, &quad)
        {
            return TransformState::Moving;
        }
        if self.corners.is_some() {
            // Outside a distorted quad: dragging still moves it.
            return TransformState::Moving;
        }
        // Outside the box rotates, like most editors.
        TransformState::Rotating
    }
}

/// Whether `p` is inside the (possibly non-convex) quad.
fn point_in_quad(p: Vec2, quad: &[Vec2; 4]) -> bool {
    let mut inside = false;
    let mut j = 3;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    fn info() -> TransformInfo {
        TransformInfo {
            bounds: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0))),
            ..TransformInfo::default()
        }
    }

    #[test]
    fn rotated_box_keeps_its_handles_rotated() {
        let mut t = info();
        t.rotation = std::f32::consts::FRAC_PI_2;
        let h = t.handles();
        // Top-left corner (0,0) rotated 90° about (50,25) lands at (75,-25).
        assert!((h[0] - Vec2::new(75.0, -25.0)).length() < 1e-3);
        assert_eq!(
            t.hit_test(Vec2::new(75.0, -25.0), 1.0),
            TransformState::Scaling(0)
        );
        assert_eq!(
            t.hit_test(Vec2::new(50.0, 25.0), 1.0),
            TransformState::Moving
        );
    }

    #[test]
    fn distort_starts_from_the_free_transform() {
        let mut t = info();
        t.offset = Vec2::new(10.0, 5.0);
        t.begin_distort();
        let c = t.corners.unwrap();
        assert!((c[0] - Vec2::new(10.0, 5.0)).length() < 1e-3);
        assert!((c[2] - Vec2::new(110.0, 55.0)).length() < 1e-3);
        assert_eq!(
            t.hit_test(Vec2::new(110.0, 55.0), 1.0),
            TransformState::Corner(2)
        );
        t.end_distort();
        assert!(t.corners.is_none());
    }

    #[test]
    fn identity_detection() {
        let mut t = info();
        assert!(t.is_identity());
        t.begin_distort();
        assert!(t.is_identity());
        t.corners.as_mut().unwrap()[1].x += 4.0;
        assert!(!t.is_identity());
    }
}
