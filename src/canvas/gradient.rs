//! Gradients: where a pixel falls between the two ends (by shape and
//! repetition), and the colours along the way.
//!
//! A gradient's colours are [`GradientStops`]: colour stops and, apart from
//! them, opacity stops, each pair with a midpoint, as Photoshop has them.
//! The gradient tool, fill layers and gradient maps all use them.
//!
//! Colours are precomputed into a fine [`Ramp`] (in the document's blend
//! space, like the brush mixes), so painting a pixel is a lookup. Dithering
//! nudges each pixel's position by up to one 8-bit step (by a per-pixel
//! hash), which breaks up the bands a smooth gradient otherwise shows.

use crate::canvas::blend::Unmultiply;
use crate::canvas::blend_modes::BlendSpace;
use eframe::egui::{Color32, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GradientShape {
    /// Along the line from start to end.
    Linear,
    /// Out from the start, reaching the end colour at the end's distance.
    Radial,
    /// Linear, mirrored on both sides of the start.
    Reflected,
    /// Around the start, beginning in the end's direction (conic).
    Angle,
    /// Out from the start in a square turned towards the end, reaching the
    /// end colour at the end's corner.
    Diamond,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GradientRepeat {
    /// Past the ends, the end colours continue.
    #[default]
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
            GradientShape::Radial | GradientShape::Diamond => {
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
            GradientShape::Diamond => (v.dot(d).abs() + (v.x * d.y - v.y * d.x).abs()) / len2,
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

fn half() -> f32 {
    0.5
}

/// A colour along a gradient.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorStop {
    /// 0 (start) to 1 (end).
    pub pos: f32,
    /// sRGB.
    pub color: [u8; 3],
    /// Where between this stop and the next their colours are mixed half
    /// and half, as a share of the way (0.5: halfway).
    #[serde(default = "half")]
    pub mid: f32,
}

/// An opacity along a gradient.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpacityStop {
    pub pos: f32,
    /// 0 (clear) to 1.
    pub opacity: f32,
    /// As [`ColorStop::mid`].
    #[serde(default = "half")]
    pub mid: f32,
}

/// A gradient's colours: colour stops, and opacity stops (none: opaque),
/// each sorted by position.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "StopsRepr")]
pub struct GradientStops {
    pub colors: Vec<ColorStop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opacity: Vec<OpacityStop>,
}

/// What [`GradientStops`] were saved as: now, or before opacity stops and
/// midpoints (a gradient map's or fill layer's eight fixed slots, `len` of
/// them used).
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum StopsRepr {
    Now {
        colors: Vec<ColorStop>,
        #[serde(default)]
        opacity: Vec<OpacityStop>,
    },
    Before {
        len: u8,
        stops: Vec<(f32, [u8; 3])>,
    },
}

impl From<StopsRepr> for GradientStops {
    fn from(repr: StopsRepr) -> Self {
        let mut stops = match repr {
            StopsRepr::Now { colors, opacity } => Self { colors, opacity },
            StopsRepr::Before { len, stops } => {
                Self::from_colors(&stops[..(len as usize).min(stops.len())])
            }
        };
        if stops.colors.is_empty() {
            stops = Self::default();
        }
        stops.sort();
        stops
    }
}

impl Default for GradientStops {
    /// Black to white.
    fn default() -> Self {
        Self::from_colors(&[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])])
    }
}

/// Where `u` (0..=1 of the way between two stops) is after the midpoint
/// `mid` moves the halfway mix: a power curve, as GIMP and Photoshop bend
/// it, straight for 0.5.
fn bend(u: f32, mid: f32) -> f32 {
    let mid = mid.clamp(0.05, 0.95);
    if (mid - 0.5).abs() < 1e-4 {
        u
    } else {
        u.powf(0.5f32.ln() / mid.ln())
    }
}

