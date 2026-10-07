//! Spine's JSON export (3.8 and 4.x), from its published format
//! description: bones, slots, the default skin's region and mesh
//! attachments (linked meshes too), IK constraints and animations (bone
//! rotation, translation, scale and shear; slot colour and attachment;
//! mesh deforms), with Spine's curves. Pictures come from the texture
//! atlas (its pages next to it) or, without one, loose PNGs in the
//! skeleton's images folder.
//!
//! Left out, with a note: transform and path constraints, draw-order and
//! event timelines, clipping, point and bounding-box attachments, other
//! skins.

use super::{ImportedRig, Siblings, decode_picture, hex_color, num};
use crate::canvas::rig::{
    Attachment, Bone, BoneTrack, Curve, DeformTrack, Ik, Inherit, Key, MeshVertices, Rig,
    RigAnimation, RigImage, Shape, Slot, SlotBlend, SlotTrack,
};
use serde_json::Value;
use std::collections::HashMap;

/// A region of an atlas page: the page, its rectangle as packed, whether
/// it's turned, and its trim (`[left, top, right, bottom]` shares).
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasRegion {
    pub page: usize,
    pub rect: [f32; 4],
    pub rotated: bool,
    pub trim: [f32; 4],
}

/// A libGDX/Spine text atlas: its pages' file names and properties, and
/// its regions by name.
#[derive(Debug, Default)]
pub struct Atlas {
    pub pages: Vec<(String, bool)>,
    pub regions: HashMap<String, AtlasRegion>,
}

/// Read a texture atlas (the old indented layout and Spine 4's).
pub fn parse_atlas(text: &str) -> Atlas {
    let mut atlas = Atlas::default();
    let mut in_page = false;
    let mut region: Option<(String, HashMap<String, Vec<f32>>, String)> = None;
    let finish = |atlas: &mut Atlas,
                  region: Option<(String, HashMap<String, Vec<f32>>, String)>| {
        let Some((name, props, rotate)) = region else {
            return;
        };
        let page = atlas.pages.len().saturating_sub(1);
        let (x, y, w, h) = match (props.get("bounds"), props.get("xy"), props.get("size")) {
            (Some(b), _, _) if b.len() == 4 => (b[0], b[1], b[2], b[3]),
            (_, Some(xy), Some(s)) if xy.len() == 2 && s.len() == 2 => (xy[0], xy[1], s[0], s[1]),
            _ => return,
        };
        let rotated = matches!(rotate.as_str(), "true" | "90" | "270");
        // Trimmed: offsets from the original picture's left and bottom.
        let (ox, oy, ow, oh) = match (props.get("offsets"), props.get("offset"), props.get("orig"))
        {
            (Some(o), _, _) if o.len() == 4 => (o[0], o[1], o[2], o[3]),
            (_, Some(o), Some(g)) if o.len() == 2 && g.len() == 2 => (o[0], o[1], g[0], g[1]),
            _ => (0.0, 0.0, w, h),
        };
        let (ow, oh) = (ow.max(1.0), oh.max(1.0));
        let trim = [ox / ow, (oh - oy - h) / oh, (ox + w) / ow, (oh - oy) / oh];
        let (pw, ph) = if rotated { (h, w) } else { (w, h) };
        atlas.regions.insert(
            name,
            AtlasRegion {
                page,
                rect: [x, y, x + pw, y + ph],
                rotated,
                trim,
            },
        );
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            finish(&mut atlas, region.take());
            in_page = false;
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let (key, value) = (key.trim(), value.trim());
            match &mut region {
                Some((_, props, rotate)) => {
                    if key == "rotate" {
                        *rotate = value.to_string();
                    } else {
                        let nums = value
                            .split(',')
                            .filter_map(|v| v.trim().parse().ok())
                            .collect();
                        props.insert(key.to_string(), nums);
                    }
                }
                None if key == "pma" => {
                    if let Some(page) = atlas.pages.last_mut() {
                        page.1 = value == "true";
                    }
                }
                None => {}
            }
            continue;
        }
        if !in_page {
            atlas.pages.push((line.to_string(), false));
            in_page = true;
        } else {
            finish(&mut atlas, region.take());
            region = Some((line.to_string(), HashMap::new(), String::new()));
        }
    }
    finish(&mut atlas, region.take());
    atlas
}

