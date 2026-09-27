//! Pixel maths shared by the canvas: sRGB/linear conversion (with lookup
//! tables), alpha over, opacity, downsampling and dithering.

use std::cell::Cell;
use std::sync::OnceLock;

use eframe::egui::{Color32, ColorImage, Rgba};
use wide::{CmpGt, f32x4, f32x8};

const GAMMA_LUT_SIZE: usize = 4096;

static GAMMA_LUT: OnceLock<[u8; GAMMA_LUT_SIZE]> = OnceLock::new();
static SRGB_TO_LINEAR_LUT: OnceLock<[f32; 256]> = OnceLock::new();

// Per-pixel blending hits these accessors billions of times; the OnceLock's
// atomic check alone was ~13% of total CPU in profiling. Cache the already-resolved
// reference per-thread so the hot path is a plain (non-atomic) TLS read instead.
fn gamma_lut() -> &'static [u8; GAMMA_LUT_SIZE] {
    thread_local! {
        static CACHED: Cell<Option<&'static [u8; GAMMA_LUT_SIZE]>> = const { Cell::new(None) };
    }
    CACHED.with(|cached| {
        if let Some(lut) = cached.get() {
            return lut;
        }
        let lut = GAMMA_LUT.get_or_init(|| {
            let mut lut = [0u8; GAMMA_LUT_SIZE];
            for (i, item) in lut.iter_mut().enumerate() {
                let linear = i as f32 / (GAMMA_LUT_SIZE - 1) as f32;
                let srgb = if linear <= 0.0031308 {
                    linear * 12.92
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                };
                *item = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            lut
        });
        cached.set(Some(lut));
        lut
    })
}

fn srgb_to_linear_lut() -> &'static [f32; 256] {
    thread_local! {
        static CACHED: Cell<Option<&'static [f32; 256]>> = const { Cell::new(None) };
    }
    CACHED.with(|cached| {
        if let Some(lut) = cached.get() {
            return lut;
        }
        // Filled from egui's own conversion (same sRGB curve) so values are
        // bit-identical to `Rgba::from(Color32)`, which the layer compositor used.
        let lut = SRGB_TO_LINEAR_LUT.get_or_init(|| {
            std::array::from_fn(|i| eframe::egui::ecolor::linear_f32_from_gamma_u8(i as u8))
        });
        cached.set(Some(lut));
        lut
    })
}

#[inline]
fn srgb_u8_to_linear(v: u8) -> f32 {
    srgb_to_linear_lut()[v as usize]
}

/// `img` shrunk by `2^level`, each pixel the average of its block in linear
/// light (partial blocks at the canvas edge average the pixels they have).
// The app composites and averages in one pass (`write_tile_rect_downsampled`);
// this two-step version stays as the reference its tests and the benchmark
// compare against, so the binary target sees it unused.
#[allow(dead_code)]
pub fn downsample(img: &ColorImage, level: u32) -> ColorImage {
    if level == 0 {
        return img.clone();
    }
    let block = 1usize << level;
    let [w, h] = img.size;
    let (out_w, out_h) = (w.div_ceil(block), h.div_ceil(block));
    let mut out = ColorImage::new([out_w, out_h], Color32::TRANSPARENT);
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = [0.0f32; 4];
            let mut count = 0.0;
            for y in oy * block..((oy + 1) * block).min(h) {
                for x in ox * block..((ox + 1) * block).min(w) {
                    let p = color32_to_linear(img.pixels[y * w + x]);
                    for (s, v) in sum.iter_mut().zip(p.to_array()) {
                        *s += v;
                    }
                    count += 1.0;
                }
            }
            let [r, g, b, a] = sum.map(|v| v / count);
            out.pixels[oy * out_w + ox] =
                rgba_to_color32_fast(Rgba::from_rgba_premultiplied(r, g, b, a));
        }
    }
    out
}

/// Average, in linear light, of `src[i]` over `dst[i]` for the pixels `i`
/// of one block (see [`alpha_over`]), rounded to sRGB once at the end.
/// `pixels` yields `(src, dst)` pairs. Equivalent to blending each pixel with
/// [`alpha_over`] and averaging with [`downsample`], without rounding every
/// full-resolution pixel to 8 bits and decoding it again (at most 1/255 off).
#[inline]
pub(crate) fn average_over(pixels: impl Iterator<Item = (Color32, Color32)>) -> Color32 {
    let lut = srgb_to_linear_lut();
    let lin = |c: Color32| {
        [
            lut[c.r() as usize],
            lut[c.g() as usize],
            lut[c.b() as usize],
            c.a() as f32 / 255.0,
        ]
    };
    let mut sum = [0.0f32; 4];
    let mut count = 0u32;
    for (src, dst) in pixels {
        // Same cases as `alpha_over`, before its final rounding.
        let px = match (src.a(), dst.a()) {
            (0, _) => lin(dst),
            (255, _) | (_, 0) => lin(src),
            (_, da) => {
                let (s, d) = (lin(src), lin(dst));
                let keep = 1.0 - s[3];
                let a = if da == 255 { 1.0 } else { s[3] + d[3] * keep };
                [
                    s[0] + d[0] * keep,
                    s[1] + d[1] * keep,
                    s[2] + d[2] * keep,
                    a,
                ]
            }
        };
        for (acc, v) in sum.iter_mut().zip(px) {
            *acc += v;
        }
        count += 1;
    }
    if count == 0 {
        return Color32::TRANSPARENT;
    }
    let n = count as f32;
    let [r, g, b, a] = sum.map(|v| v / n);
    rgba_to_color32_fast(Rgba::from_rgba_premultiplied(r, g, b, a))
}

