//! Filters (the Filter menu): colour adjustments, blurs and effects over a
//! row-major buffer of premultiplied sRGB pixels. Pure functions: the
//! Filter tool reads the layer into a buffer, runs [`Filter::apply`] and
//! writes the result back with undo.
//!
//! Colour adjustments work on unpremultiplied sRGB values, as Photoshop's
//! do; filters that mix neighbouring pixels (blurs and most effects) work
//! on sRGB values times alpha (`to_mixable`), so transparent pixels
//! neither darken nor lighten the edges.

use crate::canvas::blend::Unmultiply;
use crate::canvas::effects::{self, Frame};
use eframe::egui::Color32;
use rayon::prelude::*;
use std::sync::OnceLock;

/// Pixels as stored (egui premultiplies in linear light, then encodes as
/// sRGB) to sRGB values times alpha: the space averages have to be taken
/// in. Averaging the stored values directly turns a white line's soft edge
/// grey, since half of an sRGB-encoded value is far less than half the
/// light.
pub(crate) fn to_mixable(src: &[Color32]) -> Vec<Color32> {
    src.par_iter().map(|&c| mixable(c)).collect()
}

#[inline]
pub(crate) fn mixable(c: Color32) -> Color32 {
    match c.a() {
        0 => Color32::TRANSPARENT,
        255 => c,
        a => {
            let [r, g, b, _] = c.unmultiplied();
            mixable_rgba(r, g, b, a)
        }
    }
}

/// Unmultiplied sRGB to the [`to_mixable`] space.
#[inline]
pub(crate) fn mixable_rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    let m = |v: u8| ((v as u32 * a as u32 + 127) / 255) as u8;
    Color32::from_rgba_premultiplied(m(r), m(g), m(b), a)
}

/// A [`to_mixable`] pixel's unmultiplied sRGB values.
#[inline]
pub(crate) fn unmix(c: Color32) -> [u8; 4] {
    let a = c.a() as u32;
    if a == 0 {
        return [0; 4];
    }
    let u = |v: u8| ((v as u32 * 255 + a / 2) / a).min(255) as u8;
    [u(c.r()), u(c.g()), u(c.b()), c.a()]
}

/// Back from [`to_mixable`]: pixels the filter left as they were
/// (`out[i] == mixed[i]`) come back exactly as `src` had them.
pub(crate) fn from_mixable(out: &[Color32], mixed: &[Color32], src: &[Color32]) -> Vec<Color32> {
    // egui's premultiply per (alpha, value): it runs powers per channel.
    static ENCODE: OnceLock<Vec<u8>> = OnceLock::new();
    let lut = ENCODE.get_or_init(|| {
        let mut t = vec![0u8; 256 * 256];
        for a in 0..=255u8 {
            for v in 0..=255u8 {
                t[a as usize * 256 + v as usize] = Color32::from_rgba_unmultiplied(v, v, v, a).r();
            }
        }
        t
    });
    out.par_iter()
        .zip(mixed.par_iter())
        .zip(src.par_iter())
        .map(|((&o, &m), &s)| {
            if o == m {
                return s;
            }
            match o.a() {
                0 => Color32::TRANSPARENT,
                255 => o,
                a => {
                    let [r, g, b, _] = unmix(o);
                    let row = &lut[a as usize * 256..][..256];
                    Color32::from_rgba_premultiplied(
                        row[r as usize],
                        row[g as usize],
                        row[b as usize],
                        a,
                    )
                }
            }
        })
        .collect()
}

