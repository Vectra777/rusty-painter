//! A figure's skeleton: joints in a tree (parents before children), each
//! placed in its parent's frame by a rest transform, turned by its pose;
//! forward kinematics from those turns to every joint's place in the
//! scene; the canonical humanoid joints a model's own joint names map to
//! (so poses and presets carry across models); and joint limits, with
//! human ones for the joints of a human.
//!
//! Canonical directions: a figure stands in T-pose facing +Z, up is +Y and
//! its left is +X, palms down. Turns given "in canonical axes" (presets,
//! limits) are about those axes as the figure stands at rest; a model whose
//! joints have their own rest frames gets them carried into those frames
//! (see [`Skeleton::rest_rotations`]).

use glam::{Mat3, Mat4, Quat, Vec3};

/// The angle between two turns, robust where glam's `angle_between` gives
/// NaN for turns a rounding error apart.
pub trait TurnAngle {
    fn turn_angle(self, other: Quat) -> f32;
}

impl TurnAngle for Quat {
    fn turn_angle(self, other: Quat) -> f32 {
        let r = self * other.conjugate();
        2.0 * Vec3::new(r.x, r.y, r.z).length().atan2(r.w.abs())
    }
}

/// A joint's place in its parent's frame at rest (the model's bind pose).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestTransform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for RestTransform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl RestTransform {
    /// The joint's local matrix turned by `pose` (in its own rest frame:
    /// the identity pose is the rest pose).
    pub fn posed(&self, pose: Quat) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation * pose, self.translation)
    }
}

/// How far a joint may turn from rest: a swing (any direction away from
/// its twist axis) up to `swing` radians, and a twist about the axis
/// between `twist[0]` and `twist[1]` radians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointLimit {
    /// The twist axis in the joint's rest frame (usually along the bone).
    pub axis: Vec3,
    pub swing: f32,
    pub twist: [f32; 2],
}

impl Default for JointLimit {
    fn default() -> Self {
        Self::FREE
    }
}

impl JointLimit {
    /// No limit.
    pub const FREE: JointLimit = JointLimit {
        axis: Vec3::Y,
        swing: std::f32::consts::PI,
        twist: [-std::f32::consts::PI, std::f32::consts::PI],
    };

    /// A ball joint: swinging up to `swing` radians away from its bone
    /// (`axis`), twisting about it within `twist`.
    pub fn ball(axis: Vec3, swing: f32, twist: [f32; 2]) -> Self {
        Self { axis, swing, twist }
    }

    /// A hinge: bending about `axis` within `bend` (positive is the way it
    /// folds), with `slack` radians of give in other directions.
    pub fn hinge(axis: Vec3, bend: [f32; 2], slack: f32) -> Self {
        Self {
            axis,
            swing: slack,
            twist: bend,
        }
    }

    /// The limit with its axis carried by `rotation` (into another frame).
    pub fn rotated(&self, rotation: Quat) -> Self {
        if self.is_free() {
            return *self;
        }
        Self {
            axis: (rotation * self.axis).normalize_or(Vec3::Y),
            ..*self
        }
    }

    pub fn is_free(&self) -> bool {
        self.swing >= std::f32::consts::PI
            && self.twist[0] <= -std::f32::consts::PI
            && self.twist[1] >= std::f32::consts::PI
    }

    /// `pose` turned back inside the limit: split into a twist about the
    /// axis and the swing left over, each clamped.
    pub fn clamp(&self, pose: Quat) -> Quat {
        if self.is_free() {
            return pose;
        }
        let axis = self.axis.normalize_or(Vec3::Y);
        let q = if pose.w < 0.0 { -pose } else { pose };
        let along = Vec3::new(q.x, q.y, q.z).dot(axis);
        let twist = Quat::from_xyzw(axis.x * along, axis.y * along, axis.z * along, q.w);
        let twist = if twist.length_squared() < 1e-12 {
            Quat::IDENTITY
        } else {
            twist.normalize()
        };
        let swing = q * twist.conjugate();
        let twist_angle = 2.0 * along.atan2(q.w);
        let twist_angle = twist_angle.clamp(self.twist[0], self.twist[1]);
        let twist = Quat::from_axis_angle(axis, twist_angle);
        let (swing_axis, swing_angle) = swing.to_axis_angle();
        let swing_angle = if swing_angle > std::f32::consts::PI {
            swing_angle - std::f32::consts::TAU
        } else {
            swing_angle
        };
        let swing = if swing_angle.abs() > self.swing {
            Quat::from_axis_angle(swing_axis, self.swing.copysign(swing_angle))
        } else {
            swing
        };
        (swing * twist).normalize()
    }
}

