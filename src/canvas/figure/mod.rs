//! 3D figure layers: posable 3D models — a jointed mannequin, realistic
//! humans, imported models — seen through a camera, as a reference to draw
//! over, their poses keyed at frames to ease between.
//!
//! Like a shader layer, a figure layer shows live on the GPU (see
//! `app::view::figure_gpu`) and its tiles hold its last *baked* render,
//! which export, merging, saving and thumbnails use like any other
//! layer's pixels. This module is GPU-free: the scene (figures, their
//! poses, the camera and light), models ([`model`]), skeletons and forward
//! kinematics ([`skeleton`]), and the projection the tool and the renderer
//! share.
//!
//! Units are metres; the scene is y-up and a figure faces +Z.

pub mod ik;
pub mod mannequin;
pub mod model;
pub mod pack;
pub mod presets;
pub mod retarget;
pub mod skeleton;

use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use glam::{Mat3, Mat4, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::canvas::rig::eval::{Lerp, sample};
use crate::canvas::rig::{Curve, Key, set_key};

use skeleton::HumanJoint;

pub use model::{FigureModel, SharedModel};

/// Which model a figure is: one that ships with the app, by id, or one
/// brought in from a file and kept in the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "StoredModelRef", into = "StoredModelRef")]
pub enum ModelRef {
    Bundled(String),
    Embedded(Arc<EmbeddedModel>),
}

/// A model kept in the document: its glTF / GLB file.
pub struct EmbeddedModel {
    pub name: String,
    /// The file (empty when read from settings that don't carry it: a
    /// project keeps it beside the layer, see `project::StoredFigure`).
    pub data: Vec<u8>,
    /// The file read, once.
    parsed: OnceLock<Result<SharedModel, String>>,
}

impl EmbeddedModel {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
            parsed: OnceLock::new(),
        }
    }
}

impl PartialEq for EmbeddedModel {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.data == other.data
    }
}

impl std::fmt::Debug for EmbeddedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedModel")
            .field("name", &self.name)
            .field("bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// How a [`ModelRef`] is written in settings: an embedded model by name
/// only (its file travels separately).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredModelRef {
    Bundled(String),
    Embedded(String),
}

impl From<StoredModelRef> for ModelRef {
    fn from(stored: StoredModelRef) -> Self {
        match stored {
            StoredModelRef::Bundled(id) => ModelRef::Bundled(id),
            StoredModelRef::Embedded(name) => {
                ModelRef::Embedded(Arc::new(EmbeddedModel::new(name, Vec::new())))
            }
        }
    }
}

impl From<ModelRef> for StoredModelRef {
    fn from(model: ModelRef) -> Self {
        match model {
            ModelRef::Bundled(id) => StoredModelRef::Bundled(id),
            ModelRef::Embedded(e) => StoredModelRef::Embedded(e.name.clone()),
        }
    }
}

impl ModelRef {
    /// The jointed mannequin that ships with the app.
    pub fn mannequin() -> Self {
        ModelRef::Bundled(mannequin::ID.into())
    }

    pub fn name(&self) -> &str {
        match self {
            ModelRef::Bundled(id) if id == mannequin::ID => "Mannequin",
            ModelRef::Bundled(id) => pack::name_of(id).unwrap_or(id),
            ModelRef::Embedded(e) => &e.name,
        }
    }

    /// The model, loaded (once: later calls share it). The realistic
    /// figures come from the figure pack (see [`pack`]).
    pub fn resolve(&self) -> Result<SharedModel, String> {
        match self {
            ModelRef::Bundled(id) if id == mannequin::ID => Ok(mannequin::mannequin()),
            ModelRef::Bundled(id) => pack::model(id),
            ModelRef::Embedded(e) => e
                .parsed
                .get_or_init(|| model::load_gltf(&e.data, &e.name).map(Arc::new))
                .clone(),
        }
    }

    /// Whether settings alone (without the document's files) carry it.
    pub fn is_bundled(&self) -> bool {
        matches!(self, ModelRef::Bundled(_))
    }

    /// Whether it's one of the realistic figures (see [`pack`]).
    pub fn is_from_pack(&self) -> bool {
        matches!(self, ModelRef::Bundled(id) if id != mannequin::ID)
    }
}

/// One figure in the scene: a model, where it stands and how it's posed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FigureInstance {
    pub model: ModelRef,
    /// Where its origin stands in the scene.
    #[serde(default)]
    pub position: [f32; 3],
    /// Its turn (a quaternion: x, y, z, w).
    #[serde(default = "identity_quat")]
    pub rotation: [f32; 4],
    #[serde(default = "one")]
    pub scale: f32,
    /// Each joint's turn from its rest pose (quaternions, by joint index;
    /// missing ones are at rest).
    #[serde(default)]
    pub pose: Vec<[f32; 4]>,
    /// Morph target weights, by index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morphs: Vec<f32>,
    /// Pinned joints (by index): they keep their place in the scene while
    /// others are posed.
    #[serde(default, alias = "locked", skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<u16>,
    /// Joints whose limits are off (by index): they turn any way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unlimited: Vec<u16>,
    /// Its proportions.
    #[serde(default, skip_serializing_if = "BodyShape::is_default")]
    pub shape: BodyShape,
}

/// A figure's proportions, each a factor (1 as the model is): parts of
/// the body made longer or shorter, bigger or slimmer, by resizing the
/// parts on their joints (see [`skeleton::Skeleton::part_scale`]). They
/// work on any model with human joints; models with morph targets also
/// have those.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BodyShape {
    pub head: f32,
    pub shoulders: f32,
    /// The trunk's length.
    pub torso: f32,
    pub arms: f32,
    pub legs: f32,
    pub hands: f32,
    /// How thick the trunk and limbs are.
    pub build: f32,
}

impl Default for BodyShape {
    fn default() -> Self {
        Self {
            head: 1.0,
            shoulders: 1.0,
            torso: 1.0,
            arms: 1.0,
            legs: 1.0,
            hands: 1.0,
            build: 1.0,
        }
    }
}

impl BodyShape {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Each factor `t` of the way to `other`'s.
    fn lerp(&self, other: &Self, t: f32) -> Self {
        let mut out = *self;
        let mut other = *other;
        for ((_, a), (_, b)) in out.fields().into_iter().zip(other.fields()) {
            *a += (*b - *a) * t;
        }
        out
    }

