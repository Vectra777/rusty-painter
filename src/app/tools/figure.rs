//! The Figure tool, on a 3D figure layer: frame the scene and
//! pose its figures.
//!
//! Hovering lights up the part of a figure under the pointer. Dragging a
//! part poses by inverse kinematics: the point grabbed follows the pointer
//! (drag a hand and the arm follows, a foot and the leg bends, the head and
//! the neck and chest bend; the hips move the whole figure). Shift+drag
//! turns just that part's joint; Alt+drag moves the whole figure. A click
//! selects the part's joint: three rings round it turn it about its own
//! axes (drag a ring). A double-click pins a joint (or unpins it): pinned
//! joints keep their place while the rest is posed. The selected figure has
//! a ring on the ground: drag the ring to turn the figure, its middle to
//! move it across the ground.
//!
//! Drag empty space (or with the middle button) to orbit the camera round
//! the scene, Shift+drag to pan it, the wheel to move it closer or away
//! (Ctrl+wheel zooms the view as usual). On a touch screen, two fingers
//! orbit, pinch to dolly and twist to roll the camera (see
//! `input::touch`). Each drag is one undo step.

use std::time::{Duration, Instant};

use crate::PainterApp;
use crate::canvas::figure::ik::{Chain, Effector};
use crate::canvas::figure::{self, FigureLayer, FigureModel};
use crate::ui::style::accent;
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};
use glam::{Mat4, Quat, Vec3};

/// The rings' radius on screen, in points.
const GIZMO_RADIUS: f32 = 70.0;
/// The ground ring's radius round the selected figure, in points.
const ROOT_RADIUS: f32 = 60.0;
/// Samples round each ring.
const RING_STEPS: usize = 64;
/// How near (on screen, points) a press must be to a joint or a ring.
const PICK_RADIUS: f32 = 10.0;
/// Orbit speed: radians per screen point dragged.
const ORBIT_SPEED: f32 = 0.008;
/// Wheel steps this close together are one undo step.
const WHEEL_GESTURE: Duration = Duration::from_millis(700);
/// Two presses on one joint this close together are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// A pose drag starts once the pointer has gone this far (points), so a
/// click doesn't nudge the figure.
const DRAG_START: f32 = 2.0;
/// Most triangles lit up under the pointer.
const MAX_HIGHLIGHT: usize = 40_000;

/// The keys held as a drag starts.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FigureMods {
    pub shift: bool,
    pub alt: bool,
}

/// A plane in the scene.
#[derive(Clone, Copy, Debug)]
struct Plane {
    point: Vec3,
    normal: Vec3,
}

impl Plane {
    /// Where a ray from `origin` along `dir` meets it.
    fn hit(&self, origin: Vec3, dir: Vec3) -> Option<Vec3> {
        let along = dir.dot(self.normal);
        if along.abs() < 1e-6 {
            return None;
        }
        let t = (self.point - origin).dot(self.normal) / along;
        (t > 0.0).then(|| origin + dir * t)
    }
}

#[derive(Clone, Debug)]
enum FigureDrag {
    /// Turning the camera round its target, from the pointer at `last`.
    Orbit { last: Vec2, moved: bool },
    /// Sliding the camera.
    Pan { last: Vec2 },
    /// Turning joint `joint` of figure `figure` about its own `axis` from
    /// its turn at the press (`start`): the pointer pressed at `from`
    /// moves `speed` canvas pixels a radian along `tangent`.
    Rotate {
        figure: usize,
        joint: usize,
        axis: Vec3,
        start: Quat,
        from: Vec2,
        tangent: Vec2,
        speed: f32,
    },
    /// Posing: `effector` (on figure `figure`) follows the pointer across
    /// `plane`, which faces the camera through where it was grabbed (the
    /// pointer pressed at `from`). `only_joint` turns its own joint alone;
    /// `pins` are the pinned joints and where they stay.
    Pose {
        figure: usize,
        effector: Effector,
        plane: Plane,
        from: Vec2,
        only_joint: bool,
        pins: Vec<(usize, Vec3)>,
        moved: bool,
    },
    /// Moving figure `figure` across `plane`: it was at `start` with the
    /// pointer at `grab` on the plane.
    Move {
        figure: usize,
        plane: Plane,
        grab: Vec3,
        start: [f32; 3],
    },
    /// Turning figure `figure` about the vertical through `center`: it
    /// was turned `start`, the pointer at `from` radians round.
    Turn {
        figure: usize,
        center: Vec3,
        from: f32,
        start: Quat,
    },
}

#[derive(Default)]
pub struct FigureToolState {
    /// The selected joint: its figure in the scene, and it.
    pub selected: Option<(usize, usize)>,
    /// The selected figure (with or without a joint).
    pub figure: Option<usize>,
    /// The part under the pointer: its figure and joint.
    pub hover: Option<(usize, usize)>,
    drag: Option<FigureDrag>,
    /// When the wheel last moved the camera.
    last_wheel: Option<Instant>,
    /// The last press on a joint: when, and which.
    last_click: Option<(Instant, usize, usize)>,
}

/// The scene as the tool sees it: projected onto the canvas.
pub(crate) struct SceneView {
    view_proj: Mat4,
    width: usize,
    height: usize,
    /// Each figure's model and joints' matrices in the scene.
    figures: Vec<(figure::SharedModel, Vec<Mat4>)>,
}

impl SceneView {
    fn of(layer: &FigureLayer, width: usize, height: usize) -> Self {
        // Every figure, in step with the scene's (one that doesn't load
        // has no joints).
        let figures = layer
            .scene
            .iter()
            .map(|f| match f.model.resolve() {
                Ok(model) => {
                    let world = f.world(&model);
                    (model, world)
                }
                Err(_) => (crate::canvas::figure::mannequin::mannequin(), Vec::new()),
            })
            .collect();
        Self {
            view_proj: layer.view_proj(width, height),
            width,
            height,
            figures,
        }
    }

    /// A scene point on the canvas (pixels) and its depth.
    fn project(&self, p: Vec3) -> Option<(Vec2, f32)> {
        figure::project(&self.view_proj, p, self.width, self.height)
            .map(|(px, depth)| (Vec2::new(px[0], px[1]), depth))
    }