/// The stop pair `t` falls between, by position, and how far between them
/// (bent by the first one's midpoint): before the first and after the last,
/// that stop alone.
fn between<T>(
    stops: &[T],
    t: f32,
    pos: impl Fn(&T) -> f32,
    mid: impl Fn(&T) -> f32,
) -> (usize, usize, f32) {
    let last = stops.len() - 1;
    if t <= pos(&stops[0]) {
        return (0, 0, 0.0);
    }
    if t >= pos(&stops[last]) {
        return (last, last, 0.0);
    }
    let i = stops
        .iter()
        .rposition(|s| pos(s) <= t)
        .unwrap_or(0)
        .min(last - 1);
    let (p0, p1) = (pos(&stops[i]), pos(&stops[i + 1]));
    let u = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
    (i, i + 1, bend(u.clamp(0.0, 1.0), mid(&stops[i])))
}

impl GradientStops {
    /// Opaque, through sRGB colours at positions (any order).
    pub fn from_colors(stops: &[(f32, [u8; 3])]) -> Self {
        let mut g = Self {
            colors: stops
                .iter()
                .map(|&(pos, color)| ColorStop {
                    pos: pos.clamp(0.0, 1.0),
                    color,
                    mid: 0.5,
                })
                .collect(),
            opacity: Vec::new(),
        };
        g.sort();
        g
    }

    /// Two colours, `from` and `to` (unmultiplied, their alpha too). A
    /// fully clear end keeps no colour of its own: it fades the other's.
    pub fn two(from: Color32, to: Color32) -> Self {
        let [mut r0, mut g0, mut b0, a0] = from.unmultiplied();
        let [mut r1, mut g1, mut b1, a1] = to.unmultiplied();
        if a0 == 0 {
            [r0, g0, b0] = [r1, g1, b1];
        } else if a1 == 0 {
            [r1, g1, b1] = [r0, g0, b0];
        }
        let mut g = Self::from_colors(&[(0.0, [r0, g0, b0]), (1.0, [r1, g1, b1])]);
        if a0 < 255 || a1 < 255 {
            g.opacity = vec![
                OpacityStop {
                    pos: 0.0,
                    opacity: a0 as f32 / 255.0,
                    mid: 0.5,
                },
                OpacityStop {
                    pos: 1.0,
                    opacity: a1 as f32 / 255.0,
                    mid: 0.5,
                },
            ];
        }
        g
    }

    /// Put both kinds of stop in order.
    pub fn sort(&mut self) {
        self.colors.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        self.opacity.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    }

    /// End to end: stops mirrored, each midpoint now on the other side.
    pub fn reversed(&self) -> Self {
        fn flip<T: Copy>(
            stops: &[T],
            pos: impl Fn(&mut T) -> &mut f32,
            mid: impl Fn(&mut T) -> &mut f32,
        ) -> Vec<T> {
            let mut out: Vec<T> = stops.iter().rev().copied().collect();
            // A pair's midpoint is kept by its first stop; reversed, the
            // pair's other stop comes first.
            let mids: Vec<f32> = out.iter_mut().map(|s| *mid(s)).collect();
            for (i, s) in out.iter_mut().enumerate() {
                *pos(s) = 1.0 - *pos(s);
                *mid(s) = mids.get(i + 1).map_or(0.5, |m| 1.0 - m);
            }
            out
        }
        Self {
            colors: flip(&self.colors, |s| &mut s.pos, |s| &mut s.mid),
            opacity: flip(&self.opacity, |s| &mut s.pos, |s| &mut s.mid),
        }
    }

    /// The colour at `t` (0..=1): unmultiplied sRGB (0..=255) and alpha
    /// (0..=1), mixed in `space`.
    pub fn eval(&self, t: f32, space: BlendSpace) -> [f32; 4] {
        let rgb = if self.colors.is_empty() {
            [t * 255.0; 3]
        } else {
            let (a, b, u) = between(&self.colors, t, |s| s.pos, |s| s.mid);
            let (ca, cb) = (self.colors[a].color, self.colors[b].color);
            std::array::from_fn(|k| match space {
                BlendSpace::Linear => {
                    let (x, y) = (srgb_to_linear(ca[k] as f32), srgb_to_linear(cb[k] as f32));
                    linear_to_srgb(x + (y - x) * u)
                }
                BlendSpace::Gamma => ca[k] as f32 + (cb[k] as f32 - ca[k] as f32) * u,
            })
        };
        let alpha = if self.opacity.is_empty() {
            1.0
        } else {
            let (a, b, u) = between(&self.opacity, t, |s| s.pos, |s| s.mid);
            let (x, y) = (self.opacity[a].opacity, self.opacity[b].opacity);
            (x + (y - x) * u).clamp(0.0, 1.0)
        };
        [rgb[0], rgb[1], rgb[2], alpha]
    }