    /// The sliders: label, and the field.
    pub fn fields(&mut self) -> [(&'static str, &mut f32); 7] {
        [
            ("Head", &mut self.head),
            ("Shoulders", &mut self.shoulders),
            ("Torso", &mut self.torso),
            ("Arms", &mut self.arms),
            ("Legs", &mut self.legs),
            ("Hands", &mut self.hands),
            ("Build", &mut self.build),
        ]
    }

    /// Each joint's part resized (none when nothing changes).
    pub fn joint_scales(&self, skeleton: &skeleton::Skeleton) -> Vec<Mat3> {
        use HumanJoint::*;
        if self.is_default() {
            return Vec::new();
        }
        (0..skeleton.len())
            .map(|i| {
                let (along, across) = match skeleton.joints[i].human {
                    Some(Head) => (self.head, self.head),
                    Some(Shoulder(_)) => (self.shoulders, self.build),
                    Some(Spine | Chest | UpperChest) => (self.torso, self.build),
                    Some(Hips) => (1.0, self.build),
                    Some(UpperArm(_) | LowerArm(_)) => (self.arms, self.build),
                    Some(UpperLeg(_) | LowerLeg(_)) => (self.legs, self.build),
                    Some(Hand(_) | Finger(..)) => (self.hands, self.hands),
                    _ => (1.0, 1.0),
                };
                skeleton.part_scale(i, along, across)
            })
            .collect()
    }
}

fn identity_quat() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

fn one() -> f32 {
    1.0
}

impl FigureInstance {
    pub fn new(model: ModelRef) -> Self {
        Self {
            model,
            position: [0.0; 3],
            rotation: identity_quat(),
            scale: 1.0,
            pose: Vec::new(),
            morphs: Vec::new(),
            pinned: Vec::new(),
            unlimited: Vec::new(),
            shape: BodyShape::default(),
        }
    }

    /// Whether joint `j` is pinned.
    pub fn is_pinned(&self, j: usize) -> bool {
        self.pinned.contains(&(j as u16))
    }

    /// Pin joint `j`, or unpin it.
    pub fn toggle_pin(&mut self, j: usize) {
        toggle(&mut self.pinned, j);
    }

    /// Whether joint `j`'s limits are off.
    pub fn is_unlimited(&self, j: usize) -> bool {
        self.unlimited.contains(&(j as u16))
    }

    /// Turn joint `j`'s limits off, or back on.
    pub fn toggle_limits(&mut self, j: usize) {
        toggle(&mut self.unlimited, j);
    }

    /// Every joint's turn, one per joint of `model` (rest where unset).
    pub fn turns(&self, model: &FigureModel) -> Vec<Quat> {
        (0..model.skeleton.len())
            .map(|i| self.joint_pose(i))
            .collect()
    }

    /// Its parts' sizes (see [`BodyShape::joint_scales`]).
    pub fn scales(&self, model: &FigureModel) -> Vec<Mat3> {
        self.shape.joint_scales(&model.skeleton)
    }

    /// Ready to pose with inverse kinematics.
    pub fn rig<'a>(
        &self,
        model: &'a FigureModel,
        scales: &'a [Mat3],
        unlimited: &'a [usize],
        pinned: &'a [usize],
    ) -> ik::Rig<'a> {
        ik::Rig {
            skeleton: &model.skeleton,
            root: self.root_matrix(),
            scales,
            unlimited,
            pinned,
        }
    }

    /// The joints pinned and with their limits off, as indices.
    pub fn joint_sets(&self) -> (Vec<usize>, Vec<usize>) {
        let as_usize = |v: &[u16]| v.iter().map(|&j| j as usize).collect();
        (as_usize(&self.pinned), as_usize(&self.unlimited))
    }

    /// Its mesh's vertices in the scene, posed (and morphed).
    pub fn skinned_positions(&self, model: &FigureModel) -> Vec<Vec3> {
        let scales = self.scales(model);
        let world = model
            .skeleton
            .world_scaled(self.root_matrix(), &self.turns(model), &scales);
        let skin = model.skeleton.skin_scaled(&world, &scales);
        let positions = if self.morphs.iter().any(|&w| w != 0.0) {
            std::borrow::Cow::Owned(model.morphed_positions(&self.morphs))
        } else {
            std::borrow::Cow::Borrowed(&model.mesh.positions)
        };
        let mesh = &model.mesh;
        positions
            .iter()
            .enumerate()
            .map(|(v, p)| {
                let p = Vec3::from(*p);
                let (j, w) = (mesh.joints[v], mesh.weights[v]);
                (0..4)
                    .filter(|&k| w[k] != 0.0)
                    .map(|k| skin.get(j[k] as usize).map_or(p, |m| m.transform_point3(p)) * w[k])
                    .sum()
            })
            .collect()
    }

    /// How high its lowest point is above the ground (y = 0), posed.
    pub fn lowest_point(&self, model: &FigureModel) -> Option<f32> {
        self.skinned_positions(model)
            .iter()
            .map(|p| p.y)
            .min_by(f32::total_cmp)
    }

    /// Its placement in the scene.
    pub fn root_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            Vec3::splat(self.scale),
            Quat::from_array(self.rotation).normalize(),
            Vec3::from(self.position),
        )
    }

    /// Joint `i`'s turn (rest if not set).
    pub fn joint_pose(&self, i: usize) -> Quat {
        self.pose
            .get(i)
            .map_or(Quat::IDENTITY, |q| Quat::from_array(*q).normalize())
    }

    /// Set joint `i`'s turn.
    pub fn set_joint_pose(&mut self, i: usize, q: Quat) {
        if self.pose.len() <= i {
            self.pose.resize(i + 1, identity_quat());
        }
        self.pose[i] = q.normalize().to_array();
    }

    /// Its pose by canonical joint (see [`presets::Pose`]), to pose
    /// another model the same way; None if its model doesn't load.
    pub fn human_pose(&self) -> Option<presets::Pose> {
        let model = self.model.resolve().ok()?;
        Some(presets::Pose::capture(
            &model.skeleton,
            &self.turns(&model),
            |_| true,
        ))
    }

    /// Made the mannequin, for a figure whose model is missing (one of the
    /// realistic figures, opened where the figure pack isn't installed):
    /// posed by `pose` (its [`Self::human_pose`] when it was saved), else
    /// standing; where it stands and its proportions stay.
    pub fn fall_back_to_mannequin(&mut self, pose: Option<&presets::Pose>) {
        let mannequin = mannequin::mannequin();
        self.model = ModelRef::mannequin();
        self.pose = match pose {
            // (Both at rest in T-pose: joints the pose leaves out are too.)
            Some(pose) => {
                let mut turns = Vec::new();
                pose.apply(&mannequin.skeleton, &mut turns, |_| true, true);
                turns
            }
            None => mannequin::standing_pose(),
        };
        self.morphs.clear();
        self.pinned.clear();
        self.unlimited.clear();
    }

    /// Every joint's matrix in the scene, posed.
    pub fn world(&self, model: &FigureModel) -> Vec<Mat4> {
        model
            .skeleton
            .world_scaled(self.root_matrix(), &self.turns(model), &self.scales(model))
    }

    /// What a pose key keeps of it.
    pub fn keyed(&self) -> FigurePose {
        FigurePose {
            position: self.position,
            rotation: self.rotation,
            scale: self.scale,
            pose: self.pose.clone(),
            morphs: self.morphs.clone(),
            shape: self.shape,
        }
    }

    /// Posed, placed and shaped as `key` says (its pins stay).
    pub fn set_keyed(&mut self, key: &FigurePose) {
        self.position = key.position;
        self.rotation = key.rotation;
        self.scale = key.scale;
        self.pose.clone_from(&key.pose);
        self.morphs.clone_from(&key.morphs);
        self.shape = key.shape;
    }
}

