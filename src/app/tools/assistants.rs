//! Drawing assistants (like Clip Studio's perspective and special
//! rulers): guides a stroke snaps to, besides the straight ruler.
//!
//! - **Vanishing point** (one-point perspective): strokes run straight
//!   towards (or away from) it, and optionally level or upright.
//! - **Two-point**: two vanishing points on a horizon and the centre of
//!   vision between them; strokes run towards either, or upright (square
//!   to the horizon). The centre of vision sets how far the eye is from the
//!   picture, which makes its grid a true ground plane of squares.
//! - **Three-point**: two vanishing points on the horizon and a third above
//!   or below it for the uprights.
//! - **Perspective**: a plane set by the four corners of a rectangle seen
//!   at an angle (a [`Homography`] of the unit square). A stroke starting
//!   on it runs along either of its sides' directions (towards their
//!   vanishing points, or parallel to sides that are parallel), or upright.
//! - **Ellipse**: a stroke starting near it runs round it.
//! - **Concentric**: strokes run round ellipses of its shape nested inside
//!   and outside it, through wherever they start.
//! - **Fisheye**: two points on an axis; strokes run round the ellipse
//!   through both and through where they start (a meridian of a fisheye
//!   lens); past either point, round the next one along the axis.
//! - **Spline**: a cubic Bézier (start, two controls, end); a stroke
//!   starting near it runs along it.
//!
//! One-, two- and three-point assistants can be **local**: they only guide
//! strokes starting inside their area (a rectangle), and their lines show
//! only there.
//!
//! When the pen goes down the stroke picks what it follows ([`Lock`]).
//! With several lines or curves through its start (towards vanishing
//! points, round a fisheye or concentric assistant) it waits until the pen
//! has moved a little and takes the one the pen stays closest to.

use eframe::egui::Vec2;

/// What an assistant is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AssistantKind {
    VanishingPoint,
    Perspective,
    Ellipse,
    Concentric,
    TwoPoint,
    ThreePoint,
    Fisheye,
    Spline,
}

impl AssistantKind {
    pub const ALL: [AssistantKind; 8] = [
        Self::VanishingPoint,
        Self::TwoPoint,
        Self::ThreePoint,
        Self::Perspective,
        Self::Fisheye,
        Self::Ellipse,
        Self::Concentric,
        Self::Spline,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::VanishingPoint => "1-point",
            Self::TwoPoint => "2-point",
            Self::ThreePoint => "3-point",
            Self::Perspective => "Perspective",
            Self::Ellipse => "Ellipse",
            Self::Concentric => "Concentric",
            Self::Fisheye => "Fisheye",
            Self::Spline => "Spline",
        }
    }

    /// How many handles it has.
    pub fn handles(self) -> usize {
        match self {
            Self::VanishingPoint => 1,
            Self::TwoPoint => 3,
            Self::ThreePoint => 3,
            Self::Perspective => 4,
            Self::Ellipse | Self::Concentric | Self::Fisheye => 3,
            Self::Spline => 4,
        }
    }

    /// Whether it has lines (towards vanishing points, a fisheye's curves,
    /// a spline) which can be painted onto a layer.
    pub fn has_lines(self) -> bool {
        !matches!(self, Self::Ellipse | Self::Concentric)
    }

    /// Whether how many lines it shows can be set.
    pub fn has_line_count(self) -> bool {
        self.has_lines() && self != Self::Spline
    }

    /// Whether it can be limited to an area.
    pub fn can_be_local(self) -> bool {
        matches!(
            self,
            Self::VanishingPoint | Self::TwoPoint | Self::ThreePoint
        )
    }

    /// Whether it has a horizon to show.
    pub fn has_horizon(self) -> bool {
        matches!(
            self,
            Self::VanishingPoint | Self::TwoPoint | Self::ThreePoint
        )
    }

    /// Whether strokes can also run level / upright with it.
    pub fn has_upright(self) -> bool {
        matches!(
            self,
            Self::VanishingPoint | Self::TwoPoint | Self::Perspective
        )
    }
}

fn yes() -> bool {
    true
}

/// One assistant on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Assistant {
    pub kind: AssistantKind,
    /// Its handles, canvas coordinates (the first [`AssistantKind::handles`]
    /// are used):
    /// - vanishing point: the point;
    /// - two-point: the two vanishing points (the horizon runs through
    ///   them), then the centre of vision (on the horizon between them);
    /// - three-point: those two, then the one for the uprights;
    /// - perspective: the plane's corners, in order round it;
    /// - ellipse, concentric: the centre, the end of one axis, and the end
    ///   of the other (kept square to the first);
    /// - fisheye: the two points on its axis, then a point its main
    ///   ellipse runs through;
    /// - spline: its start, its two control points, its end.
    pub points: [[f32; 2]; 4],
    pub enabled: bool,
    /// How many lines it shows (rays round each vanishing point, or a
    /// plane's grid divisions); 0 for its kind's default.
    #[serde(default)]
    pub lines: u32,
    /// Show the horizon (through its vanishing points).
    #[serde(default = "yes")]
    pub horizon: bool,
    /// Strokes follow it (otherwise it only shows lines).
    #[serde(default = "yes")]
    pub snap: bool,
    /// Strokes may also run upright (one-, two-point and planes) and level
    /// (one-point).
    #[serde(default)]
    pub upright: bool,
    /// Two opposite corners of the area it's limited to, if it's local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<[[f32; 2]; 2]>,
}

/// The first handle number of a local assistant's area corners (after its
/// points).
pub const AREA_HANDLE: usize = 4;

