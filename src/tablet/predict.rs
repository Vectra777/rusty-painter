//! Where the pen will be a frame from now, from where it has been: a small
//! Kalman filter per axis (position, velocity, acceleration) and one for
//! the pressure, as androidx's `input-motionprediction` KalmanPredictor
//! does. Android gives no predicted samples of its own (iOS does), so the
//! line lagged a frame or two behind the pen there; these are drawn ahead
//! of the stroke and taken back by the next real sample, as iOS's are.
//!
//! Platform-agnostic: it sees positions, pressures and times only.

/// Samples needed before anything is predicted (the filter has settled).
const MIN_SAMPLES: usize = 4;
/// The most predicted samples given at once.
const MAX_AHEAD: usize = 8;
/// Slower than this (points per second), the pen is as good as still:
/// nothing ahead.
const MIN_SPEED: f64 = 20.0;
/// How far a position measurement is trusted (its variance, points²).
const POSITION_NOISE: f64 = 0.25;
/// How fast the acceleration may change (the jerk's spectral density,
/// points² per second⁵): high, as a hand turns a stroke quickly.
const POSITION_JERK: f64 = 4.0e9;
/// The same for the pressure (0..1 units).
const PRESSURE_NOISE: f64 = 1.0e-4;
const PRESSURE_JERK: f64 = 1.0e3;
/// Below this predicted pressure the pen is lifting: the prediction ends.
const LIFT_PRESSURE: f32 = 0.01;

/// One coordinate's state: value, rate and rate of the rate, and how
/// uncertain they are.
#[derive(Clone, Copy, Debug, Default)]
struct Axis {
    s: [f64; 3],
    p: [[f64; 3]; 3],
}

impl Axis {
    fn start(z: f64, noise: f64) -> Self {
        Axis {
            s: [z, 0.0, 0.0],
            // Nothing known of the motion yet.
            p: [[noise, 0.0, 0.0], [0.0, 1.0e8, 0.0], [0.0, 0.0, 1.0e12]],
        }
    }

    /// `dt` seconds on, measured at `z`.
    fn step(&mut self, z: f64, dt: f64, noise: f64, jerk: f64) {
        let (s, p) = (&mut self.s, &mut self.p);
        // Predict: constant acceleration over dt.
        let f = [[1.0, dt, dt * dt * 0.5], [0.0, 1.0, dt], [0.0, 0.0, 1.0]];
        *s = std::array::from_fn(|i| (0..3).map(|j| f[i][j] * s[j]).sum());
        let fp: [[f64; 3]; 3] = std::array::from_fn(|i| {
            std::array::from_fn(|j| (0..3).map(|k| f[i][k] * p[k][j]).sum())
        });
        // White jerk: its effect over dt on each of the three.
        let (d2, d3, d4, d5) = (dt * dt, dt.powi(3), dt.powi(4), dt.powi(5));
        let q = [
            [d5 / 20.0, d4 / 8.0, d3 / 6.0],
            [d4 / 8.0, d3 / 3.0, d2 / 2.0],
            [d3 / 6.0, d2 / 2.0, dt],
        ];
        *p = std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                (0..3).map(|k| fp[i][k] * f[j][k]).sum::<f64>() + jerk * q[i][j]
            })
        });
        // Correct by the measurement (of the value only).
        let gain_den = p[0][0] + noise;
        let k: [f64; 3] = std::array::from_fn(|i| p[i][0] / gain_den);
        let y = z - s[0];
        for i in 0..3 {
            s[i] += k[i] * y;
        }
        let row0 = p[0];
        for i in 0..3 {
            for j in 0..3 {
                p[i][j] -= k[i] * row0[j];
            }
        }
    }
}

/// A predicted pen sample: where, how hard and when (the samples' clock).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Predicted {
    pub pos: [f32; 2],
    pub pressure: f32,
    pub time: f64,
}

/// The pen's motion so far in one stroke, and where it goes next.
#[derive(Default)]
pub struct MotionPredictor {
    x: Axis,
    y: Axis,
    pressure: Axis,
    /// The last sample measured (position, pressure, time).
    last: Option<Predicted>,
    /// How far the last sample was from the one before (points).
    last_step: f32,
    /// The time between samples (seconds, smoothed).
    interval: f64,
    samples: usize,
}

