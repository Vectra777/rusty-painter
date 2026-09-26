use std::cell::Cell;
use std::sync::OnceLock;

use eframe::egui::{Color32, Rgba};
use wide::f32x4;

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
    /// What a fully covered pixel becomes (exactly the brush's RGB, opaque).
    opaque: Color32,
}

impl StrokeColor {
    pub(crate) fn new(color: Color32) -> Self {
        let luts = Luts::get();
        Self {
            linear: [luts.linear(color.r()), luts.linear(color.g()), luts.linear(color.b())],
            opaque: Color32::from_rgb(color.r(), color.g(), color.b()),
        }
    }
}

/// Composite a stroke of `color` over the pixels the tile had before the
/// stroke (`original`), given each pixel's accumulated stroke `coverage`
/// scaled by `cap` (the stroke opacity in wash mode, else 1). Pixels with zero
/// coverage are left untouched, so they never go through a lossy round trip.
pub(crate) fn resolve_stroke_normal(
    original: &[Color32],
    coverage: &[f32],
    out: &mut [Color32],
    color: StrokeColor,
    cap: f32,
) {
    // Kept scalar on purpose: most pixels in a dab's bounding rows have zero
    // coverage and are skipped outright, which beats resolving every pixel
    // branch-free with SIMD (measured ~1.6x slower on a real stroke).
    let luts = Luts::get();
    let [cr, cg, cb] = color.linear;
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
        *dst = Color32::from_rgba_premultiplied(
            luts.srgb(cr * a + luts.linear(src.r()) * keep),
            luts.srgb(cg * a + luts.linear(src.g()) * keep),
            luts.srgb(cb * a + luts.linear(src.b()) * keep),
            alpha_to_u8(a + src.a() as f32 / 255.0 * keep),
        );
    }
}

/// Eraser counterpart of [`resolve_stroke_normal`]: scales the original
/// premultiplied pixel by the uncovered fraction. A pixel whose alpha
/// rounds to zero becomes fully transparent (black RGB), which canvas
/// storage relies on.
pub(crate) fn resolve_stroke_erase(original: &[Color32], coverage: &[f32], out: &mut [Color32], cap: f32) {
    for ((dst, &src), &cov) in out.iter_mut().zip(original).zip(coverage) {
        if cov <= 0.0 {
            continue;
        }
        let keep = 1.0 - (cov * cap).min(1.0);
        let scale = |v: u8| (v as f32 * keep + 0.5) as u8;
        let a = scale(src.a());
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

pub(crate) fn premultiply(color: Color32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    let linear = Rgba::from_rgba_unmultiplied(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    );
    Color32::from(linear)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn resolve_erase_zero_alpha_implies_zero_rgb() {
        // A low-alpha, high-value pixel whose alpha rounds to 0 must become
        // fully transparent, not keep a stray color value.
        let original = [Color32::from_rgba_premultiplied(128, 200, 255, 1)];
        let mut out = original;
        resolve_stroke_erase(&original, &[0.6], &mut out, 1.0);
        assert_eq!(out[0], Color32::TRANSPARENT);
    }

    #[test]
    fn resolve_normal_full_coverage_is_the_exact_brush_color() {
        for v in 0..=255u8 {
            let color = Color32::from_rgb(v, 255 - v, v / 3);
            let original = [Color32::from_rgba_premultiplied(10, 20, 30, 200)];
            let mut out = original;
            resolve_stroke_normal(&original, &[1.0], &mut out, StrokeColor::new(color), 1.0);
            assert_eq!(out[0], color);
        }
    }

    #[test]
    fn resolve_leaves_uncovered_pixels_untouched() {
        let original = [Color32::from_rgba_premultiplied(3, 7, 11, 13)];
        let mut out = [Color32::from_rgba_premultiplied(99, 99, 99, 99)];
        resolve_stroke_normal(&original, &[0.0], &mut out, StrokeColor::new(Color32::RED), 1.0);
        resolve_stroke_erase(&original, &[0.0], &mut out, 1.0);
        assert_eq!(out[0], Color32::from_rgba_premultiplied(99, 99, 99, 99));
    }
}
