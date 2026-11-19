//! Poses that carry across models: each canonical joint's turn by name,
//! in canonical axes (the figure in T-pose facing +Z, its left at +X,
//! palms down; see [`super::skeleton`]). A model whose joints have rest
//! frames of their own gets each turn carried into them, so a pose made on
//! the mannequin poses an imported Mixamo or VRM figure the same way.
//!
//! Built in: body poses (T-pose, A-pose, standing, walking, …) and hand
//! poses (open, fist, point, …, given as each finger's curl and spread so
//! they fit any hand). The user's own poses are saved in the app's data
//! folder (`figure_poses.json`). Poses mirror left to right.

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use super::skeleton::{Finger, HumanJoint, Side, Skeleton, TurnAngle, finger_curl_axis};

/// A pose: canonical joint name → turn (a quaternion x, y, z, w) in
/// canonical axes. Joints left out are at rest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pose {
    pub joints: BTreeMap<String, [f32; 4]>,
}

/// A turn seen in a mirror (across the figure's middle, x = 0).
pub fn reflect(q: Quat) -> Quat {
    Quat::from_xyzw(q.x, -q.y, -q.z, q.w)
}

/// Whether `h` is a joint of a hand's fingers.
pub fn is_finger(h: HumanJoint) -> bool {
    matches!(h, HumanJoint::Finger(..))
}

impl Pose {
    pub fn get(&self, h: HumanJoint) -> Option<Quat> {
        self.joints
            .get(&h.name())
            .map(|q| Quat::from_array(*q).normalize())
    }

    pub fn set(&mut self, h: HumanJoint, q: Quat) {
        self.joints.insert(h.name(), q.normalize().to_array());
    }

    pub fn is_empty(&self) -> bool {
        self.joints.is_empty()
    }

    /// The same pose seen in a mirror: left and right swapped.
    pub fn mirrored(&self) -> Pose {
        let mut out = Pose::default();
        for (name, q) in &self.joints {
            match HumanJoint::from_name(name) {
                Some(h) => out.set(h.mirror(), reflect(Quat::from_array(*q))),
                None => {
                    out.joints.insert(name.clone(), *q);
                }
            }
        }
        out
    }

    /// The pose of a figure (its joints' turns `pose`, by index) for the
    /// joints `which` says.
    pub fn capture(skeleton: &Skeleton, pose: &[Quat], which: impl Fn(HumanJoint) -> bool) -> Pose {
        let rest = skeleton.rest_rotations();
        let mut out = Pose::default();
        for (i, joint) in skeleton.joints.iter().enumerate() {
            let Some(h) = joint.human.filter(|&h| which(h)) else {
                continue;
            };
            let local = pose.get(i).copied().unwrap_or(Quat::IDENTITY);
            let turn = rest[i] * local * rest[i].inverse();
            if turn.turn_angle(Quat::IDENTITY) > 1e-6 {
                out.set(h, turn);
            }
        }
        out
    }

    /// Pose a figure (its joints' turns by index, grown to the skeleton)
    /// for the joints `which` says: those this pose has take its turns;
    /// the rest go back to rest if `reset_others`, else stay.
    pub fn apply(
        &self,
        skeleton: &Skeleton,
        pose: &mut Vec<[f32; 4]>,
        which: impl Fn(HumanJoint) -> bool,
        reset_others: bool,
    ) {
        let rest = skeleton.rest_rotations();
        if pose.len() < skeleton.len() {
            pose.resize(skeleton.len(), Quat::IDENTITY.to_array());
        }
        for (i, joint) in skeleton.joints.iter().enumerate() {
            let Some(h) = joint.human.filter(|&h| which(h)) else {
                continue;
            };
            match self.get(h) {
                Some(turn) => {
                    let local = rest[i].inverse() * turn * rest[i];
                    pose[i] = local.normalize().to_array();
                }
                None if reset_others => pose[i] = Quat::IDENTITY.to_array(),
                None => {}
            }
        }
    }
}

/// A figure's pose (by joint index) mirrored: each joint takes its
/// counterpart's turn, seen in a mirror (joints in the middle mirror their
/// own). Joints with no counterpart in the model, and those that aren't
/// human joints, keep theirs, so mirroring twice gives the pose back.
pub fn mirror_figure(skeleton: &Skeleton, pose: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let turns: Vec<Quat> = (0..skeleton.len())
        .map(|i| pose.get(i).map_or(Quat::IDENTITY, |q| Quat::from_array(*q)))
        .collect();
    let all = Pose::capture(skeleton, &turns, |_| true);
    let mirrored = all.mirrored();
    let mut out: Vec<[f32; 4]> = turns.iter().map(|q| q.to_array()).collect();
    let has = |h: HumanJoint| skeleton.find(h).is_some();
    mirrored.apply(skeleton, &mut out, |h| has(h) && has(h.mirror()), true);
    out
}

