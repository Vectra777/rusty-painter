//! Lottie (Bodymovin) JSON, from the published Lottie specification. Each
//! layer becomes a bone (its parent the layer it's parented to) moved by its
//! transform (position, anchor, scale, rotation) and fading with its
//! opacity, and shows between its in and out frames. What it shows becomes
//! a picture on its slot: an image asset (embedded or a file beside it), a
//! solid, or a shape layer's paths, rectangles and ellipses filled and
//! stroked as they are on its first frame. A precomposition is a bone
//! holding its own layers.
//!
//! Left out, with a note: shapes whose paths move, text layers, masks and
//! mattes, effects, time remapping, skew, 3D.

use super::{ImportedRig, Siblings, decode_picture, num};
use crate::canvas::rig::{
    Attachment, Bone, BoneTrack, Curve, Key, Rig, RigAnimation, RigImage, Shape, Slot, SlotTrack,
};
use eframe::egui::Color32;
use serde_json::Value;
use std::sync::Arc;

/// An animatable property: its keys (frame, value) with curves, or one
/// value.
fn property(p: Option<&Value>, fps: f32, start: f32) -> Vec<Key<Vec<f32>>> {
    let Some(p) = p else {
        return Vec::new();
    };
    let floats = |v: &Value| -> Vec<f32> {
        match v {
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_f64)
                .map(|x| x as f32)
                .collect(),
            Value::Number(n) => vec![n.as_f64().unwrap_or(0.0) as f32],
            _ => Vec::new(),
        }
    };
    let k = p.get("k");
    let animated = p.get("a").and_then(Value::as_i64) == Some(1)
        || k.and_then(Value::as_array)
            .is_some_and(|a| a.first().is_some_and(|f| f.get("t").is_some()));
    if !animated {
        return k
            .map(|v| {
                vec![Key {
                    time: 0.0,
                    value: floats(v),
                    curve: Curve::Linear,
                }]
            })
            .unwrap_or_default();
    }
    let frames = k.and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out: Vec<Key<Vec<f32>>> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        // A key's value; older files give the end value on the key before.
        let value = f
            .get("s")
            .map(floats)
            .or_else(|| {
                i.checked_sub(1)
                    .and_then(|j| frames[j].get("e"))
                    .map(floats)
            })
            .unwrap_or_default();
        let tangent = |key: &str| -> Option<[f32; 2]> {
            let t = f.get(key)?;
            let first = |v: &Value| {
                v.as_array()
                    .and_then(|a| a.first())
                    .or(Some(v))
                    .and_then(Value::as_f64)
            };
            Some([first(t.get("x")?)? as f32, first(t.get("y")?)? as f32])
        };
        let curve = if f.get("h").and_then(Value::as_i64) == Some(1) {
            Curve::Stepped
        } else if let (Some(o), Some(i)) = (tangent("o"), tangent("i")) {
            Curve::Bezier([o[0], o[1], i[0], i[1]])
        } else {
            Curve::Linear
        };
        out.push(Key {
            time: (num(f, "t", 0.0) - start) / fps,
            value,
            curve,
        });
    }
    out
}

/// A property's value at its first key.
fn first(keys: &[Key<Vec<f32>>], i: usize, default: f32) -> f32 {
    keys.first()
        .and_then(|k| k.value.get(i).copied())
        .unwrap_or(default)
}

/// Keys of one or two components.
fn pick<const N: usize>(keys: &[Key<Vec<f32>>], scale: f32, default: f32) -> Vec<Key<[f32; N]>> {
    keys.iter()
        .map(|k| Key {
            time: k.time,
            value: std::array::from_fn(|i| k.value.get(i).copied().unwrap_or(default) * scale),
            curve: k.curve,
        })
        .collect()
}

/// A layer list and the bone its layers hang from.
struct Context<'a> {
    rig: Rig,
    notes: Vec<String>,
    assets: Vec<&'a Value>,
    siblings: &'a dyn Siblings,
    fps: f32,
    start: f32,
    anim: RigAnimation,
}