impl Assistant {
    /// A new assistant of `kind`, sized to fit a `w` × `h` canvas.
    pub fn new(kind: AssistantKind, w: f32, h: f32) -> Self {
        let (cx, cy) = (w * 0.5, h * 0.5);
        let s = w.min(h);
        let points = match kind {
            AssistantKind::VanishingPoint => [[cx, h * 0.4], [0.0; 2], [0.0; 2], [0.0; 2]],
            AssistantKind::TwoPoint => [
                [w * 0.05, h * 0.4],
                [w * 0.95, h * 0.4],
                [cx, h * 0.4],
                [0.0; 2],
            ],
            AssistantKind::ThreePoint => [
                [w * 0.05, h * 0.3],
                [w * 0.95, h * 0.3],
                [cx, h * 0.95],
                [0.0; 2],
            ],
            AssistantKind::Perspective => [
                [cx - s * 0.3, cy - s * 0.1],
                [cx + s * 0.25, cy - s * 0.15],
                [cx + s * 0.25, cy + s * 0.2],
                [cx - s * 0.3, cy + s * 0.12],
            ],
            AssistantKind::Ellipse | AssistantKind::Concentric => {
                [[cx, cy], [cx + s * 0.3, cy], [cx, cy - s * 0.15], [0.0; 2]]
            }
            AssistantKind::Fisheye => [
                [cx - s * 0.35, cy],
                [cx + s * 0.35, cy],
                [cx, cy - s * 0.2],
                [0.0; 2],
            ],
            AssistantKind::Spline => [
                [cx - s * 0.3, cy + s * 0.1],
                [cx - s * 0.1, cy - s * 0.25],
                [cx + s * 0.1, cy + s * 0.25],
                [cx + s * 0.3, cy - s * 0.1],
            ],
        };
        Self {
            kind,
            points,
            enabled: true,
            lines: 0,
            horizon: true,
            snap: true,
            upright: true,
            area: None,
        }
    }

    /// Limit it to an area (the middle half of a `w` × `h` canvas), or
    /// not.
    pub fn set_local(&mut self, local: bool, w: f32, h: f32) {
        self.area = (local && self.kind.can_be_local())
            .then_some([[w * 0.25, h * 0.25], [w * 0.75, h * 0.75]]);
    }

    /// Its area's corners (least, greatest), if it's local.
    pub fn area_rect(&self) -> Option<(Vec2, Vec2)> {
        let [a, b] = self.area?.map(|p| Vec2::new(p[0], p[1]));
        Some((a.min(b), a.max(b)))
    }

    /// Whether it guides a stroke starting at `p`: anywhere, unless it's
    /// local and `p` is outside its area.
    pub fn applies_at(&self, p: Vec2) -> bool {
        self.area_rect()
            .is_none_or(|(lo, hi)| p.x >= lo.x && p.y >= lo.y && p.x <= hi.x && p.y <= hi.y)
    }

    /// Its handles (number, canvas point): its points, then a local one's
    /// area corners.
    pub fn handle_points(&self) -> Vec<(usize, Vec2)> {
        let mut out: Vec<(usize, Vec2)> = (0..self.kind.handles())
            .map(|i| (i, self.point(i)))
            .collect();
        if let Some(area) = self.area {
            out.extend(
                area.iter()
                    .enumerate()
                    .map(|(i, p)| (AREA_HANDLE + i, Vec2::new(p[0], p[1]))),
            );
        }
        out
    }

    /// How many lines it shows.
    pub fn line_count(&self) -> u32 {
        match (self.lines, self.kind) {
            (0, AssistantKind::Perspective) => 8,
            (0, AssistantKind::Fisheye) => 12,
            (0, _) => 24,
            (n, _) => n,
        }
    }

    /// The horizon's direction (a unit vector), for those with one.
    pub fn horizon_dir(&self) -> Option<Vec2> {
        match self.kind {
            AssistantKind::VanishingPoint => Some(Vec2::new(1.0, 0.0)),
            AssistantKind::TwoPoint | AssistantKind::ThreePoint => {
                let d = self.point(1) - self.point(0);
                (d.length() > 1e-3).then(|| d.normalized())
            }
            _ => None,
        }
    }

