//! Layer blend modes (Photoshop's full set) and the colour space layers
//! blend in.
//!
//! Formulas follow the W3C "Compositing and Blending Level 1" spec, which
//! matches Photoshop's definitions: a mode is a function `B(Cb, Cs)` of the
//! unpremultiplied backdrop and source colours, and the result is
//! `co = cs·(1 − αb) + cb·(1 − αs) + αs·αb·B(Cb, Cs)` on premultiplied values,
//! with `αo = αs + αb − αs·αb`. Where the source doesn't overlap the backdrop
//! (`αb = 0`) it shows unchanged, as in every painting app.

use eframe::egui::Rgba;

/// How a layer's colours combine with what's below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LayerBlend {
    #[default]
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    /// Parallel: twice the harmonic mean of the two colours, like
    /// two resistors in parallel (darkens, but less than Multiply).
    Parallel,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl LayerBlend {
    /// Every mode in Photoshop's menu order, in its groups (normal, darken,
    /// lighten, contrast, inversion/cancellation, component).
    pub const GROUPS: [&'static [LayerBlend]; 6] = [
        &[Self::Normal, Self::Dissolve],
        &[
            Self::Darken,
            Self::Multiply,
            Self::ColorBurn,
            Self::LinearBurn,
            Self::DarkerColor,
        ],
        &[
            Self::Lighten,
            Self::Screen,
            Self::ColorDodge,
            Self::LinearDodge,
            Self::LighterColor,
        ],
        &[
            Self::Overlay,
            Self::SoftLight,
            Self::HardLight,
            Self::VividLight,
            Self::LinearLight,
            Self::PinLight,
            Self::HardMix,
        ],
        &[
            Self::Difference,
            Self::Exclusion,
            Self::Subtract,
            Self::Divide,
            Self::Parallel,
        ],
        &[Self::Hue, Self::Saturation, Self::Color, Self::Luminosity],
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Dissolve => "Dissolve",
            Self::Darken => "Darken",
            Self::Multiply => "Multiply",
            Self::ColorBurn => "Color Burn",
            Self::LinearBurn => "Linear Burn",
            Self::DarkerColor => "Darker Color",
            Self::Lighten => "Lighten",
            Self::Screen => "Screen",
            Self::ColorDodge => "Color Dodge",
            Self::LinearDodge => "Linear Dodge (Add)",
            Self::LighterColor => "Lighter Color",
            Self::Overlay => "Overlay",
            Self::SoftLight => "Soft Light",
            Self::HardLight => "Hard Light",
            Self::VividLight => "Vivid Light",
            Self::LinearLight => "Linear Light",
            Self::PinLight => "Pin Light",
            Self::HardMix => "Hard Mix",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::Divide => "Divide",
            Self::Parallel => "Parallel",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Color => "Color",
            Self::Luminosity => "Luminosity",
        }
    }

    /// Stable name for project files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Dissolve => "dissolve",
            Self::Darken => "darken",
            Self::Multiply => "multiply",
            Self::ColorBurn => "color_burn",
            Self::LinearBurn => "linear_burn",
            Self::DarkerColor => "darker_color",
            Self::Lighten => "lighten",
            Self::Screen => "screen",
            Self::ColorDodge => "color_dodge",
            Self::LinearDodge => "linear_dodge",
            Self::LighterColor => "lighter_color",
            Self::Overlay => "overlay",
            Self::SoftLight => "soft_light",
            Self::HardLight => "hard_light",
            Self::VividLight => "vivid_light",
            Self::LinearLight => "linear_light",
            Self::PinLight => "pin_light",
            Self::HardMix => "hard_mix",
            Self::Difference => "difference",
            Self::Exclusion => "exclusion",
            Self::Subtract => "subtract",
            Self::Divide => "divide",
            Self::Parallel => "parallel",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Color => "color",
            Self::Luminosity => "luminosity",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::GROUPS
            .iter()
            .flat_map(|g| g.iter())
            .copied()
            .find(|m| m.key() == key)
    }
}

/// The colour space layers and strokes blend in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlendSpace {
    /// Physically correct light mixing: colours are decoded from sRGB,
    /// blended, and encoded again.
    #[default]
    Linear,
    /// Blend the stored sRGB values directly, like Photoshop's 8-bit
    /// documents.
    Gamma,
}

// --- separable modes: one channel at a time, unpremultiplied, 0..=1 ---

#[inline]
fn color_dodge(b: f32, s: f32) -> f32 {
    if b <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (b / (1.0 - s)).min(1.0)
    }
}

#[inline]
fn color_burn(b: f32, s: f32) -> f32 {
    if b >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - b) / s).min(1.0)
    }
}

#[inline]
fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