/// One joint of a skeleton.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    /// Its name in the model.
    pub name: String,
    /// Its parent's index (always lower than its own).
    pub parent: Option<usize>,
    pub rest: RestTransform,
    /// The scene's frame to the joint's at rest (the model's bind matrix,
    /// inverted): a vertex's skinning matrix is the joint's posed world
    /// matrix times this.
    pub inverse_bind: Mat4,
    /// The canonical humanoid joint it is, if its name says.
    pub human: Option<HumanJoint>,
    pub limit: JointLimit,
}

/// A model's joints, parents before children.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skeleton {
    pub joints: Vec<Joint>,
}

impl Skeleton {
    pub fn len(&self) -> usize {
        self.joints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.joints.is_empty()
    }

    /// Forward kinematics: every joint's matrix in the scene, the model
    /// placed by `root` and each joint turned by `pose[i]` (missing turns
    /// are the rest pose).
    pub fn world(&self, root: Mat4, pose: &[Quat]) -> Vec<Mat4> {
        self.world_scaled(root, pose, &[])
    }

    /// [`Self::world`] with each joint's part resized by `scales[i]` (in
    /// the joint's own frame; see [`Self::part_scale`]): a joint's matrix
    /// isn't resized itself (so turns and positions stay rigid), its
    /// children are placed where its resized part puts them. Missing
    /// scales are none.
    pub fn world_scaled(&self, root: Mat4, pose: &[Quat], scales: &[Mat3]) -> Vec<Mat4> {
        let mut world: Vec<Mat4> = Vec::with_capacity(self.joints.len());
        for (i, joint) in self.joints.iter().enumerate() {
            let turn = pose.get(i).copied().unwrap_or(Quat::IDENTITY);
            let mut rest = joint.rest;
            if let Some(s) = joint.parent.and_then(|p| scales.get(p)) {
                rest.translation = *s * rest.translation;
            }
            let local = rest.posed(turn);
            let parent = joint.parent.and_then(|p| world.get(p)).unwrap_or(&root);
            world.push(*parent * local);
        }
        world
    }

    /// The skinning matrices for `world` (from [`Self::world`]).
    pub fn skin(&self, world: &[Mat4]) -> Vec<Mat4> {
        self.skin_scaled(world, &[])
    }

    /// The skinning matrices for `world` (from [`Self::world_scaled`] with
    /// the same `scales`): each part resized round its joint.
    pub fn skin_scaled(&self, world: &[Mat4], scales: &[Mat3]) -> Vec<Mat4> {
        world
            .iter()
            .zip(&self.joints)
            .enumerate()
            .map(|(i, (w, j))| match scales.get(i) {
                Some(s) => *w * Mat4::from_mat3(*s) * j.inverse_bind,
                None => *w * j.inverse_bind,
            })
            .collect()
    }

    /// Which way joint `i`'s bone runs in its own frame: towards its first
    /// child, or (at the end of a chain) on from its parent.
    pub fn bone_axis(&self, i: usize) -> Vec3 {
        let joint = &self.joints[i];
        let to_child = self
            .children(i)
            .map(|c| self.joints[c].rest.translation)
            .find(|t| t.length_squared() > 1e-12);
        let axis = to_child.unwrap_or_else(|| {
            joint.rest.rotation.inverse() * joint.rest.translation / joint.rest.scale
        });
        axis.normalize_or(Vec3::Y)
    }

    /// A resize of joint `i`'s part: `along` its bone, `across` it.
    pub fn part_scale(&self, i: usize, along: f32, across: f32) -> Mat3 {
        let a = self.bone_axis(i);
        Mat3::from_diagonal(Vec3::splat(across))
            + Mat3::from_cols(a * a.x, a * a.y, a * a.z) * (along - across)
    }