    /// The directions (unit vectors) a stroke starting at `start` may run
    /// in with it: towards its vanishing points, along its plane, upright
    /// or level as it allows. None for a plane the stroke starts off, as
    /// planes only guide strokes on them.
    pub fn snap_dirs(&self, start: Vec2) -> Vec<Vec2> {
        if !self.applies_at(start) {
            return Vec::new();
        }
        let upright = |horizon: Vec2| Vec2::new(-horizon.y, horizon.x);
        let mut dirs: Vec<Vec2> = match self.kind {
            AssistantKind::Perspective => {
                let corners = std::array::from_fn(|i| self.point(i));
                let Some(plane) = Homography::square_to(corners) else {
                    return Vec::new();
                };
                if !inside(start, &corners) {
                    return Vec::new();
                }
                (0..2).filter_map(|k| plane.dir_at(start, k)).collect()
            }
            AssistantKind::Ellipse
            | AssistantKind::Concentric
            | AssistantKind::Fisheye
            | AssistantKind::Spline => return Vec::new(),
            _ => self
                .vanishing_points()
                .into_iter()
                .filter(|vp| (*vp - start).length() > 1.0)
                .map(|vp| (vp - start).normalized())
                .collect(),
        };
        if dirs.is_empty() || !self.upright {
            return dirs;
        }
        match self.kind {
            AssistantKind::VanishingPoint => {
                dirs.extend([Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)]);
            }
            AssistantKind::TwoPoint => dirs.extend(self.horizon_dir().map(upright)),
            AssistantKind::Perspective => {
                // Square to its horizon; with a side pair parallel (no
                // horizon to go by) the screen's upright.
                let vps = self.vanishing_points();
                let up = match vps[..] {
                    [p, q] if (q - p).length() > 1e-3 => upright((q - p).normalized()),
                    _ => Vec2::new(0.0, 1.0),
                };
                dirs.push(up);
            }
            _ => {}
        }
        dirs
    }

    /// A two-point assistant's ground plane: the centre of vision put on
    /// the horizon, the horizon's direction, the two vanishing points'
    /// distances along it from the centre (one negative, one positive), and
    /// the eye's distance from the picture (√(−a·b), as the two directions
    /// are square to each other).
    fn ground(&self) -> Option<(Vec2, Vec2, f32, f32, f32)> {
        let dir = self.horizon_dir()?;
        let (p, q) = (self.point(0), self.point(1));
        let len = (q - p).length();
        let t = ((self.point(2) - p).dot(dir) / len).clamp(COV_MARGIN, 1.0 - COV_MARGIN);
        let c = p + dir * (len * t);
        let (a, b) = ((p - c).dot(dir), (q - c).dot(dir));
        Some((c, dir, a, b, (-a * b).sqrt()))
    }

    /// Old two-point assistants had no centre of vision: put it halfway.
    pub fn upgrade(&mut self) {
        if self.kind == AssistantKind::TwoPoint && self.points[2] == [0.0; 2] {
            let mid = (self.point(0) + self.point(1)) * 0.5;
            self.set_point(2, mid);
        }
    }

    /// The lines it shows, clipped to a `w` × `h` canvas: rays out of its
    /// vanishing points, a plane's grid, the uprights of a two-point one.
    /// The horizon and a plane's outline aren't among them.
    pub fn guide_lines(&self, w: f32, h: f32) -> Vec<(Vec2, Vec2)> {
        let far = w.hypot(h) * 2.0;
        let n = self.line_count().max(1);
        let mut out = Vec::new();
        let rays = |vp: Vec2, out: &mut Vec<(Vec2, Vec2)>| {
            for i in 0..n {
                let t = i as f32 / n as f32 * std::f32::consts::TAU;
                out.extend(clip_to_canvas(
                    vp,
                    Vec2::new(t.cos(), t.sin()),
                    0.0,
                    far,
                    w,
                    h,
                ));
            }
        };
        match self.kind {
            AssistantKind::VanishingPoint | AssistantKind::ThreePoint => {
                for vp in self.vanishing_points() {
                    rays(vp, &mut out);
                }
            }
            AssistantKind::TwoPoint => {
                // A ground (and ceiling) plane of squares: on the line as
                // far below the horizon as the eye is from the picture,
                // the picture's scale is the ground's, so lines towards a
                // vanishing point at `x` cross it every `s·√(x²+d²)/d` for
                // squares of side `s`.
                if let Some((c, dir, a, b, d)) = self.ground() {
                    let down = Vec2::new(-dir.y, dir.x);
                    let side = d * 8.0 / n as f32;
                    let m = 2 * n as i32;
                    for ground in [d, -d] {
                        for x in [a, b] {
                            let vp = c + dir * x;
                            let step = side * x.hypot(d) / d;
                            for i in -m..=m {
                                let p = c + dir * (i as f32 * step) + down * ground;
                                out.extend(clip_to_canvas(
                                    vp,
                                    (p - vp).normalized(),
                                    0.0,
                                    far,
                                    w,
                                    h,
                                ));
                            }
                        }
                    }
                }
                // Uprights, evenly across the horizon between the points.
                if self.upright
                    && let Some(d) = self.horizon_dir()
                {
                    let up = Vec2::new(-d.y, d.x);
                    let (a, b) = (self.point(0), self.point(1));
                    let k = n / 2 + 1;
                    for i in 1..k {
                        let p = a + (b - a) * (i as f32 / k as f32);
                        out.extend(clip_to_canvas(p, up, -far, far, w, h));
                    }
                }
            }
            AssistantKind::Perspective => {
                let p: [Vec2; 4] = std::array::from_fn(|i| self.point(i));
                // A grid on the plane: the unit square's, through the
                // plane's homography, so it shrinks into the distance and
                // each family of lines meets at its vanishing point.
                if let Some(plane) = Homography::square_to(p) {
                    for k in 1..n {
                        let t = k as f32 / n as f32;
                        out.push((plane.map(t, 0.0), plane.map(t, 1.0)));
                        out.push((plane.map(0.0, t), plane.map(1.0, t)));
                    }
                }
                // The sides carried on to the vanishing points.
                for vp in self.vanishing_points() {
                    for q in &p {
                        let d = vp - *q;
                        if d.length() > 1.0 {
                            out.extend(clip_to_canvas(*q, d.normalized(), 0.0, d.length(), w, h));
                        }
                    }
                }
            }
            AssistantKind::Fisheye => {
                // Its axis, through the cells either side.
                let (p, q) = (self.point(0), self.point(1));
                out.push((p * 2.0 - q, q * 2.0 - p));
            }
            AssistantKind::Ellipse | AssistantKind::Concentric | AssistantKind::Spline => {}
        }
        if let Some((lo, hi)) = self.area_rect() {
            out = out
                .into_iter()
                .filter_map(|(p, q)| clip_to_rect(p, q - p, 0.0, 1.0, lo, hi))
                .collect();
        }
        out
    }

    /// The curves it shows besides its lines: a fisheye's meridians, round
    /// each of its three cells (its main ellipse isn't among them).
    pub fn guide_curves(&self) -> Vec<Vec<Vec2>> {
        if self.kind != AssistantKind::Fisheye {
            return Vec::new();
        }
        let (p, q) = (self.point(0), self.point(1));
        let a = (q - p).length() * 0.5;
        let m = (self.line_count() / 2).max(1);
        let mut out = Vec::new();
        for (m1, m2) in [(p, q), (q, q * 2.0 - p), (p * 2.0 - q, p)] {
            for j in 1..=m {
                // Meridians at even turns of the sphere: the last a circle.
                let b = a * (j as f32 / m as f32 * std::f32::consts::FRAC_PI_2).sin();
                if let Some(e) = Ellipse::with_axis(m1, m2, b) {
                    out.push(e.polyline(96));
                }
            }
        }
        out
    }

    /// Its curves that painting it onto a layer paints: the guide curves,
    /// a fisheye's main ellipse, a spline.
    pub fn all_curves(&self) -> Vec<Vec<Vec2>> {
        let mut out = self.guide_curves();
        match self.kind {
            AssistantKind::Fisheye => out.extend(self.fisheye_main().map(|e| e.polyline(96))),
            AssistantKind::Spline => out.push(self.spline_points()),
            _ => {}
        }
        out
    }

    /// A fisheye's main ellipse: through its two points and its third.
    pub fn fisheye_main(&self) -> Option<Ellipse> {
        (self.kind == AssistantKind::Fisheye)
            .then(|| Ellipse::through_ends(self.point(0), self.point(1), self.point(2)))
            .flatten()
    }

    /// What a stroke starting at `start` follows round a fisheye: the
    /// ellipse through both its points and `start`; past either point, one
    /// as long the other side of it; on the axis, the axis. None beyond
    /// those.
    pub fn fisheye_through(&self, start: Vec2) -> Option<Constraint> {
        let (p, q) = (self.point(0), self.point(1));
        let d = q - p;
        if d.length() < 1e-3 {
            return None;
        }
        let dir = d.normalized();
        let off = start - p;
        if (off.x * dir.y - off.y * dir.x).abs() < 0.5 {
            return Some(Constraint::Line { origin: p, dir });
        }
        [(p, q), (q, q * 2.0 - p), (p * 2.0 - q, p)]
            .into_iter()
            .find_map(|(m1, m2)| Ellipse::through_ends(m1, m2, start))
            .map(Constraint::Ellipse)
    }

    /// The curve through `start` that a concentric or fisheye assistant
    /// offers a stroke starting there.
    pub fn curve_through(&self, start: Vec2) -> Option<Constraint> {
        match self.kind {
            AssistantKind::Concentric => self
                .ellipse()
                .map(|e| Constraint::Ellipse(e.through(start))),
            AssistantKind::Fisheye => self.fisheye_through(start),
            _ => None,
        }
    }

    /// A spline's curve as a polyline.
    pub fn spline_points(&self) -> Vec<Vec2> {
        let [p0, p1, p2, p3] = std::array::from_fn(|i| self.point(i));
        (0..=SPLINE_STEPS)
            .map(|i| {
                let t = i as f32 / SPLINE_STEPS as f32;
                let u = 1.0 - t;
                p0 * (u * u * u)
                    + p1 * (3.0 * u * u * t)
                    + p2 * (3.0 * u * t * t)
                    + p3 * (t * t * t)
            })
            .collect()
    }

    /// Its horizon clipped to a `w` × `h` canvas, if shown.
    pub fn horizon_line(&self, w: f32, h: f32) -> Option<(Vec2, Vec2)> {
        if !self.horizon || !self.kind.has_horizon() {
            return None;
        }
        let far = w.hypot(h) * 2.0;
        clip_to_canvas(self.point(0), self.horizon_dir()?, -far, far, w, h)
    }

    /// Everything [`Self::guide_lines`] gives, the horizon and a plane's
    /// outline: what painting it onto a layer paints.
    pub fn all_lines(&self, w: f32, h: f32) -> Vec<(Vec2, Vec2)> {
        let mut out = self.guide_lines(w, h);
        out.extend(self.horizon_line(w, h));
        if let Some((lo, hi)) = self.area_rect() {
            let c = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
            out.extend((0..4).map(|i| (c[i], c[(i + 1) % 4])));
        }
        if self.kind == AssistantKind::Perspective {
            out.extend((0..4).map(|i| (self.point(i), self.point((i + 1) % 4))));
        }
        out
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
        if i >= AREA_HANDLE {
            if let Some(area) = &mut self.area {
                area[(i - AREA_HANDLE).min(1)] = [p.x, p.y];
            }
            return;
        }
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
            AssistantKind::TwoPoint => {
                let (a, b) = (self.point(0), self.point(1));
                let len = (b - a).length().max(1e-3);
                if i == 2 {
                    // The centre of vision slides along the horizon.
                    let t = ((p - a).dot(b - a) / (len * len)).clamp(COV_MARGIN, 1.0 - COV_MARGIN);
                    self.set_point(2, a + (b - a) * t);
                } else {
                    // It keeps its place between the points.
                    let t = (self.point(2) - a).dot(b - a) / (len * len);
                    self.set_point(i, p);
                    let (a, b) = (self.point(0), self.point(1));
                    self.set_point(2, a + (b - a) * t.clamp(COV_MARGIN, 1.0 - COV_MARGIN));
                }
            }
            _ => self.set_point(i, p),
        }
    }

    /// Move all of it by `d`.
    pub fn move_by(&mut self, d: Vec2) {
        for i in 0..self.kind.handles() {
            let p = self.point(i) + d;
            self.set_point(i, p);
        }
        if let Some(area) = &mut self.area {
            for p in area {
                *p = [p[0] + d.x, p[1] + d.y];
            }
        }
    }

    /// Whether canvas point `p` is on it (within `near` of its lines, or
    /// inside its plane or ellipse), for grabbing it whole.
    pub fn covers(&self, p: Vec2, near: f32) -> bool {
        if let Some((lo, hi)) = self.area_rect() {
            let c = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y), lo];
            if nearest_on_polyline(&c, p).1 <= near {
                return true;
            }
        }
        match self.kind {
            // Its only handle moves it.
            AssistantKind::VanishingPoint => false,
            // Along the lines between its points.
            AssistantKind::TwoPoint | AssistantKind::ThreePoint => {
                let vps = self.vanishing_points();
                let pts: Vec<Vec2> = vps.iter().chain(vps.first()).copied().collect();
                nearest_on_polyline(&pts, p).1 <= near
            }
            AssistantKind::Perspective => {
                let corners: Vec<Vec2> = (0..4)
                    .map(|i| self.point(i))
                    .chain([self.point(0)])
                    .collect();
                nearest_on_polyline(&corners, p).1 <= near || inside(p, &corners[..4])
            }
            AssistantKind::Ellipse | AssistantKind::Concentric => self.ellipse().is_some_and(|e| {
                let u = e.unit_coords(p).length();
                u <= 1.0 || e.distance(p) <= near
            }),
            AssistantKind::Fisheye => {
                nearest_on_polyline(&[self.point(0), self.point(1)], p).1 <= near
                    || self
                        .fisheye_main()
                        .is_some_and(|e| e.unit_coords(p).length() <= 1.0 || e.distance(p) <= near)
            }
            AssistantKind::Spline => nearest_on_polyline(&self.spline_points(), p).1 <= near,
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
            AssistantKind::TwoPoint => vec![self.point(0), self.point(1)],
            AssistantKind::ThreePoint => (0..3).map(|i| self.point(i)).collect(),
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

/// How close (a share of the horizon) a two-point assistant's centre of
/// vision may come to either vanishing point.
const COV_MARGIN: f32 = 0.02;

/// The projective map taking the unit square's corners (0,0), (1,0),
/// (1,1), (0,1) onto a plane's four corners in order: lines stay straight,
/// equal steps on the square shrink into the distance, and each family of
/// the square's lines meets at a vanishing point (or stays parallel).
#[derive(Clone, Copy, Debug)]
pub struct Homography {
    /// Row by row, `[a b c; d e f; g h 1]`: (u, v) goes to
    /// ((a·u + b·v + c), (d·u + e·v + f)) / (g·u + h·v + 1).
    m: [f64; 8],
}

impl Homography {
    /// The map onto `q`; none unless `q` is a convex quadrilateral (a
    /// crossed or dented one isn't a rectangle seen in perspective).
    pub fn square_to(q: [Vec2; 4]) -> Option<Self> {
        let turns: Vec<f32> = (0..4)
            .map(|i| {
                let (a, b, c) = (q[i], q[(i + 1) % 4], q[(i + 2) % 4]);
                let (u, v) = (b - a, c - b);
                u.x * v.y - u.y * v.x
            })
            .collect();
        if !(turns.iter().all(|&t| t > 1e-3) || turns.iter().all(|&t| t < -1e-3)) {
            return None;
        }
        let [x0, x1, x2, x3] = q.map(|p| p.x as f64);
        let [y0, y1, y2, y3] = q.map(|p| p.y as f64);
        let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
        let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() < 1e-9 {
            return None;
        }
        let g = (sx * dy2 - dx2 * sy) / den;
        let h = (dx1 * sy - sx * dy1) / den;
        Some(Self {
            m: [
                x1 - x0 + g * x1,
                x3 - x0 + h * x3,
                x0,
                y1 - y0 + g * y1,
                y3 - y0 + h * y3,
                y0,
                g,
                h,
            ],
        })
    }

    /// Where square point (u, v) is on the plane.
    pub fn map(&self, u: f32, v: f32) -> Vec2 {
        let [a, b, c, d, e, f, g, h] = self.m;
        let (u, v) = (u as f64, v as f64);
        let w = g * u + h * v + 1.0;
        Vec2::new(
            ((a * u + b * v + c) / w) as f32,
            ((d * u + e * v + f) / w) as f32,
        )
    }

    /// The direction (unit) at plane point `p` of the square's `u` lines
    /// (`k` = 0) or `v` lines (`k` = 1): towards where they meet, the
    /// image of that direction's point at infinity, (a, d, g) or (b, e, h).
    pub fn dir_at(&self, p: Vec2, k: usize) -> Option<Vec2> {
        let [a, b, _, d, e, _, g, h] = self.m;
        let (x, y, w) = if k == 0 { (a, d, g) } else { (b, e, h) };
        let dir = Vec2::new((x - w * p.x as f64) as f32, (y - w * p.y as f64) as f32);
        (dir.length() > 1e-6).then(|| dir.normalized())
    }
}

/// How many pieces a spline's polyline has.
const SPLINE_STEPS: usize = 128;

/// The part of the line through `p` with direction `dir` (from `t0` to
/// `t1` along it) inside a `w` × `h` canvas, if any.
pub fn clip_to_canvas(
    p: Vec2,
    dir: Vec2,
    t0: f32,
    t1: f32,
    w: f32,
    h: f32,
) -> Option<(Vec2, Vec2)> {
    clip_to_rect(p, dir, t0, t1, Vec2::ZERO, Vec2::new(w, h))
}

/// The part of the line through `p` with direction `dir` (from `t0` to
/// `t1` along it) inside the rectangle from `min` to `max`, if any.
pub fn clip_to_rect(
    p: Vec2,
    dir: Vec2,
    t0: f32,
    t1: f32,
    min: Vec2,
    max: Vec2,
) -> Option<(Vec2, Vec2)> {
    let (mut lo, mut hi) = (t0, t1);
    for (pv, dv, min, max) in [(p.x, dir.x, min.x, max.x), (p.y, dir.y, min.y, max.y)] {
        if dv.abs() < 1e-9 {
            if pv < min || pv > max {
                return None;
            }
            continue;
        }
        let (a, b) = ((min - pv) / dv, (max - pv) / dv);
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
    }
    (lo < hi).then(|| (p + dir * lo, p + dir * hi))
}

/// Whether `p` is inside polygon `points` (crossing count).
fn inside(p: Vec2, points: &[Vec2]) -> bool {
    let mut odd = false;
    for (i, &a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            odd = !odd;
        }
    }
    odd
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
    /// The ellipse whose first axis runs from `m1` to `m2`, with second
    /// radius `b`.
    pub fn with_axis(m1: Vec2, m2: Vec2, b: f32) -> Option<Ellipse> {
        let d = m2 - m1;
        let a = d.length() * 0.5;
        (a > 1e-3 && b > 1e-3).then(|| Ellipse {
            center: (m1 + m2) * 0.5,
            angle: d.y.atan2(d.x),
            a,
            b,
        })
    }

    /// The ellipse whose first axis runs from `m1` to `m2` and which passes
    /// through `p` (none if `p` is beyond the axis's ends, or on it).
    pub fn through_ends(m1: Vec2, m2: Vec2, p: Vec2) -> Option<Ellipse> {
        let e = Self::with_axis(m1, m2, 1.0)?;
        let d = p - e.center;
        let (s, c) = e.angle.sin_cos();
        let (x, y) = ((c * d.x + s * d.y) / e.a, -s * d.x + c * d.y);
        if x.abs() >= 0.999 {
            return None;
        }
        Self::with_axis(m1, m2, y.abs() / (1.0 - x * x).sqrt())
    }

    /// It as a closed polyline of `n` pieces.
    pub fn polyline(&self, n: usize) -> Vec<Vec2> {
        (0..=n)
            .map(|i| self.at(i as f32 / n as f32 * std::f32::consts::TAU))
            .collect()
    }

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
    /// It will follow whichever of these (all through `start`) the pen
    /// stays closest to, once it has moved far enough to tell.
    Choosing {
        start: Vec2,
        options: Vec<Constraint>,
    },
    /// It runs along this polyline (the curve ruler, a spline); `at` is
    /// where it was last (a point number, the fraction past it added) and
    /// `pen` where the pen was then. It looks for the pen only as far along
    /// the curve from there as the pen could have gone, so a curve that
    /// comes back near itself doesn't make it jump across.
    Curve {
        points: std::sync::Arc<Vec<Vec2>>,
        at: Option<f32>,
        pen: Option<Vec2>,
    },
}