/// sRGB value (0..=1) to linear light.
fn srgb_to_linear(v: f32) -> f32 {
    let v = v.max(0.0);
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to an sRGB value (0..=1, unclamped above).
fn linear_to_srgb(v: f32) -> f32 {
    let v = v.max(0.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Filter {
    /// Both -1..=1.
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    /// Hue in degrees (-180..=180); saturation and lightness -1..=1.
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    /// Input levels: `black` and `white` points (0..=1) and the midtone
    /// gamma (1 = unchanged, more brightens).
    Levels {
        black: f32,
        white: f32,
        gamma: f32,
    },
    Invert,
    Desaturate,
    /// 2..=32 levels per channel.
    Posterize {
        levels: u8,
    },
    /// Black below `level` (0..=1), white from it.
    Threshold {
        level: f32,
    },
    /// Tone curves: each channel through its own curve, after the master
    /// (RGB) curve.
    Curves {
        rgb: ToneCurve,
        red: ToneCurve,
        green: ToneCurve,
        blue: ToneCurve,
    },
    /// Shift the shadows, midtones and highlights towards red, green or blue
    /// (or cyan, magenta, yellow): each -1..=1 per channel.
    ColourBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        /// Keep each pixel's lightness.
        preserve_luminosity: bool,
    },
    /// Each pixel's brightness picks a colour along the gradient (dark at
    /// the start, light at the end); transparency stays.
    GradientMap(GradientMap),
    /// Radius in pixels (the Gaussian's standard deviation).
    GaussianBlur {
        radius: f32,
    },
    /// Angle in degrees, distance in pixels.
    MotionBlur {
        angle: f32,
        distance: f32,
    },
    /// Unsharp mask: blur radius and how much of the difference to add.
    Sharpen {
        radius: f32,
        amount: f32,
    },
    /// Amount 0..=1; mono: the same noise in every channel; `size` of the
    /// grain in pixels (1 = every pixel its own).
    Noise {
        amount: f32,
        mono: bool,
        #[serde(default = "one")]
        size: f32,
    },
    /// Block size in pixels, on the canvas grid.
    Pixelate {
        size: u32,
    },
    /// Brightness to opacity, for scanned line art: pixels at or below
    /// `black` stay opaque, at or above `white` become transparent. The
    /// lines turn black unless `keep_color`.
    LineArt {
        black: f32,
        white: f32,
        keep_color: bool,
    },
    /// Stops of light (-4..=4), in linear light as a camera would.
    Exposure {
        stops: f32,
    },
    /// Warmer (+) or cooler (−), and greener (+) or more magenta (−): -1..=1.
    Temperature {
        temperature: f32,
        tint: f32,
    },
    /// More (or less) saturation, the dull colours most (-1..=1).
    Vibrance {
        amount: f32,
    },
    /// Towards an old photograph's brown, `amount` 0..=1.
    Sepia {
        amount: f32,
    },
    /// Tones above `level` inverted, as over-exposed film.
    Solarize {
        level: f32,
    },
    /// The bright parts (above `threshold`) bleed light: blur `radius`,
    /// `strength` 0..=2.
    Glow {
        radius: f32,
        strength: f32,
        threshold: f32,
    },
    /// Red and blue pulled apart towards the edges, `amount` pixels at the
    /// corners.
    ChromaticAberration {
        amount: f32,
        frame: crate::canvas::effects::Frame,
    },
    /// Printed dots, `size` pixels apart on a grid turned by `angle`.
    Halftone {
        size: f32,
        angle: f32,
        colour: bool,
    },
    /// A grey relief lit from `angle` degrees.
    Emboss {
        angle: f32,
        depth: f32,
    },
    /// Edges as dark lines on white.
    FindEdges,
    /// Grey clouds over the whole layer: `scale` pixels, `detail` octaves.
    Clouds {
        scale: f32,
        detail: u8,
    },
    /// Specks removed (median of each channel over `radius`).
    Median {
        radius: u32,
    },
    /// Flat painted areas with crisp edges (Kuwahara), `radius` pixels.
    OilPaint {
        radius: u32,
    },
    /// Darker (or lighter, negative) towards the edges, from `size` out.
    Vignette {
        amount: f32,
        size: f32,
        frame: crate::canvas::effects::Frame,
    },
    /// Streaks out from the middle, `amount` of each pixel's distance.
    ZoomBlur {
        amount: f32,
        frame: crate::canvas::effects::Frame,
    },
    /// Turning around the middle by `angle` degrees.
    SpinBlur {
        angle: f32,
        frame: crate::canvas::effects::Frame,
    },
    /// Fewer colours with an ordered pattern: `levels` per channel.
    Dither {
        levels: u8,
    },
}

fn one() -> f32 {
    1.0
}

/// Most points a tone curve holds.
pub const CURVE_POINTS: usize = 16;

/// A tone curve: input 0..=1 to output 0..=1 through its points (sorted by
/// input), with smooth monotone interpolation between them. A fixed-size
/// array of 0..=255 steps (as Photoshop keeps them), so filters stay small
/// and `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToneCurve {
    len: u8,
    points: [[u8; 2]; CURVE_POINTS],
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self::from_points(&[[0.0, 0.0], [1.0, 1.0]])
    }
}

impl ToneCurve {
    /// Through `points` (the first [`CURVE_POINTS`] of them), sorted.
    pub fn from_points(points: &[[f32; 2]]) -> Self {
        let mut curve = Self {
            len: 0,
            points: [[0; 2]; CURVE_POINTS],
        };
        for (slot, p) in curve.points.iter_mut().zip(points) {
            *slot = p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            curve.len += 1;
        }
        curve.points[..curve.len as usize].sort_by_key(|p| p[0]);
        curve
    }

    /// The points, 0..=1.
    pub fn points(&self) -> Vec<[f32; 2]> {
        self.points[..(self.len as usize).min(CURVE_POINTS)]
            .iter()
            .map(|p| p.map(|v| v as f32 / 255.0))
            .collect()
    }

    /// The straight line from (0, 0) to (1, 1): changes nothing.
    pub fn is_identity(&self) -> bool {
        self.points().iter().all(|p| p[0] == p[1])
    }

    /// As the curve editor's type.
    pub fn to_softness(&self) -> crate::brush_engine::hardness::SoftnessCurve {
        use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
        SoftnessCurve {
            points: self
                .points()
                .iter()
                .map(|p| CurvePoint::new(p[0], p[1]))
                .collect(),
        }
    }

    pub fn from_softness(curve: &crate::brush_engine::hardness::SoftnessCurve) -> Self {
        let points: Vec<[f32; 2]> = curve.points.iter().map(|p| [p.x, p.y]).collect();
        Self::from_points(&points)
    }

    /// What each 0..=255 value becomes.
    pub fn table(&self) -> [u8; 256] {
        let mut out = [0u8; 256];
        if self.len == 0 {
            for (v, o) in out.iter_mut().enumerate() {
                *o = v as u8;
            }
            return out;
        }
        let curve = self.to_softness();
        for (v, o) in out.iter_mut().enumerate() {
            *o = (curve.eval(v as f32 / 255.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        out
    }
}

/// A ready-made gradient map: its name and stops.
pub type MapPreset = (&'static str, &'static [(f32, [u8; 3])]);

/// Most stops a gradient map holds.
pub const MAP_STOPS: usize = 8;

/// A gradient map's colours, dark to light.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GradientMap {
    pub len: u8,
    /// Position 0..=1 and sRGB colour, sorted by position.
    pub stops: [(f32, [u8; 3]); MAP_STOPS],
}

impl Default for GradientMap {
    fn default() -> Self {
        Self::from_stops(&[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])])
    }
}

