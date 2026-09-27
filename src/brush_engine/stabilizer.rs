//! Smoothing for hand-drawn paths: the brush stroke and freehand selections.

use crate::brush_engine::brush::StabilizerAlgorithm;
use eframe::egui::Vec2;

/// How strongly a path is smoothed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StabilizerSettings {
    pub algorithm: StabilizerAlgorithm,
    /// Simple: 0 (off) ..= 1 (maximum smoothing).
    pub strength: f32,
    /// Dynamic: the spring's mass (0.01..=1) and drag (0..=1).
    pub mass: f32,
    pub drag: f32,
}

impl StabilizerSettings {
    /// Simple smoothing at `strength` (0 = off).
    pub fn simple(strength: f32) -> Self {
        Self {
            algorithm: if strength > 0.0 {
                StabilizerAlgorithm::Simple
            } else {
                StabilizerAlgorithm::None
            },
            strength,
            mass: 0.1,
            drag: 0.5,
        }
    }
}

/// Per-path smoothing state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stabilizer {
    velocity: Vec2,
    /// Dynamic: time not yet stepped (less than one [`STEP`]).
    carry: f32,
}

/// The input rate the settings are tuned for (a typical pen, 200 Hz): with
/// times, smoothing goes by elapsed time in steps of this, so a 1000 Hz
/// mouse and a 200 Hz pen are smoothed the same.
pub const STEP: f32 = 0.005;

impl Stabilizer {
    /// The smoothed position for input `raw`, following on from `prev` (the
    /// previous smoothed position; `None` at the start of the path).
    pub fn step(&mut self, settings: &StabilizerSettings, prev: Option<Vec2>, raw: Vec2) -> Vec2 {
        self.step_timed(settings, prev, raw, None)
    }

    /// [`Self::step`] with the time since the previous input (seconds): the
    /// smoothing then doesn't depend on how often input comes. Without a
    /// time, each input counts as one [`STEP`].
    pub fn step_timed(
        &mut self,
        settings: &StabilizerSettings,
        prev: Option<Vec2>,
        raw: Vec2,
        dt: Option<f32>,
    ) -> Vec2 {
        let Some(prev) = prev else {
            return raw;
        };
        let steps = dt.map_or(1.0, |dt| dt.max(0.0) / STEP);
        match settings.algorithm {
            StabilizerAlgorithm::None => raw,
            StabilizerAlgorithm::Simple => {
                if settings.strength > 0.0 {
                    // The share left behind per step, compounded over the
                    // elapsed steps.
                    let keep = (settings.strength * 0.95).powf(steps);
                    prev + (raw - prev) * (1.0 - keep)
                } else {
                    raw
                }
            }
            // The input pulls the pen on a spring: force = target - current,
            // acceleration = force / mass, velocity damped by the drag;
            // stepped at a fixed rate, the target moving along from the last
            // input to this one.
            StabilizerAlgorithm::Dynamic => {
                let mass = settings.mass.max(0.01) * 50.0;
                self.carry += steps;
                let n = self.carry.floor() as u32;
                self.carry -= n as f32;
                let mut pos = prev;
                for _ in 0..n.min(400) {
                    self.velocity += (raw - pos) / mass;
                    self.velocity *= 1.0 - settings.drag;
                    pos += self.velocity;
                }
                pos
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_follows_the_input_exactly() {
        let mut s = Stabilizer::default();
        let off = StabilizerSettings::simple(0.0);
        assert_eq!(
            s.step(&off, Some(Vec2::ZERO), Vec2::new(5.0, 1.0)),
            Vec2::new(5.0, 1.0)
        );
    }

    #[test]
    fn simple_smoothing_lags_and_converges() {
        let mut s = Stabilizer::default();
        let settings = StabilizerSettings::simple(0.8);
        let target = Vec2::new(100.0, 0.0);
        let mut pos = s.step(&settings, None, Vec2::ZERO);
        let first = s.step(&settings, Some(pos), target);
        assert!(first.x > 0.0 && first.x < 100.0, "lags behind: {first:?}");
        for _ in 0..200 {
            pos = s.step(&settings, Some(pos), target);
        }
        assert!((pos - target).length() < 0.01, "converges: {pos:?}");
    }
}
