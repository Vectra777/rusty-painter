//! Filters (the Filter menu): colour adjustments, blurs and effects over a
//! row-major buffer of premultiplied sRGB pixels. Pure functions: the
//! Filter tool reads the layer into a buffer, runs [`Filter::apply`] and
//! writes the result back with undo.
//!
//! Colour adjustments work on unpremultiplied sRGB values, as Photoshop's
//! do; blurs average premultiplied pixels, so transparent ones don't darken
//! the edges.

use eframe::egui::Color32;
use rayon::prelude::*;

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
    /// Amount 0..=1; mono: the same noise in every channel.
    Noise {
        amount: f32,
        mono: bool,
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
}

/// Largest radius or distance the sliders allow, so the area read around a
/// selection is known before the filter runs.
pub const MAX_REACH: i32 = 200;

impl Filter {
    /// The Filter menu: groups of filters with their default settings.
    pub const MENU: [&'static [Filter]; 4] = [
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
        ],
        &[
            Filter::Noise {
                amount: 0.1,
                mono: true,
            },
            Filter::Pixelate { size: 8 },
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
        }
    }

    /// Filters that can be adjustment layers: each pixel changes on its own
    /// (no neighbours, no position), and its transparency stays.
    pub const ADJUSTMENTS: [Filter; 7] = [
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
    ];

    /// This filter on one composited pixel (adjustment layers), through
    /// `lut` when it has one (see [`Filter::channel_lut`]).
    pub fn adjust(&self, c: Color32, lut: Option<&[u8; 256]>) -> Color32 {
        match lut {
            Some(lut) => with_lut(c, lut),
            None => self.pixel(c),
        }
    }

    /// For filters that change each channel on its own (levels,
    /// brightness/contrast, invert, posterize): what each 0..=255 value
    /// becomes, so a pixel costs three lookups instead of the maths.
    pub fn channel_lut(&self) -> Option<Box<[u8; 256]>> {
        if !matches!(
            self,
            Filter::BrightnessContrast { .. }
                | Filter::Levels { .. }
                | Filter::Invert
                | Filter::Posterize { .. }
        ) {
            return None;
        }
        let mut lut = Box::new([0u8; 256]);
        for (v, out) in lut.iter_mut().enumerate() {
            let v = v as u8;
            *out = self.pixel(Color32::from_rgb(v, v, v)).r();
        }
        Some(lut)
    }

    /// Has settings (the menu opens a dialog), rather than applying at once.
    pub fn has_settings(&self) -> bool {
        !matches!(self, Filter::Invert | Filter::Desaturate)
    }

    /// How far (pixels) a result pixel reads from its source position.
    pub fn reach(&self) -> i32 {
        match *self {
            Filter::GaussianBlur { radius } | Filter::Sharpen { radius, .. } => {
                (radius * 3.0).ceil() as i32
            }
            Filter::MotionBlur { distance, .. } => (distance * 0.5).ceil() as i32 + 1,
            Filter::Pixelate { size } => size as i32,
            _ => 0,
        }
        .min(MAX_REACH)
    }

    /// Filter the `w`×`h` buffer `src`, whose top-left pixel is at canvas
    /// point `origin` (noise and pixel blocks stay put on the canvas).
    pub fn apply(&self, src: &[Color32], w: usize, h: usize, origin: (i32, i32)) -> Vec<Color32> {
        debug_assert_eq!(src.len(), w * h);
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
            Filter::Noise { amount, mono } => src
                .par_iter()
                .enumerate()
                .map(|(i, &c)| {
                    let (x, y) = (origin.0 + (i % w) as i32, origin.1 + (i / w) as i32);
                    noise(c, x, y, amount, mono)
                })
                .collect(),
            Filter::Pixelate { size } => pixelate(src, w, h, origin, size.max(1) as usize),
            _ => match self.channel_lut() {
                Some(lut) => src.par_iter().map(|&c| with_lut(c, &lut)).collect(),
                None => src.par_iter().map(|&c| self.pixel(c)).collect(),
            },
        }
    }

    /// A per-pixel filter on one pixel (neighbourhood filters return it
    /// unchanged).
    fn pixel(&self, c: Color32) -> Color32 {
        if c.a() == 0 {
            return c;
        }
        let [r, g, b, a] = c.to_srgba_unmultiplied();
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
            _ => return None,
        })
    }

    /// An adjustment on unmultiplied sRGB values (0..1), through `lut` when
    /// it has one: for compositing in gamma space, where the values are at
    /// hand without going through 8-bit colour.
    pub fn adjust_rgb(&self, rgb: [f32; 3], lut: Option<&[u8; 256]>) -> [f32; 3] {
        let out = match lut {
            Some(lut) => {
                rgb.map(|v| lut[(v.clamp(0.0, 1.0) * 255.0).round() as usize] as f32 / 255.0)
            }
            None => self.rgb(rgb).unwrap_or(rgb),
        };
        out.map(|v| v.clamp(0.0, 1.0))
    }
}