impl GradientMap {
    /// Ready-made maps: name and stops.
    pub const PRESETS: [MapPreset; 6] = [
        (
            "Black to white",
            &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
        ),
        (
            "Sepia",
            &[
                (0.0, [30, 18, 10]),
                (0.55, [150, 105, 65]),
                (1.0, [250, 235, 205]),
            ],
        ),
        (
            "Sunset",
            &[
                (0.0, [35, 10, 60]),
                (0.4, [190, 40, 80]),
                (0.75, [250, 150, 60]),
                (1.0, [255, 240, 190]),
            ],
        ),
        (
            "Cool shadows",
            &[
                (0.0, [15, 25, 70]),
                (0.5, [120, 140, 170]),
                (1.0, [255, 245, 225]),
            ],
        ),
        ("Duotone", &[(0.0, [40, 20, 90]), (1.0, [255, 200, 90])]),
        (
            "Night",
            &[
                (0.0, [0, 0, 10]),
                (0.6, [30, 70, 130]),
                (1.0, [190, 230, 255]),
            ],
        ),
    ];

    pub fn from_stops(stops: &[(f32, [u8; 3])]) -> Self {
        let mut map = Self {
            len: 0,
            stops: [(0.0, [0; 3]); MAP_STOPS],
        };
        for (slot, &(pos, c)) in map.stops.iter_mut().zip(stops) {
            *slot = (pos.clamp(0.0, 1.0), c);
            map.len += 1;
        }
        map.sort();
        map
    }

    pub fn stops(&self) -> &[(f32, [u8; 3])] {
        &self.stops[..(self.len as usize).min(MAP_STOPS)]
    }

    pub fn sort(&mut self) {
        let n = (self.len as usize).min(MAP_STOPS);
        self.stops[..n].sort_by(|a, b| a.0.total_cmp(&b.0));
    }

    /// Light becomes dark and dark light.
    pub fn reversed(&self) -> Self {
        let stops: Vec<_> = self.stops().iter().map(|&(p, c)| (1.0 - p, c)).collect();
        Self::from_stops(&stops)
    }

    /// The colour at `t` (0..=1), sRGB 0..=1.
    pub fn color_at(&self, t: f32) -> [f32; 3] {
        let stops = self.stops();
        let f = |c: [u8; 3]| c.map(|v| v as f32 / 255.0);
        let Some(first) = stops.first() else {
            return [t; 3];
        };
        if t <= first.0 {
            return f(first.1);
        }
        for pair in stops.windows(2) {
            let ((p0, c0), (p1, c1)) = (pair[0], pair[1]);
            if t <= p1 {
                let k = if p1 - p0 > 1e-6 {
                    (t - p0) / (p1 - p0)
                } else {
                    1.0
                };
                let (a, b) = (f(c0), f(c1));
                return [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * k);
            }
        }
        f(stops[stops.len() - 1].1)
    }
}

/// What each 0..=255 value of red, green and blue becomes.
pub type ChannelLut = [[u8; 256]; 3];

/// Largest radius or distance the sliders allow, so the area read around a
/// selection is known before the filter runs.
pub const MAX_REACH: i32 = 200;

impl Filter {
    /// The submenus [`Self::MENU`]'s groups go in (a group of one goes in
    /// the menu itself).
    pub const MENU_GROUPS: [&'static str; 5] = [
        "Adjustments",
        "Blur & Sharpen",
        "Stylize",
        "Noise & Render",
        "Line Art",
    ];

