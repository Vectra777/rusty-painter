//! Bristle brush: the brush is a row
//! of hairs laid across the stroke. Each hair paints its own thin line,
//! so a stroke shows streaks; pressing harder fans the hairs out, and each
//! hair runs out of paint at its own pace, so the stroke breaks up towards
//! its end like a dry brush.
//!
//! Every dab of the stroke becomes one small round dab per hair, at the
//! hair's place across the stroke (the brush turns with the stroke's
//! direction). Hairs are placed the same way every stroke, so strokes are
//! repeatable.
//!
//! After Krita's bristle (hairy) brush, as its manual describes it: the
//! hairs can come from the tip's own pixels, lean along the stroke
//! (shear), be thinned out (density), wander (random offset), lift off
//! under light pressure, lose their colour as they run dry (at the pace a
//! curve sets), and soak up the layer's colour where the stroke starts.

use eframe::egui::Vec2;

/// A bristle brush's hairs.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// Hairs where the brush's tip has paint (an image tip's pixels, as
    /// many as `count` at most), not a row across.
    pub from_tip: bool,
    /// Hairs lean along the stroke the further across it they are (-1..1).
    pub shear: f32,
    /// The share of the hairs that paint (0..1).
    pub density: f32,
    /// Each hair wanders across by up to this share of the spread, from
    /// dab to dab.
    pub random_offset: f32,
    /// Light pressure lifts hairs off (0 never): at full strength, a hair
    /// needs as much pressure as it is long.
    pub pressure_cut: f32,
    /// Hairs running dry lose their colour's saturation as well as paint.
    pub deplete_saturation: bool,
    /// How dry a hair is (y) as it goes through its paint (x), as Krita's
    /// ink depletion curve; `None` dries at a steady pace.
    pub depletion: Option<crate::brush_engine::hardness::SoftnessCurve>,
    /// Each hair takes the layer's colour from under it where the stroke
    /// starts and paints with it (Krita's soak ink).
    pub soak: bool,
}

