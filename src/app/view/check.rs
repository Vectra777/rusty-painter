//! Check views: the canvas shown in grey, in a few bands of value, or as how
//! saturated its colours are, to judge the picture without changing it. The
//! canvas view and the navigator each have their own.
//!
//! The display shaders do it as they draw ([`WGSL`], with the mode in a
//! uniform); [`CheckMode::apply`] is the same in Rust, for pictures built on
//! the CPU.

/// How the canvas is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CheckMode {
    /// As it is.
    #[default]
    Colour,
    /// Grey of the same luminance.
    Grey,
    /// Grey in a few even steps of lightness (`values` of them).
    Values,
    /// Grey as bright as the colour is saturated (its Oklab chroma).
    Saturation,
}

impl CheckMode {
    pub const ALL: [CheckMode; 4] = [
        CheckMode::Colour,
        CheckMode::Grey,
        CheckMode::Values,
        CheckMode::Saturation,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CheckMode::Colour => "Colour",
            CheckMode::Grey => "Grey",
            CheckMode::Values => "Values",
            CheckMode::Saturation => "Saturation",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            CheckMode::Colour => "The picture as it is",
            CheckMode::Grey => "Grey of the same lightness: are the values right?",
            CheckMode::Values => {
                "A few flat steps of lightness, light to dark: does it read as big shapes?"
            }
            CheckMode::Saturation => "White where the colour is strongest, black where it's grey",
        }
    }

    /// The uniform the display shaders read ([`WGSL`]'s `Check`).
    pub fn uniform(self, values: u32) -> [u8; 16] {
        let mode: u32 = match self {
            CheckMode::Colour => 0,
            CheckMode::Grey => 1,
            CheckMode::Values => 2,
            CheckMode::Saturation => 3,
        };
        let mut bytes = [0u8; 16];
        bytes[..4].copy_from_slice(&mode.to_le_bytes());
        bytes[4..8].copy_from_slice(&(clamp_values(values) as f32).to_le_bytes());
        bytes
    }

    /// `c` (premultiplied, sRGB-encoded, as tiles hold it) shown this way.
    pub fn apply(self, c: [u8; 4], values: u32) -> [u8; 4] {
        if self == CheckMode::Colour || c[3] == 0 {
            return c;
        }
        let a = c[3] as f32 / 255.0;
        let lin = [0, 1, 2].map(|i| linear((c[i] as f32 / 255.0 / a).min(1.0)));
        let y = match self {
            CheckMode::Colour => unreachable!(),
            CheckMode::Grey => luminance(lin),
            CheckMode::Values => {
                let n = clamp_values(values) as f32;
                let step = (lightness(luminance(lin)) * n).floor().min(n - 1.0);
                from_lightness(step / (n - 1.0))
            }
            CheckMode::Saturation => from_lightness((chroma(lin) / MAX_CHROMA).min(1.0)),
        };
        let g = (gamma(y) * a * 255.0).round().clamp(0.0, 255.0) as u8;
        [g, g, g, c[3]]
    }
}

/// Steps of value, at least black and white.
pub const VALUES: std::ops::RangeInclusive<u32> = 2..=8;

pub fn clamp_values(values: u32) -> u32 {
    values.clamp(*VALUES.start(), *VALUES.end())
}

/// Oklab chroma shown white: about the strongest red, green or blue.
const MAX_CHROMA: f32 = 0.3;