    /// Each joint's turn in the scene at rest (the model placed at the
    /// origin): what carries canonical axes into the joint's frame.
    pub fn rest_rotations(&self) -> Vec<Quat> {
        self.world(Mat4::IDENTITY, &[])
            .iter()
            .map(|m| m.to_scale_rotation_translation().1.normalize())
            .collect()
    }

    /// Joint `i`'s bone direction at rest, in canonical axes (the model
    /// at the origin).
    pub fn rest_bone_direction(&self, i: usize, rest: &[Quat]) -> Vec3 {
        (rest[i] * self.bone_axis(i)).normalize_or(Vec3::Y)
    }

    /// Give every joint mapped to a human one its human limit (see
    /// [`human_limit`]), in its own frame.
    pub fn apply_human_limits(&mut self) {
        let rest = self.rest_rotations();
        for i in 0..self.joints.len() {
            let Some(h) = self.joints[i].human else {
                continue;
            };
            let bone = self.rest_bone_direction(i, &rest);
            self.joints[i].limit = human_limit(h, bone).rotated(rest[i].inverse());
        }
    }

    /// Joint `j` and every joint below it.
    pub fn subtree(&self, j: usize) -> Vec<usize> {
        let mut out = vec![j];
        // Parents come first, so one pass finds every descendant.
        for i in j + 1..self.joints.len() {
            if self.joints[i].parent.is_some_and(|p| out.contains(&p)) {
                out.push(i);
            }
        }
        out
    }

    /// Whether `a` is `b` or above it.
    pub fn is_ancestor_or_self(&self, a: usize, b: usize) -> bool {
        let mut at = Some(b);
        while let Some(i) = at {
            if i == a {
                return true;
            }
            at = self.joints.get(i).and_then(|j| j.parent);
        }
        false
    }

    /// The joint that is `human`, if any.
    pub fn find(&self, human: HumanJoint) -> Option<usize> {
        self.joints.iter().position(|j| j.human == Some(human))
    }

    /// Joint `i`'s children.
    pub fn children(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.joints.len()).filter(move |&c| self.joints[c].parent == Some(i))
    }

    /// Name every joint whose name maps to a canonical humanoid joint
    /// (the first joint of each name wins).
    pub fn map_human_names(&mut self) {
        let mut taken = Vec::new();
        for joint in &mut self.joints {
            joint.human = HumanJoint::from_name(&joint.name).filter(|h| !taken.contains(h));
            if let Some(h) = joint.human {
                taken.push(h);
            }
        }
    }
}

/// Which way a finger joint folds (positive turns about it curl the
/// finger towards the palm), from its bone's direction in canonical axes:
/// palms face down at rest.
pub fn curl_axis(bone: Vec3) -> Vec3 {
    bone.cross(Vec3::NEG_Y).normalize_or(Vec3::Z)
}

/// Which way the thumb folds: under the palm and across it towards the
/// little finger (the back of the hand, -Z, on either hand), as it closes
/// over the fingers in a fist, rather than straight down.
pub fn thumb_curl_axis(bone: Vec3) -> Vec3 {
    let towards = Vec3::new(0.0, -1.0, -0.8);
    let bend = (towards - bone * towards.dot(bone)).normalize_or(Vec3::NEG_Y);
    let axis = bone.cross(bend);
    if axis.length_squared() > 1e-8 {
        axis.normalize()
    } else {
        curl_axis(bone)
    }
}

/// The axis a finger's joints fold about (see [`curl_axis`] and
/// [`thumb_curl_axis`]).
pub fn finger_curl_axis(finger: Finger, bone: Vec3) -> Vec3 {
    match finger {
        Finger::Thumb => thumb_curl_axis(bone),
        _ => curl_axis(bone),
    }
}

