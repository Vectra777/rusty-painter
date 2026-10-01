//! Brush texture: a grey pattern (paper grain, canvas weave, charcoal) that
//! repeats seamlessly across the canvas and takes paint away from each dab
//! where it's low, the way a textured paper catches a dry brush.
//!
//! The pattern is pinned to the canvas (like real paper), so overlapping
//! strokes share their grain; [`GrainPlacement`] can instead move it with
//! the stroke or each dab, turn it, and shift it at random. How it combines
//! with a dab's alpha:
//!
//! - **Multiply**: the grain darkens the stroke evenly.
//! - **Subtract**: low spots lose paint first; heavy strokes fill in.
//! - **Height**: only the paper's peaks catch paint under light coverage
//!   (low pressure, soft edges); pressing harder fills the valleys.
//! - **Colour dodge**: the peaks strengthen the paint: soft edges and light strokes turn grainy and bright.
//! - **Hard mix**: paint snaps to full or nothing by grain and coverage, a
//!   crisp, broken dry-brush edge.

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

    /// A picture's grey (transparent counting as white) through `adjust`,
    /// as it is rather than stretched to the full range (another app's
    /// grain, kept as that app shows it); resampled like
    /// [`Self::from_image`].
    pub fn from_image_with(
        name: &str,
        img: &image::DynamicImage,
        adjust: impl Fn(f32) -> f32,
    ) -> Arc<Self> {
        let rgba = img.to_rgba8();
        let side = rgba.width().max(rgba.height()).clamp(16, 1024);
        let size = side.next_power_of_two() as usize;
        let resized = image::imageops::resize(
            &rgba,
            size as u32,
            size as u32,
            image::imageops::FilterType::Triangle,
        );
        let data = resized
            .pixels()
            .map(|p| {
                let grey = (p[0] as f32 * 11.0 + p[1] as f32 * 16.0 + p[2] as f32 * 5.0) / 32.0;
                let a = p[3] as f32 / 255.0;
                adjust(grey / 255.0 * a + (1.0 - a))
            })
            .collect();
        Arc::new(Self {
            name: name.to_string(),
            size,
            data,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextureMode {
    Multiply,
    Subtract,
    Height,
    ColorDodge,
    HardMix,
}

impl TextureMode {
    pub const ALL: [TextureMode; 5] = [
        Self::Multiply,
        Self::Subtract,
        Self::Height,
        Self::ColorDodge,
        Self::HardMix,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Multiply => "Multiply",
            Self::Subtract => "Subtract",
            Self::Height => "Height",
            Self::ColorDodge => "Colour dodge",
            Self::HardMix => "Hard mix",
        }
    }
}

/// Where a brush's grain sits. All off: pinned to the canvas, upright,
/// the same for every stroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GrainPlacement {
    /// The grain moves with the stroke: anchored to where it starts rather
    /// than to the canvas.
    pub follow_stroke: bool,
    /// The grain turned, degrees counter-clockwise.
    pub angle: f32,
    /// Shift the grain by a random amount each stroke (each dab, with
    /// `per_dab`).
    pub random_offset: bool,
    /// Each dab gets the grain afresh, anchored to its own centre, rather
    /// than the stroke sharing one sheet of it.
    pub per_dab: bool,
}

impl GrainPlacement {
    pub fn is_active(&self) -> bool {
        self.follow_stroke || self.angle != 0.0 || self.random_offset || self.per_dab
    }
}

/// What a stroke fixes about its grain when it starts (see
/// [`GrainPlacement`]): where it began, its random shift (a share of the
/// pattern's side) and a seed for each dab's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StrokeGrain {
    pub origin: [f32; 2],
    pub offset: [f32; 2],
    pub seed: u32,
}

/// An imported brush's texturing: its mode (by the preset's number) and
/// whether it uses "soft texturing" or the classic strength. The grain
/// then combines with each dab by the preset's own formulas, its value as
/// a mask (light keeps paint in multiply, takes it away in subtract and
/// the height modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KritaTexturing {
    pub mode: u8,
    pub soft: bool,
}

impl KritaTexturing {
    /// Whether the preset's mode `mode` is one of the alpha formulas here
    /// (lightness and gradient texturing colour the dab instead).
    pub fn supports(mode: u8) -> bool {
        matches!(mode, 0 | 1 | 4..=15)
    }

