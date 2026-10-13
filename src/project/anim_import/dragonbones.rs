//! DragonBones' JSON (5.x), from its published data format description:
//! the first armature's bones, slots, the first skin's image and mesh
//! displays (weighted meshes bound through their bind poses), IK, and its
//! animations (bone translate, rotate and scale frames; slot display and
//! colour frames), timed in frames at its frame rate. Pictures come from
//! the texture atlas (`<name>_tex.json` and its picture).
//!
//! DragonBones' frame is y-down, as the canvas's; a bone's `skY` is its
//! turn and `skX` its y axis's.
//!
//! Left out, with a note: other armatures, nested armatures, FFD (mesh
//! deform) frames, easing other than linear or a curve, events.

use super::{ImportedRig, Siblings, decode_picture, num};
use crate::canvas::rig::{
    Affine, Attachment, Bone, BoneTrack, Curve, Ik, Key, MeshVertices, Rig, RigAnimation, Shape,
    Slot, SlotTrack,
};
use serde_json::Value;
use std::collections::HashMap;

/// A texture atlas sub-picture: its rectangle as packed and trim shares.
struct SubTexture {
    rect: [f32; 4],
    rotated: bool,
    trim: [f32; 4],
    size: [f32; 2],
}

/// A DragonBones matrix `[a, b, c, d, tx, ty]` (x' = a·x + c·y + tx,
/// y' = b·x + d·y + ty) as ours.
fn matrix(m: &[f32]) -> Affine {
    Affine {
        m: [m[0], m[2], m[1], m[3]],
        t: [m[4], m[5]],
    }
}

/// A `transform` object: (x, y, rotation, scale, shear) for
/// [`Affine::local`].
fn transform(t: Option<&Value>) -> (f32, f32, f32, [f32; 2], [f32; 2]) {
    let Some(t) = t else {
        return (0.0, 0.0, 0.0, [1.0, 1.0], [0.0, 0.0]);
    };
    let (sk_x, sk_y) = (num(t, "skX", 0.0), num(t, "skY", 0.0));
    (
        num(t, "x", 0.0),
        num(t, "y", 0.0),
        sk_y,
        [num(t, "scX", 1.0), num(t, "scY", 1.0)],
        [0.0, sk_x - sk_y],
    )
}

/// A colour transform's multipliers (percent) as a tint.
fn color(v: Option<&Value>) -> [f32; 4] {
    let Some(v) = v else {
        return [1.0; 4];
    };
    ["rM", "gM", "bM", "aM"].map(|k| num(v, k, 100.0) / 100.0)
}

/// A frame's curve: none is a hold, `tweenEasing` a straight line, `curve`
/// a Bézier.
fn curve(f: &Value) -> Curve {
    if let Some(c) = f.get("curve").and_then(Value::as_array) {
        let n: Vec<f32> = c
            .iter()
            .filter_map(Value::as_f64)
            .map(|v| v as f32)
            .collect();
        if n.len() >= 4 {
            // (A sampled curve's first and last control points.)
            return Curve::Bezier([n[0], n[1], n[n.len() - 2], n[n.len() - 1]]);
        }
    }
    if f.get("tweenEasing").is_some_and(|e| !e.is_null()) {
        Curve::Linear
    } else {
        Curve::Stepped
    }
}

/// A frame list's keys: each frame lasts `duration` frames; `read` gives
/// its value.
fn frames<T>(list: Option<&Value>, fps: f32, read: impl Fn(&Value) -> T) -> (Vec<Key<T>>, f32) {
    let mut time = 0.0;
    let keys = (list.and_then(Value::as_array).into_iter().flatten())
        .map(|f| {
            let key = Key {
                time: time / fps,
                value: read(f),
                curve: curve(f),
            };
            time += num(f, "duration", 1.0).max(0.0);
            key
        })
        .collect();
    (keys, time / fps)
}