/// How far a human joint turns, in canonical axes, for a joint whose bone
/// runs along `bone` at rest: hinges for the elbows, knees and the
/// fingers' outer joints, cones for the rest, the hips free.
pub fn human_limit(h: HumanJoint, bone: Vec3) -> JointLimit {
    use HumanJoint::*;
    use std::f32::consts::PI;
    let deg = |d: f32| d.to_radians();
    match h {
        Hips => JointLimit::FREE,
        Spine | Chest | UpperChest => JointLimit::ball(bone, deg(28.0), [deg(-30.0), deg(30.0)]),
        Neck => JointLimit::ball(bone, deg(40.0), [deg(-50.0), deg(50.0)]),
        Head => JointLimit::ball(bone, deg(35.0), [deg(-45.0), deg(45.0)]),
        Shoulder(_) => JointLimit::ball(bone, deg(22.0), [deg(-15.0), deg(15.0)]),
        // From T-pose: down by the side, up above the head, forward and
        // a little back.
        UpperArm(_) => JointLimit::ball(bone, deg(100.0), [deg(-90.0), deg(90.0)]),
        // Bending brings the forearm forward (palms down at rest).
        LowerArm(_) => JointLimit::hinge(
            bone.cross(Vec3::Z).normalize_or(Vec3::Y),
            [deg(-5.0), deg(150.0)],
            deg(8.0),
        ),
        Hand(_) => JointLimit::ball(bone, deg(75.0), [deg(-85.0), deg(85.0)]),
        UpperLeg(_) => JointLimit::ball(bone, deg(120.0), [deg(-45.0), deg(45.0)]),
        // Bending takes the shin back.
        LowerLeg(_) => JointLimit::hinge(
            bone.cross(Vec3::NEG_Z).normalize_or(Vec3::X),
            [deg(-5.0), deg(150.0)],
            deg(6.0),
        ),
        Foot(_) => JointLimit::ball(bone, deg(45.0), [deg(-25.0), deg(25.0)]),
        Toes(_) => JointLimit::ball(bone, deg(40.0), [deg(-10.0), deg(10.0)]),
        Finger(_, self::Finger::Thumb, 0) => {
            JointLimit::ball(bone, deg(55.0), [deg(-40.0), deg(40.0)])
        }
        Finger(_, self::Finger::Thumb, _) => {
            JointLimit::hinge(thumb_curl_axis(bone), [deg(-25.0), deg(85.0)], deg(10.0))
        }
        // The knuckles also spread the fingers.
        Finger(_, _, 0) => JointLimit::hinge(curl_axis(bone), [deg(-25.0), deg(95.0)], deg(25.0)),
        Finger(_, _, 1) => JointLimit::hinge(curl_axis(bone), [deg(-2.0), deg(110.0)], deg(4.0)),
        Finger(_, _, _) => JointLimit::hinge(curl_axis(bone), [deg(-10.0), deg(85.0)], deg(4.0)),
    }
    .clamped_to(PI)
}

impl JointLimit {
    /// Its ranges inside ±`max`.
    fn clamped_to(self, max: f32) -> Self {
        Self {
            axis: self.axis,
            swing: self.swing.min(max),
            twist: [self.twist[0].max(-max), self.twist[1].min(max)],
        }
    }
}

/// A side of the body (the right by default: most people's drawing hand).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    #[default]
    Right,
}

impl Side {
    pub fn mirror(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Finger {
    Thumb,
    Index,
    Middle,
    Ring,
    Little,
}

impl Finger {
    pub const ALL: [Finger; 5] = [
        Finger::Thumb,
        Finger::Index,
        Finger::Middle,
        Finger::Ring,
        Finger::Little,
    ];

    fn word(self) -> &'static str {
        match self {
            Finger::Thumb => "Thumb",
            Finger::Index => "Index",
            Finger::Middle => "Middle",
            Finger::Ring => "Ring",
            Finger::Little => "Little",
        }
    }
}

/// The joints of the canonical humanoid skeleton every model's joints are
/// mapped onto (VRM's humanoid bones): the body, and three joints in each
/// finger (proximal, intermediate, distal: segment 0, 1, 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HumanJoint {
    Hips,
    Spine,
    Chest,
    UpperChest,
    Neck,
    Head,
    Shoulder(Side),
    UpperArm(Side),
    LowerArm(Side),
    Hand(Side),
    UpperLeg(Side),
    LowerLeg(Side),
    Foot(Side),
    Toes(Side),
    Finger(Side, Finger, u8),
}

