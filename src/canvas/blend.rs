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

pub fn blend_erase(src: Color32, dst: Color32) -> Color32 {
    let src_a = src.a() as u32;
    let inv = 255 - src_a;
    let out_a = (dst.a() as u32 * inv + 127) / 255;
    if out_a == 0 {
        // Each channel rounds independently, so a low-alpha, high-value pixel can
        // round alpha down to 0 while a color channel rounds down to 1 instead of
        // 0. Canvas storage relies on alpha==0 implying black premultiplied RGB.
        return Color32::TRANSPARENT;
    }
    let out_r = (dst.r() as u32 * inv + 127) / 255;
    let out_g = (dst.g() as u32 * inv + 127) / 255;
    let out_b = (dst.b() as u32 * inv + 127) / 255;
    Color32::from_rgba_premultiplied(
        out_r.min(255) as u8,
        out_g.min(255) as u8,
        out_b.min(255) as u8,
        out_a.min(255) as u8,
    )
}

#[derive(Clone, Debug)]
pub struct LinearBrushColor {
    r: [f32; 256],
    g: [f32; 256],
    b: [f32; 256],
    a: [f32; 256],
    premul: [Color32; 256],
    is_black: bool,
}

impl LinearBrushColor {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        let mut color = Self {
            r: [0.0; 256],
            g: [0.0; 256],
            b: [0.0; 256],
            a: [0.0; 256],
            premul: [Color32::TRANSPARENT; 256],
            is_black: r == 0 && g == 0 && b == 0,
        };

        for alpha in 0..=255 {
            let premul = Color32::from_rgba_unmultiplied(r, g, b, alpha as u8);
            color.r[alpha] = srgb_u8_to_linear(premul.r());
            color.g[alpha] = srgb_u8_to_linear(premul.g());
            color.b[alpha] = srgb_u8_to_linear(premul.b());
            color.a[alpha] = alpha as f32 / 255.0;
            color.premul[alpha] = premul;
        }

        color
    }
}