/// `c` with each unmultiplied channel mapped through `lut`.
#[inline]
fn with_lut(c: Color32, lut: &[u8; 256]) -> Color32 {
    if c.a() == 0 {
        return c;
    }
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(lut[r as usize], lut[g as usize], lut[b as usize], a)
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
    let ch = |sv: u8, bv: u8| {
        let v = sv as f32 + amount * (sv as f32 - bv as f32);
        v.round().clamp(0.0, 255.0) as u8
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

fn noise(c: Color32, x: i32, y: i32, amount: f32, mono: bool) -> Color32 {
    let a = c.a();
    if a == 0 {
        return c;
    }
    let spread = amount * a as f32;
    let ch = |v: u8, channel: u32| {
        let n = hash_noise(x, y, if mono { 0 } else { channel });
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

fn gaussian_blur(src: &[Color32], w: usize, h: usize, sigma: f32) -> Vec<Color32> {
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
    let samples = (distance.ceil() as usize + 1).min(64);
    let (dy, dx) = angle.to_radians().sin_cos();
    let offsets: Vec<(f32, f32)> = (0..samples)
        .map(|i| {
            let t = (i as f32 / (samples - 1) as f32 - 0.5) * distance;
            (dx * t, -dy * t)
        })
        .collect();
    let sample = |x: f32, y: f32| -> [f32; 4] {
        let (x, y) = (x.clamp(0.0, (w - 1) as f32), y.clamp(0.0, (h - 1) as f32));
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let px = |x: usize, y: usize| src[y * w + x].to_array().map(|v| v as f32);
        let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
        std::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            top + (bottom - top) * fy
        })
    };
    let mut out = vec![Color32::TRANSPARENT; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; 4];
            for &(ox, oy) in &offsets {
                for (s, v) in sum.iter_mut().zip(sample(x as f32 + ox, y as f32 + oy)) {
                    *s += v;
                }
            }
            let [r, g, b, a] = sum.map(|s| (s / samples as f32).round() as u8);
            *o = Color32::from_rgba_premultiplied(r, g, b, a);
        }
    });
    out
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
mod tests {
    use super::*;

    fn solid(c: Color32, n: usize) -> Vec<Color32> {
        vec![c; n]
    }

    #[test]
    fn neutral_settings_change_nothing() {
        let px = [
            Color32::from_rgb(200, 40, 90),
            Color32::from_rgba_unmultiplied(10, 250, 128, 77),
            Color32::TRANSPARENT,
        ];
        for group in Filter::MENU {
            for f in group.iter() {
                let neutral = match f {
                    Filter::BrightnessContrast { .. }
                    | Filter::HueSaturation { .. }
                    | Filter::Levels { .. } => *f,
                    Filter::GaussianBlur { .. } => Filter::GaussianBlur { radius: 0.0 },
                    Filter::Noise { mono, .. } => Filter::Noise {
                        amount: 0.0,
                        mono: *mono,
                    },
                    Filter::Pixelate { .. } => Filter::Pixelate { size: 1 },
                    _ => continue,
                };
                let out = neutral.apply(&px, 3, 1, (0, 0));
                for (a, b) in px.iter().zip(&out) {
                    let d = a
                        .to_array()
                        .iter()
                        .zip(b.to_array())
                        .map(|(x, y)| x.abs_diff(y))
                        .max()
                        .unwrap();
                    assert!(d <= 2, "{}: {a:?} -> {b:?}", f.name());
                }
            }
        }
    }