pub fn import(json: &[u8], siblings: &dyn Siblings) -> Result<ImportedRig, String> {
    let doc: Value = serde_json::from_slice(json).map_err(|e| format!("Not Lottie JSON: {e}"))?;
    let fps = num(&doc, "fr", 30.0).max(1.0);
    let (ip, op) = (num(&doc, "ip", 0.0), num(&doc, "op", 60.0));
    let (w, h) = (num(&doc, "w", 512.0), num(&doc, "h", 512.0));
    let assets: Vec<&Value> = doc
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let mut cx = Context {
        rig: Rig {
            scale: 1.0,
            ..Default::default()
        },
        notes: Vec::new(),
        assets,
        siblings,
        fps,
        start: ip,
        anim: RigAnimation {
            name: doc
                .get("nm")
                .and_then(Value::as_str)
                .unwrap_or("Animation")
                .to_string(),
            duration: ((op - ip) / fps).max(0.0),
            ..Default::default()
        },
    };
    cx.rig.bones.push(Bone {
        name: "root".into(),
        ..Default::default()
    });
    let layers = doc
        .get("layers")
        .and_then(Value::as_array)
        .ok_or("No layers")?;
    cx.layers(layers, 0, 0)?;
    let mut rig = cx.rig;
    rig.animations.push(cx.anim);
    rig.animation = Some(0);
    let mut notes = cx.notes;
    notes.sort();
    notes.dedup();
    Ok(ImportedRig {
        rig,
        bounds: Some([0.0, 0.0, w, h]),
        notes,
        fps: Some(fps.round() as u32),
    })
}

