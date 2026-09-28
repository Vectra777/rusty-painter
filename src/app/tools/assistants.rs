//! Drawing assistants (Krita's assistants, Clip Studio's perspective and
//! special rulers): guides a stroke snaps to, besides the straight ruler.
//!
//! - **Vanishing point**: strokes run straight towards (or away from) it.
//! - **Perspective**: a plane drawn in two-point perspective, set by the
//!   four corners of a rectangle seen at an angle; strokes run towards
//!   either of its two vanishing points, or upright, whichever the stroke
//!   starts closest to.
//! - **Ellipse**: a stroke starting near it runs round it.
//! - **Concentric**: strokes run round ellipses of its shape nested inside
//!   and outside it, through wherever they start.
//!
//! When the pen goes down the stroke picks what it follows ([`Lock`]);
//! for directions (vanishing points) it waits until the pen has moved a
//! little to see which way it's going.

use eframe::egui::Vec2;

/// What an assistant is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AssistantKind {
    VanishingPoint,
    Perspective,
    Ellipse,
    Concentric,
}

impl AssistantKind {
    pub const ALL: [AssistantKind; 4] = [
        Self::VanishingPoint,
        Self::Perspective,
        Self::Ellipse,
        Self::Concentric,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::VanishingPoint => "Vanishing point",
            Self::Perspective => "Perspective",
            Self::Ellipse => "Ellipse",
            Self::Concentric => "Concentric",
        }
    }

    /// How many handles it has.
    pub fn handles(self) -> usize {
        match self {
            Self::VanishingPoint => 1,
            Self::Perspective => 4,
            Self::Ellipse | Self::Concentric => 3,
        }
    }
}

/// One assistant on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Assistant {
    pub kind: AssistantKind,
    /// Its handles, canvas coordinates (the first [`AssistantKind::handles`]
    /// are used):
    /// - vanishing point: the point;
    /// - perspective: the plane's corners, in order round it;
    /// - ellipse, concentric: the centre, the end of one axis, and the end
    ///   of the other (kept square to the first).
    pub points: [[f32; 2]; 4],
    pub enabled: bool,
}

impl Assistant {
    /// A new assistant of `kind`, sized to fit a `w` × `h` canvas.
    pub fn new(kind: AssistantKind, w: f32, h: f32) -> Self {
        let (cx, cy) = (w * 0.5, h * 0.5);
        let s = w.min(h);
        let points = match kind {
            AssistantKind::VanishingPoint => [[cx, h * 0.4], [0.0; 2], [0.0; 2], [0.0; 2]],
            AssistantKind::Perspective => [
                [cx - s * 0.3, cy - s * 0.1],
                [cx + s * 0.25, cy - s * 0.15],
                [cx + s * 0.25, cy + s * 0.2],
                [cx - s * 0.3, cy + s * 0.12],
            ],
            AssistantKind::Ellipse | AssistantKind::Concentric => {
                [[cx, cy], [cx + s * 0.3, cy], [cx, cy - s * 0.15], [0.0; 2]]
            }
        };
        Self {
            kind,
            points,
            enabled: true,
        }
    }

    pub fn point(&self, i: usize) -> Vec2 {
        Vec2::new(self.points[i][0], self.points[i][1])
    }

    pub fn set_point(&mut self, i: usize, p: Vec2) {
        self.points[i] = [p.x, p.y];
    }

    /// Move handle `i` to `p`: an ellipse's centre carries the rest along,
    /// and its second axis stays square to the first.
    pub fn drag_handle(&mut self, i: usize, p: Vec2) {
        match self.kind {
            AssistantKind::Ellipse | AssistantKind::Concentric => {
                let c = self.point(0);
                match i {
                    0 => {
                        let d = p - c;
                        for k in 0..3 {
                            let q = self.point(k) + d;
                            self.set_point(k, q);
                        }
                    }
                    1 => {
                        // Turning the first axis turns the second with it.
                        let b = (self.point(2) - c).length();
                        let dir = (p - c).normalized();
                        self.set_point(1, p);
                        self.set_point(2, c + Vec2::new(dir.y, -dir.x) * b);
                    }
                    _ => {
                        let dir = (self.point(1) - c).normalized();
                        let normal = Vec2::new(dir.y, -dir.x);
                        let b = (p - c).dot(normal).abs().max(1.0);
                        self.set_point(2, c + normal * b);
                    }
                }
            }
            _ => self.set_point(i, p),
        }
    }