    pub fn label(self) -> &'static str {
        match self.mode {
            0 => "Multiply",
            1 => "Subtract",
            4 => "Darken",
            5 => "Overlay",
            6 => "Color dodge",
            7 => "Color burn",
            8 => "Linear dodge",
            9 => "Linear burn",
            10 => "Hard mix",
            11 => "Hard mix softer",
            12 => "Height",
            13 => "Linear height",
            14 => "Height (Photoshop)",
            15 => "Linear height (Photoshop)",
            _ => "Multiply",
        }
    }

    /// Dab alpha `a` (the tip's coverage, 0..1) textured by grain value `t`
    /// at strength `s`.
    #[inline]
    pub fn combine(self, a: f32, s: f32, t: f32) -> f32 {
        let unite = |x: f32, y: f32| x + y - x * y;
        let is = 1.0 - s;
        let soft = self.soft;
        let dodge = |src: f32, dst: f32| {
            if src >= 1.0 {
                if dst <= 0.0 { 0.0 } else { 1.0 }
            } else {
                dst / (1.0 - src)
            }
        };
        let burn = |src: f32, dst: f32| {
            if dst >= 1.0 {
                1.0
            } else if src < 1.0 - dst {
                0.0
            } else {
                1.0 - (1.0 - dst) / src
            }
        };
        // Overlay of `src` on `dst` is hard light the other way.
        let overlay = |src: f32, dst: f32| {
            if src > 0.5 {
                let k = 2.0 * src - 1.0;
                dst + k - dst * k
            } else {
                dst * 2.0 * src
            }
        };
        let v = match self.mode {
            0 if soft => a * unite(t, is),
            0 => a * t * s,
            1 if soft => a - t * s,
            1 => a - (t + is),
            4 if soft => unite(t, is).min(a),
            4 => t.min(a * s),
            5 if soft => overlay(a, unite(t, is)),
            5 => overlay(a * s, t),
            6 if soft => dodge(t * s, a),
            6 => dodge(t, a * s),
            7 if soft => burn(unite(t, is), a),
            7 => burn(t, a * s),
            8 if a <= 0.0 => 0.0,
            8 if soft => t * s + a,
            8 => t + a * s,
            9 if soft => unite(t, is) + a - 1.0,
            9 => t + a * s - 1.0,
            10 => {
                let hard = |src: f32, dst: f32| if src + dst > 1.0 { 1.0 } else { 0.0 };
                if soft {
                    hard(unite(t, is), a) * unite(a, s)
                } else {
                    hard(t, a * s)
                }
            }
            11 if soft => 3.0 * a - 2.0 * (1.0 - t * s),
            11 => 3.0 * a * s - 2.0 * (1.0 - t),
            12 | 13 => {
                let s = 0.99 * s;
                let is = 1.0 - s;
                if self.mode == 12 {
                    if soft {
                        a / is - t * s
                    } else {
                        a / is - (t + is)
                    }
                } else if soft {
                    let (d, ts) = (a / is, t * s);
                    (d * (1.0 - ts)).max(d - ts)
                } else {
                    let d = a / is - is;
                    (d * (1.0 - t)).max(d - t)
                }
            }
            14 if soft => a + a * 9.0 * s - t * s,
            14 => a * 10.0 * s - t,
            15 if soft => {
                let (d, ts) = (a + a * 9.0 * s, t * s);
                (d * (1.0 - ts)).max(d - ts)
            }
            15 => {
                let d = a * 10.0 * s;
                ((1.0 - t) * d).max(d - t)
            }
            _ => a * unite(t, is),
        };
        v.clamp(0.0, 1.0)
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
    /// Moved with the stroke, turned, shifted, per dab.
    pub placement: GrainPlacement,
    /// An imported brush's own texturing, instead of `mode`.
    pub krita: Option<KritaTexturing>,
}

impl BrushTexture {
    pub fn new(pattern: Arc<Pattern>) -> Self {
        Self {
            pattern,
            mode: TextureMode::Subtract,
            scale: 1.0,
            strength: 0.8,
            invert: false,
            placement: GrainPlacement::default(),
            krita: None,
        }
    }

    /// Apply to a row of dab alphas at canvas row `y`, columns
    /// `x0..x0 + alphas.len()`.
    #[inline]
    pub fn apply_row(&self, y: usize, x0: usize, alphas: &mut [f32]) {
        self.apply_row_scaled(y, x0, alphas, 1.0);
    }