    /// The Filter menu: groups of filters with their default settings.
    pub const MENU: [&'static [Filter]; 5] = [
        &[
            Filter::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            Filter::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            Filter::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 1.0,
            },
            Filter::Invert,
            Filter::Desaturate,
            Filter::Posterize { levels: 6 },
            Filter::Threshold { level: 0.5 },
            Filter::CURVES,
            Filter::COLOUR_BALANCE,
            Filter::GRADIENT_MAP,
            Filter::Exposure { stops: 0.0 },
            Filter::Temperature {
                temperature: 0.0,
                tint: 0.0,
            },
            Filter::Vibrance { amount: 0.0 },
            Filter::Sepia { amount: 1.0 },
            Filter::Solarize { level: 0.5 },
        ],
        &[
            Filter::GaussianBlur { radius: 4.0 },
            Filter::MotionBlur {
                angle: 0.0,
                distance: 20.0,
            },
            Filter::Sharpen {
                radius: 2.0,
                amount: 0.8,
            },
            Filter::ZoomBlur {
                amount: 0.1,
                frame: Frame::NONE,
            },
            Filter::SpinBlur {
                angle: 10.0,
                frame: Frame::NONE,
            },
            Filter::Median { radius: 2 },
        ],
        &[
            Filter::Glow {
                radius: 12.0,
                strength: 0.8,
                threshold: 0.6,
            },
            Filter::ChromaticAberration {
                amount: 6.0,
                frame: Frame::NONE,
            },
            Filter::Halftone {
                size: 8.0,
                angle: 45.0,
                colour: false,
            },
            Filter::Emboss {
                angle: 135.0,
                depth: 2.0,
            },
            Filter::FindEdges,
            Filter::OilPaint { radius: 4 },
            Filter::Vignette {
                amount: 0.6,
                size: 0.4,
                frame: Frame::NONE,
            },
        ],
        &[
            Filter::Noise {
                amount: 0.1,
                mono: true,
                size: 1.0,
            },
            Filter::Pixelate { size: 8 },
            Filter::Dither { levels: 4 },
            Filter::Clouds {
                scale: 128.0,
                detail: 5,
            },
        ],
        &[Filter::LineArt {
            black: 0.25,
            white: 0.85,
            keep_color: false,
        }],
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Filter::BrightnessContrast { .. } => "Brightness / Contrast",
            Filter::HueSaturation { .. } => "Hue / Saturation",
            Filter::Levels { .. } => "Levels",
            Filter::Invert => "Invert",
            Filter::Desaturate => "Desaturate",
            Filter::Posterize { .. } => "Posterize",
            Filter::Threshold { .. } => "Threshold",
            Filter::GaussianBlur { .. } => "Gaussian Blur",
            Filter::MotionBlur { .. } => "Motion Blur",
            Filter::Sharpen { .. } => "Sharpen",
            Filter::Noise { .. } => "Add Noise",
            Filter::Pixelate { .. } => "Pixelate",
            Filter::LineArt { .. } => "Extract Line Art",
            Filter::Curves { .. } => "Curves",
            Filter::ColourBalance { .. } => "Colour Balance",
            Filter::GradientMap(_) => "Gradient Map",
            Filter::Exposure { .. } => "Exposure",
            Filter::Temperature { .. } => "Temperature / Tint",
            Filter::Vibrance { .. } => "Vibrance",
            Filter::Sepia { .. } => "Sepia",
            Filter::Solarize { .. } => "Solarize",
            Filter::Glow { .. } => "Glow",
            Filter::ChromaticAberration { .. } => "Chromatic Aberration",
            Filter::Halftone { .. } => "Halftone",
            Filter::Emboss { .. } => "Emboss",
            Filter::FindEdges => "Find Edges",
            Filter::Clouds { .. } => "Clouds",
            Filter::Median { .. } => "Reduce Noise (Median)",
            Filter::OilPaint { .. } => "Oil Paint",
            Filter::Vignette { .. } => "Vignette",
            Filter::ZoomBlur { .. } => "Zoom Blur",
            Filter::SpinBlur { .. } => "Spin Blur",
            Filter::Dither { .. } => "Dither",
        }
    }

    /// Filters that can be adjustment layers: each pixel changes on its own
    /// (no neighbours, no position), and its transparency stays.
    pub const ADJUSTMENTS: [Filter; 15] = [
        Filter::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
        },
        Filter::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
        },
        Filter::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
        },
        Filter::Invert,
        Filter::Desaturate,
        Filter::Posterize { levels: 6 },
        Filter::Threshold { level: 0.5 },
        Filter::CURVES,
        Filter::COLOUR_BALANCE,
        Filter::GRADIENT_MAP,
        Filter::Exposure { stops: 0.0 },
        Filter::Temperature {
            temperature: 0.0,
            tint: 0.0,
        },
        Filter::Vibrance { amount: 0.0 },
        Filter::Sepia { amount: 1.0 },
        Filter::Solarize { level: 0.5 },
    ];

    /// This filter set up for a `w`×`h` canvas: the ones that work from its
    /// middle learn where that is.
    pub fn fitted(self, w: usize, h: usize) -> Filter {
        let canvas = Frame::of_canvas(w, h);
        let mut f = self;
        if let Filter::ChromaticAberration { frame, .. }
        | Filter::Vignette { frame, .. }
        | Filter::ZoomBlur { frame, .. }
        | Filter::SpinBlur { frame, .. } = &mut f
        {
            *frame = canvas;
        }
        f
    }

    const IDENTITY_CURVE: ToneCurve = {
        let mut points = [[0; 2]; CURVE_POINTS];
        points[1] = [255, 255];
        ToneCurve { len: 2, points }
    };
    const CURVES: Filter = Filter::Curves {
        rgb: Self::IDENTITY_CURVE,
        red: Self::IDENTITY_CURVE,
        green: Self::IDENTITY_CURVE,
        blue: Self::IDENTITY_CURVE,
    };
    const COLOUR_BALANCE: Filter = Filter::ColourBalance {
        shadows: [0.0; 3],
        midtones: [0.0; 3],
        highlights: [0.0; 3],
        preserve_luminosity: true,
    };
    const GRADIENT_MAP: Filter = Filter::GradientMap(GradientMap {
        len: 2,
        stops: {
            let mut stops = [(0.0, [0u8; 3]); MAP_STOPS];
            stops[1] = (1.0, [255; 3]);
            stops
        },
    });

    /// This filter on one composited pixel (adjustment layers), through
    /// `lut` when it has one (see [`Filter::channel_lut`]).
    pub fn adjust(&self, c: Color32, lut: Option<&ChannelLut>) -> Color32 {
        match lut {
            Some(lut) => with_lut(c, lut),
            None => self.pixel(c),
        }
    }

    /// For filters that change each channel on its own (levels,
    /// brightness/contrast, invert, posterize, curves): what each 0..=255
    /// value of each channel becomes, so a pixel costs three lookups
    /// instead of the maths.
    pub fn channel_lut(&self) -> Option<Box<ChannelLut>> {
        if let Filter::Curves {
            rgb,
            red,
            green,
            blue,
        } = self
        {
            let master = rgb.table();
            let mut lut = Box::new([[0u8; 256]; 3]);
            for (table, curve) in lut.iter_mut().zip([red, green, blue]) {
                let own = curve.table();
                for (v, out) in table.iter_mut().enumerate() {
                    *out = own[master[v] as usize];
                }
            }
            return Some(lut);
        }
        if !matches!(
            self,
            Filter::BrightnessContrast { .. }
                | Filter::Levels { .. }
                | Filter::Invert
                | Filter::Posterize { .. }
                | Filter::Exposure { .. }
                | Filter::Temperature { .. }
                | Filter::Solarize { .. }
        ) {
            return None;
        }
        // Each channel on its own: a grey in gives each channel's table.
        let mut lut = Box::new([[0u8; 256]; 3]);
        for v in 0..=255u8 {
            let [r, g, b, _] = self.pixel(Color32::from_rgb(v, v, v)).to_array();
            lut[0][v as usize] = r;
            lut[1][v as usize] = g;
            lut[2][v as usize] = b;
        }
        Some(lut)
    }

    /// Has settings (the menu opens a dialog), rather than applying at once.
    pub fn has_settings(&self) -> bool {
        !matches!(
            self,
            Filter::Invert | Filter::Desaturate | Filter::FindEdges
        )
    }

    /// The same filter on a picture `block` times smaller (a quick
    /// preview): its distances shrink with it.
    pub fn shrunk(&self, block: usize) -> Filter {
        let k = block.max(1) as f32;
        match *self {
            Filter::GaussianBlur { radius } => Filter::GaussianBlur { radius: radius / k },
            Filter::MotionBlur { angle, distance } => Filter::MotionBlur {
                angle,
                distance: distance / k,
            },
            Filter::Sharpen { radius, amount } => Filter::Sharpen {
                radius: radius / k,
                amount,
            },
            Filter::Pixelate { size } => Filter::Pixelate {
                size: (size / block.max(1) as u32).max(1),
            },
            Filter::Noise { amount, mono, size } => Filter::Noise {
                amount,
                mono,
                size: (size / k).max(1.0),
            },
            Filter::Glow {
                radius,
                strength,
                threshold,
            } => Filter::Glow {
                radius: radius / k,
                strength,
                threshold,
            },
            Filter::ChromaticAberration { amount, frame } => Filter::ChromaticAberration {
                amount: amount / k,
                frame: frame.shrunk(k),
            },
            Filter::Halftone {
                size,
                angle,
                colour,
            } => Filter::Halftone {
                size: (size / k).max(2.0),
                angle,
                colour,
            },
            Filter::Clouds { scale, detail } => Filter::Clouds {
                scale: scale / k,
                detail,
            },
            Filter::Median { radius } => Filter::Median {
                radius: (radius as f32 / k).round().max(1.0) as u32,
            },
            Filter::OilPaint { radius } => Filter::OilPaint {
                radius: (radius as f32 / k).round().max(1.0) as u32,
            },
            Filter::Vignette {
                amount,
                size,
                frame,
            } => Filter::Vignette {
                amount,
                size,
                frame: frame.shrunk(k),
            },
            Filter::ZoomBlur { amount, frame } => Filter::ZoomBlur {
                amount,
                frame: frame.shrunk(k),
            },
            Filter::SpinBlur { angle, frame } => Filter::SpinBlur {
                angle,
                frame: frame.shrunk(k),
            },
            other => other,
        }
    }

    /// How far (pixels) a result pixel reads from its source position.
    pub fn reach(&self) -> i32 {
        match *self {
            Filter::GaussianBlur { radius } | Filter::Sharpen { radius, .. } => {
                (radius * 3.0).ceil() as i32
            }
            // Half the line each way, and a pixel for each of its three
            // resampling passes.
            Filter::MotionBlur { distance, .. } => (distance * 0.5).ceil() as i32 + 3,
            Filter::Pixelate { size } => size as i32,
            Filter::Glow { radius, .. } => (radius * 3.0).ceil() as i32,
            Filter::ChromaticAberration { amount, .. } => amount.ceil() as i32 + 1,
            Filter::Halftone { size, .. } => size.ceil() as i32 + 1,
            Filter::Emboss { .. } | Filter::FindEdges => 2,
            Filter::Median { radius } | Filter::OilPaint { radius } => radius as i32,
            Filter::ZoomBlur { .. } | Filter::SpinBlur { .. } => MAX_REACH,
            _ => 0,
        }
        .min(MAX_REACH)
    }

    /// Filter the `w`×`h` buffer `src`, whose top-left pixel is at canvas
    /// point `origin` (noise and pixel blocks stay put on the canvas).
    pub fn apply(&self, src: &[Color32], w: usize, h: usize, origin: (i32, i32)) -> Vec<Color32> {
        debug_assert_eq!(src.len(), w * h);
        match *self {
            Filter::GaussianBlur { .. }
            | Filter::MotionBlur { .. }
            | Filter::Sharpen { .. }
            | Filter::Pixelate { .. }
            | Filter::Glow { .. }
            | Filter::ChromaticAberration { .. }
            | Filter::Halftone { .. }
            | Filter::Median { .. }
            | Filter::OilPaint { .. }
            | Filter::ZoomBlur { .. }
            | Filter::SpinBlur { .. } => {
                let mixed = to_mixable(src);
                let out = self.apply_mixable(&mixed, w, h, origin);
                from_mixable(&out, &mixed, src)
            }
            Filter::Noise { amount, mono, size } => src
                .par_iter()
                .enumerate()
                .map(|(i, &c)| {
                    let (x, y) = (origin.0 + (i % w) as i32, origin.1 + (i / w) as i32);
                    noise(c, x, y, amount, mono, size)
                })
                .collect(),
            Filter::Emboss { angle, depth } => effects::emboss(src, w, h, angle, depth),
            Filter::FindEdges => effects::find_edges(src, w, h),
            Filter::Clouds { scale, detail } => effects::clouds(w, h, origin, scale, detail),
            Filter::Vignette {
                amount,
                size,
                frame,
            } => effects::vignette(src, w, h, origin, amount, size, frame),
            Filter::Dither { levels } => effects::dither(src, w, h, origin, levels),
            _ => match self.channel_lut() {
                Some(lut) => src.par_iter().map(|&c| with_lut(c, &lut)).collect(),
                None => src.par_iter().map(|&c| self.pixel(c)).collect(),
            },
        }
    }

    /// The filters that mix neighbouring pixels, on a [`to_mixable`]
    /// buffer (and giving one back).
    fn apply_mixable(
        &self,
        src: &[Color32],
        w: usize,
        h: usize,
        origin: (i32, i32),
    ) -> Vec<Color32> {
        match *self {
            Filter::GaussianBlur { radius } => gaussian_blur(src, w, h, radius),
            Filter::MotionBlur { angle, distance } => motion_blur(src, w, h, angle, distance),
            Filter::Sharpen { radius, amount } => {
                let blurred = gaussian_blur(src, w, h, radius);
                src.par_iter()
                    .zip(blurred.par_iter())
                    .map(|(&s, &b)| unsharp(s, b, amount))
                    .collect()
            }
            Filter::Pixelate { size } => pixelate(src, w, h, origin, size.max(1) as usize),
            Filter::Glow {
                radius,
                strength,
                threshold,
            } => effects::glow(src, w, h, radius, strength, threshold),
            Filter::ChromaticAberration { amount, frame } => {
                effects::chromatic_aberration(src, w, h, origin, amount, frame)
            }
            Filter::Halftone {
                size,
                angle,
                colour,
            } => effects::halftone(src, w, h, origin, size, angle, colour),
            Filter::Median { radius } => effects::median(src, w, h, radius),
            Filter::OilPaint { radius } => effects::oil_paint(src, w, h, radius),
            Filter::ZoomBlur { amount, frame } => {
                effects::zoom_blur(src, w, h, origin, amount, frame)
            }
            Filter::SpinBlur { angle, frame } => {
                effects::spin_blur(src, w, h, origin, angle, frame)
            }
            _ => src.to_vec(),
        }
    }

    /// A per-pixel filter on one pixel (neighbourhood filters return it
    /// unchanged).
    fn pixel(&self, c: Color32) -> Color32 {
        if c.a() == 0 {
            return c;
        }
        let [r, g, b, a] = c.unmultiplied();
        let rgb = [r, g, b].map(|v| v as f32 / 255.0);
        if let Filter::LineArt {
            black,
            white,
            keep_color,
        } = *self
        {
            let span = (white - black).max(1e-3);
            let opacity = 1.0 - ((luma(rgb) - black) / span).clamp(0.0, 1.0);
            let a = (a as f32 * opacity).round() as u8;
            let [r, g, b] = if keep_color { [r, g, b] } else { [0, 0, 0] };
            return Color32::from_rgba_unmultiplied(r, g, b, a);
        }
        let Some(out) = self.rgb(rgb) else {
            return c;
        };
        let [r, g, b] = out.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }

    /// A colour filter on unmultiplied sRGB values (0..1), unclamped; `None`
    /// for filters that aren't one.
    fn rgb(&self, rgb: [f32; 3]) -> Option<[f32; 3]> {
        Some(match *self {
            Filter::BrightnessContrast {
                brightness,
                contrast,
            } => {
                // Contrast stretches around mid-grey; +1 is (almost) a threshold.
                let k = if contrast >= 0.0 {
                    1.0 / (1.0 - contrast.min(0.99))
                } else {
                    1.0 + contrast
                };
                rgb.map(|v| (v + brightness * 0.5 - 0.5) * k + 0.5)
            }
            Filter::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                let [h, s, l] = rgb_to_hsl(rgb);
                let s = (s * (1.0 + saturation)).clamp(0.0, 1.0);
                let l = if lightness >= 0.0 {
                    l + (1.0 - l) * lightness
                } else {
                    l * (1.0 + lightness)
                };
                hsl_to_rgb([(h + hue / 360.0).rem_euclid(1.0), s, l])
            }
            Filter::Levels {
                black,
                white,
                gamma,
            } => {
                let span = (white - black).max(1e-3);
                rgb.map(|v| {
                    ((v - black) / span)
                        .clamp(0.0, 1.0)
                        .powf(1.0 / gamma.max(0.01))
                })
            }
            Filter::Invert => rgb.map(|v| 1.0 - v),
            Filter::Desaturate => [luma(rgb); 3],
            Filter::Posterize { levels } => {
                let n = (levels.max(2) - 1) as f32;
                rgb.map(|v| (v * n).round() / n)
            }
            Filter::Threshold { level } => [if luma(rgb) >= level { 1.0 } else { 0.0 }; 3],
            Filter::Exposure { stops } => {
                let k = 2f32.powf(stops);
                rgb.map(|v| linear_to_srgb(srgb_to_linear(v) * k))
            }
            Filter::Temperature { temperature, tint } => {
                // Gains in linear light, like a white balance.
                let t = temperature * 0.35;
                let g = tint * 0.25;
                let gains = [1.0 + t, 1.0 + g, 1.0 - t];
                [0, 1, 2].map(|i| linear_to_srgb(srgb_to_linear(rgb[i]) * gains[i]))
            }
            Filter::Vibrance { amount } => {
                let [h, s, l] = rgb_to_hsl(rgb);
                // Dull colours gain the most, rich ones barely move, greys
                // (whose hue is noise) stay grey.
                let s = if amount >= 0.0 {
                    s * (1.0 + amount * 2.0 * (1.0 - s))
                } else {
                    s * (1.0 + amount * (1.0 - s * 0.5))
                };
                hsl_to_rgb([h, s.clamp(0.0, 1.0), l])
            }
            Filter::Sepia { amount } => {
                let [r, g, b] = rgb;
                let tone = [
                    0.393 * r + 0.769 * g + 0.189 * b,
                    0.349 * r + 0.686 * g + 0.168 * b,
                    0.272 * r + 0.534 * g + 0.131 * b,
                ];
                [0, 1, 2].map(|i| rgb[i] + (tone[i] - rgb[i]) * amount.clamp(0.0, 1.0))
            }
            Filter::Solarize { level } => rgb.map(|v| if v > level { 1.0 - v } else { v }),
            Filter::Curves {
                rgb: master,
                red,
                green,
                blue,
            } => {
                let (master, curves) = (master.to_softness(), [red, green, blue]);
                [0, 1, 2].map(|i| {
                    let v = master.eval(rgb[i].clamp(0.0, 1.0));
                    curves[i].to_softness().eval(v.clamp(0.0, 1.0))
                })
            }
            Filter::ColourBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => colour_balance(rgb, shadows, midtones, highlights, preserve_luminosity),
            Filter::GradientMap(map) => map.color_at(luma(rgb).clamp(0.0, 1.0)),
            _ => return None,
        })
    }

    /// A colour adjustment on one premultiplied linear-light pixel at full
    /// precision (no 8-bit table), for deeper documents; `None` for filters
    /// that aren't one pixel at a time (blurs and the like). The alpha is
    /// kept.
    pub fn adjust_linear(&self, px: [f32; 4]) -> Option<[f32; 4]> {
        use eframe::egui::ecolor::{gamma_from_linear, linear_from_gamma};
        let a = px[3];
        if a <= 0.0 {
            // Nothing to change, if the filter works one pixel at a time.
            return self.rgb([0.5; 3]).map(|_| px);
        }
        let rgb = [0, 1, 2].map(|c| gamma_from_linear((px[c] / a).clamp(0.0, 1.0)));
        let out = self.rgb(rgb)?;
        Some([
            linear_from_gamma(out[0].clamp(0.0, 1.0)) * a,
            linear_from_gamma(out[1].clamp(0.0, 1.0)) * a,
            linear_from_gamma(out[2].clamp(0.0, 1.0)) * a,
            a,
        ])
    }

    /// An adjustment on unmultiplied sRGB values (0..1), through `lut` when
    /// it has one: for compositing in gamma space, where the values are at
    /// hand without going through 8-bit colour.
    pub fn adjust_rgb(&self, rgb: [f32; 3], lut: Option<&ChannelLut>) -> [f32; 3] {
        let out = match lut {
            Some(lut) => [0, 1, 2]
                .map(|i| lut[i][(rgb[i].clamp(0.0, 1.0) * 255.0).round() as usize] as f32 / 255.0),
            None => self.rgb(rgb).unwrap_or(rgb),
        };
        out.map(|v| v.clamp(0.0, 1.0))
    }
}

