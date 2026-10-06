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
    /// The pulled string, post-correction and motion filter settings.
    pub modes: StabilizerModes,
    /// Canvas pixels → screen points (the view zoom): the string's length
    /// and the filter's speed are on screen.
    pub view_scale: f32,
}

/// Settings of the stabiliser modes beyond Simple and Dynamic, kept
/// together (saved in presets; missing fields take their defaults).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct StabilizerModes {
    /// Pulled string: its length, in screen points. The brush only moves
    /// once the pen is further away than this.
    pub string_length: f32,
    /// Pulled string: when the pen lifts, the line goes on to it.
    pub catch_up: bool,
    /// Post-correction: how much the path is smoothed when the pen lifts
    /// (0..=1).
    pub correction: f32,
    /// Post-correction while drawing: the line is smoothed as it goes (the
    /// stretch near the pen settles once it's past the smoothing's reach),
    /// rather than all at once when the pen lifts. The same line either way.
    pub correction_live: bool,
    /// Motion filter: how strongly slow movement is smoothed (0..=1).
    pub filter_strength: f32,
    /// Motion filter: how quickly the smoothing lets go as the pen speeds
    /// up (0..=1).
    pub filter_speed: f32,
}

impl Default for StabilizerModes {
    fn default() -> Self {
        Self {
            string_length: 40.0,
            catch_up: true,
            correction: 0.5,
            correction_live: false,
            filter_strength: 0.5,
            filter_speed: 0.5,
        }
    }
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
            modes: StabilizerModes::default(),
            view_scale: 1.0,
        }
    }

    /// The string's length in canvas pixels.
    pub fn string_length(&self) -> f32 {
        self.modes.string_length.max(0.0) / self.view_scale.max(1e-6)
    }
}

/// Per-path smoothing state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stabilizer {
    velocity: Vec2,
    /// Dynamic: time not yet stepped (less than one [`STEP`]).
    carry: f32,
    /// Motion filter: the previous input, and the pen's velocity smoothed
    /// (canvas pixels per second).
    last_raw: Option<Vec2>,
    filter_velocity: Vec2,
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
            StabilizerAlgorithm::String => pull_string(prev, raw, settings.string_length()),
            // The path is smoothed afterwards, when the pen lifts.
            StabilizerAlgorithm::PostCorrection => raw,
            StabilizerAlgorithm::MotionFilter => {
                self.motion_filter(settings, prev, raw, dt.unwrap_or(STEP))
            }
        }
    }

    /// A 1€ filter (Casiez et al.): a low-pass filter whose cutoff rises with
    /// the pen's speed, so slow, careful movement loses its jitter and fast
    /// movement keeps up without lag.
    fn motion_filter(
        &mut self,
        settings: &StabilizerSettings,
        prev: Vec2,
        raw: Vec2,
        dt: f32,
    ) -> Vec2 {
        let last_raw = self.last_raw.replace(raw).unwrap_or(prev);
        let strength = settings.modes.filter_strength.clamp(0.0, 1.0);
        if strength <= 0.0 || dt <= 0.0 {
            // Samples stamped at the same instant: the next one moves on.
            return if strength <= 0.0 { raw } else { prev };
        }
        let alpha = |cutoff: f32| {
            let tau = 1.0 / (std::f32::consts::TAU * cutoff);
            1.0 / (1.0 + tau / dt)
        };
        let velocity = (raw - last_raw) / dt;
        self.filter_velocity += (velocity - self.filter_velocity) * alpha(FILTER_SPEED_CUTOFF);
        let speed = self.filter_velocity.length() * settings.view_scale;
        let min_cutoff = FILTER_MAX_CUTOFF * FILTER_MIN_SHARE.powf(strength);
        let beta = 0.0005 + settings.modes.filter_speed.clamp(0.0, 1.0) * 0.02;
        prev + (raw - prev) * alpha(min_cutoff + beta * speed)
    }
}

/// Motion filter: the cutoff (Hz) at the lowest strength, the share of it
/// left at full strength, and the cutoff the speed is smoothed with.
const FILTER_MAX_CUTOFF: f32 = 30.0;
const FILTER_MIN_SHARE: f32 = 0.005;
const FILTER_SPEED_CUTOFF: f32 = 1.0;

/// Pulled string: the brush at `tip`, on a string of `length` to the pen:
/// it stays put while the pen is within the length, and is pulled along the
/// line to the pen, the length behind it, once it's further.
pub fn pull_string(tip: Vec2, pen: Vec2, length: f32) -> Vec2 {
    let d = pen - tip;
    let dist = d.length();
    if dist <= length {
        tip
    } else {
        pen - d * (length / dist)
    }
}

