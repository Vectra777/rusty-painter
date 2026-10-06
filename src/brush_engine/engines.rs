//! More brush engines, after Krita's (written from how they behave, not
//! from its code): spray, chalk, curve, grid, tangent normal and particle.
//! Their settings and the pure parts (where a spray's particles land,
//! which grid cells a dab covers, the colour a pen's tilt makes); the
//! stroke lays the dabs down.

use eframe::egui::Vec2;
use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;

/// The settings of every engine here (each read only by its own type).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Engines {
    pub spray: Spray,
    pub chalk: Chalk,
    pub curve: CurveLines,
    pub grid: Grid,
    pub normal: TangentNormal,
    pub particles: Particles,
}

/// A number 0..1 from two (the same two, the same number).
pub fn hash01(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77).rotate_left(16);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Where a spray's particles fall in its circle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Distribution {
    /// Evenly over the circle.
    #[default]
    Uniform,
    /// Thicker in the middle (a bell curve), thinning to the edge.
    Gaussian,
    /// In clumps.
    Clustered,
}

/// Spray: each dab is a cloud of small particles over the brush's circle,
/// each a dab of the brush's own tip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spray {
    /// Particles in each dab (1..=500).
    pub amount: u32,
    pub distribution: Distribution,
    /// A particle's size, as a share of the brush's.
    pub particle_size: f32,
    /// How much smaller a particle can be at random (0..1).
    pub size_random: f32,
    /// Each particle turned at random (an image or square tip).
    pub random_rotation: bool,
}

impl Default for Spray {
    fn default() -> Self {
        Self {
            amount: 40,
            distribution: Distribution::Uniform,
            particle_size: 0.08,
            size_random: 0.5,
            random_rotation: true,
        }
    }
}

/// One spray particle: where (from the dab's centre), its size (a share of
/// the brush's) and turn (radians).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub offset: Vec2,
    pub scale: f32,
    pub angle: f32,
}

impl Spray {
    /// The particles of a dab of `radius`, the same for the same `seed`.
    pub fn particles(&self, seed: u32, radius: f32) -> Vec<Particle> {
        let n = self.amount.clamp(1, 500);
        let mut k = 0u32;
        let mut next = || {
            k += 1;
            hash01(seed, k)
        };
        let disc = |u: f32, v: f32, r: f32| {
            let (s, c) = (v * TAU).sin_cos();
            Vec2::new(c, s) * (r * u.sqrt())
        };
        let gauss = |u: f32, v: f32, sigma: f32| {
            let m = (-2.0 * u.max(1e-6).ln()).sqrt() * sigma;
            let (s, c) = (v * TAU).sin_cos();
            Vec2::new(c, s) * m
        };
        let clusters: Vec<Vec2> = match self.distribution {
            Distribution::Clustered => (0..(n / 8).max(1))
                .map(|_| disc(next(), next(), radius * 0.75))
                .collect(),
            _ => Vec::new(),
        };
        let clamp = |p: Vec2| {
            let l = p.length();
            if l > radius { p * (radius / l) } else { p }
        };
        (0..n)
            .map(|i| {
                let (u, v) = (next(), next());
                let offset = match self.distribution {
                    Distribution::Uniform => disc(u, v, radius),
                    Distribution::Gaussian => clamp(gauss(u, v, radius / 2.5)),
                    Distribution::Clustered => {
                        clamp(clusters[i as usize % clusters.len()] + gauss(u, v, radius / 6.0))
                    }
                };
                let size = self.particle_size.max(0.01)
                    * (1.0 - self.size_random.clamp(0.0, 1.0) * next());
                let angle = if self.random_rotation {
                    next() * TAU
                } else {
                    0.0
                };
                Particle {
                    offset,
                    scale: size,
                    angle,
                }
            })
            .collect()
    }
}

/// Chalk: the tip broken up by a grain each dab lays differently, more of
/// it filled the harder the pen presses (as a stick of chalk on paper).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chalk {
    /// How broken up it is: 0 solid, 1 only where pressed fully.
    pub grain: f32,
    /// The grain's size, canvas pixels.
    pub scale: f32,
}

impl Default for Chalk {
    fn default() -> Self {
        Self {
            grain: 0.6,
            scale: 1.0,
        }
    }
}