/// A built-in body pose: turns in degrees about X, then Y, then Z (applied
/// Z first), by canonical name; `symmetric` ones give the right side the
/// left side's turns, mirrored.
pub struct BodyPreset {
    pub name: &'static str,
    pub joints: &'static [(&'static str, [f32; 3])],
    pub symmetric: bool,
}

/// Turns in degrees about X, Y and Z (applied Z first).
fn euler(d: [f32; 3]) -> Quat {
    Quat::from_rotation_x(d[0].to_radians())
        * Quat::from_rotation_y(d[1].to_radians())
        * Quat::from_rotation_z(d[2].to_radians())
}

impl BodyPreset {
    pub fn pose(&self) -> Pose {
        let mut pose = Pose::default();
        for (name, d) in self.joints {
            let Some(h) = HumanJoint::from_name(name) else {
                continue;
            };
            pose.set(h, euler(*d));
            if self.symmetric && h.mirror() != h {
                pose.set(h.mirror(), reflect(euler(*d)));
            }
        }
        pose
    }
}

// Directions, for reading the numbers: the left arm runs along +X, so
// about Z by -70 lowers it to the side, about Y by -90 brings it forward;
// its elbow folds about Y, negative. A leg runs down: about X negative
// lifts it forward, a knee folds about X positive. The spine bends forward
// about X positive.
pub const BODY_PRESETS: &[BodyPreset] = &[
    BodyPreset {
        name: "T-pose",
        joints: &[],
        symmetric: true,
    },
    BodyPreset {
        name: "A-pose",
        joints: &[
            ("leftUpperArm", [0.0, 0.0, -45.0]),
            ("leftLowerArm", [0.0, -8.0, 0.0]),
        ],
        symmetric: true,
    },
    BodyPreset {
        name: "Standing",
        joints: &[
            ("leftUpperArm", [-5.0, 0.0, -74.0]),
            ("leftLowerArm", [0.0, -15.0, 0.0]),
            ("leftUpperLeg", [0.0, 0.0, 2.0]),
        ],
        symmetric: true,
    },
    BodyPreset {
        name: "Walking",
        joints: &[
            ("spine", [3.0, 0.0, 0.0]),
            ("leftUpperLeg", [-25.0, 0.0, 0.0]),
            ("leftLowerLeg", [10.0, 0.0, 0.0]),
            ("rightUpperLeg", [18.0, 0.0, 0.0]),
            ("rightLowerLeg", [30.0, 0.0, 0.0]),
            ("rightFoot", [-20.0, 0.0, 0.0]),
            ("leftUpperArm", [18.0, 0.0, -76.0]),
            ("leftLowerArm", [0.0, -15.0, 0.0]),
            ("rightUpperArm", [-18.0, 0.0, 76.0]),
            ("rightLowerArm", [0.0, 25.0, 0.0]),
        ],
        symmetric: false,
    },
    BodyPreset {
        name: "Running",
        joints: &[
            ("spine", [12.0, 0.0, 0.0]),
            ("leftUpperLeg", [-65.0, 0.0, 0.0]),
            ("leftLowerLeg", [75.0, 0.0, 0.0]),
            ("leftFoot", [-10.0, 0.0, 0.0]),
            ("rightUpperLeg", [25.0, 0.0, 0.0]),
            ("rightLowerLeg", [95.0, 0.0, 0.0]),
            ("rightFoot", [-20.0, 0.0, 0.0]),
            ("leftUpperArm", [45.0, 0.0, -70.0]),
            ("leftLowerArm", [0.0, -95.0, 0.0]),
            ("rightUpperArm", [-45.0, 0.0, 70.0]),
            ("rightLowerArm", [0.0, 95.0, 0.0]),
        ],
        symmetric: false,
    },
    BodyPreset {
        name: "Sitting",
        joints: &[
            ("spine", [5.0, 0.0, 0.0]),
            ("leftUpperLeg", [-90.0, 0.0, 0.0]),
            ("leftLowerLeg", [90.0, 0.0, 0.0]),
            ("leftUpperArm", [-10.0, 0.0, -75.0]),
            ("leftLowerArm", [0.0, -50.0, 0.0]),
        ],
        symmetric: true,
    },
    BodyPreset {
        name: "Kneeling",
        joints: &[
            ("leftLowerLeg", [90.0, 0.0, 0.0]),
            ("leftUpperArm", [-5.0, 0.0, -74.0]),
            ("leftLowerArm", [0.0, -15.0, 0.0]),
        ],
        symmetric: true,
    },
    BodyPreset {
        name: "Reaching",
        joints: &[
            ("spine", [8.0, 0.0, 0.0]),
            ("rightUpperArm", [-35.0, 90.0, 0.0]),
            ("rightLowerArm", [0.0, 10.0, 0.0]),
            ("leftUpperArm", [-5.0, 0.0, -74.0]),
            ("leftLowerArm", [0.0, -15.0, 0.0]),
        ],
        symmetric: false,
    },
    BodyPreset {
        name: "Crouching",
        joints: &[
            ("spine", [25.0, 0.0, 0.0]),
            ("chest", [10.0, 0.0, 0.0]),
            ("neck", [-20.0, 0.0, 0.0]),
            ("leftUpperLeg", [-100.0, 0.0, 8.0]),
            ("leftLowerLeg", [130.0, 0.0, 0.0]),
            ("leftFoot", [-30.0, 0.0, 0.0]),
            ("leftUpperArm", [-40.0, 0.0, -70.0]),
            ("leftLowerArm", [0.0, -60.0, 0.0]),
        ],
        symmetric: true,
    },
    BodyPreset {
        name: "Lying",
        joints: &[
            ("hips", [-90.0, 0.0, 0.0]),
            ("leftUpperArm", [0.0, 0.0, -60.0]),
            ("leftLowerArm", [0.0, -10.0, 0.0]),
        ],
        symmetric: true,
    },
];

