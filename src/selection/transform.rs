//! The Transform tool's state: offset, rotation and scale, a four-corner
//! perspective or a grid warp, and the handles for each.

use crate::canvas::storage::warp::{Warp, WarpGrid};
use crate::canvas::storage::{Distort, DistortKind, TransformParams};
use eframe::egui::Rect;
use eframe::egui::Vec2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformState {
    None,
    Moving,
    Rotating,
    Scaling(usize), // Index of the handle (0-7)
    /// Dragging one point: a perspective corner (0-3, clockwise from
    /// top-left) or a warp grid point (row by row).
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
    /// Perspective mode: where each corner of `bounds` goes (top-left,
    /// top-right, bottom-right, bottom-left). Replaces offset/rotation/scale
    /// while set.
    pub corners: Option<[Vec2; 4]>,
    /// Distort mode: the warp grid's points. Replaces the rest while set.
    pub warp: Option<WarpGrid>,
    /// The point mode last used (kept across sessions).
    pub distort_kind: DistortKind,
    /// Points along a side of a new warp grid.
    pub warp_size: usize,
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
            warp: None,
            distort_kind: DistortKind::Perspective,
            warp_size: 4,
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
        if let (Some(w), Some(b)) = (self.warp, self.bounds) {
            let straight = WarpGrid::regular(b, w.n);
            return w
                .used()
                .iter()
                .zip(straight.used())
                .all(|(a, b)| (*a - *b).length() < 1e-3);
        }
        if let (Some(c), Some(b)) = (self.corners, self.bounds) {
            let src = crate::canvas::storage::rect_corners(b);
            return c.iter().zip(src).all(|(a, b)| (*a - b).length() < 1e-3);
        }
        self.offset == Vec2::ZERO && self.rotation == 0.0 && self.scale == Vec2::new(1.0, 1.0)
    }

    /// The canvas transform this session describes.
    pub fn params(&self) -> TransformParams {
        let center = self.bounds.map_or(Vec2::ZERO, |b| b.center().to_vec2());
        match (self.warp, self.corners, self.bounds) {
            (Some(grid), _, Some(src)) => TransformParams::warped(Warp { src, grid }),
            (None, Some(dst), Some(src)) => TransformParams::distorted(Distort { src, dst }),
            _ => TransformParams::new(self.offset, self.rotation, self.scale, center),
        }
    }

    /// The point mode in use: perspective, distort (warp), or free (`None`).
    pub fn point_mode(&self) -> Option<DistortKind> {
        if self.warp.is_some() {
            Some(DistortKind::Warp)
        } else if self.corners.is_some() {
            Some(DistortKind::Perspective)
        } else {
            None
        }
    }

    /// The draggable points of a perspective or warp.
    pub fn points_mut(&mut self) -> Option<&mut [Vec2]> {
        match (&mut self.warp, &mut self.corners) {
            (Some(w), _) => Some(w.used_mut()),
            (None, Some(c)) => Some(&mut c[..]),
            _ => None,
        }
    }

    /// Switch to `mode`, starting from what the box shows now (the new
    /// points sit where the picture already is).
    pub fn set_point_mode(&mut self, mode: Option<DistortKind>) {
        if mode == self.point_mode() {
            return;
        }
        let Some(bounds) = self.bounds else {
            // No box yet: remember the mode for when there is one.
            self.warp = None;
            self.corners = None;
            if let Some(kind) = mode {
                self.distort_kind = kind;
                match kind {
                    DistortKind::Perspective => self.corners = Some([Vec2::ZERO; 4]),
                    DistortKind::Warp => {
                        self.warp = Some(WarpGrid::regular(Rect::NOTHING, self.warp_size))
                    }
                }
            }
            return;
        };
        let params = self.params();
        let corners = crate::canvas::storage::rect_corners(bounds).map(|p| params.forward(p));
        let mut grid = WarpGrid::regular(bounds, self.warp_size);
        for p in grid.used_mut() {
            *p = params.forward(*p);
        }
        self.warp = None;
        self.corners = None;
        match mode {
            Some(DistortKind::Perspective) => self.corners = Some(corners),
            Some(DistortKind::Warp) => self.warp = Some(grid),
            // Back to free: the move / turn / scale from before the points
            // were used; what only points can do is dropped.
            None => {}
        }
        if let Some(kind) = mode {
            self.distort_kind = kind;
        }
    }

    /// A finer or coarser warp grid, keeping the picture's shape.
    pub fn set_warp_size(&mut self, n: usize) {
        self.warp_size = n;
        if let Some(w) = self.warp.as_mut() {
            *w = w.resized(n);
        }
    }

    /// Where the corners of `bounds` currently are (top-left clockwise).
    pub fn quad(&self) -> Option<[Vec2; 4]> {
        let bounds = self.bounds?;
        if let Some(w) = self.warp {
            let n = w.n;
            let p = w.used();
            return Some([p[0], p[n - 1], p[n * n - 1], p[n * (n - 1)]]);
        }
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
        if let Some(w) = self.warp {
            return w.used().to_vec();
        }
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
        let mode = self.point_mode();
        *self = TransformInfo {
            bounds,
            distort_kind: self.distort_kind,
            warp_size: self.warp_size,
            ..TransformInfo::default()
        };
        // Without a box yet, placeholder points just remember the mode.
        let b = bounds.unwrap_or(Rect::NOTHING);
        match mode {
            Some(DistortKind::Perspective) => {
                self.corners = Some(crate::canvas::storage::rect_corners(b))
            }
            Some(DistortKind::Warp) => self.warp = Some(WarpGrid::regular(b, self.warp_size)),
            None => {}
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
        self.warp = None;
    }

    pub fn hit_test(&self, pos: Vec2, zoom: f32) -> TransformState {
        if self.bounds.is_none() {
            return TransformState::None;
        }
        let handle_radius = 10.0 / zoom;
        let handles = self.handles();
        // The nearest handle in reach (a fine grid's points sit close).
        let nearest = handles
            .iter()
            .enumerate()
            .map(|(i, h)| (i, (pos - *h).length()))
            .filter(|&(_, d)| d < handle_radius)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = nearest {
            {
                return if self.point_mode().is_some() {
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
        if self.point_mode().is_some() {
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
