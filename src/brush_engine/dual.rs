//! Dual brush: a second tip, stamped along the same stroke, masks the
//! first (Photoshop's and Clip Studio's dual brush).
//! Paint shows only where both tips reach, so a round brush masked by a
//! spatter tip paints a broken, dry-media stroke.
//!
//! The second tip's dabs accumulate a coverage of their own per tile (the
//! stroke's mask); the two coverages combine when the stroke is resolved,
//! so the result doesn't depend on which tip's dab came first.

use crate::brush_engine::brush::Brush;
use crate::brush_engine::brush_options::{BlendMode, PaintingMode, PixelBrushShape};
use eframe::egui::Color32;

/// How the second tip's coverage combines with the brush's.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DualMode {
    /// Paint scaled by the mask: soft all over.
    #[default]
    Multiply,
    /// The weaker of the two.
    Darken,
    /// The mask's gaps eat into the paint; heavy paint fills them in
    /// (a linear burn).
    Subtract,
    /// Light paint reaches only the mask's peaks, full paint everything.
    Height,
    /// A colour burn: like subtract, but steeper, and solid paint stays
    /// solid whatever the mask.
    Burn,
}

impl DualMode {
    pub const ALL: [DualMode; 5] = [
        Self::Multiply,
        Self::Darken,
        Self::Subtract,
        Self::Height,
        Self::Burn,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Multiply => "Multiply",
            Self::Darken => "Darken",
            Self::Subtract => "Subtract",
            Self::Height => "Height",
            Self::Burn => "Burn",
        }
    }

    /// The paint's coverage `c` (0..1) masked by the second tip's `m`.
    /// Never decreases as either grows, so later dabs only add paint.
    #[inline]
    pub fn apply(self, c: f32, m: f32) -> f32 {
        match self {
            Self::Multiply => c * m,
            Self::Darken => c.min(m),
            Self::Subtract => (c - (1.0 - m)).max(0.0),
            Self::Height => {
                // Paint reaches down to 1 - c: where the mask is at
                // least that, it shows (with a short ramp).
                let h = ((m - (1.0 - c)) * 4.0).clamp(0.0, 1.0);
                c * h
            }
            // Colour burn, the mask burning the paint's coverage.
            Self::Burn => {
                if c >= 1.0 {
                    1.0
                } else if m <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - c) / m).min(1.0)
                }
            }
        }
    }
}

/// The second tip of a dual brush.
#[derive(Clone, Debug, PartialEq)]
pub struct DualTip {
    pub shape: PixelBrushShape,
    /// Size as a share of the brush's (unpressured) size.
    pub size: f32,
    /// Edge hardness of a round or square tip, 0..100.
    pub hardness: f32,
    /// Distance between its dabs, as a percentage of its own size.
    pub spacing: f32,
    /// Random offset of its dabs, as a percentage of its own size.
    pub scatter: f32,
    /// Dabs per step.
    pub count: u32,
    /// Turn each dab at random.
    pub random_angle: bool,
    pub mode: DualMode,
}

impl Default for DualTip {
    fn default() -> Self {
        Self {
            shape: PixelBrushShape::Circle,
            size: 0.4,
            hardness: 80.0,
            spacing: 50.0,
            scatter: 60.0,
            count: 1,
            random_angle: true,
            mode: DualMode::Multiply,
        }
    }
}

impl DualTip {
    /// The brush that stamps this tip for a brush `diameter` across: full
    /// strength, plain dabs (its coverage is a mask, not paint).
    pub fn brush(&self, main: &Brush, diameter: f32) -> Brush {
        let mut b = Brush::new(
            (diameter * self.size).max(1.0),
            self.hardness,
            Color32::WHITE,
            self.spacing,
        );
        b.anti_aliasing = main.anti_aliasing;
        b.antialias_width = main.antialias_width;
        let o = &mut b.brush_options;
        o.pixel_shape = self.shape.clone();
        o.blend_mode = BlendMode::Normal;
        o.painting_mode = PaintingMode::BuildUp;
        o.pressure_size = false;
        b.jitter = self.scatter;
        b
    }

    /// Distance between its dabs for a brush `diameter` across.
    pub fn step(&self, diameter: f32) -> f32 {
        (self.spacing / 100.0 * diameter * self.size).max(0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_keeps_full_paint_under_a_full_mask_and_none_under_none() {
        for mode in DualMode::ALL {
            assert_eq!(mode.apply(1.0, 1.0), 1.0, "{mode:?}");
            // Burn leaves solid paint solid.
            if mode != DualMode::Burn {
                assert_eq!(mode.apply(1.0, 0.0), 0.0, "{mode:?}");
            }
            assert_eq!(mode.apply(0.0, 1.0), 0.0, "{mode:?}");
        }
    }

    #[test]
    fn burn_is_krita_s_colour_burn_on_the_coverage() {
        let burn = |c, m| DualMode::Burn.apply(c, m);
        assert_eq!(burn(0.5, 0.5), 0.0);
        assert_eq!(burn(0.75, 0.5), 0.5);
        assert_eq!(burn(1.0, 0.0), 1.0, "solid paint stays");
        assert_eq!(burn(0.9, 0.0), 0.0);
    }

    #[test]
    fn modes_never_take_paint_away_as_either_coverage_grows() {
        for mode in DualMode::ALL {
            for i in 0..=20 {
                for j in 0..20 {
                    let (c, m) = (i as f32 / 20.0, j as f32 / 20.0);
                    let step = 1.0 / 20.0;
                    assert!(
                        mode.apply(c, m + step) >= mode.apply(c, m) - 1e-6,
                        "{mode:?}"
                    );
                    if i < 20 {
                        assert!(
                            mode.apply(c + step, m) >= mode.apply(c, m) - 1e-6,
                            "{mode:?}"
                        );
                    }
                }
            }
        }
    }
}