/// The finger segments' names, by segment.
const SEGMENTS: [&str; 3] = ["Proximal", "Intermediate", "Distal"];

impl HumanJoint {
    /// Every canonical joint, parents before children.
    pub fn all() -> Vec<HumanJoint> {
        use HumanJoint::*;
        let mut all = vec![Hips, Spine, Chest, UpperChest, Neck, Head];
        for side in [Side::Left, Side::Right] {
            all.extend([Shoulder(side), UpperArm(side), LowerArm(side), Hand(side)]);
        }
        for side in [Side::Left, Side::Right] {
            all.extend([UpperLeg(side), LowerLeg(side), Foot(side), Toes(side)]);
        }
        for side in [Side::Left, Side::Right] {
            for finger in self::Finger::ALL {
                all.extend((0..3).map(|s| Finger(side, finger, s)));
            }
        }
        all
    }

    /// Its parent in the canonical skeleton.
    pub fn parent(self) -> Option<HumanJoint> {
        use HumanJoint::*;
        Some(match self {
            Hips => return None,
            Spine => Hips,
            Chest => Spine,
            UpperChest => Chest,
            Neck => UpperChest,
            Head => Neck,
            Shoulder(_) => UpperChest,
            UpperArm(s) => Shoulder(s),
            LowerArm(s) => UpperArm(s),
            Hand(s) => LowerArm(s),
            UpperLeg(_) => Hips,
            LowerLeg(s) => UpperLeg(s),
            Foot(s) => LowerLeg(s),
            Toes(s) => Foot(s),
            Finger(s, _, 0) => Hand(s),
            Finger(s, f, k) => Finger(s, f, k - 1),
        })
    }

    /// The same joint on the other side (itself in the middle).
    pub fn mirror(self) -> HumanJoint {
        use HumanJoint::*;
        match self {
            Shoulder(s) => Shoulder(s.mirror()),
            UpperArm(s) => UpperArm(s.mirror()),
            LowerArm(s) => LowerArm(s.mirror()),
            Hand(s) => Hand(s.mirror()),
            UpperLeg(s) => UpperLeg(s.mirror()),
            LowerLeg(s) => LowerLeg(s.mirror()),
            Foot(s) => Foot(s.mirror()),
            Toes(s) => Toes(s.mirror()),
            Finger(s, f, k) => Finger(s.mirror(), f, k),
            other => other,
        }
    }

    /// Its canonical name, as VRM writes it (`leftUpperArm`,
    /// `rightIndexProximal`).
    pub fn name(self) -> String {
        use HumanJoint::*;
        let sided = |s: Side, part: &str| format!("{}{part}", s.word());
        match self {
            Hips => "hips".into(),
            Spine => "spine".into(),
            Chest => "chest".into(),
            UpperChest => "upperChest".into(),
            Neck => "neck".into(),
            Head => "head".into(),
            Shoulder(s) => sided(s, "Shoulder"),
            UpperArm(s) => sided(s, "UpperArm"),
            LowerArm(s) => sided(s, "LowerArm"),
            Hand(s) => sided(s, "Hand"),
            UpperLeg(s) => sided(s, "UpperLeg"),
            LowerLeg(s) => sided(s, "LowerLeg"),
            Foot(s) => sided(s, "Foot"),
            Toes(s) => sided(s, "Toes"),
            Finger(s, f, k) => sided(s, &format!("{}{}", f.word(), SEGMENTS[(k as usize).min(2)])),
        }
    }

