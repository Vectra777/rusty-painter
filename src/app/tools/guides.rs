//! On-canvas guides with draggable handles: the mirror-painting axes, the
//! ruler and the drawing assistants (see [`crate::app::tools::assistants`]).
//!
//! A press on a handle is the guide's, not the active tool's: it moves the
//! symmetry centre, turns its axes, or moves the ruler.
//!
//! The ruler straightens freehand strokes: a stroke starting near it runs
//! along it, one starting elsewhere runs parallel to it (like sliding a pen
//! along a real ruler).

use crate::app::PainterApp;
use crate::app::tools::assistants::{Assistant, AssistantKind, Constraint, Lock};
use crate::app::view::render::ScreenMap;
use crate::brush_engine::symmetry::SymmetryMode;
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

/// Handle hit radius and drawn size, in screen points.
const HANDLE_HIT: f32 = 14.0;
const HANDLE_RADIUS: f32 = 6.0;
/// Distance of the rotation handle from the centre, in screen points.
const ROTATE_HANDLE: f32 = 72.0;
/// Snaps, in screen points / radians.
const SNAP_DISTANCE: f32 = 10.0;
const ANGLE_STEP: f32 = std::f32::consts::PI / 12.0;

const GUIDE_COLOR: Color32 = Color32::from_rgb(90, 200, 250);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    SymmetryCenter {
        grab: Vec2,
    },
    SymmetryAngle,
    RulerEnd(usize),
    RulerMove {
        last: Vec2,
    },
    /// Assistant `index`'s handle `handle`.
    Assistant {
        index: usize,
        handle: usize,
    },
}

/// A straight ruler between two points (canvas coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ruler {
    pub a: Vec2,
    pub b: Vec2,
    /// Shown, and straightening strokes.
    pub enabled: bool,
    /// Strokes starting away from the ruler run parallel to it (otherwise
    /// they're left alone).
    pub parallel: bool,
}

impl Default for Ruler {
    fn default() -> Self {
        Self {
            a: Vec2::ZERO,
            b: Vec2::ZERO,
            enabled: false,
            parallel: true,
        }
    }
}

/// The guides as saved in a project: the ruler, the assistants and the
/// mirror painting.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct StoredGuides {
    ruler: [[f32; 2]; 2],
    ruler_enabled: bool,
    ruler_parallel: bool,
    assistants: Vec<Assistant>,
    /// `SymmetryMode` by name ("off", "vertical", …).
    symmetry: String,
    symmetry_center: [f32; 2],
    symmetry_angle: f32,
    symmetry_count: u32,
    symmetry_mirrored: bool,
    wrap_around: bool,
    /// The horizontal and vertical guide lines (View → Guides).
    guide_lines: Vec<crate::app::view::guide_lines::GuideLine>,
    guides_locked: bool,
}

impl StoredGuides {
    pub(crate) fn from_app(app: &PainterApp) -> Self {
        let g = &app.workspace.guides;
        let s = &app.workspace.symmetry;
        Self {
            ruler: [[g.ruler.a.x, g.ruler.a.y], [g.ruler.b.x, g.ruler.b.y]],
            ruler_enabled: g.ruler.enabled,
            ruler_parallel: g.ruler.parallel,
            assistants: g.assistants.clone(),
            symmetry: match s.mode {
                SymmetryMode::Off => "off",
                SymmetryMode::Vertical => "vertical",
                SymmetryMode::Horizontal => "horizontal",
                SymmetryMode::Both => "both",
                SymmetryMode::Radial => "radial",
            }
            .to_string(),
            symmetry_center: [s.center.x, s.center.y],
            symmetry_angle: s.angle,
            symmetry_count: s.count,
            symmetry_mirrored: s.mirrored,
            wrap_around: app.workspace.wrap_around,
            guide_lines: app.workspace.view_aids.guides.lines.clone(),
            guides_locked: app.workspace.view_aids.guides.locked,
        }
    }