impl Context<'_> {
    /// `layers` (top first) as bones under `parent`, their slots stacked
    /// bottom first; `depth` guards against precomps in a loop.
    fn layers(&mut self, layers: &[Value], parent: usize, depth: usize) -> Result<(), String> {
        if depth > 16 {
            return Err("Precompositions nested too deep".into());
        }
        // Bones in an order where parents come first: by `ind` and `parent`.
        let mut placed: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        let mut pending: Vec<&Value> = layers.iter().collect();
        let mut made: Vec<(&Value, usize)> = Vec::new();
        while !pending.is_empty() {
            let before = pending.len();
            pending.retain(|l| {
                let p = l.get("parent").and_then(Value::as_i64);
                let parent_bone = match p {
                    None => Some(parent),
                    Some(p) => placed.get(&p).copied(),
                };
                let Some(pb) = parent_bone else {
                    return true;
                };
                let bone = self.bone(l, pb);
                if let Some(ind) = l.get("ind").and_then(Value::as_i64) {
                    placed.insert(ind, bone);
                }
                made.push((l, bone));
                false
            });
            if pending.len() == before {
                // A parent that isn't there (or a loop): hang them on ours.
                for l in pending.drain(..) {
                    let bone = self.bone(l, parent);
                    made.push((l, bone));
                }
            }
        }
        // Slots bottom first: the file lists the top layer first.
        for l in layers.iter().rev() {
            let Some(&(_, bone)) = made.iter().find(|(m, _)| std::ptr::eq(*m, l)) else {
                continue;
            };
            self.content(l, bone, depth)?;
        }
        Ok(())
    }

    /// A layer's bone and its transform's keys.
    fn bone(&mut self, l: &Value, parent: usize) -> usize {
        let ks = l.get("ks");
        let get = |k: &str| ks.and_then(|ks| ks.get(k));
        let (fps, start) = (self.fps, self.start);
        // Position, given whole or split into x and y.
        let position = match get("p") {
            Some(p) if p.get("s").and_then(Value::as_bool) == Some(true) => {
                let (x, y) = (
                    property(p.get("x"), fps, start),
                    property(p.get("y"), fps, start),
                );
                let mut keys: Vec<Key<Vec<f32>>> = x
                    .iter()
                    .map(|k| Key {
                        time: k.time,
                        value: vec![k.value.first().copied().unwrap_or(0.0), first(&y, 0, 0.0)],
                        curve: k.curve,
                    })
                    .collect();
                if keys.is_empty() {
                    keys = vec![Key {
                        time: 0.0,
                        value: vec![0.0, first(&y, 0, 0.0)],
                        curve: Curve::Linear,
                    }];
                }
                if y.len() > 1 {
                    self.notes
                        .push("A position animated in x and y apart came across in x".into());
                }
                keys
            }
            p => property(p, fps, start),
        };
        let rotation = property(get("r").or_else(|| get("rz")), fps, start);
        let scale = property(get("s"), fps, start);
        if get("sk").is_some_and(|s| first(&property(Some(s), fps, start), 0, 0.0) != 0.0) {
            self.notes.push("Skew is left out".into());
        }
        let name = l
            .get("nm")
            .and_then(Value::as_str)
            .unwrap_or("Layer")
            .to_string();
        self.rig.bones.push(Bone {
            name,
            parent: Some(parent),
            ..Default::default()
        });
        let bone = self.rig.bones.len() - 1;
        self.anim.bones.push(BoneTrack {
            bone,
            rotate: pick::<1>(&rotation, 1.0, 0.0)
                .into_iter()
                .map(|k| Key {
                    time: k.time,
                    value: k.value[0],
                    curve: k.curve,
                })
                .collect(),
            translate: pick(&position, 1.0, 0.0),
            scale: pick(&scale, 0.01, 100.0),
            shear: Vec::new(),
        });
        bone
    }

    /// What a layer shows: a slot on its bone, with its picture placed
    /// against its anchor, its opacity and its in and out frames.
    fn content(&mut self, l: &Value, bone: usize, depth: usize) -> Result<(), String> {
        let name = l
            .get("nm")
            .and_then(Value::as_str)
            .unwrap_or("Layer")
            .to_string();
        let (fps, start) = (self.fps, self.start);
        let anchor = property(l.get("ks").and_then(|k| k.get("a")), fps, start);
        let (ax, ay) = (first(&anchor, 0, 0.0), first(&anchor, 1, 0.0));
        if anchor.len() > 1 {
            self.notes
                .push(format!("{name}: its moving anchor came across still"));
        }
        for left in [
            ("masksProperties", "masks"),
            ("tt", "mattes"),
            ("ef", "effects"),
            ("tm", "time remapping"),
        ] {
            if l.get(left.0)
                .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
            {
                self.notes.push(format!("{name}: {} are left out", left.1));
            }
        }
        // The picture and where its top left sits in the layer's frame.
        let picture: Option<(RigImage, [f32; 2])> = match l.get("ty").and_then(Value::as_i64) {
            Some(0) => {
                // A precomposition: its layers hang from this bone.
                let id = l.get("refId").and_then(Value::as_str).unwrap_or("");
                let Some(asset) = self
                    .assets
                    .iter()
                    .find(|a| a.get("id").and_then(Value::as_str) == Some(id))
                    .copied()
                else {
                    self.notes
                        .push(format!("{name}: its precomposition is missing"));
                    return Ok(());
                };
                let layers = asset
                    .get("layers")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // (Its layers are placed against the precomp's anchor.)
                self.rig.bones.push(Bone {
                    name: format!("{name} (contents)"),
                    parent: Some(bone),
                    x: -ax,
                    y: -ay,
                    ..Default::default()
                });
                let inner = self.rig.bones.len() - 1;
                return self.layers(&layers, inner, depth + 1);
            }
            Some(1) => {
                let color = l
                    .get("sc")
                    .and_then(Value::as_str)
                    .and_then(super::hex_color)
                    .unwrap_or([1.0; 4]);
                let (sw, sh) = (
                    num(l, "sw", 1.0).max(1.0) as usize,
                    num(l, "sh", 1.0).max(1.0) as usize,
                );
                let px = to_color(color);
                Some((
                    RigImage {
                        name: name.clone(),
                        width: sw.min(8192),
                        height: sh.min(8192),
                        pixels: Arc::new(vec![px; sw.min(8192) * sh.min(8192)]),
                    },
                    [0.0, 0.0],
                ))
            }
            Some(2) => {
                let id = l.get("refId").and_then(Value::as_str).unwrap_or("");
                match self.image_asset(id) {
                    Some(img) => Some((img, [0.0, 0.0])),
                    None => {
                        self.notes.push(format!("{name}: its picture is missing"));
                        None
                    }
                }
            }
            Some(3) => None,
            Some(4) => {
                let shapes = l
                    .get("shapes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                shapes::draw(&shapes, &name, &mut self.notes)
            }
            Some(5) => {
                self.notes.push(format!("{name}: text layers are left out"));
                None
            }
            _ => None,
        };
        let Some((image, corner)) = picture else {
            return Ok(());
        };
        let (iw, ih) = (image.width as f32, image.height as f32);
        self.rig.images.push(image);
        let index = self.rig.images.len() - 1;
        self.rig.slots.push(Slot {
            name: name.clone(),
            bone,
            attachment: Some(name.clone()),
            attachments: vec![Attachment {
                name: name.clone(),
                image: index,
                region: [0.0, 0.0, iw, ih],
                rotated: false,
                shape: Shape::Region {
                    x: corner[0] + iw / 2.0 - ax,
                    y: corner[1] + ih / 2.0 - ay,
                    rotation: 0.0,
                    scale: [1.0, 1.0],
                    width: iw,
                    height: ih,
                    trim: [0.0, 0.0, 1.0, 1.0],
                },
            }],
            ..Default::default()
        });
        let slot = self.rig.slots.len() - 1;
        // Shown between its in and out frames, at its opacity.
        let (ip, op) = (
            (num(l, "ip", start) - start) / fps,
            (num(l, "op", f32::MAX) - start) / fps,
        );
        let mut attachment = vec![(0.0, (ip <= 0.0).then(|| name.clone()))];
        if ip > 0.0 {
            attachment.push((ip, Some(name.clone())));
        }
        if op < self.anim.duration {
            attachment.push((op, None));
        }
        let opacity = property(l.get("ks").and_then(|k| k.get("o")), fps, start);
        let color = opacity
            .iter()
            .map(|k| Key {
                time: k.time,
                value: [
                    1.0,
                    1.0,
                    1.0,
                    (k.value.first().copied().unwrap_or(100.0) / 100.0).clamp(0.0, 1.0),
                ],
                curve: k.curve,
            })
            .collect();
        self.anim.slots.push(SlotTrack {
            slot,
            color,
            attachment,
        });
        Ok(())
    }

    /// An image asset: embedded (a data URI) or a file beside the JSON.
    fn image_asset(&self, id: &str) -> Option<RigImage> {
        let asset = self
            .assets
            .iter()
            .find(|a| a.get("id").and_then(Value::as_str) == Some(id))?;
        let p = asset.get("p").and_then(Value::as_str)?;
        let bytes = match p.split_once(";base64,") {
            Some((_, data)) => base64_decode(data)?,
            None => {
                let dir = asset.get("u").and_then(Value::as_str).unwrap_or("");
                let path = format!("{}{p}", dir.trim_start_matches("./"));
                self.siblings
                    .read(&path)
                    .or_else(|| self.siblings.read(p))?
            }
        };
        decode_picture(id, &bytes, false).ok()
    }
}