pub fn import(json: &[u8], stem: &str, siblings: &dyn Siblings) -> Result<ImportedRig, String> {
    let doc: Value =
        serde_json::from_slice(json).map_err(|e| format!("Not DragonBones JSON: {e}"))?;
    let armatures = doc
        .get("armature")
        .and_then(Value::as_array)
        .ok_or("No armature")?;
    let arm = armatures.first().ok_or("No armature")?;
    let mut notes = Vec::new();
    if armatures.len() > 1 {
        notes.push("Only the first armature came across".into());
    }
    let fps = num(arm, "frameRate", num(&doc, "frameRate", 24.0)).max(1.0);
    let mut rig = Rig {
        scale: 1.0,
        ..Default::default()
    };

    // The texture atlas.
    let tex_name = [format!("{stem}_tex.json"), "texture.json".to_string()]
        .into_iter()
        .find(|n| siblings.read(n).is_some())
        .or_else(|| {
            (siblings.with_extension("json").into_iter()).find(|n| n.ends_with("_tex.json"))
        })
        .ok_or("The DragonBones texture atlas (_tex.json) is missing")?;
    let tex: Value = serde_json::from_slice(&siblings.read(&tex_name).unwrap_or_default())
        .map_err(|e| format!("{tex_name}: {e}"))?;
    let picture = tex
        .get("imagePath")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| tex_name.replace(".json", ".png"));
    let bytes = siblings
        .read(&picture)
        .ok_or_else(|| format!("The atlas picture {picture} is missing"))?;
    rig.images.push(decode_picture(&picture, &bytes, false)?);
    let mut subs: HashMap<String, SubTexture> = HashMap::new();
    for s in tex
        .get("SubTexture")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = s.get("name").and_then(Value::as_str) else {
            continue;
        };
        let (x, y, w, h) = (
            num(s, "x", 0.0),
            num(s, "y", 0.0),
            num(s, "width", 0.0),
            num(s, "height", 0.0),
        );
        let rotated = s.get("rotated").and_then(Value::as_bool).unwrap_or(false);
        let (fx, fy) = (num(s, "frameX", 0.0), num(s, "frameY", 0.0));
        let (fw, fh) = (
            num(s, "frameWidth", w).max(1.0),
            num(s, "frameHeight", h).max(1.0),
        );
        let (pw, ph) = if rotated { (h, w) } else { (w, h) };
        subs.insert(
            name.to_string(),
            SubTexture {
                rect: [x, y, x + pw, y + ph],
                rotated,
                trim: [-fx / fw, -fy / fh, (-fx + w) / fw, (-fy + h) / fh],
                size: [fw, fh],
            },
        );
    }

    // Bones.
    for b in arm
        .get("bone")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (x, y, rotation, scale, shear) = transform(b.get("transform"));
        rig.bones.push(Bone {
            name: b
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            parent: b
                .get("parent")
                .and_then(Value::as_str)
                .and_then(|p| rig.bone_index(p)),
            x,
            y,
            rotation,
            scale,
            shear,
            length: num(b, "length", 0.0),
            ..Default::default()
        });
    }
    if rig.bones.is_empty() {
        return Err("The DragonBones armature has no bones".into());
    }

    // Slots, and the first skin's displays (by index).
    let mut display_index: Vec<i64> = Vec::new();
    for s in arm
        .get("slot")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        rig.slots.push(Slot {
            name: s
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            bone: s
                .get("parent")
                .and_then(Value::as_str)
                .and_then(|b| rig.bone_index(b))
                .unwrap_or(0),
            color: color(s.get("color")),
            ..Default::default()
        });
        display_index.push(s.get("displayIndex").and_then(Value::as_i64).unwrap_or(0));
    }
    let skins = arm.get("skin").and_then(Value::as_array);
    if skins.is_some_and(|s| s.len() > 1) {
        notes.push("Only the first skin came across".into());
    }
    let skin = skins.and_then(|s| s.first());
    for s in skin
        .and_then(|s| s.get("slot"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(si) = s
            .get("name")
            .and_then(Value::as_str)
            .and_then(|n| rig.slot_index(n))
        else {
            continue;
        };
        for (k, d) in (s
            .get("display")
            .and_then(Value::as_array)
            .into_iter()
            .flatten())
        .enumerate()
        {
            let name = d
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let path = d
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_string();
            let kind = d.get("type").and_then(Value::as_str).unwrap_or("image");
            let Some(sub) = subs.get(&path) else {
                notes.push(format!("{name}: its picture isn't in the atlas"));
                continue;
            };
            let shape = match kind {
                "image" => {
                    let (x, y, rotation, scale, _) = transform(d.get("transform"));
                    Shape::Region {
                        x,
                        y,
                        rotation,
                        scale,
                        width: sub.size[0],
                        height: sub.size[1],
                        trim: sub.trim,
                    }
                }
                "mesh" => match mesh(d, &rig) {
                    Some(m) => m,
                    None => {
                        notes.push(format!("{name}: a damaged mesh"));
                        continue;
                    }
                },
                other => {
                    notes.push(format!(
                        "{name}: DragonBones' {other} displays are left out"
                    ));
                    continue;
                }
            };
            // Displays are named by their place: attachments too.
            rig.slots[si].attachments.push(Attachment {
                name: k.to_string(),
                image: 0,
                region: sub.rect,
                rotated: sub.rotated,
                shape,
            });
        }
        let shown = display_index[si];
        rig.slots[si].attachment = (shown >= 0).then(|| shown.to_string());
    }

    // IK: the end bone and as many parents as its chain says.
    for k in arm
        .get("ik")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(end) = k
            .get("bone")
            .and_then(Value::as_str)
            .and_then(|b| rig.bone_index(b))
        else {
            continue;
        };
        let Some(target) = k
            .get("target")
            .and_then(Value::as_str)
            .and_then(|t| rig.bone_index(t))
        else {
            continue;
        };
        let bones = match (num(k, "chain", 0.0) >= 1.0, rig.bones[end].parent) {
            (true, Some(p)) => vec![p, end],
            _ => vec![end],
        };
        rig.ik.push(Ik {
            name: k
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            bones,
            target,
            mix: num(k, "weight", 1.0),
            bend_positive: k
                .get("bendPositive")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        });
    }

    // Animations.
    for a in arm
        .get("animation")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = a
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut anim = RigAnimation {
            name: name.clone(),
            duration: num(a, "duration", 0.0) / fps,
            ..Default::default()
        };
        for t in a
            .get("bone")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(bone) = t
                .get("name")
                .and_then(Value::as_str)
                .and_then(|b| rig.bone_index(b))
            else {
                continue;
            };
            let (translate, _) = frames(t.get("translateFrame"), fps, |f| {
                [num(f, "x", 0.0), num(f, "y", 0.0)]
            });
            let (rotate, _) = frames(t.get("rotateFrame"), fps, |f| num(f, "rotate", 0.0));
            let (scale, _) = frames(t.get("scaleFrame"), fps, |f| {
                [num(f, "x", 1.0), num(f, "y", 1.0)]
            });
            if t.get("frame").is_some() {
                notes.push(format!(
                    "{name}: DragonBones 4's frames are left out (5.x's come across)"
                ));
            }
            anim.bones.push(BoneTrack {
                bone,
                rotate,
                translate,
                scale,
                shear: Vec::new(),
            });
        }
        for t in a
            .get("slot")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(slot) = t
                .get("name")
                .and_then(Value::as_str)
                .and_then(|s| rig.slot_index(s))
            else {
                continue;
            };
            let (color_keys, _) = frames(t.get("colorFrame"), fps, |f| color(f.get("value")));
            let (display, _) = frames(t.get("displayFrame"), fps, |f| num(f, "value", 0.0));
            anim.slots.push(SlotTrack {
                slot,
                color: color_keys,
                attachment: display
                    .into_iter()
                    .map(|k| {
                        (
                            k.time,
                            (k.value >= 0.0).then(|| (k.value as i64).to_string()),
                        )
                    })
                    .collect(),
            });
        }
        if a.get("ffd").is_some() {
            notes.push(format!("{name}: mesh deform (FFD) frames are left out"));
        }
        rig.animations.push(anim);
    }
    rig.animation = (!rig.animations.is_empty()).then_some(0);
    let bounds = arm.get("aabb").map(|b| {
        [
            num(b, "x", 0.0),
            num(b, "y", 0.0),
            num(b, "width", 0.0),
            num(b, "height", 0.0),
        ]
    });
    notes.sort();
    notes.dedup();
    Ok(ImportedRig {
        rig,
        bounds: bounds.filter(|b| b[2] > 0.0 && b[3] > 0.0),
        notes,
        fps: Some(fps.round() as u32),
    })
}