    pub(crate) fn apply(self, app: &mut PainterApp) {
        let g = &mut app.workspace.guides;
        let [a, b] = self.ruler.map(|p| Vec2::new(p[0], p[1]));
        g.ruler = Ruler {
            a,
            b,
            enabled: self.ruler_enabled,
            parallel: self.ruler_parallel,
        };
        g.assistants = self.assistants;
        g.end_drag();
        let s = &mut app.workspace.symmetry;
        s.mode = match self.symmetry.as_str() {
            "vertical" => SymmetryMode::Vertical,
            "horizontal" => SymmetryMode::Horizontal,
            "both" => SymmetryMode::Both,
            "radial" => SymmetryMode::Radial,
            _ => SymmetryMode::Off,
        };
        s.center = Vec2::new(self.symmetry_center[0], self.symmetry_center[1]);
        s.angle = self.symmetry_angle;
        if self.symmetry_count >= 2 {
            s.count = self
                .symmetry_count
                .min(crate::brush_engine::symmetry::MAX_COUNT);
        }
        s.mirrored = self.symmetry_mirrored;
        app.workspace.wrap_around = self.wrap_around;
        let lines = &mut app.workspace.view_aids.guides;
        lines.lines = self.guide_lines;
        lines.locked = self.guides_locked;
        lines.end_drag();
        app.keep_guides_on_canvas();
    }
}

/// Within this distance of the ruler (screen points), a stroke runs along
/// the ruler itself.
const RULER_SNAP: f32 = 28.0;

#[derive(Default)]
pub struct GuideState {
    drag: Option<Drag>,
    /// Show the mirror axes (and their handles) while mirror painting.
    pub hide_symmetry: bool,
    pub ruler: Ruler,
    /// The drawing assistants.
    pub assistants: Vec<Assistant>,
    /// What the current stroke follows (a ruler line, an assistant).
    lock: Option<Lock>,
    /// The current stroke's last point on an ellipse, and its pressure: the
    /// next sample is joined to it round the curve.
    last_on_curve: Option<(Vec2, f32)>,
}

impl GuideState {
    /// Let go of any handle being dragged.
    pub(crate) fn end_drag(&mut self) {
        self.drag = None;
        self.lock = None;
        self.last_on_curve = None;
    }
}

/// Most degrees round an ellipse between two points of a stroke: a sample
/// further round is joined to the last through points in between, so the
/// stroke follows the curve rather than cutting across it.
const CURVE_STEP: f32 = 3.0;

impl PainterApp {
    fn symmetry_guides_shown(&self) -> bool {
        self.workspace.symmetry.is_active() && !self.workspace.guides.hide_symmetry
    }

    fn rotate_handle(&self) -> Vec2 {
        // Along the horizontal axis's direction, whatever the mode.
        let s = &self.workspace.symmetry;
        let dir = Vec2::new(s.angle.cos(), s.angle.sin());
        s.center + dir * (ROTATE_HANDLE / self.viewport.zoom.max(0.01))
    }

    /// Put the symmetry centre in the middle of the canvas.
    pub(crate) fn centre_symmetry(&mut self) {
        self.workspace.symmetry.center =
            Vec2::new(self.canvas.width() as f32, self.canvas.height() as f32) * 0.5;
    }

    /// Keep the symmetry centre on the canvas (a new or smaller canvas).
    pub(crate) fn keep_guides_on_canvas(&mut self) {
        let c = self.workspace.symmetry.center;
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        if c == Vec2::ZERO || c.x < 0.0 || c.y < 0.0 || c.x > w || c.y > h {
            self.centre_symmetry();
        }
    }

    /// Show or hide the ruler, placing it across the middle of the canvas
    /// the first time.
    pub(crate) fn set_ruler(&mut self, enabled: bool) {
        let ruler = &mut self.workspace.guides.ruler;
        ruler.enabled = enabled;
        if enabled && ruler.a == ruler.b {
            let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
            ruler.a = Vec2::new(w * 0.2, h * 0.5);
            ruler.b = Vec2::new(w * 0.8, h * 0.5);
        }
    }

    /// The enabled perspective assistants, for brushes that follow
    /// perspective.
    pub(crate) fn perspective_grids(&self) -> Vec<crate::brush_engine::dynamics::PerspectiveGrid> {
        self.workspace
            .guides
            .assistants
            .iter()
            .filter(|a| a.enabled && a.kind == AssistantKind::Perspective)
            .filter_map(|a| {
                crate::brush_engine::dynamics::PerspectiveGrid::new(std::array::from_fn(|i| {
                    a.point(i)
                }))
            })
            .collect()
    }

