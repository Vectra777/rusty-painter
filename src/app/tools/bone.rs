//! The Bone tool, on a rig layer: draw its bones on the canvas (setup) and
//! pose them (keyed at the frame showing when the rig has an animation).
//!
//! Setup: drag from empty canvas to draw a bone (a child of the selected
//! one; Shift+drag from a bone's tail draws its child from there, Shift
//! anywhere always draws), click a bone to select it, drag its head to move it (its children
//! go along) or its tail to aim and lengthen it, Delete to remove it.
//! Pose: drag a bone to turn it round its head, Shift+drag (or drag the
//! root) to move it. Each drag is one undo step. Bind attaches the rig's
//! mesh parts to the bones (smoothly between them, or each to one).

use crate::PainterApp;
use crate::app::rig::{bone_now, set_bone};
use crate::canvas::rig::{Affine, Rig};
use crate::ui::style::accent;
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};

/// How the Bone tool works: drawing the setup, or posing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoneSettings {
    pub pose: bool,
}

#[derive(Clone, Copy, Debug)]
enum BoneDrag {
    /// Drawing a new bone from `head` (rig frame); made once the pointer
    /// has moved.
    New {
        parent: Option<usize>,
        head: [f32; 2],
        bone: Option<usize>,
    },
    /// Setup: moving bone `b` by its head (pointer at `from`, rig frame).
    Head {
        b: usize,
        from: [f32; 2],
        head: [f32; 2],
        tail: [f32; 2],
    },
    /// Setup: aiming bone `b` by its tail.
    Tail { b: usize, head: [f32; 2] },
    /// Pose: turning bone `b` from the pointer's angle round its head.
    Turn {
        b: usize,
        pivot: [f32; 2],
        from: f32,
        rotation: f32,
        at: [f32; 2],
        mirrored: bool,
    },
    /// Pose: moving bone `b` (its parent's frame turned back to the rig's).
    Move {
        b: usize,
        from: [f32; 2],
        at: [f32; 2],
        rotation: f32,
        back: Affine,
    },
}

#[derive(Default)]
pub struct BoneState {
    pub settings: BoneSettings,
    pub selected: Option<usize>,
    drag: Option<BoneDrag>,
}

/// Bone `i`'s head and tail in the rig's frame, placed by `world`.
fn ends(rig: &Rig, world: &[Affine], i: usize) -> ([f32; 2], [f32; 2]) {
    let w = world[i];
    (w.t, w.apply([rig.bones[i].length, 0.0]))
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 1e-9 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, [a[0] + t * dx, a[1] + t * dy])
}

/// What part of a bone is under a point: its head, its tail, or it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Head(usize),
    Tail(usize),
    Body(usize),
}

impl PainterApp {
    /// The seconds the rig is posed at.
    fn bone_seconds(&self) -> f32 {
        self.canvas.time as f32 / self.canvas.timeline.fps.max(1) as f32
    }

    /// The bones' frames as the tool shows them: the setup pose, or posed
    /// at the frame showing.
    fn bone_world(&self, rig: &Rig) -> Vec<Affine> {
        if self.workspace.bone.settings.pose {
            rig.pose(self.bone_seconds()).world
        } else {
            rig.setup_world()
        }
    }