/// `c` with each unmultiplied channel mapped through `lut`.
#[inline]
fn with_lut(c: Color32, lut: &ChannelLut) -> Color32 {
    if c.a() == 0 {
        return c;
    }
    let [r, g, b, a] = c.unmultiplied();
    Color32::from_rgba_unmultiplied(
        lut[0][r as usize],
        lut[1][g as usize],
        lut[2][b as usize],
        a,
    )
}

/// GIMP's colour balance: each range's shift weighted by how far the
/// pixel's lightness is into shadows, midtones or highlights (the three
/// weights add up to one, so the same shift everywhere shifts everything).
fn colour_balance(
    rgb: [f32; 3],
    shadows: [f32; 3],
    midtones: [f32; 3],
    highlights: [f32; 3],
    preserve_luminosity: bool,
) -> [f32; 3] {
    const A: f32 = 0.25;
    const B: f32 = 0.333;
    const SCALE: f32 = 0.7;
    let lightness = rgb_to_hsl(rgb)[2];
    let ws = ((lightness - B) / -A + 0.5).clamp(0.0, 1.0) * SCALE;
    let wm = ((lightness - B) / A + 0.5).clamp(0.0, 1.0)
        * ((lightness + B - 1.0) / -A + 0.5).clamp(0.0, 1.0)
        * SCALE;
    let wh = ((lightness + B - 1.0) / A + 0.5).clamp(0.0, 1.0) * SCALE;
    let out = [0, 1, 2].map(|i| {
        (rgb[i] + shadows[i] * ws + midtones[i] * wm + highlights[i] * wh).clamp(0.0, 1.0)
    });
    if preserve_luminosity {
        let [h, s, _] = rgb_to_hsl(out);
        hsl_to_rgb([h, s, lightness])
    } else {
        out
    }
}