#[inline]
fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

#[inline]
fn vivid_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        color_burn(b, 2.0 * s)
    } else {
        color_dodge(b, 2.0 * s - 1.0)
    }
}

/// Parallel: `2 / (1/b + 1/s)`, nothing where either channel is (next to)
/// nothing, as its harmonic mean tends to. (Taking 1 for such a channel's
/// reciprocal instead would turn a channel both colours lack full: yellow
/// over yellow would come out white.)
#[inline]
fn parallel(b: f32, s: f32) -> f32 {
    if b <= f32::EPSILON || s <= f32::EPSILON {
        return 0.0;
    }
    (2.0 / (1.0 / b + 1.0 / s)).clamp(0.0, 1.0)
}

#[inline]
fn separable(mode: LayerBlend, b: f32, s: f32) -> f32 {
    match mode {
        LayerBlend::Darken => b.min(s),
        LayerBlend::Multiply => b * s,
        LayerBlend::ColorBurn => color_burn(b, s),
        LayerBlend::LinearBurn => (b + s - 1.0).max(0.0),
        LayerBlend::Lighten => b.max(s),
        LayerBlend::Screen => b + s - b * s,
        LayerBlend::ColorDodge => color_dodge(b, s),
        LayerBlend::LinearDodge => (b + s).min(1.0),
        LayerBlend::Overlay => hard_light(s, b),
        LayerBlend::SoftLight => soft_light(b, s),
        LayerBlend::HardLight => hard_light(b, s),
        LayerBlend::VividLight => vivid_light(b, s),
        LayerBlend::LinearLight => (b + 2.0 * s - 1.0).clamp(0.0, 1.0),
        LayerBlend::PinLight => {
            if s <= 0.5 {
                b.min(2.0 * s)
            } else {
                b.max(2.0 * s - 1.0)
            }
        }
        LayerBlend::HardMix => {
            if b + s >= 1.0 {
                1.0
            } else {
                0.0
            }
        }
        LayerBlend::Difference => (b - s).abs(),
        LayerBlend::Exclusion => b + s - 2.0 * b * s,
        LayerBlend::Subtract => (b - s).max(0.0),
        LayerBlend::Divide => {
            if s <= 0.0 {
                if b <= 0.0 { 0.0 } else { 1.0 }
            } else {
                (b / s).min(1.0)
            }
        }
        LayerBlend::Parallel => parallel(b, s),
        _ => s,
    }
}

// --- non-separable modes (W3C definitions) ---