impl Default for Bristles {
    fn default() -> Self {
        Self {
            count: 24,
            thickness: 2.0,
            spread: 1.0,
            ink: 0.0,
            variation: 0.5,
            from_tip: false,
            shear: 0.0,
            density: 1.0,
            random_offset: 0.0,
            pressure_cut: 0.0,
            deplete_saturation: false,
            depletion: None,
            soak: false,
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
    /// Its own number (for its randomness), and how long it is (0..1: the
    /// longest touch the canvas under the lightest pressure).
    pub id: u32,
    pub length: f32,
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
                    id: i,
                    length: rand01(i, 6),
                }
            })
            .collect()
    }

    /// Hairs where a tip mask (`width`×`height`, 0..=255) has paint: the
    /// pixels on a grid fine enough for `count` hairs, each as strong as
    /// its pixel. The tip's x is along the stroke, y across.
    pub fn hairs_from_mask(&self, width: usize, height: usize, mask: &[u8]) -> Vec<Hair> {
        let painted = mask.iter().filter(|&&m| m > 32).count();
        if painted == 0 || width == 0 || height == 0 {
            return self.hairs();
        }
        let want = self.count.clamp(1, MAX_HAIRS) as usize;
        // Every `step`th pixel each way gives about `want` hairs.
        let step = ((painted as f32 / want as f32).sqrt().round() as usize).max(1);
        let v = self.variation.clamp(0.0, 1.0);
        let half = Vec2::new(width as f32, height as f32) * 0.5;
        let mut out = Vec::new();
        for y in (step / 2..height).step_by(step) {
            for x in (step / 2..width).step_by(step) {
                let m = mask[y * width + x];
                if m <= 32 || out.len() >= MAX_HAIRS as usize {
                    continue;
                }
                let i = out.len() as u32;
                let at = (Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - half) / half.max_elem();
                out.push(Hair {
                    offset: at,
                    thickness: 1.0 - v * 0.5 * rand01(i, 3),
                    strength: m as f32 / 255.0 * (1.0 - v * 0.6 * rand01(i, 4)),
                    thirst: 0.6 + 0.8 * rand01(i, 5),
                    id: i,
                    length: rand01(i, 6),
                });
            }
        }
        out
    }

    /// Whether `hair` paints at all (the density), and under `pressure`.
    pub fn touches(&self, hair: &Hair, pressure: f32) -> bool {
        if rand01(hair.id, 7) >= self.density.clamp(0.0, 1.0) {
            return false;
        }
        let cut = self.pressure_cut.clamp(0.0, 1.0);
        cut <= 0.0 || pressure >= hair.length * cut
    }

    /// Where `hair` sits in the brush's frame (shares of the spread) on the
    /// dab `along` pixels into the stroke: leaning by the shear, wandering
    /// by the random offset.
    pub fn place(&self, hair: &Hair, along: f32) -> Vec2 {
        let wander = if self.random_offset > 0.0 {
            // A new place every few pixels, the same for the same place.
            let at = (along / 4.0).floor() as i32 as u32;
            (rand01(hair.id ^ at.wrapping_mul(0x9E37), 8) * 2.0 - 1.0) * self.random_offset
        } else {
            0.0
        };
        let y = hair.offset.y + wander;
        Vec2::new(hair.offset.x + self.shear.clamp(-1.0, 1.0) * y, y)
    }

    /// A hair's paint left after `along` pixels of stroke (1 = full).
    pub fn ink_left(&self, hair: &Hair, along: f32) -> f32 {
        if self.ink <= 0.0 {
            return 1.0;
        }
        let gone = (along * hair.thirst / self.ink).clamp(0.0, 1.0);
        match &self.depletion {
            Some(curve) => (1.0 - curve.eval(gone)).clamp(0.0, 1.0),
            None => 1.0 - gone,
        }
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
    fn hairs_can_come_from_the_tip_lean_thin_out_and_lift_off() {
        // A 20×10 tip painted on its left half only.
        let mask: Vec<u8> = (0..200)
            .map(|i| if i % 20 < 10 { 255 } else { 0 })
            .collect();
        let b = Bristles {
            count: 24,
            ..Default::default()
        };
        let hairs = b.hairs_from_mask(20, 10, &mask);
        assert!((10..=50).contains(&hairs.len()), "{}", hairs.len());
        assert!(
            hairs.iter().all(|h| h.offset.x < 0.0),
            "only where the tip paints"
        );
        // Shear: hairs further across lean further along.
        let sheared = Bristles {
            shear: 0.5,
            ..b.clone()
        };
        let h = Hair {
            offset: Vec2::new(0.0, 1.0),
            ..hairs[0]
        };
        assert_eq!(sheared.place(&h, 0.0), Vec2::new(0.5, 1.0));
        // Density keeps about that share.
        let half = Bristles {
            density: 0.5,
            count: 96,
            ..Default::default()
        };
        let kept = half.hairs().iter().filter(|h| half.touches(h, 1.0)).count();
        assert!((30..=66).contains(&kept), "{kept}");
        // Light pressure lifts the long hairs off; full pressure none.
        let lifted = Bristles {
            pressure_cut: 1.0,
            count: 96,
            ..Default::default()
        };
        let all = lifted.hairs();
        let touching = |p| all.iter().filter(|h| lifted.touches(h, p)).count();
        assert!(touching(0.2) < touching(0.6) && touching(1.0) == 96);
        // Random offset: the same place for the same dab, another further on.
        let wandering = Bristles {
            random_offset: 0.3,
            ..Default::default()
        };
        let h0 = wandering.hairs()[3];
        assert_eq!(wandering.place(&h0, 10.0), wandering.place(&h0, 10.5));
        let ys: Vec<f32> = (0..20)
            .map(|k| wandering.place(&h0, k as f32 * 8.0).y)
            .collect();
        assert!(ys.iter().any(|&y| (y - h0.offset.y).abs() > 0.05));
        assert!(ys.iter().all(|&y| (y - h0.offset.y).abs() <= 0.3 + 1e-6));
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
        // A curve: Krita's straight one dries as the steady pace does, one
        // that stays wet longer leaves more paint halfway.
        use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
        let curve = |mid: f32| Bristles {
            depletion: Some(SoftnessCurve {
                points: vec![
                    CurvePoint::new(0.0, 0.0),
                    CurvePoint::new(0.5, mid),
                    CurvePoint::new(1.0, 1.0),
                ],
            }),
            ..b.clone()
        };
        let h = Hair {
            thirst: 1.0,
            ..hairs[0]
        };
        assert!((curve(0.5).ink_left(&h, 50.0) - b.ink_left(&h, 50.0)).abs() < 1e-3);
        assert!(curve(0.1).ink_left(&h, 50.0) > 0.8);
        assert_eq!(curve(0.1).ink_left(&h, 100.0), 0.0);
    }
}