/// Un-premultiplied sRGB value of a stored channel, by `(value, alpha)`.
/// Stored pixels are premultiplied in linear light (egui's `Color32`), in
/// either blend space, so every tool reads them the same way.
fn unmultiply_lut() -> &'static [u8; 256 * 256] {
    // Cached per thread, like `gamma_lut`: this runs per pixel.
    thread_local! {
        static CACHED: Cell<Option<&'static [u8; 256 * 256]>> = const { Cell::new(None) };
    }
    CACHED.with(|cached| {
        if let Some(lut) = cached.get() {
            return lut;
        }
        let lut = build_unmultiply_lut();
        cached.set(Some(lut));
        lut
    })
}

fn build_unmultiply_lut() -> &'static [u8; 256 * 256] {
    static LUT: OnceLock<Box<[u8; 256 * 256]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = Box::new([0u8; 256 * 256]);
        for a in 1..256usize {
            for v in 0..256usize {
                let linear = srgb_u8_to_linear(v as u8) * 255.0 / a as f32;
                lut[a << 8 | v] = linear_to_srgb_exact(linear.min(1.0));
            }
        }
        lut
    })
}

fn linear_to_srgb_exact(linear: f32) -> u8 {
    let v = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A stored colour premultiplied in gamma space (sRGB values times alpha),
/// for gamma-space blending: no decoding to linear light. Opaque pixels
/// are just their stored values.
#[inline]
pub(crate) fn gamma_color32_to_rgba(c: Color32) -> Rgba {
    GammaReader::new().read(c)
}

/// [`gamma_color32_to_rgba`] with its table fetched once, for loops.
#[derive(Clone, Copy)]
pub(crate) struct GammaReader(&'static [u8; 256 * 256]);

impl GammaReader {
    #[inline]
    pub(crate) fn new() -> Self {
        Self(unmultiply_lut())
    }

    #[inline]
    pub(crate) fn read(self, c: Color32) -> Rgba {
        let a8 = c.a();
        if a8 == 255 || a8 == 0 {
            return Rgba::from_rgba_premultiplied(
                c.r() as f32 / 255.0,
                c.g() as f32 / 255.0,
                c.b() as f32 / 255.0,
                a8 as f32 / 255.0,
            );
        }
        let row = &self.0[(a8 as usize) << 8..][..256];
        let k = a8 as f32 / (255.0 * 255.0);
        let g = |v: u8| row[v as usize] as f32 * k;
        Rgba::from_rgba_premultiplied(g(c.r()), g(c.g()), g(c.b()), a8 as f32 / 255.0)
    }

    /// Normal-mode "over" of two stored colours, mixed as sRGB values.
    #[inline]
    pub(crate) fn over(self, src: Color32, dst: Color32) -> Color32 {
        match src.a() {
            255 => src,
            0 => dst,
            _ => {
                let s = self.read(src);
                gamma_rgba_to_color32(s + self.read(dst) * (1.0 - s.a()))
            }
        }
    }
}

/// Inverse of [`gamma_color32_to_rgba`]: gamma-premultiplied values back to
/// a stored colour, in 8 bits.
#[inline]
pub(crate) fn gamma_rgba_to_color32(c: Rgba) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let a = c.a();
    if a >= 1.0 {
        return Color32::from_rgb(q(c.r()), q(c.g()), q(c.b()));
    }
    let a8 = q(a);
    if a8 == 0 {
        return Color32::TRANSPARENT;
    }
    let inv = 1.0 / a;
    Color32::from_rgba_unmultiplied(q(c.r() * inv), q(c.g() * inv), q(c.b() * inv), a8)
}

/// Lookup-table equivalent of `Rgba::from(Color32)` (which calls `powf` per
/// channel); produces bit-identical values.
#[inline]
pub(crate) fn color32_to_linear(c: Color32) -> Rgba {
    Rgba::from_rgba_premultiplied(
        srgb_u8_to_linear(c.r()),
        srgb_u8_to_linear(c.g()),
        srgb_u8_to_linear(c.b()),
        c.a() as f32 / 255.0,
    )
}

/// Convert a whole tile to linear light, resolving the lookup table once.
pub(crate) fn color32s_to_linear(data: &[Color32]) -> Vec<Rgba> {
    let lut = srgb_to_linear_lut();
    data.iter()
        .map(|c| {
            Rgba::from_rgba_premultiplied(
                lut[c.r() as usize],
                lut[c.g() as usize],
                lut[c.b() as usize],
                c.a() as f32 / 255.0,
            )
        })
        .collect()
}

#[inline]
fn linear_to_srgb_u8(linear: f32) -> u8 {
    Luts::get().srgb(linear)
}

/// Both gamma tables, resolved once per batch instead of once per channel
/// (each accessor is a thread-local lookup).
#[derive(Clone, Copy)]
struct Luts {
    to_linear: &'static [f32; 256],
    to_srgb: &'static [u8; GAMMA_LUT_SIZE],
}

impl Luts {
    #[inline]
    fn get() -> Self {
        Self {
            to_linear: srgb_to_linear_lut(),
            to_srgb: gamma_lut(),
        }
    }

    #[inline]
    fn linear(self, v: u8) -> f32 {
        self.to_linear[v as usize]
    }

    #[inline]
    fn srgb(self, linear: f32) -> u8 {
        let clamped = linear.clamp(0.0, 1.0);
        let index = (clamped * (GAMMA_LUT_SIZE - 1) as f32 + 0.5) as usize;
        self.to_srgb[index.min(GAMMA_LUT_SIZE - 1)]
    }
}

/// Per-pixel offsets in -0.5..0.5 for dithering 8-bit alpha, by canvas
/// position. Rounding alpha to 1/255 steps is invisible in the light parts
/// of a soft edge, but composited in linear light the darkest steps are far
/// apart (alpha 254/255 of black over white is already sRGB 13), which drew
/// hard rings and flat "cubes" at the core of soft strokes. Dithering
/// spreads that rounding into invisible grain instead.
///
/// A 64×64 table repeated across the canvas: at under one 8-bit step the
/// repeat is invisible, and a lookup is cheaper than hashing every pixel.
/// Fetch it once per row, not per pixel (the `OnceLock` check isn't free).
fn dither_table() -> &'static [f32; 4096] {
    static TABLE: OnceLock<[f32; 4096]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            crate::canvas::blend_modes::pixel_noise(i as u32 % 64, i as u32 / 64) - 0.5
        })
    })
}