#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 {
        let d = l - n;
        if d > 0.0 {
            c = c.map(|v| l + (v - l) * l / d);
        }
    }
    if x > 1.0 {
        let d = x - l;
        if d > 0.0 {
            c = c.map(|v| l + (v - l) * (1.0 - l) / d);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    if max <= min {
        return [0.0; 3];
    }
    c.map(|v| (v - min) * s / (max - min))
}

/// `B(Cb, Cs)` for any mode (Normal/Dissolve return the source).
pub fn blend_color(mode: LayerBlend, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
    match mode {
        LayerBlend::Normal | LayerBlend::Dissolve => s,
        LayerBlend::DarkerColor => {
            if lum(s) < lum(b) {
                s
            } else {
                b
            }
        }
        LayerBlend::LighterColor => {
            if lum(s) > lum(b) {
                s
            } else {
                b
            }
        }
        LayerBlend::Hue => set_lum(set_sat(s, sat(b)), lum(b)),
        LayerBlend::Saturation => set_lum(set_sat(b, sat(s)), lum(b)),
        LayerBlend::Color => set_lum(s, lum(b)),
        LayerBlend::Luminosity => set_lum(b, lum(s)),
        _ => [
            separable(mode, b[0], s[0]),
            separable(mode, b[1], s[1]),
            separable(mode, b[2], s[2]),
        ],
    }
}

pub(crate) const MIN_ALPHA: f32 = 1e-6;

/// Composite premultiplied `src` onto premultiplied `dst` with `mode`.
/// `dissolve_noise` (0..1, stable per pixel) decides Dissolve's pixels: a
/// pixel shows the source fully opaque where the noise is below its alpha.
#[inline]
pub fn composite(mode: LayerBlend, src: Rgba, dst: Rgba, dissolve_noise: f32) -> Rgba {
    let a_s = src.a();
    // Below this the source can't show in 8 bits, and un-premultiplying
    // it would divide by almost nothing (0 × inf = NaN, drawn black).
    // NaN takes the branch too.
    if a_s.is_nan() || a_s <= MIN_ALPHA {
        return dst;
    }
    if mode == LayerBlend::Normal {
        return src + dst * (1.0 - a_s);
    }
    if mode == LayerBlend::Dissolve {
        if dissolve_noise >= a_s {
            return dst;
        }
        let inv = 1.0 / a_s;
        let opaque =
            Rgba::from_rgba_premultiplied(src.r() * inv, src.g() * inv, src.b() * inv, 1.0);
        return opaque;
    }
    let a_b = dst.a();
    if a_b.is_nan() || a_b <= MIN_ALPHA {
        return src;
    }
    let unpremul = |c: Rgba, a: f32| {
        let inv = 1.0 / a;
        [c.r() * inv, c.g() * inv, c.b() * inv].map(|v| v.clamp(0.0, 1.0))
    };
    let mixed = blend_color(mode, unpremul(dst, a_b), unpremul(src, a_s));
    let both = a_s * a_b;
    let (keep_s, keep_b) = (1.0 - a_b, 1.0 - a_s);
    Rgba::from_rgba_premultiplied(
        src.r() * keep_s + dst.r() * keep_b + both * mixed[0],
        src.g() * keep_s + dst.g() * keep_b + both * mixed[1],
        src.b() * keep_s + dst.b() * keep_b + both * mixed[2],
        a_s + a_b - both,
    )
}

/// Deterministic per-pixel noise in 0..1 for Dissolve, from canvas pixel
/// coordinates (stable across redraws, so the pattern doesn't crawl).
#[inline]
pub fn pixel_noise(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(r: f32, g: f32, b: f32) -> Rgba {
        Rgba::from_rgba_premultiplied(r, g, b, 1.0)
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn parallel_leaves_a_channel_neither_colour_has_empty() {
        // Yellow over yellow is yellow (it once came out white), and a
        // channel only one colour has is nothing.
        let yellow = [1.0, 0.6, 0.0];
        assert!(close(
            blend_color(LayerBlend::Parallel, yellow, yellow),
            yellow
        ));
        let blue = [0.0, 0.0, 1.0];
        assert!(close(
            blend_color(LayerBlend::Parallel, yellow, blue),
            [0.0; 3]
        ));
    }

    #[test]
    fn every_mode_is_listed_once_and_round_trips_its_key() {
        let all: Vec<LayerBlend> = LayerBlend::GROUPS
            .iter()
            .flat_map(|g| g.iter())
            .copied()
            .collect();
        assert_eq!(all.len(), 28);
        for m in &all {
            assert_eq!(all.iter().filter(|x| *x == m).count(), 1);
            assert_eq!(LayerBlend::from_key(m.key()), Some(*m));
        }
    }

    #[test]
    fn reference_values() {
        let b = [0.2, 0.5, 0.8];
        let s = [0.6, 0.5, 0.1];
        let cases: [(LayerBlend, [f32; 3]); 12] = [
            (LayerBlend::Multiply, [0.12, 0.25, 0.08]),
            (LayerBlend::Screen, [0.68, 0.75, 0.82]),
            (LayerBlend::Darken, [0.2, 0.5, 0.1]),
            (LayerBlend::Lighten, [0.6, 0.5, 0.8]),
            (LayerBlend::Difference, [0.4, 0.0, 0.7]),
            (LayerBlend::Exclusion, [0.56, 0.5, 0.74]),
            (LayerBlend::LinearDodge, [0.8, 1.0, 0.9]),
            (LayerBlend::LinearBurn, [0.0, 0.0, 0.0]),
            (LayerBlend::Subtract, [0.0, 0.0, 0.7]),
            // Overlay = HardLight with the layers swapped.
            (
                LayerBlend::Overlay,
                [0.2 * 2.0 * 0.6, 0.5, 1.0 - 2.0 * 0.2 * 0.9],
            ),
            (LayerBlend::HardMix, [0.0, 1.0, 0.0]),
            (LayerBlend::PinLight, [0.2, 0.5, 0.2]),
        ];
        for (mode, expected) in cases {
            assert!(
                close(blend_color(mode, b, s), expected),
                "{mode:?}: {:?}",
                blend_color(mode, b, s)
            );
        }
    }

    #[test]
    fn reference_values_of_the_other_modes() {
        // W3C / Photoshop formulas worked by hand for the same inputs.
        let b = [0.2, 0.5, 0.8];
        let s = [0.6, 0.5, 0.1];
        let cases: [(LayerBlend, [f32; 3]); 10] = [
            (LayerBlend::ColorDodge, [0.5, 1.0, 0.8 / 0.9]),
            (LayerBlend::ColorBurn, [0.0, 0.0, 0.0]),
            (LayerBlend::HardLight, [0.36, 0.5, 0.16]),
            // Upper half: b + (2s − 1)(D(b) − b), D(0.2) = 0.448.
            (LayerBlend::SoftLight, [0.2496, 0.5, 0.672]),
            (LayerBlend::VividLight, [0.25, 0.5, 0.0]),
            (LayerBlend::LinearLight, [0.4, 0.5, 0.0]),
            (LayerBlend::Divide, [0.2 / 0.6, 1.0, 1.0]),
            // 2 / (1/b + 1/s).
            (LayerBlend::Parallel, [0.3, 0.5, 2.0 / 11.25]),
            // Whole colours: lum(b) = 0.443 < lum(s) = 0.486.
            (LayerBlend::DarkerColor, b),
            (LayerBlend::LighterColor, s),
        ];
        for (mode, expected) in cases {
            assert!(
                close(blend_color(mode, b, s), expected),
                "{mode:?}: {:?}",
                blend_color(mode, b, s)
            );
        }
    }

    #[test]
    fn a_nearly_invisible_source_never_makes_nan() {
        let dst = opaque(1.0, 1.0, 1.0);
        for &mode in LayerBlend::GROUPS.iter().flat_map(|g| g.iter()) {
            for a in [1e-39_f32, 3e-39, 1e-20, 1e-7, f32::NAN] {
                let src = Rgba::from_rgba_premultiplied(0.0, 0.0, 0.0, a);
                let out = composite(mode, src, dst, 0.5);
                let c = [out.r(), out.g(), out.b(), out.a()];
                assert!(c.iter().all(|v| v.is_finite()), "{mode:?} at {a}: {c:?}");
                assert!(
                    c.iter().all(|v| (v - 1.0).abs() < 1e-4),
                    "{mode:?} at {a}: {c:?}"
                );
            }
        }
    }

    #[test]
    fn component_modes_keep_the_right_components() {
        let b = [0.8, 0.3, 0.2];
        let s = [0.1, 0.4, 0.9];
        // Luminosity takes the source's luminance; Color keeps the backdrop's.
        assert!((lum(blend_color(LayerBlend::Luminosity, b, s)) - lum(s)).abs() < 1e-4);
        assert!((lum(blend_color(LayerBlend::Color, b, s)) - lum(b)).abs() < 1e-4);
        assert!((lum(blend_color(LayerBlend::Hue, b, s)) - lum(b)).abs() < 1e-4);
        assert!((sat(blend_color(LayerBlend::Saturation, b, s)) - sat(s)).abs() < 1e-3);
    }

    #[test]
    fn normal_matches_source_over_and_transparent_backdrop_shows_source() {
        let src = Rgba::from_rgba_premultiplied(0.3, 0.1, 0.0, 0.5);
        let dst = opaque(0.2, 0.4, 0.6);
        let n = composite(LayerBlend::Normal, src, dst, 0.0);
        let expect = src + dst * 0.5;
        assert!(close(
            [n.r(), n.g(), n.b()],
            [expect.r(), expect.g(), expect.b()]
        ));
        for group in LayerBlend::GROUPS {
            for &mode in group {
                if mode == LayerBlend::Dissolve {
                    continue;
                }
                let out = composite(mode, src, Rgba::TRANSPARENT, 0.0);
                assert!(
                    close([out.r(), out.g(), out.b()], [src.r(), src.g(), src.b()]),
                    "{mode:?}"
                );
                assert!((out.a() - src.a()).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn multiply_with_white_and_screen_with_black_are_identity() {
        let src = opaque(1.0, 1.0, 1.0);
        let dst = opaque(0.3, 0.6, 0.9);
        let m = composite(LayerBlend::Multiply, src, dst, 0.0);
        assert!(close([m.r(), m.g(), m.b()], [0.3, 0.6, 0.9]));
        let s = composite(LayerBlend::Screen, opaque(0.0, 0.0, 0.0), dst, 0.0);
        assert!(close([s.r(), s.g(), s.b()], [0.3, 0.6, 0.9]));
    }

    #[test]
    fn dissolve_is_all_or_nothing_and_stable() {
        let src = Rgba::from_rgba_premultiplied(0.25, 0.0, 0.0, 0.5);
        let dst = opaque(0.0, 0.0, 1.0);
        assert_eq!(composite(LayerBlend::Dissolve, src, dst, 0.9), dst);
        let shown = composite(LayerBlend::Dissolve, src, dst, 0.1);
        assert!((shown.a() - 1.0).abs() < 1e-6 && (shown.r() - 0.5).abs() < 1e-6);
        assert_eq!(pixel_noise(12, 34), pixel_noise(12, 34));
        let mean: f32 = (0..10_000)
            .map(|i| pixel_noise(i % 100, i / 100))
            .sum::<f32>()
            / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "noise mean {mean}");
    }
}
