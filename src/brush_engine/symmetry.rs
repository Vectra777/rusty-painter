//! Mirror painting: every dab is repeated across one or two axes, or around
//! a centre (mandala).
//!
//! The copies join the original dabs' batch, so a mirrored stroke is still
//! one stroke: one undo step, shared pressure, spacing and jitter, and the
//! same indirect painting (overlapping copies build up like a stroke
//! crossing itself, and never beyond the stroke's opacity in wash mode).

use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SymmetryMode {
    #[default]
    Off,
    /// Mirrored left/right across a vertical axis.
    Vertical,
    /// Mirrored top/bottom across a horizontal axis.
    Horizontal,
    /// Both axes: four copies.
    Both,
    /// `count` copies rotated around the centre, optionally mirrored too.
    Radial,
}

/// Symmetry settings, in canvas coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Symmetry {
    pub mode: SymmetryMode,
    /// Where the axes cross (the centre of rotation).
    pub center: Vec2,
    /// Rotation of the axes, radians.
    pub angle: f32,
    /// Radial: number of copies, including the original (2..=32).
    pub count: u32,
    /// Radial: also mirror each copy (kaleidoscope).
    pub mirrored: bool,
}

impl Default for Symmetry {
    fn default() -> Self {
        Self {
            mode: SymmetryMode::Off,
            center: Vec2::ZERO,
            angle: 0.0,
            count: 6,
            mirrored: false,
        }
    }
}

/// A linear map about the symmetry centre: `p' = center + m · (p - center)`.
/// `m` is row-major `[a, b, c, d]`: `x' = a x + b y`, `y' = c x + d y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Copy2 {
    pub m: [f32; 4],
}

impl Copy2 {
    fn rotation(theta: f32) -> Self {
        let (s, c) = theta.sin_cos();
        Self { m: [c, -s, s, c] }
    }

    /// Reflection across the line through the centre with direction `angle`.
    fn reflection(angle: f32) -> Self {
        let (s, c) = (2.0 * angle).sin_cos();
        Self { m: [c, s, s, -c] }
    }

    fn apply(&self, v: Vec2) -> Vec2 {
        let [a, b, c, d] = self.m;
        Vec2::new(a * v.x + b * v.y, c * v.x + d * v.y)
    }

    /// How the brush tip is turned for this copy (from canvas offsets to
    /// tip offsets): the inverse map, which for rotations and reflections
    /// is the transpose.
    pub fn tip_orientation(&self) -> [f32; 4] {
        let [a, b, c, d] = self.m;
        [a, c, b, d]
    }
}

/// The largest radial count.
pub const MAX_COUNT: u32 = 32;

/// Copies closer than this to another copy of the same dab are dropped (on
/// an axis, at the centre), so they don't paint twice.
const MERGE_DISTANCE: f32 = 0.5;

impl Symmetry {
    pub fn is_active(&self) -> bool {
        self.mode != SymmetryMode::Off
    }

    /// The maps making the copies of each dab (not including the original).
    pub fn copies(&self) -> Vec<Copy2> {
        use std::f32::consts::{FRAC_PI_2, TAU};
        // Direction of the vertical axis (a mirror across it swaps left and
        // right) and of the horizontal one.
        let vertical = FRAC_PI_2 + self.angle;
        let horizontal = self.angle;
        match self.mode {
            SymmetryMode::Off => Vec::new(),
            SymmetryMode::Vertical => vec![Copy2::reflection(vertical)],
            SymmetryMode::Horizontal => vec![Copy2::reflection(horizontal)],
            SymmetryMode::Both => vec![
                Copy2::reflection(vertical),
                Copy2::reflection(horizontal),
                Copy2::rotation(std::f32::consts::PI),
            ],
            SymmetryMode::Radial => {
                let n = self.count.clamp(2, MAX_COUNT);
                let step = TAU / n as f32;
                let mut copies: Vec<Copy2> =
                    (1..n).map(|k| Copy2::rotation(step * k as f32)).collect();
                if self.mirrored {
                    copies.extend(
                        (0..n).map(|k| Copy2::reflection(vertical + step * 0.5 * k as f32)),
                    );
                }
                copies
            }
        }
    }

    /// `centers` followed by their copies (each copy after the originals),
    /// with the tip orientation of each dab.
    pub fn expand(&self, copies: &[Copy2], centers: &[Vec2]) -> (Vec<Vec2>, Vec<[f32; 4]>) {
        let identity = [1.0, 0.0, 0.0, 1.0];
        let mut out = Vec::with_capacity(centers.len() * (copies.len() + 1));
        let mut orients = Vec::with_capacity(out.capacity());
        out.extend_from_slice(centers);
        orients.resize(centers.len(), identity);
        let mut placed: Vec<Vec2> = Vec::with_capacity(copies.len() + 1);
        for &c in centers {
            placed.clear();
            placed.push(c);
            let offset = c - self.center;
            for copy in copies {
                let p = self.center + copy.apply(offset);
                if placed
                    .iter()
                    .any(|q| (*q - p).length_sq() < MERGE_DISTANCE * MERGE_DISTANCE)
                {
                    continue;
                }
                placed.push(p);
                out.push(p);
                orients.push(copy.tip_orientation());
            }
        }
        (out, orients)
    }

