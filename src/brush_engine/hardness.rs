//! Brush softness: the falloff from the dab centre to its edge, as a
//! preset or an editable curve.

/// Option for how the brush softness falloff is calculated.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SoftnessSelector {
    Gaussian,
    Curve,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

impl CurvePoint {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SoftnessCurve {
    pub points: Vec<CurvePoint>,
}

impl Default for SoftnessCurve {
    fn default() -> Self {
        Self {
            points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 0.0)],
        }
    }
}

/// A softness curve tabulated for painting: [`SoftnessCurve::eval`]'s
/// spline per pixel costs as much as the rest of a soft dab together.
pub(crate) struct CurveLut {
    table: Vec<f32>,
}

impl CurveLut {
    /// Steps across 0..1; linear between them (well under a level of 8-bit
    /// alpha from the spline).
    const STEPS: usize = 1024;

    pub(crate) fn new(curve: &SoftnessCurve) -> Self {
        Self {
            table: (0..=Self::STEPS)
                .map(|i| curve.eval(i as f32 / Self::STEPS as f32))
                .collect(),
        }
    }

    #[inline]
    pub(crate) fn at(&self, t: f32) -> f32 {
        let x = t.clamp(0.0, 1.0) * Self::STEPS as f32;
        let i = (x as usize).min(Self::STEPS - 1);
        let f = x - i as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }
}

impl SoftnessCurve {
    pub fn eval(&self, t: f32) -> f32 {
        if self.points.is_empty() {
            return 0.0;
        }
        // Clamp t to 0..1 just in case
        let t = t.clamp(0.0, 1.0);

        let len = self.points.len();
        if len == 1 {
            return self.points[0].y;
        }

        if t <= self.points[0].x {
            return self.points[0].y;
        }
        if t >= self.points[len - 1].x {
            return self.points[len - 1].y;
        }

        // Find the segment i such that points[i].x <= t <= points[i+1].x
        let mut i = 0;
        // Since points are sorted and N is small, linear scan is fine.
        // If N grows large, use binary search.
        for idx in 0..len - 1 {
            if t >= self.points[idx].x && t <= self.points[idx + 1].x {
                i = idx;
                break;
            }
        }

        // Monotone Cubic Hermite Interpolation
        // p0 = points[i], p1 = points[i+1]
        let p0 = &self.points[i];
        let p1 = &self.points[i + 1];

        let dx = p1.x - p0.x;
        if dx.abs() < 1e-6 {
            return p0.y;
        }

        // Calculate slopes (tangents)
        // m0 = slope at p0, m1 = slope at p1
        // Secants
        let secant0 = if i > 0 {
            let pm1 = &self.points[i - 1];
            (p0.y - pm1.y) / (p0.x - pm1.x)
        } else {
            (p1.y - p0.y) / dx // One-sided difference for start
        };

        let secant1 = (p1.y - p0.y) / dx;

        let secant2 = if i < len - 2 {
            let pp2 = &self.points[i + 2];
            (pp2.y - p1.y) / (pp2.x - p1.x)
        } else {
            secant1 // One-sided difference for end
        };

        // Tangents (using simple finite difference or centripetal)
        // Standard Monotone checks:
        // If secant k-1 and secant k have different signs, tangent is 0.
        // Else, tangent is arithmetic mean (simple) or harmonic mean (Fritsch-Butland).

        let tangent = |sec_prev: f32, sec_next: f32| -> f32 {
            if sec_prev * sec_next <= 0.0 {
                0.0
            } else {
                (sec_prev + sec_next) * 0.5
            }
        };

        let m0 = if i == 0 {
            secant1 // Start point
        } else {
            tangent(secant0, secant1)
        };

        let m1 = if i == len - 2 {
            secant1 // End point
        } else {
            tangent(secant1, secant2)
        };

        // Evaluate cubic hermite
        let t_local = (t - p0.x) / dx;
        let t2 = t_local * t_local;
        let t3 = t2 * t_local;

        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t_local;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;

        p0.y * h00 + m0 * dx * h10 + p1.y * h01 + m1 * dx * h11
    }
}

