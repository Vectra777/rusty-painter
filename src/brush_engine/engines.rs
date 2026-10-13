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
    /// Density mode: as many particles as cover this share of the area
    /// (bigger sprays get more), in place of `amount`; 0 is off.
    pub coverage: f32,
    /// The spray area's height to width (1 a circle).
    pub aspect: f32,
    /// The spray area's turn, degrees.
    pub rotation: f32,
    /// How far the whole cloud jumps about from dab to dab, a share of
    /// the radius (0 stays on the stroke).
    pub jitter: f32,
    /// Each particle's colour shifted at random by up to this much: hue
    /// (degrees), saturation and value (0..1); all zero is off.
    pub random_hsv: [f32; 3],
    /// Each particle's opacity at random.
    pub random_opacity: bool,
    /// Each particle mixed with the secondary colour at random.
    pub mix_secondary: bool,
}

impl Default for Spray {
    fn default() -> Self {
        Self {
            amount: 40,
            distribution: Distribution::Uniform,
            particle_size: 0.08,
            size_random: 0.5,
            random_rotation: true,
            coverage: 0.0,
            aspect: 1.0,
            rotation: 0.0,
            jitter: 0.0,
            random_hsv: [0.0; 3],
            random_opacity: false,
            mix_secondary: false,
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
    /// Its colour shift (hue degrees, saturation, value), opacity factor
    /// and share of the secondary colour.
    pub hsv: [f32; 3],
    pub opacity: f32,
    pub mix: f32,
}

impl Spray {
    /// How many particles a dab has.
    pub fn count(&self) -> u32 {
        if self.coverage > 0.0 {
            // A particle covers `particle_size`² of the area.
            let share = self.particle_size.max(0.01).powi(2);
            ((self.coverage.min(1.0) / share).round() as u32).clamp(1, 500)
        } else {
            self.amount.clamp(1, 500)
        }
    }

    /// The particles of a dab of `radius`, the same for the same `seed`.
    pub fn particles(&self, seed: u32, radius: f32) -> Vec<Particle> {
        let n = self.count();
        let mut k = 0u32;
        let mut next = || {
            k += 1;
            hash01(seed, k)
        };
        let disc = |u: f32, v: f32, r: f32| {
            let (s, c) = (v * TAU).sin_cos();
            Vec2::new(c, s) * (r * u.sqrt())
        };
        // A bell curve of `sigma` cut off at `reach` (sampled within it,
        // not moved onto its edge: that would ring the circle).
        let gauss = |u: f32, v: f32, sigma: f32, reach: f32| {
            let sigma = sigma.max(1e-6);
            let inside = 1.0 - (-reach * reach / (2.0 * sigma * sigma)).exp();
            let m = (-2.0 * (1.0 - u * inside).max(1e-12).ln()).sqrt() * sigma;
            let (s, c) = (v * TAU).sin_cos();
            Vec2::new(c, s) * m.min(reach)
        };
        let clusters: Vec<Vec2> = match self.distribution {
            Distribution::Clustered => (0..(n / 8).max(1))
                .map(|_| disc(next(), next(), radius * 0.75))
                .collect(),
            _ => Vec::new(),
        };
        // The area's shape and turn, and where the cloud jumped to.
        let (aspect, (rs, rc)) = (
            self.aspect.clamp(0.05, 20.0),
            self.rotation.to_radians().sin_cos(),
        );
        let shift = if self.jitter > 0.0 {
            disc(next(), next(), radius * self.jitter)
        } else {
            Vec2::ZERO
        };
        let [dh, ds, dv] = self.random_hsv;
        (0..n)
            .map(|i| {
                let (u, v) = (next(), next());
                let offset = match self.distribution {
                    Distribution::Uniform => disc(u, v, radius),
                    Distribution::Gaussian => gauss(u, v, radius / 2.5, radius),
                    Distribution::Clustered => {
                        let c = clusters[i as usize % clusters.len()];
                        c + gauss(u, v, radius / 6.0, radius - c.length())
                    }
                };
                let size = self.particle_size.max(0.01)
                    * (1.0 - self.size_random.clamp(0.0, 1.0) * next());
                let angle = if self.random_rotation {
                    next() * TAU
                } else {
                    0.0
                };
                let offset = Vec2::new(offset.x, offset.y * aspect);
                let offset =
                    Vec2::new(offset.x * rc - offset.y * rs, offset.x * rs + offset.y * rc) + shift;
                let mut signed = || next() * 2.0 - 1.0;
                let hsv = [dh * signed(), ds * signed(), dv * signed()];
                let opacity = if self.random_opacity { next() } else { 1.0 };
                let mix = if self.mix_secondary { next() } else { 0.0 };
                Particle {
                    offset,
                    scale: size,
                    angle,
                    hsv,
                    opacity,
                    mix,
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
    /// A cell's height, canvas pixels (0: square, as wide as `cell`).
    pub cell_height: f32,
    /// Each cell divided into this many across and down (1 = whole).
    pub divisions: u32,
    /// The divisions grow with the pen's pressure (from whole cells at the
    /// lightest to `divisions` at the hardest).
    pub divide_by_pressure: bool,
    /// How much each shape shrinks at random (0..1).
    pub random_border: f32,
    /// Cells are painted again by every dab over them (they build up),
    /// not once a stroke.
    pub repaint: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            cell: 16.0,
            offset: [0.0, 0.0],
            scale: 0.9,
            hue_jitter: 0.0,
            cell_height: 0.0,
            divisions: 1,
            divide_by_pressure: false,
            random_border: 0.0,
            repaint: false,
        }
    }
}

/// A grid cell: its column and row, and how finely it's divided.
pub type GridCell = (i32, i32, u32);

impl Grid {
    /// How finely cells are divided at `pressure`.
    pub fn division(&self, pressure: f32) -> u32 {
        let most = self.divisions.clamp(1, 16);
        if self.divide_by_pressure {
            1 + ((most - 1) as f32 * pressure.clamp(0.0, 1.0)).round() as u32
        } else {
            most
        }
    }

    /// A (divided) cell's width and height.
    fn sides(&self, division: u32) -> Vec2 {
        let w = self.cell.max(1.0);
        let h = if self.cell_height > 0.0 {
            self.cell_height.max(1.0)
        } else {
            w
        };
        Vec2::new(w, h) / division.max(1) as f32
    }

    /// The cells (divided `division` times) a dab at `center` of `radius`
    /// covers part of.
    pub fn cells(&self, center: Vec2, radius: f32, division: u32) -> Vec<GridCell> {
        let side = self.sides(division);
        let local = center - Vec2::from(self.offset);
        let r = radius.max(0.0);
        let (x0, x1) = (
            ((local.x - r) / side.x).floor() as i32,
            ((local.x + r) / side.x).floor() as i32,
        );
        let (y0, y1) = (
            ((local.y - r) / side.y).floor() as i32,
            ((local.y + r) / side.y).floor() as i32,
        );
        let mut out = Vec::new();
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                // The cell's nearest point to the centre, inside the circle.
                let near = Vec2::new(
                    local.x.clamp(cx as f32 * side.x, (cx + 1) as f32 * side.x),
                    local.y.clamp(cy as f32 * side.y, (cy + 1) as f32 * side.y),
                );
                if (near - local).length() <= r {
                    out.push((cx, cy, division));
                }
            }
        }
        out
    }

    /// The middle of `cell`, canvas pixels.
    pub fn center(&self, (cx, cy, division): GridCell) -> Vec2 {
        let side = self.sides(division);
        Vec2::from(self.offset) + Vec2::new((cx as f32 + 0.5) * side.x, (cy as f32 + 0.5) * side.y)
    }

    /// A shape's radius across and its height to width in `cell` (shrunk
    /// at random by the random border).
    pub fn shape(&self, cell: GridCell) -> (f32, f32) {
        let side = self.sides(cell.2);
        let shrink = 1.0
            - self.random_border.clamp(0.0, 1.0) * hash01(cell.0 as u32 ^ 0x5bd1, cell.1 as u32);
        (
            side.x * 0.5 * self.scale.clamp(0.05, 1.5) * shrink,
            side.y / side.x,
        )
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
    /// How differently each answers the pen's pull (0 all alike, 1 from
    /// none to twice the weight).
    pub weight_spread: f32,
    /// Dots where they are each step, not lines along their paths.
    pub dots: bool,
    /// Steps they take for each dab (1..=30).
    pub iterations: u32,
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
            weight_spread: 0.0,
            dots: false,
            iterations: 1,
        }
    }
}

/// A particle swarm in flight.
#[derive(Clone, Debug, Default)]
pub struct Swarm {
    pub pos: Vec<Vec2>,
    pub vel: Vec<Vec2>,
    /// Each particle's pull factor.
    pub pull: Vec<f32>,
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
        let spread = self.weight_spread.clamp(0.0, 1.0);
        let pull = (0..n)
            .map(|i| 1.0 + spread * (hash01(seed ^ 0x9e37, i) * 2.0 - 1.0))
            .collect();
        Swarm {
            pos,
            vel: vec![Vec2::ZERO; n as usize],
            pull,
        }
    }