/// A hand's shape: each finger's curl at its three joints and spread
/// (degrees; spread towards the thumb's side positive), thumb first.
pub type HandShape = [[f32; 4]; 5];

pub struct HandPreset {
    pub name: &'static str,
    pub shape: HandShape,
}

const FIST_FINGER: [f32; 4] = [88.0, 100.0, 60.0, 0.0];
const FIST_THUMB: [f32; 4] = [35.0, 45.0, 40.0, -10.0];

pub const HAND_PRESETS: &[HandPreset] = &[
    HandPreset {
        name: "Open",
        shape: [
            [0.0, 0.0, 0.0, 12.0],
            [0.0, 0.0, 0.0, 10.0],
            [0.0, 0.0, 0.0, 2.0],
            [0.0, 0.0, 0.0, -6.0],
            [0.0, 0.0, 0.0, -12.0],
        ],
    },
    HandPreset {
        name: "Relaxed",
        shape: [
            [10.0, 10.0, 10.0, 5.0],
            [15.0, 20.0, 10.0, 3.0],
            [20.0, 25.0, 12.0, 0.0],
            [25.0, 28.0, 14.0, -2.0],
            [28.0, 30.0, 15.0, -4.0],
        ],
    },
    HandPreset {
        name: "Fist",
        shape: [
            FIST_THUMB,
            FIST_FINGER,
            FIST_FINGER,
            FIST_FINGER,
            FIST_FINGER,
        ],
    },
    HandPreset {
        name: "Point",
        shape: [
            FIST_THUMB,
            [0.0, 0.0, 0.0, 0.0],
            FIST_FINGER,
            FIST_FINGER,
            FIST_FINGER,
        ],
    },
    HandPreset {
        name: "Pinch",
        shape: [
            [25.0, 25.0, 25.0, -5.0],
            [40.0, 45.0, 35.0, 0.0],
            [25.0, 30.0, 15.0, 0.0],
            [30.0, 35.0, 15.0, 0.0],
            [35.0, 40.0, 20.0, 0.0],
        ],
    },
    HandPreset {
        name: "Peace",
        shape: [
            FIST_THUMB,
            [0.0, 0.0, 0.0, 8.0],
            [0.0, 0.0, 0.0, -8.0],
            FIST_FINGER,
            FIST_FINGER,
        ],
    },
    HandPreset {
        name: "Thumbs up",
        shape: [
            [-10.0, 0.0, 0.0, 25.0],
            FIST_FINGER,
            FIST_FINGER,
            FIST_FINGER,
            FIST_FINGER,
        ],
    },
    HandPreset {
        name: "Holding a cup",
        shape: [
            [20.0, 15.0, 10.0, 20.0],
            [40.0, 40.0, 25.0, 0.0],
            [45.0, 40.0, 25.0, 0.0],
            [50.0, 40.0, 25.0, 0.0],
            [55.0, 40.0, 25.0, 0.0],
        ],
    },
    HandPreset {
        name: "Holding a pen",
        shape: [
            [30.0, 25.0, 20.0, 0.0],
            [35.0, 40.0, 30.0, 0.0],
            [50.0, 55.0, 30.0, -3.0],
            [75.0, 85.0, 50.0, -5.0],
            [80.0, 85.0, 50.0, -8.0],
        ],
    },
];