/// A mesh display: plain points (in its slot's bone's frame) or weighted
/// ones bound by `bonePose` (each point placed by `slotPose`).
fn mesh(d: &Value, rig: &Rig) -> Option<Shape> {
    let floats = |key: &str| -> Vec<f32> {
        (d.get(key).and_then(Value::as_array).into_iter().flatten())
            .filter_map(Value::as_f64)
            .map(|v| v as f32)
            .collect()
    };
    let (v, uv, tri) = (floats("vertices"), floats("uvs"), floats("triangles"));
    if v.len() != uv.len() || v.len() % 2 == 1 || tri.len() % 3 != 0 {
        return None;
    }
    let n = v.len() / 2;
    let uvs: Vec<[f32; 2]> = uv.chunks(2).map(|c| [c[0], c[1]]).collect();
    let triangles: Vec<[u32; 3]> = tri
        .chunks(3)
        .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
        .collect();
    let points: Vec<[f32; 2]> = v.chunks(2).map(|c| [c[0], c[1]]).collect();
    let weights = floats("weights");
    let vertices = if weights.is_empty() {
        MeshVertices::Plain(points)
    } else {
        let slot_pose = floats("slotPose");
        let slot = if slot_pose.len() == 6 {
            matrix(&slot_pose)
        } else {
            Affine::IDENTITY
        };
        // Each bone's bind pose (its inverse takes points into its frame).
        let bone_pose = floats("bonePose");
        let mut bind: HashMap<usize, Affine> = HashMap::new();
        for c in bone_pose.chunks(7).filter(|c| c.len() == 7) {
            bind.insert(c[0] as usize, matrix(&c[1..]).inverse()?);
        }
        let mut out = Vec::with_capacity(n);
        let mut i = 0;
        for p in &points {
            let count = *weights.get(i)? as usize;
            i += 1;
            if count > 64 || i + count * 2 > weights.len() {
                return None;
            }
            let at = slot.apply(*p);
            let influences = (0..count)
                .map(|k| {
                    let bone = (weights[i + k * 2] as usize).min(rig.bones.len().saturating_sub(1));
                    let local = bind.get(&bone).map_or(at, |inv| inv.apply(at));
                    (bone, local, weights[i + k * 2 + 1])
                })
                .collect();
            i += count * 2;
            out.push(influences);
        }
        MeshVertices::Weighted(out)
    };
    Some(Shape::Mesh {
        uvs,
        triangles,
        vertices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::anim_import::InMemory;
    use eframe::egui::Color32;

    const TEX: &str = r#"{ "name": "arm", "imagePath": "arm_tex.png", "SubTexture": [
        { "name": "red", "x": 0, "y": 0, "width": 8, "height": 8 },
        { "name": "blue", "x": 8, "y": 0, "width": 8, "height": 8 }
    ] }"#;

    /// The same arm as the Spine test, y down: the forearm turns down a
    /// quarter over 24 frames.
    const ARMATURE: &str = r#"{ "frameRate": 24, "name": "arm", "version": "5.5", "armature": [ {
        "type": "Armature", "name": "arm", "frameRate": 24,
        "aabb": { "x": -4, "y": -4, "width": 24, "height": 8 },
        "bone": [
            { "name": "root" },
            { "name": "upper", "parent": "root", "length": 8 },
            { "name": "lower", "parent": "upper", "length": 8, "transform": { "x": 8 } }
        ],
        "slot": [
            { "name": "upper", "parent": "upper" },
            { "name": "lower", "parent": "lower", "color": { "aM": 50 } }
        ],
        "skin": [ { "slot": [
            { "name": "upper", "display": [ { "name": "red", "transform": { "x": 4 } } ] },
            { "name": "lower", "display": [ { "name": "blue", "transform": { "x": 4 } } ] }
        ] } ],
        "animation": [ { "name": "bend", "duration": 24, "bone": [ { "name": "lower",
            "rotateFrame": [ { "duration": 24, "tweenEasing": 0, "rotate": 0 }, { "duration": 0, "rotate": 90 } ] } ] } ]
    } ] }"#;

    fn files() -> InMemory {
        InMemory(vec![
            ("arm_tex.json".into(), TEX.as_bytes().to_vec()),
            (
                "arm_tex.png".into(),
                crate::project::anim_import::spine::tests::page_png(),
            ),
        ])
    }

    #[test]
    fn a_dragonbones_armature_comes_across_posed_and_animated() {
        let imported = import(ARMATURE.as_bytes(), "arm", &files()).unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        assert_eq!(imported.fps, Some(24));
        let mut rig = imported.rig;
        assert_eq!(rig.animations[0].duration, 1.0);
        rig.origin = [32.0, 32.0];
        rig.scale = 2.0;
        let tiles = rig.render(&rig.pose(0.0), 64, 64, 64);
        let px = |tiles: &[((i32, i32), Vec<Color32>)], x: usize, y: usize| tiles[0].1[y * 64 + x];
        assert_eq!(px(&tiles, 38, 32), Color32::RED);
        let blue = px(&tiles, 54, 32);
        assert!(
            blue.b() > 100 && (120..=135).contains(&blue.a()),
            "half transparent blue: {blue:?}"
        );
        // Nearly at the end: turned a quarter, y down: below the elbow.
        let tiles = rig.render(&rig.pose(0.99), 64, 64, 64);
        assert_eq!(px(&tiles, 54, 26).a(), 0, "not where it was");
        assert!(px(&tiles, 49, 44).a() > 100, "{:?}", px(&tiles, 49, 44));
    }

    #[test]
    fn weighted_meshes_bind_through_their_poses() {
        let rig = Rig {
            bones: vec![
                Bone::default(),
                Bone {
                    x: 10.0,
                    parent: Some(0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let d = serde_json::json!({
            "type": "mesh", "uvs": [0, 0, 1, 0, 1, 1], "triangles": [0, 1, 2],
            "vertices": [0, 0, 10, 0, 10, 10],
            "weights": [1, 0, 1, 1, 1, 1, 2, 0, 0.5, 1, 0.5],
            "slotPose": [1, 0, 0, 1, 0, 0],
            "bonePose": [0, 1, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 10, 0]
        });
        let Some(Shape::Mesh {
            vertices: MeshVertices::Weighted(v),
            ..
        }) = mesh(&d, &rig)
        else {
            panic!("a weighted mesh");
        };
        // The second point, on bone 1 bound at x = 10: at its origin.
        assert_eq!(v[1], vec![(1, [0.0, 0.0], 1.0)]);
        assert_eq!(v[2], vec![(0, [10.0, 10.0], 0.5), (1, [0.0, 10.0], 0.5)]);
    }

    #[test]
    fn a_missing_atlas_or_damage_is_an_error() {
        assert!(import(ARMATURE.as_bytes(), "arm", &InMemory::default()).is_err());
        for cut in [0, 30, ARMATURE.len() / 2] {
            let _ = import(&ARMATURE.as_bytes()[..cut], "arm", &files());
        }
    }
}