/// How a curve tip's falloff gets softer for a dab whose Softness input
/// is below full (0.1..1).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Softening {
    /// The curve's inner points scaled down.
    #[default]
    Curve,
    /// A round (or square) fade, solid out to this share of the
    /// radius: a softer dab shrinks the solid core.
    Fade(f32),
    /// A soft circle: its own curve, read at the squared
    /// distance from the centre, inner points scaled down.
    SquaredCurve(SoftnessCurve),
}

/// Points an imported falloff is sampled at.
const FALLOFF_SAMPLES: usize = 24;

impl Softening {
    /// The falloff at softness `s` (1: as it is), given the brush's curve.
    pub fn falloff(&self, curve: &SoftnessCurve, s: f32) -> SoftnessCurve {
        let s = s.clamp(0.01, 1.0);
        match self {
            Softening::Curve => curve.softened(s),
            // The fade shrinks with the softness: solid inside the fade, then falling with the
            // squared distance.
            Softening::Fade(core) => {
                let f = (core * s).clamp(0.0, 1.0);
                sampled(|r| {
                    let n = r * r;
                    if f >= 0.999 || n <= f * f {
                        1.0
                    } else {
                        1.0 - (n - f * f) / (1.0 - f * f)
                    }
                })
            }
            Softening::SquaredCurve(krita) => {
                let krita = krita.softened(s);
                sampled(|r| krita.eval(r * r))
            }
        }
    }
}

/// `f` (strength by distance from the centre, 0..1) as a curve.
pub(crate) fn sampled(f: impl Fn(f32) -> f32) -> SoftnessCurve {
    SoftnessCurve {
        points: (0..=FALLOFF_SAMPLES)
            .map(|i| {
                let r = i as f32 / FALLOFF_SAMPLES as f32;
                CurvePoint::new(r, f(r).clamp(0.0, 1.0))
            })
            .collect(),
    }
}

impl SoftnessCurve {
    /// Every point but the ends
    /// scaled by `s` (a straight line gets a middle point to scale).
    pub fn softened(&self, s: f32) -> SoftnessCurve {
        if s >= 1.0 {
            return self.clone();
        }
        let mut points = self.points.clone();
        if let [a, b] = &points[..] {
            let (a, b) = (a.clone(), b.clone());
            points = vec![
                a.clone(),
                CurvePoint::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5),
                b,
            ];
        }
        let n = points.len();
        for p in points.iter_mut().take(n.saturating_sub(1)).skip(1) {
            p.y = (p.y * s).clamp(0.0, 1.0);
        }
        SoftnessCurve { points }
    }
}

#[cfg(test)]
mod lut_tests {
    use super::*;

    #[test]
    fn softening_follows_krita_masks() {
        // A curve's inner points scale; a straight line gets a middle one.
        let line = SoftnessCurve::default().softened(0.5);
        assert_eq!(line.points.len(), 3);
        assert_eq!(line.points[1], CurvePoint::new(0.5, 0.25));
        assert_eq!((line.points[0].y, line.points[2].y), (1.0, 0.0));
        // A fade's solid core shrinks with the softness.
        let fade = Softening::Fade(0.6);
        let (full, half) = (
            fade.falloff(&SoftnessCurve::default(), 1.0),
            fade.falloff(&SoftnessCurve::default(), 0.5),
        );
        assert_eq!(full.eval(0.5), 1.0);
        assert!(half.eval(0.5) < 0.9, "{}", half.eval(0.5));
        assert_eq!(half.eval(0.25), 1.0);
    }

    #[test]
    fn the_table_follows_the_curve() {
        let curve = SoftnessCurve {
            points: vec![
                CurvePoint::new(0.0, 0.4),
                CurvePoint::new(0.43, 0.12),
                CurvePoint::new(1.0, 0.0),
            ],
        };
        let lut = CurveLut::new(&curve);
        for i in 0..=1000 {
            let t = i as f32 / 1000.0;
            assert!((lut.at(t) - curve.eval(t)).abs() < 1e-3, "{t}");
        }
    }
}