/// Unmultiplied 0..1 colour as a pixel.
fn to_color([r, g, b, a]: [f32; 4]) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgba_unmultiplied(q(r), q(g), q(b), q(a))
}

/// Standard base64 (as data URIs carry), whitespace skipped.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let clean: Vec<u8> = text
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

/// Shape layers drawn into a picture.
mod shapes {
    use super::*;

    /// One filled or stroked outline: its closed or open polylines.
    struct Paint {
        polys: Vec<(Vec<[f32; 2]>, bool)>,
        fill: Option<[f32; 4]>,
        stroke: Option<([f32; 4], f32)>,
    }

    /// A static property's numbers.
    fn value(p: Option<&Value>) -> Vec<f32> {
        property(p, 1.0, 0.0)
            .first()
            .map(|k| k.value.clone())
            .unwrap_or_default()
    }

    /// Whether a property moves.
    fn moves(p: Option<&Value>) -> bool {
        p.is_some_and(|p| p.get("a").and_then(Value::as_i64) == Some(1))
    }

    /// A bezier path `{c, v, i, o}` as points.
    fn path(ks: &Value) -> Option<(Vec<[f32; 2]>, bool)> {
        let shape = match ks.get("k") {
            Some(Value::Array(a)) => a
                .first()?
                .get("s")
                .and_then(|s| s.as_array()?.first().cloned())
                .unwrap_or(Value::Null),
            Some(v) => v.clone(),
            None => return None,
        };
        let pts = |k: &str| -> Vec<[f32; 2]> {
            (shape.get(k).and_then(Value::as_array).into_iter().flatten())
                .filter_map(|p| {
                    let a = p.as_array()?;
                    Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                })
                .collect()
        };
        let (v, i, o) = (pts("v"), pts("i"), pts("o"));
        let closed = shape.get("c").and_then(Value::as_bool).unwrap_or(false);
        if v.is_empty() {
            return None;
        }
        let n = v.len();
        let mut out = Vec::new();
        let segments = if closed { n } else { n - 1 };
        for s in 0..segments {
            let (a, b) = (v[s], v[(s + 1) % n]);
            let c1 = [
                a[0] + o.get(s).map_or(0.0, |t| t[0]),
                a[1] + o.get(s).map_or(0.0, |t| t[1]),
            ];
            let c2 = [
                b[0] + i.get((s + 1) % n).map_or(0.0, |t| t[0]),
                b[1] + i.get((s + 1) % n).map_or(0.0, |t| t[1]),
            ];
            for k in 0..16 {
                let t = k as f32 / 16.0;
                let r = 1.0 - t;
                out.push(std::array::from_fn(|d| {
                    r * r * r * a[d]
                        + 3.0 * r * r * t * c1[d]
                        + 3.0 * r * t * t * c2[d]
                        + t * t * t * b[d]
                }));
            }
        }
        if !closed {
            out.push(v[n - 1]);
        }
        Some((out, closed))
    }

