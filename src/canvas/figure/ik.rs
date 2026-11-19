//! Inverse kinematics: turning a figure's joints so a point on it reaches
//! a target. Arms and legs use the analytic two-bone solution (the law of
//! cosines, as the 2D rig's `canvas::rig::eval` does, in 3D with a pole
//! saying which way the elbow or knee points); the spine, neck and fingers
//! use FABRIK, every joint kept inside its limit. Pinned joints stay put:
//! a chain stops below them, and [`Rig::restore_pins`] brings back the
//! ones a move carried away.

use glam::{Mat3, Mat4, Quat, Vec3};

use super::skeleton::{Finger, HumanJoint, JointLimit, Skeleton, TurnAngle};

/// A point on the figure, riding with joint `joint` (in its frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effector {
    pub joint: usize,
    pub local: Vec3,
}

impl Effector {
    /// The point `at` (in the scene) on joint `joint` as `world` has it.
    pub fn at(world: &[Mat4], joint: usize, at: Vec3) -> Self {
        Self {
            joint,
            local: world[joint].inverse().transform_point3(at),
        }
    }

    pub fn position(&self, world: &[Mat4]) -> Vec3 {
        world[self.joint].transform_point3(self.local)
    }
}

/// Which joints a drag turns.
#[derive(Clone, Debug, PartialEq)]
pub enum Chain {
    /// The whole figure moves (the hips, or a model's root).
    Root,
    /// An arm or leg: the upper and middle joints of a two-bone limb.
    TwoBone { upper: usize, mid: usize },
    /// Joints from the top down, each aimed in turn (FABRIK).
    Fabrik(Vec<usize>),
}

/// A skeleton placed in the scene, ready to pose.
pub struct Rig<'a> {
    pub skeleton: &'a Skeleton,
    /// The figure's placement.
    pub root: Mat4,
    /// Its parts' sizes (see [`Skeleton::world_scaled`]).
    pub scales: &'a [Mat3],
    /// Joints whose limits are off.
    pub unlimited: &'a [usize],
    /// Joints that stay where they are.
    pub pinned: &'a [usize],
}

/// FABRIK passes over the points, and rounds of turning the joints to
/// follow them, at most.
const FABRIK_PASSES: usize = 12;
const FABRIK_ROUNDS: usize = 8;
/// Close enough (metres).
const TOLERANCE: f32 = 1e-4;

fn position(m: &Mat4) -> Vec3 {
    m.w_axis.truncate()
}

fn rotation(m: &Mat4) -> Quat {
    m.to_scale_rotation_translation().1.normalize()
}

/// The turn taking the frame of `u1` (and `v1` across it) to that of `u2`
/// (and `v2`).
fn frame_turn(u1: Vec3, v1: Vec3, u2: Vec3, v2: Vec3) -> Option<Quat> {
    let basis = |u: Vec3, v: Vec3| {
        let e1 = u.normalize_or_zero();
        let e2 = (v - e1 * e1.dot(v)).normalize_or_zero();
        (e1 != Vec3::ZERO && e2 != Vec3::ZERO).then(|| Mat3::from_cols(e1, e2, e1.cross(e2)))
    };
    let (a, b) = (basis(u1, v1)?, basis(u2, v2)?);
    Some(Quat::from_mat3(&(b * a.transpose())).normalize())
}

