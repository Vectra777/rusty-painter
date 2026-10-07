//! A rig posed at a time: the animation's keys read (with their curves),
//! the bones placed from the root out, IK applied, and the slots' tints,
//! attachments and mesh deforms.

use super::{Affine, Curve, Inherit, Key, Rig};

/// A bone's local values: x, y, turn, scale and shear.
type Local = (f32, f32, f32, [f32; 2], [f32; 2]);

/// A mesh attachment's point offsets: (slot, attachment index) and them.
pub type Deform = ((usize, usize), Vec<[f32; 2]>);

/// A rig at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    /// Each bone's frame in the rig's frame.
    pub world: Vec<Affine>,
    /// Each slot's tint (unmultiplied) and the attachment showing (its
    /// index in the slot's list).
    pub colors: Vec<[f32; 4]>,
    pub attachments: Vec<Option<usize>>,
    /// Offsets for mesh attachments' points: (slot, attachment index).
    pub deforms: Vec<Deform>,
}

/// Where `u` (0..1 of the way between two keys in time) is along a curve.
pub fn ease(curve: Curve, u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    match curve {
        Curve::Linear => u,
        Curve::Stepped => 0.0,
        Curve::Bezier([x1, y1, x2, y2]) => {
            // Solve x(s) = u for s, then y(s).
            let bez = |p1: f32, p2: f32, s: f32| {
                let r = 1.0 - s;
                3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s
            };
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..24 {
                let mid = (lo + hi) * 0.5;
                if bez(x1, x2, mid) < u {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            bez(y1, y2, (lo + hi) * 0.5)
        }
    }
}

/// Things keys can hold.
pub trait Lerp: Clone {
    fn lerp(&self, other: &Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(&self, other: &Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl<const N: usize> Lerp for [f32; N] {
    fn lerp(&self, other: &Self, t: f32) -> Self {
        std::array::from_fn(|i| self[i] + (other[i] - self[i]) * t)
    }
}

impl Lerp for Vec<[f32; 2]> {
    fn lerp(&self, other: &Self, t: f32) -> Self {
        (0..self.len().max(other.len()))
            .map(|i| {
                let a = self.get(i).copied().unwrap_or_default();
                let b = other.get(i).copied().unwrap_or_default();
                a.lerp(&b, t)
            })
            .collect()
    }
}

/// The value of `keys` (sorted by time) at `time`: held before the first
/// and after the last.
pub fn sample<T: Lerp>(keys: &[Key<T>], time: f32) -> Option<T> {
    let first = keys.first()?;
    if time <= first.time {
        return Some(first.value.clone());
    }
    let next = keys.iter().position(|k| k.time > time);
    let Some(n) = next else {
        return Some(keys[keys.len() - 1].value.clone());
    };
    let (a, b) = (&keys[n - 1], &keys[n]);
    let span = (b.time - a.time).max(1e-6);
    let u = ease(a.curve, (time - a.time) / span);
    Some(a.value.lerp(&b.value, u))
}

/// The length of an affine's columns (each axis's scale).
fn column_lengths(m: [f32; 4]) -> (f32, f32) {
    (
        (m[0] * m[0] + m[2] * m[2]).sqrt(),
        (m[1] * m[1] + m[3] * m[3]).sqrt(),
    )
}

/// The x axis's angle (degrees) in a frame.
fn angle_of(a: &Affine) -> f32 {
    a.m[2].atan2(a.m[0]).to_degrees()
}

impl Rig {
    /// The rig at `time` seconds into its animation (looping), or its setup
    /// pose without one.
    pub fn pose(&self, time: f32) -> Pose {
        let animation = self.animation.and_then(|i| self.animations.get(i));
        let time = match animation {
            Some(a) if a.duration > 0.0 => time.rem_euclid(a.duration),
            _ => time,
        };
        // The bones' local values: setup, moved by the animation.
        let mut local: Vec<Local> = (self.bones.iter())
            .map(|b| (b.x, b.y, b.rotation, b.scale, b.shear))
            .collect();
        if let Some(a) = animation {
            for track in &a.bones {
                let Some(l) = local.get_mut(track.bone) else {
                    continue;
                };
                if let Some(r) = sample(&track.rotate, time) {
                    l.2 += r;
                }
                if let Some([x, y]) = sample(&track.translate, time) {
                    l.0 += x;
                    l.1 += y;
                }
                if let Some([sx, sy]) = sample(&track.scale, time) {
                    l.3 = [l.3[0] * sx, l.3[1] * sy];
                }
                if let Some([hx, hy]) = sample(&track.shear, time) {
                    l.4 = [l.4[0] + hx, l.4[1] + hy];
                }
            }
        }
        let mut world = self.world(&local);
        // IK: turn the bones to reach their targets, then place them again.
        for ik in &self.ik {
            if ik.mix <= 0.0 || ik.bones.is_empty() || ik.target >= self.bones.len() {
                continue;
            }
            let target = world[ik.target].t;
            match ik.bones[..] {
                [one] => {
                    local[one].2 += self.aim(&world, one, target) * ik.mix;
                    world = self.world(&local);
                }
                [parent, child, ..] => {
                    let p = world[parent].t;
                    let j = world[child].t;
                    let l1 = ((j[0] - p[0]).powi(2) + (j[1] - p[1]).powi(2)).sqrt();
                    let child_scale = column_lengths(world[child].m).0;
                    let l2 = self.bones[child].length * child_scale;
                    let dx = target[0] - p[0];
                    let dy = target[1] - p[1];
                    let d = (dx * dx + dy * dy)
                        .sqrt()
                        .clamp((l1 - l2).abs() + 1e-4, l1 + l2 - 1e-4);
                    let cos =
                        ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d).max(1e-6)).clamp(-1.0, 1.0);
                    let bend = if ik.bend_positive { 1.0 } else { -1.0 };
                    // The joint goes where the triangle puts it.
                    let base = dy.atan2(dx);
                    let a = base + bend * cos.acos();
                    let joint = [p[0] + a.cos() * l1, p[1] + a.sin() * l1];
                    local[parent].2 += self.aim(&world, parent, joint) * ik.mix;
                    world = self.world(&local);
                    local[child].2 += self.aim(&world, child, target) * ik.mix;
                    world = self.world(&local);
                }
                [] => {}
            }
        }
        // Slots.
        let mut colors: Vec<[f32; 4]> = self.slots.iter().map(|s| s.color).collect();
        let mut names: Vec<Option<String>> =
            self.slots.iter().map(|s| s.attachment.clone()).collect();
        let mut deforms = Vec::new();
        if let Some(a) = animation {
            for track in &a.slots {
                if let Some(c) = sample(&track.color, time)
                    && let Some(slot) = colors.get_mut(track.slot)
                {
                    *slot = c;
                }
                if let Some((_, name)) = track.attachment.iter().rev().find(|(t, _)| *t <= time)
                    && let Some(slot) = names.get_mut(track.slot)
                {
                    *slot = name.clone();
                }
            }
            for track in &a.deforms {
                let Some(slot) = self.slots.get(track.slot) else {
                    continue;
                };
                let Some(i) = slot
                    .attachments
                    .iter()
                    .position(|x| x.name == track.attachment)
                else {
                    continue;
                };
                if let Some(offsets) = sample(&track.keys, time) {
                    deforms.push(((track.slot, i), offsets));
                }
            }
        }
        let attachments = (self.slots.iter().zip(names))
            .map(|(slot, name)| {
                let name = name?;
                slot.attachments.iter().position(|x| x.name == name)
            })
            .collect();
        Pose {
            world,
            colors,
            attachments,
            deforms,
        }
    }

    /// The bones' frames from their local values, root first.
    fn world(&self, local: &[Local]) -> Vec<Affine> {
        let mut world: Vec<Affine> = Vec::with_capacity(self.bones.len());
        for (i, bone) in self.bones.iter().enumerate() {
            let (x, y, r, s, h) = local[i];
            let own = Affine::local(x, y, r, s, h);
            let parent = bone.parent.filter(|&p| p < i).map(|p| world[p]);
            let w = match parent {
                None => own,
                Some(p) => match bone.inherit {
                    Inherit::Normal => p.then(&own),
                    Inherit::OnlyTranslation => Affine {
                        m: own.m,
                        t: p.apply([x, y]),
                    },
                    Inherit::NoRotation => {
                        let (sx, sy) = column_lengths(p.m);
                        Affine {
                            m: [sx, 0.0, 0.0, sy],
                            t: [0.0, 0.0],
                        }
                        .then(&Affine {
                            t: [0.0, 0.0],
                            ..own
                        })
                        .with_t(p.apply([x, y]))
                    }
                    Inherit::NoScale => {
                        let (sx, sy) = column_lengths(p.m);
                        let m = [
                            p.m[0] / sx.max(1e-6),
                            p.m[1] / sy.max(1e-6),
                            p.m[2] / sx.max(1e-6),
                            p.m[3] / sy.max(1e-6),
                        ];
                        Affine { m, t: [0.0, 0.0] }
                            .then(&Affine {
                                t: [0.0, 0.0],
                                ..own
                            })
                            .with_t(p.apply([x, y]))
                    }
                },
            };
            world.push(w);
        }
        world
    }

    /// How far (degrees, in the bone's own turning sense) bone `i` must
    /// turn for its x axis to point at `target`.
    fn aim(&self, world: &[Affine], i: usize, target: [f32; 2]) -> f32 {
        let w = &world[i];
        let want = (target[1] - w.t[1]).atan2(target[0] - w.t[0]).to_degrees();
        let mut delta = want - angle_of(w);
        delta = (delta + 180.0).rem_euclid(360.0) - 180.0;
        // A mirrored parent turns its children the other way.
        let mirrored = self.bones[i]
            .parent
            .is_some_and(|p| world[p].m[0] * world[p].m[3] - world[p].m[1] * world[p].m[2] < 0.0);
        if mirrored { -delta } else { delta }
    }
}

impl Affine {
    fn with_t(mut self, t: [f32; 2]) -> Affine {
        self.t = t;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::rig::{Bone, BoneTrack, Ik, RigAnimation};

    fn arm() -> Rig {
        Rig {
            bones: vec![
                Bone {
                    name: "root".into(),
                    ..Default::default()
                },
                Bone {
                    name: "upper".into(),
                    parent: Some(0),
                    length: 10.0,
                    ..Default::default()
                },
                Bone {
                    name: "lower".into(),
                    parent: Some(1),
                    x: 10.0,
                    length: 10.0,
                    ..Default::default()
                },
                Bone {
                    name: "target".into(),
                    parent: Some(0),
                    x: 12.0,
                    y: 12.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn children_follow_their_parents() {
        let mut rig = arm();
        rig.bones[1].rotation = 90.0;
        let pose = rig.pose(0.0);
        assert!(close(pose.world[2].t, [0.0, 10.0]), "{:?}", pose.world[2].t);
        // Scaled parent: the child sits further out.
        rig.bones[1].scale = [2.0, 1.0];
        assert!(close(rig.pose(0.0).world[2].t, [0.0, 20.0]));
        // Only translation: the child keeps its own turn.
        rig.bones[2].inherit = Inherit::OnlyTranslation;
        assert!((angle_of(&rig.pose(0.0).world[2])).abs() < 1e-3);
    }

    #[test]
    fn keys_are_read_with_their_curves_and_the_animation_loops() {
        let mut rig = arm();
        rig.animations.push(RigAnimation {
            name: "wave".into(),
            duration: 2.0,
            bones: vec![BoneTrack {
                bone: 1,
                rotate: vec![
                    Key {
                        time: 0.0,
                        value: 0.0,
                        curve: Curve::Linear,
                    },
                    Key {
                        time: 1.0,
                        value: 90.0,
                        curve: Curve::Stepped,
                    },
                    Key {
                        time: 2.0,
                        value: 0.0,
                        curve: Curve::Linear,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        });
        rig.animation = Some(0);
        let angle = |t| angle_of(&rig.pose(t).world[1]);
        assert!((angle(0.5) - 45.0).abs() < 1e-3);
        assert!((angle(1.5) - 90.0).abs() < 1e-3, "stepped: held");
        assert!((angle(2.5) - 45.0).abs() < 1e-3, "looped");
        // An ease: slow at both ends.
        let eased = ease(Curve::Bezier([0.42, 0.0, 0.58, 1.0]), 0.25);
        assert!(eased < 0.2 && eased > 0.05, "{eased}");
        assert!((ease(Curve::Bezier([0.42, 0.0, 0.58, 1.0]), 0.5) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn two_bone_ik_reaches_the_target_bending_either_way() {
        let mut rig = arm();
        rig.ik.push(Ik {
            name: "arm".into(),
            bones: vec![1, 2],
            target: 3,
            ..Default::default()
        });
        for bend in [true, false] {
            rig.ik[0].bend_positive = bend;
            let pose = rig.pose(0.0);
            let tip = pose.world[2].apply([10.0, 0.0]);
            assert!(close(tip, [12.0, 12.0]), "{bend}: {tip:?}");
            let joint = pose.world[2].t;
            // The two bends put the elbow either side of the line.
            let side = joint[0] * 12.0 - joint[1] * 12.0;
            assert!(
                if bend { side < 0.0 } else { side > 0.0 },
                "{bend}: {joint:?}"
            );
        }
        // Out of reach: pointing straight at it.
        rig.bones[3].x = 40.0;
        rig.bones[3].y = 0.0;
        let pose = rig.pose(0.0);
        let tip = pose.world[2].apply([10.0, 0.0]);
        assert!((tip[0] - 20.0).abs() < 0.1 && tip[1].abs() < 0.5, "{tip:?}");
    }
}
