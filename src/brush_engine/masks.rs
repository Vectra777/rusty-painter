//! Tip alpha for round and square tips (Gaussian or curve falloff); image
//! tips sample their [`crate::brush_engine::tip::TipMask`].

use super::{
    brush_options::PixelBrushShape,
    hardness::{SoftnessCurve, SoftnessSelector},
};

/// Smoothstep falloff used by the Gaussian softness curve: 1.0 inside the
/// hardness radius, smoothly falling to 0.0 at the brush edge.
#[inline]
pub(crate) fn gaussian_falloff(t: f32, hardness_val: f32) -> f32 {
    if t < hardness_val || hardness_val >= 1.0 {
        1.0
    } else {
        let v = (t - hardness_val) / (1.0 - hardness_val);
        let falloff = 1.0 - v.clamp(0.0, 1.0);
        let f2 = falloff * falloff;
        f2 * (3.0 - 2.0 * falloff)
    }
}

pub(super) fn calc_soft_brush_alpha(
    dx: f32,
    dy: f32,
    radius: f32,
    shape: &PixelBrushShape,
    hardness_val: f32,
    softness_selector: SoftnessSelector,
    softness_curve: &SoftnessCurve,
) -> (f32, f32) {
    match shape {
        PixelBrushShape::Circle => {
            let dist_sq = dx * dx + dy * dy;
            let r_sq = radius * radius;
            if dist_sq >= r_sq {
                (0.0, dist_sq)
            } else {
                let dist = dist_sq.sqrt();
                let t = dist / radius;
                let alpha = match softness_selector {
                    SoftnessSelector::Gaussian => gaussian_falloff(t, hardness_val),
                    SoftnessSelector::Curve => softness_curve.eval(t),
                };
                (alpha, dist_sq)
            }
        }
        PixelBrushShape::Square => {
            let dist_x = dx.abs();
            let dist_y = dy.abs();
            let dist = dist_x.max(dist_y);
            let dist_sq = dist * dist;
            let t = dist / radius;
            if dist >= radius {
                (0.0, dist_sq)
            } else {
                let alpha = match softness_selector {
                    SoftnessSelector::Gaussian => gaussian_falloff(t, hardness_val),
                    SoftnessSelector::Curve => softness_curve.eval(t),
                };
                (alpha, dist_sq)
            }
        }
        PixelBrushShape::Custom(tip) => (tip.sample(dx, dy, radius), dx * dx + dy * dy),
    }
}