    /// [`Self::apply_row`] with the strength scaled by `factor` (a dab's
    /// input mappings).
    #[inline]
    pub fn apply_row_scaled(&self, y: usize, x0: usize, alphas: &mut [f32], factor: f32) {
        if let Some(k) = self.krita {
            return self.krita_row(y, x0, alphas, factor, k);
        }
        // The mode chosen once per row, not per pixel.
        match self.mode {
            TextureMode::Multiply => self.scaled_row(y, x0, alphas, factor, |a, s, t| {
                combine(TextureMode::Multiply, a, s, t)
            }),
            TextureMode::Subtract => self.scaled_row(y, x0, alphas, factor, |a, s, t| {
                combine(TextureMode::Subtract, a, s, t)
            }),
            TextureMode::Height => self.scaled_row(y, x0, alphas, factor, |a, s, t| {
                combine(TextureMode::Height, a, s, t)
            }),
            TextureMode::ColorDodge => self.scaled_row(y, x0, alphas, factor, |a, s, t| {
                combine(TextureMode::ColorDodge, a, s, t)
            }),
            TextureMode::HardMix => self.scaled_row(y, x0, alphas, factor, |a, s, t| {
                combine(TextureMode::HardMix, a, s, t)
            }),
        }
    }

    /// [`Self::apply_row_scaled`] by an imported texturing's formula: out of line, so the
    /// usual modes' row stays small enough to inline into the painter.
    #[inline(never)]
    fn krita_row(&self, y: usize, x0: usize, alphas: &mut [f32], factor: f32, k: KritaTexturing) {
        self.scaled_row(y, x0, alphas, factor, |a, s, t| k.combine(a, s, t));
    }

    /// [`Self::apply_row_scaled`] combining by `combine(alpha, strength,
    /// grain)` (a constant formula at each call, so each copy is
    /// specialised to it).
    #[inline(always)]
    fn scaled_row(
        &self,
        y: usize,
        x0: usize,
        alphas: &mut [f32],
        factor: f32,
        combine: impl Fn(f32, f32, f32) -> f32,
    ) {
        let p = &*self.pattern;
        let inv = 1.0 / self.scale.max(0.05);
        let s = (self.strength * factor).clamp(0.0, 1.0);
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
            *a = combine(*a, s, t);
        }
    }
}

impl BrushTexture {
    /// [`Self::apply_row_scaled`] with the grain placed by
    /// [`Self::placement`]: `grain` is the stroke's, `center` the dab's.
    pub fn apply_row_placed(
        &self,
        y: usize,
        x0: usize,
        alphas: &mut [f32],
        factor: f32,
        grain: &StrokeGrain,
        center: [f32; 2],
    ) {
        // The mode chosen once per row, not per pixel.
        if let Some(k) = self.krita {
            return self.krita_placed_row(y, x0, alphas, factor, grain, center, k);
        }
        // The mode chosen once per row, each arm its own copy of the loop.
        macro_rules! row {
            ($mode:expr) => {
                self.placed_row(y, x0, alphas, factor, grain, center, |a, s, t| {
                    combine($mode, a, s, t)
                })
            };
        }
        match self.mode {
            TextureMode::Multiply => row!(TextureMode::Multiply),
            TextureMode::Subtract => row!(TextureMode::Subtract),
            TextureMode::Height => row!(TextureMode::Height),
            TextureMode::ColorDodge => row!(TextureMode::ColorDodge),
            TextureMode::HardMix => row!(TextureMode::HardMix),
        }
    }