    /// The ray through canvas point `p`.
    fn ray(&self, p: Vec2) -> (Vec3, Vec3) {
        figure::unproject(&self.view_proj, [p.x, p.y], self.width, self.height)
    }

    fn joint(&self, figure: usize, joint: usize) -> Option<(&FigureModel, Mat4)> {
        let (model, world) = self.figures.get(figure)?;
        Some((model, *world.get(joint)?))
    }

    /// Scene units per canvas pixel at `at`.
    fn per_pixel(&self, at: Vec3, camera: &figure::FigureCamera) -> Option<f32> {
        let across = camera.view().inverse().transform_vector3(Vec3::X);
        let (a, _) = self.project(at)?;
        let (b, _) = self.project(at + across * 0.01)?;
        Some(0.01 / (b - a).length().max(1e-6))
    }
}

/// One of the rotation gizmo's rings: the joint's own axis (in its frame
/// and in the scene) and points round it (in the scene, on the canvas,
/// and whether that side faces the camera).
pub(crate) struct Ring {
    local: Vec3,
    axis: Vec3,
    points: Vec<(Vec3, Vec2, bool)>,
}

/// The rotation gizmo round the selected joint.
pub(crate) struct Gizmo {
    figure: usize,
    joint: usize,
    center: Vec3,
    center_px: Vec2,
    rings: Vec<Ring>,
}

/// The ground ring round the selected figure: its middle (in the scene
/// and on the canvas) and points round it on the canvas.
pub(crate) struct RootGizmo {
    center: Vec3,
    center_px: Vec2,
    points: Vec<Vec2>,
}

/// What a press landed on: joint `joint` of figure `figure` (its dot, or
/// a part of the figure it moves), the point `point` grabbed, riding with
/// joint `owner`.
#[derive(Clone, Copy, Debug)]
struct Grab {
    figure: usize,
    joint: usize,
    owner: usize,
    point: Vec3,
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 1e-9 {
        ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + ab * t - p).length()
}

impl PainterApp {
    /// The selected figure layer's scene projected onto the canvas.
    pub(crate) fn figure_view(&self) -> Option<SceneView> {
        let layer = self.active_figure()?;
        Some(SceneView::of(
            layer,
            self.canvas.width(),
            self.canvas.height(),
        ))
    }

    /// Canvas pixels a screen point covers.
    fn figure_pick_radius(&self, points: f32) -> f32 {
        points / self.viewport.zoom.max(1e-3)
    }

    /// The joint whose dot is nearest canvas point `p`, within reach.
    fn figure_joint_at(&self, view: &SceneView, p: Vec2) -> Option<(usize, usize)> {
        let reach = self.figure_pick_radius(PICK_RADIUS);
        let mut best: Option<((usize, usize), f32, f32)> = None;
        for (f, (_, world)) in view.figures.iter().enumerate() {
            for (j, m) in world.iter().enumerate() {
                let Some((px, depth)) = view.project(m.w_axis.truncate()) else {
                    continue;
                };
                let d = (px - p).length();
                if d > reach {
                    continue;
                }
                // Nearest on the canvas; the nearer to the camera if level.
                let better =
                    best.is_none_or(|(_, bd, bz)| d < bd - 0.5 || (d < bd + 0.5 && depth < bz));
                if better {
                    best = Some(((f, j), d, depth));
                }
            }
        }
        best.map(|(fj, _, _)| fj)
    }

    /// What's under canvas point `p`: a joint's dot, else a part of a
    /// figure.
    fn figure_grab_at(&self, view: &SceneView, p: Vec2) -> Option<Grab> {
        if let Some((figure, joint)) = self.figure_joint_at(view, p) {
            let (model, m) = view.joint(figure, joint)?;
            // A joint's place rides with the bone above it.
            let owner = model.skeleton.joints[joint].parent.unwrap_or(joint);
            return Some(Grab {
                figure,
                joint,
                owner,
                point: m.w_axis.truncate(),
            });
        }
        let layer = self.active_figure()?;
        let (origin, dir) = view.ray(p);
        let hit = layer.pick(origin, dir)?;
        Some(Grab {
            figure: hit.figure,
            joint: hit.joint,
            owner: hit.joint,
            point: hit.point,
        })
    }

    /// The pointer moved to canvas point `pos` with no button down: light
    /// up the part under it. Returns whether that changed.
    pub(crate) fn figure_hover(&mut self, pos: Vec2) -> bool {
        if self.workspace.figure.tool.drag.is_some() {
            return false;
        }
        let hover = self
            .figure_view()
            .and_then(|view| self.figure_grab_at(&view, pos))
            .map(|g| (g.figure, g.joint));
        let changed = self.workspace.figure.tool.hover != hover;
        self.workspace.figure.tool.hover = hover;
        changed
    }

    /// The rotation gizmo round the selected joint, if one is.
    pub(crate) fn figure_gizmo(&self) -> Option<Gizmo> {
        let (figure, joint) = self.workspace.figure.tool.selected?;
        let view = self.figure_view()?;
        self.figure_gizmo_in(&view, figure, joint)
    }

    fn figure_gizmo_in(&self, view: &SceneView, figure: usize, joint: usize) -> Option<Gizmo> {
        let (_, m) = view.joint(figure, joint)?;
        let (_, turn, center) = m.to_scale_rotation_translation();
        let (center_px, _) = view.project(center)?;
        // A ring as wide on screen wherever the joint is.
        let layer = self.active_figure()?;
        let per_pixel = view.per_pixel(center, &layer.camera)?;
        let radius = self.figure_pick_radius(GIZMO_RADIUS) * per_pixel;
        let eye = layer.camera.eye();
        let rings = [Vec3::X, Vec3::Y, Vec3::Z]
            .into_iter()
            .map(|local| {
                let axis = (turn * local).normalize_or(local);
                let (u, v) = axis.any_orthonormal_pair();
                let points = (0..=RING_STEPS)
                    .filter_map(|k| {
                        let a = std::f32::consts::TAU * k as f32 / RING_STEPS as f32;
                        let p = center + (u * a.cos() + v * a.sin()) * radius;
                        let (px, _) = view.project(p)?;
                        Some((p, px, (p - center).dot(eye - center) >= 0.0))
                    })
                    .collect();
                Ring {
                    local,
                    axis,
                    points,
                }
            })
            .collect();
        Some(Gizmo {
            figure,
            joint,
            center,
            center_px,
            rings,
        })
    }

