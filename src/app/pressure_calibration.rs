//! Calibrating the pen's pressure curve from a few test strokes: the raw
//! pressures drawn on a pad in Settings are spread over the full range, so
//! a light hand reaches full pressure and a heavy one keeps its range.

use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
use crate::tablet::{TabletPhase, TabletSample};
use eframe::egui;

/// Fewest pen samples a curve is fitted from.
pub const MIN_SAMPLES: usize = 200;
/// Most samples kept (a few minutes of drawing is plenty).
const MAX_SAMPLES: usize = 20_000;
/// The raw pressures (as quantiles of what was drawn) and the pressure each
/// becomes: what the hand presses most lightly maps near 0, its heaviest to
/// full pressure, and the rest spread evenly between.
const QUANTILES: [(f32, f32); 5] = [
    (0.02, 0.02),
    (0.25, 0.25),
    (0.5, 0.5),
    (0.75, 0.75),
    (0.98, 1.0),
];

/// The test strokes being drawn (Settings → Calibrate).
#[derive(Default)]
pub struct Calibration {
    /// Where the pad was drawn last frame (screen points).
    pub pad: Option<egui::Rect>,
    /// Raw pressures of the pen's contact on the pad.
    pub samples: Vec<f32>,
    /// The strokes, to draw on the pad: position and raw pressure.
    pub strokes: Vec<Vec<(egui::Pos2, f32)>>,
    /// The pen is down on the pad.
    down: bool,
}

impl Calibration {
    /// Keep this frame's pen samples that touch the pad.
    pub fn record(&mut self, samples: &[TabletSample]) {
        let Some(pad) = self.pad else {
            return;
        };
        for s in samples {
            let pos = egui::pos2(s.pos[0], s.pos[1]);
            match s.phase {
                TabletPhase::Down if pad.contains(pos) => {
                    self.down = true;
                    self.strokes.push(Vec::new());
                }
                TabletPhase::Move if self.down => {}
                TabletPhase::Up | TabletPhase::Cancel => {
                    self.down = false;
                    continue;
                }
                _ => continue,
            }
            if pad.contains(pos) && s.pressure > 0.0 && self.samples.len() < MAX_SAMPLES {
                self.samples.push(s.pressure.clamp(0.0, 1.0));
                if let Some(stroke) = self.strokes.last_mut() {
                    stroke.push((pos, s.pressure));
                }
            }
        }
    }
}

/// The pressure curve that spreads `raw` (pen pressures, 0..=1) over the
/// full range; `None` with too few samples or no real range of pressure.
pub fn fit_pressure_curve(raw: &[f32]) -> Option<SoftnessCurve> {
    let mut v: Vec<f32> = raw
        .iter()
        .copied()
        .filter(|p| p.is_finite() && *p > 0.0)
        .map(|p| p.min(1.0))
        .collect();
    if v.len() < MIN_SAMPLES {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let quantile = |q: f32| v[(q * (v.len() - 1) as f32).round() as usize];
    if quantile(0.98) - quantile(0.02) < 0.05 {
        return None;
    }
    let mut points = vec![CurvePoint::new(0.0, 0.0)];
    for (q, y) in QUANTILES {
        let x = quantile(q);
        // Close quantiles (a hand that barely varies there) would make the
        // curve jump: keep the first.
        if x > points.last().map_or(0.0, |p| p.x) + 0.01 {
            points.push(CurvePoint::new(x, y));
        }
    }
    if points.last().is_some_and(|p| p.x < 1.0) {
        points.push(CurvePoint::new(1.0, 1.0));
    }
    Some(SoftnessCurve { points })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn even(from: f32, to: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| from + (to - from) * (i as f32 + 0.5) / n as f32)
            .collect()
    }

    #[test]
    fn an_even_hand_keeps_close_to_a_straight_line() {
        let curve = fit_pressure_curve(&even(0.0, 1.0, 1000)).unwrap();
        for x in [0.1, 0.3, 0.5, 0.7] {
            assert!((curve.eval(x) - x).abs() < 0.03, "{x} → {}", curve.eval(x));
        }
        // Near the top it rises a little faster: the heaviest 2% is full.
        assert!((curve.eval(0.9) - 0.9).abs() < 0.06);
        assert!(curve.eval(0.98) > 0.99);
    }

    #[test]
    fn a_light_hand_reaches_full_pressure_and_the_curve_rises() {
        let curve = fit_pressure_curve(&even(0.05, 0.6, 1000)).unwrap();
        assert!(curve.eval(0.6) > 0.99);
        assert!((curve.eval(0.33) - 0.5).abs() < 0.05);
        let mut last = 0.0;
        for i in 0..=100 {
            let y = curve.eval(i as f32 / 100.0);
            assert!(y >= last - 1e-6, "falls at {i}");
            last = y;
        }
    }

    #[test]
    fn too_few_samples_or_no_range_fit_nothing() {
        assert!(fit_pressure_curve(&even(0.0, 1.0, MIN_SAMPLES - 1)).is_none());
        assert!(fit_pressure_curve(&[0.5; 1000]).is_none());
        // Zeros (hovering) and junk don't count.
        let mut junk = vec![0.0; 1000];
        junk.extend([f32::NAN; 10]);
        assert!(fit_pressure_curve(&junk).is_none());
    }

    #[test]
    fn only_pen_contact_on_the_pad_is_recorded() {
        let sample = |x: f32, pressure: f32, phase| TabletSample {
            pos: [x, 10.0],
            pressure,
            tilt: None,
            roll: None,
            wheel: None,
            is_eraser: false,
            phase,
        };
        let mut c = Calibration::default();
        c.record(&[sample(10.0, 0.5, TabletPhase::Down)]);
        assert!(c.samples.is_empty(), "no pad shown yet");
        c.pad = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(100.0, 50.0),
        ));
        c.record(&[
            sample(200.0, 0.9, TabletPhase::Down),
            sample(20.0, 0.9, TabletPhase::Move),
            sample(200.0, 0.9, TabletPhase::Up),
            sample(10.0, 0.3, TabletPhase::Down),
            sample(20.0, 0.4, TabletPhase::Move),
            sample(300.0, 0.6, TabletPhase::Move),
            sample(30.0, 0.5, TabletPhase::Up),
            sample(40.0, 0.7, TabletPhase::Move),
        ]);
        assert_eq!(
            c.samples,
            [0.3, 0.4],
            "a stroke from off the pad, or moves after Up, don't count"
        );
        assert_eq!(c.strokes.len(), 1);
    }
}