    /// The steps towards `target` for one dab: each particle's path each
    /// step (from, to).
    pub fn step(&self, swarm: &mut Swarm, target: Vec2) -> Vec<(Vec2, Vec2)> {
        let pull = self.weight.clamp(0.0, 1.0);
        let keep = 1.0 - self.drag.clamp(0.0, 1.0);
        let gravity = Vec2::from(self.gravity);
        let mut out = Vec::new();
        for _ in 0..self.iterations.clamp(1, 30) {
            for ((p, v), k) in swarm.pos.iter_mut().zip(&mut swarm.vel).zip(&swarm.pull) {
                *v = *v * keep + (target - *p) * (pull * k) + gravity;
                let from = *p;
                *p += *v;
                out.push((from, *p));
            }
        }
        out
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
        // None piled up on the edge (as if pushed in from past it).
        for d in [Distribution::Gaussian, Distribution::Clustered] {
            let s = Spray {
                amount: 500,
                distribution: d,
                ..Default::default()
            };
            let rim = (s.particles(9, 20.0).iter())
                .filter(|p| p.offset.length() > 19.9)
                .count();
            assert!(rim <= 2, "{d:?}: {rim} on the rim");
        }
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
        assert_eq!(grid.cells(Vec2::new(5.0, 5.0), 2.0, 1), [(0, 0, 1)]);
        let cells = grid.cells(Vec2::new(10.0, 10.0), 3.0, 1);
        assert_eq!(cells.len(), 4, "a corner: four cells");
        assert_eq!(grid.center((1, 2, 1)), Vec2::new(15.0, 25.0));
        let shifted = Grid {
            offset: [3.0, 0.0],
            ..grid
        };
        assert_eq!(shifted.center((0, 0, 1)), Vec2::new(8.0, 5.0));
        // A big dab: no cell twice.
        let mut many = grid.cells(Vec2::new(50.0, 50.0), 35.0, 1);
        let n = many.len();
        many.sort_unstable();
        many.dedup();
        assert_eq!(many.len(), n);
    }