    /// The ground ring round the selected figure.
    pub(crate) fn figure_root_gizmo(&self) -> Option<RootGizmo> {
        let figure = self.figure_selected_instance()?;
        let view = self.figure_view()?;
        self.figure_root_gizmo_in(&view, figure)
    }

    fn figure_root_gizmo_in(&self, view: &SceneView, figure: usize) -> Option<RootGizmo> {
        let layer = self.active_figure()?;
        let center = Vec3::from(layer.scene.get(figure)?.position);
        let (center_px, _) = view.project(center)?;
        let radius =
            self.figure_pick_radius(ROOT_RADIUS) * view.per_pixel(center, &layer.camera)?;
        let points = (0..=RING_STEPS)
            .filter_map(|k| {
                let a = std::f32::consts::TAU * k as f32 / RING_STEPS as f32;
                let p = center + Vec3::new(a.sin(), 0.0, a.cos()) * radius;
                Some(view.project(p)?.0)
            })
            .collect();
        Some(RootGizmo {
            center,
            center_px,
            points,
        })
    }

    /// The drag a press at `pos` on the selected figure's ground ring
    /// starts: its middle moves the figure, the ring turns it.
    fn figure_root_drag(&self, view: &SceneView, pos: Vec2) -> Option<FigureDrag> {
        let figure = self.figure_selected_instance()?;
        let gizmo = self.figure_root_gizmo_in(view, figure)?;
        let reach = self.figure_pick_radius(PICK_RADIUS);
        let ground = Plane {
            point: gizmo.center,
            normal: Vec3::Y,
        };
        let (origin, dir) = view.ray(pos);
        let instance = self.active_figure()?.scene.get(figure)?;
        if (pos - gizmo.center_px).length() <= reach {
            return Some(FigureDrag::Move {
                figure,
                plane: ground,
                grab: ground.hit(origin, dir).unwrap_or(gizmo.center),
                start: instance.position,
            });
        }
        let on_ring = (1..gizmo.points.len())
            .any(|k| to_segment(pos, gizmo.points[k - 1], gizmo.points[k]) <= reach);
        if !on_ring {
            return None;
        }
        let at = ground.hit(origin, dir)?;
        Some(FigureDrag::Turn {
            figure,
            center: gizmo.center,
            from: (at.x - gizmo.center.x).atan2(at.z - gizmo.center.z),
            start: Quat::from_array(instance.rotation).normalize(),
        })
    }

    /// A press with the Figure tool at canvas point `pos`.
    pub(crate) fn figure_press(&mut self, pos: Vec2, mods: FigureMods) {
        // A wheel's camera move ends here.
        self.figure_edit_done();
        let Some(view) = self.figure_view() else {
            self.report(
                "The Figure tool works on a 3D figure layer (Layer → New 3D Figure)".into(),
            );
            return;
        };
        let state = &self.workspace.figure.tool;
        // A joint's dot comes first: a ring may pass over another joint's,
        // and seen edge-on through its own.
        let on_dot = self.figure_joint_at(&view, pos).is_some();
        // A ring of the gizmo: turn the joint.
        if let Some((figure, joint)) = state.selected
            && !mods.alt
            && !on_dot
            && let Some(gizmo) = self.figure_gizmo_in(&view, figure, joint)
            && let Some(drag) = self.figure_ring_drag(&view, &gizmo, pos)
        {
            self.workspace.figure.tool.drag = Some(drag);
            return;
        }
        // A joint's dot, the selected figure's ground ring (to move or turn
        // it), or a part of a figure: a joint or part is selected, and
        // posed (or moved).
        let dot = self
            .figure_joint_at(&view, pos)
            .and_then(|_| self.figure_grab_at(&view, pos));
        if dot.is_none()
            && let Some(drag) = self.figure_root_drag(&view, pos)
        {
            self.workspace.figure.tool.drag = Some(drag);
            return;
        }
        if let Some(grab) = dot.or_else(|| self.figure_grab_at(&view, pos)) {
            let now = Instant::now();
            let tool = &mut self.workspace.figure.tool;
            let double = tool.last_click.is_some_and(|(t, f, j)| {
                f == grab.figure && j == grab.joint && now.duration_since(t) < DOUBLE_CLICK
            });
            tool.selected = Some((grab.figure, grab.joint));
            tool.figure = Some(grab.figure);
            tool.hover = None;
            if double {
                tool.last_click = None;
                tool.drag = None;
                self.figure_toggle_pin(grab.figure, grab.joint);
                return;
            }
            tool.last_click = Some((now, grab.figure, grab.joint));
            let Some(layer) = self.active_figure() else {
                return;
            };
            let toward_camera = (layer.camera.eye() - grab.point).normalize_or(Vec3::Z);
            let plane = Plane {
                point: grab.point,
                normal: toward_camera,
            };
            let instance = &layer.scene[grab.figure];
            let drag = if mods.alt {
                FigureDrag::Move {
                    figure: grab.figure,
                    plane,
                    grab: grab.point,
                    start: instance.position,
                }
            } else {
                let world = &view.figures[grab.figure].1;
                let pins = instance
                    .pinned
                    .iter()
                    .filter_map(|&j| Some((j as usize, world.get(j as usize)?.w_axis.truncate())))
                    .collect();
                FigureDrag::Pose {
                    figure: grab.figure,
                    effector: Effector::at(world, grab.owner, grab.point),
                    plane,
                    from: pos,
                    only_joint: mods.shift,
                    pins,
                    moved: false,
                }
            };
            self.workspace.figure.tool.drag = Some(drag);
            return;
        }
        self.workspace.figure.tool.drag = Some(if mods.shift {
            FigureDrag::Pan { last: pos }
        } else {
            FigureDrag::Orbit {
                last: pos,
                moved: false,
            }
        });
    }