impl Chalk {
    /// The grain on one row of a dab's coverage (`alphas`, from canvas
    /// pixel `x0` on row `y`; `seed` tells the dab apart): a pixel keeps its
    /// paint when the grain there is under it, so stronger paint keeps more.
    pub fn apply_row(&self, y: usize, x0: usize, seed: u32, alphas: &mut [f32]) {
        let grain = self.grain.clamp(0.0, 1.0);
        if grain <= 0.0 {
            return;
        }
        let scale = self.scale.max(0.5);
        let gy = (y as f32 / scale) as u32;
        for (i, a) in alphas.iter_mut().enumerate() {
            if *a <= 0.0 {
                continue;
            }
            let gx = ((x0 + i) as f32 / scale) as u32;
            let n = hash01(
                gx ^ seed,
                gy.wrapping_mul(0x01F3_5A7B) ^ seed.rotate_left(7),
            );
            if n > *a * grain + (1.0 - grain) {
                *a = 0.0;
            }
        }
    }
}

/// Curve: instead of dabs, curves from points the stroke passed a while
/// back, through one between, to the pen: loose, swinging lines.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurveLines {
    /// How many points back each curve starts (3..=200).
    pub history: usize,
    /// The curves' width, canvas pixels.
    pub line_width: f32,
    /// Their strength, 0..1.
    pub opacity: f32,
    /// Also a straight line from where each curve starts to the pen.
    pub connection: bool,
}

impl Default for CurveLines {
    fn default() -> Self {
        Self {
            history: 30,
            line_width: 1.0,
            opacity: 0.6,
            connection: false,
        }
    }
}

impl CurveLines {
    /// The curve to the newest of `points` (the stroke's, oldest first):
    /// from, through and to (a quadratic's start, control and end).
    pub fn curve(&self, points: &[Vec2]) -> Option<(Vec2, Vec2, Vec2)> {
        let n = points.len();
        if n < 3 {
            return None;
        }
        let back = self.history.clamp(2, 200).min(n - 1);
        Some((
            points[n - 1 - back],
            points[n - 1 - back / 2],
            points[n - 1],
        ))
    }
}

/// Points along the quadratic `a`, through control `c`, to `b`, about
/// `step` apart (both ends included).
pub fn quadratic(a: Vec2, c: Vec2, b: Vec2, step: f32) -> Vec<Vec2> {
    let length = (c - a).length() + (b - c).length();
    let n = (length / step.max(0.1)).ceil().clamp(1.0, 4096.0) as usize;
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            a * (u * u) + c * (2.0 * u * t) + b * (t * t)
        })
        .collect()
}

/// Grid: the canvas divided into cells; each cell the brush passes over
/// gets one shape of the brush's tip, filling (most of) it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grid {
    /// A cell's side, canvas pixels.
    pub cell: f32,
    /// Where the grid starts, canvas pixels.
    pub offset: [f32; 2],
    /// The shape's size, a share of the cell.
    pub scale: f32,
    /// Each cell's hue turned at random, degrees either way.
    pub hue_jitter: f32,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            cell: 16.0,
            offset: [0.0, 0.0],
            scale: 0.9,
            hue_jitter: 0.0,
        }
    }
}

impl Grid {
    fn side(&self) -> f32 {
        self.cell.max(1.0)
    }

    /// The cells a dab at `center` of `radius` covers part of.
    pub fn cells(&self, center: Vec2, radius: f32) -> Vec<(i32, i32)> {
        let side = self.side();
        let local = center - Vec2::from(self.offset);
        let r = radius.max(0.0);
        let (x0, x1) = (
            ((local.x - r) / side).floor() as i32,
            ((local.x + r) / side).floor() as i32,
        );
        let (y0, y1) = (
            ((local.y - r) / side).floor() as i32,
            ((local.y + r) / side).floor() as i32,
        );
        let mut out = Vec::new();
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                // The cell's nearest point to the centre, inside the circle.
                let near = Vec2::new(
                    local.x.clamp(cx as f32 * side, (cx + 1) as f32 * side),
                    local.y.clamp(cy as f32 * side, (cy + 1) as f32 * side),
                );
                if (near - local).length() <= r {
                    out.push((cx, cy));
                }
            }
        }
        out
    }

    /// The middle of cell `c`, canvas pixels.
    pub fn center(&self, (cx, cy): (i32, i32)) -> Vec2 {
        let side = self.side();
        Vec2::from(self.offset) + Vec2::new((cx as f32 + 0.5) * side, (cy as f32 + 0.5) * side)
    }

    /// A shape's radius in a cell.
    pub fn radius(&self) -> f32 {
        self.side() * 0.5 * self.scale.clamp(0.05, 1.5)
    }
}