#[inline]
fn dither_at(table: &[f32; 4096], x: u32, y: u32) -> f32 {
    table[((y & 63) * 64 + (x & 63)) as usize]
}

/// `alpha` (0..1) to 8 bits, rounded with the dither offset `noise`.
#[inline]
fn alpha_to_u8_dithered(alpha: f32, noise: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0 + 0.5 + noise).clamp(0.0, 255.0) as u8
}

#[inline]
fn alpha_to_u8(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[inline]
pub(crate) fn rgba_to_color32_fast(rgba: Rgba) -> Color32 {
    Color32::from_rgba_premultiplied(
        linear_to_srgb_u8(rgba.r()),
        linear_to_srgb_u8(rgba.g()),
        linear_to_srgb_u8(rgba.b()),
        alpha_to_u8(rgba.a()),
    )
}

/// The brush color of a stroke, prepared for resolving stroke buffers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StrokeColor {
    /// Unpremultiplied RGB in linear light.
    linear: [f32; 3],
    /// Unpremultiplied RGB as stored (sRGB), 0..1, for gamma-space strokes.
    gamma: [f32; 3],
    /// What a fully covered pixel becomes (exactly the brush's RGB, opaque).
    opaque: Color32,
}

impl StrokeColor {
    pub(crate) fn new(color: Color32) -> Self {
        let luts = Luts::get();
        Self {
            linear: [
                luts.linear(color.r()),
                luts.linear(color.g()),
                luts.linear(color.b()),
            ],
            gamma: [color.r(), color.g(), color.b()].map(|v| v as f32 / 255.0),
            opaque: Color32::from_rgb(color.r(), color.g(), color.b()),
        }
    }
}

/// Composite a stroke of `color` over the pixels the tile had before the
/// stroke (`original`), given each pixel's accumulated stroke `coverage`
/// scaled by `cap` (the stroke opacity in wash mode, else 1). Pixels with zero
/// coverage are left untouched, so they never go through a lossy round trip.
///
/// `origin` is the canvas position of the first pixel (the slice is one
/// row): it seeds the alpha dither, see [`dither_at`].
pub(crate) fn resolve_stroke_normal(
    original: &[Color32],
    coverage: &[f32],
    out: &mut [Color32],
    color: StrokeColor,
    cap: f32,
    origin: [u32; 2],
) {
    // Kept scalar on purpose: most pixels in a dab's bounding rows have zero
    // coverage and are skipped outright, which beats resolving every pixel
    // branch-free with SIMD (measured ~1.6x slower on a real stroke).
    let luts = Luts::get();
    let dither = dither_table();
    let [cr, cg, cb] = color.linear;
    for (i, ((dst, &src), &cov)) in out.iter_mut().zip(original).zip(coverage).enumerate() {
        if cov <= 0.0 {
            continue;
        }
        let a = cov * cap;
        if a >= 1.0 {
            *dst = color.opaque;
            continue;
        }
        let keep = 1.0 - a;
        // Opaque stays exactly opaque.
        let noise = if src.a() == 255 {
            0.0
        } else {
            dither_at(dither, origin[0] + i as u32, origin[1])
        };
        *dst = Color32::from_rgba_premultiplied(
            luts.srgb(cr * a + luts.linear(src.r()) * keep),
            luts.srgb(cg * a + luts.linear(src.g()) * keep),
            luts.srgb(cb * a + luts.linear(src.b()) * keep),
            alpha_to_u8_dithered(a + src.a() as f32 / 255.0 * keep, noise),
        );
    }
}

