//! Skeletal animation: a rig is bones (a tree, each placed, turned, scaled
//! and sheared from its parent), slots that hang pictures on bones in a
//! drawing order, and animations that move the bones and change the slots
//! over time. A rig layer's pixels are its rig posed at the document's
//! current frame ([`raster`]).
//!
//! Written from the published descriptions of skeletal animation formats
//! (Spine's JSON format, DragonBones', Lottie's), not from any runtime's
//! code: what they have in common is what's here. The importers are in
//! `crate::project::anim_import`.

pub mod eval;
pub mod raster;

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A 2D affine transform: `[a, b, c, d]` (x' = a·x + b·y + tx, y' = c·x +
/// d·y + ty) and its translation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub m: [f32; 4],
    pub t: [f32; 2],
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: [1.0, 0.0, 0.0, 1.0],
        t: [0.0, 0.0],
    };

    pub fn apply(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let [a, b, c, d] = self.m;
        [a * x + b * y + self.t[0], c * x + d * y + self.t[1]]
    }

    /// `self` after `inner` (inner's points, then self).
    pub fn then(&self, inner: &Affine) -> Affine {
        let [a, b, c, d] = self.m;
        let [e, f, g, h] = inner.m;
        Affine {
            m: [a * e + b * g, a * f + b * h, c * e + d * g, c * f + d * h],
            t: self.apply(inner.t),
        }
    }

    /// Translate, rotate (degrees, counter-clockwise in a y-up frame),
    /// shear (degrees, each axis's own turn) and scale, as Spine and
    /// DragonBones compose a bone's local transform.
    pub fn local(x: f32, y: f32, rotation: f32, scale: [f32; 2], shear: [f32; 2]) -> Affine {
        let rx = (rotation + shear[0]).to_radians();
        let ry = (rotation + 90.0 + shear[1]).to_radians();
        Affine {
            m: [
                rx.cos() * scale[0],
                ry.cos() * scale[1],
                rx.sin() * scale[0],
                ry.sin() * scale[1],
            ],
            t: [x, y],
        }
    }

    pub fn inverse(&self) -> Option<Affine> {
        let [a, b, c, d] = self.m;
        let det = a * d - b * c;
        if det.abs() < 1e-12 {
            return None;
        }
        let m = [d / det, -b / det, -c / det, a / det];
        let t = [
            -(m[0] * self.t[0] + m[1] * self.t[1]),
            -(m[2] * self.t[0] + m[3] * self.t[1]),
        ];
        Some(Affine { m, t })
    }
}

/// What a bone takes from its parent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Inherit {
    /// Everything.
    #[default]
    Normal,
    /// Only where it is (its turn and size are its own).
    OnlyTranslation,
    /// Not the parent's turn.
    NoRotation,
    /// Not the parent's scale.
    NoScale,
}

/// A bone in its setup pose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bone {
    pub name: String,
    /// Always before it in the rig's list.
    pub parent: Option<usize>,
    pub x: f32,
    pub y: f32,
    /// Degrees.
    pub rotation: f32,
    pub scale: [f32; 2],
    /// Degrees.
    pub shear: [f32; 2],
    /// For drawing it (and IK): how long it is along its x axis.
    pub length: f32,
    pub inherit: Inherit,
}

impl Default for Bone {
    fn default() -> Self {
        Self {
            name: String::new(),
            parent: None,
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            scale: [1.0, 1.0],
            shear: [0.0, 0.0],
            length: 0.0,
            inherit: Inherit::Normal,
        }
    }
}

/// How a slot's picture goes over what's below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotBlend {
    #[default]
    Normal,
    Additive,
    Multiply,
    Screen,
}

/// A place on a bone where one of its attachments shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slot {
    pub name: String,
    pub bone: usize,
    /// Tint and opacity, unmultiplied, 0..1.
    pub color: [f32; 4],
    /// The attachment showing in the setup pose.
    pub attachment: Option<String>,
    pub blend: SlotBlend,
    /// Its attachments, by name.
    pub attachments: Vec<Attachment>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            name: String::new(),
            bone: 0,
            color: [1.0; 4],
            attachment: None,
            blend: SlotBlend::Normal,
            attachments: Vec::new(),
        }
    }
}