/// Spine 4's curve (control points in absolute time and value) or 3.x's
/// (shares of the step), normalised.
fn curve(key: &Value, next: Option<&Value>, value: &dyn Fn(&Value) -> f32, v4: bool) -> Curve {
    let Some(c) = key.get("curve") else {
        return Curve::Linear;
    };
    if c.as_str() == Some("stepped") {
        return Curve::Stepped;
    }
    let numbers: Vec<f32> = match c {
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_f64)
            .map(|n| n as f32)
            .collect(),
        // 3.8: `curve: c1, c2: .., c3: .., c4: ..`.
        Value::Number(n) => vec![
            n.as_f64().unwrap_or(0.0) as f32,
            num(key, "c2", 0.0),
            num(key, "c3", 1.0),
            num(key, "c4", 1.0),
        ],
        _ => return Curve::Linear,
    };
    if numbers.len() < 4 {
        return Curve::Linear;
    }
    let [cx1, cy1, cx2, cy2] = [numbers[0], numbers[1], numbers[2], numbers[3]];
    if !v4 {
        return Curve::Bezier([cx1, cy1, cx2, cy2]);
    }
    let Some(next) = next else {
        return Curve::Linear;
    };
    let (t0, t1) = (num(key, "time", 0.0), num(next, "time", 0.0));
    let (v0, v1) = (value(key), value(next));
    if (t1 - t0).abs() < 1e-6 || (v1 - v0).abs() < 1e-6 {
        return Curve::Linear;
    }
    Curve::Bezier([
        (cx1 - t0) / (t1 - t0),
        (cy1 - v0) / (v1 - v0),
        (cx2 - t0) / (t1 - t0),
        (cy2 - v0) / (v1 - v0),
    ])
}

/// A timeline's keys, each value read by `read`; `first` reads the value
/// the curve is measured on.
fn keys<T>(
    list: Option<&Value>,
    read: impl Fn(&Value) -> T,
    first: impl Fn(&Value) -> f32,
    v4: bool,
) -> Vec<Key<T>> {
    let Some(list) = list.and_then(Value::as_array) else {
        return Vec::new();
    };
    (0..list.len())
        .map(|i| Key {
            time: num(&list[i], "time", 0.0),
            value: read(&list[i]),
            curve: curve(&list[i], list.get(i + 1), &first, v4),
        })
        .collect()
}