    /// [`Self::apply_row_placed`] by an imported texturing's formula, out
    /// of line.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn krita_placed_row(
        &self,
        y: usize,
        x0: usize,
        alphas: &mut [f32],
        factor: f32,
        grain: &StrokeGrain,
        center: [f32; 2],
        k: KritaTexturing,
    ) {
        self.placed_row(y, x0, alphas, factor, grain, center, |a, s, t| {
            k.combine(a, s, t)
        });
    }

    /// [`Self::apply_row_placed`] combining by `combine` (see
    /// [`Self::scaled_row`]).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn placed_row(
        &self,
        y: usize,
        x0: usize,
        alphas: &mut [f32],
        factor: f32,
        grain: &StrokeGrain,
        center: [f32; 2],
        combine: impl Fn(f32, f32, f32) -> f32,
    ) {
        let p = &*self.pattern;
        let place = &self.placement;
        let scale = self.scale.max(0.05);
        let inv = 1.0 / scale;
        let s = (self.strength * factor).clamp(0.0, 1.0);
        let mask = p.size as i64 - 1;
        let period = p.size as f32 * scale;
        let anchor = if place.per_dab {
            center
        } else if place.follow_stroke {
            grain.origin
        } else {
            [0.0, 0.0]
        };
        let shift = match (place.random_offset, place.per_dab) {
            (false, _) => [0.0, 0.0],
            (true, false) => grain.offset,
            (true, true) => {
                let (bx, by) = (center[0].to_bits(), center[1].to_bits());
                [hash(bx, by, grain.seed), hash(by, bx, grain.seed ^ 0x51f1)]
            }
        };
        let (sin, cos) = (-place.angle.to_radians()).sin_cos();
        // Canvas point → pattern pixels: from the anchor, turned back by the
        // grain's angle, then shifted.
        let ry = y as f32 + 0.5 - anchor[1];
        for (i, a) in alphas.iter_mut().enumerate() {
            if *a <= 0.0 {
                continue;
            }
            let rx = (x0 + i) as f32 + 0.5 - anchor[0];
            // Screen y points down: a counter-clockwise turn on screen.
            let tx = cos * rx + sin * ry + shift[0] * period;
            let ty = -sin * rx + cos * ry + shift[1] * period;
            let (u, v) = (tx * inv + 0.5, ty * inv + 0.5);
            let (fu, fv) = (u.floor(), v.floor());
            let (du, dv) = (u - fu, v - fv);
            let (iu, iv) = (fu as i64, fv as i64);
            let (c0, c1) = (((iu - 1) & mask) as usize, (iu & mask) as usize);
            let (r0, r1) = (
                ((iv - 1) & mask) as usize * p.size,
                (iv & mask) as usize * p.size,
            );
            let top = p.data[r0 + c0] + (p.data[r0 + c1] - p.data[r0 + c0]) * du;
            let bottom = p.data[r1 + c0] + (p.data[r1 + c1] - p.data[r1 + c0]) * du;
            let mut t = top + (bottom - top) * dv;
            if self.invert {
                t = 1.0 - t;
            }
            *a = combine(*a, s, t);
        }
    }
}

/// A dab alpha `a` with grain height `t` at strength `s`.
#[inline]
fn combine(mode: TextureMode, a: f32, s: f32, t: f32) -> f32 {
    match mode {
        TextureMode::Multiply => a * (1.0 - s + s * t),
        TextureMode::Subtract => (a - s * (1.0 - t)).max(0.0),
        TextureMode::Height => {
            // Paint reaches down to 1 - a: a light dab only the peaks, a
            // full one everything.
            let floor = 1.0 - a;
            let h = ((t - floor) * 4.0 + 0.5).clamp(0.0, 1.0);
            a * (1.0 - s + s * h)
        }
        // The grain as the dodging layer: a / (1 - t), at strength s.
        TextureMode::ColorDodge => (a / (1.0 - s * t).max(1e-3)).min(1.0),
        // Photoshop's hard mix, softened (3a - 2(1 - t)), blended in by s.
        TextureMode::HardMix => {
            let h = (3.0 * a - 2.0 * (1.0 - t)).clamp(0.0, 1.0);
            a + (h - a) * s
        }
    }
}

/// Side of the built-in patterns: large enough that the repeat doesn't show.
const TILE: usize = 512;