    /// A group's items: outlines, then the fills and strokes that paint
    /// them (in the group's transform).
    fn group(
        items: &[Value],
        transform: &crate::canvas::rig::Affine,
        out: &mut Vec<Paint>,
        notes: &mut Vec<String>,
        name: &str,
    ) {
        let mut polys: Vec<(Vec<[f32; 2]>, bool)> = Vec::new();
        let mut fill = None;
        let mut stroke = None;
        // The group's own transform comes last in the list.
        let own = items
            .iter()
            .find(|i| i.get("ty").and_then(Value::as_str) == Some("tr"))
            .map_or(crate::canvas::rig::Affine::IDENTITY, |tr| {
                let p = value(tr.get("p"));
                let a = value(tr.get("a"));
                let s = value(tr.get("s"));
                let r = value(tr.get("r"));
                let at = |v: &Vec<f32>, i, d| v.get(i).copied().unwrap_or(d);
                let base = crate::canvas::rig::Affine::local(
                    at(&p, 0, 0.0),
                    at(&p, 1, 0.0),
                    at(&r, 0, 0.0),
                    [at(&s, 0, 100.0) / 100.0, at(&s, 1, 100.0) / 100.0],
                    [0.0, 0.0],
                );
                base.then(&crate::canvas::rig::Affine {
                    m: [1.0, 0.0, 0.0, 1.0],
                    t: [-at(&a, 0, 0.0), -at(&a, 1, 0.0)],
                })
            });
        let frame = transform.then(&own);
        for item in items {
            match item.get("ty").and_then(Value::as_str) {
                Some("gr") => {
                    let inner = item
                        .get("it")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    group(&inner, &frame, out, notes, name);
                }
                Some("sh") => {
                    if moves(item.get("ks")) {
                        notes.push(format!("{name}: its moving paths came across still"));
                    }
                    if let Some((pts, closed)) = item.get("ks").and_then(path) {
                        polys.push((pts.into_iter().map(|p| frame.apply(p)).collect(), closed));
                    }
                }
                Some("rc") => {
                    let (p, s) = (value(item.get("p")), value(item.get("s")));
                    let (cx, cy, w, h) = (
                        p.first().copied().unwrap_or(0.0),
                        p.get(1).copied().unwrap_or(0.0),
                        s.first().copied().unwrap_or(0.0),
                        s.get(1).copied().unwrap_or(0.0),
                    );
                    let pts = [
                        [cx - w / 2.0, cy - h / 2.0],
                        [cx + w / 2.0, cy - h / 2.0],
                        [cx + w / 2.0, cy + h / 2.0],
                        [cx - w / 2.0, cy + h / 2.0],
                    ];
                    polys.push((pts.iter().map(|&p| frame.apply(p)).collect(), true));
                }
                Some("el") => {
                    let (p, s) = (value(item.get("p")), value(item.get("s")));
                    let (cx, cy) = (
                        p.first().copied().unwrap_or(0.0),
                        p.get(1).copied().unwrap_or(0.0),
                    );
                    let (rx, ry) = (
                        s.first().copied().unwrap_or(0.0) / 2.0,
                        s.get(1).copied().unwrap_or(0.0) / 2.0,
                    );
                    let pts = (0..64).map(|k| {
                        let t = k as f32 / 64.0 * std::f32::consts::TAU;
                        frame.apply([cx + rx * t.cos(), cy + ry * t.sin()])
                    });
                    polys.push((pts.collect(), true));
                }
                Some("fl") => {
                    let c = value(item.get("c"));
                    let o = value(item.get("o")).first().copied().unwrap_or(100.0) / 100.0;
                    fill = Some([
                        c.first().copied().unwrap_or(0.0),
                        c.get(1).copied().unwrap_or(0.0),
                        c.get(2).copied().unwrap_or(0.0),
                        o,
                    ]);
                }
                Some("st") => {
                    let c = value(item.get("c"));
                    let o = value(item.get("o")).first().copied().unwrap_or(100.0) / 100.0;
                    let w = value(item.get("w")).first().copied().unwrap_or(1.0);
                    let scale = (frame.m[0] * frame.m[3] - frame.m[1] * frame.m[2])
                        .abs()
                        .sqrt();
                    stroke = Some((
                        [
                            c.first().copied().unwrap_or(0.0),
                            c.get(1).copied().unwrap_or(0.0),
                            c.get(2).copied().unwrap_or(0.0),
                            o,
                        ],
                        w * scale,
                    ));
                }
                Some("tr") | None => {}
                Some(other) => notes.push(format!("{name}: shape item {other} is left out")),
            }
        }
        if !polys.is_empty() && (fill.is_some() || stroke.is_some()) {
            out.push(Paint {
                polys,
                fill,
                stroke,
            });
        }
    }