/// How far each finger fans out on the hand pad's spread, thumb first.
const FAN: [f32; 5] = [1.2, 1.0, 0.2, -0.6, -1.2];

/// The hand pad's shape at `spread` (0 together .. 1 spread wide) and
/// `curl` (0 open .. 1 a fist).
pub fn pad_shape(spread: f32, curl: f32) -> HandShape {
    let (spread, curl) = (spread.clamp(0.0, 1.0), curl.clamp(0.0, 1.0));
    let fan = -3.0 + 18.0 * spread;
    std::array::from_fn(|f| {
        let full = if f == 0 { FIST_THUMB } else { FIST_FINGER };
        [
            full[0] * curl,
            full[1] * curl,
            full[2] * curl,
            fan * FAN[f] * (1.0 - curl * 0.7),
        ]
    })
}

/// `side`'s hand in `shape` on a model with `skeleton`, as a pose of its
/// finger joints (for the fingers `fingers` says): each joint curls about
/// the axis its own bone gives (see [`finger_curl_axis`]), each finger spreads at
/// its knuckle.
pub fn hand_pose(
    skeleton: &Skeleton,
    side: Side,
    shape: &HandShape,
    fingers: impl Fn(Finger) -> bool,
) -> Pose {
    let rest = skeleton.rest_rotations();
    let mut pose = Pose::default();
    for (f, finger) in Finger::ALL.into_iter().enumerate() {
        if !fingers(finger) {
            continue;
        }
        for k in 0..3u8 {
            let h = HumanJoint::Finger(side, finger, k);
            let Some(i) = skeleton.find(h) else {
                continue;
            };
            let bone = skeleton.rest_bone_direction(i, &rest);
            let curl = Quat::from_axis_angle(
                finger_curl_axis(finger, bone),
                shape[f][k as usize].to_radians(),
            );
            let turn = if k == 0 {
                // Towards the thumb's side: about the palm's normal, the
                // way that takes the bone forward (+Z).
                let spread_axis = bone.cross(Vec3::Z).normalize_or(Vec3::NEG_Y);
                Quat::from_axis_angle(spread_axis, shape[f][3].to_radians()) * curl
            } else {
                curl
            };
            pose.set(h, turn);
        }
    }
    pose
}

/// What a saved pose is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoseKind {
    /// The whole body (fingers too).
    Body,
    /// One hand's fingers, as the left hand (mirrored for the right).
    Hand,
}