impl MotionPredictor {
    /// Forget the stroke (the pen went down or up).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A real sample of the stroke: at `pos` with `pressure`, `time`
    /// seconds on the samples' clock.
    pub fn update(&mut self, pos: [f32; 2], pressure: f32, time: f64) {
        let now = Predicted {
            pos,
            pressure,
            time,
        };
        let Some(last) = self.last.replace(now) else {
            self.x = Axis::start(pos[0] as f64, POSITION_NOISE);
            self.y = Axis::start(pos[1] as f64, POSITION_NOISE);
            self.pressure = Axis::start(pressure as f64, PRESSURE_NOISE);
            self.samples = 1;
            return;
        };
        let (dx, dy) = (pos[0] - last.pos[0], pos[1] - last.pos[1]);
        self.last_step = (dx * dx + dy * dy).sqrt();
        // (Samples batched with one time, or a clock gone backwards: as if
        // a millisecond apart.)
        let dt = (time - last.time).clamp(0.0, 0.1).max(0.001);
        self.interval = if self.samples < 2 {
            dt
        } else {
            self.interval * 0.8 + dt * 0.2
        };
        self.x
            .step(pos[0] as f64, dt, POSITION_NOISE, POSITION_JERK);
        self.y
            .step(pos[1] as f64, dt, POSITION_NOISE, POSITION_JERK);
        (self.pressure).step(pressure as f64, dt, PRESSURE_NOISE, PRESSURE_JERK);
        self.samples += 1;
    }

    /// Where the pen will be up to `horizon` seconds after its last sample,
    /// a sample interval apart. Empty before the stroke has samples
    /// enough; just the last sample when the pen is (as good as) still, so
    /// what was drawn ahead before is taken back.
    pub fn predict(&self, horizon: f64) -> Vec<Predicted> {
        let Some(last) = self.last else {
            return Vec::new();
        };
        if self.samples < MIN_SAMPLES {
            return Vec::new();
        }
        let ([px, vx, ax], [py, vy, ay]) = (self.x.s, self.y.s);
        let speed = (vx * vx + vy * vy).sqrt();
        // The pen didn't move since its last sample, or barely moves: it's
        // stopping, and anything ahead would overshoot.
        if speed < MIN_SPEED || self.last_step < 0.01 {
            return vec![last];
        }
        // Slowing down: no farther than where it would come to rest (no
        // swinging back past it).
        let along = (ax * vx + ay * vy) / speed;
        let horizon = if along < 0.0 {
            horizon.min(speed / -along)
        } else {
            horizon
        };
        let interval = self.interval.max(0.001);
        let n = ((horizon / interval).ceil() as usize).clamp(1, MAX_AHEAD);
        let [pp, pv, _] = self.pressure.s;
        let mut out = Vec::with_capacity(n);
        for i in 1..=n {
            let t = (horizon * i as f64 / n as f64).max(0.0);
            let pressure = (pp + pv * t).clamp(0.0, 1.0) as f32;
            if pressure < LIFT_PRESSURE {
                break;
            }
            let at = |p: f64, v: f64, a: f64| (p + v * t + 0.5 * a * t * t) as f32;
            out.push(Predicted {
                pos: [at(px, vx, ax), at(py, vy, ay)],
                pressure,
                time: last.time + t,
            });
        }
        if out.is_empty() {
            out.push(last);
        }
        out
    }