    /// The layer's shapes as a picture, and where its top left is in the
    /// layer's frame.
    pub(super) fn draw(
        items: &[Value],
        name: &str,
        notes: &mut Vec<String>,
    ) -> Option<(RigImage, [f32; 2])> {
        let mut paints = Vec::new();
        group(
            items,
            &crate::canvas::rig::Affine::IDENTITY,
            &mut paints,
            notes,
            name,
        );
        let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &paints {
            let pad = p.stroke.map_or(0.0, |s| s.1 / 2.0) + 1.0;
            for (poly, _) in &p.polys {
                for q in poly {
                    bounds = [
                        bounds[0].min(q[0] - pad),
                        bounds[1].min(q[1] - pad),
                        bounds[2].max(q[0] + pad),
                        bounds[3].max(q[1] + pad),
                    ];
                }
            }
        }
        if bounds[0] >= bounds[2] || !bounds.iter().all(|v| v.is_finite()) {
            return None;
        }
        let (x0, y0) = (bounds[0].floor(), bounds[1].floor());
        let (w, h) = (
            ((bounds[2] - x0).ceil() as usize).clamp(1, 4096),
            ((bounds[3] - y0).ceil() as usize).clamp(1, 4096),
        );
        // Painted in order, in linear light.
        let mut px = vec![[0.0f32; 4]; w * h];
        for p in &paints {
            if let Some(fill) = p.fill {
                let cover = fill_coverage(&p.polys, x0, y0, w, h);
                paint(&mut px, &cover, fill);
            }
            if let Some((color, width)) = p.stroke {
                let cover = stroke_coverage(&p.polys, width, x0, y0, w, h);
                paint(&mut px, &cover, color);
            }
        }
        let encoder = crate::canvas::blend::LinearEncoder::new();
        let pixels = px
            .iter()
            .map(|&[r, g, b, a]| {
                encoder.encode(eframe::egui::Rgba::from_rgba_premultiplied(r, g, b, a))
            })
            .collect();
        Some((
            RigImage {
                name: name.to_string(),
                width: w,
                height: h,
                pixels: Arc::new(pixels),
            },
            [x0, y0],
        ))
    }

