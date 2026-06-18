use std::sync::OnceLock;

use eframe::egui::{Color32, Rgba};
use wide::f32x4;

const GAMMA_LUT_SIZE: usize = 4096;

static GAMMA_LUT: OnceLock<[u8; GAMMA_LUT_SIZE]> = OnceLock::new();
static SRGB_TO_LINEAR_LUT: OnceLock<[f32; 256]> = OnceLock::new();

fn gamma_lut() -> &'static [u8; GAMMA_LUT_SIZE] {
    GAMMA_LUT.get_or_init(|| {
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
    })
}

fn srgb_to_linear_lut() -> &'static [f32; 256] {
    SRGB_TO_LINEAR_LUT.get_or_init(|| {
        let mut lut = [0.0; 256];
        for (i, item) in lut.iter_mut().enumerate() {
            let srgb = i as f32 / 255.0;
            *item = if srgb <= 0.04045 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            };
        }
        lut
    })
}

#[inline]
fn srgb_u8_to_linear(v: u8) -> f32 {
    srgb_to_linear_lut()[v as usize]
}

#[inline]
fn linear_to_srgb_u8(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let index = (clamped * (GAMMA_LUT_SIZE - 1) as f32 + 0.5) as usize;
    gamma_lut()[index.min(GAMMA_LUT_SIZE - 1)]
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

#[derive(Clone)]
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

#[inline]
fn alpha_over_x4(src: [Color32; 4], dst: [Color32; 4]) -> [Color32; 4] {
    let sr = f32x4::new([
        srgb_u8_to_linear(src[0].r()),
        srgb_u8_to_linear(src[1].r()),
        srgb_u8_to_linear(src[2].r()),
        srgb_u8_to_linear(src[3].r()),
    ]);
    let sg = f32x4::new([
        srgb_u8_to_linear(src[0].g()),
        srgb_u8_to_linear(src[1].g()),
        srgb_u8_to_linear(src[2].g()),
        srgb_u8_to_linear(src[3].g()),
    ]);
    let sb = f32x4::new([
        srgb_u8_to_linear(src[0].b()),
        srgb_u8_to_linear(src[1].b()),
        srgb_u8_to_linear(src[2].b()),
        srgb_u8_to_linear(src[3].b()),
    ]);
    let sa = f32x4::new([
        src[0].a() as f32 / 255.0,
        src[1].a() as f32 / 255.0,
        src[2].a() as f32 / 255.0,
        src[3].a() as f32 / 255.0,
    ]);
    let dr = f32x4::new([
        srgb_u8_to_linear(dst[0].r()),
        srgb_u8_to_linear(dst[1].r()),
        srgb_u8_to_linear(dst[2].r()),
        srgb_u8_to_linear(dst[3].r()),
    ]);
    let dg = f32x4::new([
        srgb_u8_to_linear(dst[0].g()),
        srgb_u8_to_linear(dst[1].g()),
        srgb_u8_to_linear(dst[2].g()),
        srgb_u8_to_linear(dst[3].g()),
    ]);
    let db = f32x4::new([
        srgb_u8_to_linear(dst[0].b()),
        srgb_u8_to_linear(dst[1].b()),
        srgb_u8_to_linear(dst[2].b()),
        srgb_u8_to_linear(dst[3].b()),
    ]);
    let inv_alpha = f32x4::splat(1.0) - sa;
    let r = (sr + dr * inv_alpha).to_array();
    let g = (sg + dg * inv_alpha).to_array();
    let b = (sb + db * inv_alpha).to_array();
    let da = f32x4::new([
        dst[0].a() as f32 / 255.0,
        dst[1].a() as f32 / 255.0,
        dst[2].a() as f32 / 255.0,
        dst[3].a() as f32 / 255.0,
    ]);
    let a = (sa + da * inv_alpha).to_array();

    [
        Color32::from_rgba_premultiplied(
            linear_to_srgb_u8(r[0]),
            linear_to_srgb_u8(g[0]),
            linear_to_srgb_u8(b[0]),
            alpha_to_u8(a[0]),
        ),
        Color32::from_rgba_premultiplied(
            linear_to_srgb_u8(r[1]),
            linear_to_srgb_u8(g[1]),
            linear_to_srgb_u8(b[1]),
            alpha_to_u8(a[1]),
        ),
        Color32::from_rgba_premultiplied(
            linear_to_srgb_u8(r[2]),
            linear_to_srgb_u8(g[2]),
            linear_to_srgb_u8(b[2]),
            alpha_to_u8(a[2]),
        ),
        Color32::from_rgba_premultiplied(
            linear_to_srgb_u8(r[3]),
            linear_to_srgb_u8(g[3]),
            linear_to_srgb_u8(b[3]),
            alpha_to_u8(a[3]),
        ),
    ]
}

#[inline]
pub fn alpha_over_batch(src: &[Color32], dst: &[Color32], out: &mut [Color32]) {
    assert_eq!(src.len(), dst.len());
    assert_eq!(src.len(), out.len());

    let simd_len = src.len() / 4 * 4;
    let mut i = 0;
    while i < simd_len {
        let blended = alpha_over_x4(
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
}