impl Rig<'_> {
    /// Every joint's matrix in the scene, posed by `pose`.
    pub fn world(&self, pose: &[Quat]) -> Vec<Mat4> {
        self.skeleton.world_scaled(self.root, pose, self.scales)
    }

    /// Joint `j`'s limit (none if its limits are off).
    pub fn limit(&self, j: usize) -> JointLimit {
        if self.unlimited.contains(&j) {
            JointLimit::FREE
        } else {
            self.skeleton.joints[j].limit
        }
    }

    /// Turn joint `j` by `delta` in the scene (about the joint), inside
    /// its limit.
    pub fn turn_world(&self, world: &[Mat4], pose: &mut [Quat], j: usize, delta: Quat) {
        let joint = &self.skeleton.joints[j];
        let parent = joint.parent.map_or(self.root, |p| world[p]);
        let frame = rotation(&parent) * joint.rest.rotation;
        let local = frame.inverse() * delta * frame;
        pose[j] = self.limit(j).clamp((local * pose[j]).normalize());
    }

    /// The joints a drag of a point riding with joint `owner` turns: the
    /// limb, finger or stretch of spine it's on, stopping below any
    /// pinned joint above it.
    pub fn chain_for(&self, owner: usize) -> Chain {
        use HumanJoint::*;
        let s = self.skeleton;
        let find = |h: HumanJoint| s.find(h);
        let joints =
            |list: &[HumanJoint]| -> Vec<usize> { list.iter().filter_map(|&h| find(h)).collect() };
        let chain = match s.joints[owner].human {
            Some(Hips) => return Chain::Root,
            None if s.joints[owner].parent.is_none() => return Chain::Root,
            Some(LowerArm(side) | Hand(side)) => {
                match (find(UpperArm(side)), find(LowerArm(side))) {
                    (Some(upper), Some(mid)) => vec![upper, mid],
                    _ => vec![owner],
                }
            }
            Some(LowerLeg(side) | Foot(side) | Toes(side)) => {
                match (find(UpperLeg(side)), find(LowerLeg(side))) {
                    (Some(upper), Some(mid)) => vec![upper, mid],
                    _ => vec![owner],
                }
            }
            Some(Finger(side, finger, k)) => {
                joints(&(0..=k).map(|i| Finger(side, finger, i)).collect::<Vec<_>>())
            }
            Some(Spine) => joints(&[Spine]),
            Some(Chest) => joints(&[Spine, Chest]),
            Some(UpperChest) => joints(&[Spine, Chest, UpperChest]),
            Some(Neck) => joints(&[Chest, UpperChest, Neck]),
            Some(Head) => joints(&[UpperChest, Neck, Head]),
            Some(Shoulder(_) | UpperArm(_) | UpperLeg(_)) => vec![owner],
            None => {
                // A joint of another kind of model: it and up to two above
                // it (not the model's root).
                let mut chain = vec![owner];
                let mut at = s.joints[owner].parent;
                while let Some(p) = at {
                    if chain.len() >= 3 || s.joints[p].parent.is_none() {
                        break;
                    }
                    chain.insert(0, p);
                    at = s.joints[p].parent;
                }
                chain
            }
        };
        // Pinned joints at or above the owner keep their place: what's
        // above them doesn't turn.
        let mut chain: Vec<usize> = chain
            .into_iter()
            .filter(|&j| {
                !self.pinned.iter().any(|&p| {
                    p != j && s.is_ancestor_or_self(p, owner) && s.is_ancestor_or_self(j, p)
                })
            })
            .collect();
        if chain.is_empty() {
            chain.push(owner);
        }
        let two_bone = chain.len() == 2
            && matches!(s.joints[chain[1]].human, Some(LowerArm(_) | LowerLeg(_)))
            && s.joints[chain[1]].parent == Some(chain[0]);
        if two_bone {
            Chain::TwoBone {
                upper: chain[0],
                mid: chain[1],
            }
        } else {
            Chain::Fabrik(chain)
        }
    }

    /// Bring `effector` to `target` by turning the joints its chain says
    /// (nothing for [`Chain::Root`]: moving the figure is the caller's).
    pub fn reach(&self, pose: &mut [Quat], effector: Effector, target: Vec3) {
        match self.chain_for(effector.joint) {
            Chain::Root => {}
            Chain::TwoBone { upper, mid } => {
                self.two_bone(pose, upper, mid, effector, target, None);
            }
            Chain::Fabrik(chain) => self.fabrik(pose, &chain, effector, target),
        }
    }

    /// Joint `mid`'s fold axis in the scene, if it's a hinge.
    fn hinge_axis(&self, world: &[Mat4], mid: usize) -> Option<Vec3> {
        let limit = self.skeleton.joints[mid].limit;
        (!limit.is_free() && limit.swing < 0.3)
            .then(|| (rotation(&world[mid]) * limit.axis).normalize_or_zero())
            .filter(|a| *a != Vec3::ZERO)
    }

    /// Two-bone IK: turn `upper` and `mid` so `effector` (riding with
    /// `mid`) reaches `target`, or points straight at it when it's out of
    /// reach. The limb bends towards `pole` (a point in the scene; by
    /// default where `mid` is now, which keeps the way it bends).
    pub fn two_bone(
        &self,
        pose: &mut [Quat],
        upper: usize,
        mid: usize,
        effector: Effector,
        target: Vec3,
        pole: Option<Vec3>,
    ) {
        let world = self.world(pose);
        let a = position(&world[upper]);
        let b = position(&world[mid]);
        let c = effector.position(&world);
        let (la, lb) = ((b - a).length(), (c - b).length());
        let to_target = target - a;
        if la < 1e-6 || lb < 1e-6 || to_target.length() < 1e-6 {
            return;
        }
        let dir = to_target.normalize();
        let shortest = (la - lb).abs() + 1e-5;
        let longest = (la + lb - 1e-5).max(shortest);
        let d = to_target.length().clamp(shortest, longest);
        // Which way the middle joint goes: towards the pole, or failing
        // that the way the hinge folds.
        let hinge = self.hinge_axis(&world, mid);
        let toward = pole.unwrap_or(b) - a;
        let mut up = toward - dir * toward.dot(dir);
        if up.length() < 1e-4 * la {
            up = hinge.map_or(Vec3::ZERO, |h| dir.cross(h));
            up -= dir * up.dot(dir);
        }
        let up = up
            .try_normalize()
            .unwrap_or_else(|| dir.any_orthonormal_vector());
        let cos = ((la * la + d * d - lb * lb) / (2.0 * la * d)).clamp(-1.0, 1.0);
        let sin = (1.0 - cos * cos).max(0.0).sqrt();
        let b_new = a + (dir * cos + up * sin) * la;
        let c_new = a + dir * d;
        // The upper joint takes the bone, and the plane it bends in, to
        // their new places (so a hinge still bends about its own axis).
        let (ab, ab_new) = ((b - a) / la, (b_new - a) / la);
        let bend_old = ab.cross(c - b);
        let bend_old = if bend_old.length() > 1e-4 * lb {
            bend_old
        } else {
            hinge.unwrap_or(Vec3::ZERO)
        };
        let bend_new = ab_new.cross(c_new - b_new);
        let delta = if bend_new.length() > 1e-4 * lb {
            frame_turn(ab, bend_old, ab_new, bend_new)
        } else {
            None
        }
        .unwrap_or_else(|| Quat::from_rotation_arc(ab, ab_new));
        self.turn_world(&world, pose, upper, delta);
        // Then the middle joint aims the effector.
        let world = self.world(pose);
        let b = position(&world[mid]);
        let c = effector.position(&world);
        if let (Some(from), Some(to)) = ((c - b).try_normalize(), (c_new - b).try_normalize()) {
            self.turn_world(&world, pose, mid, Quat::from_rotation_arc(from, to));
        }
    }

    /// FABRIK: turn `chain` (joints from the top down; `effector` rides
    /// with the last or below it) so `effector` reaches `target`, each
    /// joint kept inside its limit.
    pub fn fabrik(&self, pose: &mut [Quat], chain: &[usize], effector: Effector, target: Vec3) {
        if chain.is_empty() {
            return;
        }
        let n = chain.len();
        for _ in 0..FABRIK_ROUNDS {
            let world = self.world(pose);
            let mut points: Vec<Vec3> = chain.iter().map(|&j| position(&world[j])).collect();
            points.push(effector.position(&world));
            if (points[n] - target).length() < TOLERANCE {
                return;
            }
            let lengths: Vec<f32> = (0..n)
                .map(|i| (points[i + 1] - points[i]).length())
                .collect();
            let base = points[0];
            let total: f32 = lengths.iter().sum();
            if (target - base).length() >= total {
                // Out of reach: straight at it.
                let dir = (target - base).normalize_or_zero();
                for i in 0..n {
                    points[i + 1] = points[i] + dir * lengths[i];
                }
            } else {
                for _ in 0..FABRIK_PASSES {
                    points[n] = target;
                    for i in (0..n).rev() {
                        let towards = (points[i] - points[i + 1]).normalize_or_zero();
                        points[i] = points[i + 1] + towards * lengths[i];
                    }
                    points[0] = base;
                    for i in 0..n {
                        let towards = (points[i + 1] - points[i]).normalize_or_zero();
                        points[i + 1] = points[i] + towards * lengths[i];
                    }
                    if (points[n] - target).length() < TOLERANCE {
                        break;
                    }
                }
            }
            // Each joint in turn aims its bone at its new place.
            let mut moved = false;
            for i in 0..n {
                let world = self.world(pose);
                let at = position(&world[chain[i]]);
                let next = if i + 1 < n {
                    position(&world[chain[i + 1]])
                } else {
                    effector.position(&world)
                };
                let (Some(from), Some(to)) = (
                    (next - at).try_normalize(),
                    (points[i + 1] - at).try_normalize(),
                ) else {
                    continue;
                };
                let before = pose[chain[i]];
                self.turn_world(&world, pose, chain[i], Quat::from_rotation_arc(from, to));
                moved |= pose[chain[i]].turn_angle(before) > 1e-5;
            }
            if !moved {
                return;
            }
        }
    }

    /// Bring every pinned joint in `pins` (with where it should be) back
    /// to its place, by its own chain.
    pub fn restore_pins(&self, pose: &mut [Quat], pins: &[(usize, Vec3)]) {
        for _ in 0..2 {
            for &(joint, at) in pins {
                let world = self.world(pose);
                let Some(&m) = world.get(joint) else {
                    continue;
                };
                if (position(&m) - at).length() < TOLERANCE {
                    continue;
                }
                // The joint's own place rides with its parent.
                let Some(owner) = self.skeleton.joints[joint].parent else {
                    continue;
                };
                let effector = Effector::at(&world, owner, position(&m));
                match self.chain_for(owner) {
                    Chain::Root => {}
                    Chain::TwoBone { upper, mid } => {
                        self.two_bone(pose, upper, mid, effector, at, None);
                    }
                    Chain::Fabrik(chain) => self.fabrik(pose, &chain, effector, at),
                }
            }
        }
    }
}

