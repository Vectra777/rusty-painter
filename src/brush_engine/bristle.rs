//! Bristle brush (Krita's bristle engine, simplified): the brush is a row
//! of hairs laid across the stroke. Each hair paints its own thin line,
//! so a stroke shows streaks; pressing harder fans the hairs out, and each
//! hair runs out of paint at its own pace, so the stroke breaks up towards
//! its end like a dry brush.
//!
//! Every dab of the stroke becomes one small round dab per hair, at the
//! hair's place across the stroke (the brush turns with the stroke's
//! direction). Hairs are placed the same way every stroke, so strokes are
//! repeatable.

use eframe::egui::Vec2;

/// A bristle brush's hairs.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Bristles {
    /// How many hairs.
    pub count: u32,
    /// Hair thickness, in canvas pixels.
    pub thickness: f32,
    /// How far the hairs spread, as a share of the brush radius at full
    /// pressure.
    pub spread: f32,
    /// Stroke length (canvas pixels) a hair's paint lasts, on average
    /// (0: it never runs out).
    pub ink: f32,
    /// How much the hairs differ in strength and thickness, 0..1.
    pub variation: f32,
}

impl Default for Bristles {
    fn default() -> Self {
        Self {
            count: 24,
            thickness: 2.0,
            spread: 1.0,
            ink: 0.0,
            variation: 0.5,
        }
    }
}

/// One hair.
#[derive(Clone, Copy, Debug)]
pub struct Hair {
    /// Place in the brush's frame, as a share of its radius: across the
    /// stroke (`y`) and a little along it (`x`).
    pub offset: Vec2,
    /// Thickness factor.
    pub thickness: f32,
    /// Strength factor.
    pub strength: f32,
    /// How fast its paint runs out (1 = the brush's `ink` length).
    pub thirst: f32,
}

impl Bristles {
    /// The hairs, the same for every stroke.
    pub fn hairs(&self) -> Vec<Hair> {
        let n = self.count.clamp(1, MAX_HAIRS);
        let v = self.variation.clamp(0.0, 1.0);
        (0..n)
            .map(|i| {
                // Evenly across, nudged a little so they don't line up.
                let across = if n == 1 {
                    0.0
                } else {
                    (i as f32 + 0.5) / n as f32 * 2.0 - 1.0
                };
                let nudge = (rand01(i, 1) - 0.5) * 2.0 / n as f32;
                Hair {
                    offset: Vec2::new(
                        (rand01(i, 2) - 0.5) * 0.3,
                        (across + nudge).clamp(-1.0, 1.0),
                    ),
                    thickness: 1.0 - v * 0.5 * rand01(i, 3),
                    strength: 1.0 - v * 0.6 * rand01(i, 4),
                    thirst: 0.6 + 0.8 * rand01(i, 5),
                }
            })
            .collect()
    }

    /// A hair's paint left after `along` pixels of stroke (1 = full).
    pub fn ink_left(&self, hair: &Hair, along: f32) -> f32 {
        if self.ink <= 0.0 {
            return 1.0;
        }
        (1.0 - along * hair.thirst / self.ink).clamp(0.0, 1.0)
    }

    /// Distance between the stroke's dabs: under a hair's thickness, so
    /// each hair draws a continuous line.
    pub fn step(&self) -> f32 {
        (self.thickness * 0.4).max(0.5)
    }
}

/// Most hairs a brush can have.
pub const MAX_HAIRS: u32 = 96;

/// A repeatable pseudo-random value in 0..1.
fn rand01(i: u32, seed: u32) -> f32 {
    let mut h = i.wrapping_mul(0x9E37_79B9) ^ seed.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65535.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hairs_spread_across_the_whole_brush_and_repeat() {
        let b = Bristles::default();
        let hairs = b.hairs();
        assert_eq!(hairs.len(), 24);
        let ys: Vec<f32> = hairs.iter().map(|h| h.offset.y).collect();
        assert!(ys.iter().all(|y| (-1.0..=1.0).contains(y)));
        assert!(ys.iter().cloned().fold(f32::MAX, f32::min) < -0.8);
        assert!(ys.iter().cloned().fold(f32::MIN, f32::max) > 0.8);
        let again: Vec<f32> = b.hairs().iter().map(|h| h.offset.y).collect();
        assert_eq!(ys, again);
    }

    #[test]
    fn hairs_run_dry_at_their_own_pace() {
        let b = Bristles {
            ink: 100.0,
            ..Default::default()
        };
        let hairs = b.hairs();
        assert!(hairs.iter().all(|h| b.ink_left(h, 0.0) == 1.0));
        let left: Vec<f32> = hairs.iter().map(|h| b.ink_left(h, 90.0)).collect();
        assert!(left.contains(&0.0) && left.iter().any(|&l| l > 0.2));
        let never = Bristles::default();
        assert_eq!(never.ink_left(&hairs[0], 1e6), 1.0);
    }
}