    /// The start of a freehand stroke at `pos`: pick what it follows (the
    /// ruler or a line parallel to it, an assistant) and put `pos` on it.
    pub(crate) fn ruler_begin_stroke(&mut self, pos: Vec2) -> Vec2 {
        let zoom = self.viewport.zoom.max(0.01);
        let snap = RULER_SNAP / zoom;
        let guides = &mut self.workspace.guides;
        guides.lock = None;
        guides.last_on_curve = None;
        let ruler = guides.ruler;
        let d = ruler.b - ruler.a;
        let ruler_dir = (ruler.enabled && d.length() >= 1e-3).then(|| d.normalized());
        let near_ruler = ruler_dir.is_some_and(|dir| {
            let off = pos - ruler.a;
            (off.x * dir.y - off.y * dir.x).abs() <= snap
        });
        let enabled = || guides.assistants.iter().filter(|a| a.enabled);
        let near_ellipse = enabled()
            .filter(|a| a.kind == AssistantKind::Ellipse)
            .filter_map(|a| a.ellipse())
            .filter(|e| e.distance(pos) <= snap)
            .min_by(|a, b| a.distance(pos).total_cmp(&b.distance(pos)));
        let concentric = enabled()
            .filter(|a| a.kind == AssistantKind::Concentric)
            .find_map(|a| a.ellipse());
        // Lines through the start towards the vanishing points (and
        // upright, with a perspective plane).
        let mut dirs: Vec<Vec2> = enabled()
            .flat_map(|a| a.vanishing_points())
            .filter(|vp| (*vp - pos).length() > 1.0)
            .map(|vp| (vp - pos).normalized())
            .collect();
        if !dirs.is_empty() && enabled().any(|a| a.kind == AssistantKind::Perspective) {
            dirs.push(Vec2::new(0.0, 1.0));
        }
        guides.lock = if let (true, Some(dir)) = (near_ruler, ruler_dir) {
            Some(Lock::Fixed(Constraint::Line {
                origin: ruler.a,
                dir,
            }))
        } else if let Some(e) = near_ellipse {
            Some(Lock::Fixed(Constraint::Ellipse(e)))
        } else if let Some(e) = concentric {
            Some(Lock::Fixed(Constraint::Ellipse(e.through(pos))))
        } else if !dirs.is_empty() {
            Some(Lock::Choosing { start: pos, dirs })
        } else if let (true, Some(dir)) = (ruler.parallel, ruler_dir) {
            Some(Lock::Fixed(Constraint::Line { origin: pos, dir }))
        } else {
            None
        };
        self.ruler_snap(pos)
    }

    /// `pos` put on what the current stroke follows, if anything.
    pub(crate) fn ruler_snap(&mut self, pos: Vec2) -> Vec2 {
        let far = crate::app::tools::assistants::CHOOSE_AFTER / self.viewport.zoom.max(0.01);
        match self.workspace.guides.lock.as_mut() {
            Some(lock) => lock.snap(pos, far),
            None => pos,
        }
    }

    /// The points a stroke sample at `pos` (with `pressure`) becomes: on
    /// what the stroke follows, and round an ellipse, joined to the last
    /// point through the points in between.
    pub(crate) fn guide_samples(&mut self, pos: Vec2, pressure: f32) -> Vec<(Vec2, f32)> {
        let p = self.ruler_snap(pos);
        let guides = &mut self.workspace.guides;
        let Some(e) = guides.lock.as_ref().and_then(Lock::ellipse) else {
            return vec![(p, pressure)];
        };
        let from = guides.last_on_curve.replace((p, pressure));
        let Some((last, last_pressure)) = from else {
            return vec![(p, pressure)];
        };
        let (t0, t1) = (e.theta(last), e.theta(p));
        // The short way round.
        let mut turn = t1 - t0;
        if turn > std::f32::consts::PI {
            turn -= std::f32::consts::TAU;
        } else if turn < -std::f32::consts::PI {
            turn += std::f32::consts::TAU;
        }
        let steps = (turn.abs().to_degrees() / CURVE_STEP).ceil().max(1.0) as usize;
        (1..=steps)
            .map(|i| {
                let k = i as f32 / steps as f32;
                let q = if i == steps { p } else { e.at(t0 + turn * k) };
                (q, last_pressure + (pressure - last_pressure) * k)
            })
            .collect()
    }

    /// The start of a stroke on a curve: remember its first point.
    pub(crate) fn note_curve_start(&mut self, pos: Vec2, pressure: f32) {
        let guides = &mut self.workspace.guides;
        if guides.lock.as_ref().and_then(Lock::ellipse).is_some() {
            guides.last_on_curve = Some((pos, pressure));
        }
    }

    /// Whether the current stroke goes round an ellipse.
    pub(crate) fn stroke_on_curve(&self) -> bool {
        self.workspace
            .guides
            .lock
            .as_ref()
            .and_then(Lock::ellipse)
            .is_some()
    }

