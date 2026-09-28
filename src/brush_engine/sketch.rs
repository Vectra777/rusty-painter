//! Sketch brush (Krita's sketch engine): besides its own line, each new
//! point of the stroke is joined by fine lines to earlier points of the
//! stroke nearby, so going back and forth over an area builds up a web of
//! shading, the way a quick pencil sketch does.

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
}

impl Default for Sketch {
    fn default() -> Self {
        Self {
            reach: 50.0,
            density: 0.15,
            opacity: 0.35,
            thickness: 1.0,
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

    /// Distance between the dabs drawing a line.
    pub fn step(&self) -> f32 {
        (self.thickness * 0.5).max(0.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_fade_with_length_and_stop_at_the_reach() {
        let s = Sketch::default();
        assert!((s.strength(0.0) - s.opacity).abs() < 1e-6);
        assert!(s.strength(25.0) < s.strength(10.0));
        assert_eq!(s.strength(50.0), 0.0);
        assert_eq!(s.strength(80.0), 0.0);
    }
}