    #[test]
    fn invert_and_desaturate_do_what_they_say() {
        let out = Filter::Invert.apply(&[Color32::from_rgb(255, 0, 100)], 1, 1, (0, 0));
        assert_eq!(out[0], Color32::from_rgb(0, 255, 155));
        let out = Filter::Desaturate.apply(&[Color32::from_rgb(255, 0, 0)], 1, 1, (0, 0));
        let [r, g, b, _] = out[0].to_array();
        assert!(r == g && g == b && (74..=78).contains(&r), "{:?}", out[0]);
    }

    #[test]
    fn hue_turns_red_to_green_and_back() {
        let red = Color32::from_rgb(255, 0, 0);
        let f = |hue| Filter::HueSaturation {
            hue,
            saturation: 0.0,
            lightness: 0.0,
        };
        assert_eq!(
            f(120.0).apply(&[red], 1, 1, (0, 0))[0],
            Color32::from_rgb(0, 255, 0)
        );
        assert_eq!(
            f(-120.0).apply(&[red], 1, 1, (0, 0))[0],
            Color32::from_rgb(0, 0, 255)
        );
    }

    #[test]
    fn blur_spreads_a_dot_and_keeps_the_total() {
        let (w, h) = (41, 41);
        let mut src = solid(Color32::TRANSPARENT, w * h);
        src[20 * w + 20] = Color32::WHITE;
        let out = Filter::GaussianBlur { radius: 2.0 }.apply(&src, w, h, (0, 0));
        assert!(out[20 * w + 20].a() < 255, "the centre fades");
        assert!(out[20 * w + 22].a() > 0, "its neighbours gain");
        assert_eq!(out[20 * w + 38].a(), 0, "far pixels stay empty");
        // Box blurs round each pass; the total alpha stays about the same.
        let total: u32 = out.iter().map(|c| c.a() as u32).sum();
        assert!((200..=300).contains(&total), "{total}");
    }

    #[test]
    fn blur_leaves_a_flat_colour_flat() {
        let c = Color32::from_rgb(30, 120, 200);
        let out = Filter::GaussianBlur { radius: 5.0 }.apply(&solid(c, 30 * 20), 30, 20, (0, 0));
        assert!(out.iter().all(|&p| p == c));
    }

    #[test]
    fn motion_blur_smears_along_its_angle_only() {
        let (w, h) = (41, 41);
        let mut src = solid(Color32::TRANSPARENT, w * h);
        src[20 * w + 20] = Color32::WHITE;
        let out = Filter::MotionBlur {
            angle: 0.0,
            distance: 10.0,
        }
        .apply(&src, w, h, (0, 0));
        assert!(out[20 * w + 24].a() > 0, "along the line");
        assert_eq!(out[24 * w + 20].a(), 0, "not across it");
    }

    #[test]
    fn sharpen_raises_contrast_at_an_edge() {
        let (w, h) = (20, 1);
        let src: Vec<Color32> = (0..w)
            .map(|x| {
                if x < 10 {
                    Color32::from_gray(100)
                } else {
                    Color32::from_gray(150)
                }
            })
            .collect();
        let out = Filter::Sharpen {
            radius: 1.5,
            amount: 1.0,
        }
        .apply(&src, w, h, (0, 0));
        assert!(
            out[9].r() < 100 && out[10].r() > 150,
            "{:?} {:?}",
            out[9],
            out[10]
        );
        assert_eq!(out[0], src[0], "flat areas are unchanged");
    }

    #[test]
    fn noise_is_repeatable_and_stays_in_range() {
        let src = solid(Color32::from_rgba_unmultiplied(128, 128, 128, 128), 64);
        let f = Filter::Noise {
            amount: 0.5,
            mono: false,
        };
        let a = f.apply(&src, 8, 8, (3, 5));
        assert_eq!(a, f.apply(&src, 8, 8, (3, 5)));
        assert!(a.iter().all(|c| c.a() == 128));
        assert!(a.iter().any(|&c| c != src[0]));
    }

    #[test]
    fn pixelate_blocks_follow_the_canvas_grid() {
        let (w, h) = (8, 4);
        let src: Vec<Color32> = (0..w * h)
            .map(|i| Color32::from_gray((i % w) as u8 * 30))
            .collect();
        // The buffer starts at canvas x = 2: blocks of 4 end at buffer x = 2, 6.
        let out = Filter::Pixelate { size: 4 }.apply(&src, w, h, (2, 0));
        assert_eq!(out[0], out[1]);
        assert_ne!(out[1], out[2]);
        assert_eq!(out[2], out[5]);
        assert_ne!(out[5], out[6]);
    }