    /// The drag a press at `pos` on one of `gizmo`'s rings starts.
    fn figure_ring_drag(&self, view: &SceneView, gizmo: &Gizmo, pos: Vec2) -> Option<FigureDrag> {
        let reach = self.figure_pick_radius(PICK_RADIUS);
        // The nearest ring point, the side facing the camera first.
        let mut best: Option<(usize, usize, f32)> = None;
        for (r, ring) in gizmo.rings.iter().enumerate() {
            for k in 1..ring.points.len() {
                let (a, b) = (ring.points[k - 1], ring.points[k]);
                let d = to_segment(pos, a.1, b.1) - if a.2 { 1.0 } else { 0.0 };
                if d <= reach && best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((r, k, d));
                }
            }
        }
        let (r, k, _) = best?;
        let ring = &gizmo.rings[r];
        let (p, p_px, _) = ring.points[k];
        // Which way a turn about the axis moves that point on the canvas,
        // and how far per radian.
        let tangent = ring.axis.cross(p - gizmo.center);
        let eps = 1e-3;
        let (ahead, _) = view.project(p + tangent * eps)?;
        let along = (ahead - p_px) / eps;
        let (tangent, speed) = if along.length() > 1e-3 {
            (along.normalized(), along.length())
        } else {
            let out = p_px - gizmo.center_px;
            (Vec2::new(-out.y, out.x).normalized(), out.length().max(1.0))
        };
        let layer = self.active_figure()?;
        let start = layer.scene.get(gizmo.figure)?.joint_pose(gizmo.joint);
        Some(FigureDrag::Rotate {
            figure: gizmo.figure,
            joint: gizmo.joint,
            axis: ring.local,
            start,
            from: pos,
            tangent,
            speed,
        })
    }

    /// The pointer moved to canvas point `pos` with the button down.
    pub(crate) fn figure_drag(&mut self, pos: Vec2) {
        let Some(drag) = self.workspace.figure.tool.drag.clone() else {
            return;
        };
        let zoom = self.viewport.zoom.max(1e-3);
        match drag {
            FigureDrag::Orbit { last, moved } => {
                let d = (pos - last) * zoom;
                if !moved && d.length() < 2.0 {
                    return;
                }
                self.workspace.figure.tool.drag = Some(FigureDrag::Orbit {
                    last: pos,
                    moved: true,
                });
                self.figure_edit("Orbit camera", |f| {
                    f.camera.orbit(-d.x * ORBIT_SPEED, d.y * ORBIT_SPEED)
                });
            }
            FigureDrag::Pan { last } => {
                let d = pos - last;
                self.workspace.figure.tool.drag = Some(FigureDrag::Pan { last: pos });
                let h = self.canvas.height() as f32;
                self.figure_edit("Pan camera", |f| f.camera.pan(d.x, d.y, h));
            }
            FigureDrag::Rotate {
                figure,
                joint,
                axis,
                start,
                from,
                tangent,
                speed,
            } => {
                let angle = (pos - from).dot(tangent) / speed;
                self.figure_edit("Pose", |f| {
                    let Some(instance) = f.scene.get_mut(figure) else {
                        return;
                    };
                    let mut turn = start * Quat::from_axis_angle(axis, angle);
                    if !instance.is_unlimited(joint)
                        && let Ok(model) = instance.model.resolve()
                        && let Some(j) = model.skeleton.joints.get(joint)
                    {
                        turn = j.limit.clamp(turn);
                    }
                    instance.set_joint_pose(joint, turn);
                });
            }
            FigureDrag::Pose {
                figure,
                effector,
                plane,
                from,
                only_joint,
                ref pins,
                moved,
            } => {
                if !moved && (pos - from).length() * zoom < DRAG_START {
                    return;
                }
                if let Some(FigureDrag::Pose { moved, .. }) = &mut self.workspace.figure.tool.drag {
                    *moved = true;
                }
                let Some(view) = self.figure_view() else {
                    return;
                };
                let (origin, dir) = view.ray(pos);
                let Some(target) = plane.hit(origin, dir) else {
                    return;
                };
                let Some((pose, position)) =
                    self.figure_solve(figure, effector, target, only_joint, pins)
                else {
                    return;
                };
                let label = if position.is_some() {
                    "Move figure"
                } else {
                    "Pose"
                };
                self.figure_edit(label, |f| {
                    if let Some(instance) = f.scene.get_mut(figure) {
                        instance.pose = pose;
                        if let Some(p) = position {
                            instance.position = p;
                        }
                    }
                });
            }
            FigureDrag::Move {
                figure,
                plane,
                grab,
                start,
            } => {
                let Some(view) = self.figure_view() else {
                    return;
                };
                let (origin, dir) = view.ray(pos);
                let Some(at) = plane.hit(origin, dir) else {
                    return;
                };
                let moved = Vec3::from(start) + (at - grab);
                self.figure_edit("Move figure", |f| {
                    if let Some(instance) = f.scene.get_mut(figure) {
                        instance.position = moved.into();
                    }
                });
            }
            FigureDrag::Turn {
                figure,
                center,
                from,
                start,
            } => {
                let Some(view) = self.figure_view() else {
                    return;
                };
                let (origin, dir) = view.ray(pos);
                let ground = Plane {
                    point: center,
                    normal: Vec3::Y,
                };
                let Some(at) = ground.hit(origin, dir) else {
                    return;
                };
                let angle = (at.x - center.x).atan2(at.z - center.z) - from;
                let turned = (Quat::from_rotation_y(angle) * start).normalize();
                self.figure_edit("Turn figure", |f| {
                    if let Some(instance) = f.scene.get_mut(figure) {
                        instance.rotation = turned.to_array();
                    }
                });
            }
        }
    }

    /// Figure `f` posed so `effector` reaches `target`: its new turns, and
    /// its new place when the whole figure moved (dragging its hips). The
    /// pinned joints in `pins` are brought back to their places.
    fn figure_solve(
        &self,
        f: usize,
        effector: Effector,
        target: Vec3,
        only_joint: bool,
        pins: &[(usize, Vec3)],
    ) -> Option<Solved> {
        let instance = self.active_figure()?.scene.get(f)?;
        let model = instance.model.resolve().ok()?;
        let (pinned, unlimited) = instance.joint_sets();
        let scales = instance.scales(&model);
        let mut turns = instance.turns(&model);
        let rig = instance.rig(&model, &scales, &unlimited, &pinned);
        let mut position = None;
        if only_joint {
            rig.fabrik(&mut turns, &[effector.joint], effector, target);
        } else if rig.chain_for(effector.joint) == Chain::Root {
            // The whole figure follows; pinned joints stay where they are.
            let world = rig.world(&turns);
            let shift = target - effector.position(&world);
            let mut moved = instance.clone();
            moved.position = (Vec3::from(instance.position) + shift).into();
            let rig = moved.rig(&model, &scales, &unlimited, &pinned);
            rig.restore_pins(&mut turns, pins);
            position = Some(moved.position);
        } else {
            rig.reach(&mut turns, effector, target);
            rig.restore_pins(&mut turns, pins);
        }
        Some((turns.iter().map(|q| q.to_array()).collect(), position))
    }

    /// The button came up: the drag (and its undo step) ends. A click on
    /// empty space (an orbit that never moved) deselects.
    pub(crate) fn figure_release(&mut self) {
        let tool = &mut self.workspace.figure.tool;
        if let Some(FigureDrag::Orbit { moved: false, .. }) = tool.drag {
            tool.selected = None;
            tool.figure = None;
        }
        tool.drag = None;
        self.figure_edit_done();
    }

    /// A drag taken over by something else (a second finger landed): it
    /// ends where it is, selecting nothing new.
    pub(crate) fn figure_cancel_drag(&mut self) {
        self.workspace.figure.tool.drag = None;
        self.figure_edit_done();
    }

    /// Whether the Figure tool works on the selected layer (it then takes
    /// the middle button, the wheel and touch gestures).
    pub(crate) fn figure_tool_active(&self) -> bool {
        matches!(self.active_tool, crate::app::tools::Tool::Figure)
            && self.active_figure().is_some()
    }

    /// The middle button, with the Figure tool over a figure layer:
    /// orbits (instead of turning the view). False if it isn't the tool's.
    pub(crate) fn figure_middle(&mut self, pos: Vec2, pressed: bool) -> bool {
        if !self.figure_tool_active() {
            return false;
        }
        if pressed {
            self.figure_edit_done();
            self.workspace.figure.tool.drag = Some(FigureDrag::Orbit {
                last: pos,
                moved: true,
            });
        } else {
            self.workspace.figure.tool.drag = None;
            self.figure_edit_done();
        }
        true
    }

    /// Whether a drag is going on (the pointer's moves are the tool's).
    pub(crate) fn figure_dragging(&self) -> bool {
        self.workspace.figure.tool.drag.is_some()
    }

    /// The wheel, with the Figure tool over a figure layer: the camera
    /// closer (`steps` > 0) or away. False if it isn't the tool's.
    pub(crate) fn figure_wheel(&mut self, steps: f32) -> bool {
        if !self.figure_tool_active() {
            return false;
        }
        let now = Instant::now();
        let state = &mut self.workspace.figure.tool;
        let joined = state
            .last_wheel
            .is_some_and(|t| now.duration_since(t) < WHEEL_GESTURE);
        state.last_wheel = Some(now);
        if !joined {
            self.figure_edit_done();
        }
        let factor = (1.0 - steps * 0.1).clamp(0.5, 2.0);
        self.figure_edit("Zoom camera", |f| f.camera.dolly(factor));
        true
    }

    /// Two fingers on a figure layer with the Figure tool: moving them
    /// together (`pan`, screen points) orbits the camera, pinching
    /// (`zoom`, the spread's ratio) brings it closer or away, twisting
    /// (`twist`, radians clockwise) rolls it. One undo step a gesture (see
    /// [`Self::figure_touch_end`]).
    pub(crate) fn figure_touch(&mut self, pan: Vec2, zoom: f32, twist: f32) {
        self.figure_edit("Orbit camera", |f| {
            f.camera.orbit(-pan.x * ORBIT_SPEED, pan.y * ORBIT_SPEED);
            if zoom > 0.0 && (zoom - 1.0).abs() > f32::EPSILON {
                f.camera.dolly(1.0 / zoom);
            }
            f.camera.turn(twist);
        });
    }

    /// The fingers lifted: the gesture's undo step is complete.
    pub(crate) fn figure_touch_end(&mut self) {
        self.figure_edit_done();
    }

    /// The selected layer's figures back in their standing pose.
    pub(crate) fn figure_reset_pose(&mut self) {
        self.figure_step("Reset pose", |f| {
            for instance in &mut f.scene {
                instance.pose = if instance.model == figure::ModelRef::mannequin() {
                    figure::mannequin::standing_pose()
                } else {
                    Vec::new()
                };
            }
        });
    }

    /// The selected joint back at rest.
    pub(crate) fn figure_reset_joint(&mut self) {
        let Some((figure, joint)) = self.workspace.figure.tool.selected else {
            return;
        };
        self.figure_step("Reset joint", |f| {
            if let Some(instance) = f.scene.get_mut(figure) {
                instance.set_joint_pose(joint, Quat::IDENTITY);
            }
        });
    }

    /// The camera back where a new figure layer has it.
    pub(crate) fn figure_reset_camera(&mut self) {
        self.figure_step("Reset camera", |f| f.camera = Default::default());
    }

    /// The selected joint's name, for the options bar.
    pub(crate) fn figure_selected_name(&self) -> Option<String> {
        let (figure, joint) = self.workspace.figure.tool.selected?;
        let model = self
            .active_figure()?
            .scene
            .get(figure)?
            .model
            .resolve()
            .ok()?;
        let j = model.skeleton.joints.get(joint)?;
        Some(j.human.map_or_else(|| j.name.clone(), |h| h.name()))
    }
}