    /// The last real sample (to take back what was drawn ahead, when the
    /// pen has gone quiet).
    pub fn last(&self) -> Option<Predicted> {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 240.0;
    const HORIZON: f64 = 1.0 / 60.0;

    /// Feed `path(t)` (position, pressure) at `RATE` for `seconds`, with a
    /// little measurement noise; after warming up, check each prediction's
    /// end against where the path really is then. Returns the worst miss.
    fn worst_miss(path: impl Fn(f64) -> [f64; 2], seconds: f64) -> f32 {
        let mut p = MotionPredictor::default();
        let mut worst = 0.0f32;
        let n = (seconds * RATE) as usize;
        for i in 0..n {
            let t = i as f64 / RATE;
            let [x, y] = path(t);
            // ±0.2 points of jitter, the same every run.
            let jitter = |k: usize| ((((i * 7 + k * 13) % 11) as f64 / 10.0) - 0.5) * 0.4;
            p.update([(x + jitter(0)) as f32, (y + jitter(1)) as f32], 0.6, t);
            if i < 12 {
                continue;
            }
            let ahead = p.predict(HORIZON);
            let end = ahead.last().expect("predicted");
            let [tx, ty] = path(end.time);
            let miss = ((end.pos[0] - tx as f32).powi(2) + (end.pos[1] - ty as f32).powi(2)).sqrt();
            worst = worst.max(miss);
        }
        worst
    }

    #[test]
    fn a_straight_line_is_predicted_closely() {
        let miss = worst_miss(|t| [100.0 + 900.0 * t, 200.0 + 300.0 * t], 0.5);
        assert!(miss < 1.0, "{miss}");
    }

    #[test]
    fn a_circle_is_predicted_closely() {
        // 80 points across, once round in 0.8 s (about 630 points/s).
        let miss = worst_miss(
            |t| {
                let a = t / 0.8 * std::f64::consts::TAU;
                [300.0 + 80.0 * a.cos(), 300.0 + 80.0 * a.sin()]
            },
            1.0,
        );
        assert!(miss < 2.5, "{miss}");
    }

    #[test]
    fn predictions_reach_about_a_frame_ahead() {
        let mut p = MotionPredictor::default();
        for i in 0..20 {
            let t = i as f64 / RATE;
            p.update([500.0 * t as f32, 0.0], 0.5, t);
        }
        let ahead = p.predict(HORIZON);
        assert!(ahead.len() >= 2 && ahead.len() <= MAX_AHEAD);
        let last = ahead.last().unwrap();
        assert!((last.time - (19.0 / RATE + HORIZON)).abs() < 1e-9);
        // Each farther along than the last.
        assert!(ahead.windows(2).all(|w| w[1].pos[0] > w[0].pos[0]));
    }

    /// The line moves right at `speed` until `stop` seconds, slowing over
    /// `ease` seconds (0: at once), then rests: how far predictions go past
    /// where it rests, made at any time and made once it's at rest.
    fn overshoot(ease: f64) -> (f32, f32) {
        let (speed, stop) = (900.0, 0.3);
        let x = |t: f64| {
            let start = stop - ease;
            if t <= start {
                speed * t
            } else if t < stop {
                // Slowing evenly to rest at `stop`.
                let u = t - start;
                speed * start + speed * u - speed / ease * u * u * 0.5
            } else {
                speed * start + speed * ease * 0.5
            }
        };
        let rest = x(stop + 1.0) as f32;
        let mut p = MotionPredictor::default();
        let (mut worst, mut resting) = (0.0f32, 0.0f32);
        for i in 0..(0.6 * RATE) as usize {
            let t = i as f64 / RATE;
            p.update([x(t) as f32, 50.0], 0.5, t);
            // A sample where the last one was: it's seen to be at rest.
            let seen = i > 0 && x(t) as f32 == x((i - 1) as f64 / RATE) as f32;
            for a in p.predict(HORIZON) {
                let past = a.pos[0] - rest;
                worst = worst.max(past);
                if seen {
                    resting = resting.max(past);
                }
            }
        }
        (worst, resting)
    }

    #[test]
    fn an_abrupt_stop_is_not_overshot_once_seen() {
        // (Up to the stop nothing says it's coming.)
        let (_, resting) = overshoot(0.0);
        assert!(resting <= 0.01, "{resting}");
    }

    #[test]
    fn slowing_to_a_stop_is_hardly_overshot() {
        let (worst, resting) = overshoot(0.06);
        assert!(worst <= 1.5, "{worst}");
        assert!(resting <= 0.01, "{resting}");
    }

    #[test]
    fn a_still_pen_predicts_where_it_is_and_a_new_stroke_starts_over() {
        let mut p = MotionPredictor::default();
        assert!(p.predict(HORIZON).is_empty());
        for i in 0..10 {
            p.update([40.0, 40.0], 0.5, i as f64 / RATE);
        }
        let ahead = p.predict(HORIZON);
        assert_eq!(ahead.len(), 1);
        assert_eq!(ahead[0].pos, [40.0, 40.0]);
        p.reset();
        assert!(
            p.predict(HORIZON).is_empty(),
            "nothing until it has samples"
        );
    }

    #[test]
    fn a_pen_lifting_ends_the_prediction() {
        let mut p = MotionPredictor::default();
        for i in 0..20 {
            let t = i as f64 / RATE;
            // Pressure falling fast toward lifting off.
            let pressure = (0.3 - 4.0 * t).max(0.02) as f32;
            p.update([800.0 * t as f32, 0.0], pressure, t);
        }
        let ahead = p.predict(HORIZON);
        assert!(ahead.iter().all(|a| a.pressure >= LIFT_PRESSURE));
    }
}