/// The built-in patterns (generated once).
pub fn builtin() -> &'static [Arc<Pattern>] {
    static PATTERNS: OnceLock<Vec<Arc<Pattern>>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let make = |name: &str, f: &(dyn Fn(f32, f32) -> f32 + Sync)| {
            use rayon::prelude::*;
            let data = (0..TILE * TILE)
                .into_par_iter()
                .map(|i| f((i % TILE) as f32, (i / TILE) as f32))
                .collect();
            Arc::new(Pattern {
                name: name.to_string(),
                size: TILE,
                data: normalized(data),
            })
        };
        vec![
            // Fine tooth with a little larger-scale unevenness.
            make("Paper", &|x, y| {
                0.75 * fbm(x, y, (8.0, 8.0), 3, 1) + 0.25 * fbm(x, y, (64.0, 64.0), 2, 2)
            }),
            // Coarser, deeper tooth: pronounced peaks and valleys.
            make("Rough paper", &|x, y| {
                smoothstep(0.3, 0.7, fbm(x, y, (16.0, 16.0), 4, 3))
            }),
            make("Canvas", &weave),
            // Pixel-scale grain.
            make("Fine grain", &|x, y| fbm(x, y, (3.0, 3.0), 1, 5)),
            // Streaks along x on a fine tooth.
            make("Charcoal", &|x, y| {
                0.55 * fbm(x, y, (64.0, 4.0), 3, 6) + 0.45 * fbm(x, y, (4.0, 4.0), 2, 7)
            }),
        ]
    })
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A plain canvas weave: threads 8 px apart going over and under each
/// other, each thread rounded across and slightly uneven along.
fn weave(x: f32, y: f32) -> f32 {
    const PITCH: f32 = 8.0;
    let (cx, cy) = ((x / PITCH).floor(), (y / PITCH).floor());
    let (fx, fy) = (x / PITCH - cx, y / PITCH - cy);
    // Which thread is on top alternates cell by cell.
    let warp_on_top = (cx as i64 + cy as i64).rem_euclid(2) == 0;
    // A thread's cross-section: high in the middle, low at its edges; along
    // it, it dips where it goes under the other.
    let across = |f: f32| (std::f32::consts::PI * f).sin();
    let warp = across(fx) * (0.6 + 0.4 * across(fy));
    let weft = across(fy) * (0.6 + 0.4 * across(fx));
    let top = if warp_on_top { warp } else { weft };
    let under = if warp_on_top { weft } else { warp };
    let v = top.max(under * 0.55);
    0.85 * v + 0.15 * fbm(x, y, (4.0, 4.0), 2, 8)
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

/// Tileable fractal gradient noise over the [`TILE`] square: `octaves`,
/// the first with cells of `cell` pixels (powers of two, so it tiles), each
/// next one half the size. Gradient (Perlin) noise rather than value
/// noise: no blocky, cross-shaped lattice artefacts.
fn fbm(x: f32, y: f32, cell: (f32, f32), octaves: u32, seed: u32) -> f32 {
    // At pixel centres: on whole-number points fine octaves would sample
    // the lattice itself, where gradient noise is zero, and draw a grid.
    let (x, y) = (x + 0.5, y + 0.5);
    let (mut sum, mut amp, mut norm, mut cell) = (0.0, 1.0, 0.0, cell);
    for o in 0..octaves {
        if cell.0 < 2.0 || cell.1 < 2.0 {
            break;
        }
        let s = seed * 31 + o;
        let period = (
            ((TILE as f32 / cell.0) as u32).max(1),
            ((TILE as f32 / cell.1) as u32).max(1),
        );
        // Each octave shifted by its own part of a cell, so their lattices
        // don't line up (the pattern still tiles: the period is unchanged).
        let (ox, oy) = (hash(o, 1, s) * cell.0, hash(o, 2, s) * cell.1);
        sum += amp * gradient_noise((x + ox) / cell.0, (y + oy) / cell.1, period, s);
        norm += amp;
        amp *= 0.5;
        cell = (cell.0 * 0.5, cell.1 * 0.5);
    }
    0.5 + 0.5 * sum / norm.max(1e-6)
}

/// Perlin gradient noise (about -1..1) on a lattice repeating every
/// `period` cells.
fn gradient_noise(x: f32, y: f32, period: (u32, u32), seed: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (u, v) = (fade(fx), fade(fy));
    let corner = |dx: u32, dy: u32| {
        let cx = (xi as i64 as u32).wrapping_add(dx) % period.0;
        let cy = (yi as i64 as u32).wrapping_add(dy) % period.1;
        let a = hash(cx, cy, seed) * std::f32::consts::TAU;
        // The corner's gradient dotted with the offset to it.
        a.cos() * (fx - dx as f32) + a.sin() * (fy - dy as f32)
    };
    let top = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * u;
    let bottom = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * u;
    (top + (bottom - top) * v) * std::f32::consts::SQRT_2
}

/// A repeatable pseudo-random value in 0..1 for a lattice point: x, y and
/// the seed mixed in one after another (a plain xor of their products
/// correlates neighbours and draws repeating motifs).
fn hash(x: u32, y: u32, seed: u32) -> f32 {
    fn mix(mut h: u32) -> u32 {
        h ^= h >> 16;
        h = h.wrapping_mul(0x7feb_352d);
        h ^= h >> 15;
        h = h.wrapping_mul(0x846c_a68b);
        h ^= h >> 16;
        h
    }
    let h = mix(mix(mix(seed.wrapping_add(0x9e37_79b9)) ^ x) ^ y);
    (h >> 8) as f32 / (1u32 << 24) as f32
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
    fn every_mode_stays_in_range_and_never_paints_outside_the_dab() {
        let steps = || (0..=20).map(|i| i as f32 / 20.0);
        for mode in TextureMode::ALL {
            for a in steps() {
                for s in steps() {
                    for t in steps() {
                        let v = combine(mode, a, s, t);
                        assert!((0.0..=1.0).contains(&v), "{mode:?} {a} {s} {t}: {v}");
                        if a == 0.0 && mode != TextureMode::HardMix {
                            assert_eq!(v, 0.0, "{mode:?}: paint from nothing");
                        }
                    }
                    // No strength, no change.
                    assert!((combine(mode, a, 0.0, 0.3) - a).abs() < 1e-6, "{mode:?}");
                }
            }
        }
        // Colour dodge strengthens on the peaks, hard mix snaps.
        assert!(combine(TextureMode::ColorDodge, 0.5, 1.0, 0.9) > 0.9);
        assert_eq!(combine(TextureMode::HardMix, 0.9, 1.0, 0.9), 1.0);
        assert_eq!(combine(TextureMode::HardMix, 0.2, 1.0, 0.2), 0.0);
    }

    #[test]
    fn krita_texturing_follows_krita_s_formulas() {
        let k = |mode, soft| KritaTexturing { mode, soft };
        let (a, s, t) = (0.8f32, 0.5f32, 0.6f32);
        let close = |x: f32, y: f32| (x - y).abs() < 1e-5;
        // Classic multiply scales the whole dab by the strength; soft only
        // the grain.
        assert!(close(k(0, false).combine(a, s, t), a * t * s));
        assert!(close(k(0, true).combine(a, s, t), a * (t + 0.5 - t * 0.5)));
        // Subtract takes paint away where the grain is light.
        assert!(close(k(1, true).combine(a, s, t), a - t * s));
        assert!(close(
            k(1, false).combine(a, s, t),
            (a - (t + 0.5)).max(0.0)
        ));
        assert!(k(1, true).combine(a, 1.0, 1.0) < k(1, true).combine(a, 1.0, 0.0));
        // Colour dodge: a / (1 - t·s) soft, (a·s) / (1 - t) classic.
        assert!(close(k(6, true).combine(0.4, s, t), 0.4 / (1.0 - t * s)));
        assert!(close(
            k(6, false).combine(0.4, s, t),
            (0.4 * s / (1.0 - t)).min(1.0)
        ));
        // Height: a / (1 - 0.99s) - t·0.99s, soft.
        let s99 = 0.99 * s;
        assert!(close(
            k(12, true).combine(0.3, s, t),
            (0.3 / (1.0 - s99) - t * s99).clamp(0.0, 1.0)
        ));
        // Every mode stays in range and leaves no paint outside the dab
        // (linear dodge on nothing is nothing).
        for mode in (0..=15).filter(|&m| KritaTexturing::supports(m)) {
            for soft in [false, true] {
                for i in 0..=10 {
                    for j in 0..=10 {
                        let v = k(mode, soft).combine(i as f32 / 10.0, 0.7, j as f32 / 10.0);
                        assert!((0.0..=1.0).contains(&v), "{mode} {soft}: {v}");
                    }
                }
            }
        }
        assert_eq!(k(8, true).combine(0.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn a_krita_texture_row_uses_krita_s_formula() {
        let mut tex = BrushTexture::new(builtin()[0].clone());
        tex.strength = 0.5;
        tex.krita = Some(KritaTexturing {
            mode: 0,
            soft: false,
        });
        let mut row = vec![0.8f32; 32];
        tex.apply_row(7, 3, &mut row);
        for (i, &v) in row.iter().enumerate() {
            let t = tex.pattern.at(3.0 + i as f32 + 0.5, 7.5, 1.0);
            assert!((v - 0.8 * t * 0.5).abs() < 1e-4, "{i}");
        }
    }

    #[test]
    fn rows_in_each_mode_match_the_formula() {
        // The row loops are specialised per mode: each must give `combine`.
        let mut tex = BrushTexture::new(builtin()[0].clone());
        tex.strength = 0.7;
        for mode in TextureMode::ALL {
            tex.mode = mode;
            let alphas: Vec<f32> = (0..64).map(|i| i as f32 / 63.0).collect();
            let mut row = alphas.clone();
            tex.apply_row(9, 5, &mut row);
            let mut placed = alphas.clone();
            tex.apply_row_placed(9, 5, &mut placed, 1.0, &StrokeGrain::default(), [0.0, 0.0]);
            for (i, &a) in alphas.iter().enumerate() {
                let t = tex.pattern.at(5.0 + i as f32 + 0.5, 9.5, 1.0);
                let want = if a <= 0.0 {
                    a
                } else {
                    combine(mode, a, 0.7, t)
                };
                assert!((row[i] - want).abs() < 1e-4, "{mode:?} pinned {i}");
                assert!((placed[i] - want).abs() < 1e-3, "{mode:?} placed {i}");
            }
        }
    }

    /// The grain's height (Multiply at full strength on full alphas) over
    /// a `n`×`n` square from canvas pixel `(x0, y0)`.
    fn heights(
        tex: &BrushTexture,
        grain: &StrokeGrain,
        x0: usize,
        y0: usize,
        n: usize,
    ) -> Vec<f32> {
        let mut out = Vec::new();
        for y in y0..y0 + n {
            let mut row = vec![1.0; n];
            tex.apply_row_placed(y, x0, &mut row, 1.0, grain, [0.0, 0.0]);
            out.extend(row);
        }
        out
    }

    fn multiply(pattern: usize) -> BrushTexture {
        let mut tex = BrushTexture::new(builtin()[pattern].clone());
        tex.mode = TextureMode::Multiply;
        tex.strength = 1.0;
        tex
    }

    #[test]
    fn placed_grain_matches_the_pinned_one_when_not_moved() {
        let mut tex = multiply(1);
        tex.scale = 1.7;
        // Placed, but with nothing moved: the canvas-pinned grain.
        tex.placement.random_offset = true;
        let grain = StrokeGrain::default();
        let placed = heights(&tex, &grain, 30, 40, 48);
        let mut pinned = Vec::new();
        for y in 40..88 {
            let mut row = vec![1.0; 48];
            tex.apply_row(y, 30, &mut row);
            pinned.extend(row);
        }
        for (a, b) in placed.iter().zip(&pinned) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn a_rotated_grain_is_rotated() {
        // Anchored at (100, 100): turned a quarter counter-clockwise, the
        // grain at offset (i, j) from there is the upright grain's at
        // (-j - 1, i).
        let mut tex = multiply(4);
        tex.placement.follow_stroke = true;
        let grain = StrokeGrain {
            origin: [100.0, 100.0],
            ..Default::default()
        };
        let upright = heights(&tex, &grain, 36, 100, 64);
        tex.placement.angle = 90.0;
        let turned = heights(&tex, &grain, 100, 100, 64);
        let mut differs = 0;
        for j in 0..64 {
            for i in 0..64 {
                let t = turned[j * 64 + i];
                // Upright square: columns 36..100, so x = 100 - j - 1.
                let u = upright[i * 64 + (63 - j)];
                assert!((t - u).abs() < 1e-3, "({i}, {j}): {t} vs {u}");
                differs += usize::from((t - upright[j * 64 + i]).abs() > 0.05);
            }
        }
        // Charcoal streaks along x: turning them shows.
        assert!(differs > 500, "{differs}");
    }

    #[test]
    fn the_grain_moves_with_its_anchor_and_offset() {
        let mut tex = multiply(0);
        tex.placement.follow_stroke = true;
        let at = |origin: [f32; 2]| {
            let grain = StrokeGrain {
                origin,
                ..Default::default()
            };
            heights(&tex, &grain, origin[0] as usize, origin[1] as usize, 32)
        };
        assert_eq!(at([10.0, 20.0]), at([47.0, 81.0]));
        // A random offset shifts it.
        tex.placement.random_offset = true;
        let shifted = |offset: [f32; 2]| {
            let grain = StrokeGrain {
                offset,
                ..Default::default()
            };
            heights(&tex, &grain, 0, 0, 32)
        };
        assert_ne!(shifted([0.0, 0.0]), shifted([0.3, 0.6]));
        assert_eq!(shifted([0.3, 0.6]), shifted([0.3, 0.6]));
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