    /// Add an assistant of `kind`, placed in the middle of the canvas.
    pub(crate) fn add_assistant(&mut self, kind: AssistantKind) {
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        self.workspace
            .guides
            .assistants
            .push(Assistant::new(kind, w, h));
    }

    /// Which handle (if any) is under canvas point `pos`.
    fn guide_hit(&self, pos: Vec2) -> Option<Drag> {
        let hit = HANDLE_HIT / self.viewport.zoom.max(0.01);
        for (index, a) in self.workspace.guides.assistants.iter().enumerate() {
            if !a.enabled {
                continue;
            }
            for handle in 0..a.kind.handles() {
                if (pos - a.point(handle)).length() <= hit {
                    return Some(Drag::Assistant { index, handle });
                }
            }
        }
        let ruler = self.workspace.guides.ruler;
        if ruler.enabled {
            for (i, end) in [ruler.a, ruler.b].into_iter().enumerate() {
                if (pos - end).length() <= hit {
                    return Some(Drag::RulerEnd(i));
                }
            }
            // The middle handle moves it (strokes start along its body).
            if (pos - (ruler.a + ruler.b) * 0.5).length() <= hit {
                return Some(Drag::RulerMove { last: pos });
            }
        }
        if !self.symmetry_guides_shown() {
            return None;
        }
        let center = self.workspace.symmetry.center;
        if (pos - center).length() <= hit {
            return Some(Drag::SymmetryCenter { grab: pos - center });
        }
        if (pos - self.rotate_handle()).length() <= hit {
            return Some(Drag::SymmetryAngle);
        }
        None
    }

    /// Whether canvas point `pos` is over a guide handle.
    pub(crate) fn over_guide_handle(&self, pos: Vec2) -> bool {
        self.guide_hit(pos).is_some()
    }

    /// A press at canvas point `pos`: grab a guide handle under it. Returns
    /// whether the press was the guide's.
    pub(crate) fn guides_press(&mut self, pos: Vec2) -> bool {
        self.workspace.guides.drag = self.guide_hit(pos);
        self.workspace.guides.drag.is_some()
    }

    /// Drag the grabbed handle to `pos`; `snap` (Shift) keeps the angle to
    /// 15° steps. Returns whether a handle is being dragged.
    pub(crate) fn guides_drag(&mut self, pos: Vec2, snap: bool) -> bool {
        let Some(drag) = self.workspace.guides.drag else {
            return false;
        };
        let zoom = self.viewport.zoom.max(0.01);
        match drag {
            Drag::SymmetryCenter { grab } => {
                let mut c = pos - grab;
                // Snap to the middle of the canvas.
                let mid = Vec2::new(self.canvas.width() as f32, self.canvas.height() as f32) * 0.5;
                let snap_distance = SNAP_DISTANCE / zoom;
                if (c.x - mid.x).abs() <= snap_distance {
                    c.x = mid.x;
                }
                if (c.y - mid.y).abs() <= snap_distance {
                    c.y = mid.y;
                }
                self.workspace.symmetry.center = c;
            }
            Drag::RulerEnd(i) => {
                let ruler = &mut self.workspace.guides.ruler;
                let other = if i == 0 { ruler.b } else { ruler.a };
                let end = if snap {
                    let d = pos - other;
                    let a = (d.y.atan2(d.x) / ANGLE_STEP).round() * ANGLE_STEP;
                    other + Vec2::new(a.cos(), a.sin()) * d.length()
                } else {
                    pos
                };
                if i == 0 {
                    ruler.a = end;
                } else {
                    ruler.b = end;
                }
            }
            Drag::Assistant { index, handle } => {
                if let Some(a) = self.workspace.guides.assistants.get_mut(index) {
                    a.drag_handle(handle, pos);
                }
            }
            Drag::RulerMove { last } => {
                let ruler = &mut self.workspace.guides.ruler;
                ruler.a += pos - last;
                ruler.b += pos - last;
                self.workspace.guides.drag = Some(Drag::RulerMove { last: pos });
            }
            Drag::SymmetryAngle => {
                let s = &mut self.workspace.symmetry;
                let d = pos - s.center;
                if d.length() > 1.0 {
                    let mut angle = d.y.atan2(d.x);
                    // Without Shift it still settles on upright within a
                    // few degrees.
                    let step = if snap {
                        ANGLE_STEP
                    } else {
                        std::f32::consts::FRAC_PI_2
                    };
                    let snapped = (angle / step).round() * step;
                    if snap || (angle - snapped).abs() < 0.05 {
                        angle = snapped;
                    }
                    s.angle = angle;
                }
            }
        }
        true
    }

