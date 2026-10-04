//! Tip alpha for round and square tips (Gaussian or curve falloff); image
//! tips sample their [`crate::brush_engine::tip::TipMask`].

use super::hardness::SoftnessSelector;

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

/// A round or square tip's alpha at `(dx, dy)` from its centre, edged as
/// Krita's auto brush tips are (`KisAntialiasingFadeMaker`). Anti-aliased,
/// the last pixel inside the edge fades linearly from the falloff's value
/// one pixel in to nothing at the edge (a square fades each side on its own);
/// without, the falloff is cut off at the edge. `falloff` is the tip's
/// alpha at a distance from 0 (centre) to 1 (edge).
pub(super) fn auto_tip_alpha(
    (dx, dy): (f32, f32),
    radius: f32,
    square: bool,
    softness_selector: SoftnessSelector,
    falloff: impl Fn(f32) -> f32,
    antialias: bool,
) -> f32 {
    let fade_start = (radius - 1.0).max(0.0);
    let fade_width = radius - fade_start;
    if square {
        let (ax, ay) = (dx.abs(), dy.abs());
        if ax >= radius || ay >= radius {
            return 0.0;
        }
        let base = falloff(ax.max(ay) / radius);
        if !antialias {
            return base;
        }
        let fx = ((ax - fade_start) / fade_width).clamp(0.0, 1.0);
        let fy = ((ay - fade_start) / fade_width).clamp(0.0, 1.0);
        return base * (1.0 - fx) * (1.0 - fy);
    }
    let dist = (dx * dx + dy * dy).sqrt();
    if dist >= radius {
        return 0.0;
    }
    if !antialias || dist <= fade_start {
        return falloff(dist / radius);
    }
    let base = falloff(fade_start / radius);
    match softness_selector {
        SoftnessSelector::Gaussian => base * (radius - dist) / fade_width,
        // Krita's curve tips fade in squared distance.
        SoftnessSelector::Curve => {
            let (n2, s2) = ((dist / radius).powi(2), (fade_start / radius).powi(2));
            base * (1.0 - n2) / (1.0 - s2)
        }
    }
}

/// Samples per pixel side for a tip `radius` across, as Krita takes them:
/// anti-aliased tips under 10 px average 3×3 samples a pixel, under 1 px
/// 6×6, so small dabs don't flicker along a stroke.
pub(super) fn supersamples(radius: f32, antialias: bool) -> usize {
    match 2.0 * radius {
        _ if !antialias => 1,
        d if d < 1.0 => 6,
        d if d < 10.0 => 3,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(dx: f32, r: f32, aa: bool) -> f32 {
        let hard = |t| gaussian_falloff(t, 1.0);
        auto_tip_alpha((dx, 0.0), r, false, SoftnessSelector::Gaussian, hard, aa)
    }

    #[test]
    fn the_edge_fades_over_the_last_pixel_as_in_krita() {
        // Hard tip, radius 10: solid to 9, linear to nothing at 10.
        assert_eq!(round(8.9, 10.0, true), 1.0);
        assert!((round(9.5, 10.0, true) - 0.5).abs() < 1e-6);
        assert!((round(9.75, 10.0, true) - 0.25).abs() < 1e-6);
        assert_eq!(round(10.0, 10.0, true), 0.0);
        // Off: cut at the edge.
        assert_eq!(round(9.9, 10.0, false), 1.0);
        // A square fades each side on its own; the corner gets both.
        let sq = |dx, dy| {
            let hard = |t| gaussian_falloff(t, 1.0);
            auto_tip_alpha((dx, dy), 10.0, true, SoftnessSelector::Gaussian, hard, true)
        };
        assert!((sq(9.5, 0.0) - 0.5).abs() < 1e-6);
        assert!((sq(9.5, 9.5) - 0.25).abs() < 1e-6);
        // Small anti-aliased tips are supersampled.
        assert_eq!(supersamples(4.0, true), 3);
        assert_eq!(supersamples(0.4, true), 6);
        assert_eq!(supersamples(4.0, false), 1);
        assert_eq!(supersamples(5.0, true), 1);
    }

    #[test]
    fn without_anti_aliasing_a_soft_tip_stays_soft_inside() {
        // Krita keeps the falloff and only drops the edge fade.
        let soft = |t| gaussian_falloff(t, 0.2);
        let a = auto_tip_alpha(
            (8.0, 0.0),
            10.0,
            false,
            SoftnessSelector::Gaussian,
            soft,
            false,
        );
        assert!(a > 0.0 && a < 1.0);
    }
}