/// A pose the user saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedPose {
    pub name: String,
    pub kind: PoseKind,
    pub pose: Pose,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::figure::mannequin::mannequin;
    use crate::canvas::figure::model::load_gltf;
    use glam::Mat4;

    fn turns(pose: &[[f32; 4]]) -> Vec<Quat> {
        pose.iter().map(|q| Quat::from_array(*q)).collect()
    }

    #[test]
    fn mirroring_twice_gives_the_pose_back() {
        let model = mannequin();
        let s = &model.skeleton;
        // An uneven pose: every joint turned some different way.
        let pose: Vec<[f32; 4]> = (0..s.len())
            .map(|i| {
                let t = i as f32;
                euler([
                    t * 7.0 % 50.0 - 20.0,
                    t * 13.0 % 40.0 - 15.0,
                    t * 5.0 % 60.0 - 30.0,
                ])
                .to_array()
            })
            .collect();
        let once = mirror_figure(s, &pose);
        assert_ne!(once, pose);
        let twice = mirror_figure(s, &once);
        for (a, b) in turns(&twice).iter().zip(turns(&pose)) {
            assert!(a.turn_angle(b) < 1e-4, "{a} vs {b}");
        }
        // The left arm's turn is the right arm's, seen in a mirror.
        let (l, r) = (
            s.find(HumanJoint::UpperArm(Side::Left)).unwrap(),
            s.find(HumanJoint::UpperArm(Side::Right)).unwrap(),
        );
        let mirrored = Quat::from_array(once[r]);
        assert!(mirrored.turn_angle(reflect(Quat::from_array(pose[l]))) < 1e-4);
        // And the hand lands where the other hand was, mirrored.
        let at = |pose: &[[f32; 4]], h| {
            let w = s.world(Mat4::IDENTITY, &turns(pose));
            w[s.find(h).unwrap()].w_axis.truncate()
        };
        let left = at(&pose, HumanJoint::Hand(Side::Left));
        let right = at(&once, HumanJoint::Hand(Side::Right));
        assert!(right.abs_diff_eq(Vec3::new(-left.x, left.y, left.z), 1e-4));
        // A pose by name mirrors back too.
        let walk = BODY_PRESETS
            .iter()
            .find(|p| p.name == "Walking")
            .unwrap()
            .pose();
        assert_eq!(walk.mirrored().mirrored().joints.len(), walk.joints.len());
        for (name, q) in walk.mirrored().mirrored().joints {
            assert!(Quat::from_array(q).turn_angle(Quat::from_array(walk.joints[&name])) < 1e-5);
        }
    }

    #[test]
    fn mirroring_a_model_without_a_counterpart_keeps_that_joint() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/figure-tiny.gltf"
        ))
        .unwrap();
        let model = load_gltf(&bytes, "tiny").unwrap();
        let s = &model.skeleton;
        let pose = vec![
            euler([0.0, 20.0, 0.0]).to_array(),
            euler([10.0, 0.0, 5.0]).to_array(),
            euler([0.0, 0.0, -40.0]).to_array(),
        ];
        let once = mirror_figure(s, &pose);
        // The arm has no right arm to swap with: it stays.
        assert!(Quat::from_array(once[2]).turn_angle(Quat::from_array(pose[2])) < 1e-5);
        // The hips turn the other way.
        assert!(Quat::from_array(once[0]).turn_angle(euler([0.0, -20.0, 0.0])) < 1e-5);
        let twice = mirror_figure(s, &once);
        for (a, b) in turns(&twice).iter().zip(turns(&pose)) {
            assert!(a.turn_angle(b) < 1e-4);
        }
    }

    #[test]
    fn presets_apply_by_name_to_the_mannequin_and_an_imported_model() {
        let preset = BODY_PRESETS
            .iter()
            .find(|p| p.name == "A-pose")
            .unwrap()
            .pose();
        // The mannequin: its left arm goes down 45 degrees.
        let model = mannequin();
        let s = &model.skeleton;
        let mut pose = Vec::new();
        preset.apply(s, &mut pose, |h| !is_finger(h), true);
        let world = s.world(Mat4::IDENTITY, &turns(&pose));
        let at = |h| world[s.find(h).unwrap()].w_axis.truncate();
        let arm = at(HumanJoint::LowerArm(Side::Left)) - at(HumanJoint::UpperArm(Side::Left));
        assert!(
            arm.normalize()
                .abs_diff_eq(Vec3::new(1.0, -1.0, 0.0).normalize(), 1e-3),
            "{arm}"
        );
        // The fixture (Mixamo names, its arm joint at the end of the
        // spine): the same name takes the same turn.
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/figure-tiny.gltf"
        ))
        .unwrap();
        let tiny = load_gltf(&bytes, "tiny").unwrap();
        let mut pose = Vec::new();
        preset.apply(&tiny.skeleton, &mut pose, |h| !is_finger(h), true);
        let arm = tiny
            .skeleton
            .find(HumanJoint::UpperArm(Side::Left))
            .unwrap();
        assert!(
            Quat::from_array(pose[arm])
                .turn_angle(preset.get(HumanJoint::UpperArm(Side::Left)).unwrap())
                < 1e-5
        );
        // Captured back, it's the preset (the joints the model has).
        let back = Pose::capture(&tiny.skeleton, &turns(&pose), |_| true);
        assert_eq!(back.joints.keys().collect::<Vec<_>>(), ["leftUpperArm"]);
        // A model whose joints have rest turns of their own gets the turn
        // carried into them: the bone ends up where the mannequin's does.
        let mut turned = tiny.skeleton.clone();
        turned.joints[2].rest.rotation = Quat::from_rotation_y(1.0);
        turned.joints[2].limit = Default::default();
        let mut pose = Vec::new();
        preset.apply(&turned, &mut pose, |_| true, true);
        let world = turned.world(Mat4::IDENTITY, &turns(&pose));
        let rest = turned.world(Mat4::IDENTITY, &[]);
        let bone = |w: &[Mat4]| w[2].transform_vector3(Vec3::X);
        let expected = preset.get(HumanJoint::UpperArm(Side::Left)).unwrap() * bone(&rest);
        assert!(bone(&world).abs_diff_eq(expected, 1e-4));
    }

    #[test]
    fn every_body_preset_names_real_joints_and_lying_lies_down() {
        for preset in BODY_PRESETS {
            for (name, _) in preset.joints {
                assert!(
                    HumanJoint::from_name(name).is_some(),
                    "{}: {name}",
                    preset.name
                );
            }
        }
        let model = mannequin();
        let s = &model.skeleton;
        let lying = BODY_PRESETS
            .iter()
            .find(|p| p.name == "Lying")
            .unwrap()
            .pose();
        let mut pose = Vec::new();
        lying.apply(s, &mut pose, |h| !is_finger(h), true);
        let world = s.world(Mat4::IDENTITY, &turns(&pose));
        let at = |h| world[s.find(h).unwrap()].w_axis.truncate();
        let body = at(HumanJoint::Head) - at(HumanJoint::Hips);
        assert!(body.y.abs() < 0.05 && body.z.abs() > 0.5, "{body}");
        let sitting = BODY_PRESETS
            .iter()
            .find(|p| p.name == "Sitting")
            .unwrap()
            .pose();
        let mut pose = Vec::new();
        sitting.apply(s, &mut pose, |h| !is_finger(h), true);
        let world = s.world(Mat4::IDENTITY, &turns(&pose));
        let at = |h| world[s.find(h).unwrap()].w_axis.truncate();
        let thigh = at(HumanJoint::LowerLeg(Side::Left)) - at(HumanJoint::UpperLeg(Side::Left));
        assert!(
            thigh.z > 0.35 && thigh.y.abs() < 0.05,
            "thigh forward: {thigh}"
        );
    }

    #[test]
    fn a_fist_curls_every_finger_towards_the_palm_on_either_hand() {
        let model = mannequin();
        let s = &model.skeleton;
        let fist = &HAND_PRESETS
            .iter()
            .find(|p| p.name == "Fist")
            .unwrap()
            .shape;
        for side in [Side::Left, Side::Right] {
            let hand = hand_pose(s, side, fist, |_| true);
            let mut pose = Vec::new();
            hand.apply(s, &mut pose, is_finger, false);
            let world = s.world(Mat4::IDENTITY, &turns(&pose));
            let rest = s.world(Mat4::IDENTITY, &[]);
            for finger in [Finger::Index, Finger::Middle, Finger::Ring, Finger::Little] {
                let tip = s.find(HumanJoint::Finger(side, finger, 2)).unwrap();
                let (a, b) = (world[tip].w_axis.truncate(), rest[tip].w_axis.truncate());
                // Palms face down: a curled fingertip is lower, and nearer
                // the wrist.
                assert!(a.y < b.y - 0.02, "{side:?} {finger:?}: {a} vs {b}");
                assert!(a.x.abs() < b.x.abs(), "{side:?} {finger:?}");
            }
            // Within the fingers' limits.
            for (i, q) in turns(&pose).iter().enumerate() {
                if s.joints[i].human.is_some_and(|h| {
                    is_finger(h) && !matches!(h, HumanJoint::Finger(_, Finger::Thumb, _))
                }) {
                    assert!(
                        s.joints[i].limit.clamp(*q).turn_angle(*q) < 1e-3,
                        "{}",
                        s.joints[i].name
                    );
                }
            }
        }
        // The two hands mirror each other.
        let left = hand_pose(s, Side::Left, fist, |_| true);
        let right = hand_pose(s, Side::Right, fist, |_| true);
        for (name, q) in left.mirrored().joints {
            assert!(
                Quat::from_array(q).turn_angle(Quat::from_array(right.joints[&name])) < 1e-4,
                "{name}"
            );
        }
        // The pad's corners: open, and a fist.
        assert_eq!(pad_shape(0.5, 0.0)[1][0], 0.0);
        assert_eq!(pad_shape(0.5, 1.0)[1][..3], FIST_FINGER[..3]);
    }
}
