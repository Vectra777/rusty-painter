//! The mannequin that ships with the app: an artist's wooden figure built
//! here in code (so it needs no file and no licence), 1.75 m tall, with
//! the whole canonical humanoid skeleton — spine, neck, shoulders, arms,
//! legs, feet and three joints in every finger — each part a smooth
//! ellipsoid riding rigidly on its joint, with a ball at each joint.

use std::sync::{Arc, OnceLock};

use glam::{Mat4, Quat, Vec3};

use super::model::{FigureModel, Mesh, SharedModel};
use super::skeleton::{Finger as Digit, HumanJoint, JointLimit, RestTransform, Side, Skeleton};

/// Its [`super::ModelRef::Bundled`] id.
pub const ID: &str = "mannequin";

/// The mannequin, built once.
pub fn mannequin() -> SharedModel {
    static MODEL: OnceLock<SharedModel> = OnceLock::new();
    MODEL.get_or_init(|| Arc::new(build())).clone()
}

/// Wood, and the darker joints.
const BODY: [u8; 4] = [214, 196, 170, 255];
const JOINT: [u8; 4] = [170, 150, 126, 255];

/// +1 on the figure's left (+X: it faces +Z), -1 on its right.
fn sign(side: Side) -> f32 {
    match side {
        Side::Left => 1.0,
        Side::Right => -1.0,
    }
}

/// The finger's knuckle offset across the hand (+Z is the front), its
/// segments' lengths, and how thick it is.
fn finger_shape(finger: Digit) -> (f32, [f32; 3], f32) {
    match finger {
        Digit::Thumb => (0.03, [0.035, 0.030, 0.025], 0.011),
        Digit::Index => (0.027, [0.040, 0.025, 0.020], 0.0095),
        Digit::Middle => (0.009, [0.043, 0.027, 0.021], 0.0095),
        Digit::Ring => (-0.009, [0.040, 0.025, 0.020], 0.009),
        Digit::Little => (-0.027, [0.032, 0.020, 0.018], 0.008),
    }
}

/// Which way a finger points from its knuckle, in T-pose (the thumb
/// forward and out, the others straight out along the arm).
fn finger_direction(side: Side, finger: Digit) -> Vec3 {
    let s = sign(side);
    match finger {
        Digit::Thumb => Vec3::new(s * 0.6, -0.1, 0.8).normalize(),
        _ => Vec3::new(s, 0.0, 0.0),
    }
}

/// Where joint `h` is at rest (T-pose), in metres.
fn rest_position(h: HumanJoint) -> Vec3 {
    use HumanJoint::*;
    match h {
        Hips => Vec3::new(0.0, 0.95, 0.0),
        Spine => Vec3::new(0.0, 1.05, 0.0),
        Chest => Vec3::new(0.0, 1.18, 0.0),
        UpperChest => Vec3::new(0.0, 1.30, 0.0),
        Neck => Vec3::new(0.0, 1.47, 0.0),
        Head => Vec3::new(0.0, 1.56, 0.0),
        Shoulder(s) => Vec3::new(sign(s) * 0.03, 1.42, 0.0),
        UpperArm(s) => Vec3::new(sign(s) * 0.17, 1.42, 0.0),
        LowerArm(s) => Vec3::new(sign(s) * 0.45, 1.42, 0.0),
        Hand(s) => Vec3::new(sign(s) * 0.70, 1.42, 0.0),
        UpperLeg(s) => Vec3::new(sign(s) * 0.09, 0.92, 0.0),
        LowerLeg(s) => Vec3::new(sign(s) * 0.09, 0.51, 0.0),
        Foot(s) => Vec3::new(sign(s) * 0.09, 0.09, 0.0),
        Toes(s) => Vec3::new(sign(s) * 0.09, 0.025, 0.12),
        Finger(s, f, k) => {
            let (across, lengths, _) = finger_shape(f);
            let base = match f {
                Digit::Thumb => Vec3::new(sign(s) * 0.725, 1.41, across),
                _ => Vec3::new(sign(s) * 0.785, 1.42, across),
            };
            let along: f32 = lengths[..k as usize].iter().sum();
            base + finger_direction(s, f) * along
        }
    }
}

/// A mesh being built, every part on one joint.
struct Builder {
    mesh: Mesh,
}