    #[test]
    fn grid_cells_can_be_tall_divided_by_pressure_and_shrunk_at_random() {
        let grid = Grid {
            cell: 10.0,
            cell_height: 30.0,
            divisions: 3,
            divide_by_pressure: true,
            ..Default::default()
        };
        assert_eq!(
            (grid.division(0.0), grid.division(0.5), grid.division(1.0)),
            (1, 2, 3)
        );
        // Tall cells: a dab at (5, 25) is in the first one.
        assert_eq!(grid.cells(Vec2::new(5.0, 25.0), 1.0, 1), [(0, 0, 1)]);
        assert_eq!(grid.center((0, 0, 1)), Vec2::new(5.0, 15.0));
        let (r, aspect) = grid.shape((0, 0, 1));
        assert!((r - 4.5).abs() < 1e-4 && (aspect - 3.0).abs() < 1e-4);
        // Divided in three: a third the size.
        assert_eq!(grid.center((0, 0, 3)), Vec2::new(10.0 / 6.0, 5.0));
        let bordered = Grid {
            random_border: 1.0,
            ..grid
        };
        let radii: Vec<f32> = (0..20).map(|i| bordered.shape((i, 0, 1)).0).collect();
        assert!(radii.iter().all(|&r| (0.0..=4.5).contains(&r)));
        assert!(radii.iter().any(|&r| r < 3.0), "{radii:?}");
    }