/// Any stroke over the pixels the tile had before it: its colour
/// (`colors[i]`, unmultiplied in the document's blend space, or `color`'s
/// everywhere) blended onto them with `mode` at each pixel's `coverage` ×
/// `cap`. Slower than [`resolve_stroke_normal`]; for brushes with a blend
/// mode or colour randomness. Pixels with no coverage are left untouched.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_stroke_general(
    original: &[Color32],
    coverage: &[f32],
    colors: Option<&[[f32; 3]]>,
    out: &mut [Color32],
    color: StrokeColor,
    cap: f32,
    mode: crate::canvas::blend_modes::LayerBlend,
    space: crate::canvas::blend_modes::BlendSpace,
    origin: [u32; 2],
) {
    use crate::canvas::blend_modes::{BlendSpace, composite, pixel_noise};
    let luts = Luts::get();
    let dither = dither_table();
    let linear = space == BlendSpace::Linear;
    let base = if linear { color.linear } else { color.gamma };
    let gamma = GammaReader::new();
    for (i, ((dst, &src), &cov)) in out.iter_mut().zip(original).zip(coverage).enumerate() {
        if cov <= 0.0 {
            continue;
        }
        let a = (cov * cap).min(1.0);
        let c = colors.map_or(base, |c| c[i]);
        let stroke = Rgba::from_rgba_premultiplied(c[0] * a, c[1] * a, c[2] * a, a);
        let below = if linear {
            color32_to_linear(src)
        } else {
            gamma.read(src)
        };
        let x = origin[0] + i as u32;
        let mixed = composite(mode, stroke, below, pixel_noise(x, origin[1]));
        *dst = if linear {
            // Opaque stays exactly opaque; soft alpha is dithered (see
            // `dither_at`).
            let noise = if mixed.a() >= 1.0 {
                0.0
            } else {
                dither_at(dither, x, origin[1])
            };
            Color32::from_rgba_premultiplied(
                luts.srgb(mixed.r()),
                luts.srgb(mixed.g()),
                luts.srgb(mixed.b()),
                alpha_to_u8_dithered(mixed.a(), noise),
            )
        } else {
            gamma_rgba_to_color32(mixed)
        };
    }
}

/// [`resolve_stroke_normal`] for gamma-space documents: the brush colour
/// and the pixel below are mixed as stored sRGB values, like Photoshop and
/// Krita's 8-bit documents, instead of in linear light.
pub(crate) fn resolve_stroke_normal_gamma(
    original: &[Color32],
    coverage: &[f32],
    out: &mut [Color32],
    color: StrokeColor,
    cap: f32,
) {
    let [cr, cg, cb] = color.gamma;
    let gamma = GammaReader::new();
    for ((dst, &src), &cov) in out.iter_mut().zip(original).zip(coverage) {
        if cov <= 0.0 {
            continue;
        }
        let a = cov * cap;
        if a >= 1.0 {
            *dst = color.opaque;
            continue;
        }
        let keep = 1.0 - a;
        let below = gamma.read(src);
        *dst = gamma_rgba_to_color32(Rgba::from_rgba_premultiplied(
            cr * a + below.r() * keep,
            cg * a + below.g() * keep,
            cb * a + below.b() * keep,
            a + below.a() * keep,
        ));
    }
}