/// One figure as a pose key keeps it: where it stands, its turn and size,
/// its joints' turns, morph weights and proportions (not its pins).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FigurePose {
    #[serde(default)]
    pub position: [f32; 3],
    #[serde(default = "identity_quat")]
    pub rotation: [f32; 4],
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub pose: Vec<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morphs: Vec<f32>,
    #[serde(default, skip_serializing_if = "BodyShape::is_default")]
    pub shape: BodyShape,
}

/// Turn `a` to `b` (quaternions as arrays), the shorter way.
fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let (a, b) = (Quat::from_array(a), Quat::from_array(b));
    Lerp::lerp(&a.normalize(), &b.normalize(), t).to_array()
}

impl Lerp for FigurePose {
    /// Joints turn the shorter way round (each its own turn, missing ones
    /// at rest); where it stands, its size, morphs and proportions go
    /// straight.
    fn lerp(&self, other: &Self, t: f32) -> Self {
        let joints = self.pose.len().max(other.pose.len());
        let turn = |p: &[[f32; 4]], i| p.get(i).copied().unwrap_or_else(identity_quat);
        let morphs = self.morphs.len().max(other.morphs.len());
        let weight = |m: &[f32], i| m.get(i).copied().unwrap_or(0.0);
        FigurePose {
            position: self.position.lerp(&other.position, t),
            rotation: slerp(self.rotation, other.rotation, t),
            scale: self.scale.lerp(&other.scale, t),
            pose: (0..joints)
                .map(|i| slerp(turn(&self.pose, i), turn(&other.pose, i), t))
                .collect(),
            morphs: (0..morphs)
                .map(|i| weight(&self.morphs, i).lerp(&weight(&other.morphs, i), t))
                .collect(),
            shape: self.shape.lerp(&other.shape, t),
        }
    }
}

/// A pose key: every figure of the scene (by index) as it was keyed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenePose {
    pub figures: Vec<FigurePose>,
}

impl Lerp for ScenePose {
    /// Figure by figure (one keyed only here stays as it is).
    fn lerp(&self, other: &Self, t: f32) -> Self {
        ScenePose {
            figures: (self.figures.iter().enumerate())
                .map(|(i, a)| {
                    other
                        .figures
                        .get(i)
                        .map_or_else(|| a.clone(), |b| a.lerp(b, t))
                })
                .collect(),
        }
    }
}

fn toggle(list: &mut Vec<u16>, j: usize) {
    let j = j as u16;
    match list.iter().position(|&x| x == j) {
        Some(at) => {
            list.remove(at);
        }
        None => list.push(j),
    }
}

/// The camera: it orbits `target` at `distance`, turned `yaw` about the
/// vertical and tilted `pitch` (radians; positive looks down from above).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FigureCamera {
    pub target: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// How far it's turned about the way it looks (radians).
    #[serde(default)]
    pub roll: f32,
}

impl Default for FigureCamera {
    fn default() -> Self {
        Self {
            target: [0.0, 0.9, 0.0],
            yaw: 0.0,
            pitch: 0.08,
            distance: 3.2,
            fov: 35.0,
            roll: 0.0,
        }
    }
}

/// Smallest and largest camera distance (metres).
const MIN_DISTANCE: f32 = 0.05;
const MAX_DISTANCE: f32 = 500.0;
/// The camera never quite looks straight down or up.
const MAX_PITCH: f32 = 1.55;

impl FigureCamera {
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::from(self.target) + Vec3::new(cp * sy, sp, cp * cy) * self.distance
    }

    pub fn view(&self) -> Mat4 {
        let eye = self.eye();
        let forward = (Vec3::from(self.target) - eye).normalize_or(Vec3::NEG_Z);
        let up = Quat::from_axis_angle(forward, -self.roll) * Vec3::Y;
        Mat4::look_at_rh(eye, Vec3::from(self.target), up)
    }

    /// Turn about the way it looks.
    pub fn turn(&mut self, angle: f32) {
        self.roll = (self.roll + angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
    }

    /// The projection for a picture `aspect` wide (to 1 tall), depth 0..1.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.01).max(0.001);
        let far = self.distance * 100.0 + 10.0;
        Mat4::perspective_rh(
            self.fov.clamp(1.0, 170.0).to_radians(),
            aspect.max(1e-6),
            near,
            far,
        )
    }

    /// Look at the middle of the box from `lo` to `hi`, from far enough
    /// to see all of it (keeping the camera's angle and lens).
    pub fn frame(&mut self, lo: Vec3, hi: Vec3) {
        self.target = ((lo + hi) / 2.0).into();
        let size = (hi - lo).length().max(0.01);
        let half_fov = (self.fov.clamp(1.0, 170.0).to_radians() / 2.0).tan();
        self.distance = (size * 0.6 / half_fov).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Turn round the target.
    pub fn orbit(&mut self, yaw: f32, pitch: f32) {
        self.yaw = (self.yaw + yaw).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + pitch).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Move closer (`factor` < 1) or away.
    pub fn dolly(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Slide the camera and its target sideways and up by `dx`, `dy`
    /// pixels of a picture `height` pixels tall (the scene follows).
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let per_pixel = 2.0 * self.distance * (self.fov.to_radians() / 2.0).tan() / height.max(1.0);
        let view = self.view().inverse();
        let right = view.transform_vector3(Vec3::X);
        let up = view.transform_vector3(Vec3::Y);
        let target = Vec3::from(self.target) - right * dx * per_pixel + up * dy * per_pixel;
        self.target = target.into();
    }
}