    /// Let go of the handle. Returns whether one was being dragged.
    pub(crate) fn guides_release(&mut self) -> bool {
        self.workspace.guides.drag.take().is_some()
    }

    pub(crate) fn guides_dragging(&self) -> bool {
        self.workspace.guides.drag.is_some()
    }
}

/// The part of the line through `p` with direction `dir` (from `t0` to
/// `t1` along it) inside the canvas, if any.
fn clip_to_canvas(p: Vec2, dir: Vec2, t0: f32, t1: f32, w: f32, h: f32) -> Option<(Vec2, Vec2)> {
    let (mut lo, mut hi) = (t0, t1);
    for (pv, dv, max) in [(p.x, dir.x, w), (p.y, dir.y, h)] {
        if dv.abs() < 1e-9 {
            if pv < 0.0 || pv > max {
                return None;
            }
            continue;
        }
        let (a, b) = ((0.0 - pv) / dv, (max - pv) / dv);
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
    }
    (lo < hi).then(|| (p + dir * lo, p + dir * hi))
}

/// Draw the guides over the canvas.
pub(crate) fn draw_guides(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    draw_ruler(app, painter, map);
    draw_assistants(app, painter, map);
    if !app.symmetry_guides_shown() {
        return;
    }
    let s = &app.workspace.symmetry;
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let far = w.hypot(h) * 2.0;
    let line = |a: Vec2, b: Vec2| {
        let (a, b) = (map.to_screen(a), map.to_screen(b));
        painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
        painter.line_segment([a, b], Stroke::new(1.0_f32, GUIDE_COLOR));
    };
    let half_lines = s.mode == SymmetryMode::Radial;
    for dir in s.axis_directions() {
        let t0 = if half_lines { 0.0 } else { -far };
        if let Some((a, b)) = clip_to_canvas(s.center, dir, t0, far, w, h) {
            line(a, b);
        }
    }
    let handle = |p: Pos2, filled: bool| {
        painter.circle_filled(p, HANDLE_RADIUS + 1.5, Color32::BLACK);
        if filled {
            painter.circle_filled(p, HANDLE_RADIUS, GUIDE_COLOR);
        } else {
            painter.circle_filled(p, HANDLE_RADIUS, Color32::from_gray(30));
            painter.circle_stroke(p, HANDLE_RADIUS - 1.5, Stroke::new(1.5_f32, GUIDE_COLOR));
        }
    };
    let center = map.to_screen(s.center);
    let rotate = map.to_screen(app.rotate_handle());
    painter.line_segment(
        [center, rotate],
        Stroke::new(1.0_f32, GUIDE_COLOR.gamma_multiply(0.6)),
    );
    handle(center, true);
    handle(rotate, false);
}