pub fn import(json: &[u8], stem: &str, siblings: &dyn Siblings) -> Result<ImportedRig, String> {
    let doc: Value = serde_json::from_slice(json).map_err(|e| format!("Not Spine JSON: {e}"))?;
    let skeleton = doc.get("skeleton").ok_or("Not Spine JSON: no skeleton")?;
    let version = skeleton.get("spine").and_then(Value::as_str).unwrap_or("4");
    let v4 = !version.starts_with('3') && !version.starts_with('2');
    let mut notes = Vec::new();
    let mut rig = Rig {
        y_up: true,
        scale: 1.0,
        ..Default::default()
    };

    // Bones (parents come first in the file).
    for b in doc
        .get("bones")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = b
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let parent = b
            .get("parent")
            .and_then(Value::as_str)
            .and_then(|p| rig.bone_index(p));
        let inherit = b
            .get("inherit")
            .or_else(|| b.get("transform"))
            .and_then(Value::as_str);
        rig.bones.push(Bone {
            name,
            parent,
            x: num(b, "x", 0.0),
            y: num(b, "y", 0.0),
            rotation: num(b, "rotation", 0.0),
            scale: [num(b, "scaleX", 1.0), num(b, "scaleY", 1.0)],
            shear: [num(b, "shearX", 0.0), num(b, "shearY", 0.0)],
            length: num(b, "length", 0.0),
            inherit: match inherit {
                Some("onlyTranslation") => Inherit::OnlyTranslation,
                Some("noRotationOrReflection") => Inherit::NoRotation,
                Some("noScale" | "noScaleOrReflection") => Inherit::NoScale,
                _ => Inherit::Normal,
            },
        });
    }
    if rig.bones.is_empty() {
        return Err("The Spine skeleton has no bones".into());
    }

    // Pictures: the atlas's pages, or loose files.
    let atlas_name = [format!("{stem}.atlas"), format!("{stem}.atlas.txt")]
        .into_iter()
        .find(|n| siblings.read(n).is_some())
        .or_else(|| siblings.with_extension("atlas").into_iter().next());
    let atlas = atlas_name
        .as_deref()
        .and_then(|n| siblings.read(n))
        .map(|b| parse_atlas(&String::from_utf8_lossy(&b)));
    let images_dir = skeleton
        .get("images")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_start_matches("./")
        .to_string();
    let mut loose: HashMap<String, usize> = HashMap::new();
    if let Some(atlas) = &atlas {
        for (page, pma) in &atlas.pages {
            let bytes = siblings
                .read(page)
                .ok_or_else(|| format!("The atlas page {page} is missing"))?;
            rig.images.push(decode_picture(page, &bytes, *pma)?);
        }
    } else {
        notes.push("No texture atlas: pictures read from the images folder".into());
    }

    // Slots.
    for s in doc
        .get("slots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let bone = s
            .get("bone")
            .and_then(Value::as_str)
            .and_then(|b| rig.bone_index(b))
            .unwrap_or(0);
        rig.slots.push(Slot {
            name: s
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            bone,
            color: s
                .get("color")
                .and_then(Value::as_str)
                .and_then(hex_color)
                .unwrap_or([1.0; 4]),
            attachment: s
                .get("attachment")
                .and_then(Value::as_str)
                .map(str::to_string),
            blend: match s.get("blend").and_then(Value::as_str) {
                Some("additive") => SlotBlend::Additive,
                Some("multiply") => SlotBlend::Multiply,
                Some("screen") => SlotBlend::Screen,
                _ => SlotBlend::Normal,
            },
            attachments: Vec::new(),
        });
    }

    // The default skin (4.x: a list; 3.8: an object of skins).
    let skins = doc.get("skins");
    let default_skin = match skins {
        Some(Value::Array(list)) => {
            if list.len() > 1 {
                notes.push("Only the default skin came across".into());
            }
            list.iter()
                .find(|s| s.get("name").and_then(Value::as_str) == Some("default"))
                .or(list.first())
                .and_then(|s| s.get("attachments"))
        }
        Some(Value::Object(map)) => map.get("default").or_else(|| map.values().next()),
        _ => None,
    };
    let mut linked: Vec<(usize, String, String, String)> = Vec::new();
    for (slot_name, atts) in default_skin
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(si) = rig.slot_index(slot_name) else {
            continue;
        };
        for (att_name, a) in atts.as_object().into_iter().flatten() {
            let kind = a.get("type").and_then(Value::as_str).unwrap_or("region");
            let path = a
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(att_name)
                .to_string();
            let Some((image, region, rotated, trim)) = picture(
                &mut rig.images,
                &mut loose,
                atlas.as_ref(),
                &images_dir,
                &path,
                siblings,
            ) else {
                notes.push(format!("{att_name}: its picture {path} wasn't found"));
                continue;
            };
            let shape = match kind {
                "region" => Shape::Region {
                    x: num(a, "x", 0.0),
                    y: num(a, "y", 0.0),
                    rotation: num(a, "rotation", 0.0),
                    scale: [num(a, "scaleX", 1.0), num(a, "scaleY", 1.0)],
                    width: num(a, "width", 32.0),
                    height: num(a, "height", 32.0),
                    trim,
                },
                "mesh" => match mesh(a, rig.bones.len()) {
                    Some(m) => m,
                    None => {
                        notes.push(format!("{att_name}: a damaged mesh"));
                        continue;
                    }
                },
                "linkedmesh" => {
                    let parent = a
                        .get("parent")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    // (Its parent hangs on the same slot.)
                    let parent_slot = slot_name.clone();
                    linked.push((si, att_name.clone(), parent_slot, parent));
                    // Placeholder, filled from its parent below.
                    Shape::Mesh {
                        uvs: Vec::new(),
                        triangles: Vec::new(),
                        vertices: MeshVertices::Plain(Vec::new()),
                    }
                }
                other => {
                    notes.push(format!(
                        "{att_name}: Spine's {other} attachments are left out"
                    ));
                    continue;
                }
            };
            rig.slots[si].attachments.push(Attachment {
                name: att_name.clone(),
                image,
                region,
                rotated,
                shape,
            });
        }
    }
    // Linked meshes take their parent's shape.
    for (si, name, parent_slot, parent) in linked {
        let source = (rig.slot_index(&parent_slot))
            .and_then(|ps| rig.slots[ps].attachments.iter().find(|a| a.name == parent))
            .map(|a| a.shape.clone());
        if let (Some(shape), Some(a)) = (
            source,
            rig.slots[si]
                .attachments
                .iter_mut()
                .find(|a| a.name == name),
        ) {
            a.shape = shape;
        }
    }

    // IK.
    for k in doc
        .get("ik")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let bones: Vec<usize> = (k
            .get("bones")
            .and_then(Value::as_array)
            .into_iter()
            .flatten())
        .filter_map(|b| b.as_str().and_then(|b| rig.bone_index(b)))
        .collect();
        let Some(target) = k
            .get("target")
            .and_then(Value::as_str)
            .and_then(|t| rig.bone_index(t))
        else {
            continue;
        };
        rig.ik.push(Ik {
            name: k
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            bones,
            target,
            mix: num(k, "mix", 1.0),
            bend_positive: k
                .get("bendPositive")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        });
    }
    for unsupported in ["transform", "path", "physics"] {
        if doc
            .get(unsupported)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            notes.push(format!("Spine's {unsupported} constraints are left out"));
        }
    }

    // Animations.
    for (name, a) in doc
        .get("animations")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let mut anim = RigAnimation {
            name: name.clone(),
            ..Default::default()
        };
        let mut latest = 0.0f32;
        let mut seen = |keys_time: f32| latest = latest.max(keys_time);
        for (bone_name, tl) in a
            .get("bones")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some(bone) = rig.bone_index(bone_name) else {
                continue;
            };
            let angle = |k: &Value| num(k, if v4 { "value" } else { "angle" }, 0.0);
            let pair = |k: &Value, d: f32| [num(k, "x", d), num(k, "y", d)];
            let track = BoneTrack {
                bone,
                rotate: keys(tl.get("rotate"), angle, angle, v4),
                translate: keys(
                    tl.get("translate"),
                    |k| pair(k, 0.0),
                    |k| num(k, "x", 0.0),
                    v4,
                ),
                scale: keys(tl.get("scale"), |k| pair(k, 1.0), |k| num(k, "x", 1.0), v4),
                shear: keys(tl.get("shear"), |k| pair(k, 0.0), |k| num(k, "x", 0.0), v4),
            };
            for list in [
                &track.rotate.iter().map(|k| k.time).collect::<Vec<_>>(),
                &track.translate.iter().map(|k| k.time).collect(),
                &track.scale.iter().map(|k| k.time).collect(),
                &track.shear.iter().map(|k| k.time).collect(),
            ] {
                list.iter().for_each(|&t| seen(t));
            }
            for split in [
                "translatex",
                "translatey",
                "scalex",
                "scaley",
                "shearx",
                "sheary",
            ] {
                if tl.get(split).is_some() {
                    notes.push(format!(
                        "{name}: {bone_name}'s {split} timeline is left out"
                    ));
                }
            }
            anim.bones.push(track);
        }
        for (slot_name, tl) in a
            .get("slots")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some(slot) = rig.slot_index(slot_name) else {
                continue;
            };
            let color_list = tl.get("rgba").or_else(|| tl.get("color"));
            let color = keys(
                color_list,
                |k| {
                    k.get("color")
                        .and_then(Value::as_str)
                        .and_then(hex_color)
                        .unwrap_or([1.0; 4])
                },
                |k| {
                    k.get("color")
                        .and_then(Value::as_str)
                        .and_then(hex_color)
                        .map_or(1.0, |c| c[3])
                },
                v4,
            );
            let attachment: Vec<(f32, Option<String>)> = (tl
                .get("attachment")
                .and_then(Value::as_array)
                .into_iter()
                .flatten())
            .map(|k| {
                (
                    num(k, "time", 0.0),
                    k.get("name").and_then(Value::as_str).map(str::to_string),
                )
            })
            .collect();
            color.iter().for_each(|k| seen(k.time));
            attachment.iter().for_each(|(t, _)| seen(*t));
            anim.slots.push(SlotTrack {
                slot,
                color,
                attachment,
            });
        }
        // Deforms: 4.0 and 3.x under "deform", 4.1+ under "attachments".
        let mut deform_lists: Vec<(String, String, &Value)> = Vec::new();
        for (_, skin) in a
            .get("deform")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            for (slot_name, atts) in skin.as_object().into_iter().flatten() {
                for (att, list) in atts.as_object().into_iter().flatten() {
                    deform_lists.push((slot_name.clone(), att.clone(), list));
                }
            }
        }
        for (_, skin) in a
            .get("attachments")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            for (slot_name, atts) in skin.as_object().into_iter().flatten() {
                for (att, timelines) in atts.as_object().into_iter().flatten() {
                    if let Some(list) = timelines.get("deform") {
                        deform_lists.push((slot_name.clone(), att.clone(), list));
                    }
                }
            }
        }
        for (slot_name, att, list) in deform_lists {
            let Some(slot) = rig.slot_index(&slot_name) else {
                continue;
            };
            let read = |k: &Value| {
                let offset = num(k, "offset", 0.0) as usize;
                let values: Vec<f32> = (k
                    .get("vertices")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten())
                .filter_map(Value::as_f64)
                .map(|v| v as f32)
                .collect();
                let mut flat = vec![0.0; offset + values.len()];
                flat[offset..].copy_from_slice(&values);
                if flat.len() % 2 == 1 {
                    flat.push(0.0);
                }
                flat.chunks(2)
                    .map(|c| [c[0], c[1]])
                    .collect::<Vec<[f32; 2]>>()
            };
            let track = DeformTrack {
                slot,
                attachment: att,
                keys: keys(Some(list), read, |_| 0.0, false),
            };
            track.keys.iter().for_each(|k| seen(k.time));
            anim.deforms.push(track);
        }
        for left in ["drawOrder", "draworder", "events", "path", "transform"] {
            if a.get(left).is_some() {
                notes.push(format!("{name}: its {left} timelines are left out"));
            }
        }
        anim.duration = latest;
        rig.animations.push(anim);
    }
    rig.animation = (!rig.animations.is_empty()).then_some(0);
    let bounds = Some([
        num(skeleton, "x", 0.0),
        num(skeleton, "y", 0.0),
        num(skeleton, "width", 0.0),
        num(skeleton, "height", 0.0),
    ])
    .filter(|b| b[2] > 0.0 && b[3] > 0.0);
    notes.sort();
    notes.dedup();
    Ok(ImportedRig {
        rig,
        bounds,
        notes,
        fps: None,
    })
}