/// Rec. 601 luma of sRGB values, as Photoshop's desaturate uses.
fn luma([r, g, b]: [f32; 3]) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

fn rgb_to_hsl([r, g, b]: [f32; 3]) -> [f32; 3] {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, s.min(1.0), l]
}

fn hsl_to_rgb([h, s, l]: [f32; 3]) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h * 6.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let [r, g, b] = match hp as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    let m = l - c * 0.5;
    [r + m, g + m, b + m]
}

/// `sharp + amount · (sharp − blurred)`, premultiplied, keeping the alpha.
fn unsharp(s: Color32, b: Color32, amount: f32) -> Color32 {
    let a = s.a();
    if a == 0 {
        return s;
    }
    // Sharpened colour can't be brighter than the pixel is opaque.
    let ch = |sv: u8, bv: u8| {
        let v = sv as f32 + amount * (sv as f32 - bv as f32);
        v.round().clamp(0.0, a as f32) as u8
    };
    Color32::from_rgba_premultiplied(ch(s.r(), b.r()), ch(s.g(), b.g()), ch(s.b(), b.b()), a)
}

/// A repeatable value in -1..1 for a canvas pixel and channel.
fn hash_noise(x: i32, y: i32, channel: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ channel.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    (h >> 8) as f32 / (1u32 << 23) as f32 - 1.0
}