/// The point of polyline `points` nearest `p`, and how far it is.
pub fn nearest_on_polyline(points: &[Vec2], p: Vec2) -> (Vec2, f32) {
    let (q, dist, _) = nearest_on_part(points, p, 0, points.len().saturating_sub(1));
    (q, dist)
}

/// The point of polyline `points` nearest `p` on its pieces `from` to
/// `to` (piece `i` runs from point `i` to `i + 1`): the point, how far it
/// is, and where it is along the polyline (a point number, the fraction
/// past it added).
fn nearest_on_part(points: &[Vec2], p: Vec2, from: usize, to: usize) -> (Vec2, f32, f32) {
    let first = points.get(from).copied().unwrap_or(p);
    let mut best = (first, (first - p).length(), from as f32);
    for i in from..to.min(points.len().saturating_sub(1)) {
        let (a, d) = (points[i], points[i + 1] - points[i]);
        let t = if d.length_sq() < 1e-9 {
            0.0
        } else {
            ((p - a).dot(d) / d.length_sq()).clamp(0.0, 1.0)
        };
        let q = a + d * t;
        let dist = (q - p).length();
        if dist < best.1 {
            best = (q, dist, i as f32 + t);
        }
    }
    best
}

/// The point `at` along polyline `points` (a point number, the fraction
/// past it added).
pub fn point_along(points: &[Vec2], at: f32) -> Vec2 {
    let last = points.len().saturating_sub(1);
    let i = (at.max(0.0) as usize).min(last);
    let t = at - i as f32;
    match points.get(i + 1) {
        Some(&next) if t > 0.0 => points[i] + (next - points[i]) * t,
        _ => points.get(i).copied().unwrap_or_default(),
    }
}

