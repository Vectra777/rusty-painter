//! From tracked landmarks to a pose: a hand's 21 points (MediaPipe's
//! numbering, see below) become its finger joints' turns, in canonical
//! axes (see [`super::presets`]), for any model with finger joints.
//!
//! Each bone is swung from where it points at rest to where the landmarks
//! say it points, joint after joint down each finger, in the hand's own
//! frame: the palm's plane through the wrist and the index and little
//! fingers' bases. The model's palm is measured the same way at rest, so
//! hands whose fingers don't lie exactly along the arm still match. The
//! thumb's first joint also gets its twist, from the plane its two outer
//! joints bend in (a swing alone leaves it free). Every turn is kept
//! within its joint's limit.
//!
//! Landmarks: 0 the wrist, then four a finger from its base to its tip,
//! thumb first (1-4: CMC, MCP, IP, tip; 5-8 the index: MCP, PIP, DIP,
//! tip; 9-12 middle; 13-16 ring; 17-20 little). The thumb's CMC is its
//! first joint (the metacarpal's), the other fingers' bases their
//! knuckles.
//!
//! A body's 33 points (BlazePose's numbering, see [`retarget_body`])
//! become the body's joints' turns the same way: the torso's frame from
//! the hips and shoulders, the head's from the ears and eyes, each limb
//! swung from the hips and shoulders outwards, with the elbows and knees
//! kept to their hinges.
//!
//! A [`OneEuro`] filter smooths a live stream of them: still hands stop
//! shaking, moving ones keep up.

use glam::{Mat3, Mat4, Quat, Vec3};

use super::presets::Pose;
use super::skeleton::{Finger, HumanJoint, JointLimit, Side, Skeleton, thumb_curl_axis};

/// How many landmarks a hand has.
pub const HAND_LANDMARKS: usize = 21;

/// Each finger's first landmark (its first joint), thumb first.
const FINGER_BASE: [usize; 5] = [1, 5, 9, 13, 17];

/// A hand's pose read from its landmarks.
#[derive(Clone, Debug, PartialEq)]
pub struct HandRetarget {
    /// The finger joints' turns (the hand's own joint is left alone).
    pub pose: Pose,
    /// How the hand is turned in the landmarks' space, from its rest
    /// (canonical axes) — for placing the whole hand.
    pub palm: Quat,
}

/// The landmarks seen in a mirror (x negated): a left hand's become a
/// right hand's.
pub fn mirror_landmarks(points: &[Vec3; HAND_LANDMARKS]) -> [Vec3; HAND_LANDMARKS] {
    points.map(|p| Vec3::new(-p.x, p.y, p.z))
}

/// A right-handed frame from a hand's forward direction and its side
/// towards the thumb: (forward, thumbwards, their cross product).
fn palm_frame(forward: Vec3, across: Vec3) -> Option<Mat3> {
    let f = forward.try_normalize()?;
    let t = (across - f * across.dot(f)).try_normalize()?;
    Some(Mat3::from_cols(f, t, f.cross(t)))
}

/// `side`'s fingers posed as `points` show, on a model with `skeleton`.
/// The points are in any right-handed space (a camera's, y down and z
/// away, is one) and must be a `side` hand's: mirror the other's first
/// ([`mirror_landmarks`]). None if the model has no such hand, or the
/// points don't make a palm.
pub fn retarget_hand(
    skeleton: &Skeleton,
    side: Side,
    points: &[Vec3; HAND_LANDMARKS],
) -> Option<HandRetarget> {
    if points.iter().any(|p| !p.is_finite()) {
        return None;
    }
    let rest = skeleton.rest_rotations();
    let world = skeleton.world(Mat4::IDENTITY, &[]);
    let at = |h: HumanJoint| Some(world[skeleton.find(h)?].w_axis.truncate());
    let base = |f: Finger| at(HumanJoint::Finger(side, f, 0));
    let wrist = at(HumanJoint::Hand(side))?;
    let model = palm_frame(
        base(Finger::Middle)? - wrist,
        base(Finger::Index)? - base(Finger::Little)?,
    )?;
    let seen = palm_frame(points[9] - points[0], points[5] - points[17])?;
    // Landmark space to the hand's at rest.
    let into_rest = model * seen.transpose();
    let mut pose = Pose::default();
    for (f, finger) in Finger::ALL.into_iter().enumerate() {
        // The turn so far down the finger, in canonical axes.
        let mut above = Quat::IDENTITY;
        let first = FINGER_BASE[f];
        let dir =
            |k: usize| (into_rest * (points[first + k + 1] - points[first + k])).try_normalize();
        for k in 0..3usize {
            let h = HumanJoint::Finger(side, finger, k as u8);
            let Some(i) = skeleton.find(h) else {
                break;
            };
            let Some(want) = dir(k) else {
                break;
            };
            let bone = skeleton.rest_bone_direction(i, &rest);
            let turn = match hinge_fold(&skeleton.joints[i].limit, rest[i], above, bone, want) {
                Some(turn) => turn,
                None => {
                    let mut whole = Quat::from_rotation_arc(above * bone, want) * above;
                    if finger == Finger::Thumb && k == 0 {
                        whole = thumb_twist(skeleton, side, &rest, whole, want, dir(1), dir(2));
                    }
                    above.inverse() * whole
                }
            };
            // In the joint's frame, inside its limit.
            let local = rest[i].inverse() * turn * rest[i];
            let local = skeleton.joints[i].limit.clamp(local.normalize());
            let turn = (rest[i] * local * rest[i].inverse()).normalize();
            pose.set(h, turn);
            above = (above * turn).normalize();
        }
    }
    let palm = Quat::from_mat3(&into_rest.transpose()).normalize();
    Some(HandRetarget { pose, palm })
}

/// The thumb's first joint twisted about its bone (`whole`, its turn so
/// far, points it `along`) so its next joint folds in the plane the
/// landmarks show (`next`, `last`: the next two bones' directions). Left
/// as it is when the thumb is too straight to show a plane.
fn thumb_twist(
    skeleton: &Skeleton,
    side: Side,
    rest: &[Quat],
    whole: Quat,
    along: Vec3,
    next: Option<Vec3>,
    last: Option<Vec3>,
) -> Quat {
    let (Some(next), Some(last)) = (next, last) else {
        return whole;
    };
    let Some(j) = skeleton.find(HumanJoint::Finger(side, Finger::Thumb, 1)) else {
        return whole;
    };
    // The way the thumb bends: about the axis a fold of its last joint
    // turns about (its rest curl axis, carried by the first joint). Not
    // the next joint's fold: its bones meet at an angle at rest on some
    // models, which would tilt the plane.
    let bend = next.cross(last);
    if bend.length() < 0.25 {
        return whole;
    }
    let flat = |v: Vec3| (v - along * v.dot(along)).try_normalize();
    let axis = whole * thumb_curl_axis(skeleton.rest_bone_direction(j, rest));
    let (Some(have), Some(want)) = (flat(axis), flat(bend)) else {
        return whole;
    };
    // A thumb bent back folds about the same axis the other way: the
    // least twist lining the axis up either way round (else a thumb bent
    // back a little would be twisted half a turn, clamped to its limit).
    let angle = have.cross(want).dot(along).atan2(have.dot(want));
    let angle = if angle.abs() > std::f32::consts::FRAC_PI_2 {
        angle - std::f32::consts::PI.copysign(angle)
    } else {
        angle
    };
    Quat::from_axis_angle(along, angle) * whole
}

