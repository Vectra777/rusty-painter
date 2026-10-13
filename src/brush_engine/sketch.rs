//! Sketch brush: besides its own line, each new
//! point of the stroke is joined by fine lines to earlier points of the
//! stroke nearby, so going back and forth over an area builds up a web of
//! shading, the way a quick pencil sketch does.

use eframe::egui::Vec2;

/// A sketch brush's lines.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Sketch {
    /// How far back an earlier point can be to be joined, canvas pixels.
    pub reach: f32,
    /// How likely each point in reach is joined, 0..1.
    pub density: f32,
    /// The joining lines' strength next to the pen (they fade with
    /// distance), 0..1.
    pub opacity: f32,
    /// The joining lines' thickness, canvas pixels.
    pub thickness: f32,
    /// How much of each joining line is left off at both ends, as a share
    /// of its length: 0 joins the points, a half draws
    /// nothing.
    pub offset: f32,
}

impl Default for Sketch {
    fn default() -> Self {
        Self {
            reach: 50.0,
            density: 0.15,
            opacity: 0.35,
            thickness: 1.0,
            offset: 0.0,
        }
    }
}

/// Earlier points kept for joining (the most recent ones).
pub const HISTORY: usize = 600;
/// Most lines from one point.
pub const MAX_LINES: usize = 6;

impl Sketch {
    /// How strongly a line of `length` to an earlier point is drawn (0 past
    /// the reach).
    pub fn strength(&self, length: f32) -> f32 {
        if length >= self.reach || self.reach <= 0.0 {
            return 0.0;
        }
        self.opacity.clamp(0.0, 1.0) * (1.0 - length / self.reach)
    }

    /// How far the pen moves before its point joins the history: closer
    /// points only draw over the line itself.
    pub fn point_spacing(&self) -> f32 {
        (self.reach / 12.0).max(2.0)
    }

    /// The line joining `p` to an earlier point `q`, left off at both
    /// ends by the offset (crossing over past a half).
    pub fn ends(&self, p: Vec2, q: Vec2) -> (Vec2, Vec2) {
        (p + (q - p) * self.offset, q - (q - p) * self.offset)
    }

    /// Distance between the dabs drawing a line.
    pub fn step(&self) -> f32 {
        (self.thickness * 0.5).max(0.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_leaves_both_ends_off() {
        let s = Sketch {
            offset: 0.25,
            ..Default::default()
        };
        let (a, b) = s.ends(Vec2::ZERO, Vec2::new(100.0, 0.0));
        assert_eq!((a.x, b.x), (25.0, 75.0));
        let whole = Sketch::default().ends(Vec2::ZERO, Vec2::new(100.0, 0.0));
        assert_eq!((whole.0.x, whole.1.x), (0.0, 100.0));
    }

    #[test]
    fn lines_fade_with_length_and_stop_at_the_reach() {
        let s = Sketch::default();
        assert!((s.strength(0.0) - s.opacity).abs() < 1e-6);
        assert!(s.strength(25.0) < s.strength(10.0));
        assert_eq!(s.strength(50.0), 0.0);
        assert_eq!(s.strength(80.0), 0.0);
    }
}