/// The points of polyline `points` strictly between `from` and `to` along
/// it (point numbers, fractions added), in the order from one to the other.
pub fn points_between(points: &[Vec2], from: f32, to: f32) -> Vec<Vec2> {
    let mut out = Vec::new();
    if to > from {
        let mut i = from.floor() as usize + 1;
        while (i as f32) < to && i < points.len() {
            out.push(points[i]);
            i += 1;
        }
    } else {
        let mut i = from.ceil() as isize - 1;
        while i >= 0 && (i as f32) > to {
            if let Some(&p) = points.get(i as usize) {
                out.push(p);
            }
            i -= 1;
        }
    }
    out
}

/// How far along a curve (canvas pixels) a stroke may go past twice what
/// the pen moved, from one sample to the next.
const CURVE_REACH: f32 = 8.0;

/// How far (screen points) the pen moves before a stroke picks its
/// direction.
pub const CHOOSE_AFTER: f32 = 6.0;

impl Lock {
    /// What a stroke starting at `start` with these `options` through it
    /// follows: the one, or a choice among several.
    pub fn among(start: Vec2, mut options: Vec<Constraint>) -> Option<Lock> {
        match options.len() {
            0 => None,
            1 => options.pop().map(Lock::Fixed),
            _ => Some(Lock::Choosing { start, options }),
        }
    }