    /// `color` over `px` by each pixel's `cover`.
    fn paint(px: &mut [[f32; 4]], cover: &[f32], [r, g, b, a]: [f32; 4]) {
        let lin = eframe::egui::ecolor::linear_from_gamma;
        let c = [
            lin(r.clamp(0.0, 1.0)),
            lin(g.clamp(0.0, 1.0)),
            lin(b.clamp(0.0, 1.0)),
        ];
        for (p, &k) in px.iter_mut().zip(cover) {
            let s = a * k;
            if s <= 0.0 {
                continue;
            }
            *p = [
                c[0] * s + p[0] * (1.0 - s),
                c[1] * s + p[1] * (1.0 - s),
                c[2] * s + p[2] * (1.0 - s),
                s + p[3] * (1.0 - s),
            ];
        }
    }

    /// How much of each pixel the closed outlines fill (non-zero winding,
    /// four rows of samples a pixel).
    fn fill_coverage(
        polys: &[(Vec<[f32; 2]>, bool)],
        x0: f32,
        y0: f32,
        w: usize,
        h: usize,
    ) -> Vec<f32> {
        let mut cover = vec![0.0f32; w * h];
        for row in 0..h {
            for sub in 0..4 {
                let y = y0 + row as f32 + (sub as f32 + 0.5) / 4.0;
                let mut crossings: Vec<(f32, i32)> = Vec::new();
                for (poly, _) in polys {
                    for k in 0..poly.len() {
                        let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                        if (a[1] <= y) != (b[1] <= y) {
                            let x = a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                            crossings.push((x, if b[1] > a[1] { 1 } else { -1 }));
                        }
                    }
                }
                crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut winding = 0;
                for pair in crossings.windows(2) {
                    winding += pair[0].1;
                    if winding == 0 {
                        continue;
                    }
                    let (l, r) = (pair[0].0 - x0, pair[1].0 - x0);
                    for x in (l.max(0.0).floor() as usize)..(r.max(0.0).ceil() as usize).min(w) {
                        let inside = (r.min(x as f32 + 1.0) - l.max(x as f32)).clamp(0.0, 1.0);
                        cover[row * w + x] += inside / 4.0;
                    }
                }
            }
        }
        cover.iter_mut().for_each(|c| *c = c.min(1.0));
        cover
    }

    /// How much of each pixel lines `width` wide along the outlines cover.
    fn stroke_coverage(
        polys: &[(Vec<[f32; 2]>, bool)],
        width: f32,
        x0: f32,
        y0: f32,
        w: usize,
        h: usize,
    ) -> Vec<f32> {
        let mut cover = vec![0.0f32; w * h];
        let half = (width / 2.0).max(0.5);
        for (poly, closed) in polys {
            let n = poly.len();
            let segments = if *closed { n } else { n.saturating_sub(1) };
            for k in 0..segments {
                let (a, b) = (poly[k], poly[(k + 1) % n]);
                let (lx, hx) = (
                    (a[0].min(b[0]) - half - x0).floor().max(0.0) as usize,
                    ((a[0].max(b[0]) + half - x0).ceil() as usize).min(w),
                );
                let (ly, hy) = (
                    (a[1].min(b[1]) - half - y0).floor().max(0.0) as usize,
                    ((a[1].max(b[1]) + half - y0).ceil() as usize).min(h),
                );
                let d = [b[0] - a[0], b[1] - a[1]];
                let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-6);
                for y in ly..hy {
                    for x in lx..hx {
                        let p = [x0 + x as f32 + 0.5, y0 + y as f32 + 0.5];
                        let t =
                            (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0);
                        let q = [a[0] + d[0] * t - p[0], a[1] + d[1] * t - p[1]];
                        let dist = (q[0] * q[0] + q[1] * q[1]).sqrt();
                        let c = (half + 0.5 - dist).clamp(0.0, 1.0);
                        let i = y * w + x;
                        cover[i] = cover[i].max(c);
                    }
                }
            }
        }
        cover
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::anim_import::InMemory;

