//! Gradients: where a pixel falls between the two ends (by shape and
//! repetition), and the colours along the way.
//!
//! Colours are precomputed into a fine [`Ramp`] (in the document's blend
//! space, like the brush mixes), so painting a pixel is a lookup. Ordered
//! dithering nudges each pixel's position by up to one 8-bit step, which
//! breaks up the bands a smooth gradient otherwise shows.

use crate::canvas::blend_modes::BlendSpace;
use eframe::egui::{Color32, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientShape {
    /// Along the line from start to end.
    Linear,
    /// Out from the start, reaching the end colour at the end's distance.
    Radial,
    /// Linear, mirrored on both sides of the start.
    Reflected,
    /// Around the start, beginning in the end's direction (conic).
    Angle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientRepeat {
    /// Past the ends, the end colours continue.
    None,
    /// Start over past the end.
    Repeat,
    /// Go back and forth.
    Mirror,
}

/// A gradient's geometry, in canvas coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gradient {
    pub shape: GradientShape,
    pub repeat: GradientRepeat,
    pub start: Vec2,
    pub end: Vec2,
    pub reverse: bool,
}

impl Gradient {
    /// Positions of the pixels `x0..x0 + out.len()` on row `y` (at pixel
    /// centres). Linear shapes step along the row instead of recomputing.
    pub fn row_positions(&self, x0: i32, y: i32, out: &mut [f32]) {
        let d = self.end - self.start;
        let len2 = d.length_sq().max(1e-6);
        match self.shape {
            GradientShape::Linear | GradientShape::Reflected => {
                let v = Vec2::new(x0 as f32 + 0.5, y as f32 + 0.5) - self.start;
                let (mut t, step) = (v.dot(d) / len2, d.x / len2);
                let reflected = self.shape == GradientShape::Reflected;
                for o in out.iter_mut() {
                    *o = self.finish(if reflected { t.abs() } else { t });
                    t += step;
                }
            }
            GradientShape::Radial | GradientShape::Angle => {
                let py = y as f32 + 0.5;
                for (i, o) in out.iter_mut().enumerate() {
                    *o = self.position(Vec2::new((x0 + i as i32) as f32 + 0.5, py));
                }
            }
        }
    }

    /// Repetition and reversal of a raw position.
    fn finish(&self, t: f32) -> f32 {
        let t = match self.repeat {
            GradientRepeat::None => t.clamp(0.0, 1.0),
            GradientRepeat::Repeat => t.rem_euclid(1.0),
            GradientRepeat::Mirror => {
                let f = t.rem_euclid(2.0);
                if f > 1.0 { 2.0 - f } else { f }
            }
        };
        if self.reverse { 1.0 - t } else { t }
    }

    /// Where `p` falls, from 0 (start colour) to 1 (end colour).
    pub fn position(&self, p: Vec2) -> f32 {
        let d = self.end - self.start;
        let len2 = d.length_sq().max(1e-6);
        let v = p - self.start;
        let t = match self.shape {
            GradientShape::Linear => v.dot(d) / len2,
            GradientShape::Reflected => (v.dot(d) / len2).abs(),
            GradientShape::Radial => (v.length_sq() / len2).sqrt(),
            GradientShape::Angle => {
                let a = v.y.atan2(v.x) - d.y.atan2(d.x);
                (a / std::f32::consts::TAU).rem_euclid(1.0)
            }
        };
        self.finish(t)
    }
}

/// Number of precomputed colours along a gradient.
const RAMP_SIZE: usize = 4096;

/// The colours along a gradient, precomputed.
pub struct Ramp {
    /// Unmultiplied sRGB colour and alpha (0..=255) at each step.
    unmultiplied: Vec<[f32; 4]>,
    /// The same, as premultiplied pixels ready to paint.
    pixels: Vec<Color32>,
    /// How far along the gradient one 8-bit step of its fastest-changing
    /// channel is, for dithering.
    step: f32,
}

fn srgb_to_linear(v: f32) -> f32 {
    let c = v / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    255.0
        * if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
}

/// A pixel from unmultiplied sRGB colour and alpha (0..=255).
fn pixel(c: [f32; 4]) -> Color32 {
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]))
}