/// An attachment's picture: (image index, region, rotated, trim), from the
/// atlas or a loose file `<images>/<path>.png`.
fn picture(
    images: &mut Vec<RigImage>,
    loose: &mut HashMap<String, usize>,
    atlas: Option<&Atlas>,
    images_dir: &str,
    path: &str,
    siblings: &dyn Siblings,
) -> Option<(usize, [f32; 4], bool, [f32; 4])> {
    if let Some(atlas) = atlas {
        let r = atlas.regions.get(path)?;
        return Some((r.page, r.rect, r.rotated, r.trim));
    }
    let index = match loose.get(path) {
        Some(&i) => i,
        None => {
            let file = if images_dir.is_empty() {
                format!("{path}.png")
            } else {
                format!("{}/{path}.png", images_dir.trim_end_matches('/'))
            };
            let bytes = siblings
                .read(&file)
                .or_else(|| siblings.read(&format!("{path}.png")))?;
            images.push(decode_picture(path, &bytes, false).ok()?);
            loose.insert(path.to_string(), images.len() - 1);
            images.len() - 1
        }
    };
    let img = &images[index];
    Some((
        index,
        [0.0, 0.0, img.width as f32, img.height as f32],
        false,
        [0.0, 0.0, 1.0, 1.0],
    ))
}