    /// The canonical joint a model's joint `name` stands for, read from
    /// the naming of the common rigs: VRM (`leftUpperArm`), Mixamo
    /// (`mixamorig:LeftForeArm`, `LeftHandIndex1`), Unreal and MakeHuman's
    /// game rig (`upperarm_l`, `spine_01`, `index_01_r`), Blender's Rigify
    /// (`upper_arm.L`, `DEF-thigh.R`), 3ds Max's Biped (`Bip01 L
    /// Forearm`) and VRoid (`J_Bip_L_UpperArm`).
    pub fn from_name(name: &str) -> Option<HumanJoint> {
        use HumanJoint::*;
        // Namespaces (`mixamorig:`) go.
        let name = name.rsplit(':').next().unwrap_or(name);
        let mut side = None;
        let mut base = String::new();
        for token in tokens(name) {
            match token.as_str() {
                "l" | "left" => side = Some(Side::Left),
                "r" | "right" => side = Some(Side::Right),
                // Rig prefixes that say nothing about the joint.
                "def" | "org" | "mch" | "j" | "bip" | "bip01" | "bip001" | "jnt" | "bone" | "c" => {
                }
                t => base.push_str(t),
            }
        }
        let base = base
            .strip_prefix("hand")
            .filter(|b| finger_of(b).is_some())
            .map_or(base.as_str(), |b| b);
        let body = match base {
            "hips" | "hip" | "pelvis" => Some(Hips),
            "spine" | "spine0" | "spine00" | "spine01" | "abdomen" => Some(Spine),
            "spine1" | "spine02" | "chest" => Some(Chest),
            "spine2" | "spine03" | "upperchest" | "chest1" => Some(UpperChest),
            "neck" | "neck01" | "neck1" => Some(Neck),
            "head" => Some(Head),
            _ => None,
        };
        if body.is_some() {
            return body;
        }
        let side = side?;
        let sided = match base {
            "shoulder" | "clavicle" | "collar" => Some(Shoulder(side)),
            "arm" | "upperarm" | "uparm" => Some(UpperArm(side)),
            "forearm" | "lowerarm" | "elbow" => Some(LowerArm(side)),
            "hand" | "wrist" => Some(Hand(side)),
            "upleg" | "thigh" | "upperleg" => Some(UpperLeg(side)),
            "leg" | "calf" | "lowerleg" | "shin" | "knee" => Some(LowerLeg(side)),
            "foot" | "ankle" => Some(Foot(side)),
            "toebase" | "toes" | "toe" | "ball" | "toe0" => Some(Toes(side)),
            _ => None,
        };
        if sided.is_some() {
            return sided;
        }
        let (finger, segment) = finger_of(base)?;
        Some(Finger(side, finger, segment))
    }
}

/// A finger joint's finger and segment from a name with its side taken
/// out (`index1`, `index01`, `indexproximal`, `pinky3`).
fn finger_of(base: &str) -> Option<(Finger, u8)> {
    let fingers = [
        ("thumb", Finger::Thumb),
        ("index", Finger::Index),
        ("middle", Finger::Middle),
        ("ring", Finger::Ring),
        ("pinky", Finger::Little),
        ("little", Finger::Little),
    ];
    let (finger, rest) = fingers
        .iter()
        .find_map(|(word, f)| base.strip_prefix(word).map(|rest| (*f, rest)))?;
    let rest = rest.strip_prefix("finger").unwrap_or(rest);
    let segment = match rest {
        "1" | "01" | "proximal" => 0,
        "2" | "02" | "intermediate" | "middle" => 1,
        "3" | "03" | "distal" => 2,
        _ => return None,
    };
    Some((finger, segment))
}