/// [`resolve_stroke_normal`] 8 pixels at a time, with identical results:
/// the blend math and gamma-table indexing are vectorised (same operations
/// in the same order), only the table lookups themselves stay scalar.
/// Callers pass just the span of each row that dabs reached, so nearly every
/// pixel is covered; that is what makes the SIMD path pay off (over whole
/// dab rectangles, full of zero-coverage pixels, it measured slower).
pub(crate) fn resolve_stroke_normal_simd(
    original: &[Color32],
    coverage: &[f32],
    out: &mut [Color32],
    color: StrokeColor,
    cap: f32,
    origin: [u32; 2],
) {
    const LANES: usize = 8;
    let luts = Luts::get();
    let dither = dither_table();
    let len = out.len().min(original.len()).min(coverage.len());
    let chunks = len / LANES;
    let [cr, cg, cb] = color.linear;
    let (vcr, vcg, vcb) = (f32x8::splat(cr), f32x8::splat(cg), f32x8::splat(cb));
    let (zero, one, half) = (f32x8::ZERO, f32x8::ONE, f32x8::splat(0.5));
    let vcap = f32x8::splat(cap);
    let lut_scale = f32x8::splat((GAMMA_LUT_SIZE - 1) as f32);
    let max_index = GAMMA_LUT_SIZE as i32 - 1;
    let to_index = |v: f32x8| {
        (v.max(zero).min(one) * lut_scale + half)
            .trunc_int()
            .to_array()
    };

    for chunk in 0..chunks {
        let base = chunk * LANES;
        let cov = f32x8::new(coverage[base..base + LANES].try_into().unwrap());
        let covered = cov.cmp_gt(zero).move_mask();
        if covered == 0 {
            continue;
        }
        let a = (cov * vcap).min(one);
        let keep = one - a;

        let src = &original[base..base + LANES];
        let (mut lr, mut lg, mut lb, mut la, mut noise) = (
            [0.0; LANES],
            [0.0; LANES],
            [0.0; LANES],
            [0.0; LANES],
            [0.0; LANES],
        );
        for k in 0..LANES {
            lr[k] = luts.linear(src[k].r());
            lg[k] = luts.linear(src[k].g());
            lb[k] = luts.linear(src[k].b());
            la[k] = src[k].a() as f32 / 255.0;
            if src[k].a() != 255 {
                noise[k] = dither_at(dither, origin[0] + (base + k) as u32, origin[1]);
            }
        }
        let r = to_index(vcr * a + f32x8::new(lr) * keep);
        let g = to_index(vcg * a + f32x8::new(lg) * keep);
        let b = to_index(vcb * a + f32x8::new(lb) * keep);
        let alpha = ((a + f32x8::new(la) * keep).max(zero).min(one) * f32x8::splat(255.0)
            + half
            + f32x8::new(noise))
        .max(zero)
        .min(f32x8::splat(255.0))
        .trunc_int()
        .to_array();
        let a = a.to_array();

        for k in 0..LANES {
            if covered & (1 << k) == 0 {
                continue; // untouched pixels keep their exact value
            }
            out[base + k] = if a[k] >= 1.0 {
                color.opaque
            } else {
                let lut = |i: i32| luts.to_srgb[i.clamp(0, max_index) as usize];
                Color32::from_rgba_premultiplied(lut(r[k]), lut(g[k]), lut(b[k]), alpha[k] as u8)
            };
        }
    }

    let tail = chunks * LANES;
    resolve_stroke_normal(
        &original[tail..len],
        &coverage[tail..len],
        &mut out[tail..len],
        color,
        cap,
        [origin[0] + tail as u32, origin[1]],
    );
}

/// Eraser counterpart of [`resolve_stroke_normal`]: scales the original
/// premultiplied pixel by the uncovered fraction. A pixel whose alpha
/// rounds to zero becomes fully transparent (black RGB), which canvas
/// storage relies on.
pub(crate) fn resolve_stroke_erase(
    original: &[Color32],
    coverage: &[f32],
    out: &mut [Color32],
    cap: f32,
    origin: [u32; 2],
) {
    let dither = dither_table();
    for (i, ((dst, &src), &cov)) in out.iter_mut().zip(original).zip(coverage).enumerate() {
        if cov <= 0.0 {
            continue;
        }
        let keep = 1.0 - (cov * cap).min(1.0);
        let scale = |v: u8| (v as f32 * keep + 0.5) as u8;
        // The alpha dithered like painting's, so soft erased edges fade
        // smoothly too.
        let noise = dither_at(dither, origin[0] + i as u32, origin[1]);
        let a = (src.a() as f32 * keep + 0.5 + noise).clamp(0.0, 255.0) as u8;
        *dst = if a == 0 {
            Color32::TRANSPARENT
        } else {
            Color32::from_rgba_premultiplied(scale(src.r()), scale(src.g()), scale(src.b()), a)
        };
    }
}

/// Plain unrolled stand-in for `[T; 4]::map`: the std version goes through
/// the same `try_from_fn`/`NeverShortCircuit` machinery flagged below and
/// still cost ~12% inclusive in profiling even after `pack_colors_x4` was
/// fixed, since every SIMD lane-extraction below also calls `.map`.
#[inline]
fn map4<T: Copy, U>(arr: [T; 4], mut f: impl FnMut(T) -> U) -> [U; 4] {
    [f(arr[0]), f(arr[1]), f(arr[2]), f(arr[3])]
}

/// Packs 4 lanes of linear r/g/b/a into `Color32`s. Deliberately a plain
/// unrolled array literal rather than `std::array::from_fn`: the latter goes
/// through `core::array::try_from_fn`'s `Try`/`NeverShortCircuit` machinery,
/// which showed up as the dominant cost (>60% inclusive) once this hot path
/// was profiled at scale — a manual literal has no such indirection to inline.
#[inline]
fn pack_colors_x4(luts: Luts, r: [f32; 4], g: [f32; 4], b: [f32; 4], a: [f32; 4]) -> [Color32; 4] {
    [
        Color32::from_rgba_premultiplied(
            luts.srgb(r[0]),
            luts.srgb(g[0]),
            luts.srgb(b[0]),
            alpha_to_u8(a[0]),
        ),
        Color32::from_rgba_premultiplied(
            luts.srgb(r[1]),
            luts.srgb(g[1]),
            luts.srgb(b[1]),
            alpha_to_u8(a[1]),
        ),
        Color32::from_rgba_premultiplied(
            luts.srgb(r[2]),
            luts.srgb(g[2]),
            luts.srgb(b[2]),
            alpha_to_u8(a[2]),
        ),
        Color32::from_rgba_premultiplied(
            luts.srgb(r[3]),
            luts.srgb(g[3]),
            luts.srgb(b[3]),
            alpha_to_u8(a[3]),
        ),
    ]
}