/// A mesh attachment: plain points (as many numbers as its UVs) or weighted
/// ones (for each point, its bone count and then bone, x, y, weight each).
fn mesh(a: &Value, bones: usize) -> Option<Shape> {
    let floats = |key: &str| -> Vec<f32> {
        (a.get(key).and_then(Value::as_array).into_iter().flatten())
            .filter_map(Value::as_f64)
            .map(|v| v as f32)
            .collect()
    };
    let uv = floats("uvs");
    let tri = floats("triangles");
    let v = floats("vertices");
    if uv.len() % 2 == 1 || tri.len() % 3 != 0 {
        return None;
    }
    let uvs: Vec<[f32; 2]> = uv.chunks(2).map(|c| [c[0], c[1]]).collect();
    let n = uvs.len();
    let triangles: Vec<[u32; 3]> = tri
        .chunks(3)
        .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
        .filter(|t| t.iter().all(|&i| (i as usize) < n))
        .collect();
    let vertices = if v.len() == uv.len() {
        MeshVertices::Plain(v.chunks(2).map(|c| [c[0], c[1]]).collect())
    } else {
        let mut out = Vec::with_capacity(n);
        let mut i = 0;
        while i < v.len() && out.len() < n {
            let count = v[i] as usize;
            i += 1;
            if count > 64 || i + count * 4 > v.len() {
                return None;
            }
            let influences = (0..count)
                .map(|k| {
                    let at = i + k * 4;
                    (
                        (v[at] as usize).min(bones.saturating_sub(1)),
                        [v[at + 1], v[at + 2]],
                        v[at + 3],
                    )
                })
                .collect();
            i += count * 4;
            out.push(influences);
        }
        if out.len() != n {
            return None;
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
pub(crate) mod tests {
    use super::*;
    use crate::project::anim_import::InMemory;
    use eframe::egui::Color32;

    /// A 16×8 page: red left half, blue right half, as a PNG.
    pub(crate) fn page_png() -> Vec<u8> {
        let img = image::RgbaImage::from_fn(16, 8, |x, _| {
            if x < 8 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    const ATLAS: &str = "
arm.png
size: 16,8
format: RGBA8888
filter: Linear,Linear
repeat: none
red
  rotate: false
  xy: 0, 0
  size: 8, 8
  orig: 8, 8
  offset: 0, 0
  index: -1
blue
  rotate: false
  xy: 8, 0
  size: 8, 8
  orig: 8, 8
  offset: 0, 0
  index: -1
";

    /// A two-bone arm: the upper half red, the forearm blue, the forearm
    /// turning a quarter over one second (ease in and out), and the red
    /// one fading out.
    pub(crate) const SKELETON: &str = r#"{
  "skeleton": { "spine": "4.1.24", "x": -4, "y": -4, "width": 24, "height": 8 },
  "bones": [
    { "name": "root" },
    { "name": "upper", "parent": "root", "length": 8 },
    { "name": "lower", "parent": "upper", "x": 8, "length": 8 }
  ],
  "slots": [
    { "name": "upper", "bone": "upper", "attachment": "red" },
    { "name": "lower", "bone": "lower", "attachment": "blue" }
  ],
  "skins": [ { "name": "default", "attachments": {
    "upper": { "red": { "x": 4, "width": 8, "height": 8 } },
    "lower": { "blue": { "x": 4, "width": 8, "height": 8 } }
  } } ],
  "animations": { "bend": {
    "bones": { "lower": { "rotate": [
      { "time": 0, "value": 0, "curve": [0.33, 0, 0.67, 90] },
      { "time": 1, "value": 90 }
    ] } },
    "slots": { "upper": { "rgba": [
      { "time": 0, "color": "ffffffff" },
      { "time": 1, "color": "ffffff00" }
    ] } }
  } }
}"#;

    pub(crate) fn files() -> InMemory {
        InMemory(vec![
            ("arm.atlas".into(), ATLAS.as_bytes().to_vec()),
            ("arm.png".into(), page_png()),
        ])
    }

    #[test]
    fn an_atlas_gives_its_pages_and_regions_in_either_layout() {
        let atlas = parse_atlas(ATLAS);
        assert_eq!(atlas.pages, [("arm.png".to_string(), false)]);
        assert_eq!(atlas.regions["blue"].rect, [8.0, 0.0, 16.0, 8.0]);
        let v4 = parse_atlas(
            "page.png\nsize:64,64\npma:true\nhead\nbounds:2,4,10,20\noffsets:1,2,12,24\nrotate:90\n",
        );
        assert!(v4.pages[0].1, "premultiplied");
        let head = &v4.regions["head"];
        assert!(head.rotated);
        assert_eq!(
            head.rect,
            [2.0, 4.0, 22.0, 14.0],
            "packed turned: 20 wide, 10 tall"
        );
        let [l, t, r, b] = head.trim;
        assert!((l - 1.0 / 12.0).abs() < 1e-6 && (r - 11.0 / 12.0).abs() < 1e-6);
        assert!((t - 2.0 / 24.0).abs() < 1e-6 && (b - 22.0 / 24.0).abs() < 1e-6);
    }

    #[test]
    fn a_spine_skeleton_comes_across_posed_and_animated() {
        let imported = import(SKELETON.as_bytes(), "arm", &files()).unwrap();
        let rig = &imported.rig;
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        assert_eq!(rig.bones.len(), 3);
        assert_eq!(rig.slots[1].attachments[0].region, [8.0, 0.0, 16.0, 8.0]);
        let anim = &rig.animations[0];
        assert_eq!((anim.name.as_str(), anim.duration), ("bend", 1.0));
        // Spine 4's absolute curve points, normalised.
        assert_eq!(
            anim.bones[0].rotate[0].curve,
            Curve::Bezier([0.33, 0.0, 0.67, 1.0])
        );
        let mut rig = rig.clone();
        rig.origin = [32.0, 32.0];
        rig.scale = 2.0;
        // At the start: red then blue along x.
        let tiles = rig.render(&rig.pose(0.0), 64, 64, 64);
        let px = |tiles: &[((i32, i32), Vec<Color32>)], x: usize, y: usize| tiles[0].1[y * 64 + x];
        assert_eq!(px(&tiles, 38, 32), Color32::RED);
        assert_eq!(px(&tiles, 54, 32), Color32::BLUE);
        // Half way (0.5 s, and loops at 1 s): the forearm turned up, the red faded half.
        let tiles = rig.render(&rig.pose(0.99), 64, 64, 64);
        assert_eq!(px(&tiles, 54, 32).a(), 0, "the forearm went up");
        assert!(
            px(&tiles, 49, 20).b() > 200,
            "up the canvas: {:?}",
            px(&tiles, 49, 20)
        );
        assert!(px(&tiles, 38, 32).a() < 10, "the red faded out");
    }

    #[test]
    fn weighted_meshes_and_their_deforms_are_read() {
        let mesh_doc = serde_json::json!({
            "type": "mesh",
            "uvs": [0, 0, 1, 0, 1, 1],
            "triangles": [0, 1, 2],
            "vertices": [1, 0, 0, 0, 1, 2, 0, 5, 0, 0.5, 1, 5, 0, 0.5, 1, 1, 5, 5, 1]
        });
        let Some(Shape::Mesh {
            vertices: MeshVertices::Weighted(v),
            ..
        }) = mesh(&mesh_doc, 2)
        else {
            panic!("a weighted mesh");
        };
        assert_eq!(v.len(), 3);
        assert_eq!(v[1], vec![(0, [5.0, 0.0], 0.5), (1, [5.0, 0.0], 0.5)]);
        let damaged =
            serde_json::json!({ "uvs": [0, 0, 1, 0], "triangles": [0, 1, 7], "vertices": [9, 1] });
        assert!(mesh(&damaged, 2).is_none());
    }

    #[test]
    fn damaged_files_are_refused_not_panicked_on() {
        let good = SKELETON.as_bytes();
        for cut in [0, 10, good.len() / 2, good.len() - 3] {
            let _ = import(&good[..cut], "arm", &files());
        }
        assert!(import(b"{\"skeleton\":{},\"bones\":[]}", "x", &files()).is_err());
        // An atlas page missing: an error, not a panic.
        let no_page = InMemory(vec![("arm.atlas".into(), ATLAS.as_bytes().to_vec())]);
        assert!(import(good, "arm", &no_page).is_err());
    }
}