/// Vanishing points from a perspective ruler (canvas pixels, y down), to
/// set the camera from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Vanishing {
    /// Where lines going away from the viewer meet (on the horizon).
    One([f32; 2]),
    /// Two points on the horizon, where two level directions square to
    /// each other meet, and the ruler's centre of vision.
    Two([f32; 2], [f32; 2], [f32; 2]),
    /// Two on the horizon, then the uprights'.
    Three([f32; 2], [f32; 2], [f32; 2]),
}

impl FigureCamera {
    /// Turn the camera and set its lens so the scene's level axes (x, z)
    /// and uprights run to `vanishing`'s points on a `width`×`height`
    /// canvas (standard vanishing point calibration, the canvas's middle
    /// taken as the centre of vision: the camera has no lens shift). Of the
    /// four ways the scene's axes could go, the one turned least from the
    /// camera's. One point keeps the default lens and a level horizon.
    /// Its target and distance stay. False (and unchanged) if the points
    /// don't fit a camera.
    pub fn match_perspective(&mut self, vanishing: Vanishing, width: usize, height: usize) -> bool {
        match self.matched_to(vanishing, width as f32, height as f32) {
            Some(camera) => {
                *self = camera;
                true
            }
            None => false,
        }
    }

    fn matched_to(&self, vanishing: Vanishing, w: f32, h: f32) -> Option<Self> {
        use std::f32::consts::{PI, TAU};
        if w < 1.0 || h < 1.0 {
            return None;
        }
        let centre = Vec2::new(w, h) / 2.0;
        let off = |p: [f32; 2]| Vec2::from(p) - centre;
        // The way to a point on the picture, in the camera's axes (x
        // right, y up, looking down -z), `f` pixels from the eye.
        let dir = |p: [f32; 2], f: f32| {
            let o = off(p);
            Vec3::new(o.x, -o.y, -f)
        };
        // Two points whose directions are square to each other: the
        // focal length that makes them so.
        let focal = |a: [f32; 2], b: [f32; 2]| {
            let f2 = -off(a).dot(off(b));
            (f2 > 1e-3).then(|| f2.sqrt())
        };
        let (f, up, level) = match vanishing {
            Vanishing::One(v) => {
                let f = h / 2.0 / (Self::default().fov.to_radians() / 2.0).tan();
                let d = dir(v, f).normalize();
                // The horizon level: up is square to the picture's x.
                (f, Vec3::new(0.0, -d.z, d.y), d)
            }
            Vanishing::Two(a, b, c) => {
                // Both on one side of the middle: the ruler's own eye
                // distance (its centre of vision's), approximately.
                let f = focal(a, b).or_else(|| {
                    let (c, a, b) = (Vec2::from(c), Vec2::from(a), Vec2::from(b));
                    let f2 = (c - a).length() * (b - c).length();
                    (f2 > 1e-3).then(|| f2.sqrt())
                })?;
                let (da, db) = (dir(a, f), dir(b, f));
                (f, da.cross(db), da)
            }
            Vanishing::Three(a, b, c) => {
                let fs: Vec<f32> = [(a, b), (b, c), (a, c)]
                    .into_iter()
                    .filter_map(|(x, y)| focal(x, y))
                    .collect();
                if fs.is_empty() {
                    return None;
                }
                let f = fs.iter().sum::<f32>() / fs.len() as f32;
                (f, dir(c, f), dir(a, f))
            }
        };
        let mut up = up.try_normalize()?;
        if up.y < 0.0 {
            up = -up;
        }
        let level = (level - up * level.dot(up)).try_normalize()?;
        // The scene's x axis along either level direction, either way.
        let candidates = [level, -level, up.cross(level), -up.cross(level)];
        let turned = |yaw: f32| ((yaw - self.yaw + PI).rem_euclid(TAU) - PI).abs();
        let (world_from_camera, yaw) = candidates
            .into_iter()
            .map(|x| {
                let camera_from_world = Mat3::from_cols(x, up, x.cross(up));
                let world = camera_from_world.transpose();
                let forward = world * Vec3::NEG_Z;
                (world, (-forward.x).atan2(-forward.z))
            })
            .min_by(|a, b| turned(a.1).total_cmp(&turned(b.1)))?;
        let forward = world_from_camera * Vec3::NEG_Z;
        let pitch = (-forward.y).clamp(-1.0, 1.0).asin();
        if pitch.abs() > MAX_PITCH {
            return None;
        }
        // The roll: from the level camera's up to this one's, about the
        // way it looks.
        let level_up = (Vec3::Y - forward * forward.y).try_normalize()?;
        let camera_up = world_from_camera * Vec3::Y;
        let roll = -forward
            .dot(level_up.cross(camera_up))
            .atan2(level_up.dot(camera_up));
        Some(Self {
            yaw: yaw.rem_euclid(TAU),
            pitch,
            roll,
            fov: (2.0 * (h / 2.0 / f).atan()).to_degrees().clamp(1.0, 170.0),
            ..*self
        })
    }
}

/// The light, from the camera's point of view: from `azimuth` degrees
/// round to the right and `elevation` degrees up, with `ambient` light
/// everywhere (0..1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FigureLight {
    pub azimuth: f32,
    pub elevation: f32,
    pub ambient: f32,
}

impl Default for FigureLight {
    fn default() -> Self {
        Self {
            azimuth: -35.0,
            elevation: 40.0,
            ambient: 0.35,
        }
    }
}

/// How figures are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderStyle {
    /// Lit and shaded.
    #[default]
    Shaded,
    /// Two flat tones, light and shadow.
    Toon,
    /// One flat colour.
    Silhouette,
    /// Outlines round a plain fill (or none), to trace.
    LineArt,
}

