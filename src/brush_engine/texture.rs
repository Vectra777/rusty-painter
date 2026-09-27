//! Brush texture: a grey pattern (paper grain, canvas weave, charcoal) that
//! repeats seamlessly across the canvas and takes paint away from each dab
//! where it's low, the way a textured paper catches a dry brush.
//!
//! The pattern is pinned to the canvas (like real paper), so overlapping
//! strokes share their grain. How it combines with a dab's alpha:
//!
//! - **Multiply**: the grain darkens the stroke evenly.
//! - **Subtract**: low spots lose paint first; heavy strokes fill in.
//! - **Height**: only the paper's peaks catch paint under light coverage
//!   (low pressure, soft edges); pressing harder fills the valleys.

use std::sync::{Arc, OnceLock};

/// A repeating grey pattern, 0 (valley) ..= 1 (peak).
#[derive(Debug, PartialEq)]
pub struct Pattern {
    pub name: String,
    /// Power-of-two sides, so wrapping is a mask.
    pub size: usize,
    pub data: Vec<f32>,
}

impl Pattern {
    /// From a greyscale picture, resampled to a power-of-two square (the
    /// nearest size up to 1024) and stretched to use the full range.
    pub fn from_image(name: &str, img: &image::DynamicImage) -> Arc<Self> {
        let grey = img.to_luma8();
        let side = grey.width().max(grey.height()).clamp(16, 1024);
        let size = side.next_power_of_two() as usize;
        let resized = image::imageops::resize(
            &grey,
            size as u32,
            size as u32,
            image::imageops::FilterType::Triangle,
        );
        let data = resized.pixels().map(|p| p[0] as f32 / 255.0).collect();
        Arc::new(Self {
            name: name.to_string(),
            size,
            data: normalized(data),
        })
    }

    /// Height at canvas point `(x, y)` with the pattern scaled by `scale`
    /// (bilinear, wrapping).
    #[inline]
    pub fn at(&self, x: f32, y: f32, inv_scale: f32) -> f32 {
        let mask = self.size - 1;
        // Canvas points are never negative, so u, v > -1: truncating u + 1
        // floors it (no libm call per pixel), then the lattice wraps.
        let (u, v) = (x * inv_scale + 0.5, y * inv_scale + 0.5);
        let (iu, iv) = (u as usize, v as usize);
        let (du, dv) = (u - iu as f32, v - iv as f32);
        let (x0, y0) = ((iu + mask) & mask, (iv + mask) & mask);
        let (x1, y1) = (iu & mask, iv & mask);
        let at = |x: usize, y: usize| self.data[y * self.size + x];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * du;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * du;
        top + (bottom - top) * dv
    }
}

/// How the texture combines with a dab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureMode {
    Multiply,
    Subtract,
    Height,
}

impl TextureMode {
    pub const ALL: [TextureMode; 3] = [Self::Multiply, Self::Subtract, Self::Height];

    pub fn label(self) -> &'static str {
        match self {
            Self::Multiply => "Multiply",
            Self::Subtract => "Subtract",
            Self::Height => "Height",
        }
    }
}

/// A brush's texture settings.
#[derive(Clone, Debug, PartialEq)]
pub struct BrushTexture {
    pub pattern: Arc<Pattern>,
    pub mode: TextureMode,
    /// Pattern size factor (1 = its own pixels).
    pub scale: f32,
    /// 0 = no effect, 1 = full.
    pub strength: f32,
    /// Swap peaks and valleys.
    pub invert: bool,
}

impl BrushTexture {
    pub fn new(pattern: Arc<Pattern>) -> Self {
        Self {
            pattern,
            mode: TextureMode::Subtract,
            scale: 1.0,
            strength: 0.8,
            invert: false,
        }
    }

    /// Apply to a row of dab alphas at canvas row `y`, columns
    /// `x0..x0 + alphas.len()`.
    #[inline]
    pub fn apply_row(&self, y: usize, x0: usize, alphas: &mut [f32]) {
        let p = &*self.pattern;
        let inv = 1.0 / self.scale.max(0.05);
        let s = self.strength.clamp(0.0, 1.0);
        let mask = p.size - 1;
        // What depends on the row, once: the two pattern rows and their
        // blend (canvas points are never negative, see `Pattern::at`).
        let v = y as f32 * inv + 0.5 * inv + 0.5;
        let iv = v as usize;
        let dv = v - iv as f32;
        let (r0, r1) = (((iv + mask) & mask) * p.size, (iv & mask) * p.size);
        let mut u = x0 as f32 * inv + 0.5 * inv + 0.5;
        for a in alphas.iter_mut() {
            let here = u;
            u += inv;
            if *a <= 0.0 {
                continue;
            }
            let iu = here as usize;
            let du = here - iu as f32;
            let (c0, c1) = ((iu + mask) & mask, iu & mask);
            let top = p.data[r0 + c0] + (p.data[r0 + c1] - p.data[r0 + c0]) * du;
            let bottom = p.data[r1 + c0] + (p.data[r1 + c1] - p.data[r1 + c0]) * du;
            let mut t = top + (bottom - top) * dv;
            if self.invert {
                t = 1.0 - t;
            }
            *a = match self.mode {
                TextureMode::Multiply => *a * (1.0 - s + s * t),
                TextureMode::Subtract => (*a - s * (1.0 - t)).max(0.0),
                TextureMode::Height => {
                    // Paint reaches down to 1 - a: a light dab only the
                    // peaks, a full one everything.
                    let floor = 1.0 - *a;
                    let h = ((t - floor) * 4.0 + 0.5).clamp(0.0, 1.0);
                    *a * (1.0 - s + s * h)
                }
            };
        }
    }
}