impl Builder {
    /// An ellipsoid round `center` with semi-axes `axes` (orthogonal),
    /// on joint `joint`.
    fn ellipsoid(
        &mut self,
        center: Vec3,
        axes: [Vec3; 3],
        joint: usize,
        color: [u8; 4],
        detail: u32,
    ) {
        let rings = detail;
        let segments = detail * 2;
        let base = self.mesh.positions.len() as u32;
        let inv = axes.map(|a| a / a.length_squared().max(1e-12));
        for r in 0..=rings {
            let u = std::f32::consts::PI * r as f32 / rings as f32;
            let (su, cu) = u.sin_cos();
            for s in 0..=segments {
                let v = std::f32::consts::TAU * s as f32 / segments as f32;
                let (sv, cv) = v.sin_cos();
                let p = center + axes[0] * cu + (axes[1] * cv + axes[2] * sv) * su;
                // The ellipsoid's gradient there.
                let n = inv[0] * cu + (inv[1] * cv + inv[2] * sv) * su;
                self.mesh.positions.push(p.into());
                self.mesh.normals.push(n.normalize_or(Vec3::Y).into());
                self.mesh.colors.push(color);
                self.mesh.joints.push([joint as u16, 0, 0, 0]);
                self.mesh.weights.push([1.0, 0.0, 0.0, 0.0]);
            }
        }
        // Triangles wound counter-clockwise seen from outside (as glTF
        // has them): which way round depends on the axes' handedness.
        let flip = axes[0].dot(axes[1].cross(axes[2])) < 0.0;
        let row = segments + 1;
        for r in 0..rings {
            for s in 0..segments {
                let a = base + r * row + s;
                let b = a + row;
                if flip {
                    self.mesh.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
                } else {
                    self.mesh.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
                }
            }
        }
    }

    /// A limb from `a` to `b`, `radius` thick (overlapping its ends a
    /// little so the joints' balls close the gaps).
    fn limb(&mut self, a: Vec3, b: Vec3, radius: f32, joint: usize, detail: u32) {
        let along = (b - a) * 0.5;
        let dir = along.normalize_or(Vec3::Y);
        let (u, v) = dir.any_orthonormal_pair();
        let half = along + dir * radius * 0.2;
        self.ellipsoid(
            (a + b) * 0.5,
            [half, u * radius, v * radius],
            joint,
            BODY,
            detail,
        );
    }

    fn ball(&mut self, center: Vec3, radius: f32, joint: usize, detail: u32) {
        self.ellipsoid(
            center,
            [Vec3::X * radius, Vec3::Y * radius, Vec3::Z * radius],
            joint,
            JOINT,
            detail,
        );
    }

    fn blob(&mut self, center: Vec3, radii: Vec3, joint: usize) {
        self.ellipsoid(
            center,
            [Vec3::Y * radii.y, Vec3::X * radii.x, Vec3::Z * radii.z],
            joint,
            BODY,
            10,
        );
    }
}

fn build() -> FigureModel {
    use HumanJoint::*;
    let all = HumanJoint::all();
    let index = |h: HumanJoint| all.iter().position(|&x| x == h).unwrap_or(0);
    let joints = all
        .iter()
        .map(|&h| {
            let at = rest_position(h);
            let parent = h.parent();
            let from = parent.map_or(Vec3::ZERO, rest_position);
            super::skeleton::Joint {
                name: h.name(),
                parent: parent.map(index),
                rest: RestTransform {
                    translation: at - from,
                    ..Default::default()
                },
                inverse_bind: Mat4::from_translation(-at),
                human: Some(h),
                limit: JointLimit::FREE,
            }
        })
        .collect();
    let skeleton = Skeleton { joints };

    let mut b = Builder {
        mesh: Mesh::default(),
    };
    let p = rest_position;
    // Trunk and head.
    b.blob(
        Vec3::new(0.0, 0.93, 0.0),
        Vec3::new(0.155, 0.10, 0.105),
        index(Hips),
    );
    b.blob(
        Vec3::new(0.0, 1.10, 0.0),
        Vec3::new(0.125, 0.09, 0.095),
        index(Spine),
    );
    b.blob(
        Vec3::new(0.0, 1.22, 0.0),
        Vec3::new(0.14, 0.08, 0.10),
        index(Chest),
    );
    b.blob(
        Vec3::new(0.0, 1.33, 0.005),
        Vec3::new(0.165, 0.12, 0.11),
        index(UpperChest),
    );
    b.limb(
        Vec3::new(0.0, 1.43, 0.0),
        Vec3::new(0.0, 1.58, 0.0),
        0.045,
        index(Neck),
        8,
    );
    b.blob(
        Vec3::new(0.0, 1.655, 0.01),
        Vec3::new(0.085, 0.11, 0.10),
        index(Head),
    );
    // A nose, so it's clear which way the head faces.
    b.blob(
        Vec3::new(0.0, 1.645, 0.105),
        Vec3::new(0.015, 0.022, 0.02),
        index(Head),
    );
    for side in [Side::Left, Side::Right] {
        let s = sign(side);
        // Arms.
        b.limb(
            Vec3::new(s * 0.05, 1.42, 0.0),
            p(UpperArm(side)),
            0.05,
            index(Shoulder(side)),
            8,
        );
        b.ball(p(UpperArm(side)), 0.055, index(UpperArm(side)), 8);
        b.limb(
            p(UpperArm(side)),
            p(LowerArm(side)),
            0.045,
            index(UpperArm(side)),
            8,
        );
        b.ball(p(LowerArm(side)), 0.042, index(LowerArm(side)), 8);
        b.limb(
            p(LowerArm(side)),
            p(Hand(side)),
            0.037,
            index(LowerArm(side)),
            8,
        );
        b.ball(p(Hand(side)), 0.03, index(Hand(side)), 6);
        b.ellipsoid(
            p(Hand(side)) + Vec3::new(s * 0.045, 0.0, 0.0),
            [
                Vec3::new(s * 0.05, 0.0, 0.0),
                Vec3::new(0.0, 0.017, 0.0),
                Vec3::new(0.0, 0.0, 0.045),
            ],
            index(Hand(side)),
            BODY,
            8,
        );
        // Fingers: a segment on each joint, out to the tip.
        for finger in Digit::ALL {
            let (_, lengths, radius) = finger_shape(finger);
            let dir = finger_direction(side, finger);
            for k in 0..3u8 {
                let j = Finger(side, finger, k);
                let from = p(j);
                let to = from + dir * lengths[k as usize];
                b.limb(from, to, radius, index(j), 5);
            }
        }
        // Legs.
        b.ball(p(UpperLeg(side)), 0.075, index(UpperLeg(side)), 8);
        b.limb(
            p(UpperLeg(side)),
            p(LowerLeg(side)),
            0.07,
            index(UpperLeg(side)),
            9,
        );
        b.ball(p(LowerLeg(side)), 0.055, index(LowerLeg(side)), 8);
        b.limb(
            p(LowerLeg(side)),
            p(Foot(side)),
            0.05,
            index(LowerLeg(side)),
            9,
        );
        b.ball(p(Foot(side)), 0.045, index(Foot(side)), 6);
        b.blob(
            Vec3::new(s * 0.09, 0.045, 0.035),
            Vec3::new(0.045, 0.04, 0.09),
            index(Foot(side)),
        );
        b.blob(
            Vec3::new(s * 0.09, 0.025, 0.15),
            Vec3::new(0.042, 0.022, 0.04),
            index(Toes(side)),
        );
    }
    FigureModel::new("Mannequin", skeleton, b.mesh, Vec::new())
}

