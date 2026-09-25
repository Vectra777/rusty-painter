use super::{
    brush_options::PixelBrushShape,
    hardness::{SoftnessCurve, SoftnessSelector},
};

#[inline]
pub(super) fn sample_custom_mask_nn(
    dx: f32,
    dy: f32,
    diameter: f32,
    width: usize,
    height: usize,
    mask: &[u8],
) -> (bool, f32) {
    if width == 0 || height == 0 || mask.is_empty() || diameter <= 0.0 {
        return (false, 0.0);
    }
    let r = diameter / 2.0;
    let nx = (dx + r) / diameter;
    let ny = (dy + r) / diameter;

    if (0.0..1.0).contains(&nx) && (0.0..1.0).contains(&ny) {
        let ix = (nx * width as f32).floor() as usize;
        let iy = (ny * height as f32).floor() as usize;
        let idx = iy * width + ix;
        if idx < mask.len() {
            let val = mask[idx];
            return (val > 0, val as f32 / 255.0);
        }
    }
    (false, 0.0)
}

#[inline]
fn sample_custom_mask_bilinear(
    dx: f32,
    dy: f32,
    radius: f32,
    width: usize,
    height: usize,
    data: &[u8],
) -> f32 {
    if width == 0 || height == 0 || data.is_empty() || radius <= 0.0 {
        return 0.0;
    }
    let nx = (dx + radius) / (radius * 2.0);
    let ny = (dy + radius) / (radius * 2.0);

    if (0.0..1.0).contains(&nx) && (0.0..1.0).contains(&ny) {
        let tx = nx * (width as f32);
        let ty = ny * (height as f32);

        let x0 = tx.floor() as usize;
        let y0 = ty.floor() as usize;
        let x1 = (x0 + 1).min(width - 1);
        let y1 = (y0 + 1).min(height - 1);

        let fx = tx - x0 as f32;
        let fy = ty - y0 as f32;

        let get_pixel = |x: usize, y: usize| -> f32 {
            if x < width && y < height {
                data[y * width + x] as f32 / 255.0
            } else {
                0.0
            }
        };

        let c00 = get_pixel(x0, y0);
        let c10 = get_pixel(x1, y0);
        let c01 = get_pixel(x0, y1);
        let c11 = get_pixel(x1, y1);

        c00 * (1.0 - fx) * (1.0 - fy)
            + c10 * fx * (1.0 - fy)
            + c01 * (1.0 - fx) * fy
            + c11 * fx * fy
    } else {
        0.0
    }
}

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
        PixelBrushShape::Custom {
            width,
            height,
            data,
        } => {
            let alpha = sample_custom_mask_bilinear(dx, dy, radius, *width, *height, data);
            let dist_sq = dx * dx + dy * dy;
            (alpha, dist_sq)
        }
    }
}