impl RenderStyle {
    pub const ALL: [RenderStyle; 4] = [
        RenderStyle::Shaded,
        RenderStyle::Toon,
        RenderStyle::Silhouette,
        RenderStyle::LineArt,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RenderStyle::Shaded => "Shaded",
            RenderStyle::Toon => "Toon",
            RenderStyle::Silhouette => "Silhouette",
            RenderStyle::LineArt => "Line art",
        }
    }

    /// Its code in the renderer.
    pub fn code(self) -> u32 {
        match self {
            RenderStyle::Shaded => 0,
            RenderStyle::Toon => 1,
            RenderStyle::Silhouette => 2,
            RenderStyle::LineArt => 3,
        }
    }
}

/// A figure layer's scene: what its pixels are a picture of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FigureLayer {
    pub scene: Vec<FigureInstance>,
    #[serde(default)]
    pub camera: FigureCamera,
    #[serde(default)]
    pub light: FigureLight,
    #[serde(default)]
    pub style: RenderStyle,
    /// Line art: the outline's width in canvas pixels.
    #[serde(default = "default_outline")]
    pub outline: f32,
    /// Line art: white inside the outlines (else see-through).
    #[serde(default = "yes")]
    pub line_fill: bool,
    /// Pose keys at frames (a key's `time` is its frame, in order): the
    /// figures ease from each to the next, along its curve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key<ScenePose>>,
}

fn default_outline() -> f32 {
    3.0
}

fn yes() -> bool {
    true
}

/// Where a ray met a figure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// The figure, by index in the scene.
    pub figure: usize,
    /// The joint the part met belongs to.
    pub joint: usize,
    /// The point met, in the scene.
    pub point: Vec3,
    /// How far along the ray.
    pub distance: f32,
}

