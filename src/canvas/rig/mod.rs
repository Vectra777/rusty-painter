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
pub mod skin;

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
    /// The mesh points pinned by Puppet Warp (made in the app; none from
    /// an imported rig), so its pins show again at any frame.
    pub pins: Vec<u32>,
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

    /// A new bone from `head` to `tail` (rig frame, in the setup pose), a
    /// child of `parent`. Returns its index (always last).
    pub fn add_bone(
        &mut self,
        name: String,
        parent: Option<usize>,
        head: [f32; 2],
        tail: [f32; 2],
    ) -> usize {
        let parent = parent.filter(|&p| p < self.bones.len());
        let to_parent = parent
            .and_then(|p| self.setup_world()[p].inverse())
            .unwrap_or(Affine::IDENTITY);
        let (h, t) = (to_parent.apply(head), to_parent.apply(tail));
        let (dx, dy) = (t[0] - h[0], t[1] - h[1]);
        self.bones.push(Bone {
            name,
            parent,
            x: h[0],
            y: h[1],
            rotation: dy.atan2(dx).to_degrees(),
            length: (dx * dx + dy * dy).sqrt(),
            ..Default::default()
        });
        self.bones.len() - 1
    }

    /// A name for a new bone no other has.
    pub fn new_bone_name(&self) -> String {
        (1..)
            .map(|n| format!("bone {n}"))
            .find(|n| self.bone_index(n).is_none())
            .unwrap_or_default()
    }

    /// Put bone `b` (setup pose) where its head is `head` and its tail
    /// `tail` (rig frame): its children go with it.
    pub fn place_bone(&mut self, b: usize, head: [f32; 2], tail: [f32; 2]) {
        let Some(bone) = self.bones.get(b) else {
            return;
        };
        let to_parent = bone
            .parent
            .and_then(|p| self.setup_world()[p].inverse())
            .unwrap_or(Affine::IDENTITY);
        let (h, t) = (to_parent.apply(head), to_parent.apply(tail));
        let (dx, dy) = (t[0] - h[0], t[1] - h[1]);
        let bone = &mut self.bones[b];
        (bone.x, bone.y) = (h[0], h[1]);
        if dx * dx + dy * dy > 1e-6 {
            bone.rotation = dy.atan2(dx).to_degrees() - bone.shear[0];
            bone.length = (dx * dx + dy * dy).sqrt() / bone.scale[0].abs().max(1e-6);
        }
    }

    /// Take bone `b` out (not a root): its children and the pictures on it
    /// go to its parent, staying where they are; its keys go. False if it
    /// can't be.
    pub fn remove_bone(&mut self, b: usize) -> bool {
        let Some(parent) = self.bones.get(b).and_then(|x| x.parent) else {
            return false;
        };
        let world = self.setup_world();
        // From b's frame to its parent's.
        let Some(into_parent) = world[parent].inverse().map(|inv| inv.then(&world[b])) else {
            return false;
        };
        let linear = Affine {
            t: [0.0, 0.0],
            ..into_parent
        };
        // Children: the same place in the world, from the parent's frame.
        for c in 0..self.bones.len() {
            if self.bones[c].parent != Some(b) {
                continue;
            }
            let local = world[parent]
                .inverse()
                .map_or(Affine::IDENTITY, |inv| inv.then(&world[c]));
            let bone = &mut self.bones[c];
            bone.parent = Some(parent);
            (bone.x, bone.y, bone.rotation, bone.scale, bone.shear) = decompose(&local);
        }
        // Pictures on it: their points (and deforms) in the parent's frame.
        for (s, slot) in self.slots.iter_mut().enumerate() {
            let plain_on_b = slot.bone == b;
            if plain_on_b {
                slot.bone = parent;
            }
            for a in &mut slot.attachments {
                match &mut a.shape {
                    Shape::Mesh {
                        vertices: MeshVertices::Plain(v),
                        ..
                    } if plain_on_b => {
                        v.iter_mut().for_each(|p| *p = into_parent.apply(*p));
                        for anim in &mut self.animations {
                            for track in anim
                                .deforms
                                .iter_mut()
                                .filter(|t| t.slot == s && t.attachment == a.name)
                            {
                                for key in &mut track.keys {
                                    key.value.iter_mut().for_each(|d| *d = linear.apply(*d));
                                }
                            }
                        }
                    }
                    Shape::Region {
                        x,
                        y,
                        rotation,
                        scale,
                        ..
                    } if plain_on_b => {
                        let local =
                            into_parent.then(&Affine::local(*x, *y, *rotation, *scale, [0.0, 0.0]));
                        let (nx, ny, nr, ns, _) = decompose(&local);
                        (*x, *y, *rotation, *scale) = (nx, ny, nr, ns);
                    }
                    Shape::Mesh {
                        vertices: MeshVertices::Weighted(v),
                        ..
                    } => {
                        // Each influence of b, through the list (the deform
                        // offsets follow the same order).
                        let mut k = 0;
                        let mut moved = Vec::new();
                        for influences in v.iter_mut() {
                            for inf in influences.iter_mut() {
                                if inf.0 == b {
                                    inf.0 = parent;
                                    inf.1 = into_parent.apply(inf.1);
                                    moved.push(k);
                                }
                                k += 1;
                            }
                        }
                        for anim in &mut self.animations {
                            for track in anim
                                .deforms
                                .iter_mut()
                                .filter(|t| t.slot == s && t.attachment == a.name)
                            {
                                for key in &mut track.keys {
                                    for &k in &moved {
                                        if let Some(d) = key.value.get_mut(k) {
                                            *d = linear.apply(*d);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Every index past b one less.
        let shift = |i: usize| if i > b { i - 1 } else { i };
        self.bones.remove(b);
        for bone in &mut self.bones {
            bone.parent = bone.parent.map(shift);
        }
        for slot in &mut self.slots {
            slot.bone = shift(slot.bone);
            for a in &mut slot.attachments {
                if let Shape::Mesh {
                    vertices: MeshVertices::Weighted(v),
                    ..
                } = &mut a.shape
                {
                    v.iter_mut().flatten().for_each(|inf| inf.0 = shift(inf.0));
                }
            }
        }
        for anim in &mut self.animations {
            anim.bones.retain(|t| t.bone != b);
            anim.bones.iter_mut().for_each(|t| t.bone = shift(t.bone));
        }
        self.ik
            .retain(|ik| ik.target != b && !ik.bones.contains(&b));
        for ik in &mut self.ik {
            ik.target = shift(ik.target);
            ik.bones.iter_mut().for_each(|x| *x = shift(*x));
        }
        true
    }
}

/// The values `Affine::local` makes `a` from (no shear on x).
pub fn decompose(a: &Affine) -> (f32, f32, f32, [f32; 2], [f32; 2]) {
    let [m0, m1, m2, m3] = a.m;
    let rot = m2.atan2(m0).to_degrees();
    let sx = (m0 * m0 + m2 * m2).sqrt();
    let sy = (m1 * m1 + m3 * m3).sqrt();
    let y_angle = m3.atan2(m1).to_degrees();
    let shear_y = ((y_angle - rot - 90.0) + 180.0).rem_euclid(360.0) - 180.0;
    (a.t[0], a.t[1], rot, [sx, sy], [0.0, shear_y])
}

/// Put `value` into `keys` at `time`: the key there changed, or a new one
/// (linear) in its place in time.
pub fn set_key<T>(keys: &mut Vec<Key<T>>, time: f32, value: T) {
    match keys.iter().position(|k| (k.time - time).abs() < 1e-4) {
        Some(i) => keys[i].value = value,
        None => {
            let at = keys
                .iter()
                .position(|k| k.time > time)
                .unwrap_or(keys.len());
            keys.insert(
                at,
                Key {
                    time,
                    value,
                    curve: Curve::Linear,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn a_bone_goes_where_it_was_drawn() {
        let mut rig = Rig {
            bones: vec![Bone {
                name: "root".into(),
                x: 10.0,
                y: 10.0,
                rotation: 90.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let a = rig.add_bone("a".into(), Some(0), [20.0, 10.0], [20.0, 40.0]);
        let b = rig.add_bone("b".into(), Some(a), [20.0, 40.0], [50.0, 40.0]);
        let w = rig.setup_world();
        assert!(close(w[a].t, [20.0, 10.0]), "{:?}", w[a].t);
        assert!(close(w[a].apply([rig.bones[a].length, 0.0]), [20.0, 40.0]));
        assert!(close(w[b].apply([rig.bones[b].length, 0.0]), [50.0, 40.0]));
        // Its head moved: the child goes with it.
        rig.place_bone(a, [0.0, 0.0], [0.0, 30.0]);
        let w = rig.setup_world();
        assert!(close(w[b].t, [0.0, 30.0]), "{:?}", w[b].t);
    }

    #[test]
    fn a_removed_bone_leaves_its_children_and_pictures_where_they_were() {
        let mut rig = Rig {
            bones: vec![Bone {
                name: "root".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let a = rig.add_bone("a".into(), Some(0), [10.0, 0.0], [10.0, 20.0]);
        let b = rig.add_bone("b".into(), Some(a), [10.0, 20.0], [30.0, 20.0]);
        let c = rig.add_bone("c".into(), Some(b), [30.0, 20.0], [30.0, 0.0]);
        rig.bones[a].scale = [2.0, 1.0];
        let before = rig.setup_world();
        rig.slots.push(Slot {
            bone: a,
            attachments: vec![Attachment {
                name: "m".into(),
                image: 0,
                region: [0.0; 4],
                rotated: false,
                shape: Shape::Mesh {
                    uvs: vec![[0.0; 2]; 2],
                    triangles: vec![],
                    vertices: MeshVertices::Plain(vec![[1.0, 2.0], [3.0, -1.0]]),
                },
            }],
            ..Default::default()
        });
        rig.animations.push(RigAnimation {
            bones: vec![
                BoneTrack {
                    bone: a,
                    ..Default::default()
                },
                BoneTrack {
                    bone: c,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let pts = |rig: &Rig| {
            let pose = rig.setup_pose();
            rig.attachment_mesh(&pose, 0, 0, false).unwrap().0
        };
        let at_first = pts(&rig);
        assert!(rig.remove_bone(a));
        assert!(!rig.remove_bone(0), "not the root");
        let after = rig.setup_world();
        assert_eq!(rig.bones.len(), 3);
        assert_eq!(rig.bones[1].parent, Some(0));
        assert_eq!(rig.bones[2].parent, Some(1));
        assert!(close(after[1].t, before[b].t));
        assert!(close(after[2].t, before[c].t));
        assert!(close(
            after[2].apply([20.0, 0.0]),
            before[c].apply([20.0, 0.0])
        ));
        assert_eq!(rig.slots[0].bone, 0);
        for (p, q) in pts(&rig).iter().zip(&at_first) {
            assert!(close(*p, *q), "{p:?} {q:?}");
        }
        assert_eq!(rig.animations[0].bones.len(), 1);
        assert_eq!(rig.animations[0].bones[0].bone, 2);
    }
}
