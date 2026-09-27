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
            GradientShape::Radial => {
                let py = y as f32 + 0.5;
                for (i, o) in out.iter_mut().enumerate() {
                    *o = self.position(Vec2::new((x0 + i as i32) as f32 + 0.5, py));
                }
            }
            GradientShape::Angle => {
                // The end's direction is the same for the whole row.
                let base = d.y.atan2(d.x);
                let vy = y as f32 + 0.5 - self.start.y;
                let mut vx = x0 as f32 + 0.5 - self.start.x;
                for o in out.iter_mut() {
                    let a = fast_atan2(vy, vx) - base;
                    *o = self.finish((a * (1.0 / std::f32::consts::TAU)).rem_euclid(1.0));
                    vx += 1.0;
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
                let a = fast_atan2(v.y, v.x) - d.y.atan2(d.x);
                (a / std::f32::consts::TAU).rem_euclid(1.0)
            }
        };
        self.finish(t)
    }
}

/// `atan2` to within about 1e-4 rad (a thousandth of one step of the
/// colour ramp around a full turn), several times faster.
#[inline]
fn fast_atan2(y: f32, x: f32) -> f32 {
    use std::f32::consts::{FRAC_PI_2, PI};
    let (ax, ay) = (x.abs(), y.abs());
    if ax == 0.0 && ay == 0.0 {
        return 0.0;
    }
    // atan on [0, 1], then unfolded by octant.
    let (t, swap) = if ay > ax {
        (ax / ay, true)
    } else {
        (ay / ax, false)
    };
    let t2 = t * t;
    let mut a = t
        * (0.999_866
            + t2 * (-0.330_299_5 + t2 * (0.180_141 + t2 * (-0.085_133 + t2 * 0.020_835_1))));
    if swap {
        a = FRAC_PI_2 - a;
    }
    if x < 0.0 {
        a = PI - a;
    }
    if y < 0.0 { -a } else { a }
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

/// A colour along a gradient: `pos` from 0 (start) to 1 (end), unmultiplied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub pos: f32,
    pub color: Color32,
}

impl Ramp {
    /// From `from` to `to` (unmultiplied colours), mixed in `space`, with
    /// alpha scaled by `opacity`.
    pub fn new(from: Color32, to: Color32, space: BlendSpace, opacity: f32) -> Self {
        let stops = [
            Stop {
                pos: 0.0,
                color: from,
            },
            Stop {
                pos: 1.0,
                color: to,
            },
        ];
        Self::from_stops(&stops, space, opacity)
    }

    /// Through `stops` (sorted by position, at least one), each pair mixed
    /// in `space`; before the first and after the last, their colours.
    pub fn from_stops(stops: &[Stop], space: BlendSpace, opacity: f32) -> Self {
        assert!(!stops.is_empty(), "a gradient needs a colour");
        // Premultiplied, in the space colours mix in.
        let to_space = |c: Color32| {
            let [r, g, b, a] = c.to_srgba_unmultiplied();
            let a = a as f32 / 255.0;
            match space {
                BlendSpace::Linear => [
                    srgb_to_linear(r as f32) * a,
                    srgb_to_linear(g as f32) * a,
                    srgb_to_linear(b as f32) * a,
                    a,
                ],
                BlendSpace::Gamma => [
                    r as f32 / 255.0 * a,
                    g as f32 / 255.0 * a,
                    b as f32 / 255.0 * a,
                    a,
                ],
            }
        };
        let points: Vec<(f32, [f32; 4])> = stops
            .iter()
            .map(|s| (s.pos.clamp(0.0, 1.0), to_space(s.color)))
            .collect();
        let opacity = opacity.clamp(0.0, 1.0);
        let mut segment = 0;
        let unmultiplied: Vec<[f32; 4]> = (0..RAMP_SIZE)
            .map(|i| {
                let t = i as f32 / (RAMP_SIZE - 1) as f32;
                while segment + 1 < points.len() - 1 && t > points[segment + 1].0 {
                    segment += 1;
                }
                let (p0, pa) = points[segment];
                let (p1, pb) = points[(segment + 1).min(points.len() - 1)];
                let u = if p1 > p0 {
                    ((t - p0) / (p1 - p0)).clamp(0.0, 1.0)
                } else if t < p0 {
                    0.0
                } else {
                    1.0
                };
                let m: [f32; 4] = std::array::from_fn(|k| pa[k] + (pb[k] - pa[k]) * u);
                let alpha = m[3];
                // A fully transparent mix keeps the colour it fades from.
                let (a_src, inv) = if alpha > 1e-6 {
                    (m, 1.0 / alpha)
                } else if pa[3] > 1e-6 {
                    (pa, 1.0 / pa[3])
                } else {
                    (pb, 1.0 / pb[3].max(1e-6))
                };
                let rgb: [f32; 3] = std::array::from_fn(|k| match space {
                    BlendSpace::Linear => linear_to_srgb(a_src[k] * inv),
                    BlendSpace::Gamma => (a_src[k] * inv * 255.0).clamp(0.0, 255.0),
                });
                [rgb[0], rgb[1], rgb[2], alpha * opacity * 255.0]
            })
            .collect();
        let pixels = unmultiplied.iter().map(|&c| pixel(c)).collect();
        // How far each channel travels along the whole gradient (over every
        // stop), in 8-bit steps.
        let span = (0..4)
            .map(|k| {
                unmultiplied
                    .windows(2)
                    .map(|w| (w[1][k] - w[0][k]).abs())
                    .sum::<f32>()
            })
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
    fn ramp_passes_through_every_stop() {
        let stop = |pos, color| Stop { pos, color };
        let red = Color32::from_rgb(255, 0, 0);
        let green = Color32::from_rgb(0, 255, 0);
        let blue = Color32::from_rgb(0, 0, 255);
        let ramp = Ramp::from_stops(
            &[stop(0.2, red), stop(0.5, green), stop(1.0, blue)],
            BlendSpace::Gamma,
            1.0,
        );
        assert_eq!(ramp.pixel(0.0, None), red, "before the first stop");
        assert_eq!(ramp.pixel(0.2, None), red);
        assert_eq!(ramp.pixel(0.5, None), green);
        assert_eq!(ramp.pixel(1.0, None), blue);
        let between = ramp.pixel(0.75, None);
        assert!(
            (120..=135).contains(&between.g()) && (120..=135).contains(&between.b()),
            "halfway from green to blue: {between:?}"
        );
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

    #[test]
    fn fast_atan2_is_close() {
        for i in 0..1000 {
            let a = i as f32 / 1000.0 * std::f32::consts::TAU;
            let (y, x) = (a.sin() * 3.7, a.cos() * 3.7);
            assert!((fast_atan2(y, x) - y.atan2(x)).abs() < 2e-4, "at {a}");
        }
    }
}