/// The built-in patterns (generated once).
pub fn builtin() -> &'static [Arc<Pattern>] {
    static PATTERNS: OnceLock<Vec<Arc<Pattern>>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let make = |name: &str, f: &dyn Fn(f32, f32) -> f32| {
            let size = 256;
            let data = (0..size * size)
                .map(|i| f((i % size) as f32, (i / size) as f32))
                .collect();
            Arc::new(Pattern {
                name: name.to_string(),
                size,
                data: normalized(data),
            })
        };
        vec![
            make("Paper", &|x, y| fbm(x, y, 32.0, 4, 1)),
            make("Rough paper", &|x, y| {
                let n = fbm(x, y, 64.0, 5, 2);
                n * n
            }),
            make("Canvas", &|x, y| {
                let t = std::f32::consts::TAU / 8.0;
                let weave = ((x * t).sin() * (y * t).cos()).abs();
                0.7 * weave + 0.3 * fbm(x, y, 16.0, 2, 3)
            }),
            make("Fine grain", &|x, y| fbm(x, y, 4.0, 2, 4)),
            make("Charcoal", &|x, y| {
                // Streaks: cells long along x, short across.
                fbm2(x, y, (128.0, 8.0), 4, 5).powf(1.5)
            }),
        ]
    })
}

/// Stretch values to 0..=1.
fn normalized(mut data: Vec<f32>) -> Vec<f32> {
    let (lo, hi) = data
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let span = (hi - lo).max(1e-6);
    for v in &mut data {
        *v = (*v - lo) / span;
    }
    data
}

/// Tileable fractal noise over a 256 px square: `octaves` of value noise,
/// the first with cells of `cell` pixels (a power of two, so it tiles).
fn fbm(x: f32, y: f32, cell: f32, octaves: u32, seed: u32) -> f32 {
    fbm2(x, y, (cell, cell), octaves, seed)
}

/// [`fbm`] with cells of different width and height (streaks).
fn fbm2(x: f32, y: f32, cell: (f32, f32), octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut cell) = (0.0, 1.0, 0.0, cell);
    for o in 0..octaves {
        let period = ((256.0 / cell.0) as u32, (256.0 / cell.1) as u32);
        sum += amp * value_noise(x / cell.0, y / cell.1, period, seed * 31 + o);
        norm += amp;
        amp *= 0.5;
        cell = ((cell.0 * 0.5).max(1.0), (cell.1 * 0.5).max(1.0));
    }
    sum / norm
}

/// Smooth value noise on a lattice that repeats every `period` cells.
fn value_noise(x: f32, y: f32, period: (u32, u32), seed: u32) -> f32 {
    let period = (period.0.max(1), period.1.max(1));
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (smooth(fx), smooth(fy));
    let corner = |dx: u32, dy: u32| {
        let cx = (xi as i64 as u32).wrapping_add(dx) % period.0;
        let cy = (yi as i64 as u32).wrapping_add(dy) % period.1;
        hash(cx, cy, seed)
    };
    let top = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * sx;
    let bottom = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * sx;
    top + (bottom - top) * sy
}

/// A repeatable pseudo-random value in 0..1 for a lattice point.
fn hash(x: u32, y: u32, seed: u32) -> f32 {
    let mut h =
        x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_patterns_repeat_seamlessly_and_use_the_full_range() {
        for p in builtin() {
            assert!(p.size.is_power_of_two());
            let (lo, hi) = p
                .data
                .iter()
                .fold((1.0f32, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            assert!(lo < 0.02 && hi > 0.98, "{}: {lo}..{hi}", p.name);
            // The edges meet: a row's last and first texels are close.
            let n = p.size;
            let seam: f32 = (0..n)
                .map(|y| (p.data[y * n] - p.data[y * n + n - 1]).abs())
                .sum::<f32>()
                / n as f32;
            let inside: f32 = (0..n)
                .map(|y| (p.data[y * n + n / 2] - p.data[y * n + n / 2 - 1]).abs())
                .sum::<f32>()
                / n as f32;
            assert!(
                seam < inside * 2.5 + 0.02,
                "{}: seam {seam} vs {inside}",
                p.name
            );
            // Wrapping reads past the edges.
            assert_eq!(p.at(0.5, 0.5, 1.0), p.at(n as f32 + 0.5, 0.5, 1.0));
        }
    }

    #[test]
    fn no_strength_changes_nothing_and_full_strength_varies() {
        let mut tex = BrushTexture::new(builtin()[0].clone());
        for mode in TextureMode::ALL {
            tex.mode = mode;
            tex.strength = 0.0;
            let mut row = vec![0.6; 64];
            tex.apply_row(10, 0, &mut row);
            assert!(row.iter().all(|&a| (a - 0.6).abs() < 1e-6), "{mode:?}");
            tex.strength = 1.0;
            let mut row = vec![0.6; 256];
            tex.apply_row(10, 0, &mut row);
            let (lo, hi) = row
                .iter()
                .fold((1.0f32, 0.0f32), |(lo, hi), &a| (lo.min(a), hi.max(a)));
            assert!(hi - lo > 0.2, "{mode:?} varies: {lo}..{hi}");
        }
    }

    #[test]
    fn height_fills_in_with_pressure() {
        let mut tex = BrushTexture::new(builtin()[0].clone());
        tex.mode = TextureMode::Height;
        tex.strength = 1.0;
        let covered = |a: f32| {
            let mut row = vec![a; 256];
            tex.apply_row(40, 0, &mut row);
            row.iter().filter(|&&v| v > a * 0.5).count()
        };
        assert!(
            covered(0.3) < covered(0.9),
            "{} vs {}",
            covered(0.3),
            covered(0.9)
        );
    }
}