fn draw_assistants(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let far = w.hypot(h) * 2.0;
    let faint = Stroke::new(1.0_f32, GUIDE_COLOR.gamma_multiply(0.3));
    let line = |a: Vec2, b: Vec2, stroke: Stroke| {
        painter.line_segment([map.to_screen(a), map.to_screen(b)], stroke);
    };
    let solid = |a: Vec2, b: Vec2| {
        line(a, b, Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
        line(a, b, Stroke::new(1.0_f32, GUIDE_COLOR));
    };
    let handle = |p: Vec2| {
        let p = map.to_screen(p);
        painter.circle_filled(p, HANDLE_RADIUS + 1.5, Color32::BLACK);
        painter.circle_filled(p, HANDLE_RADIUS, GUIDE_COLOR);
    };
    for a in app.workspace.guides.assistants.iter().filter(|a| a.enabled) {
        match a.kind {
            AssistantKind::VanishingPoint => {
                // Rays out of the point.
                let vp = a.point(0);
                for i in 0..24 {
                    let t = i as f32 / 24.0 * std::f32::consts::TAU;
                    let dir = Vec2::new(t.cos(), t.sin());
                    if let Some((p, q)) = clip_to_canvas(vp, dir, 0.0, far, w, h) {
                        line(p, q, faint);
                    }
                }
            }
            AssistantKind::Perspective => {
                let p: Vec<Vec2> = (0..4).map(|i| a.point(i)).collect();
                // A grid on the plane: lines between points along opposite
                // sides (straight lines in perspective, near enough).
                for k in 1..8 {
                    let t = k as f32 / 8.0;
                    let lerp = |u: Vec2, v: Vec2| u + (v - u) * t;
                    line(lerp(p[0], p[1]), lerp(p[3], p[2]), faint);
                    line(lerp(p[0], p[3]), lerp(p[1], p[2]), faint);
                }
                for i in 0..4 {
                    solid(p[i], p[(i + 1) % 4]);
                }
                // The sides carried on to the vanishing points.
                for vp in a.vanishing_points() {
                    for q in &p {
                        let d = vp - *q;
                        if d.length() > 1.0
                            && let Some((u, v)) =
                                clip_to_canvas(*q, d.normalized(), 0.0, d.length(), w, h)
                        {
                            line(u, v, faint);
                        }
                    }
                }
            }
            AssistantKind::Ellipse | AssistantKind::Concentric => {
                let Some(e) = a.ellipse() else {
                    continue;
                };
                let ring = |e: &crate::app::tools::assistants::Ellipse, stroke: Stroke| {
                    let points: Vec<Pos2> = (0..=96)
                        .map(|i| map.to_screen(e.at(i as f32 / 96.0 * std::f32::consts::TAU)))
                        .collect();
                    painter.add(egui::Shape::line(points, stroke));
                };
                if a.kind == AssistantKind::Concentric {
                    for k in [0.5, 1.5, 2.0] {
                        let inner = e.through(e.at(0.0) + (e.at(0.0) - e.center) * (k - 1.0));
                        ring(&inner, faint);
                    }
                }
                ring(&e, Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
                ring(&e, Stroke::new(1.0_f32, GUIDE_COLOR));
                solid(a.point(0), a.point(1));
                line(a.point(0), a.point(2), Stroke::new(1.0_f32, GUIDE_COLOR));
            }
        }
        for i in 0..a.kind.handles() {
            handle(a.point(i));
        }
    }
}

fn draw_ruler(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    let ruler = app.workspace.guides.ruler;
    if !ruler.enabled || (ruler.b - ruler.a).length() < 1e-3 {
        return;
    }
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let dir = (ruler.b - ruler.a).normalized();
    // Where strokes are straightened: the whole line, faintly.
    let far = w.hypot(h) * 2.0;
    if let Some((a, b)) = clip_to_canvas(ruler.a, dir, -far, far, w, h) {
        painter.line_segment(
            [map.to_screen(a), map.to_screen(b)],
            Stroke::new(1.0_f32, GUIDE_COLOR.gamma_multiply(0.35)),
        );
    }
    let (a, b) = (map.to_screen(ruler.a), map.to_screen(ruler.b));
    // The ruler's body: a band with tick marks.
    let normal = {
        let d = (b - a).normalized();
        egui::vec2(-d.y, d.x)
    };
    let band = 6.0;
    let quad = vec![
        a + normal * band,
        b + normal * band,
        b - normal * band,
        a - normal * band,
    ];
    painter.add(egui::Shape::convex_polygon(
        quad,
        GUIDE_COLOR.gamma_multiply(0.18),
        Stroke::new(1.0_f32, Color32::from_black_alpha(120)),
    ));
    painter.line_segment([a, b], Stroke::new(3.0_f32, Color32::from_black_alpha(140)));
    painter.line_segment([a, b], Stroke::new(1.0_f32, GUIDE_COLOR));
    let length = (b - a).length();
    let ticks = (length / 16.0).floor() as usize;
    for i in 1..ticks {
        let t = i as f32 / ticks as f32;
        let p = a + (b - a) * t;
        let size = if i % 4 == 0 { band } else { band * 0.5 };
        painter.line_segment([p, p + normal * size], Stroke::new(1.0_f32, GUIDE_COLOR));
    }
    for end in [a, b] {
        painter.circle_filled(end, HANDLE_RADIUS + 1.5, Color32::BLACK);
        painter.circle_filled(end, HANDLE_RADIUS, GUIDE_COLOR);
    }
    // The move handle: a square in the middle.
    let mid = egui::Rect::from_center_size(
        a + (b - a) * 0.5,
        egui::vec2(HANDLE_RADIUS, HANDLE_RADIUS) * 2.0,
    );
    painter.rect_filled(mid.expand(1.5), 0.0, Color32::BLACK);
    painter.rect_filled(mid, 0.0, GUIDE_COLOR);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_clipped_to_the_canvas() {
        let (a, b) = clip_to_canvas(
            Vec2::new(50.0, 50.0),
            Vec2::new(1.0, 0.0),
            -1e4,
            1e4,
            100.0,
            80.0,
        )
        .unwrap();
        assert!((a - Vec2::new(0.0, 50.0)).length() < 1e-3);
        assert!((b - Vec2::new(100.0, 50.0)).length() < 1e-3);
        assert!(
            clip_to_canvas(
                Vec2::new(50.0, 90.0),
                Vec2::new(1.0, 0.0),
                -1e4,
                1e4,
                100.0,
                80.0
            )
            .is_none()
        );
        // A half line from the centre only goes one way.
        let (a, _) = clip_to_canvas(
            Vec2::new(50.0, 40.0),
            Vec2::new(0.0, 1.0),
            0.0,
            1e4,
            100.0,
            80.0,
        )
        .unwrap();
        assert!((a - Vec2::new(50.0, 40.0)).length() < 1e-3);
    }

    #[test]
    fn dragging_the_centre_moves_the_mirror_and_snaps_to_the_middle() {
        use crate::canvas::Canvas;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(200, 100, Color32::WHITE, 64));
        app.workspace.symmetry.mode = SymmetryMode::Vertical;
        app.centre_symmetry();
        assert!(
            app.guides_press(Vec2::new(101.0, 50.0)),
            "grabbed the centre handle"
        );
        app.guides_drag(Vec2::new(141.0, 70.0), false);
        assert!(app.guides_release());
        assert_eq!(app.workspace.symmetry.center, Vec2::new(140.0, 70.0));
        // Back near the middle: it snaps there.
        app.guides_press(Vec2::new(140.0, 70.0));
        app.guides_drag(Vec2::new(103.0, 52.0), false);
        app.guides_release();
        assert_eq!(app.workspace.symmetry.center, Vec2::new(100.0, 50.0));
        // Away from the handles a press is the tool's.
        assert!(!app.guides_press(Vec2::new(20.0, 20.0)));
    }

    #[test]
    fn a_mirrored_stroke_paints_both_sides_as_one_step() {
        use crate::canvas::Canvas;
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.diameter = 6.0;
        app.workspace.symmetry.mode = SymmetryMode::Vertical;
        app.centre_symmetry();
        app.start_stroke_with_pressure(Vec2::new(20.0, 20.0), 1.0);
        app.add_stroke_point(Vec2::new(30.0, 40.0), 1.0);
        app.release_canvas();
        let painted = |app: &crate::PainterApp, x: i32, y: i32| {
            let tile = app
                .canvas
                .get_layer_tile_data(1, x / 64, y / 64)
                .unwrap_or_default();
            tile.get(((y % 64) * 64 + x % 64) as usize)
                .is_some_and(|c| c.a() > 0)
        };
        assert!(painted(&app, 20, 20));
        assert!(painted(&app, 108, 20), "the mirror image across x = 64");
        assert!(painted(&app, 98, 40));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1, "one undo step");
        app.apply_history(false);
        assert!(!painted(&app, 20, 20) && !painted(&app, 108, 20));
    }

    fn assistant_app() -> crate::PainterApp {
        use crate::canvas::Canvas;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.diameter = 3.0;
        app
    }

    /// Canvas points with paint.
    fn painted_points(app: &crate::PainterApp) -> Vec<Vec2> {
        let mut out = Vec::new();
        for y in 0..256 {
            for x in 0..256 {
                let tile = app.canvas.get_layer_tile_data(1, x / 64, y / 64);
                if tile.is_some_and(|t| t[((y % 64) * 64 + x % 64) as usize].a() > 128) {
                    out.push(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                }
            }
        }
        out
    }

    #[test]
    fn a_stroke_near_an_ellipse_goes_round_it() {
        let mut app = assistant_app();
        app.add_assistant(AssistantKind::Ellipse);
        let e = app.workspace.guides.assistants[0].ellipse().unwrap();
        // A wobbly, sparse freehand arc near the ellipse.
        let path: Vec<Vec2> = (0..=12)
            .map(|i| {
                let t = i as f32 * 0.25;
                e.at(t) + Vec2::new((t * 7.0).sin() * 4.0, (t * 5.0).cos() * 4.0)
            })
            .collect();
        app.start_stroke_with_pressure(path[0], 1.0);
        for &p in &path[1..] {
            app.add_stroke_point(p, 1.0);
        }
        app.release_canvas();
        let painted = painted_points(&app);
        assert!(painted.len() > 100);
        let worst = painted.iter().map(|&p| e.distance(p)).fold(0.0, f32::max);
        assert!(worst < 2.5, "strayed {worst} px from the ellipse");
        // Round the curve, not across it: its middle stays empty.
        assert!(painted.iter().all(|p| (*p - e.center).length() > e.b * 0.8));
    }

    #[test]
    fn a_stroke_runs_towards_the_vanishing_point() {
        let mut app = assistant_app();
        app.add_assistant(AssistantKind::VanishingPoint);
        let vp = app.workspace.guides.assistants[0].point(0);
        let start = Vec2::new(40.0, 220.0);
        app.start_stroke_with_pressure(start, 1.0);
        // Roughly towards the point, wobbling.
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            let p = start + (vp - start) * t * 0.8 + Vec2::new(6.0 * (t * 9.0).sin(), 0.0);
            app.add_stroke_point(p, 1.0);
        }
        app.release_canvas();
        let dir = (vp - start).normalized();
        for p in painted_points(&app) {
            let off = p - start;
            let across = (off.x * dir.y - off.y * dir.x).abs();
            assert!(
                across < 2.5,
                "{p:?} is {across} px off the line to the point"
            );
        }
    }

    #[test]
    fn a_perspective_stroke_picks_the_direction_it_starts_in() {
        let mut app = assistant_app();
        app.add_assistant(AssistantKind::Perspective);
        let start = Vec2::new(128.0, 200.0);
        app.ruler_begin_stroke(start);
        // Straight up: the upright line, whatever the plane's sides do.
        let p = app.ruler_snap(Vec2::new(131.0, 150.0));
        assert!((p.x - start.x).abs() < 1e-3, "{p:?}");
        let vps = app.workspace.guides.assistants[0].vanishing_points();
        assert!(!vps.is_empty());
        // Towards a vanishing point: along the line to it.
        app.workspace.guides.end_drag();
        app.ruler_begin_stroke(start);
        let dir = (vps[0] - start).normalized();
        let p = app.ruler_snap(start + dir * 40.0 + Vec2::new(dir.y, -dir.x) * 5.0);
        let off = p - start;
        assert!((off.x * dir.y - off.y * dir.x).abs() < 1e-2);
    }

    #[test]
    fn guides_are_saved_with_the_project() {
        let mut app = assistant_app();
        app.add_assistant(AssistantKind::Concentric);
        app.workspace.guides.assistants[0].set_point(0, Vec2::new(70.0, 80.0));
        app.set_ruler(true);
        app.workspace.symmetry.mode = SymmetryMode::Radial;
        app.workspace.symmetry.count = 5;
        app.workspace.wrap_around = true;
        let saved = StoredGuides::from_app(&app);
        let json = serde_json::to_string(&saved).unwrap();
        let mut other = assistant_app();
        serde_json::from_str::<StoredGuides>(&json)
            .unwrap()
            .apply(&mut other);
        assert_eq!(
            other.workspace.guides.assistants,
            app.workspace.guides.assistants
        );
        assert_eq!(other.workspace.guides.ruler, app.workspace.guides.ruler);
        assert_eq!(other.workspace.symmetry.mode, SymmetryMode::Radial);
        assert_eq!(other.workspace.symmetry.count, 5);
        assert!(other.workspace.wrap_around);
        // Older files have none: nothing changes.
        let empty: StoredGuides = serde_json::from_str("{}").unwrap();
        assert!(empty.assistants.is_empty());
    }

    #[test]
    fn the_ruler_straightens_strokes() {
        use crate::canvas::Canvas;
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(200, 200, Color32::WHITE, 64));
        app.set_ruler(true);
        app.workspace.guides.ruler.a = Vec2::new(20.0, 100.0);
        app.workspace.guides.ruler.b = Vec2::new(180.0, 100.0);
        // Starting next to the ruler: along the ruler itself.
        let start = app.ruler_begin_stroke(Vec2::new(50.0, 110.0));
        assert_eq!(start, Vec2::new(50.0, 100.0));
        assert_eq!(
            app.ruler_snap(Vec2::new(120.0, 90.0)),
            Vec2::new(120.0, 100.0)
        );
        // Starting away from it: parallel to it, through the start.
        app.ruler_begin_stroke(Vec2::new(50.0, 40.0));
        assert_eq!(
            app.ruler_snap(Vec2::new(120.0, 60.0)),
            Vec2::new(120.0, 40.0)
        );
        // Not parallel: left alone.
        app.workspace.guides.ruler.parallel = false;
        app.ruler_begin_stroke(Vec2::new(50.0, 40.0));
        assert_eq!(
            app.ruler_snap(Vec2::new(120.0, 60.0)),
            Vec2::new(120.0, 60.0)
        );
    }
}