#[inline]
fn alpha_over_x4(luts: Luts, src: [Color32; 4], dst: [Color32; 4]) -> [Color32; 4] {
    let sr = f32x4::new(map4(src, |c| luts.linear(c.r())));
    let sg = f32x4::new(map4(src, |c| luts.linear(c.g())));
    let sb = f32x4::new(map4(src, |c| luts.linear(c.b())));
    let sa = f32x4::new(map4(src, |c| c.a() as f32 / 255.0));
    let dr = f32x4::new(map4(dst, |c| luts.linear(c.r())));
    let dg = f32x4::new(map4(dst, |c| luts.linear(c.g())));
    let db = f32x4::new(map4(dst, |c| luts.linear(c.b())));
    let inv_alpha = f32x4::splat(1.0) - sa;
    let r = (sr + dr * inv_alpha).to_array();
    let g = (sg + dg * inv_alpha).to_array();
    let b = (sb + db * inv_alpha).to_array();
    let da = f32x4::new(map4(dst, |c| c.a() as f32 / 255.0));
    let a = (sa + da * inv_alpha).to_array();

    pack_colors_x4(luts, r, g, b, a)
}

#[inline]
pub fn alpha_over_batch(src: &[Color32], dst: &[Color32], out: &mut [Color32]) {
    assert_eq!(src.len(), dst.len());
    assert_eq!(src.len(), out.len());
    let luts = Luts::get();

    let simd_len = src.len() / 4 * 4;
    let mut i = 0;
    while i < simd_len {
        let blended = alpha_over_x4(
            luts,
            [src[i], src[i + 1], src[i + 2], src[i + 3]],
            [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]],
        );
        out[i] = blended[0];
        out[i + 1] = blended[1];
        out[i + 2] = blended[2];
        out[i + 3] = blended[3];
        i += 4;
    }

    for i in simd_len..src.len() {
        out[i] = alpha_over(src[i], dst[i]);
    }
}

/// `Color32::to_srgba_unmultiplied`, from a table: egui's version runs
/// `powf` three times per channel. Each output channel depends only on the
/// channel and alpha, so a 256×256 table built with egui's own function is
/// exact.
#[inline]
pub fn unmultiply(c: Color32) -> [u8; 4] {
    use std::sync::OnceLock;
    let a = c.a();
    if a == 255 {
        return [c.r(), c.g(), c.b(), 255];
    }
    static LUT: OnceLock<Vec<u8>> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        let mut t = vec![0u8; 256 * 256];
        for alpha in 0..=255u8 {
            for v in 0..=255u8 {
                t[alpha as usize * 256 + v as usize] =
                    Color32::from_rgba_premultiplied(v, v, v, alpha).to_srgba_unmultiplied()[0];
            }
        }
        t
    });
    let row = &lut[a as usize * 256..][..256];
    [
        row[c.r() as usize],
        row[c.g() as usize],
        row[c.b() as usize],
        a,
    ]
}

/// `c` with its alpha replaced by `alpha`, keeping its (unpremultiplied)
/// colour: how alpha-locked layers take paint.
#[inline]
pub(crate) fn with_alpha_of(c: Color32, alpha: u8) -> Color32 {
    if alpha == 0 {
        return Color32::TRANSPARENT;
    }
    if c.a() == alpha {
        return c;
    }
    if c.a() == 0 {
        // Fully erased: nothing to recolour with; keep it transparent-black.
        return Color32::from_rgba_premultiplied(0, 0, 0, alpha);
    }
    let k = alpha as u32;
    let a = c.a() as u32;
    let scale = |v: u8| ((v as u32 * k + a / 2) / a).min(k) as u8;
    Color32::from_rgba_premultiplied(scale(c.r()), scale(c.g()), scale(c.b()), alpha)
}

#[inline]
pub fn alpha_over(src: Color32, dst: Color32) -> Color32 {
    match src.a() {
        0 => return dst,
        255 => return src,
        _ => {}
    }
    if dst.a() == 0 {
        return src;
    }

    let src_a = src.a() as f32 / 255.0;
    let inv_alpha = 1.0 - src_a;

    let out_r = srgb_u8_to_linear(src.r()) + srgb_u8_to_linear(dst.r()) * inv_alpha;
    let out_g = srgb_u8_to_linear(src.g()) + srgb_u8_to_linear(dst.g()) * inv_alpha;
    let out_b = srgb_u8_to_linear(src.b()) + srgb_u8_to_linear(dst.b()) * inv_alpha;
    let out_a = if dst.a() == 255 {
        255
    } else {
        alpha_to_u8(src_a + (dst.a() as f32 / 255.0) * inv_alpha)
    };

    Color32::from_rgba_premultiplied(
        linear_to_srgb_u8(out_r),
        linear_to_srgb_u8(out_g),
        linear_to_srgb_u8(out_b),
        out_a,
    )
}

#[inline]
pub(crate) fn apply_opacity_scale(color: Color32, opacity_scale: f32) -> Color32 {
    if opacity_scale >= 1.0 {
        return color;
    }
    if opacity_scale <= 0.0 {
        return Color32::TRANSPARENT;
    }

    Color32::from(Rgba::from(color) * opacity_scale)
}