/// Tangent normal: paints a normal map, the pen's tilt as the colour (red
/// leaning right, green leaning up, blue upright), for lighting 3D models.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TangentNormal {
    /// Red the other way (leaning left).
    pub flip_x: bool,
    /// Green the other way (leaning down): DirectX's maps, not OpenGL's.
    pub flip_y: bool,
    /// Without tilt (a mouse): how steeply the normal leans the way the
    /// stroke goes, degrees above the canvas (90 upright).
    pub elevation: f32,
}

impl Default for TangentNormal {
    fn default() -> Self {
        Self {
            flip_x: false,
            flip_y: false,
            elevation: 45.0,
        }
    }
}

impl TangentNormal {
    /// The colour (0..1 each) of a normal leaning by `lean` (0 upright, 1
    /// flat) towards `direction` (radians, counter-clockwise from right).
    pub fn color(&self, lean: f32, direction: f32) -> [f32; 3] {
        let lean = lean.clamp(0.0, 1.0);
        let (s, c) = direction.sin_cos();
        let (mut x, mut y) = (c * lean, s * lean);
        if self.flip_x {
            x = -x;
        }
        if self.flip_y {
            y = -y;
        }
        let z = (1.0 - lean * lean).max(0.0).sqrt();
        [x, y, z].map(|v| 0.5 + 0.5 * v)
    }

    /// The lean a mouse's strokes get (from the elevation).
    pub fn mouse_lean(&self) -> f32 {
        self.elevation.clamp(0.0, 90.0).to_radians().cos()
    }
}

/// Particle: a swarm the pen pulls along (each with weight and drag), each
/// drawing its own path: lines that swing and overshoot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Particles {
    /// How many (1..=200).
    pub count: u32,
    /// How strongly the pen pulls them (0..1).
    pub weight: f32,
    /// How much of their speed they lose each step (0..1).
    pub drag: f32,
    /// A steady pull every step, canvas pixels (x right, y down).
    pub gravity: [f32; 2],
    /// Their lines' width, canvas pixels.
    pub line_width: f32,
    /// How far apart they start, a share of the brush's size.
    pub spread: f32,
}

impl Default for Particles {
    fn default() -> Self {
        Self {
            count: 30,
            weight: 0.2,
            drag: 0.15,
            gravity: [0.0, 0.0],
            line_width: 1.0,
            spread: 0.5,
        }
    }
}

/// A particle swarm in flight.
#[derive(Clone, Debug, Default)]
pub struct Swarm {
    pub pos: Vec<Vec2>,
    pub vel: Vec<Vec2>,
}

impl Particles {
    /// A swarm starting around `at`, spread over `radius`, its shape from
    /// `seed`.
    pub fn start(&self, at: Vec2, radius: f32, seed: u32) -> Swarm {
        let n = self.count.clamp(1, 200);
        let spread = radius * self.spread.max(0.0);
        let pos = (0..n)
            .map(|i| {
                let (u, v) = (hash01(seed, 2 * i), hash01(seed, 2 * i + 1));
                let (s, c) = (v * TAU).sin_cos();
                at + Vec2::new(c, s) * (spread * u.sqrt())
            })
            .collect();
        Swarm {
            pos,
            vel: vec![Vec2::ZERO; n as usize],
        }
    }