/// A hinge joint's turn (canonical axes, after `above`, the turn of the
/// joints above it) folding its bone (`bone` at rest) towards `want`: the
/// fold about its hinge that matches `want` seen along the hinge's axis.
/// Swinging the bone straight to `want` and then clamping to the hinge
/// would fold it less whenever `want` leaves the hinge's plane, as
/// tracked points always do a little. None if the joint isn't a hinge
/// (`limit`, in its rest frame `rest`), or `want` lies along its axis.
fn hinge_fold(limit: &JointLimit, rest: Quat, above: Quat, bone: Vec3, want: Vec3) -> Option<Quat> {
    if limit.is_free() || limit.swing > 12f32.to_radians() {
        return None;
    }
    let axis = (rest * limit.axis).try_normalize()?;
    let want = above.inverse() * want;
    let flat = |v: Vec3| (v - axis * v.dot(axis)).try_normalize();
    let (have, want) = (flat(bone)?, flat(want)?);
    Some(Quat::from_axis_angle(
        axis,
        have.cross(want).dot(axis).atan2(have.dot(want)),
    ))
}

/// How many landmarks a body has.
pub const BODY_LANDMARKS: usize = 33;

/// A landmark counts as seen from this visibility.
pub const BODY_VISIBLE: f32 = 0.5;

/// Each body landmark's counterpart on the other side (BlazePose's
/// numbering: the middle ones are their own).
const BODY_MIRROR: [usize; BODY_LANDMARKS] = [
    0, 4, 5, 6, 1, 2, 3, 8, 7, 10, 9, 12, 11, 14, 13, 16, 15, 18, 17, 20, 19, 22, 21, 24, 23, 26,
    25, 28, 27, 30, 29, 32, 31,
];

/// A body the camera saw: its landmarks in metres in the camera's axes
/// (x right, y down, z away), and how likely each is seen (0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct BodyPoints {
    pub points: [Vec3; BODY_LANDMARKS],
    pub visibility: [f32; BODY_LANDMARKS],
}

impl BodyPoints {
    /// The body seen in a mirror: x negated, left and right swapped.
    pub fn mirrored(&self) -> BodyPoints {
        BodyPoints {
            points: std::array::from_fn(|i| {
                let p = self.points[BODY_MIRROR[i]];
                Vec3::new(-p.x, p.y, p.z)
            }),
            visibility: std::array::from_fn(|i| self.visibility[BODY_MIRROR[i]]),
        }
    }
}

/// A body's pose read from its landmarks: the turns of the joints that
/// were seen (canonical axes), the hips' turn (the whole body's, from
/// facing the camera) among them. Joints of limbs not seen are left out.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyRetarget {
    pub pose: Pose,
}

/// The camera's axes (x right, y down, z away) to a figure's facing the
/// viewer (x its left, y up, z towards the viewer): a half turn about x.
fn from_camera() -> Quat {
    Quat::from_rotation_x(std::f32::consts::PI)
}

/// A right-handed frame from a side-to-side direction (towards the
/// figure's left) and an upward one: (left, up, forward).
fn body_frame(across: Vec3, up: Vec3) -> Option<Mat3> {
    let x = across.try_normalize()?;
    let y = (up - x * up.dot(x)).try_normalize()?;
    Some(Mat3::from_cols(x, y, x.cross(y)))
}

/// The turn taking frame `from` to frame `to`.
fn frame_turn(to: Mat3, from: Mat3) -> Quat {
    Quat::from_mat3(&(to * from.transpose())).normalize()
}

/// `whole` (a bone's turn so far, pointing it along `along`) twisted
/// about the bone so `axis` (in rest axes) carried by it lines up with
/// `want` (both seen across the bone).
fn twist_to(whole: Quat, along: Vec3, axis: Vec3, want: Vec3) -> Quat {
    let flat = |v: Vec3| (v - along * v.dot(along)).try_normalize();
    let (Some(have), Some(want)) = (flat(whole * axis), flat(want)) else {
        return whole;
    };
    let angle = have.cross(want).dot(along).atan2(have.dot(want));
    (Quat::from_axis_angle(along, angle) * whole).normalize()
}

/// The pose being built: each joint's turn kept inside its limit, and
/// each joint's whole turn (its own and those above it), which the next
/// joint down starts from.
struct Chain<'a> {
    skeleton: &'a Skeleton,
    rest: Vec<Quat>,
    world: Vec<Option<Quat>>,
    pose: Pose,
}

impl<'a> Chain<'a> {
    fn new(skeleton: &'a Skeleton) -> Self {
        Self {
            skeleton,
            rest: skeleton.rest_rotations(),
            world: vec![None; skeleton.len()],
            pose: Pose::default(),
        }
    }

    /// The whole turn above `h`: its nearest posed ancestor's (joints a
    /// model lacks turn nothing).
    fn above(&self, h: HumanJoint) -> Quat {
        let mut at = h.parent();
        while let Some(p) = at {
            if let Some(q) = self.skeleton.find(p).and_then(|i| self.world[i]) {
                return q;
            }
            at = p.parent();
        }
        Quat::IDENTITY
    }

    /// `h`'s bone at rest, in canonical axes (None if the model lacks it).
    fn bone(&self, h: HumanJoint) -> Option<Vec3> {
        let i = self.skeleton.find(h)?;
        Some(self.skeleton.rest_bone_direction(i, &self.rest))
    }

    /// Turn `h` by `turn` (canonical axes, after the joints above it),
    /// kept inside its limit; its whole turn after.
    fn set(&mut self, h: HumanJoint, turn: Quat) -> Quat {
        let above = self.above(h);
        let Some(i) = self.skeleton.find(h) else {
            return above;
        };
        let local = self.rest[i].inverse() * turn * self.rest[i];
        let local = self.skeleton.joints[i].limit.clamp(local.normalize());
        let turn = (self.rest[i] * local * self.rest[i].inverse()).normalize();
        self.pose.set(h, turn);
        let whole = (above * turn).normalize();
        self.world[i] = Some(whole);
        whole
    }