    /// The bone part under canvas point `p` (the selected bone's ends
    /// first, then the nearest).
    fn bone_hit(&self, rig: &Rig, p: [f32; 2]) -> Option<Hit> {
        let world = self.bone_world(rig);
        let to_canvas = rig.to_canvas();
        let r = 8.0 / self.viewport.zoom.max(1e-3);
        let canvas_ends = |i: usize| {
            let (h, t) = ends(rig, &world, i);
            (to_canvas.apply(h), to_canvas.apply(t))
        };
        let mut order: Vec<usize> = (0..rig.bones.len()).rev().collect();
        if let Some(s) = self
            .workspace
            .bone
            .selected
            .filter(|&s| s < rig.bones.len())
        {
            order.retain(|&i| i != s);
            order.insert(0, s);
        }
        for &i in &order {
            let (h, t) = canvas_ends(i);
            if rig.bones[i].length > 0.0 && dist(p, t) <= r {
                return Some(Hit::Tail(i));
            }
            if dist(p, h) <= r {
                return Some(Hit::Head(i));
            }
        }
        order
            .iter()
            .map(|&i| {
                let (h, t) = canvas_ends(i);
                (i, to_segment(p, h, t))
            })
            .filter(|&(_, d)| d <= r * 0.75)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| Hit::Body(i))
    }

    pub(crate) fn bone_press(&mut self, pos: Vec2, shift: bool) {
        let Some(rig) = self.active_rig() else {
            self.report(
                "The Bone tool works on a rig layer (Layer → Rig → Make Rig from Layers)".into(),
            );
            return;
        };
        let p = [pos.x, pos.y];
        let Some(back) = rig.to_canvas().inverse() else {
            return;
        };
        let at = back.apply(p);
        let hit = self.bone_hit(rig, p);
        let world = self.bone_world(rig);
        let seconds = self.bone_seconds();
        let pose = self.workspace.bone.settings.pose;
        let drag = match hit {
            // Shift: a new bone, from where it's pressed (a bone's tail: a
            // child of that bone from there).
            hit if shift && !pose => {
                let (parent, head) = match hit {
                    Some(Hit::Tail(b)) => (Some(b), ends(rig, &world, b).1),
                    Some(Hit::Head(b) | Hit::Body(b)) => (Some(b), at),
                    None => (
                        self.workspace
                            .bone
                            .selected
                            .filter(|&s| s < rig.bones.len())
                            .or((!rig.bones.is_empty()).then_some(0)),
                        at,
                    ),
                };
                Some(BoneDrag::New {
                    parent,
                    head,
                    bone: None,
                })
            }
            None if pose => {
                self.workspace.bone.selected = None;
                None
            }
            None => Some(BoneDrag::New {
                parent: self
                    .workspace
                    .bone
                    .selected
                    .filter(|&s| s < rig.bones.len())
                    .or((!rig.bones.is_empty()).then_some(0)),
                head: at,
                bone: None,
            }),
            Some(Hit::Head(b) | Hit::Tail(b) | Hit::Body(b)) if pose => {
                let (rotation, x, y) = bone_now(rig, b, seconds);
                let parent = rig.bones[b].parent;
                if shift || parent.is_none() {
                    let lin = parent.map_or(Affine::IDENTITY, |p| Affine {
                        t: [0.0, 0.0],
                        ..world[p]
                    });
                    Some(BoneDrag::Move {
                        b,
                        from: at,
                        at: [x, y],
                        rotation,
                        back: lin.inverse().unwrap_or(Affine::IDENTITY),
                    })
                } else {
                    let pivot = world[b].t;
                    let mirrored = parent.is_some_and(|p| {
                        let m = world[p].m;
                        m[0] * m[3] - m[1] * m[2] < 0.0
                    });
                    Some(BoneDrag::Turn {
                        b,
                        pivot,
                        from: (at[1] - pivot[1]).atan2(at[0] - pivot[0]),
                        rotation,
                        at: [x, y],
                        mirrored,
                    })
                }
            }
            Some(Hit::Tail(b)) => Some(BoneDrag::Tail {
                b,
                head: ends(rig, &world, b).0,
            }),
            Some(Hit::Head(b) | Hit::Body(b)) => {
                let (head, tail) = ends(rig, &world, b);
                Some(BoneDrag::Head {
                    b,
                    from: at,
                    head,
                    tail,
                })
            }
        };
        if let Some(Hit::Head(b) | Hit::Tail(b) | Hit::Body(b)) = hit
            && !matches!(drag, Some(BoneDrag::New { .. }))
        {
            self.workspace.bone.selected = Some(b);
        }
        self.workspace.bone.drag = drag;
    }

    pub(crate) fn bone_drag(&mut self, pos: Vec2) {
        let Some(drag) = self.workspace.bone.drag else {
            return;
        };
        let Some(rig) = self.active_rig() else {
            return;
        };
        let Some(back) = rig.to_canvas().inverse() else {
            return;
        };
        let at = back.apply([pos.x, pos.y]);
        let seconds = self.bone_seconds();
        let min = 3.0 / (self.viewport.zoom.max(1e-3) * rig.scale.max(1e-6));
        match drag {
            BoneDrag::New { parent, head, bone } => {
                if dist(head, at) < min {
                    return;
                }
                match bone {
                    None => {
                        let name = rig.new_bone_name();
                        let mut made = None;
                        self.rig_edit(|rig| made = Some(rig.add_bone(name, parent, head, at)));
                        self.workspace.bone.selected = made;
                        self.workspace.bone.drag = Some(BoneDrag::New {
                            parent,
                            head,
                            bone: made,
                        });
                    }
                    Some(b) => self.rig_edit(|rig| rig.place_bone(b, head, at)),
                }
            }
            BoneDrag::Head {
                b,
                from,
                head,
                tail,
            } => {
                let d = [at[0] - from[0], at[1] - from[1]];
                self.rig_edit(|rig| {
                    rig.place_bone(
                        b,
                        [head[0] + d[0], head[1] + d[1]],
                        [tail[0] + d[0], tail[1] + d[1]],
                    )
                });
            }
            BoneDrag::Tail { b, head } => self.rig_edit(|rig| rig.place_bone(b, head, at)),
            BoneDrag::Turn {
                b,
                pivot,
                from,
                rotation,
                at: place,
                mirrored,
            } => {
                let now = (at[1] - pivot[1]).atan2(at[0] - pivot[0]);
                let mut delta = (now - from).to_degrees();
                delta = (delta + 180.0).rem_euclid(360.0) - 180.0;
                if mirrored {
                    delta = -delta;
                }
                self.rig_edit(|rig| set_bone(rig, b, seconds, rotation + delta, place));
            }
            BoneDrag::Move {
                b,
                from,
                at: place,
                rotation,
                back,
            } => {
                let d = back.apply([at[0] - from[0], at[1] - from[1]]);
                self.rig_edit(|rig| {
                    set_bone(
                        rig,
                        b,
                        seconds,
                        rotation,
                        [place[0] + d[0], place[1] + d[1]],
                    )
                });
            }
        }
    }

    pub(crate) fn bone_release(&mut self) {
        self.workspace.bone.drag = None;
        self.rig_edit_done();
    }

    /// Take the selected bone out (Delete). False if there's none to take.
    pub(crate) fn bone_remove_selected(&mut self) -> bool {
        let Some(b) = self.workspace.bone.selected else {
            return false;
        };
        let Some(rig) = self.active_rig() else {
            return false;
        };
        if rig.bones.get(b).is_none_or(|x| x.parent.is_none()) {
            self.report("The root bone stays".into());
            return true;
        }
        self.rig_edit(|rig| {
            rig.remove_bone(b);
        });
        self.rig_edit_done();
        self.workspace.bone.selected = None;
        true
    }
}