    /// `p` put on what the stroke follows; a choosing lock settles once `p`
    /// is far enough (`far`, canvas pixels) from the start.
    pub fn snap(&mut self, p: Vec2, far: f32) -> Vec2 {
        match self {
            Lock::Fixed(c) => c.project(p),
            Lock::Curve { points, at, pen } => {
                let n = points.len().saturating_sub(1);
                let (from, to) = match (*at, *pen) {
                    (Some(a), Some(last)) if n > 0 => {
                        // Along the curve, twice as far as the pen moved
                        // (more on a tight bend than across it), and a bit.
                        let reach = (p - last).length() * 2.0 + CURVE_REACH;
                        let here = point_along(points, a);
                        let i = (a as usize).min(n - 1);
                        let (mut from, mut back) = (i, (here - points[i]).length());
                        while from > 0 && back <= reach {
                            back += (points[from] - points[from - 1]).length();
                            from -= 1;
                        }
                        let (mut to, mut ahead) = (i + 1, (points[i + 1] - here).length());
                        while to < n && ahead <= reach {
                            ahead += (points[to + 1] - points[to]).length();
                            to += 1;
                        }
                        (from, to)
                    }
                    _ => (0, n),
                };
                let (q, _, along) = nearest_on_part(points, p, from, to);
                (*at, *pen) = (Some(along), Some(p));
                q
            }
            Lock::Choosing { start, options } => {
                let start = *start;
                if (p - start).length() < far || options.is_empty() {
                    return start;
                }
                let c = *options
                    .iter()
                    .min_by(|a, b| {
                        (a.project(p) - p)
                            .length()
                            .total_cmp(&(b.project(p) - p).length())
                    })
                    .expect("some option");
                *self = Lock::Fixed(c);
                c.project(p)
            }
        }
    }