impl Ramp {
    /// From `from` to `to` (unmultiplied colours), mixed in `space`, with
    /// alpha scaled by `opacity`.
    pub fn new(from: Color32, to: Color32, space: BlendSpace, opacity: f32) -> Self {
        let unmult = |c: Color32| {
            let [r, g, b, a] = c.to_srgba_unmultiplied();
            [r as f32, g as f32, b as f32, a as f32 / 255.0]
        };
        let (a, b) = (unmult(from), unmult(to));
        // Premultiplied, in the space colours mix in.
        let to_space = |c: [f32; 4]| match space {
            BlendSpace::Linear => [
                srgb_to_linear(c[0]) * c[3],
                srgb_to_linear(c[1]) * c[3],
                srgb_to_linear(c[2]) * c[3],
                c[3],
            ],
            BlendSpace::Gamma => [
                c[0] / 255.0 * c[3],
                c[1] / 255.0 * c[3],
                c[2] / 255.0 * c[3],
                c[3],
            ],
        };
        let (pa, pb) = (to_space(a), to_space(b));
        let opacity = opacity.clamp(0.0, 1.0);
        let unmultiplied: Vec<[f32; 4]> = (0..RAMP_SIZE)
            .map(|i| {
                let t = i as f32 / (RAMP_SIZE - 1) as f32;
                let m: [f32; 4] = std::array::from_fn(|k| pa[k] + (pb[k] - pa[k]) * t);
                let alpha = m[3];
                // A fully transparent mix keeps the colour it fades from.
                let (a_src, inv) = if alpha > 1e-6 {
                    (m, 1.0 / alpha)
                } else {
                    (pa, 1.0 / pa[3].max(1e-6))
                };
                let rgb: [f32; 3] = std::array::from_fn(|k| match space {
                    BlendSpace::Linear => linear_to_srgb(a_src[k] * inv),
                    BlendSpace::Gamma => (a_src[k] * inv * 255.0).clamp(0.0, 255.0),
                });
                [rgb[0], rgb[1], rgb[2], alpha * opacity * 255.0]
            })
            .collect();
        let pixels = unmultiplied.iter().map(|&c| pixel(c)).collect();
        let span = (0..4)
            .map(|k| (unmultiplied[RAMP_SIZE - 1][k] - unmultiplied[0][k]).abs())
            .fold(0.0f32, f32::max);
        Self {
            unmultiplied,
            pixels,
            step: if span > 0.0 { 1.0 / span } else { 0.0 },
        }
    }

    #[inline]
    fn index(&self, t: f32, noise: Option<f32>) -> usize {
        let t = match noise {
            Some(n) => t + (n - 0.5) * self.step,
            None => t,
        };
        // Non-negative after the clamp, so adding a half and truncating
        // rounds (without a libm call per pixel).
        ((t.clamp(0.0, 1.0) * (RAMP_SIZE - 1) as f32 + 0.5) as usize).min(RAMP_SIZE - 1)
    }

    /// The pixel at position `t`, dithered by `noise` (0..1) if given.
    #[inline]
    pub fn pixel(&self, t: f32, noise: Option<f32>) -> Color32 {
        self.pixels[self.index(t, noise)]
    }