    /// One step towards `target`: each particle's path this step (from, to).
    pub fn step(&self, swarm: &mut Swarm, target: Vec2) -> Vec<(Vec2, Vec2)> {
        let pull = self.weight.clamp(0.0, 1.0);
        let keep = 1.0 - self.drag.clamp(0.0, 1.0);
        let gravity = Vec2::from(self.gravity);
        (swarm.pos.iter_mut().zip(&mut swarm.vel))
            .map(|(p, v)| {
                *v = *v * keep + (target - *p) * pull + gravity;
                let from = *p;
                *p += *v;
                (from, *p)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spray_particles_stay_in_the_circle_and_repeat_with_the_seed() {
        for distribution in [
            Distribution::Uniform,
            Distribution::Gaussian,
            Distribution::Clustered,
        ] {
            let spray = Spray {
                amount: 300,
                distribution,
                ..Default::default()
            };
            let ps = spray.particles(7, 20.0);
            assert_eq!(ps.len(), 300);
            assert!(
                ps.iter().all(|p| p.offset.length() <= 20.0 + 1e-3),
                "{distribution:?}"
            );
            assert_eq!(ps, spray.particles(7, 20.0), "repeatable");
            assert_ne!(ps, spray.particles(8, 20.0));
            assert!(
                ps.iter()
                    .all(|p| p.scale > 0.0 && p.scale <= spray.particle_size)
            );
        }
        let mean = |d| {
            let s = Spray {
                amount: 400,
                distribution: d,
                ..Default::default()
            };
            s.particles(3, 20.0)
                .iter()
                .map(|p| p.offset.length())
                .sum::<f32>()
                / 400.0
        };
        assert!(
            mean(Distribution::Gaussian) < mean(Distribution::Uniform) * 0.8,
            "denser in the middle"
        );
    }

    #[test]
    fn chalk_keeps_more_where_the_paint_is_stronger() {
        let chalk = Chalk::default();
        let kept = |a: f32| {
            let mut row = vec![a; 1000];
            chalk.apply_row(5, 0, 99, &mut row);
            row.iter().filter(|&&v| v > 0.0).count()
        };
        assert!(kept(0.2) < kept(0.6) && kept(0.6) < kept(1.0));
        assert_eq!(kept(1.0), 1000, "pressed fully: solid");
        let solid = Chalk {
            grain: 0.0,
            ..Default::default()
        };
        let mut row = vec![0.1; 100];
        solid.apply_row(0, 0, 1, &mut row);
        assert!(row.iter().all(|&v| v == 0.1));
    }

    #[test]
    fn a_curve_runs_from_a_while_back_to_the_pen() {
        let lines = CurveLines {
            history: 4,
            ..Default::default()
        };
        let pts: Vec<Vec2> = (0..10)
            .map(|i| Vec2::new(i as f32, (i * i) as f32))
            .collect();
        assert_eq!(lines.curve(&pts[..2]), None);
        let (a, c, b) = lines.curve(&pts).unwrap();
        assert_eq!((a, c, b), (pts[5], pts[7], pts[9]));
        let q = quadratic(a, c, b, 1.0);
        assert_eq!((q[0], *q.last().unwrap()), (a, b));
    }

    #[test]
    fn grid_cells_are_the_ones_the_dab_reaches() {
        let grid = Grid {
            cell: 10.0,
            ..Default::default()
        };
        assert_eq!(grid.cells(Vec2::new(5.0, 5.0), 2.0), [(0, 0)]);
        let cells = grid.cells(Vec2::new(10.0, 10.0), 3.0);
        assert_eq!(cells.len(), 4, "a corner: four cells");
        assert_eq!(grid.center((1, 2)), Vec2::new(15.0, 25.0));
        let shifted = Grid {
            offset: [3.0, 0.0],
            ..grid
        };
        assert_eq!(shifted.center((0, 0)), Vec2::new(8.0, 5.0));
        // A big dab: no cell twice.
        let mut many = grid.cells(Vec2::new(50.0, 50.0), 35.0);
        let n = many.len();
        many.sort_unstable();
        many.dedup();
        assert_eq!(many.len(), n);
    }

    #[test]
    fn an_upright_pen_paints_flat_blue_and_a_lean_tints_it() {
        let normal = TangentNormal::default();
        let up = normal.color(0.0, 0.0).map(|v| (v * 255.0).round() as u8);
        assert_eq!(up, [128, 128, 255]);
        let right = normal.color(0.7, 0.0);
        assert!(right[0] > 0.8 && (right[1] - 0.5).abs() < 1e-6 && right[2] < 1.0);
        let upward = normal.color(0.7, std::f32::consts::FRAC_PI_2);
        assert!(upward[1] > 0.8);
        let directx = TangentNormal {
            flip_y: true,
            ..normal
        };
        assert!(directx.color(0.7, std::f32::consts::FRAC_PI_2)[1] < 0.2);
        assert!((normal.mouse_lean() - 0.5f32.sqrt()).abs() < 1e-4);
    }

    #[test]
    fn particles_are_pulled_after_the_pen() {
        let p = Particles::default();
        let mut swarm = p.start(Vec2::ZERO, 10.0, 5);
        let target = Vec2::new(100.0, 0.0);
        let far = |s: &Swarm| s.pos.iter().map(|q| (target - *q).length()).sum::<f32>();
        let before = far(&swarm);
        for _ in 0..20 {
            let paths = p.step(&mut swarm, target);
            assert_eq!(paths.len(), swarm.pos.len());
        }
        assert!(far(&swarm) < before * 0.3);
        // Gravity pulls them off the line.
        let heavy = Particles {
            gravity: [0.0, 3.0],
            ..p
        };
        let mut s = heavy.start(Vec2::ZERO, 0.0, 1);
        for _ in 0..5 {
            heavy.step(&mut s, Vec2::ZERO);
        }
        assert!(s.pos.iter().all(|q| q.y > 0.0));
    }
}
