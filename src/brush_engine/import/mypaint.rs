//! MyPaint brushes (`.myb`, JSON, version 3): each setting has a base value
//! and curves for what drives it (pressure, speed, random…). MyPaint's
//! dabs are round, so the tip is this app's round one; its size, hardness,
//! spacing, shape, scatter and pressure response come across, the rest of
//! its engine (smudge, speed filters, colour changes) doesn't.

use super::Imported;
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
use eframe::egui::Color32;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Myb {
    version: u32,
    settings: HashMap<String, Setting>,
}

#[derive(Deserialize)]
struct Setting {
    base_value: f32,
    #[serde(default)]
    inputs: HashMap<String, Vec<[f32; 2]>>,
}

pub(super) fn import(bytes: &[u8], stem: &str) -> Result<Imported, String> {
    let myb: Myb =
        serde_json::from_slice(bytes).map_err(|e| format!("Not a MyPaint brush ({e})"))?;
    if myb.version != 3 {
        return Err(format!(
            "MyPaint brush version {} isn't supported",
            myb.version
        ));
    }
    let base = |k: &str, default: f32| myb.settings.get(k).map_or(default, |s| s.base_value);
    let pressure = |k: &str| {
        myb.settings
            .get(k)
            .and_then(|s| s.inputs.get("pressure"))
            .filter(|c| c.len() >= 2)
    };
    let radius = base("radius_logarithmic", 2.0).exp();
    let diameter = (radius * 2.0).clamp(1.0, 3000.0);
    // Dabs per radius travelled: the spacing as a share of the diameter.
    let per_radius =
        (base("dabs_per_actual_radius", 2.0) + base("dabs_per_basic_radius", 0.0)).max(0.05);
    let spacing = (50.0 / per_radius).clamp(1.0, 1000.0);
    let hardness = (base("hardness", 0.8) * 100.0).clamp(0.0, 100.0);
    let mut b = Brush::new(diameter, hardness, Color32::BLACK, spacing);
    b.brush_options.opacity = base("opaque", 1.0).clamp(0.0, 1.0);
    b.brush_options.pressure_size = false;
    // Radius against pressure: an offset to the log radius.
    if let Some(curve) = pressure("radius_logarithmic") {
        let at = |x: f32| lerp_curve(curve, x);
        let (low, high) = (at(0.0), at(1.0));
        b.brush_options.pressure_size = high > low;
        b.brush_options.pressure_min_size = (low - high).exp().clamp(0.0, 1.0);
    }
    // Opacity against pressure (MyPaint's usual 0 → 1).
    if let Some(curve) = pressure("opaque_multiply").or(pressure("opaque")) {
        b.brush_options.pressure_opacity = true;
        let top = curve
            .iter()
            .map(|p| p[1])
            .fold(f32::MIN, f32::max)
            .max(1e-3);
        let points: Vec<CurvePoint> = curve
            .iter()
            .map(|p| CurvePoint::new(p[0].clamp(0.0, 1.0), (p[1] / top).clamp(0.0, 1.0)))
            .collect();
        let straight = points.len() == 2
            && points[0] == CurvePoint::new(0.0, 0.0)
            && points[1] == CurvePoint::new(1.0, 1.0);
        if !straight {
            b.brush_options.pressure_curves.opacity = Some(SoftnessCurve { points });
        }
    }
    let ratio = base("elliptical_dab_ratio", 1.0).max(1.0);
    b.dynamics.tip.ratio = (1.0 / ratio).clamp(0.02, 1.0);
    b.dynamics.tip.angle = base("elliptical_dab_angle", 90.0) - 90.0;
    // Random offset, in radii: the scatter, as a share of the size.
    b.jitter = (base("offset_by_random", 0.0) * 50.0).clamp(0.0, 500.0);
    b.dynamics.random.size = (base("radius_by_random", 0.0) * 0.5).clamp(0.0, 1.0);
    let mut out = Imported::default();
    if base("smudge", 0.0) > 0.1 {
        out.notes.push(format!(
            "{stem}: MyPaint's smudging wasn't imported (for mixing, use the Smudge tool)"
        ));
    }
    out.presets.push(BrushPreset {
        name: stem.to_string(),
        brush: b,
        file: None,
    });
    Ok(out)
}

/// A MyPaint input curve at `x` (straight between its points).
fn lerp_curve(points: &[[f32; 2]], x: f32) -> f32 {
    let first = points[0];
    if x <= first[0] {
        return first[1];
    }
    for w in points.windows(2) {
        let ([x0, y0], [x1, y1]) = (w[0], w[1]);
        if x <= x1 {
            let t = if x1 > x0 { (x - x0) / (x1 - x0) } else { 1.0 };
            return y0 + (y1 - y0) * t;
        }
    }
    points[points.len() - 1][1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mypaint_brush_brings_its_size_spacing_and_pressure() {
        let json = br#"{"version": 3, "settings": {
            "radius_logarithmic": {"base_value": 2.3, "inputs": {"pressure": [[0, -0.7], [1, 0.0]]}},
            "hardness": {"base_value": 0.6, "inputs": {}},
            "dabs_per_actual_radius": {"base_value": 4.0, "inputs": {}},
            "opaque_multiply": {"base_value": 0.0, "inputs": {"pressure": [[0, 0], [1, 1]]}},
            "elliptical_dab_ratio": {"base_value": 2.0, "inputs": {}},
            "offset_by_random": {"base_value": 0.4, "inputs": {}}
        }}"#;
        let imported = import(json, "Pencil").unwrap();
        let b = &imported.presets[0].brush;
        let o = &b.brush_options;
        assert!((o.diameter - 2.0 * 2.3f32.exp()).abs() < 1e-3);
        assert!((o.spacing - 12.5).abs() < 1e-3);
        assert!((o.hardness - 60.0).abs() < 1e-3);
        assert!(o.pressure_size && o.pressure_opacity);
        assert!((o.pressure_min_size - (-0.7f32).exp()).abs() < 1e-3);
        assert!(o.pressure_curves.opacity.is_none(), "straight: no curve");
        assert_eq!(b.dynamics.tip.ratio, 0.5);
        assert!((b.jitter - 20.0).abs() < 1e-3);
    }

    #[test]
    fn other_versions_and_non_json_are_refused() {
        assert!(import(br#"{"version": 2, "settings": {}}"#, "x").is_err());
        assert!(import(b"radius 2.0", "x").is_err());
    }
}