    /// A 100×100 comp at 10 fps for 1 s: a red square (a shape layer)
    /// moving right from (20, 50) to (80, 50), parented to a null that
    /// does nothing, and a green circle fading in.
    const LOTTIE: &str = r#"{
      "v": "5.7.0", "fr": 10, "ip": 0, "op": 10, "w": 100, "h": 100, "nm": "slide",
      "layers": [
        { "ty": 4, "ind": 2, "parent": 1, "nm": "square", "ip": 0, "op": 10,
          "ks": {
            "p": { "a": 1, "k": [
              { "t": 0, "s": [20, 50], "o": { "x": [0.5], "y": [0] }, "i": { "x": [0.5], "y": [1] } },
              { "t": 10, "s": [80, 50] } ] },
            "a": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [100, 100] },
            "r": { "a": 0, "k": 0 }, "o": { "a": 0, "k": 100 } },
          "shapes": [ { "ty": "gr", "it": [
            { "ty": "rc", "p": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [10, 10] } },
            { "ty": "fl", "c": { "a": 0, "k": [1, 0, 0, 1] }, "o": { "a": 0, "k": 100 } },
            { "ty": "tr", "p": { "a": 0, "k": [0, 0] }, "a": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [100, 100] }, "r": { "a": 0, "k": 0 } }
          ] } ] },
        { "ty": 3, "ind": 1, "nm": "null", "ks": {} },
        { "ty": 4, "ind": 3, "nm": "circle", "ip": 0, "op": 10,
          "ks": { "p": { "a": 0, "k": [50, 20] },
            "o": { "a": 1, "k": [ { "t": 0, "s": [0] }, { "t": 10, "s": [100] } ] } },
          "shapes": [
            { "ty": "el", "p": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [12, 12] } },
            { "ty": "fl", "c": { "a": 0, "k": [0, 1, 0, 1] }, "o": { "a": 0, "k": 100 } } ] }
      ]
    }"#;

    #[test]
    fn a_lottie_animation_comes_across_moving_and_fading() {
        let imported = import(LOTTIE.as_bytes(), &InMemory::default()).unwrap();
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);
        assert_eq!(imported.fps, Some(10));
        let rig = imported.rig;
        assert_eq!(rig.animations[0].duration, 1.0);
        let px = |t: f32, x: usize, y: usize| {
            let tiles = rig.render(&rig.pose(t), 100, 100, 64);
            (tiles.iter())
                .find(|(k, _)| *k == ((x / 64) as i32, (y / 64) as i32))
                .map_or(Color32::TRANSPARENT, |(_, d)| d[(y % 64) * 64 + x % 64])
        };
        assert_eq!(px(0.0, 20, 50), Color32::RED);
        assert_eq!(px(0.0, 50, 50).a(), 0);
        // Half way, eased in and out: in the middle.
        assert_eq!(px(0.5, 50, 50), Color32::RED);
        assert!(px(0.95, 78, 50).r() > 200);
        // The circle fades in.
        assert_eq!(px(0.0, 50, 20).a(), 0);
        assert!(
            px(0.5, 50, 20).a() > 100 && px(0.5, 50, 20).a() < 160,
            "{:?}",
            px(0.5, 50, 20)
        );
        assert!(px(0.99, 50, 20).g() > 200);
    }

    #[test]
    fn embedded_pictures_and_damage() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
        assert!(base64_decode("a*b").is_none());
        for cut in [0, 20, LOTTIE.len() / 2] {
            let _ = import(&LOTTIE.as_bytes()[..cut], &InMemory::default());
        }
        // Precomps in a loop are refused, not followed for ever.
        let looped = r#"{ "fr": 10, "w": 10, "h": 10, "layers": [ { "ty": 0, "refId": "a", "ks": {} } ],
            "assets": [ { "id": "a", "layers": [ { "ty": 0, "refId": "a", "ks": {} } ] } ] }"#;
        assert!(import(looped.as_bytes(), &InMemory::default()).is_err());
    }
}