/// Post-correction: the widest smoothing, in screen points (the Gaussian's
/// spread at full strength).
pub const MAX_CORRECTION: f32 = 24.0;

/// Post-correction: `points` smoothed along the path, each averaged with
/// its neighbours by a Gaussian over the distance along the path, of a
/// spread up to [`MAX_CORRECTION`] screen points at `strength` 1
/// (`view_scale`: canvas pixels → screen points). The ends stay where they
/// are: the spread narrows near them.
pub fn smooth_path(points: &[Vec2], strength: f32, view_scale: f32) -> Vec<Vec2> {
    smooth_path_from(points, strength, view_scale, 0)
}

/// [`smooth_path`]'s points from index `from` on (the same values).
pub fn smooth_path_from(points: &[Vec2], strength: f32, view_scale: f32, from: usize) -> Vec<Vec2> {
    let sigma = correction_sigma(strength, view_scale);
    if points.len() < 3 || sigma <= 0.0 {
        return points[from.min(points.len())..].to_vec();
    }
    let (along, total) = distances(points);
    (from..points.len())
        .map(|i| {
            let s = along[i];
            let sigma = sigma.min(s.min(total - s) / 3.0);
            if sigma <= 1e-3 {
                return points[i];
            }
            let reach = sigma * 3.0;
            let k = -0.5 / (sigma * sigma);
            let (mut sum, mut weight) = (Vec2::ZERO, 0.0);
            let mut add = |j: usize| {
                let d = along[j] - s;
                let w = (d * d * k).exp();
                sum += points[j] * w;
                weight += w;
            };
            add(i);
            for j in (0..i).rev().take_while(|&j| s - along[j] <= reach) {
                add(j);
            }
            for j in (i + 1..points.len()).take_while(|&j| along[j] - s <= reach) {
                add(j);
            }
            sum / weight
        })
        .collect()
}

/// How many of `points` (from the first) [`smooth_path`] puts where it
/// will whatever points come after them: those further back along the path
/// than its widest reach. Post-correction can paint them while drawing.
pub fn smooth_path_settled(points: &[Vec2], strength: f32, view_scale: f32) -> usize {
    let sigma = correction_sigma(strength, view_scale);
    if sigma <= 0.0 {
        return points.len();
    }
    let (along, total) = distances(points);
    // (Strictly: a point to come exactly at the reach would still count.)
    along.partition_point(|&s| total - s > sigma * 3.0)
}

fn correction_sigma(strength: f32, view_scale: f32) -> f32 {
    strength.clamp(0.0, 1.0) * MAX_CORRECTION / view_scale.max(1e-6)
}