/// Where a ray from `origin` along `dir` meets triangle `a b c` (both
/// sides), as distance and the barycentric weights of `b` and `c`.
fn ray_triangle(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<(f32, f32, f32)> {
    let (e1, e2) = (b - a, c - a);
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some((t, u, v))
}

impl FigureLayer {
    /// A scene of the mannequin standing, arms down, the camera framing it.
    pub fn mannequin() -> Self {
        let mut figure = FigureInstance::new(ModelRef::mannequin());
        figure.pose = mannequin::standing_pose();
        Self {
            scene: vec![figure],
            camera: FigureCamera::default(),
            light: FigureLight::default(),
            style: RenderStyle::default(),
            outline: default_outline(),
            line_fill: true,
            keys: Vec::new(),
        }
    }

    /// Take figure `f` out of the scene, and out of the pose keys.
    pub fn remove_figure(&mut self, f: usize) {
        if f >= self.scene.len() {
            return;
        }
        self.scene.remove(f);
        for key in &mut self.keys {
            if f < key.value.figures.len() {
                key.value.figures.remove(f);
            }
        }
    }

    /// Every figure's pose as a key keeps it.
    pub fn scene_pose(&self) -> ScenePose {
        ScenePose {
            figures: self.scene.iter().map(FigureInstance::keyed).collect(),
        }
    }

    /// The figures posed as `pose` says (ones it doesn't have stay).
    pub fn set_scene_pose(&mut self, pose: &ScenePose) {
        for (instance, key) in self.scene.iter_mut().zip(&pose.figures) {
            instance.set_keyed(key);
        }
    }

    /// Key the figures' poses at `frame` (replacing a key there).
    pub fn key_pose(&mut self, frame: u32) {
        let pose = self.scene_pose();
        set_key(&mut self.keys, frame as f32, pose);
    }

    fn key_index(&self, frame: u32) -> Option<usize> {
        self.keys
            .iter()
            .position(|k| k.time.round() as u32 == frame)
    }

    /// The key at `frame`, if there is one.
    pub fn key_at(&self, frame: u32) -> Option<&Key<ScenePose>> {
        self.key_index(frame).map(|i| &self.keys[i])
    }

    /// The frames with pose keys, in order.
    pub fn key_frames(&self) -> Vec<u32> {
        self.keys.iter().map(|k| k.time.round() as u32).collect()
    }

    /// Take away the key at `frame`; whether there was one.
    pub fn remove_key(&mut self, frame: u32) -> bool {
        let Some(i) = self.key_index(frame) else {
            return false;
        };
        self.keys.remove(i);
        true
    }

    /// Move the key at `from` to `to` (not onto another key).
    pub fn move_key(&mut self, from: u32, to: u32) -> bool {
        if from == to || self.key_index(to).is_some() {
            return false;
        }
        let Some(i) = self.key_index(from) else {
            return false;
        };
        let mut key = self.keys.remove(i);
        key.time = to as f32;
        let at = (self.keys.iter())
            .position(|k| k.time > key.time)
            .unwrap_or(self.keys.len());
        self.keys.insert(at, key);
        true
    }

    /// How the key at `frame` eases to the next.
    pub fn set_key_curve(&mut self, frame: u32, curve: Curve) -> bool {
        let Some(i) = self.key_index(frame) else {
            return false;
        };
        self.keys[i].curve = curve;
        true
    }

    /// The figures' poses at frame `t` (between frames too), from the
    /// keys: None without keys.
    pub fn pose_at(&self, t: f32) -> Option<ScenePose> {
        sample(&self.keys, t)
    }

    /// Pose the figures as the keys have them at frame `t`; whether
    /// anything changed (nothing without keys).
    pub fn follow_time(&mut self, t: f32) -> bool {
        let Some(pose) = self.pose_at(t) else {
            return false;
        };
        let before = self.content_hash();
        self.set_scene_pose(&pose);
        self.content_hash() != before
    }

    /// The figure (and part of it) a ray from `origin` along `dir` meets
    /// first: the ray against every posed triangle. Skinning on the CPU
    /// is cheap for figures this size (a realistic figure's 13.5k vertices
    /// and 27k triangles take about 0.2 ms), so picking needs no GPU round
    /// trip or acceleration structure.
    pub fn pick(&self, origin: Vec3, dir: Vec3) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for (f, instance) in self.scene.iter().enumerate() {
            let Ok(model) = instance.model.resolve() else {
                continue;
            };
            let points = instance.skinned_positions(&model);
            let mesh = &model.mesh;
            for tri in mesh.indices.as_chunks::<3>().0 {
                let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| i as usize);
                let (Some(&pa), Some(&pb), Some(&pc)) =
                    (points.get(a), points.get(b), points.get(c))
                else {
                    continue;
                };
                let Some((t, u, v)) = ray_triangle(origin, dir, pa, pb, pc) else {
                    continue;
                };
                if best.is_some_and(|h| h.distance <= t) {
                    continue;
                }
                // The corner nearest the point, and its strongest joint.
                let w = [1.0 - u - v, u, v];
                let corner = [a, b, c][(0..3).max_by(|&i, &j| w[i].total_cmp(&w[j])).unwrap_or(0)];
                let weights = mesh.weights[corner];
                let k = (0..4)
                    .max_by(|&i, &j| weights[i].total_cmp(&weights[j]))
                    .unwrap_or(0);
                best = Some(Hit {
                    figure: f,
                    joint: mesh.joints[corner][k] as usize,
                    point: origin + dir * t,
                    distance: t,
                });
            }
        }
        best
    }

    /// The ray through canvas point `px` of a `width`×`height` picture:
    /// its origin (on the near plane) and direction.
    pub fn ray(&self, px: [f32; 2], width: usize, height: usize) -> (Vec3, Vec3) {
        unproject(&self.view_proj(width, height), px, width, height)
    }

    /// Whether every model is one the app ships (settings alone carry the
    /// scene).
    pub fn is_self_contained(&self) -> bool {
        self.scene.iter().all(|f| f.model.is_bundled())
    }

    /// The scene's view and projection for a `width`×`height` picture.
    pub fn view_proj(&self, width: usize, height: usize) -> Mat4 {
        let aspect = width as f32 / height.max(1) as f32;
        self.camera.projection(aspect) * self.camera.view()
    }

    /// The light's direction (towards it) in the scene.
    pub fn light_direction(&self) -> Vec3 {
        let (sa, ca) = self.light.azimuth.to_radians().sin_cos();
        let (se, ce) = self.light.elevation.to_radians().sin_cos();
        let in_view = Vec3::new(ce * sa, se, ce * ca);
        self.camera
            .view()
            .inverse()
            .transform_vector3(in_view)
            .normalize_or(Vec3::Y)
    }

    /// Everything the renderer needs this frame, for a `width`×`height`
    /// picture; figures whose model doesn't load are left out.
    pub fn render_frame(&self, width: usize, height: usize) -> RenderFrame {
        let instances = self
            .scene
            .iter()
            .filter_map(|f| {
                let model = f.model.resolve().ok()?;
                let scales = f.scales(&model);
                let world = model
                    .skeleton
                    .world_scaled(f.root_matrix(), &f.turns(&model), &scales);
                let skin = model.skeleton.skin_scaled(&world, &scales);
                Some(RenderInstance {
                    model,
                    skin,
                    morphs: f.morphs.clone(),
                })
            })
            .collect();
        RenderFrame {
            instances,
            view_proj: self.view_proj(width, height),
            light: self.light_direction(),
            ambient: self.light.ambient.clamp(0.0, 1.0),
            style: self.style,
            eye: self.camera.eye(),
            outline: self.outline.clamp(0.0, 64.0),
            line_fill: self.line_fill,
            canvas: [width, height],
        }
    }

    /// Changes whenever the picture would (but not with the canvas size).
    pub fn content_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let floats = |h: &mut std::collections::hash_map::DefaultHasher, v: &[f32]| {
            for f in v {
                f.to_bits().hash(h);
            }
        };
        for f in &self.scene {
            match &f.model {
                ModelRef::Bundled(id) => id.hash(&mut h),
                ModelRef::Embedded(e) => {
                    e.name.hash(&mut h);
                    (Arc::as_ptr(e) as usize).hash(&mut h);
                }
            }
            floats(&mut h, &f.position);
            floats(&mut h, &f.rotation);
            floats(&mut h, &[f.scale]);
            for q in &f.pose {
                floats(&mut h, q);
            }
            floats(&mut h, &f.morphs);
            let s = &f.shape;
            floats(
                &mut h,
                &[
                    s.head,
                    s.shoulders,
                    s.torso,
                    s.arms,
                    s.legs,
                    s.hands,
                    s.build,
                ],
            );
        }
        let c = &self.camera;
        floats(&mut h, &c.target);
        floats(&mut h, &[c.yaw, c.pitch, c.distance, c.fov, c.roll]);
        let l = &self.light;
        floats(&mut h, &[l.azimuth, l.elevation, l.ambient]);
        self.style.code().hash(&mut h);
        if self.style == RenderStyle::LineArt {
            floats(&mut h, &[self.outline]);
            self.line_fill.hash(&mut h);
        }
        h.finish()
    }
}

/// A point in the scene projected onto a `width`×`height` picture: its
/// pixel (y down) and depth (0 near .. 1 far), or `None` behind the camera.
pub fn project(view_proj: &Mat4, p: Vec3, width: usize, height: usize) -> Option<([f32; 2], f32)> {
    let clip = *view_proj * p.extend(1.0);
    if clip.w <= 1e-6 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some((
        [
            (ndc.x + 1.0) * 0.5 * width as f32,
            (1.0 - ndc.y) * 0.5 * height as f32,
        ],
        ndc.z,
    ))
}

/// The ray through pixel `px` (y down) of a `width`×`height` picture seen
/// through `view_proj`: its origin on the near plane, and its direction.
pub fn unproject(view_proj: &Mat4, px: [f32; 2], width: usize, height: usize) -> (Vec3, Vec3) {
    let ndc_x = px[0] / width.max(1) as f32 * 2.0 - 1.0;
    let ndc_y = 1.0 - px[1] / height.max(1) as f32 * 2.0;
    let inverse = view_proj.inverse();
    let near = inverse.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
    let far = inverse.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
    (near, (far - near).normalize_or(Vec3::NEG_Z))
}

/// One figure as the renderer takes it.
pub struct RenderInstance {
    pub model: SharedModel,
    /// Skinning matrices, one per joint.
    pub skin: Vec<Mat4>,
    pub morphs: Vec<f32>,
}