    /// The pixel at `t` with `coverage` (0..=255) of it, for soft edges.
    #[inline]
    pub fn pixel_covered(&self, t: f32, noise: Option<f32>, coverage: u8) -> Color32 {
        if coverage == 255 {
            return self.pixel(t, noise);
        }
        let mut c = self.unmultiplied[self.index(t, noise)];
        c[3] *= coverage as f32 / 255.0;
        pixel(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(start: Vec2, end: Vec2) -> Gradient {
        Gradient {
            shape: GradientShape::Linear,
            repeat: GradientRepeat::None,
            start,
            end,
            reverse: false,
        }
    }

    #[test]
    fn positions_follow_the_shape() {
        let g = linear(Vec2::ZERO, Vec2::new(100.0, 0.0));
        assert_eq!(g.position(Vec2::new(25.0, 40.0)), 0.25);
        assert_eq!(
            g.position(Vec2::new(-10.0, 0.0)),
            0.0,
            "clamped before the start"
        );
        let reflected = Gradient {
            shape: GradientShape::Reflected,
            ..g
        };
        assert_eq!(reflected.position(Vec2::new(-25.0, 0.0)), 0.25);
        let radial = Gradient {
            shape: GradientShape::Radial,
            ..g
        };
        assert!((radial.position(Vec2::new(0.0, 50.0)) - 0.5).abs() < 1e-6);
        let angle = Gradient {
            shape: GradientShape::Angle,
            ..g
        };
        assert!(
            (angle.position(Vec2::new(0.0, 10.0)) - 0.25).abs() < 1e-6,
            "a quarter turn"
        );
        let mirror = Gradient {
            repeat: GradientRepeat::Mirror,
            ..g
        };
        assert!((mirror.position(Vec2::new(150.0, 0.0)) - 0.5).abs() < 1e-6);
        let repeat = Gradient {
            repeat: GradientRepeat::Repeat,
            ..g
        };
        assert!((repeat.position(Vec2::new(125.0, 0.0)) - 0.25).abs() < 1e-6);
        let reversed = Gradient { reverse: true, ..g };
        assert_eq!(reversed.position(Vec2::new(25.0, 0.0)), 0.75);
    }

    #[test]
    fn ramp_runs_between_its_colours() {
        let black = Color32::BLACK;
        let white = Color32::WHITE;
        for space in [BlendSpace::Linear, BlendSpace::Gamma] {
            let ramp = Ramp::new(black, white, space, 1.0);
            assert_eq!(ramp.pixel(0.0, None), black);
            assert_eq!(ramp.pixel(1.0, None), white);
        }
        // Mixed in linear light, the middle is lighter than half-grey bytes.
        let lin = Ramp::new(black, white, BlendSpace::Linear, 1.0).unmultiplied[RAMP_SIZE / 2][0];
        let gam = Ramp::new(black, white, BlendSpace::Gamma, 1.0).unmultiplied[RAMP_SIZE / 2][0];
        assert!(lin > gam + 20.0, "linear {lin} vs gamma {gam}");
    }

    #[test]
    fn fading_to_transparent_keeps_the_colour() {
        let red = Color32::from_rgb(200, 20, 20);
        let clear = Color32::from_rgba_unmultiplied(200, 20, 20, 0);
        let ramp = Ramp::new(red, clear, BlendSpace::Linear, 1.0);
        let mid = ramp.unmultiplied[RAMP_SIZE / 2];
        assert!(
            (mid[0] - 200.0).abs() < 1.0 && (mid[3] - 127.5).abs() < 1.0,
            "{mid:?}"
        );
    }

    #[test]
    fn dithering_stays_within_one_step() {
        let ramp = Ramp::new(
            Color32::from_gray(100),
            Color32::from_gray(110),
            BlendSpace::Gamma,
            1.0,
        );
        for i in 0..100 {
            let t = i as f32 / 99.0;
            let plain = ramp.pixel(t, None).r() as i32;
            for n in [0.0, 0.3, 0.7, 0.999] {
                let d = ramp.pixel(t, Some(n)).r() as i32;
                assert!((d - plain).abs() <= 1, "t {t}: {plain} vs dithered {d}");
            }
        }
    }

    #[test]
    fn row_positions_match_single_positions() {
        for shape in [
            GradientShape::Linear,
            GradientShape::Reflected,
            GradientShape::Radial,
            GradientShape::Angle,
        ] {
            let g = Gradient {
                shape,
                repeat: GradientRepeat::Mirror,
                start: Vec2::new(30.0, 20.0),
                end: Vec2::new(70.0, 45.0),
                reverse: true,
            };
            let mut row = vec![0.0; 100];
            g.row_positions(-10, 33, &mut row);
            for (i, &t) in row.iter().enumerate() {
                let one = g.position(Vec2::new((-10 + i as i32) as f32 + 0.5, 33.5));
                assert!((t - one).abs() < 1e-4, "{shape:?} at {i}: {t} vs {one}");
            }
        }
    }
}