/// [`hash_noise`] in grains `size` pixels across: smooth value noise
/// between lattice points, scaled back up to the spread a single pixel's
/// has (interpolating averages some of it away).
fn grain(x: i32, y: i32, channel: u32, size: f32) -> f32 {
    if size <= 1.0 {
        return hash_noise(x, y, channel);
    }
    let (u, v) = ((x as f32 + 0.5) / size, (y as f32 + 0.5) / size);
    let (x0, y0) = (u.floor() as i32, v.floor() as i32);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(u - x0 as f32), s(v - y0 as f32));
    let n = |dx, dy| hash_noise(x0 + dx, y0 + dy, channel);
    let top = n(0, 0) + (n(1, 0) - n(0, 0)) * sx;
    let bottom = n(0, 1) + (n(1, 1) - n(0, 1)) * sx;
    ((top + (bottom - top) * sy) * 1.35).clamp(-1.0, 1.0)
}

fn noise(c: Color32, x: i32, y: i32, amount: f32, mono: bool, size: f32) -> Color32 {
    let a = c.a();
    if a == 0 {
        return c;
    }
    let spread = amount * a as f32;
    let ch = |v: u8, channel: u32| {
        let n = grain(x, y, if mono { 0 } else { channel }, size);
        (v as f32 + n * spread).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_premultiplied(ch(c.r(), 0), ch(c.g(), 1), ch(c.b(), 2), a)
}

/// Three box blurs approximate a Gaussian of standard deviation `sigma`
/// (the box widths from Kovesi, "Fast almost-Gaussian filtering").
fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n)
        / (-4.0 * wl as f32 - 4.0))
        .round() as i32;
    std::array::from_fn(|i| {
        let w = if (i as i32) < m { wl } else { wu };
        ((w - 1) / 2).max(0) as usize
    })
}