/// The joints of `finger` on `side`'s hand in `skeleton`, by segment.
pub fn finger_joints(
    skeleton: &Skeleton,
    side: super::skeleton::Side,
    finger: Finger,
) -> Vec<usize> {
    (0..3)
        .filter_map(|k| skeleton.find(HumanJoint::Finger(side, finger, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::figure::mannequin::mannequin;
    use crate::canvas::figure::skeleton::Side;

    fn rig<'a>(skeleton: &'a Skeleton, pinned: &'a [usize]) -> Rig<'a> {
        Rig {
            skeleton,
            root: Mat4::IDENTITY,
            scales: &[],
            unlimited: &[],
            pinned,
        }
    }

    fn at(world: &[Mat4], j: usize) -> Vec3 {
        position(&world[j])
    }

    fn within_limits(rig: &Rig<'_>, pose: &[Quat]) {
        for (j, q) in pose.iter().enumerate() {
            let clamped = rig.limit(j).clamp(*q);
            assert!(
                clamped.turn_angle(*q) < 2e-3,
                "{}: {q} outside its limit",
                rig.skeleton.joints[j].name
            );
        }
    }

    #[test]
    fn two_bone_ik_reaches_a_reachable_target() {
        let model = mannequin();
        let s = &model.skeleton;
        let rig = rig(s, &[]);
        let mut pose = vec![Quat::IDENTITY; s.len()];
        let (upper, mid, hand) = (
            s.find(HumanJoint::UpperArm(Side::Left)).unwrap(),
            s.find(HumanJoint::LowerArm(Side::Left)).unwrap(),
            s.find(HumanJoint::Hand(Side::Left)).unwrap(),
        );
        let world = rig.world(&pose);
        let shoulder = at(&world, upper);
        // In front of the shoulder, nearer than the arm is long: the
        // elbow has to bend.
        let target = shoulder + Vec3::new(0.15, -0.1, 0.3);
        let effector = Effector::at(&world, mid, at(&world, hand));
        rig.reach(&mut pose, effector, target);
        let world = rig.world(&pose);
        let reached = at(&world, hand);
        assert!((reached - target).length() < 2e-3, "{reached} vs {target}");
        // The bones keep their lengths, and the elbow folds as elbows do.
        let la = (at(&world, mid) - shoulder).length();
        assert!((la - 0.28).abs() < 1e-3, "{la}");
        within_limits(&rig, &pose);
        let bend = s.joints[mid].limit.axis;
        let q = pose[mid];
        assert!(
            Vec3::new(q.x, q.y, q.z).dot(bend) > 0.3,
            "folds forward: {q}"
        );
    }

    #[test]
    fn two_bone_ik_points_at_an_unreachable_target() {
        let model = mannequin();
        let s = &model.skeleton;
        let rig = rig(s, &[]);
        let mut pose = vec![Quat::IDENTITY; s.len()];
        let (upper, mid, hand) = (
            s.find(HumanJoint::UpperArm(Side::Right)).unwrap(),
            s.find(HumanJoint::LowerArm(Side::Right)).unwrap(),
            s.find(HumanJoint::Hand(Side::Right)).unwrap(),
        );
        let world = rig.world(&pose);
        let shoulder = at(&world, upper);
        let target = shoulder + Vec3::new(-0.5, 2.0, 1.0);
        let effector = Effector::at(&world, mid, at(&world, hand));
        rig.two_bone(&mut pose, upper, mid, effector, target, None);
        let world = rig.world(&pose);
        let reached = at(&world, hand);
        // As far as the arm goes, straight towards it.
        let length = 0.28 + 0.25;
        assert!(((reached - shoulder).length() - length).abs() < 2e-3);
        let along = (reached - shoulder).normalize();
        assert!(
            along.dot((target - shoulder).normalize()) > 0.999,
            "{along}"
        );
    }

    #[test]
    fn the_pole_says_which_way_the_knee_points() {
        let model = mannequin();
        let s = &model.skeleton;
        let mut free = Vec::new();
        free.extend(0..s.len());
        // Limits off, so either way is allowed: only the pole decides.
        let rig = Rig {
            unlimited: &free,
            ..rig(s, &[])
        };
        let (upper, mid, foot) = (
            s.find(HumanJoint::UpperLeg(Side::Left)).unwrap(),
            s.find(HumanJoint::LowerLeg(Side::Left)).unwrap(),
            s.find(HumanJoint::Foot(Side::Left)).unwrap(),
        );
        let world = rig.world(&[]);
        let hip = at(&world, upper);
        let target = hip + Vec3::new(0.0, -0.6, 0.0);
        let effector = Effector::at(&world, mid, at(&world, foot));
        for side in [Vec3::Z, Vec3::NEG_Z, Vec3::X] {
            let mut pose = vec![Quat::IDENTITY; s.len()];
            let pole = hip + Vec3::new(0.0, -0.4, 0.0) + side;
            rig.two_bone(&mut pose, upper, mid, effector, target, Some(pole));
            let world = rig.world(&pose);
            let knee = at(&world, mid);
            assert!((at(&world, foot) - target).length() < 2e-3);
            // The knee is in the plane of hip, target and pole, on the
            // pole's side.
            let normal = (target - hip).cross(pole - hip).normalize();
            assert!((knee - hip).dot(normal).abs() < 2e-3, "{side}: {knee}");
            assert!((knee - hip).dot(side) > 0.1, "{side}: {knee}");
        }
    }

    #[test]
    fn fabrik_reaches_within_limits() {
        let model = mannequin();
        let s = &model.skeleton;
        let rig = rig(s, &[]);
        let mut pose = vec![Quat::IDENTITY; s.len()];
        let head = s.find(HumanJoint::Head).unwrap();
        let world = rig.world(&pose);
        // The top of the head, forward and down: the neck and chest bend.
        let top = at(&world, head) + Vec3::new(0.0, 0.2, 0.0);
        let effector = Effector::at(&world, head, top);
        let target = top + Vec3::new(0.08, -0.05, 0.2);
        rig.reach(&mut pose, effector, target);
        let world = rig.world(&pose);
        assert!((effector.position(&world) - target).length() < 0.01);
        within_limits(&rig, &pose);
        // A target the limits can't allow (the head bent right back): the
        // joints stop at their limits.
        let mut pose = vec![Quat::IDENTITY; s.len()];
        rig.reach(&mut pose, effector, top + Vec3::new(0.0, -0.6, -0.6));
        within_limits(&rig, &pose);
        // And a finger can't bend backwards.
        let tip = s
            .find(HumanJoint::Finger(Side::Left, Finger::Index, 2))
            .unwrap();
        let mut pose = vec![Quat::IDENTITY; s.len()];
        let world = rig.world(&pose);
        let end = at(&world, tip) + Vec3::new(0.02, 0.0, 0.0);
        let effector = Effector::at(&world, tip, end);
        rig.reach(&mut pose, effector, end + Vec3::new(-0.04, 0.06, 0.0));
        within_limits(&rig, &pose);
        let curled = Effector::at(&world, tip, end);
        let mut pose = vec![Quat::IDENTITY; s.len()];
        rig.reach(&mut pose, curled, end + Vec3::new(-0.05, -0.05, 0.0));
        let world = rig.world(&pose);
        assert!(
            curled.position(&world).y < end.y - 0.03,
            "curls towards the palm"
        );
        within_limits(&rig, &pose);
    }

    #[test]
    fn pinned_joints_stay_put() {
        let model = mannequin();
        let s = &model.skeleton;
        let upper_chest = s.find(HumanJoint::UpperChest).unwrap();
        let neck = s.find(HumanJoint::Neck).unwrap();
        let head = s.find(HumanJoint::Head).unwrap();
        let pins = [neck];
        let rig = rig(s, &pins);
        // The head's chain would turn the upper chest too; with the neck
        // pinned only the neck and head turn.
        assert_eq!(rig.chain_for(head), Chain::Fabrik(vec![neck, head]));
        let mut pose = vec![Quat::IDENTITY; s.len()];
        let world = rig.world(&pose);
        let before = at(&world, neck);
        let top = at(&world, head) + Vec3::new(0.0, 0.2, 0.0);
        let effector = Effector::at(&world, head, top);
        rig.reach(&mut pose, effector, top + Vec3::new(0.1, -0.1, 0.15));
        let world = rig.world(&pose);
        assert!((at(&world, neck) - before).length() < 1e-5);
        assert_eq!(pose[upper_chest], Quat::IDENTITY);
        assert!(effector.position(&world).distance(top) > 0.05, "it moved");

        // A pinned hand comes back after its shoulder turned.
        let hand = s.find(HumanJoint::Hand(Side::Left)).unwrap();
        let upper = s.find(HumanJoint::UpperArm(Side::Left)).unwrap();
        let rig2 = Rig {
            pinned: &[hand],
            ..rig
        };
        let mut pose = vec![Quat::IDENTITY; s.len()];
        let world = rig2.world(&pose);
        let pin = at(&world, hand) + Vec3::new(-0.1, 0.0, 0.1);
        pose[upper] = Quat::from_rotation_z(-1.0);
        rig2.restore_pins(&mut pose, &[(hand, pin)]);
        let world = rig2.world(&pose);
        assert!((at(&world, hand) - pin).length() < 2e-3);
        // Dragging the pinned hand itself only turns the hand.
        assert_eq!(rig2.chain_for(hand), Chain::Fabrik(vec![hand]));
    }

    #[test]
    fn frame_turns_take_one_frame_to_the_other() {
        let q = frame_turn(Vec3::X, Vec3::Y, Vec3::Z, Vec3::X).unwrap();
        assert!((q * Vec3::X).abs_diff_eq(Vec3::Z, 1e-5));
        assert!((q * Vec3::Y).abs_diff_eq(Vec3::X, 1e-5));
        assert!(frame_turn(Vec3::X, Vec3::X, Vec3::Z, Vec3::X).is_none());
    }
}