    /// Every joint's place with the pose so far (the model at the origin).
    fn places(&self) -> Vec<Mat4> {
        let mut turns = Vec::new();
        self.pose.apply(self.skeleton, &mut turns, |_| true, false);
        let turns: Vec<Quat> = turns.iter().map(|q| Quat::from_array(*q)).collect();
        self.skeleton.world(Mat4::IDENTITY, &turns)
    }

    /// Turn `h` so its whole turn is as near `whole` as its limit allows.
    fn set_whole(&mut self, h: HumanJoint, whole: Quat) -> Quat {
        let turn = self.above(h).inverse() * whole;
        self.set(h, turn)
    }

    /// Swing `h`'s bone to point along `want` (the least turn from where
    /// the joints above leave it).
    fn swing(&mut self, h: HumanJoint, want: Vec3) -> Option<Quat> {
        let bone = self.bone(h)?;
        let above = self.above(h);
        let i = self.skeleton.find(h)?;
        if let Some(turn) = hinge_fold(
            &self.skeleton.joints[i].limit,
            self.rest[i],
            above,
            bone,
            want,
        ) {
            return Some(self.set(h, turn));
        }
        let whole = Quat::from_rotation_arc(above * bone, want) * above;
        Some(self.set_whole(h, whole))
    }
}

/// The figure (a model with `skeleton`) posed as `body` shows. `hands`
/// are the left and right hands' landmarks in the camera's axes (the hand
/// model's, read at the wrists), for the fingers and the wrists' turns;
/// without them the wrists turn as the body's knuckle points show. Limbs
/// whose landmarks aren't seen are left out (they keep their pose). None
/// if the shoulders aren't seen or the points don't make a body.
///
/// Landmarks (BlazePose's): 0 the nose, 1-3 the left eye (inner, middle,
/// outer), 4-6 the right, 7-8 the ears, 9-10 the mouth, then left before
/// right: 11-12 shoulders, 13-14 elbows, 15-16 wrists, 17-18 the little
/// fingers' knuckles, 19-20 the index fingers', 21-22 the thumbs, 23-24
/// hips, 25-26 knees, 27-28 ankles, 29-30 heels, 31-32 the feet's tips.
pub fn retarget_body(
    skeleton: &Skeleton,
    body: &BodyPoints,
    hands: [Option<&[Vec3; HAND_LANDMARKS]>; 2],
) -> Option<BodyRetarget> {
    use HumanJoint::*;
    if body.points.iter().any(|p| !p.is_finite()) || body.visibility.iter().any(|v| v.is_nan()) {
        return None;
    }
    let camera = from_camera();
    let p = |i: usize| camera * body.points[i];
    let seen = |ids: &[usize]| ids.iter().all(|&i| body.visibility[i] >= BODY_VISIBLE);
    if !seen(&[11, 12]) {
        return None;
    }
    let world = skeleton.world(Mat4::IDENTITY, &[]);
    let at = |h: HumanJoint| Some(world[skeleton.find(h)?].w_axis.truncate());
    let mid = |a: Vec3, b: Vec3| (a + b) * 0.5;

    // The torso: the model's hips and shoulders at rest, and as seen.
    let model_up = mid(at(UpperArm(Side::Left))?, at(UpperArm(Side::Right))?)
        - mid(at(UpperLeg(Side::Left))?, at(UpperLeg(Side::Right))?);
    let model_hips = body_frame(
        at(UpperLeg(Side::Left))? - at(UpperLeg(Side::Right))?,
        model_up,
    )?;
    let model_chest = body_frame(
        at(UpperArm(Side::Left))? - at(UpperArm(Side::Right))?,
        model_up,
    )?;
    let up = mid(p(11), p(12)) - mid(p(23), p(24));
    let hips_whole = frame_turn(body_frame(p(23) - p(24), up)?, model_hips);
    let chest_whole = frame_turn(body_frame(p(11) - p(12), up)?, model_chest);

    let mut chain = Chain::new(skeleton);
    // Part of a lean (from upright in the picture) goes to the spine; a
    // body lying down turns as a whole.
    let lean = Vec3::Y.angle_between(up.normalize());
    let share = 0.5 * (1.0 - ((lean.to_degrees() - 50.0) / 30.0).clamp(0.0, 1.0));
    let upright = Quat::from_rotation_arc((hips_whole * Vec3::Y).normalize(), Vec3::Y);
    let mut hips = hips_whole.slerp((upright * hips_whole).normalize(), share);
    // The rest of the torso's turn shared along the spine. A bent spine
    // carries the shoulders off the line from the hips the landmarks
    // show: the hips are turned back onto it, and the spine shared again.
    let spine: Vec<HumanJoint> = [Spine, Chest, UpperChest]
        .into_iter()
        .filter(|&h| skeleton.find(h).is_some())
        .collect();
    for pass in 0..3 {
        chain.set(Hips, hips);
        for (k, &h) in spine.iter().enumerate() {
            let left = chain.above(h).inverse() * chest_whole;
            let turn = Quat::IDENTITY.slerp(left.normalize(), 1.0 / (spine.len() - k) as f32);
            chain.set(h, turn);
        }
        if pass == 2 || spine.is_empty() {
            break;
        }
        let posed = chain.places();
        let at = |h: HumanJoint| Some(posed[skeleton.find(h)?].w_axis.truncate());
        let posed_up = mid(at(UpperArm(Side::Left))?, at(UpperArm(Side::Right))?)
            - mid(at(UpperLeg(Side::Left))?, at(UpperLeg(Side::Right))?);
        let (Some(have), Some(want)) = (posed_up.try_normalize(), up.try_normalize()) else {
            break;
        };
        hips = (Quat::from_rotation_arc(have, want) * hips).normalize();
    }

    // The head: across from ear to ear, forward from the ears to the eyes.
    if seen(&[2, 5, 7, 8]) {
        let across = p(7) - p(8);
        let forward = mid(p(2), p(5)) - mid(p(7), p(8));
        if let Some(head) = body_frame(across, forward.cross(across)) {
            let whole = frame_turn(head, Mat3::IDENTITY);
            if skeleton.find(Neck).is_some() {
                let left = chain.above(Neck).inverse() * whole;
                chain.set(Neck, Quat::IDENTITY.slerp(left.normalize(), 0.5));
            }
            chain.set_whole(Head, whole);
        }
    }

    for (k, side) in [Side::Left, Side::Right].into_iter().enumerate() {
        // The arm: shoulder, elbow, wrist, little and index knuckles.
        let [sh, el, wr, li, ix] = [11, 13, 15, 17, 19].map(|i| i + k);
        if seen(&[el]) {
            chain.set(Shoulder(side), Quat::IDENTITY);
            limb(
                &mut chain,
                [UpperArm(side), LowerArm(side)],
                [p(sh), p(el), p(wr)],
                seen(&[wr]),
                |bone| bone.cross(Vec3::Z),
                None,
            );
            if seen(&[wr]) {
                hand(
                    &mut chain,
                    side,
                    hands[k],
                    [p(wr), p(li), p(ix)],
                    seen(&[li, ix]),
                );
            }
        }
        // The leg: hip, knee, ankle, heel, toes.
        let [hp, kn, an, he, to] = [23, 25, 27, 29, 31].map(|i| i + k);
        if seen(&[kn]) {
            let foot = seen(&[he, to]).then(|| p(to) - p(he));
            limb(
                &mut chain,
                [UpperLeg(side), LowerLeg(side)],
                [p(hp), p(kn), p(an)],
                seen(&[an]),
                |bone| bone.cross(Vec3::NEG_Z),
                foot,
            );
            if seen(&[an, to]) {
                if let Some(dir) = (p(to) - p(an)).try_normalize() {
                    chain.swing(Foot(side), dir);
                }
                chain.set(Toes(side), Quat::IDENTITY);
            }
        }
    }
    Some(BodyRetarget { pose: chain.pose })
}