    /// Whether any part is see-through.
    pub fn has_transparency(&self) -> bool {
        self.opacity.iter().any(|s| s.opacity < 1.0)
    }
}

impl Ramp {
    /// Along `stops`, mixed in `space`, with alpha scaled by `opacity`.
    pub fn new(stops: &GradientStops, space: BlendSpace, opacity: f32) -> Self {
        let opacity = opacity.clamp(0.0, 1.0);
        let unmultiplied: Vec<[f32; 4]> = (0..RAMP_SIZE)
            .map(|i| {
                let [r, g, b, a] = stops.eval(i as f32 / (RAMP_SIZE - 1) as f32, space);
                [r, g, b, a * opacity * 255.0]
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

    /// The colour at `t` at full precision, premultiplied in linear light,
    /// with `coverage` (0..=1) of it: for deeper documents, which need no
    /// dither. Interpolated between the precomputed steps.
    #[inline]
    pub fn linear_covered(&self, t: f32, coverage: f32) -> [f32; 4] {
        let at = t.clamp(0.0, 1.0) * (RAMP_SIZE - 1) as f32;
        let i = (at as usize).min(RAMP_SIZE - 2);
        let u = at - i as f32;
        let (a, b) = (self.unmultiplied[i], self.unmultiplied[i + 1]);
        let c: [f32; 4] = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * u);
        let alpha = c[3] / 255.0 * coverage;
        let lin = |v: f32| srgb_to_linear(v) * alpha;
        [lin(c[0]), lin(c[1]), lin(c[2]), alpha]
    }

    /// The colour at `t` at full precision, premultiplied in `space`
    /// (linear light or the stored sRGB values): a deeper document's fill
    /// layer, as the compositor reads it.
    #[inline]
    pub fn premultiplied_in(&self, t: f32, space: BlendSpace) -> [f32; 4] {
        match space {
            BlendSpace::Linear => self.linear_covered(t, 1.0),
            BlendSpace::Gamma => {
                let at = t.clamp(0.0, 1.0) * (RAMP_SIZE - 1) as f32;
                let i = (at as usize).min(RAMP_SIZE - 2);
                let u = at - i as f32;
                let (a, b) = (self.unmultiplied[i], self.unmultiplied[i + 1]);
                let c: [f32; 4] = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * u);
                let alpha = c[3] / 255.0;
                [
                    c[0] / 255.0 * alpha,
                    c[1] / 255.0 * alpha,
                    c[2] / 255.0 * alpha,
                    alpha,
                ]
            }
        }
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
        let diamond = Gradient {
            shape: GradientShape::Diamond,
            ..g
        };
        // A square turned towards the end: its sides at a quarter of the
        // way reach (25, 0) and (0, 25), and (12.5, 12.5) between them.
        for p in [(25.0, 0.0), (0.0, 25.0), (12.5, 12.5), (-12.5, -12.5)] {
            let t = diamond.position(Vec2::new(p.0, p.1));
            assert!((t - 0.25).abs() < 1e-6, "{p:?}: {t}");
        }
    }

    #[test]
    fn a_midpoint_moves_the_halfway_mix() {
        let mut stops = GradientStops::default();
        let half = |s: &GradientStops, t| s.eval(t, BlendSpace::Gamma)[0];
        assert!((half(&stops, 0.5) - 127.5).abs() < 0.5);
        stops.colors[0].mid = 0.25;
        assert!(
            (half(&stops, 0.25) - 127.5).abs() < 0.5,
            "half grey at the midpoint"
        );
        assert!(half(&stops, 0.5) > 160.0, "lighter past it");
        // Reversed, the midpoint is as far from the other end.
        let back = stops.reversed();
        assert!((half(&back, 0.75) - 127.5).abs() < 0.5);
    }

    #[test]
    fn opacity_stops_fade_apart_from_the_colours() {
        let mut stops = GradientStops::from_colors(&[(0.0, [255, 0, 0]), (1.0, [0, 0, 255])]);
        stops.opacity = vec![
            OpacityStop {
                pos: 0.0,
                opacity: 1.0,
                mid: 0.5,
            },
            OpacityStop {
                pos: 0.5,
                opacity: 0.0,
                mid: 0.5,
            },
            OpacityStop {
                pos: 1.0,
                opacity: 1.0,
                mid: 0.5,
            },
        ];
        let [r, _, b, a] = stops.eval(0.5, BlendSpace::Gamma);
        assert_eq!(a, 0.0);
        assert!(
            (r - 127.5).abs() < 0.5 && (b - 127.5).abs() < 0.5,
            "the colour mixes on regardless"
        );
        assert!((stops.eval(0.25, BlendSpace::Gamma)[3] - 0.5).abs() < 1e-6);
        assert!(stops.has_transparency());
    }

    #[test]
    fn stops_saved_before_opacity_stops_still_read() {
        // A gradient map or fill layer as saved before: eight slots.
        let old = r#"{"len":2,"stops":[[0.0,[10,20,30]],[1.0,[200,210,220]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]],[0.0,[0,0,0]]]}"#;
        let stops: GradientStops = serde_json::from_str(old).unwrap();
        assert_eq!(
            stops,
            GradientStops::from_colors(&[(0.0, [10, 20, 30]), (1.0, [200, 210, 220])])
        );
        let again: GradientStops =
            serde_json::from_str(&serde_json::to_string(&stops).unwrap()).unwrap();
        assert_eq!(again, stops);
    }

    #[test]
    fn ramp_runs_between_its_colours() {
        let black = Color32::BLACK;
        let white = Color32::WHITE;
        for space in [BlendSpace::Linear, BlendSpace::Gamma] {
            let ramp = Ramp::new(&GradientStops::two(black, white), space, 1.0);
            assert_eq!(ramp.pixel(0.0, None), black);
            assert_eq!(ramp.pixel(1.0, None), white);
        }
        // Mixed in linear light, the middle is lighter than half-grey bytes.
        let bw = GradientStops::two(black, white);
        let lin = Ramp::new(&bw, BlendSpace::Linear, 1.0).unmultiplied[RAMP_SIZE / 2][0];
        let gam = Ramp::new(&bw, BlendSpace::Gamma, 1.0).unmultiplied[RAMP_SIZE / 2][0];
        assert!(lin > gam + 20.0, "linear {lin} vs gamma {gam}");
    }

    #[test]
    fn ramp_passes_through_every_stop() {
        let red = Color32::from_rgb(255, 0, 0);
        let green = Color32::from_rgb(0, 255, 0);
        let blue = Color32::from_rgb(0, 0, 255);
        let stops = GradientStops::from_colors(&[
            (0.2, [255, 0, 0]),
            (0.5, [0, 255, 0]),
            (1.0, [0, 0, 255]),
        ]);
        let ramp = Ramp::new(&stops, BlendSpace::Gamma, 1.0);
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
        let ramp = Ramp::new(&GradientStops::two(red, clear), BlendSpace::Linear, 1.0);
        let mid = ramp.unmultiplied[RAMP_SIZE / 2];
        assert!(
            (mid[0] - 200.0).abs() < 1.0 && (mid[3] - 127.5).abs() < 1.0,
            "{mid:?}"
        );
    }

    #[test]
    fn dithering_stays_within_one_step() {
        let ramp = Ramp::new(
            &GradientStops::two(Color32::from_gray(100), Color32::from_gray(110)),
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
            GradientShape::Diamond,
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