/// A joint name's words, lower case: split at separators, at lower to
/// upper case changes and before digits that follow a single letter
/// (`L01`), so `LeftHandIndex1` is `left`, `hand`, `index1`.
fn tokens(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            prev = None;
            continue;
        }
        if c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            out.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
        prev = Some(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn approx(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    /// A chain along +X: root at the origin, then two joints a unit apart.
    fn chain() -> Skeleton {
        let joint = |name: &str, parent, x| Joint {
            name: name.into(),
            parent,
            rest: RestTransform {
                translation: Vec3::new(x, 0.0, 0.0),
                ..Default::default()
            },
            inverse_bind: Mat4::IDENTITY,
            human: None,
            limit: JointLimit::FREE,
        };
        Skeleton {
            joints: vec![
                joint("root", None, 0.0),
                joint("elbow", Some(0), 1.0),
                joint("hand", Some(1), 1.0),
            ],
        }
    }

    #[test]
    fn forward_kinematics_turns_children_with_their_parents() {
        let skeleton = chain();
        let at = |world: &[Mat4], i: usize| world[i].transform_point3(Vec3::ZERO);
        let rest = skeleton.world(Mat4::IDENTITY, &[]);
        assert!(approx(at(&rest, 2), Vec3::new(2.0, 0.0, 0.0)));
        // A quarter turn at the root about Z swings the chain to +Y.
        let pose = [Quat::from_rotation_z(FRAC_PI_2)];
        let world = skeleton.world(Mat4::IDENTITY, &pose);
        assert!(approx(at(&world, 1), Vec3::new(0.0, 1.0, 0.0)));
        assert!(approx(at(&world, 2), Vec3::new(0.0, 2.0, 0.0)));
        // Then the elbow a quarter turn back: the hand points along +X again.
        let pose = [
            Quat::from_rotation_z(FRAC_PI_2),
            Quat::from_rotation_z(-FRAC_PI_2),
        ];
        let world = skeleton.world(Mat4::IDENTITY, &pose);
        assert!(approx(at(&world, 2), Vec3::new(1.0, 1.0, 0.0)));
        // The root's placement moves everything.
        let root = Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0));
        let world = skeleton.world(root, &pose);
        assert!(approx(at(&world, 2), Vec3::new(1.0, 1.0, 5.0)));
    }

    #[test]
    fn skinning_matrices_are_identity_at_rest() {
        let mut skeleton = chain();
        let rest = skeleton.world(Mat4::IDENTITY, &[]);
        for (joint, w) in skeleton.joints.iter_mut().zip(&rest) {
            joint.inverse_bind = w.inverse();
        }
        for m in skeleton.skin(&skeleton.world(Mat4::IDENTITY, &[])) {
            assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-5));
        }
    }

    #[test]
    fn limits_clamp_swing_and_twist() {
        let limit = JointLimit {
            axis: Vec3::X,
            swing: 0.5,
            twist: [-0.2, 0.2],
        };
        // A swing (about Z) past the cone comes back to its edge.
        let clamped = limit.clamp(Quat::from_rotation_z(1.2));
        assert!((clamped.turn_angle(Quat::IDENTITY) - 0.5).abs() < 1e-4);
        // A twist (about X, the axis) past its range, likewise.
        let clamped = limit.clamp(Quat::from_rotation_x(-1.0));
        assert!(clamped.abs_diff_eq(Quat::from_rotation_x(-0.2), 1e-4));
        // Inside: unchanged.
        let inside = Quat::from_rotation_y(0.3) * Quat::from_rotation_x(0.1);
        assert!(limit.clamp(inside).abs_diff_eq(inside, 1e-4));
        assert_eq!(
            JointLimit::FREE.clamp(Quat::from_rotation_z(3.0)),
            Quat::from_rotation_z(3.0)
        );
    }

    #[test]
    fn common_rig_names_map_to_canonical_joints() {
        use HumanJoint::*;
        let cases = [
            ("mixamorig:Hips", Hips),
            ("mixamorig:Spine1", Chest),
            ("mixamorig:LeftForeArm", LowerArm(Side::Left)),
            (
                "mixamorig:RightHandIndex1",
                Finger(Side::Right, super::Finger::Index, 0),
            ),
            (
                "mixamorig:LeftHandPinky3",
                Finger(Side::Left, super::Finger::Little, 2),
            ),
            ("mixamorig:RightUpLeg", UpperLeg(Side::Right)),
            ("mixamorig:LeftToeBase", Toes(Side::Left)),
            ("upperarm_l", UpperArm(Side::Left)),
            ("spine_01", Spine),
            ("spine_03", UpperChest),
            ("calf_r", LowerLeg(Side::Right)),
            ("thumb_02_l", Finger(Side::Left, super::Finger::Thumb, 1)),
            ("upper_arm.L", UpperArm(Side::Left)),
            ("DEF-thigh.R", UpperLeg(Side::Right)),
            ("Bip01 L Forearm", LowerArm(Side::Left)),
            ("J_Bip_R_Hand", Hand(Side::Right)),
            ("leftUpperArm", UpperArm(Side::Left)),
            (
                "rightRingIntermediate",
                Finger(Side::Right, super::Finger::Ring, 1),
            ),
            ("clavicle_r", Shoulder(Side::Right)),
        ];
        for (name, joint) in cases {
            assert_eq!(HumanJoint::from_name(name), Some(joint), "{name}");
        }
        for name in ["Armature", "root", "mixamorig:HeadTop_End", "IK_target"] {
            assert_eq!(HumanJoint::from_name(name), None, "{name}");
        }
    }

    #[test]
    fn canonical_names_read_back_and_mirror() {
        let all = HumanJoint::all();
        assert_eq!(all.len(), 52);
        for (i, joint) in all.iter().enumerate() {
            assert_eq!(HumanJoint::from_name(&joint.name()), Some(*joint));
            assert_eq!(joint.mirror().mirror(), *joint);
            // Parents come first.
            if let Some(p) = joint.parent() {
                assert!(all[..i].contains(&p), "{joint:?}");
            }
        }
    }

    #[test]
    fn human_limits_bend_elbows_and_knees_the_human_way() {
        let model = crate::canvas::figure::mannequin::mannequin();
        let s = &model.skeleton;
        for side in [Side::Left, Side::Right] {
            let elbow = s.joints[s.find(HumanJoint::LowerArm(side)).unwrap()].limit;
            let knee = s.joints[s.find(HumanJoint::LowerLeg(side)).unwrap()].limit;
            // An elbow folds forward (palms down, the arm out to the side)
            // but not backward.
            let forearm = Vec3::X * if side == Side::Left { 1.0 } else { -1.0 };
            let fold = Quat::from_axis_angle(elbow.axis, 1.5);
            assert!((fold * forearm).z > 0.9, "{side:?} folds forward");
            assert!(elbow.clamp(fold).turn_angle(fold) < 1e-4);
            let back = Quat::from_axis_angle(elbow.axis, -1.0);
            assert!(elbow.clamp(back).turn_angle(Quat::IDENTITY) < 0.1);
            // A knee folds back.
            let shin = Quat::from_axis_angle(knee.axis, 1.5) * Vec3::NEG_Y;
            assert!(shin.z < -0.9, "{side:?}: {shin}");
            let forward = Quat::from_axis_angle(knee.axis, -1.0);
            assert!(knee.clamp(forward).turn_angle(Quat::IDENTITY) < 0.1);
        }
        // The hips are free; a neck can't turn all the way round.
        assert!(s.joints[s.find(HumanJoint::Hips).unwrap()].limit.is_free());
        let neck = s.joints[s.find(HumanJoint::Neck).unwrap()].limit;
        let round = Quat::from_rotation_y(3.0);
        assert!(neck.clamp(round).turn_angle(Quat::IDENTITY) < 1.0);
    }

    #[test]
    fn resized_parts_move_their_children_without_scaling_them() {
        let skeleton = chain();
        // The first bone twice as long, its children's frames untouched.
        let scales = vec![skeleton.part_scale(0, 2.0, 1.0)];
        let world = skeleton.world_scaled(Mat4::IDENTITY, &[], &scales);
        let at = |i: usize| world[i].transform_point3(Vec3::ZERO);
        assert!(approx(at(1), Vec3::new(2.0, 0.0, 0.0)));
        assert!(approx(at(2), Vec3::new(3.0, 0.0, 0.0)));
        assert!(world[1].abs_diff_eq(Mat4::from_translation(at(1)), 1e-5));
        // Its part (skinned to it) is stretched along the bone only.
        let skin = skeleton.skin_scaled(&world, &scales);
        let p = skin[0].transform_point3(Vec3::new(0.5, 0.2, 0.0));
        assert!(approx(p, Vec3::new(1.0, 0.2, 0.0)), "{p}");
        // The bone runs to the child; at the end of the chain it goes on.
        assert!(approx(skeleton.bone_axis(0), Vec3::X));
        assert!(approx(skeleton.bone_axis(2), Vec3::X));
        assert_eq!(skeleton.subtree(1), vec![1, 2]);
        assert!(skeleton.is_ancestor_or_self(0, 2) && !skeleton.is_ancestor_or_self(2, 0));
    }
}