/// Standing at ease (the "Standing" preset): the arms down by the sides
/// instead of T-pose.
pub fn standing_pose() -> Vec<[f32; 4]> {
    standing_pose_of(&mannequin().skeleton)
}

/// The "Standing" preset on any model (its rest pose if it has no human
/// joints).
pub fn standing_pose_of(skeleton: &Skeleton) -> Vec<[f32; 4]> {
    let mut pose = vec![Quat::IDENTITY.to_array(); skeleton.len()];
    if let Some(standing) = super::presets::BODY_PRESETS
        .iter()
        .find(|p| p.name == "Standing")
    {
        standing.pose().apply(skeleton, &mut pose, |_| true, true);
    }
    pose
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mannequin_has_the_whole_canonical_skeleton() {
        let model = mannequin();
        assert_eq!(model.skeleton.len(), HumanJoint::all().len());
        for h in HumanJoint::all() {
            assert!(model.skeleton.find(h).is_some(), "{h:?}");
        }
        // Inverse binds undo the rest placement.
        let rest = model.skeleton.world(Mat4::IDENTITY, &[]);
        for (j, w) in model.skeleton.joints.iter().zip(&rest) {
            assert!((j.inverse_bind * *w).abs_diff_eq(Mat4::IDENTITY, 1e-5));
        }
        let mesh = &model.mesh;
        assert_eq!(mesh.positions.len(), mesh.normals.len());
        assert_eq!(mesh.positions.len(), mesh.joints.len());
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.positions.len())
        );
        assert!(mesh.indices.len().is_multiple_of(3));
        // Small enough to upload at once on a phone.
        assert!(mesh.positions.len() < 20_000, "{}", mesh.positions.len());
    }

    #[test]
    fn standing_lowers_the_arms() {
        let model = mannequin();
        let pose: Vec<Quat> = standing_pose().into_iter().map(Quat::from_array).collect();
        let world = model.skeleton.world(Mat4::IDENTITY, &pose);
        for side in [Side::Left, Side::Right] {
            let hand = model.skeleton.find(HumanJoint::Hand(side)).unwrap();
            let shoulder = model.skeleton.find(HumanJoint::UpperArm(side)).unwrap();
            let h = world[hand].transform_point3(Vec3::ZERO);
            let s = world[shoulder].transform_point3(Vec3::ZERO);
            assert!(h.y < s.y - 0.4, "{side:?}: hand {h}, shoulder {s}");
            assert!(h.x * sign(side) > 0.0, "the hand stays on its side");
        }
    }
}