/// The rig's bones over the canvas, the selected one highlighted.
pub(crate) fn draw_bones(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> Pos2,
) {
    if !matches!(app.active_tool, crate::app::tools::Tool::Bone) {
        return;
    }
    let Some(rig) = app.active_rig() else {
        return;
    };
    let world = app.bone_world(rig);
    let to_canvas = rig.to_canvas();
    let at = |p: [f32; 2]| {
        let c = to_canvas.apply(p);
        to_screen(app.to_canvas_space(Vec2::new(c[0], c[1])))
    };
    let outline = Stroke::new(1.5_f32, Color32::BLACK);
    for i in 0..rig.bones.len() {
        let (h, t) = ends(rig, &world, i);
        let (h, t) = (at(h), at(t));
        let selected = app.workspace.bone.selected == Some(i);
        let fill = if selected {
            accent()
        } else {
            Color32::from_white_alpha(200)
        };
        let along = t - h;
        let len = along.length();
        if len > 4.0 {
            // A wedge: wide near the head, to a point at the tail.
            let dir = along / len;
            let side = Vec2::new(-dir.y, dir.x) * (len * 0.12).clamp(3.0, 10.0);
            let knee = h + along * 0.15;
            let shape = vec![h, knee + side, t, knee - side];
            painter.add(egui::Shape::convex_polygon(
                shape,
                fill.gamma_multiply(0.6),
                outline,
            ));
        }
        painter.circle(h, 4.0, fill, outline);
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tools::Tool;
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    #[test]
    fn bones_drawn_bind_the_part_and_posing_them_keys_it() {
        let canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        let tiles =
            crate::app::import::pixels_to_tiles(64, (24, 24), (80, 16), |_, _| Color32::RED);
        app.add_layer_with_tiles("bar".into(), tiles, |_| {});
        app.make_rig_from_layers();
        app.active_tool = Tool::Bone;
        // Two bones along the bar, the second a child of the first.
        let draw = |app: &mut crate::PainterApp, a: [f32; 2], b: [f32; 2]| {
            app.bone_press(Vec2::new(a[0], a[1]), true);
            app.bone_drag(Vec2::new((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0));
            app.bone_drag(Vec2::new(b[0], b[1]));
            app.bone_release();
        };
        draw(&mut app, [24.0, 32.0], [64.0, 32.0]);
        draw(&mut app, [64.0, 32.0], [104.0, 32.0]);
        let rig = app.active_rig().unwrap();
        assert_eq!(rig.bones.len(), 3);
        assert_eq!(rig.bones[2].parent, Some(1));
        let before = app.canvas.flatten_final().pixels;
        app.bind_rig(true);
        assert_eq!(
            app.canvas.flatten_final().pixels,
            before,
            "binding moves nothing"
        );
        // Pose: turn the second bone down at frame 6.
        app.workspace.bone.settings.pose = true;
        app.go_to_frame(6);
        app.bone_press(Vec2::new(90.0, 32.0), false);
        app.bone_drag(Vec2::new(90.0, 50.0));
        app.bone_release();
        let rig = app.active_rig().unwrap();
        assert_eq!(rig.bones[2].rotation, 0.0, "the setup stays");
        let red_at = |app: &crate::PainterApp, x: usize, y: usize| {
            let c = app.canvas.flatten_final().pixels[y * 128 + x];
            c.r() > 200 && c.g() < 60
        };
        assert!(!red_at(&app, 100, 32), "the tip swung away");
        assert!(red_at(&app, 30, 32), "the first half stays");
        app.go_to_frame(0);
        assert!(!red_at(&app, 100, 32), "one key holds at every frame");
        // Removing a bone is an undo step.
        app.workspace.bone.selected = Some(2);
        assert!(app.bone_remove_selected());
        assert_eq!(app.active_rig().unwrap().bones.len(), 2);
        app.apply_history(false);
        assert_eq!(app.active_rig().unwrap().bones.len(), 3);
    }

    #[test]
    fn binding_keeps_a_pinned_part_where_its_keys_put_it() {
        let canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        let tiles =
            crate::app::import::pixels_to_tiles(64, (24, 24), (80, 16), |_, _| Color32::RED);
        app.add_layer_with_tiles("bar".into(), tiles, |_| {});
        app.make_rig_from_layers();
        // A bend keyed at frame 3.
        app.active_tool = Tool::Puppet;
        app.go_to_frame(3);
        app.puppet_press(Vec2::new(28.0, 32.0), false, false);
        app.puppet_release();
        app.puppet_press(Vec2::new(100.0, 32.0), false, false);
        app.puppet_drag(Vec2::new(100.0, 20.0));
        app.puppet_release();
        let bent = app.canvas.flatten_final();
        // Bones, then bound both ways: the bend stays.
        app.active_tool = Tool::Bone;
        app.bone_press(Vec2::new(24.0, 32.0), true);
        app.bone_drag(Vec2::new(64.0, 32.0));
        app.bone_release();
        app.bone_press(Vec2::new(64.0, 32.0), true);
        app.bone_drag(Vec2::new(104.0, 32.0));
        app.bone_release();
        for smooth in [true, false] {
            app.bind_rig(smooth);
            let now = app.canvas.flatten_final();
            let worst = (bent.pixels.iter().zip(&now.pixels))
                .map(|(a, b)| {
                    (0..4)
                        .map(|k| (a.to_array()[k] as i32 - b.to_array()[k] as i32).abs())
                        .max()
                        .unwrap()
                })
                .max()
                .unwrap();
            assert!(worst <= 3, "smooth {smooth}: {worst}");
        }
    }
}