    /// Whether the stroke follows (or may yet follow) a curve.
    pub fn may_curve(&self) -> bool {
        match self {
            Lock::Fixed(c) => matches!(c, Constraint::Ellipse(_)),
            Lock::Curve { .. } => true,
            Lock::Choosing { options, .. } => {
                options.iter().any(|c| matches!(c, Constraint::Ellipse(_)))
            }
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
    fn two_point_lines_and_directions() {
        let mut a = Assistant::new(AssistantKind::TwoPoint, 200.0, 100.0);
        a.lines = 8;
        assert_eq!(a.vanishing_points().len(), 2);
        // Level horizon: towards either point, and the upright straight down.
        let start = Vec2::new(100.0, 80.0);
        let dirs = a.snap_dirs(start);
        assert_eq!(dirs.len(), 3);
        assert!((dirs[2].x).abs() < 1e-5 && (dirs[2].y.abs() - 1.0).abs() < 1e-5);
        let lines = a.guide_lines(200.0, 100.0);
        assert!(lines.len() > 8, "{}", lines.len());
        assert_eq!(a.all_lines(200.0, 100.0).len(), lines.len() + 1);
        a.upright = false;
        assert_eq!(a.snap_dirs(start).len(), 2);
        // Old files: defaults fill the new fields.
        let old = r#"{"kind":"VanishingPoint","points":[[1,2],[0,0],[0,0],[0,0]],"enabled":true}"#;
        let a: Assistant = serde_json::from_str(old).unwrap();
        assert!(a.snap && a.horizon && !a.upright && a.line_count() == 24);
    }

    fn plane(corners: [(f32, f32); 4]) -> Assistant {
        let mut a = Assistant::new(AssistantKind::Perspective, 100.0, 100.0);
        for (i, p) in corners.into_iter().enumerate() {
            a.set_point(i, Vec2::new(p.0, p.1));
        }
        a
    }

    #[test]
    fn a_planes_grid_is_in_perspective() {
        let a = plane([(0.0, 0.0), (100.0, 25.0), (100.0, 75.0), (0.0, 100.0)]);
        let corners = std::array::from_fn(|i| a.point(i));
        let h = Homography::square_to(corners).unwrap();
        for (i, uv) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
            .iter()
            .enumerate()
        {
            assert!((h.map(uv.0, uv.1) - corners[i]).length() < 1e-3);
        }
        // Every line across it heads for the vanishing point (200, 50)...
        let vp = Vec2::new(200.0, 50.0);
        let lines = a.guide_lines(1000.0, 1000.0);
        // 8 squares across: 7 lines each way, alternating.
        let grid = &lines[..2 * 7];
        for (p, q) in grid.iter().skip(1).step_by(2) {
            let (d, e) = (*q - *p, vp - *p);
            assert!(
                (d.x * e.y - d.y * e.x).abs() / d.length() < 1e-2,
                "{p:?} {q:?}"
            );
        }
        // ...and the uprights crowd together towards it: foreshortened.
        let xs: Vec<f32> = grid.iter().step_by(2).map(|(p, _)| p.x).collect();
        let gaps: Vec<f32> = xs.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] < g[0]), "{gaps:?}");
        // A dented quad is no plane.
        let dent = plane([(0.0, 0.0), (100.0, 0.0), (20.0, 20.0), (0.0, 100.0)]);
        assert!(Homography::square_to(std::array::from_fn(|i| dent.point(i))).is_none());
    }

    #[test]
    fn a_plane_guides_strokes_on_it_along_its_sides_even_parallel_ones() {
        // A floor seen straight on: level sides, the others meet above.
        let floor = plane([(30.0, 40.0), (70.0, 40.0), (90.0, 80.0), (10.0, 80.0)]);
        let dirs = floor.snap_dirs(Vec2::new(50.0, 60.0));
        assert!(dirs.iter().any(|d| d.y.abs() < 1e-5), "level: {dirs:?}");
        let to_vp = dirs.iter().find(|d| d.y.abs() > 1e-5 && d.x.abs() > 1e-5);
        assert!(
            to_vp.is_none(),
            "from the middle it's straight up to the point"
        );
        let side = floor.snap_dirs(Vec2::new(25.0, 60.0));
        let vp = floor.vanishing_points()[0];
        let d = (vp - Vec2::new(25.0, 60.0)).normalized();
        assert!(
            side.iter().any(|e| e.dot(d).abs() > 0.9999),
            "{side:?} {vp:?}"
        );
        // Off it: nothing.
        assert!(floor.snap_dirs(Vec2::new(5.0, 5.0)).is_empty());
    }

    #[test]
    fn a_fisheye_stroke_runs_round_the_ellipse_through_both_points_and_its_start() {
        let mut a = Assistant::new(AssistantKind::Fisheye, 400.0, 400.0);
        a.set_point(0, Vec2::new(100.0, 200.0));
        a.set_point(1, Vec2::new(300.0, 200.0));
        let start = Vec2::new(150.0, 150.0);
        let Some(Constraint::Ellipse(e)) = a.fisheye_through(start) else {
            panic!("an ellipse");
        };
        for p in [a.point(0), a.point(1), start] {
            assert!(e.distance(p) < 1e-2, "{p:?} is {} off", e.distance(p));
        }
        // Past a point: round the next cell, through that point.
        let past = Vec2::new(350.0, 180.0);
        let Some(Constraint::Ellipse(e)) = a.fisheye_through(past) else {
            panic!("an ellipse");
        };
        assert!(e.distance(a.point(1)) < 1e-2 && e.distance(Vec2::new(500.0, 200.0)) < 1e-2);
        // On the axis: along it. Beyond the cells: nothing.
        assert!(matches!(
            a.fisheye_through(Vec2::new(250.0, 200.0)),
            Some(Constraint::Line { .. })
        ));
        assert!(a.fisheye_through(Vec2::new(550.0, 150.0)).is_none());
        // Meridians round each of the three cells, and its main ellipse.
        assert_eq!(a.guide_curves().len(), 3 * 6);
        assert_eq!(a.all_curves().len(), 3 * 6 + 1);
    }

    #[test]
    fn a_spline_lock_stays_on_its_stretch_where_the_curve_comes_back() {
        let mut a = Assistant::new(AssistantKind::Spline, 400.0, 400.0);
        // A hairpin: out right along y = 100, back left along y = 120.
        for (i, p) in [(0.0, 100.0), (400.0, 100.0), (400.0, 120.0), (0.0, 120.0)]
            .into_iter()
            .enumerate()
        {
            a.set_point(i, Vec2::new(p.0, p.1));
        }
        let points = std::sync::Arc::new(a.spline_points());
        assert_eq!(points[0], a.point(0));
        assert_eq!(*points.last().unwrap(), a.point(3));
        let mut lock = Lock::Curve {
            points,
            at: None,
            pen: None,
        };
        let p = lock.snap(Vec2::new(20.0, 101.0), 5.0);
        assert!((p.y - 101.0).abs() < 3.0, "{p:?}");
        // Drifting towards the other stretch: it stays on its own.
        let p = lock.snap(Vec2::new(40.0, 112.0), 5.0);
        assert!(p.y < 108.0, "jumped across: {p:?}");
        let Lock::Curve { at: Some(at), .. } = lock else {
            panic!()
        };
        assert!(at < 20.0);
    }

    #[test]
    fn a_local_assistant_only_guides_strokes_in_its_area() {
        let mut a = Assistant::new(AssistantKind::VanishingPoint, 200.0, 200.0);
        assert!(!a.snap_dirs(Vec2::new(10.0, 10.0)).is_empty());
        a.set_local(true, 200.0, 200.0);
        assert!(a.snap_dirs(Vec2::new(10.0, 10.0)).is_empty());
        assert!(!a.snap_dirs(Vec2::new(100.0, 120.0)).is_empty());
        // Its lines stay inside, plus the area's outline when painted.
        for (p, q) in a.guide_lines(200.0, 200.0) {
            for r in [p, q] {
                assert!(r.x >= 49.9 && r.x <= 150.1 && r.y >= 49.9 && r.y <= 150.1);
            }
        }
        // The corners are handles: dragged, and moved with it.
        assert_eq!(a.handle_points().len(), 3);
        a.drag_handle(AREA_HANDLE + 1, Vec2::new(190.0, 190.0));
        a.move_by(Vec2::new(5.0, 0.0));
        assert_eq!(
            a.area_rect(),
            Some((Vec2::new(55.0, 50.0), Vec2::new(195.0, 190.0)))
        );
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Assistant>(&json).unwrap(), a);
        a.set_local(false, 200.0, 200.0);
        assert!(!serde_json::to_string(&a).unwrap().contains("area"));
        // Not every kind can be local.
        let mut e = Assistant::new(AssistantKind::Ellipse, 200.0, 200.0);
        e.set_local(true, 200.0, 200.0);
        assert!(e.area.is_none());
    }

    #[test]
    fn a_two_point_ground_grid_is_made_of_squares() {
        let mut a = Assistant::new(AssistantKind::TwoPoint, 400.0, 400.0);
        a.set_point(0, Vec2::new(-100.0, 0.0));
        a.set_point(1, Vec2::new(300.0, 0.0));
        a.set_point(2, Vec2::new(0.0, 0.0));
        let (c, _, x0, x1, d) = a.ground().unwrap();
        assert_eq!(c, Vec2::new(0.0, 0.0));
        // The two directions are square: x0·x1 = −d².
        assert!((x0 * x1 + d * d).abs() < 1e-2);
        assert!(a.guide_lines(400.0, 400.0).len() > 20);
        // Dragging a point keeps the centre where it was between them.
        a.drag_handle(1, Vec2::new(100.0, 0.0));
        assert!((a.point(2) - Vec2::new(-50.0, 0.0)).length() < 1e-3);
        // It can't leave the horizon, nor reach a point.
        a.drag_handle(2, Vec2::new(500.0, 40.0));
        assert!(a.point(2).y.abs() < 1e-3 && a.point(2).x < 100.0);
        // Old files had none: it goes halfway.
        let old = r#"{"kind":"TwoPoint","points":[[0,10],[100,10],[0,0],[0,0]],"enabled":true}"#;
        let mut a: Assistant = serde_json::from_str(old).unwrap();
        a.upgrade();
        assert_eq!(a.point(2), Vec2::new(50.0, 10.0));
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
        let start = Vec2::new(10.0, 10.0);
        let mut lock = Lock::among(
            start,
            [Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)]
                .map(|dir| Constraint::Line { origin: start, dir })
                .to_vec(),
        )
        .unwrap();
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