    #[test]
    fn line_art_keeps_the_dark_lines_and_drops_the_paper() {
        let src = [
            Color32::from_gray(20),
            Color32::from_gray(245),
            Color32::from_gray(140),
        ];
        let out = Filter::LineArt {
            black: 0.25,
            white: 0.85,
            keep_color: false,
        }
        .apply(&src, 3, 1, (0, 0));
        assert_eq!(out[0], Color32::BLACK, "ink stays, black");
        assert_eq!(out[1].a(), 0, "paper goes");
        assert!(
            (50..200).contains(&out[2].a()),
            "grey is half-opaque: {:?}",
            out[2]
        );
    }

    #[test]
    fn a_channel_table_gives_the_same_pixels_as_the_maths() {
        let px: Vec<Color32> = (0..4096u32)
            .map(|i| {
                Color32::from_rgba_unmultiplied(
                    (i * 7) as u8,
                    (i * 13) as u8,
                    (i * 29) as u8,
                    (i % 256) as u8,
                )
            })
            .collect();
        for f in [
            Filter::BrightnessContrast {
                brightness: 0.2,
                contrast: 0.4,
            },
            Filter::Levels {
                black: 0.1,
                white: 0.8,
                gamma: 1.7,
            },
            Filter::Invert,
            Filter::Posterize { levels: 5 },
        ] {
            let lut = f.channel_lut().expect("per-channel");
            for &c in &px {
                assert_eq!(f.adjust(c, Some(&lut)), f.pixel(c), "{}: {c:?}", f.name());
            }
        }
        assert!(Filter::Desaturate.channel_lut().is_none(), "mixes channels");
        // On float values (gamma compositing) they agree with the 8-bit path.
        for f in [
            Filter::Levels {
                black: 0.1,
                white: 0.8,
                gamma: 1.7,
            },
            Filter::HueSaturation {
                hue: 50.0,
                saturation: 0.3,
                lightness: -0.1,
            },
            Filter::Threshold { level: 0.4 },
        ] {
            let lut = f.channel_lut();
            for &c in &px[..512] {
                if c.a() == 0 {
                    continue;
                }
                let [r, g, b, _] = c.to_srgba_unmultiplied();
                let rgb = [r, g, b].map(|v| v as f32 / 255.0);
                let fast = f
                    .adjust_rgb(rgb, lut.as_deref())
                    .map(|v| (v * 255.0).round() as u8);
                let [er, eg, eb, _] = f.pixel(Color32::from_rgb(r, g, b)).to_array();
                for (x, y) in fast.iter().zip([er, eg, eb]) {
                    assert!(
                        x.abs_diff(y) <= 1,
                        "{}: {fast:?} vs {:?}",
                        f.name(),
                        [er, eg, eb]
                    );
                }
            }
        }
    }

    #[test]
    fn box_radii_grow_with_sigma() {
        let small: usize = box_radii(1.0).iter().sum();
        let large: usize = box_radii(10.0).iter().sum();
        assert!(small < large);
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    #[test]
    #[ignore = "timing; run with --release --ignored --nocapture"]
    fn filters_4k() {
        let (w, h) = (4096, 4096);
        let src: Vec<Color32> = (0..w * h)
            .map(|i| Color32::from_gray((i % 251) as u8))
            .collect();
        for f in [
            Filter::HueSaturation {
                hue: 30.0,
                saturation: 0.2,
                lightness: 0.0,
            },
            Filter::GaussianBlur { radius: 10.0 },
            Filter::GaussianBlur { radius: 60.0 },
            Filter::MotionBlur {
                angle: 30.0,
                distance: 200.0,
            },
            Filter::Sharpen {
                radius: 2.0,
                amount: 1.0,
            },
            Filter::Pixelate { size: 16 },
        ] {
            let t = std::time::Instant::now();
            std::hint::black_box(f.apply(&src, w, h, (0, 0)));
            println!("{:>22}: {:?}", f.name(), t.elapsed());
        }
    }
}