    /// Its ellipse, for an ellipse or concentric assistant.
    pub fn ellipse(&self) -> Option<Ellipse> {
        if !matches!(
            self.kind,
            AssistantKind::Ellipse | AssistantKind::Concentric
        ) {
            return None;
        }
        let c = self.point(0);
        let major = self.point(1) - c;
        let a = major.length();
        let b = (self.point(2) - c).length();
        (a > 1e-3 && b > 1e-3).then(|| Ellipse {
            center: c,
            angle: major.y.atan2(major.x),
            a,
            b,
        })
    }

    /// A perspective plane's two vanishing points (where its opposite
    /// sides meet), those that exist.
    pub fn vanishing_points(&self) -> Vec<Vec2> {
        match self.kind {
            AssistantKind::VanishingPoint => vec![self.point(0)],
            AssistantKind::Perspective => {
                let p: Vec<Vec2> = (0..4).map(|i| self.point(i)).collect();
                [(0, 1, 3, 2), (0, 3, 1, 2)]
                    .into_iter()
                    .filter_map(|(a, b, c, d)| intersect(p[a], p[b], p[c], p[d]))
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Where the lines through `a`, `b` and through `c`, `d` cross (none if
/// they're parallel, or cross absurdly far away).
fn intersect(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<Vec2> {
    let (r, s) = (b - a, d - c);
    let denom = r.x * s.y - r.y * s.x;
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = ((c - a).x * s.y - (c - a).y * s.x) / denom;
    let p = a + r * t;
    (p.length() < 1e7).then_some(p)
}

/// An ellipse: centre, the angle of its first axis, and its radii.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ellipse {
    pub center: Vec2,
    pub angle: f32,
    pub a: f32,
    pub b: f32,
}

impl Ellipse {
    /// `p` in the ellipse's own frame, scaled so the ellipse is the unit
    /// circle.
    fn unit_coords(&self, p: Vec2) -> Vec2 {
        let d = p - self.center;
        let (s, c) = self.angle.sin_cos();
        Vec2::new((c * d.x + s * d.y) / self.a, (-s * d.x + c * d.y) / self.b)
    }

    /// The point at `theta` round it (radians, in its own frame).
    pub fn at(&self, theta: f32) -> Vec2 {
        let (s, c) = self.angle.sin_cos();
        let (x, y) = (self.a * theta.cos(), self.b * theta.sin());
        self.center + Vec2::new(c * x - s * y, s * x + c * y)
    }

    /// Where `p` is round it (radians).
    pub fn theta(&self, p: Vec2) -> f32 {
        let u = self.unit_coords(p);
        u.y.atan2(u.x)
    }

    /// How far `p` is from it, roughly (exact on the axes), canvas pixels.
    pub fn distance(&self, p: Vec2) -> f32 {
        (p - self.at(self.theta(p))).length()
    }

    /// The same shape, scaled about its centre to pass through `p`.
    pub fn through(&self, p: Vec2) -> Ellipse {
        let k = self.unit_coords(p).length().max(1e-3);
        Ellipse {
            a: self.a * k,
            b: self.b * k,
            ..*self
        }
    }
}

/// What a stroke follows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Constraint {
    /// The line through `origin` along `dir` (a unit vector).
    Line {
        origin: Vec2,
        dir: Vec2,
    },
    Ellipse(Ellipse),
}

impl Constraint {
    /// `p` put on it.
    pub fn project(&self, p: Vec2) -> Vec2 {
        match *self {
            Constraint::Line { origin, dir } => origin + dir * (p - origin).dot(dir),
            Constraint::Ellipse(e) => e.at(e.theta(p)),
        }
    }
}

/// The current stroke's hold on a guide.
#[derive(Clone, Debug, PartialEq)]
pub enum Lock {
    /// It follows this.
    Fixed(Constraint),
    /// It will follow whichever of these lines (all through `start`) is
    /// closest to the way it goes, once it has moved far enough to tell.
    Choosing { start: Vec2, dirs: Vec<Vec2> },
}

/// How far (screen points) the pen moves before a stroke picks its
/// direction.
pub const CHOOSE_AFTER: f32 = 6.0;

impl Lock {
    /// `p` put on what the stroke follows; a choosing lock settles once `p`
    /// is far enough (`far`, canvas pixels) from the start.
    pub fn snap(&mut self, p: Vec2, far: f32) -> Vec2 {
        match self {
            Lock::Fixed(c) => c.project(p),
            Lock::Choosing { start, dirs } => {
                let (start, off) = (*start, p - *start);
                if off.length() < far || dirs.is_empty() {
                    return start;
                }
                let dir = *dirs
                    .iter()
                    .max_by(|a, b| {
                        off.dot(**a)
                            .abs()
                            .partial_cmp(&off.dot(**b).abs())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .expect("some direction");
                let c = Constraint::Line { origin: start, dir };
                *self = Lock::Fixed(c);
                c.project(p)
            }
        }
    }

    /// The ellipse the stroke goes round, if it does.
    pub fn ellipse(&self) -> Option<Ellipse> {
        match self {
            Lock::Fixed(Constraint::Ellipse(e)) => Some(*e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_in_perspective_has_two_vanishing_points() {
        let mut a = Assistant::new(AssistantKind::Perspective, 100.0, 100.0);
        // Top edge falls to the right, bottom rises: they meet at x = 200.
        for (i, p) in [(0.0, 0.0), (100.0, 25.0), (100.0, 75.0), (0.0, 100.0)]
            .into_iter()
            .enumerate()
        {
            a.set_point(i, Vec2::new(p.0, p.1));
        }
        let vps = a.vanishing_points();
        assert_eq!(vps.len(), 1, "the upright sides are parallel");
        assert!((vps[0] - Vec2::new(200.0, 50.0)).length() < 1e-3);
    }

    #[test]
    fn points_go_onto_the_ellipse_and_concentric_ones_through_the_start() {
        let e = Ellipse {
            center: Vec2::new(50.0, 50.0),
            angle: 0.3,
            a: 40.0,
            b: 20.0,
        };
        for theta in [0.0f32, 1.0, 2.5, 4.0] {
            let p = e.at(theta);
            assert!(e.distance(p) < 1e-3);
            assert!((e.theta(p) - theta.sin().atan2(theta.cos())).abs() < 1e-4);
        }
        let near = e.at(1.0) + Vec2::new(3.0, -2.0);
        let on = Constraint::Ellipse(e).project(near);
        assert!(e.distance(on) < 1e-3);
        let start = Vec2::new(50.0, 90.0);
        let inner = e.through(start);
        assert!(inner.distance(start) < 1e-3);
        assert!((inner.a / inner.b - 2.0).abs() < 1e-4, "the same shape");
    }

    #[test]
    fn a_choosing_lock_waits_then_takes_the_closest_direction() {
        let mut lock = Lock::Choosing {
            start: Vec2::new(10.0, 10.0),
            dirs: vec![Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        };
        assert_eq!(lock.snap(Vec2::new(12.0, 11.0), 5.0), Vec2::new(10.0, 10.0));
        let p = lock.snap(Vec2::new(14.0, 30.0), 5.0);
        assert_eq!(p, Vec2::new(10.0, 30.0), "went down: the upright line");
        assert_eq!(lock.snap(Vec2::new(40.0, 50.0), 5.0), Vec2::new(10.0, 50.0));
    }

    #[test]
    fn an_ellipse_handle_keeps_its_axes_square() {
        let mut a = Assistant::new(AssistantKind::Ellipse, 200.0, 200.0);
        a.drag_handle(1, Vec2::new(100.0, 160.0));
        let e = a.ellipse().unwrap();
        let (u, v) = (a.point(1) - a.point(0), a.point(2) - a.point(0));
        assert!(u.dot(v).abs() < 1e-3);
        assert!((e.a - 60.0).abs() < 1e-3);
        a.drag_handle(0, Vec2::new(0.0, 0.0));
        assert!(
            (a.ellipse().unwrap().a - 60.0).abs() < 1e-3,
            "moved, same size"
        );
    }
}