#[cfg(test)]
mod unmultiply_tests {
    use super::*;

    #[test]
    fn table_unmultiply_matches_egui_exactly() {
        for a in 0..=255u8 {
            for v in (0..=a).step_by(3) {
                for c in [
                    Color32::from_rgba_premultiplied(v, a / 2, 0, a),
                    Color32::from_rgba_premultiplied(0, v, a, a),
                ] {
                    assert_eq!(unmultiply(c), c.to_srgba_unmultiplied(), "{c:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_blending_reads_and_writes_the_stored_format() {
        // Stored pixels are egui's (premultiplied in linear light); gamma
        // blending sees them as sRGB values times alpha.
        for a in [1u8, 3, 64, 128, 200, 254, 255] {
            for v in [0u8, 1, 40, 128, 200, 255] {
                let stored = Color32::from_rgba_unmultiplied(v, v, v, a);
                let g = gamma_color32_to_rgba(stored);
                let want = v as f32 / 255.0 * a as f32 / 255.0;
                // Low alpha keeps few distinct values, like any 8-bit storage.
                let tolerance = if a < 16 { 0.02 } else { 1.5 / 255.0 };
                assert!(
                    (g.r() - want).abs() <= tolerance,
                    "v {v} a {a}: {} vs {want}",
                    g.r()
                );
                let back = gamma_rgba_to_color32(g);
                assert!(
                    back.a() == a && back.r().abs_diff(stored.r()) <= 1,
                    "v {v} a {a}: {back:?} vs {stored:?}"
                );
            }
        }
        assert_eq!(gamma_color32_to_rgba(Color32::TRANSPARENT).a(), 0.0);
        assert_eq!(
            gamma_rgba_to_color32(Rgba::TRANSPARENT),
            Color32::TRANSPARENT
        );
    }

    #[test]
    fn a_soft_gamma_stroke_exports_its_own_colour() {
        // Grey at half coverage on a transparent layer: unmultiplied, it's
        // still the brush's grey (it used to read as a darker 90).
        let mut out = [Color32::TRANSPARENT; 1];
        resolve_stroke_normal_gamma(
            &[Color32::TRANSPARENT],
            &[128.0 / 255.0],
            &mut out,
            StrokeColor::new(Color32::from_gray(128)),
            1.0,
        );
        let [r, g, b, a] = out[0].to_srgba_unmultiplied();
        assert_eq!(a, 128);
        for c in [r, g, b] {
            assert!(c.abs_diff(128) <= 1, "{:?}", out[0].to_srgba_unmultiplied());
        }
    }

    fn reference_alpha_over(src: Color32, dst: Color32) -> Color32 {
        let src_l = Rgba::from(src);
        let dst_l = Rgba::from(dst);
        let inv_alpha = 1.0 - src_l.a();

        rgba_to_color32_fast(Rgba::from_rgba_premultiplied(
            src_l.r() + dst_l.r() * inv_alpha,
            src_l.g() + dst_l.g() * inv_alpha,
            src_l.b() + dst_l.b() * inv_alpha,
            src_l.a() + dst_l.a() * inv_alpha,
        ))
    }

    fn assert_color_close(actual: Color32, expected: Color32) {
        for (actual, expected) in actual.to_array().into_iter().zip(expected.to_array()) {
            assert!((actual as i16 - expected as i16).abs() <= 1);
        }
    }

    #[test]
    fn lut_linear_conversion_matches_egui_bit_for_bit() {
        for i in 0..=255u8 {
            let c = Color32::from_rgba_premultiplied(i, 255 - i, i / 2, i);
            assert_eq!(color32_to_linear(c), Rgba::from(c));
            assert_eq!(color32s_to_linear(&[c])[0], Rgba::from(c));
        }
    }

    #[test]
    fn lut_alpha_over_matches_rgba_reference() {
        let cases = [
            (
                Color32::from_rgba_unmultiplied(255, 128, 64, 80),
                Color32::from_rgba_unmultiplied(64, 100, 255, 180),
            ),
            (
                Color32::from_rgba_unmultiplied(20, 220, 90, 170),
                Color32::from_rgba_unmultiplied(240, 30, 160, 90),
            ),
            (
                Color32::from_rgba_unmultiplied(0, 0, 0, 0),
                Color32::from_rgba_unmultiplied(90, 80, 70, 120),
            ),
        ];

        for (src, dst) in cases {
            assert_color_close(alpha_over(src, dst), reference_alpha_over(src, dst));
        }
    }

    #[test]
    fn batch_alpha_over_matches_scalar_alpha_over() {
        // 5 pixels: exercises both the alpha_over_x4 SIMD path (first 4)
        // and the scalar remainder loop (5th), guarding the alpha_over_x4
        // array-indexing -> array::map/from_fn refactor above.
        let src = [
            Color32::from_rgba_unmultiplied(255, 128, 64, 80),
            Color32::from_rgba_unmultiplied(20, 220, 90, 170),
            Color32::from_rgba_unmultiplied(0, 0, 0, 0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 255),
            Color32::from_rgba_unmultiplied(10, 200, 30, 40),
        ];
        let dst = [
            Color32::from_rgba_unmultiplied(64, 100, 255, 180),
            Color32::from_rgba_unmultiplied(240, 30, 160, 90),
            Color32::from_rgba_unmultiplied(90, 80, 70, 120),
            Color32::from_rgba_unmultiplied(0, 0, 0, 255),
            Color32::from_rgba_unmultiplied(200, 200, 200, 200),
        ];
        let mut out = [Color32::TRANSPARENT; 5];
        alpha_over_batch(&src, &dst, &mut out);

        for i in 0..5 {
            assert_color_close(out[i], alpha_over(src[i], dst[i]));
        }
    }

    #[test]
    fn simd_resolve_matches_scalar_exactly() {
        // Deterministic pseudo-random pixels and coverages, including zero
        // coverage, full coverage, and caps that saturate.
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for len in [0, 1, 7, 8, 9, 16, 23, 64, 100] {
            for &cap in &[1.0, 0.35, 2.0] {
                let original: Vec<Color32> = (0..len)
                    .map(|_| {
                        let v = next();
                        let a = (v >> 24) as u8;
                        let pm = |c: u32| ((c & 0xff) * a as u32 / 255) as u8;
                        Color32::from_rgba_premultiplied(pm(v), pm(v >> 8), pm(v >> 16), a)
                    })
                    .collect();
                let coverage: Vec<f32> = (0..len)
                    .map(|_| match next() % 5 {
                        0 => 0.0,
                        1 => 1.0,
                        _ => (next() % 1000) as f32 / 1000.0,
                    })
                    .collect();
                let color = StrokeColor::new(Color32::from_rgba_unmultiplied(
                    (next() & 0xff) as u8,
                    (next() & 0xff) as u8,
                    (next() & 0xff) as u8,
                    255,
                ));
                let mut scalar = original.clone();
                let mut simd = original.clone();
                resolve_stroke_normal(&original, &coverage, &mut scalar, color, cap, [37, 11]);
                resolve_stroke_normal_simd(&original, &coverage, &mut simd, color, cap, [37, 11]);
                assert_eq!(scalar, simd, "len {len}, cap {cap}");
            }
        }
    }

    #[test]
    fn soft_edges_have_no_alpha_steps_near_opaque() {
        // Black at 99.6% coverage on a transparent layer: plain rounding
        // made every pixel alpha 254 (a flat band, sRGB 13 over white next
        // to 0). Dithered, the row averages to the true value instead.
        let n = 512;
        let original = vec![Color32::TRANSPARENT; n];
        let mut out = original.clone();
        let cov = vec![0.996f32; n];
        resolve_stroke_normal_simd(
            &original,
            &cov,
            &mut out,
            StrokeColor::new(Color32::BLACK),
            1.0,
            [0, 5],
        );
        let mean = out.iter().map(|p| p.a() as f32).sum::<f32>() / n as f32;
        assert!((mean - 0.996 * 255.0).abs() < 0.2, "{mean}");
        assert!(out.iter().any(|p| p.a() == 253) && out.iter().any(|p| p.a() == 254));
        // Paint over opaque pixels stays exactly opaque.
        let opaque = vec![Color32::WHITE; n];
        let mut out = opaque.clone();
        let cov = vec![0.4f32; n];
        resolve_stroke_normal_simd(
            &opaque,
            &cov,
            &mut out,
            StrokeColor::new(Color32::BLACK),
            1.0,
            [0, 5],
        );
        assert!(out.iter().all(|p| p.a() == 255));
    }

    #[test]
    fn resolve_erase_zero_alpha_implies_zero_rgb() {
        // A low-alpha, high-value pixel whose alpha rounds to 0 must become
        // fully transparent, not keep a stray color value.
        let original = [Color32::from_rgba_premultiplied(128, 200, 255, 1)];
        let mut out = original;
        resolve_stroke_erase(&original, &[0.6], &mut out, 1.0, [0, 0]);
        assert_eq!(out[0], Color32::TRANSPARENT);
    }

    #[test]
    fn resolve_normal_full_coverage_is_the_exact_brush_color() {
        for v in 0..=255u8 {
            let color = Color32::from_rgb(v, 255 - v, v / 3);
            let original = [Color32::from_rgba_premultiplied(10, 20, 30, 200)];
            let mut out = original;
            resolve_stroke_normal(
                &original,
                &[1.0],
                &mut out,
                StrokeColor::new(color),
                1.0,
                [0, 0],
            );
            assert_eq!(out[0], color);
        }
    }

    #[test]
    fn resolve_leaves_uncovered_pixels_untouched() {
        let original = [Color32::from_rgba_premultiplied(3, 7, 11, 13)];
        let mut out = [Color32::from_rgba_premultiplied(99, 99, 99, 99)];
        resolve_stroke_normal(
            &original,
            &[0.0],
            &mut out,
            StrokeColor::new(Color32::RED),
            1.0,
            [0, 0],
        );
        resolve_stroke_erase(&original, &[0.0], &mut out, 1.0, [0, 0]);
        assert_eq!(out[0], Color32::from_rgba_premultiplied(99, 99, 99, 99));
    }
}