    #[test]
    fn a_spray_can_go_by_coverage_be_stretched_jitter_and_vary_its_particles() {
        let spray = Spray {
            coverage: 0.5,
            particle_size: 0.1,
            ..Default::default()
        };
        assert_eq!(
            spray.count(),
            50,
            "half the area in particles a tenth across"
        );
        // Stretched tall: further up and down than across.
        let tall = Spray {
            amount: 400,
            aspect: 3.0,
            ..Default::default()
        };
        let ps = tall.particles(3, 10.0);
        let reach = |f: fn(&Particle) -> f32| ps.iter().map(f).fold(0.0f32, f32::max);
        assert!(reach(|p| p.offset.y.abs()) > 2.0 * reach(|p| p.offset.x.abs()));
        // Turned a quarter: the other way round.
        let turned = Spray {
            rotation: 90.0,
            ..tall
        };
        let ps = turned.particles(3, 10.0);
        assert!(
            ps.iter().map(|p| p.offset.x.abs()).fold(0.0f32, f32::max)
                > 2.0 * ps.iter().map(|p| p.offset.y.abs()).fold(0.0f32, f32::max)
        );
        // Jitter moves the whole cloud: its middle is off the dab's.
        let jittery = Spray {
            amount: 200,
            jitter: 1.0,
            ..Default::default()
        };
        let middles: Vec<Vec2> = (0..8)
            .map(|seed| {
                let ps = jittery.particles(seed, 10.0);
                ps.iter().map(|p| p.offset).fold(Vec2::ZERO, |a, b| a + b) / ps.len() as f32
            })
            .collect();
        assert!(middles.iter().any(|m| m.length() > 2.0), "{middles:?}");
        // Colours, opacity and mixing, each within its range.
        let colourful = Spray {
            amount: 100,
            random_hsv: [30.0, 0.2, 0.1],
            random_opacity: true,
            mix_secondary: true,
            ..Default::default()
        };
        let ps = colourful.particles(1, 10.0);
        assert!(
            ps.iter()
                .all(|p| p.hsv[0].abs() <= 30.0 && p.hsv[1].abs() <= 0.2)
        );
        assert!(ps.iter().any(|p| p.hsv[0] > 10.0) && ps.iter().any(|p| p.hsv[0] < -10.0));
        assert!(
            ps.iter()
                .all(|p| (0.0..=1.0).contains(&p.opacity) && (0.0..=1.0).contains(&p.mix))
        );
        assert!(
            Spray::default()
                .particles(1, 10.0)
                .iter()
                .all(|p| p.opacity == 1.0 && p.mix == 0.0)
        );
    }

    #[test]
    fn particles_answer_the_pull_at_their_own_rates_and_step_several_times() {
        let p = Particles {
            count: 20,
            spread: 0.0,
            weight_spread: 1.0,
            iterations: 3,
            ..Default::default()
        };
        let mut swarm = p.start(Vec2::ZERO, 10.0, 4);
        assert!(
            swarm.pos.iter().all(|&q| q == Vec2::ZERO),
            "all start at the pen"
        );
        let paths = p.step(&mut swarm, Vec2::new(100.0, 0.0));
        assert_eq!(paths.len(), 60, "three steps each");
        let mut xs: Vec<f32> = swarm.pos.iter().map(|q| q.x).collect();
        xs.sort_by(f32::total_cmp);
        assert!(xs[19] > 2.0 * xs[0] + 1.0, "some keep up, some lag: {xs:?}");
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