#[inline]
pub fn alpha_over_brush(src: &LinearBrushColor, alpha: u8, dst: Color32) -> Color32 {
    match alpha {
        0 => return dst,
        255 => return src.premul[255],
        _ => {}
    }
    if dst.a() == 0 {
        return src.premul[alpha as usize];
    }

    let idx = alpha as usize;
    let src_a = src.a[idx];
    let inv_alpha = 1.0 - src_a;

    let dst_r = srgb_u8_to_linear(dst.r()) * inv_alpha;
    let dst_g = srgb_u8_to_linear(dst.g()) * inv_alpha;
    let dst_b = srgb_u8_to_linear(dst.b()) * inv_alpha;

    let (out_r, out_g, out_b) = if src.is_black {
        (dst_r, dst_g, dst_b)
    } else {
        (src.r[idx] + dst_r, src.g[idx] + dst_g, src.b[idx] + dst_b)
    };

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
fn alpha_over_brush_x4(luts: Luts, src: &LinearBrushColor, alpha: [u8; 4], dst: [Color32; 4]) -> [Color32; 4] {
    let idx = map4(alpha, |a| a as usize);
    let src_a = f32x4::new(map4(idx, |i| src.a[i]));
    let inv_alpha = f32x4::splat(1.0) - src_a;

    let dst_r = f32x4::new(map4(dst, |c| luts.linear(c.r()))) * inv_alpha;
    let dst_g = f32x4::new(map4(dst, |c| luts.linear(c.g()))) * inv_alpha;
    let dst_b = f32x4::new(map4(dst, |c| luts.linear(c.b()))) * inv_alpha;

    let (out_r, out_g, out_b) = if src.is_black {
        (dst_r, dst_g, dst_b)
    } else {
        let src_r = f32x4::new(map4(idx, |i| src.r[i]));
        let src_g = f32x4::new(map4(idx, |i| src.g[i]));
        let src_b = f32x4::new(map4(idx, |i| src.b[i]));
        (src_r + dst_r, src_g + dst_g, src_b + dst_b)
    };

    let dst_a = f32x4::new(map4(dst, |c| c.a() as f32 / 255.0));
    let out_a = (src_a + dst_a * inv_alpha).to_array();

    pack_colors_x4(luts, out_r.to_array(), out_g.to_array(), out_b.to_array(), out_a)
}

/// Batched form of [`alpha_over_brush`] for a run of pixels sharing one brush
/// color (e.g. a contiguous mask span). Matches the scalar function within
/// LUT rounding (~1/255) since it always takes the general blend path instead
/// of the alpha==0/255/dst-transparent shortcuts — the same tradeoff already
/// accepted by `alpha_over_batch` below.
#[inline]
pub fn alpha_over_brush_batch(src: &LinearBrushColor, alphas: &[u8], dst: &mut [Color32]) {
    assert_eq!(alphas.len(), dst.len());
    let luts = Luts::get();

    let simd_len = dst.len() / 4 * 4;
    let mut i = 0;
    while i < simd_len {
        let a = [alphas[i], alphas[i + 1], alphas[i + 2], alphas[i + 3]];
        let d = [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]];
        let blended = alpha_over_brush_x4(luts, src, a, d);
        dst[i..i + 4].copy_from_slice(&blended);
        i += 4;
    }

    for i in simd_len..dst.len() {
        dst[i] = alpha_over_brush(src, alphas[i], dst[i]);
    }
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
    fn blend_erase_zero_alpha_implies_zero_rgb() {
        // src_a=254 -> inv=1. dst.a()=1 rounds to out_a=(1*1+127)/255=0, but
        // out_r for dst.r()=128 would independently round to 1 without the
        // early-out, violating "alpha==0 implies premultiplied RGB==0".
        let src = Color32::from_rgba_premultiplied(0, 0, 0, 254);
        let dst = Color32::from_rgba_premultiplied(128, 200, 255, 1);
        assert_eq!(blend_erase(src, dst), Color32::TRANSPARENT);
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
    fn brush_alpha_over_matches_regular_alpha_over() {
        let cases = [
            (
                LinearBrushColor::new(240, 120, 30),
                (240, 120, 30),
                Color32::from_rgba_unmultiplied(40, 80, 220, 180),
            ),
            (
                LinearBrushColor::new(0, 0, 0),
                (0, 0, 0),
                Color32::from_rgba_unmultiplied(240, 240, 240, 255),
            ),
        ];

        for (brush, (r, g, b), dst) in cases {
            for alpha in [0, 1, 80, 170, 254, 255] {
                let src = Color32::from_rgba_unmultiplied(r, g, b, alpha);
                assert_color_close(alpha_over_brush(&brush, alpha, dst), alpha_over(src, dst));
            }
        }
    }

    #[test]
    fn batch_alpha_over_brush_matches_scalar_alpha_over_brush() {
        // 5 pixels covering the alpha==0, alpha==255 and dst.a()==0 shortcuts
        // that alpha_over_brush special-cases and alpha_over_brush_x4 folds
        // into the general formula instead: exercises both the SIMD path
        // (first 4) and the scalar remainder (5th).
        let brush = LinearBrushColor::new(240, 120, 30);
        let alphas = [0u8, 255, 170, 80, 40];
        let dst = [
            Color32::from_rgba_unmultiplied(64, 100, 255, 180),
            Color32::from_rgba_unmultiplied(240, 30, 160, 90),
            Color32::from_rgba_unmultiplied(0, 0, 0, 0),
            Color32::from_rgba_unmultiplied(0, 0, 0, 255),
            Color32::from_rgba_unmultiplied(200, 200, 200, 200),
        ];
        let mut out = dst;
        alpha_over_brush_batch(&brush, &alphas, &mut out);

        for i in 0..5 {
            assert_color_close(out[i], alpha_over_brush(&brush, alphas[i], dst[i]));
        }
    }
}