/// A figure layer's scene ready to draw.
pub struct RenderFrame {
    pub instances: Vec<RenderInstance>,
    pub view_proj: Mat4,
    /// Towards the light, in the scene.
    pub light: Vec3,
    pub ambient: f32,
    pub style: RenderStyle,
    /// Where the camera is, in the scene.
    pub eye: Vec3,
    /// Line art: outline width (canvas pixels), and whether it's filled.
    pub outline: f32,
    pub line_fill: bool,
    /// The canvas size it's drawn for.
    pub canvas: [usize; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scene_round_trips_through_settings() {
        let mut layer = FigureLayer::mannequin();
        layer.scene[0].set_joint_pose(3, Quat::from_rotation_x(0.5));
        layer.camera.orbit(0.3, 0.2);
        layer.style = RenderStyle::Toon;
        let json = serde_json::to_string(&layer).unwrap();
        let back: FigureLayer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layer);
        assert_eq!(back.content_hash(), layer.content_hash());
        // An embedded model is written by name; its file goes elsewhere.
        let embedded = ModelRef::Embedded(Arc::new(EmbeddedModel::new("prop", vec![1, 2, 3])));
        let json = serde_json::to_string(&embedded).unwrap();
        assert_eq!(json, r#"{"embedded":"prop"}"#);
        // Older settings without camera or style get the defaults.
        let old: FigureLayer = serde_json::from_str(r#"{"scene":[]}"#).unwrap();
        assert_eq!(old.camera, FigureCamera::default());
    }

    #[test]
    fn the_hash_follows_the_pose_and_camera() {
        let layer = FigureLayer::mannequin();
        let mut posed = layer.clone();
        posed.scene[0].set_joint_pose(5, Quat::from_rotation_y(0.1));
        assert_ne!(posed.content_hash(), layer.content_hash());
        let mut moved = layer.clone();
        moved.camera.dolly(0.5);
        assert_ne!(moved.content_hash(), layer.content_hash());
    }

    #[test]
    fn the_camera_frames_its_target_in_the_middle() {
        let layer = FigureLayer::mannequin();
        let vp = layer.view_proj(200, 100);
        let (px, depth) = project(&vp, Vec3::from(layer.camera.target), 200, 100).unwrap();
        assert!((px[0] - 100.0).abs() < 1e-3 && (px[1] - 50.0).abs() < 1e-3);
        assert!((0.0..1.0).contains(&depth));
        // Behind the camera: nothing.
        let behind = layer.camera.eye() * 2.0 - Vec3::from(layer.camera.target);
        assert!(project(&vp, behind, 200, 100).is_none());
        // Up in the scene is up in the picture (y down).
        let up = Vec3::from(layer.camera.target) + Vec3::Y * 0.5;
        assert!(project(&vp, up, 200, 100).unwrap().0[1] < 50.0);
    }

    #[test]
    fn orbiting_keeps_the_distance_and_panning_moves_the_target() {
        let mut camera = FigureCamera::default();
        let d = (camera.eye() - Vec3::from(camera.target)).length();
        camera.orbit(1.0, 0.3);
        let after = (camera.eye() - Vec3::from(camera.target)).length();
        assert!((d - after).abs() < 1e-4);
        camera.orbit(0.0, 10.0);
        assert!(camera.pitch <= MAX_PITCH);
        let before = camera.target;
        camera.pan(10.0, 0.0, 100.0);
        assert_ne!(camera.target, before);
        camera.dolly(1e-9);
        assert_eq!(camera.distance, MIN_DISTANCE);
    }

    #[test]
    fn the_mannequin_stands_in_the_default_view() {
        let layer = FigureLayer::mannequin();
        let frame = layer.render_frame(300, 400);
        assert_eq!(frame.instances.len(), 1);
        let model = &frame.instances[0].model;
        let (lo, hi) = model.bounds();
        assert!(hi.y - lo.y > 1.6 && hi.y - lo.y < 1.9, "{lo} {hi}");
        // Head and feet both in the picture.
        let vp = layer.view_proj(300, 400);
        for p in [Vec3::new(0.0, hi.y, 0.0), Vec3::new(0.0, lo.y, 0.0)] {
            let (px, _) = project(&vp, p, 300, 400).unwrap();
            assert!((0.0..400.0).contains(&px[1]), "{p} at {px:?}");
        }
    }

    #[test]
    fn a_ray_picks_the_part_it_meets_first() {
        let layer = FigureLayer::mannequin();
        let model = layer.scene[0].model.resolve().unwrap();
        let s = &model.skeleton;
        // Straight at the head from in front.
        let head = s.find(HumanJoint::Head).unwrap();
        let hit = layer
            .pick(Vec3::new(0.0, 1.66, 3.0), Vec3::NEG_Z)
            .expect("the head");
        assert_eq!((hit.figure, hit.joint), (0, head));
        assert!(hit.point.z > 0.05 && hit.point.z < 0.15, "{}", hit.point);
        // Past the side: nothing.
        assert!(layer.pick(Vec3::new(2.0, 1.0, 3.0), Vec3::NEG_Z).is_none());
        // A second figure in front of the first is met first.
        let mut two = layer.clone();
        let mut front = two.scene[0].clone();
        front.position = [0.0, 0.0, 1.0];
        two.scene.push(front);
        let hit = two.pick(Vec3::new(0.0, 1.66, 3.0), Vec3::NEG_Z).unwrap();
        assert_eq!(hit.figure, 1);
    }

    /// Where the lines along `dir` meet in a `w`×`h` picture through
    /// `camera`.
    fn vanishing_point(camera: &FigureCamera, dir: Vec3, w: usize, h: usize) -> [f32; 2] {
        let vp = camera.projection(w as f32 / h as f32) * camera.view();
        let clip = vp * dir.extend(0.0);
        assert!(clip.w.abs() > 1e-6, "{dir} has no vanishing point");
        let (x, y) = (clip.x / clip.w, clip.y / clip.w);
        [(x + 1.0) * 0.5 * w as f32, (1.0 - y) * 0.5 * h as f32]
    }

    fn assert_same_camera(got: &FigureCamera, want: &FigureCamera) {
        let angle = |a: f32, b: f32| {
            let d = (a - b).rem_euclid(std::f32::consts::TAU);
            d.min(std::f32::consts::TAU - d)
        };
        assert!(angle(got.yaw, want.yaw) < 1e-3, "yaw {got:?} vs {want:?}");
        assert!(
            (got.pitch - want.pitch).abs() < 1e-3,
            "pitch {got:?} vs {want:?}"
        );
        assert!(
            angle(got.roll, want.roll) < 1e-3,
            "roll {got:?} vs {want:?}"
        );
        assert!((got.fov - want.fov).abs() < 0.05, "fov {got:?} vs {want:?}");
        assert_eq!((got.target, got.distance), (want.target, want.distance));
    }

    #[test]
    fn the_camera_is_recovered_from_its_vanishing_points() {
        let (w, h) = (1200, 800);
        let want = FigureCamera {
            yaw: 0.6,
            pitch: 0.3,
            roll: 0.08,
            fov: 42.0,
            ..Default::default()
        };
        let [x, y, z] = [Vec3::X, Vec3::Y, Vec3::Z].map(|d| vanishing_point(&want, d, w, h));
        // Three points: everything.
        let mut camera = FigureCamera::default();
        assert!(camera.match_perspective(Vanishing::Three(x, z, y), w, h));
        assert_same_camera(&camera, &want);
        // The points the other way round: the same camera (the turn
        // nearest the camera's is taken).
        let mut camera = FigureCamera::default();
        assert!(camera.match_perspective(Vanishing::Three(z, x, y), w, h));
        assert_same_camera(&camera, &want);
        // Two: also, the uprights' from the horizon.
        let mut camera = FigureCamera::default();
        assert!(camera.match_perspective(Vanishing::Two(x, z, [0.0, 0.0]), w, h));
        assert_same_camera(&camera, &want);
        // Rendered, the scene's axes run to the ruler's points.
        for (d, p) in [(Vec3::X, x), (Vec3::Z, z), (Vec3::Y, y)] {
            let got = vanishing_point(&camera, d, w, h);
            assert!(
                Vec2::from(got).distance(Vec2::from(p)) < 0.5,
                "{got:?} {p:?}"
            );
        }
        // One point: the default lens, the horizon level, looking along z.
        let one = FigureCamera {
            yaw: 0.3,
            pitch: -0.2,
            ..Default::default()
        };
        let v = vanishing_point(&one, Vec3::NEG_Z, w, h);
        let mut camera = FigureCamera {
            yaw: 0.2,
            roll: 0.5,
            fov: 70.0,
            ..Default::default()
        };
        assert!(camera.match_perspective(Vanishing::One(v), w, h));
        assert_same_camera(&camera, &one);
        // Points that fit no camera: left as it was.
        let mut camera = FigureCamera::default();
        let both_left = Vanishing::Three([0.0, 400.0], [100.0, 400.0], [50.0, 400.0]);
        assert!(!camera.match_perspective(both_left, w, h));
        assert_eq!(camera, FigureCamera::default());
    }

    #[test]
    fn pose_keys_ease_between_frames() {
        let mut layer = FigureLayer::mannequin();
        let j = 3;
        layer.scene[0].set_joint_pose(j, Quat::IDENTITY);
        layer.scene[0].position = [0.0, 0.0, 0.0];
        layer.scene[0].pinned = vec![1];
        layer.key_pose(0);
        layer.scene[0].set_joint_pose(j, Quat::from_rotation_x(1.0));
        layer.scene[0].position = [2.0, 0.0, 0.0];
        layer.scene[0].shape.legs = 1.4;
        layer.scene[0].pinned.clear();
        layer.key_pose(10);
        assert_eq!(layer.key_frames(), vec![0, 10]);
        // Halfway: halfway there, pins as they are.
        assert!(layer.follow_time(5.0));
        let f = &layer.scene[0];
        assert!(f.joint_pose(j).angle_between(Quat::from_rotation_x(0.5)) < 1e-4);
        assert!((f.position[0] - 1.0).abs() < 1e-5);
        assert!((f.shape.legs - 1.2).abs() < 1e-5);
        assert!(f.pinned.is_empty(), "pins aren't keyed");
        // Held past the last key; stepped holds until the next.
        layer.follow_time(30.0);
        assert!((layer.scene[0].position[0] - 2.0).abs() < 1e-6);
        assert!(layer.set_key_curve(0, Curve::Stepped));
        layer.follow_time(9.0);
        assert_eq!(layer.scene[0].position[0], 0.0);
        // Moved, removed, and saved with the layer.
        assert!(layer.move_key(10, 20));
        assert!(!layer.move_key(20, 0), "not onto another key");
        assert_eq!(layer.key_frames(), vec![0, 20]);
        let json = serde_json::to_string(&layer).unwrap();
        let back: FigureLayer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.keys, layer.keys);
        // A figure taken out leaves the keys of the others on them.
        let mut two = layer.clone();
        two.scene.insert(0, two.scene[0].clone());
        two.key_pose(0);
        two.scene[1].position = [5.0, 0.0, 0.0];
        two.key_pose(20);
        two.remove_figure(0);
        two.follow_time(20.0);
        assert_eq!(two.scene[0].position[0], 5.0);
        assert!(two.keys.iter().all(|k| k.value.figures.len() == 1));
        assert!(layer.remove_key(0) && !layer.remove_key(0));
        // Without keys the scene doesn't follow the time.
        layer.keys.clear();
        assert!(!layer.follow_time(3.0));
    }

    #[test]
    fn rolling_the_camera_turns_the_picture() {
        let mut layer = FigureLayer::mannequin();
        let above = Vec3::from(layer.camera.target) + Vec3::Y * 0.5;
        let before = project(&layer.view_proj(200, 200), above, 200, 200)
            .unwrap()
            .0;
        layer.camera.turn(std::f32::consts::FRAC_PI_2);
        let after = project(&layer.view_proj(200, 200), above, 200, 200)
            .unwrap()
            .0;
        // Above the middle before; beside it after (clockwise: right).
        assert!(before[1] < 90.0 && (before[0] - 100.0).abs() < 1.0);
        assert!(
            after[0] > 110.0 && (after[1] - 100.0).abs() < 1.0,
            "{after:?}"
        );
        assert_ne!(
            layer.content_hash(),
            FigureLayer::mannequin().content_hash()
        );
        // Older settings without the new fields read as before.
        let old: FigureInstance =
            serde_json::from_str(r#"{"model":{"bundled":"mannequin"},"locked":[3]}"#).unwrap();
        assert_eq!(old.pinned, vec![3]);
        assert!(old.shape.is_default() && old.unlimited.is_empty());
    }
}