fn linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn gamma(l: f32) -> f32 {
    if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// CIE L* (0 to 1) of luminance `y`.
fn lightness(y: f32) -> f32 {
    if y <= 216.0 / 24389.0 {
        y * 24389.0 / 27.0 / 100.0
    } else {
        (116.0 * y.cbrt() - 16.0) / 100.0
    }
}

/// The luminance of CIE L* `l` (0 to 1).
fn from_lightness(l: f32) -> f32 {
    let l = l * 100.0;
    if l <= 8.0 {
        l * 27.0 / 24389.0
    } else {
        ((l + 16.0) / 116.0).powi(3)
    }
}

fn chroma(c: [f32; 3]) -> f32 {
    let l = (0.41222147 * c[0] + 0.53633254 * c[1] + 0.051445993 * c[2]).cbrt();
    let m = (0.2119035 * c[0] + 0.6806995 * c[1] + 0.10739696 * c[2]).cbrt();
    let s = (0.08830246 * c[0] + 0.28171884 * c[1] + 0.6299787 * c[2]).cbrt();
    let a = 1.9779985 * l - 2.4285922 * m + 0.4505937 * s;
    let b = 0.025904037 * l + 0.78277177 * m - 0.80867577 * s;
    (a * a + b * b).sqrt()
}

/// The display shaders' side: `check_view(stored, a)` takes and returns
/// premultiplied sRGB-encoded colour. Needs `linear_from_gamma_rgb` and
/// `gamma_from_linear_rgb` from the shader it's put in, and the uniform at
/// group 1, binding 0.
pub const WGSL: &str = r#"
struct Check {
    mode: u32,
    values: f32,
    pad0: f32,
    pad1: f32,
};
@group(1) @binding(0) var<uniform> check: Check;

fn check_lightness(y: f32) -> f32 {
    if (y <= 216.0 / 24389.0) { return y * 24389.0 / 27.0 / 100.0; }
    return (116.0 * pow(y, 1.0 / 3.0) - 16.0) / 100.0;
}

fn check_from_lightness(l: f32) -> f32 {
    let l100 = l * 100.0;
    if (l100 <= 8.0) { return l100 * 27.0 / 24389.0; }
    let f = (l100 + 16.0) / 116.0;
    return f * f * f;
}

fn check_chroma(c: vec3<f32>) -> f32 {
    let lms = vec3<f32>(
        dot(c, vec3<f32>(0.41222147, 0.53633254, 0.051445993)),
        dot(c, vec3<f32>(0.2119035, 0.6806995, 0.10739696)),
        dot(c, vec3<f32>(0.08830246, 0.28171884, 0.6299787)),
    );
    let r = pow(max(lms, vec3<f32>(0.0)), vec3<f32>(1.0 / 3.0));
    let a = dot(r, vec3<f32>(1.9779985, -2.4285922, 0.4505937));
    let b = dot(r, vec3<f32>(0.025904037, 0.78277177, -0.80867577));
    return sqrt(a * a + b * b);
}

fn check_view(stored: vec3<f32>, alpha: f32) -> vec3<f32> {
    if (check.mode == 0u || alpha <= 0.0) { return stored; }
    let lin = linear_from_gamma_rgb(min(stored / alpha, vec3<f32>(1.0)));
    var y = dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722));
    if (check.mode == 2u) {
        let n = check.values;
        let step = min(floor(check_lightness(y) * n), n - 1.0);
        y = check_from_lightness(step / (n - 1.0));
    } else if (check.mode == 3u) {
        y = check_from_lightness(min(check_chroma(lin) / 0.3, 1.0));
    }
    return gamma_from_linear_rgb(vec3<f32>(y)) * alpha;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_is_left_alone() {
        for c in [[200, 30, 90, 255], [10, 20, 30, 40], [0, 0, 0, 0]] {
            assert_eq!(CheckMode::Colour.apply(c, 3), c);
        }
    }

    #[test]
    fn grey_keeps_luminance_and_greys_stay_grey() {
        for v in [0u8, 1, 60, 128, 200, 255] {
            assert_eq!(CheckMode::Grey.apply([v, v, v, 255], 3), [v, v, v, 255]);
        }
        // Pure green is much lighter than pure blue.
        let green = CheckMode::Grey.apply([0, 255, 0, 255], 3)[0];
        let blue = CheckMode::Grey.apply([0, 0, 255, 255], 3)[0];
        assert!(green > 200 && blue < 90, "{green} {blue}");
    }

    #[test]
    fn premultiplied_colour_stays_premultiplied() {
        // Half-covered white: grey of white, at half coverage.
        assert_eq!(
            CheckMode::Grey.apply([128, 128, 128, 128], 3),
            [128, 128, 128, 128]
        );
    }

    #[test]
    fn values_come_in_even_steps() {
        let shown: std::collections::BTreeSet<u8> = (0..=255u8)
            .map(|v| CheckMode::Values.apply([v, v, v, 255], 3)[0])
            .collect();
        // Black, middle grey (L* 50) and white.
        assert_eq!(shown.into_iter().collect::<Vec<_>>(), vec![0, 119, 255]);
        let two: std::collections::BTreeSet<u8> = (0..=255u8)
            .map(|v| CheckMode::Values.apply([v, v, v, 255], 2)[0])
            .collect();
        assert_eq!(two.into_iter().collect::<Vec<_>>(), vec![0, 255]);
    }

    #[test]
    fn saturation_is_dark_for_grey_and_bright_for_strong_colour() {
        for v in [0u8, 128, 255] {
            assert!(CheckMode::Saturation.apply([v, v, v, 255], 3)[0] < 3);
        }
        let red = CheckMode::Saturation.apply([255, 0, 0, 255], 3)[0];
        let dull = CheckMode::Saturation.apply([150, 120, 110, 255], 3)[0];
        assert!(red > 200 && dull < 120, "{red} {dull}");
    }

    #[test]
    fn the_uniform_holds_the_mode_and_steps() {
        let u = CheckMode::Values.uniform(40);
        assert_eq!(u32::from_le_bytes(u[..4].try_into().unwrap()), 2);
        assert_eq!(f32::from_le_bytes(u[4..8].try_into().unwrap()), 8.0);
    }
}