/// Distance along `points` to each, and in all.
fn distances(points: &[Vec2]) -> (Vec<f32>, f32) {
    let mut along = Vec::with_capacity(points.len());
    let mut total = 0.0;
    for (i, p) in points.iter().enumerate() {
        if i > 0 {
            total += (*p - points[i - 1]).length();
        }
        along.push(total);
    }
    (along, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_points_are_smoothed_the_same_whatever_follows() {
        let path: Vec<Vec2> = (0..200)
            .map(|i| Vec2::new(i as f32 * 2.0, (i as f32 * 0.37).sin() * 9.0))
            .collect();
        let full = smooth_path(&path, 0.8, 1.5);
        for n in [3, 10, 50, 120, 199] {
            let part = &path[..n];
            let settled = smooth_path_settled(part, 0.8, 1.5);
            assert!(settled < n, "the end is never settled");
            let early = smooth_path(part, 0.8, 1.5);
            // Bit for bit: painting them early paints the same.
            assert_eq!(early[..settled], full[..settled], "{n}");
            assert_eq!(smooth_path_from(part, 0.8, 1.5, settled), early[settled..]);
        }
        assert!(
            smooth_path_settled(&path, 0.8, 1.5) > 150,
            "most of a long path"
        );
        assert_eq!(smooth_path_settled(&path, 0.0, 1.0), path.len(), "off");
    }

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

    fn mode(algorithm: StabilizerAlgorithm) -> StabilizerSettings {
        StabilizerSettings {
            algorithm,
            ..StabilizerSettings::simple(0.0)
        }
    }

    #[test]
    fn a_pulled_string_waits_within_its_length_and_follows_at_it() {
        let mut s = Stabilizer::default();
        let mut settings = mode(StabilizerAlgorithm::String);
        settings.modes.string_length = 20.0;
        settings.view_scale = 2.0; // 20 screen points = 10 canvas px
        let start = s.step(&settings, None, Vec2::new(50.0, 50.0));
        assert_eq!(start, Vec2::new(50.0, 50.0), "starts at the pen");
        // Anywhere within the length: the brush stays put.
        for pen in [
            Vec2::new(59.0, 50.0),
            Vec2::new(50.0, 41.0),
            Vec2::new(56.0, 56.0),
        ] {
            assert_eq!(s.step(&settings, Some(start), pen), start, "{pen:?}");
        }
        // Beyond it: pulled along the line to the pen, the length behind.
        let pen = Vec2::new(80.0, 90.0);
        let tip = s.step(&settings, Some(start), pen);
        assert!(((pen - tip).length() - 10.0).abs() < 1e-4, "{tip:?}");
        let (along, to_pen) = ((tip - start).normalized(), (pen - start).normalized());
        assert!((along - to_pen).length() < 1e-5, "towards the pen");
        // And keeps following at the length.
        let mut tip = tip;
        for i in 0..50 {
            let pen = Vec2::new(80.0 + i as f32 * 3.0, 90.0);
            tip = s.step(&settings, Some(tip), pen);
            assert!((pen - tip).length() <= 10.0 + 1e-3);
        }
        assert!(((Vec2::new(227.0, 90.0) - tip).length() - 10.0).abs() < 1e-3);
    }

    /// How much of the input's shake is left, following a line across at
    /// `speed` (canvas px per second) at 200 Hz with ±2 px noise across it.
    fn jitter_left(speed: f32) -> f32 {
        let mut settings = mode(StabilizerAlgorithm::MotionFilter);
        settings.modes.filter_strength = 0.6;
        settings.modes.filter_speed = 0.5;
        let mut s = Stabilizer::default();
        let mut pos = None;
        let (mut shake_in, mut shake_out) = (0.0, 0.0);
        let mut noise = 12345u32;
        for i in 0..400 {
            noise = noise.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let n = ((noise >> 16) & 0x7fff) as f32 / 32767.0 * 4.0 - 2.0;
            let raw = Vec2::new(i as f32 * speed * STEP, 100.0 + n);
            let out = s.step_timed(&settings, pos, raw, Some(STEP));
            pos = Some(out);
            if i >= 100 {
                shake_in += n.abs();
                shake_out += (out.y - 100.0).abs();
            }
        }
        shake_out / shake_in
    }

    #[test]
    fn the_motion_filter_takes_more_shake_out_of_slow_lines_than_fast_ones() {
        let (slow, fast) = (jitter_left(40.0), jitter_left(3000.0));
        assert!(slow < 0.3, "slow lines lose most of their shake: {slow}");
        assert!(
            fast > slow * 2.0,
            "fast lines are followed closely: {slow} vs {fast}"
        );
        // Fast, it keeps up with the pen along the line.
        let mut settings = mode(StabilizerAlgorithm::MotionFilter);
        settings.modes.filter_strength = 0.6;
        let mut s = Stabilizer::default();
        let mut pos = None;
        let mut raw = Vec2::ZERO;
        for i in 0..200 {
            raw = Vec2::new(i as f32 * 3000.0 * STEP, 0.0);
            pos = Some(s.step_timed(&settings, pos, raw, Some(STEP)));
        }
        // (15 px between samples: at most about a sample and a half behind.)
        assert!((raw - pos.unwrap()).length() < 25.0, "{pos:?}");
        // No strength: the input as it is.
        settings.modes.filter_strength = 0.0;
        let p = Vec2::new(3.0, 4.0);
        assert_eq!(s.step_timed(&settings, Some(Vec2::ZERO), p, Some(STEP)), p);
    }

    /// The sum of squared second differences: how much a path wiggles.
    fn roughness(points: &[Vec2]) -> f32 {
        points
            .windows(3)
            .map(|w| (w[0] - w[1] * 2.0 + w[2]).length_sq())
            .sum()
    }

    #[test]
    fn post_correction_smooths_the_path_and_keeps_its_ends() {
        let zigzag: Vec<Vec2> = (0..80)
            .map(|i| Vec2::new(i as f32 * 2.0, if i % 2 == 0 { 0.0 } else { 3.0 }))
            .collect();
        let smoothed = smooth_path(&zigzag, 0.5, 1.0);
        assert_eq!(smoothed.len(), zigzag.len());
        assert_eq!(smoothed[0], zigzag[0]);
        assert_eq!(smoothed[79], zigzag[79]);
        let (before, after) = (roughness(&zigzag), roughness(&smoothed));
        assert!(after < before * 0.05, "{before} → {after}");
        // The middle runs down the zigzag's middle.
        assert!((smoothed[40].y - 1.5).abs() < 0.2, "{:?}", smoothed[40]);
        // Stronger is smoother; none leaves the path as it is.
        assert!(roughness(&smooth_path(&zigzag, 1.0, 1.0)) < after);
        assert_eq!(smooth_path(&zigzag, 0.0, 1.0), zigzag);
    }
}