/// A figure's new turns, and its new place if the whole figure moved.
type Solved = (Vec<[f32; 4]>, Option<[f32; 3]>);

/// The joint each vertex of `model` follows most.
fn dominant_joints(model: &FigureModel) -> Vec<usize> {
    let mesh = &model.mesh;
    mesh.joints
        .iter()
        .zip(&mesh.weights)
        .map(|(j, w)| {
            let k = (0..4).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap_or(0);
            j[k] as usize
        })
        .collect()
}

/// Joint `joint`'s part of figure `f`, lit up in `color`.
fn draw_part(
    app: &PainterApp,
    view: &SceneView,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> Pos2,
    (f, joint): (usize, usize),
    color: Color32,
) {
    let Some(instance) = app.active_figure().and_then(|l| l.scene.get(f)) else {
        return;
    };
    let Some((model, _)) = view.figures.get(f) else {
        return;
    };
    let owner = dominant_joints(model);
    let points = instance.skinned_positions(model);
    let mut mesh = egui::Mesh::default();
    let mut placed: std::collections::HashMap<u32, u32> = Default::default();
    for tri in model.mesh.indices.as_chunks::<3>().0 {
        if mesh.indices.len() / 3 >= MAX_HIGHLIGHT {
            break;
        }
        if !tri.iter().all(|&v| owner.get(v as usize) == Some(&joint)) {
            continue;
        }
        let mut corners = [0u32; 3];
        let mut ok = true;
        for (c, &v) in corners.iter_mut().zip(tri) {
            if let Some(&i) = placed.get(&v) {
                *c = i;
                continue;
            }
            let Some((px, _)) = points.get(v as usize).and_then(|p| view.project(*p)) else {
                ok = false;
                break;
            };
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(to_screen(px), color);
            placed.insert(v, i);
            *c = i;
        }
        if ok {
            mesh.add_triangle(corners[0], corners[1], corners[2]);
        }
    }
    if !mesh.indices.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// The figures' joints (pins marked), the part under the pointer lit up,
/// the rotation gizmo round the selected joint and the ground ring round
/// the selected figure, over the canvas.
pub(crate) fn draw_figure(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> Pos2,
) {
    if !matches!(app.active_tool, crate::app::tools::Tool::Figure) {
        return;
    }
    let Some(view) = app.figure_view() else {
        return;
    };
    let Some(layer) = app.active_figure() else {
        return;
    };
    let tool = &app.workspace.figure.tool;
    let lit = accent();
    if let Some(hover) = tool.hover.filter(|_| tool.drag.is_none()) {
        draw_part(
            app,
            &view,
            painter,
            to_screen,
            hover,
            Color32::from_rgba_unmultiplied(lit.r(), lit.g(), lit.b(), 70),
        );
    }
    let selected = tool.selected;
    let outline = Stroke::new(1.0_f32, Color32::BLACK);
    let pin = Color32::from_rgb(232, 72, 72);
    for (f, (_, world)) in view.figures.iter().enumerate() {
        let instance = &layer.scene[f];
        for (j, m) in world.iter().enumerate() {
            let Some((px, _)) = view.project(m.w_axis.truncate()) else {
                continue;
            };
            let at = to_screen(px);
            if instance.is_pinned(j) {
                // A pin: a red diamond.
                let r = 6.0;
                let points = vec![
                    at + egui::vec2(0.0, -r),
                    at + egui::vec2(r, 0.0),
                    at + egui::vec2(0.0, r),
                    at + egui::vec2(-r, 0.0),
                ];
                painter.add(egui::Shape::convex_polygon(points, pin, outline));
                continue;
            }
            let (r, fill) = if selected == Some((f, j)) {
                (5.0, lit)
            } else {
                (3.0, Color32::from_white_alpha(220))
            };
            // Joints with their limits off: ringed in orange.
            let ring = if instance.is_unlimited(j) {
                Stroke::new(1.5_f32, Color32::from_rgb(240, 150, 40))
            } else {
                outline
            };
            painter.circle(at, r, fill, ring);
        }
    }
    if let Some(root) = app.figure_root_gizmo() {
        let stroke = Stroke::new(2.0_f32, lit.gamma_multiply(0.8));
        for k in 1..root.points.len() {
            painter.line_segment(
                [to_screen(root.points[k - 1]), to_screen(root.points[k])],
                stroke,
            );
        }
        let c = to_screen(root.center_px);
        painter.rect(
            egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)),
            1.0,
            lit,
            outline,
        );
    }
    let Some(gizmo) = app.figure_gizmo() else {
        return;
    };
    let colors = [
        Color32::from_rgb(232, 72, 72),
        Color32::from_rgb(88, 200, 88),
        Color32::from_rgb(80, 130, 245),
    ];
    for (ring, color) in gizmo.rings.iter().zip(colors) {
        for k in 1..ring.points.len() {
            let (a, b) = (ring.points[k - 1], ring.points[k]);
            let stroke = if a.2 && b.2 {
                Stroke::new(2.5_f32, color)
            } else {
                Stroke::new(1.0_f32, color.gamma_multiply(0.45))
            };
            painter.line_segment([to_screen(a.1), to_screen(b.1)], stroke);
        }
    }
    painter.circle(to_screen(gizmo.center_px), 5.0, lit, outline);
}