/// A picture hung on a slot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// Which of the rig's images, and its part of it (pixels:
    /// `[x0, y0, x1, y1]`); `rotated` when the part is stored turned a
    /// quarter clockwise (packed atlases).
    pub image: usize,
    pub region: [f32; 4],
    pub rotated: bool,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    /// A rectangle `width`×`height` centred on `(x, y)` in the bone's
    /// frame, turned and scaled.
    Region {
        x: f32,
        y: f32,
        rotation: f32,
        scale: [f32; 2],
        width: f32,
        height: f32,
        /// The part of the rectangle the picture covers, as shares
        /// `[left, top, right, bottom]` of it seen as the picture is (packed
        /// atlases trim pictures' empty edges): `[0, 0, 1, 1]` for all.
        trim: [f32; 4],
    },
    /// Triangles of points (each at `uvs[i]`, 0..1 across the region),
    /// placed in the bone's frame or weighted between bones.
    Mesh {
        uvs: Vec<[f32; 2]>,
        triangles: Vec<[u32; 3]>,
        vertices: MeshVertices,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MeshVertices {
    /// In the slot's bone's frame.
    Plain(Vec<[f32; 2]>),
    /// Each point from several bones: (bone, its place in that bone's
    /// frame, weight).
    Weighted(Vec<Vec<(usize, [f32; 2], f32)>>),
}

/// Two bones bent to reach a target (or one bone turned to point at it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ik {
    pub name: String,
    /// One or two bones, parent first.
    pub bones: Vec<usize>,
    pub target: usize,
    /// How much of the bend is applied (0..1).
    pub mix: f32,
    /// Which way the joint bends.
    pub bend_positive: bool,
}

impl Default for Ik {
    fn default() -> Self {
        Self {
            name: String::new(),
            bones: Vec::new(),
            target: 0,
            mix: 1.0,
            bend_positive: true,
        }
    }
}

/// How a value goes from one key to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    #[default]
    Linear,
    /// Holds until the next key.
    Stepped,
    /// A cubic Bézier from (0, 0) to (1, 1) through these two control
    /// points (time, value).
    Bezier([f32; 4]),
}

/// A value at a time (seconds), and how it goes on to the next.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key<T> {
    pub time: f32,
    pub value: T,
    pub curve: Curve,
}

/// What an animation does to a bone. Rotation and translation and shear
/// are added to the setup pose; scale multiplies it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BoneTrack {
    pub bone: usize,
    pub rotate: Vec<Key<f32>>,
    pub translate: Vec<Key<[f32; 2]>>,
    pub scale: Vec<Key<[f32; 2]>>,
    pub shear: Vec<Key<[f32; 2]>>,
}

/// What an animation does to a slot: its tint and which attachment shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlotTrack {
    pub slot: usize,
    pub color: Vec<Key<[f32; 4]>>,
    pub attachment: Vec<(f32, Option<String>)>,
}

/// A mesh attachment's points moved: offsets added to its vertices (each
/// weighted vertex's offsets in turn, one per bone).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeformTrack {
    pub slot: usize,
    pub attachment: String,
    pub keys: Vec<Key<Vec<[f32; 2]>>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RigAnimation {
    pub name: String,
    /// Seconds.
    pub duration: f32,
    pub bones: Vec<BoneTrack>,
    pub slots: Vec<SlotTrack>,
    pub deforms: Vec<DeformTrack>,
}

/// A picture the attachments are cut from (premultiplied).
#[derive(Clone, Debug, PartialEq)]
pub struct RigImage {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub pixels: Arc<Vec<Color32>>,
}

/// A whole rig, and where it stands on the canvas.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rig {
    pub bones: Vec<Bone>,
    /// Bottom first.
    pub slots: Vec<Slot>,
    pub ik: Vec<Ik>,
    pub animations: Vec<RigAnimation>,
    /// The animation playing (none: the setup pose).
    pub animation: Option<usize>,
    /// The canvas point the rig's origin is at, and its size there.
    pub origin: [f32; 2],
    pub scale: f32,
    /// The rig's y points up (Spine): flipped onto the canvas.
    pub y_up: bool,
    /// Its pictures (kept beside the rest in saved files).
    #[serde(skip)]
    pub images: Vec<RigImage>,
}

impl Rig {
    /// The rig's frame to the canvas's.
    pub fn to_canvas(&self) -> Affine {
        let s = if self.scale > 0.0 { self.scale } else { 1.0 };
        Affine {
            m: [s, 0.0, 0.0, if self.y_up { -s } else { s }],
            t: self.origin,
        }
    }

    pub fn bone_index(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }

    pub fn slot_index(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.name == name)
    }
}