pub(crate) fn gaussian_blur(src: &[Color32], w: usize, h: usize, sigma: f32) -> Vec<Color32> {
    if sigma < 0.3 || w == 0 || h == 0 {
        return src.to_vec();
    }
    let mut buf = src.to_vec();
    for r in box_radii(sigma) {
        buf = box_blur_rows(&buf, w, h, r);
        buf = transpose(&buf, w, h);
        buf = box_blur_rows(&buf, h, w, r);
        buf = transpose(&buf, h, w);
    }
    buf
}

/// Each row averaged over `2r + 1` pixels (edge pixels repeat).
fn box_blur_rows(src: &[Color32], w: usize, h: usize, r: usize) -> Vec<Color32> {
    if r == 0 {
        return src.to_vec();
    }
    let mut out = vec![Color32::TRANSPARENT; w * h];
    let n = (2 * r + 1) as u32;
    out.par_chunks_mut(w)
        .zip(src.par_chunks(w))
        .for_each(|(out, row)| {
            let at = |i: isize| row[i.clamp(0, w as isize - 1) as usize].to_array();
            let mut sum = [0u32; 4];
            for i in -(r as isize)..=r as isize {
                for (s, v) in sum.iter_mut().zip(at(i)) {
                    *s += v as u32;
                }
            }
            for (x, o) in out.iter_mut().enumerate() {
                let [r0, g0, b0, a0] = sum.map(|s| ((s + n / 2) / n) as u8);
                *o = Color32::from_rgba_premultiplied(r0, g0, b0, a0);
                let (add, sub) = (at(x as isize + r as isize + 1), at(x as isize - r as isize));
                for ((s, a), b) in sum.iter_mut().zip(add).zip(sub) {
                    *s = *s + a as u32 - b as u32;
                }
            }
        });
    out
}

fn transpose(src: &[Color32], w: usize, h: usize) -> Vec<Color32> {
    let mut out = vec![Color32::TRANSPARENT; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, o) in col.iter_mut().enumerate() {
            *o = src[y * w + x];
        }
    });
    out
}

/// The average along a line `distance` long through each pixel, at up to
/// 64 bilinear samples (a long blur costs no more than a 64 px one).
fn motion_blur(src: &[Color32], w: usize, h: usize, angle: f32, distance: f32) -> Vec<Color32> {
    if distance < 1.0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    // Along a line centred on each pixel: steps along a line add, so the
    // three-pass average is the 64-point one.
    let (dy, dx) = angle.to_radians().sin_cos();
    let (ux, uy) = (dx * distance, -dy * distance);
    super::effects::path_blur(src, w, h, true, |x, y, t, dt| {
        Some(std::array::from_fn(|d| {
            let s = t + d as f32 * dt;
            (x + ux * s, y + uy * s)
        }))
    })
}

/// Each `size`² block of the canvas grid set to its average.
fn pixelate(src: &[Color32], w: usize, h: usize, origin: (i32, i32), size: usize) -> Vec<Color32> {
    let s = size as i32;
    // Block edges on the canvas grid, as buffer columns / rows.
    let edges = |start: i32, len: usize| -> Vec<usize> {
        let mut e = vec![0];
        let first = (start.div_euclid(s) + 1) * s - start;
        let mut at = first;
        while (at as usize) < len {
            e.push(at as usize);
            at += s;
        }
        e.push(len);
        e
    };
    let (xs, ys) = (edges(origin.0, w), edges(origin.1, h));
    let mut out = src.to_vec();
    let rows: Vec<(usize, usize)> = ys.windows(2).map(|p| (p[0], p[1])).collect();
    let bands: Vec<Vec<Color32>> = rows
        .par_iter()
        .map(|&(y0, y1)| {
            let mut band = src[y0 * w..y1 * w].to_vec();
            for p in xs.windows(2) {
                let (x0, x1) = (p[0], p[1]);
                let mut sum = [0u32; 4];
                for y in 0..y1 - y0 {
                    for px in &band[y * w + x0..y * w + x1] {
                        for (s, v) in sum.iter_mut().zip(px.to_array()) {
                            *s += v as u32;
                        }
                    }
                }
                let n = ((x1 - x0) * (y1 - y0)) as u32;
                let [r, g, b, a] = sum.map(|v| ((v + n / 2) / n) as u8);
                let avg = Color32::from_rgba_premultiplied(r, g, b, a);
                for y in 0..y1 - y0 {
                    band[y * w + x0..y * w + x1].fill(avg);
                }
            }
            band
        })
        .collect();
    for (&(y0, _), band) in rows.iter().zip(bands) {
        out[y0 * w..y0 * w + band.len()].copy_from_slice(&band);
    }
    out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod timing;