/// A two-bone limb (arm or leg) from its three points: the first bone
/// swung to the middle point, twisted so the hinge between them (its axis
/// at rest from the second bone, `hinge`) folds in the plane the points
/// show, then the second bone folded to the last point (if `last_seen`).
/// A limb too straight to show a plane is twisted so its front (+Z at
/// rest) faces `front`, if given, else left untwisted.
fn limb(
    chain: &mut Chain,
    [upper, lower]: [HumanJoint; 2],
    [a, b, c]: [Vec3; 3],
    last_seen: bool,
    hinge: impl Fn(Vec3) -> Vec3,
    front: Option<Vec3>,
) {
    let (Some(bone), Some(along)) = (chain.bone(upper), (b - a).try_normalize()) else {
        return;
    };
    let next = (c - b).try_normalize().filter(|_| last_seen);
    let above = chain.above(upper);
    let mut whole = (Quat::from_rotation_arc(above * bone, along) * above).normalize();
    let bend = next.map(|n| along.cross(n));
    match (bend, chain.bone(lower)) {
        (Some(bend), Some(lower_bone)) if bend.length() > 15f32.to_radians().sin() => {
            whole = twist_to(whole, along, hinge(lower_bone), bend);
        }
        _ => {
            if let Some(front) = front {
                whole = twist_to(whole, along, Vec3::Z, front);
            }
        }
    }
    chain.set_whole(upper, whole);
    if let Some(next) = next {
        chain.swing(lower, next);
    }
}

/// A hand turned to the hand model's landmarks (`points`, which also pose
/// its fingers) if there are any, else to the body's wrist and knuckle
/// points (if `knuckles_seen`).
fn hand(
    chain: &mut Chain,
    side: Side,
    points: Option<&[Vec3; HAND_LANDMARKS]>,
    [wrist, little, index]: [Vec3; 3],
    knuckles_seen: bool,
) {
    let h = HumanJoint::Hand(side);
    if let Some(hand) = points.and_then(|pts| retarget_hand(chain.skeleton, side, pts)) {
        chain.set_whole(h, (from_camera() * hand.palm).normalize());
        for (name, q) in hand.pose.joints {
            chain.pose.joints.insert(name, q);
        }
        return;
    }
    if !knuckles_seen {
        return;
    }
    // The model's hand at rest: forward to the knuckles, across towards
    // the thumb (its index and little fingers' bases if it has them, else
    // its bone and the front: palms face down at rest).
    let world = chain.skeleton.world(Mat4::IDENTITY, &[]);
    let at = |j: HumanJoint| Some(world[chain.skeleton.find(j)?].w_axis.truncate());
    let base = |f: Finger| at(HumanJoint::Finger(side, f, 0));
    let model = match (at(h), base(Finger::Index), base(Finger::Little)) {
        (Some(w), Some(i), Some(l)) => palm_frame((i + l) * 0.5 - w, i - l),
        _ => chain.bone(h).and_then(|b| palm_frame(b, Vec3::Z)),
    };
    let seen = palm_frame((index + little) * 0.5 - wrist, index - little);
    if let (Some(model), Some(seen)) = (model, seen) {
        chain.set_whole(h, frame_turn(seen, model));
    }
}

/// The One Euro filter (Casiez, Roussel and Vogel, 2012) over a few
/// values at once: a low-pass filter whose cut-off rises with speed, so
/// slow movement is smoothed and fast movement isn't delayed.
#[derive(Clone, Debug)]
pub struct OneEuro {
    /// The cut-off at rest (Hz): lower is smoother.
    pub min_cutoff: f32,
    /// How fast the cut-off rises with speed: higher lags less.
    pub beta: f32,
    /// The cut-off for the speed itself (Hz).
    pub d_cutoff: f32,
    last: Option<(f64, Vec<f32>, Vec<f32>)>,
}