    /// `p` mapped by one of the copies.
    pub fn map(&self, copy: &Copy2, p: Vec2) -> Vec2 {
        self.center + copy.apply(p - self.center)
    }

    /// Where one dab at `p` is repeated: `(copy number, position)`, the
    /// original being copy 0. Copy numbers stay the same along a stroke
    /// (copies merged at an axis are skipped, not renumbered), for tools
    /// that keep state per copy.
    pub fn positions(&self, copies: &[Copy2], p: Vec2) -> Vec<(usize, Vec2)> {
        let mut out = vec![(0, p)];
        let offset = p - self.center;
        for (k, copy) in copies.iter().enumerate() {
            let q = self.center + copy.apply(offset);
            if out
                .iter()
                .all(|(_, o)| (*o - q).length_sq() >= MERGE_DISTANCE * MERGE_DISTANCE)
            {
                out.push((k + 1, q));
            }
        }
        out
    }

    /// The axis lines through the centre, as directions, for the guides.
    pub fn axis_directions(&self) -> Vec<Vec2> {
        use std::f32::consts::{FRAC_PI_2, PI};
        let dir = |a: f32| Vec2::new(a.cos(), a.sin());
        match self.mode {
            SymmetryMode::Off => Vec::new(),
            SymmetryMode::Vertical => vec![dir(FRAC_PI_2 + self.angle)],
            SymmetryMode::Horizontal => vec![dir(self.angle)],
            SymmetryMode::Both => vec![dir(FRAC_PI_2 + self.angle), dir(self.angle)],
            SymmetryMode::Radial => {
                let n = self.count.clamp(2, MAX_COUNT);
                // Spokes between the copies' sectors (half lines).
                (0..n)
                    .map(|k| dir(-FRAC_PI_2 + self.angle + 2.0 * PI * k as f32 / n as f32))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn vertical_mirrors_left_and_right() {
        let s = Symmetry {
            mode: SymmetryMode::Vertical,
            center: Vec2::new(50.0, 50.0),
            ..Default::default()
        };
        let (out, _) = s.expand(&s.copies(), &[Vec2::new(40.0, 30.0)]);
        assert_eq!(out.len(), 2);
        assert!(close(out[1], Vec2::new(60.0, 30.0)), "{:?}", out[1]);
    }

    #[test]
    fn both_axes_make_four_copies() {
        let s = Symmetry {
            mode: SymmetryMode::Both,
            center: Vec2::new(50.0, 50.0),
            ..Default::default()
        };
        let (out, _) = s.expand(&s.copies(), &[Vec2::new(40.0, 30.0)]);
        let expected = [(40.0, 30.0), (60.0, 30.0), (40.0, 70.0), (60.0, 70.0)];
        assert_eq!(out.len(), 4);
        for (x, y) in expected {
            assert!(
                out.iter().any(|p| close(*p, Vec2::new(x, y))),
                "missing ({x}, {y})"
            );
        }
    }

    #[test]
    fn radial_copies_share_the_distance_to_the_centre() {
        for mirrored in [false, true] {
            let s = Symmetry {
                mode: SymmetryMode::Radial,
                center: Vec2::new(0.0, 0.0),
                count: 5,
                mirrored,
                ..Default::default()
            };
            let (out, _) = s.expand(&s.copies(), &[Vec2::new(100.0, 30.0)]);
            assert_eq!(out.len(), if mirrored { 10 } else { 5 });
            for p in &out {
                assert!((p.length() - Vec2::new(100.0, 30.0).length()).abs() < 1e-2);
            }
        }
    }

    #[test]
    fn copies_on_the_axis_are_merged() {
        let s = Symmetry {
            mode: SymmetryMode::Radial,
            center: Vec2::new(20.0, 20.0),
            count: 8,
            mirrored: true,
            ..Default::default()
        };
        let (out, _) = s.expand(&s.copies(), &[Vec2::new(20.0, 20.0), Vec2::new(20.2, 20.0)]);
        assert_eq!(out.len(), 2, "a dab at the centre paints once: {out:?}");
    }

    #[test]
    fn tip_orientation_undoes_the_copy() {
        let s = Symmetry {
            mode: SymmetryMode::Radial,
            count: 7,
            mirrored: true,
            ..Default::default()
        };
        for copy in s.copies() {
            let v = Vec2::new(3.0, -2.0);
            let there = copy.apply(v);
            let [a, b, c, d] = copy.tip_orientation();
            let back = Vec2::new(a * there.x + b * there.y, c * there.x + d * there.y);
            assert!(close(back, v));
        }
    }
}