#[cfg(test)]
mod tests {
    use super::FigureMods;
    use crate::app::tools::Tool;
    use crate::canvas::Canvas;
    use crate::canvas::figure::skeleton::TurnAngle;
    use crate::canvas::figure::skeleton::{HumanJoint, Side};
    use eframe::egui::{Color32, Vec2};
    use glam::{Quat, Vec3};

    const NONE: FigureMods = FigureMods {
        shift: false,
        alt: false,
    };

    fn app_with_figure() -> crate::PainterApp {
        let canvas = Canvas::new(300, 400, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.add_figure_layer().unwrap();
        app
    }

    /// Joint `h`'s dot on the canvas.
    fn dot(app: &crate::PainterApp, h: HumanJoint) -> (usize, Vec2) {
        let view = app.figure_view().unwrap();
        let (model, world) = &view.figures[0];
        let j = model.skeleton.find(h).unwrap();
        (j, view.project(world[j].w_axis.truncate()).unwrap().0)
    }

    /// Where joint `h` is in the scene.
    fn place(app: &crate::PainterApp, h: HumanJoint) -> Vec3 {
        let view = app.figure_view().unwrap();
        let (model, world) = &view.figures[0];
        world[model.skeleton.find(h).unwrap()].w_axis.truncate()
    }

    /// Two presses on one spot, as a double-click (the second at once).
    fn double_click(app: &mut crate::PainterApp, at: Vec2) {
        for _ in 0..2 {
            app.figure_press(at, NONE);
            app.figure_release();
        }
    }

    fn last_label(app: &crate::PainterApp) -> Option<String> {
        app.layer_state.history.labels().0.last().cloned()
    }

    #[test]
    fn clicking_a_joint_selects_it() {
        let mut app = app_with_figure();
        assert!(matches!(app.active_tool, Tool::Figure));
        let (j, at) = dot(&app, HumanJoint::Head);
        app.figure_press(at + Vec2::new(2.0, -1.0), NONE);
        app.figure_release();
        assert_eq!(app.workspace.figure.tool.selected, Some((0, j)));
        assert_eq!(app.figure_selected_name().as_deref(), Some("head"));
        assert_eq!(app.figure_selected_instance(), Some(0));
        // A click on empty canvas deselects it.
        app.figure_press(Vec2::new(5.0, 5.0), NONE);
        app.figure_release();
        assert_eq!(app.workspace.figure.tool.selected, None);
        assert_eq!(app.figure_selected_instance(), None);
    }

    #[test]
    fn the_part_under_the_pointer_is_picked() {
        let mut app = app_with_figure();
        // Between the knee and the ankle: the shin.
        let (_, knee) = dot(&app, HumanJoint::LowerLeg(Side::Left));
        let (_, ankle) = dot(&app, HumanJoint::Foot(Side::Left));
        let model = crate::canvas::figure::mannequin::mannequin();
        let shin = model
            .skeleton
            .find(HumanJoint::LowerLeg(Side::Left))
            .unwrap();
        assert!(app.figure_hover((knee + ankle) / 2.0));
        assert_eq!(app.workspace.figure.tool.hover, Some((0, shin)));
        // The chest; then nothing.
        let (_, chest) = dot(&app, HumanJoint::UpperChest);
        app.figure_hover(chest + Vec2::new(0.0, 12.0));
        let chest_joint = model.skeleton.find(HumanJoint::Chest).unwrap();
        let upper = model.skeleton.find(HumanJoint::UpperChest).unwrap();
        let hovered = app.workspace.figure.tool.hover.unwrap().1;
        assert!(hovered == chest_joint || hovered == upper, "{hovered}");
        app.figure_hover(Vec2::new(4.0, 4.0));
        assert_eq!(app.workspace.figure.tool.hover, None);
        // The ray behind the picture: the right pixel's ray meets the
        // scene point it shows.
        let layer = app.active_figure().unwrap();
        let p = Vec3::new(0.1, 1.2, 0.05);
        let px = crate::canvas::figure::project(&layer.view_proj(300, 400), p, 300, 400)
            .unwrap()
            .0;
        let (o, d) = layer.ray(px, 300, 400);
        assert!((p - o).cross(d).length() < 1e-3);
    }

    #[test]
    fn dragging_a_hand_moves_the_arm_by_ik_in_one_undo_step() {
        let mut app = app_with_figure();
        let hand = place(&app, HumanJoint::Hand(Side::Left));
        let elbow = place(&app, HumanJoint::LowerArm(Side::Left));
        let shoulder = place(&app, HumanJoint::UpperArm(Side::Left));
        let (_, at) = dot(&app, HumanJoint::Hand(Side::Left));
        let steps = app.layer_state.history.labels().0.len();
        app.figure_press(at, NONE);
        for k in 1..=10 {
            app.figure_drag(at + Vec2::new(-1.5 * k as f32, -4.0 * k as f32));
        }
        app.figure_release();
        let moved = place(&app, HumanJoint::Hand(Side::Left));
        assert!(
            moved.y > hand.y + 0.05,
            "the hand went up: {hand} → {moved}"
        );
        assert!(
            place(&app, HumanJoint::LowerArm(Side::Left)).distance(elbow) > 0.01,
            "the elbow followed"
        );
        assert!(place(&app, HumanJoint::UpperArm(Side::Left)).distance(shoulder) < 1e-4);
        assert_eq!(app.layer_state.history.labels().0.len(), steps + 1);
        assert_eq!(last_label(&app).as_deref(), Some("Pose"));
        app.apply_history(false);
        assert!(place(&app, HumanJoint::Hand(Side::Left)).distance(hand) < 1e-5);
    }

    #[test]
    fn a_double_click_pins_a_joint_and_pins_hold_while_posing() {
        let mut app = app_with_figure();
        let (foot, at) = dot(&app, HumanJoint::Foot(Side::Right));
        double_click(&mut app, at);
        assert!(app.active_figure().unwrap().scene[0].is_pinned(foot));
        assert_eq!(last_label(&app).as_deref(), Some("Pin joint"));
        let (other, at_left) = dot(&app, HumanJoint::Foot(Side::Left));
        double_click(&mut app, at_left);
        assert!(app.active_figure().unwrap().scene[0].is_pinned(other));
        let right = place(&app, HumanJoint::Foot(Side::Right));
        let hips = place(&app, HumanJoint::Hips);
        // Dragging the hips down moves the figure; the pinned feet stay
        // and the knees bend.
        let (_, at) = dot(&app, HumanJoint::Hips);
        app.figure_press(at, NONE);
        for k in 1..=6 {
            app.figure_drag(at + Vec2::new(0.0, 3.0 * k as f32));
        }
        app.figure_release();
        assert!(place(&app, HumanJoint::Hips).y < hips.y - 0.02);
        assert!(place(&app, HumanJoint::Foot(Side::Right)).distance(right) < 3e-3);
        assert_eq!(last_label(&app).as_deref(), Some("Move figure"));
        // Double-clicked again: unpinned.
        let (_, at) = dot(&app, HumanJoint::Foot(Side::Right));
        double_click(&mut app, at);
        assert!(!app.active_figure().unwrap().scene[0].is_pinned(foot));
    }

    #[test]
    fn alt_drag_moves_the_figure_and_shift_drag_turns_one_joint() {
        let mut app = app_with_figure();
        let (_, chest) = dot(&app, HumanJoint::UpperChest);
        let alt = FigureMods {
            alt: true,
            shift: false,
        };
        app.figure_press(chest, alt);
        app.figure_drag(chest + Vec2::new(30.0, 0.0));
        app.figure_release();
        let figure = &app.active_figure().unwrap().scene[0];
        assert!(figure.position[0].abs() > 0.05, "{:?}", figure.position);
        // Shift: only the forearm turns (the shoulder stays as it was).
        let model = crate::canvas::figure::mannequin::mannequin();
        let upper = model
            .skeleton
            .find(HumanJoint::UpperArm(Side::Left))
            .unwrap();
        let lower = model
            .skeleton
            .find(HumanJoint::LowerArm(Side::Left))
            .unwrap();
        let before = app.active_figure().unwrap().scene[0].clone();
        let (_, elbow) = dot(&app, HumanJoint::LowerArm(Side::Left));
        let (_, wrist) = dot(&app, HumanJoint::Hand(Side::Left));
        let forearm = elbow + (wrist - elbow) * 0.6;
        let shift = FigureMods {
            shift: true,
            alt: false,
        };
        app.figure_press(forearm, shift);
        app.figure_drag(forearm + Vec2::new(0.0, -20.0));
        app.figure_release();
        let after = &app.active_figure().unwrap().scene[0];
        assert_eq!(after.joint_pose(upper), before.joint_pose(upper));
        assert!(after.joint_pose(lower).turn_angle(before.joint_pose(lower)) > 0.05);
    }

    /// A ring point a quarter of the way round (at the figure's side).
    const RING_QUARTER: usize = super::RING_STEPS / 4;

    #[test]
    fn the_ground_ring_turns_and_moves_the_figure() {
        let mut app = app_with_figure();
        app.workspace.figure.tool.figure = Some(0);
        let root = app.figure_root_gizmo().unwrap();
        // The middle: across the ground.
        app.figure_press(root.center_px, NONE);
        app.figure_drag(root.center_px + Vec2::new(25.0, 0.0));
        app.figure_release();
        let figure = &app.active_figure().unwrap().scene[0];
        assert!(figure.position[0].abs() > 0.05 && figure.position[1].abs() < 1e-5);
        // The ring: about the vertical.
        let root = app.figure_root_gizmo().unwrap();
        let on = root.points[RING_QUARTER];
        let next = root.points[RING_QUARTER + 2];
        app.figure_press(on, NONE);
        app.figure_drag(on + (next - on) * 3.0);
        app.figure_release();
        let turn = Quat::from_array(app.active_figure().unwrap().scene[0].rotation);
        assert!(turn.turn_angle(Quat::IDENTITY) > 0.05);
        let (axis, _) = turn.to_axis_angle();
        assert!(axis.y.abs() > 0.999, "{axis}");
        assert_eq!(last_label(&app).as_deref(), Some("Turn figure"));
    }

    #[test]
    fn dragging_a_ring_turns_the_joint_in_one_undo_step() {
        let mut app = app_with_figure();
        let (j, at) = dot(&app, HumanJoint::Head);
        app.figure_press(at, NONE);
        app.figure_release();
        let gizmo = app.figure_gizmo().expect("a gizmo round the head");
        // Press on a ring point that faces the camera, drag along the ring.
        let ring = &gizmo.rings[1];
        let k = (1..ring.points.len())
            .find(|&k| ring.points[k - 1].2 && ring.points[k].2)
            .unwrap();
        let p = ring.points[k - 1].1;
        let along = (ring.points[k].1 - p).normalized();
        let before = app.active_figure().unwrap().scene[0].joint_pose(j);
        app.figure_press(p, NONE);
        for step in 1..=5 {
            app.figure_drag(p + along * (6.0 * step as f32));
        }
        app.figure_release();
        let after = app.active_figure().unwrap().scene[0].joint_pose(j);
        assert!(after.turn_angle(before) > 0.05, "the head turned");
        // The turn is about the joint's own Y axis.
        let delta = before.conjugate() * after;
        let (axis, _) = delta.to_axis_angle();
        assert!(axis.y.abs() > 0.99, "{axis}");
        app.apply_history(false);
        let undone = app.active_figure().unwrap().scene[0].joint_pose(j);
        assert!(undone.abs_diff_eq(before, 1e-6));
    }

    #[test]
    fn dragging_empty_space_orbits_and_the_wheel_dollies() {
        let mut app = app_with_figure();
        let camera = app.active_figure().unwrap().camera;
        app.figure_press(Vec2::new(10.0, 200.0), NONE);
        app.figure_drag(Vec2::new(60.0, 200.0));
        app.figure_release();
        let orbited = app.active_figure().unwrap().camera;
        assert!(orbited.yaw != camera.yaw);
        assert_eq!(orbited.distance, camera.distance);
        assert!(app.figure_wheel(1.0));
        assert!(app.figure_wheel(1.0));
        let dollied = app.active_figure().unwrap().camera;
        assert!(dollied.distance < camera.distance);
        let labels = app.layer_state.history.labels().0;
        assert_eq!(
            labels[labels.len() - 2..],
            ["Orbit camera".to_string(), "Zoom camera".to_string()]
        );
        // Both wheel steps, one undo step; then the orbit's.
        app.apply_history(false);
        assert_eq!(
            app.active_figure().unwrap().camera.distance,
            camera.distance
        );
        app.apply_history(false);
        assert_eq!(app.active_figure().unwrap().camera, camera);
        // Reset pose puts a turned joint back.
        let (j, _) = dot(&app, HumanJoint::Head);
        app.figure_edit("Pose", |f| {
            f.scene[0].set_joint_pose(j, Quat::from_rotation_y(1.0))
        });
        app.figure_edit_done();
        app.figure_reset_pose();
        assert!(
            app.active_figure().unwrap().scene[0]
                .joint_pose(j)
                .abs_diff_eq(Quat::IDENTITY, 1e-6)
        );
    }
}