impl OneEuro {
    pub fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            d_cutoff: 1.0,
            last: None,
        }
    }

    /// For hand landmarks in metres, as the Hand Scanner streams them.
    pub fn for_hands() -> Self {
        Self::new(1.0, 10.0)
    }

    /// Start over (the next value passes unfiltered).
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// `values` at `time` (seconds) filtered. A different number of
    /// values, or a time not after the last, starts over.
    pub fn filter(&mut self, time: f64, values: &[f32]) -> Vec<f32> {
        let Some((t0, x0, dx0)) = self.last.as_ref() else {
            self.last = Some((time, values.to_vec(), vec![0.0; values.len()]));
            return values.to_vec();
        };
        let dt = (time - t0) as f32;
        if x0.len() != values.len() || dt.is_nan() || dt <= 0.0 {
            self.last = None;
            return self.filter(time, values);
        }
        let alpha = |cutoff: f32| {
            let tau = 1.0 / (std::f32::consts::TAU * cutoff);
            1.0 / (1.0 + tau / dt)
        };
        let a_d = alpha(self.d_cutoff);
        let mut x = Vec::with_capacity(values.len());
        let mut dx = Vec::with_capacity(values.len());
        for ((&v, &prev), &prev_d) in values.iter().zip(x0).zip(dx0) {
            let d = prev_d + a_d * ((v - prev) / dt - prev_d);
            let a = alpha(self.min_cutoff + self.beta * d.abs());
            x.push(prev + a * (v - prev));
            dx.push(d);
        }
        self.last = Some((time, x.clone(), dx));
        x
    }

    /// For body landmarks in metres, as Pose from Camera streams them:
    /// smoother at rest than hands (a body shakes less), as quick.
    pub fn for_bodies() -> Self {
        Self::new(0.8, 6.0)
    }

    /// Landmarks filtered (see [`OneEuro::filter`]).
    pub fn filter_points<const N: usize>(&mut self, time: f64, points: &[Vec3; N]) -> [Vec3; N] {
        let flat: Vec<f32> = points.iter().flat_map(|p| p.to_array()).collect();
        let out = self.filter(time, &flat);
        std::array::from_fn(|i| Vec3::new(out[i * 3], out[i * 3 + 1], out[i * 3 + 2]))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::canvas::figure::mannequin::mannequin;
    use crate::canvas::figure::presets::{HAND_PRESETS, hand_pose, is_finger};
    use crate::canvas::figure::skeleton::TurnAngle;

    fn preset(name: &str) -> crate::canvas::figure::presets::HandShape {
        HAND_PRESETS.iter().find(|p| p.name == name).unwrap().shape
    }

    /// The joints' places with `pose` applied.
    fn posed_world(skeleton: &Skeleton, pose: &Pose) -> Vec<Mat4> {
        let mut turns = Vec::new();
        pose.apply(skeleton, &mut turns, is_finger, false);
        let turns: Vec<Quat> = turns.iter().map(|q| Quat::from_array(*q)).collect();
        skeleton.world(Mat4::IDENTITY, &turns)
    }

    /// The 21 landmarks of `side`'s hand posed `pose`, as a camera would
    /// see them: turned by `view`, moved and in its axes (y down).
    fn landmarks(skeleton: &Skeleton, side: Side, pose: &Pose, view: Quat) -> [Vec3; 21] {
        let world = posed_world(skeleton, pose);
        let at = |h: HumanJoint| world[skeleton.find(h).unwrap()].w_axis.truncate();
        let mut p = [Vec3::ZERO; 21];
        p[0] = at(HumanJoint::Hand(side));
        for (f, finger) in Finger::ALL.into_iter().enumerate() {
            for k in 0..3u8 {
                p[FINGER_BASE[f] + k as usize] = at(HumanJoint::Finger(side, finger, k));
            }
            // The tip: on along the last bone.
            let last = skeleton.find(HumanJoint::Finger(side, finger, 2)).unwrap();
            let axis = skeleton.bone_axis(last);
            p[FINGER_BASE[f] + 3] = world[last].transform_point3(axis * 0.02);
        }
        let camera = Quat::from_rotation_x(std::f32::consts::PI) * view;
        p.map(|q| camera * (q - p[0]) * 1.3 + Vec3::new(0.1, -0.2, 0.6))
    }

    /// Each fingertip's place relative to the wrist, in the hand's frame,
    /// with `pose` applied.
    fn tips(skeleton: &Skeleton, side: Side, pose: &Pose) -> Vec<Vec3> {
        let world = posed_world(skeleton, pose);
        let hand = world[skeleton.find(HumanJoint::Hand(side)).unwrap()]
            .w_axis
            .truncate();
        Finger::ALL
            .iter()
            .map(|&f| {
                let i = skeleton.find(HumanJoint::Finger(side, f, 2)).unwrap();
                world[i].transform_point3(skeleton.bone_axis(i) * 0.02) - hand
            })
            .collect()
    }

    fn views() -> [Quat; 3] {
        [
            Quat::IDENTITY,
            Quat::from_euler(glam::EulerRot::XYZ, 0.4, -1.1, 0.7),
            Quat::from_euler(glam::EulerRot::YXZ, 2.5, 0.3, -1.9),
        ]
    }

    #[test]
    fn open_fist_and_point_come_back_from_their_landmarks() {
        let model = mannequin();
        let s = &model.skeleton;
        for side in [Side::Left, Side::Right] {
            for name in ["Open", "Fist", "Point", "Relaxed", "Peace"] {
                let shape = preset(name);
                let truth = hand_pose(s, side, &shape, |_| true);
                for view in views() {
                    let points = landmarks(s, side, &truth, view);
                    let got = retarget_hand(s, side, &points).unwrap();
                    for (f, (a, b)) in tips(s, side, &got.pose)
                        .iter()
                        .zip(tips(s, side, &truth))
                        .enumerate()
                    {
                        assert!(
                            (*a - b).length() < 0.006,
                            "{side:?} {name} finger {f}: {a} vs {b}"
                        );
                    }
                    // The index's middle joint folds as far.
                    let h = HumanJoint::Finger(side, Finger::Index, 1);
                    let (a, b) = (got.pose.get(h).unwrap(), truth.get(h).unwrap());
                    assert!(
                        a.turn_angle(b) < 4f32.to_radians(),
                        "{side:?} {name}: {a} vs {b}"
                    );
                    // The palm's turn undoes the view.
                    let camera = Quat::from_rotation_x(std::f32::consts::PI) * view;
                    assert!(got.palm.turn_angle(camera) < 1f32.to_radians());
                }
            }
        }
    }

    #[test]
    fn a_fist_curls_and_an_open_hand_stays_straight() {
        let model = mannequin();
        let s = &model.skeleton;
        let side = Side::Left;
        let curl = |name: &str, finger: Finger, k: u8| {
            let truth = hand_pose(s, side, &preset(name), |_| true);
            let points = landmarks(s, side, &truth, views()[1]);
            let got = retarget_hand(s, side, &points).unwrap();
            got.pose
                .get(HumanJoint::Finger(side, finger, k))
                .unwrap()
                .turn_angle(Quat::IDENTITY)
                .to_degrees()
        };
        for finger in [Finger::Index, Finger::Middle, Finger::Ring, Finger::Little] {
            assert!((curl("Fist", finger, 1) - 100.0).abs() < 4.0);
            assert!(curl("Open", finger, 1) < 2.0);
        }
        // Pointing: the index straight, the middle finger curled.
        assert!(curl("Point", Finger::Index, 1) < 2.0);
        assert!(curl("Point", Finger::Middle, 1) > 90.0);
    }

    #[test]
    fn a_mirrored_hand_poses_the_other_side_as_a_mirror_would() {
        let model = mannequin();
        let s = &model.skeleton;
        let truth = hand_pose(s, Side::Left, &preset("Point"), |_| true);
        let points = landmarks(s, Side::Left, &truth, views()[2]);
        let left = retarget_hand(s, Side::Left, &points).unwrap();
        let right = retarget_hand(s, Side::Right, &mirror_landmarks(&points)).unwrap();
        let mirrored = left.pose.mirrored();
        assert_eq!(right.pose.joints.len(), 15);
        for (name, q) in &right.pose.joints {
            let other = Quat::from_array(mirrored.joints[name]);
            assert!(
                Quat::from_array(*q).turn_angle(other) < 1f32.to_radians(),
                "{name}"
            );
        }
        // A left hand's points read as a right hand's would turn the palm
        // over: the caller has to mirror them.
        let wrong = retarget_hand(s, Side::Right, &points).unwrap();
        assert!(wrong.pose != right.pose);
    }

    #[test]
    fn turns_stay_within_limits_and_bad_points_give_nothing() {
        let model = mannequin();
        let s = &model.skeleton;
        // Fingers bent backwards past their limits.
        let truth = hand_pose(s, Side::Left, &preset("Fist"), |_| true);
        let mut points = landmarks(s, Side::Left, &truth, Quat::IDENTITY);
        let palm = (points[9] - points[0]).cross(points[5] - points[17]);
        for &b in &FINGER_BASE[1..] {
            let out = (points[b] - points[0]).normalize();
            let back = (out - palm.normalize() * 1.2).normalize();
            for k in 1..4 {
                points[b + k] = points[b + k - 1] + back * 0.02;
            }
        }
        let got = retarget_hand(s, Side::Left, &points).unwrap();
        let rest = s.rest_rotations();
        for (name, q) in &got.pose.joints {
            let i = s.find(HumanJoint::from_name(name).unwrap()).unwrap();
            let local = rest[i].inverse() * Quat::from_array(*q) * rest[i];
            assert!(
                s.joints[i].limit.clamp(local).turn_angle(local) < 1e-3,
                "{name}"
            );
        }
        let flat = [Vec3::ZERO; 21];
        assert!(retarget_hand(s, Side::Left, &flat).is_none());
        let mut nan = points;
        nan[3].x = f32::NAN;
        assert!(retarget_hand(s, Side::Left, &nan).is_none());
    }

    #[test]
    fn the_filter_smooths_jitter_and_follows_a_step() {
        let mut filter = OneEuro::for_hands();
        // A still hand at 0.1 m with ±2 mm of noise, 30 frames a second.
        let mut noise = 12345u32;
        let mut jitter = || {
            noise = noise.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((noise >> 16) as f32 / 32768.0 - 1.0) * 0.002
        };
        let mut spread_in = 0f32;
        let mut spread_out = 0f32;
        let mut t = 0.0;
        for frame in 0..90 {
            t += 1.0 / 30.0;
            let x = 0.1 + jitter();
            let y = filter.filter(t, &[x])[0];
            if frame >= 10 {
                spread_in += (x - 0.1).powi(2);
                spread_out += (y - 0.1).powi(2);
            }
        }
        // Its shake (root mean square) under half the input's.
        assert!(
            spread_out.sqrt() < spread_in.sqrt() * 0.5,
            "{spread_out} vs {spread_in}"
        );
        // A step of 10 cm: followed within a fifth of a second.
        let mut y = 0.0;
        for _ in 0..6 {
            t += 1.0 / 30.0;
            y = filter.filter(t, &[0.2])[0];
        }
        assert!((y - 0.2).abs() < 0.005, "{y}");
        // Points filter the same, and a reset passes the next unchanged.
        filter.reset();
        let p = [Vec3::splat(0.3); 21];
        assert_eq!(filter.filter_points(t, &p), p);
    }

    use crate::canvas::figure::presets::BODY_PRESETS;
    use crate::canvas::figure::skeleton::Finger as Digit;

    pub(crate) fn body_preset(name: &str) -> Pose {
        BODY_PRESETS.iter().find(|p| p.name == name).unwrap().pose()
    }

    /// Every joint's place with `pose` applied over the rest pose.
    fn body_world(skeleton: &Skeleton, pose: &Pose) -> Vec<Mat4> {
        let mut turns = Vec::new();
        pose.apply(skeleton, &mut turns, |_| true, false);
        let turns: Vec<Quat> = turns.iter().map(|q| Quat::from_array(*q)).collect();
        skeleton.world(Mat4::IDENTITY, &turns)
    }

    /// The 33 landmarks of the figure posed `pose`, as a camera in front
    /// of it sees them (the figure turned by `view` first), round the
    /// hips' middle, as BlazePose gives them.
    pub(crate) fn body_landmarks(skeleton: &Skeleton, pose: &Pose, view: Quat) -> BodyPoints {
        use HumanJoint::*;
        let rest = skeleton.world(Mat4::IDENTITY, &[]);
        let posed = body_world(skeleton, pose);
        let at = |h: HumanJoint| posed[skeleton.find(h).unwrap()].w_axis.truncate();
        // A point riding on joint `h`, where it is at rest (canonical).
        let on = |h: HumanJoint, rest_point: Vec3| {
            let i = skeleton.find(h).unwrap();
            (posed[i] * rest[i].inverse()).transform_point3(rest_point)
        };
        let head = rest[skeleton.find(Head).unwrap()].w_axis.truncate();
        let face = |x: f32, y: f32, z: f32| on(Head, head + Vec3::new(x, y, z));
        let mut p = [Vec3::ZERO; 33];
        p[0] = face(0.0, 0.05, 0.10);
        for (k, s) in [1.0f32, -1.0].into_iter().enumerate() {
            for (j, dx) in [0.018, 0.032, 0.046].into_iter().enumerate() {
                p[1 + 3 * k + j] = face(s * dx, 0.07, 0.085);
            }
            p[7 + k] = face(s * 0.075, 0.07, 0.0);
            p[9 + k] = face(s * 0.025, 0.02, 0.09);
        }
        for (k, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            p[11 + k] = at(UpperArm(side));
            p[13 + k] = at(LowerArm(side));
            p[15 + k] = at(Hand(side));
            p[17 + k] = at(Finger(side, Digit::Little, 0));
            p[19 + k] = at(Finger(side, Digit::Index, 0));
            p[21 + k] = at(Finger(side, Digit::Thumb, 1));
            p[23 + k] = at(UpperLeg(side));
            p[25 + k] = at(LowerLeg(side));
            p[27 + k] = at(Foot(side));
            let ankle = rest[skeleton.find(Foot(side)).unwrap()].w_axis.truncate();
            p[29 + k] = on(Foot(side), ankle + Vec3::new(0.0, -0.06, -0.05));
            p[31 + k] = at(Toes(side));
        }
        let hips = (p[23] + p[24]) * 0.5;
        let camera = from_camera() * view;
        BodyPoints {
            points: p.map(|q| camera * (q - hips)),
            visibility: [0.95; 33],
        }
    }

    /// The limbs' joints, where `pose` puts them relative to the hips
    /// joint, turned by `view`.
    fn limb_places(skeleton: &Skeleton, pose: &Pose, view: Quat) -> Vec<(HumanJoint, Vec3)> {
        use HumanJoint::*;
        let world = body_world(skeleton, pose);
        let hips = world[skeleton.find(Hips).unwrap()].w_axis.truncate();
        let mut out = Vec::new();
        for side in [Side::Left, Side::Right] {
            for h in [
                LowerArm(side),
                Hand(side),
                Finger(side, Digit::Middle, 0),
                LowerLeg(side),
                Foot(side),
                Toes(side),
                UpperArm(side),
            ] {
                let at = world[skeleton.find(h).unwrap()].w_axis.truncate();
                out.push((h, view * (at - hips)));
            }
        }
        let head = world[skeleton.find(Head).unwrap()];
        out.push((Head, view * (head.w_axis.truncate() - hips)));
        out
    }

    fn assert_places_match(
        got: &[(HumanJoint, Vec3)],
        want: &[(HumanJoint, Vec3)],
        tol: f32,
        what: &str,
    ) {
        for ((h, a), (_, b)) in got.iter().zip(want) {
            assert!(
                a.distance(*b) < tol,
                "{what} {h:?}: {a} vs {b} ({} m)",
                a.distance(*b)
            );
        }
    }

    fn body_views() -> [(&'static str, Quat); 3] {
        [
            ("facing", Quat::IDENTITY),
            ("turned", Quat::from_rotation_y(0.6)),
            (
                "leaning",
                Quat::from_euler(glam::EulerRot::YXZ, -0.9, 0.25, 0.15),
            ),
        ]
    }

    #[test]
    fn a_t_pose_comes_back_as_rest() {
        let model = mannequin();
        let s = &model.skeleton;
        let seen = body_landmarks(s, &Pose::default(), Quat::IDENTITY);
        let got = retarget_body(s, &seen, [None, None]).unwrap();
        assert!(got.pose.get(HumanJoint::Hips).is_some());
        for (name, q) in &got.pose.joints {
            let angle = Quat::from_array(*q).turn_angle(Quat::IDENTITY).to_degrees();
            assert!(angle < 1.5, "{name}: {angle}");
        }
    }

    #[test]
    fn body_poses_come_back_from_their_landmarks() {
        let model = mannequin();
        let s = &model.skeleton;
        for name in [
            "Standing",
            "Sitting",
            "Reaching",
            "Walking",
            "Crouching",
            "Running",
        ] {
            let truth = body_preset(name);
            for (how, view) in body_views() {
                let seen = body_landmarks(s, &truth, view);
                let got = retarget_body(s, &seen, [None, None]).unwrap();
                // (Within a few centimetres: where along the spine a bend
                // is can't be told from the hips and shoulders alone.)
                assert_places_match(
                    &limb_places(s, &got.pose, Quat::IDENTITY),
                    &limb_places(s, &truth, view),
                    0.045,
                    &format!("{name} {how}"),
                );
                // The elbows fold as far as they did.
                for side in [Side::Left, Side::Right] {
                    let h = HumanJoint::LowerArm(side);
                    let a = got.pose.get(h).unwrap().turn_angle(Quat::IDENTITY);
                    let b = truth.get(h).unwrap_or_default().turn_angle(Quat::IDENTITY);
                    assert!(
                        (a - b).abs() < 3f32.to_radians(),
                        "{name} {how} {h:?}: {a} vs {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_raised_arm_raises_the_figures_arm() {
        let model = mannequin();
        let s = &model.skeleton;
        // The left arm straight up (from T-pose, 90° about Z), the right
        // one down by the side.
        let mut truth = body_preset("Standing");
        truth.set(HumanJoint::UpperArm(Side::Left), Quat::from_rotation_z(1.5));
        truth.set(HumanJoint::LowerArm(Side::Left), Quat::IDENTITY);
        let seen = body_landmarks(s, &truth, Quat::IDENTITY);
        let got = retarget_body(s, &seen, [None, None]).unwrap();
        let places = limb_places(s, &got.pose, Quat::IDENTITY);
        let place = |h| places.iter().find(|(j, _)| *j == h).unwrap().1;
        let hand = place(HumanJoint::Hand(Side::Left));
        let head = place(HumanJoint::Head);
        assert!(hand.y > head.y + 0.3, "{hand} {head}");
        assert!(place(HumanJoint::Hand(Side::Right)).y < 0.2);
    }

    #[test]
    fn a_mirrored_body_poses_the_mirror_image() {
        let model = mannequin();
        let s = &model.skeleton;
        let truth = body_preset("Reaching");
        let view = Quat::from_rotation_y(0.4);
        let seen = body_landmarks(s, &truth, view);
        let got = retarget_body(s, &seen.mirrored(), [None, None]).unwrap();
        // The mirror image of the truth, seen turned the other way.
        let mirror_view = Quat::from_rotation_y(-0.4);
        assert_places_match(
            &limb_places(s, &got.pose, Quat::IDENTITY),
            &limb_places(s, &truth.mirrored(), mirror_view),
            0.035,
            "mirrored",
        );
        // Mirroring twice gives the landmarks back.
        assert_eq!(seen.mirrored().mirrored(), seen);
    }

    #[test]
    fn limbs_not_seen_keep_their_pose_and_bad_points_give_nothing() {
        use HumanJoint::*;
        let model = mannequin();
        let s = &model.skeleton;
        let mut seen = body_landmarks(s, &body_preset("Walking"), Quat::IDENTITY);
        seen.visibility[13] = 0.1; // the left elbow
        seen.visibility[28] = 0.2; // the right ankle
        let got = retarget_body(s, &seen, [None, None]).unwrap().pose;
        for h in [
            Shoulder(Side::Left),
            UpperArm(Side::Left),
            LowerArm(Side::Left),
            Hand(Side::Left),
        ] {
            assert!(got.get(h).is_none(), "{h:?}");
        }
        assert!(got.get(UpperArm(Side::Right)).is_some());
        // The right thigh and knee are posed (the knee's direction is
        // seen), the shin and foot below it aren't.
        assert!(got.get(UpperLeg(Side::Right)).is_some());
        assert!(got.get(LowerLeg(Side::Right)).is_none() && got.get(Foot(Side::Right)).is_none());
        // Every turn inside its limit.
        let rest = s.rest_rotations();
        for (name, q) in &got.joints {
            let i = s.find(HumanJoint::from_name(name).unwrap()).unwrap();
            let local = rest[i].inverse() * Quat::from_array(*q) * rest[i];
            assert!(
                s.joints[i].limit.clamp(local).turn_angle(local) < 1e-3,
                "{name}"
            );
        }
        // Shoulders unseen, NaN, or everything in one place: nothing.
        let mut hidden = seen.clone();
        hidden.visibility[11] = 0.0;
        assert!(retarget_body(s, &hidden, [None, None]).is_none());
        let mut nan = seen.clone();
        nan.points[25].y = f32::NAN;
        assert!(retarget_body(s, &nan, [None, None]).is_none());
        let flat = BodyPoints {
            points: [Vec3::ZERO; 33],
            visibility: [1.0; 33],
        };
        assert!(retarget_body(s, &flat, [None, None]).is_none());
    }

    #[test]
    fn the_hand_models_landmarks_turn_the_wrist_and_pose_the_fingers() {
        use HumanJoint::*;
        let model = mannequin();
        let s = &model.skeleton;
        // Standing, the left hand bent up at the wrist and made a fist.
        let mut truth = body_preset("Standing");
        let fist = hand_pose(s, Side::Left, &preset("Fist"), |_| true);
        truth.joints.extend(fist.joints.clone());
        truth.set(
            Hand(Side::Left),
            Quat::from_euler(glam::EulerRot::XYZ, 0.3, -0.5, 0.4),
        );
        let view = Quat::from_rotation_y(0.5);
        let seen = body_landmarks(s, &truth, view);
        // The hand model's 21 points of that hand, in the camera's axes.
        let world = body_world(s, &truth);
        let at = |h: HumanJoint| world[s.find(h).unwrap()].w_axis.truncate();
        let mut hand = [Vec3::ZERO; 21];
        hand[0] = at(Hand(Side::Left));
        for (f, finger) in Digit::ALL.into_iter().enumerate() {
            for k in 0..3u8 {
                hand[FINGER_BASE[f] + k as usize] = at(HumanJoint::Finger(Side::Left, finger, k));
            }
            let last = s.find(HumanJoint::Finger(Side::Left, finger, 2)).unwrap();
            hand[FINGER_BASE[f] + 3] = world[last].transform_point3(s.bone_axis(last) * 0.02);
        }
        let camera = from_camera() * view;
        let hand = hand.map(|p| camera * (p - hand[0]));
        let got = retarget_body(s, &seen, [Some(&hand), None]).unwrap();
        // The wrist turns as it did, the fist closes.
        let places = limb_places(s, &got.pose, Quat::IDENTITY);
        let truth_places = limb_places(s, &truth, view);
        assert_places_match(&places, &truth_places, 0.03, "with the hand");
        let curl = HumanJoint::Finger(Side::Left, Digit::Middle, 1);
        assert!(
            got.pose
                .get(curl)
                .unwrap()
                .turn_angle(Quat::IDENTITY)
                .to_degrees()
                > 80.0
        );
        // The right hand has no landmarks of its own: no finger turns.
        assert!(
            got.pose
                .get(HumanJoint::Finger(Side::Right, Digit::Middle, 1))
                .is_none()
        );
        // The wrist's whole turn matches the truth's.
        let whole = |pose: &Pose, v: Quat| {
            let w = body_world(s, pose);
            let rest = s.world(Mat4::IDENTITY, &[]);
            let i = s.find(Hand(Side::Left)).unwrap();
            let r = (w[i] * rest[i].inverse()).to_scale_rotation_translation().1;
            v * r
        };
        let (a, b) = (whole(&got.pose, Quat::IDENTITY), whole(&truth, view));
        assert!(a.turn_angle(b).to_degrees() < 4.0, "{a} vs {b}");
    }

    /// The realistic hand, if Git LFS has pulled the figure pack.
    fn realistic_hand() -> Option<crate::canvas::figure::SharedModel> {
        use crate::canvas::figure::pack;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/figure-pack/v1");
        let manifest = pack::manifest(&root).ok()?;
        pack::load_from(&root, &manifest, "hand-left").ok()
    }

    /// A thumb bent back a little (as many are when straight) comes back
    /// bent back, not twisted half a turn and clamped: on the mannequin
    /// and on the realistic hand.
    #[test]
    fn a_thumb_bent_back_comes_back_from_its_landmarks() {
        let models = [Some(mannequin()), realistic_hand()];
        for model in models.into_iter().flatten() {
            let s = &model.skeleton;
            let side = Side::Left;
            let mut shape = preset("Open");
            shape[0][1] = -15.0;
            shape[0][2] = -20.0;
            let truth = hand_pose(s, side, &shape, |_| true);
            for view in views() {
                let got = retarget_hand(s, side, &landmarks(s, side, &truth, view)).unwrap();
                let (a, b) = (tips(s, side, &got.pose)[0], tips(s, side, &truth)[0]);
                assert!((a - b).length() < 0.004, "{}: {a} vs {b}", model.name);
                let h = HumanJoint::Finger(side, Finger::Thumb, 0);
                let (a, b) = (got.pose.get(h).unwrap(), truth.get(h).unwrap());
                assert!(
                    a.turn_angle(b) < 5f32.to_radians(),
                    "{}: {a} vs {b}",
                    model.name
                );
            }
        }
    }

    /// Tracked points off a finger's plane (a real hand's are) still fold
    /// its middle joint as far as they show, not less.
    #[test]
    fn points_off_a_finger_s_plane_still_fold_it_fully() {
        let models = [Some(mannequin()), realistic_hand()];
        for model in models.into_iter().flatten() {
            let s = &model.skeleton;
            let side = Side::Left;
            let truth = hand_pose(s, side, &preset("Fist"), |_| true);
            let mut points = landmarks(s, side, &truth, Quat::IDENTITY);
            // The index's last two points pushed sideways, across the palm.
            let across = (points[5] - points[17]).normalize() * 0.012;
            points[7] += across;
            points[8] += across * 1.5;
            let got = retarget_hand(s, side, &points).unwrap();
            let h = HumanJoint::Finger(side, Finger::Index, 1);
            let (a, b) = (got.pose.get(h).unwrap(), truth.get(h).unwrap());
            let (a, b) = (a.turn_angle(Quat::IDENTITY), b.turn_angle(Quat::IDENTITY));
            assert!(
                (a - b).abs() < 6f32.to_radians(),
                "{}: {a} vs {b}",
                model.name
            );
        }
    }
}
