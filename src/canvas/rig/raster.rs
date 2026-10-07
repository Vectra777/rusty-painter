//! Drawing a posed rig: each slot's attachment as textured triangles (a
//! region is two), in draw order, onto the canvas's tiles. Edges are
//! sampled four times a pixel; pictures are read bilinearly.

use super::eval::Pose;
use super::{Attachment, MeshVertices, Rig, Shape, SlotBlend};
use eframe::egui::{Color32, Rgba};
use rayon::prelude::*;

/// A triangle ready to draw: canvas points, picture points (pixels), which
/// picture, the slot's tint (premultiplied, linear) and blend.
struct Tri {
    /// Its attachment's place in draw order (one attachment's triangles
    /// are drawn together, so their shared edges don't show).
    group: usize,
    p: [[f32; 2]; 3],
    uv: [[f32; 2]; 3],
    image: usize,
    tint: [f32; 4],
    blend: SlotBlend,
}

/// An attachment's points (canvas), their picture points (0..1) and its
/// triangles.
type Mesh = (Vec<[f32; 2]>, Vec<[f32; 2]>, Vec<[u32; 3]>);

/// Where on its picture an attachment's point `(u, v)` (0..1 across its
/// part, as the picture is) is, in pixels.
fn picture_point(a: &Attachment, [u, v]: [f32; 2]) -> [f32; 2] {
    let [x0, y0, x1, y1] = a.region;
    if a.rotated {
        // Packed turned a quarter clockwise.
        [x0 + (1.0 - v) * (x1 - x0), y0 + u * (y1 - y0)]
    } else {
        [x0 + u * (x1 - x0), y0 + v * (y1 - y0)]
    }
}

impl Rig {
    /// The triangles of the rig in `pose`, in draw order.
    fn triangles(&self, pose: &Pose) -> Vec<Tri> {
        let to_canvas = self.to_canvas();
        let mut out = Vec::new();
        for (s, slot) in self.slots.iter().enumerate() {
            let Some(ai) = pose.attachments[s] else {
                continue;
            };
            let Some(a) = slot.attachments.get(ai) else {
                continue;
            };
            if a.image >= self.images.len() {
                continue;
            }
            let [r, g, b, alpha] = pose.colors[s];
            if alpha <= 0.0 {
                continue;
            }
            let lin = |v: f32| eframe::egui::ecolor::linear_from_gamma(v.clamp(0.0, 1.0));
            let tint = [lin(r) * alpha, lin(g) * alpha, lin(b) * alpha, alpha];
            let bone = pose.world.get(slot.bone).copied().unwrap_or_default();
            let deform = (pose.deforms.iter())
                .find(|(key, _)| *key == (s, ai))
                .map(|(_, d)| d.as_slice());
            let (points, uvs, tris): Mesh = match &a.shape {
                Shape::Region {
                    x,
                    y,
                    rotation,
                    scale,
                    width,
                    height,
                    trim,
                } => {
                    let local = super::Affine::local(*x, *y, *rotation, *scale, [0.0, 0.0]);
                    let frame = to_canvas.then(&bone).then(&local);
                    let [l, t, r, b] = *trim;
                    // The picture's top is the rig's up (or down).
                    let up = if self.y_up { 1.0 } else { -1.0 };
                    let corner =
                        |u: f32, v: f32| frame.apply([(u - 0.5) * width, (0.5 - v) * height * up]);
                    let points = vec![corner(l, t), corner(r, t), corner(r, b), corner(l, b)];
                    let uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
                    (points, uvs, vec![[0, 1, 2], [0, 2, 3]])
                }
                Shape::Mesh {
                    uvs,
                    triangles,
                    vertices,
                } => {
                    let points = match vertices {
                        MeshVertices::Plain(v) => {
                            let frame = to_canvas.then(&bone);
                            (v.iter().enumerate())
                                .map(|(i, &p)| {
                                    let d =
                                        deform.and_then(|d| d.get(i)).copied().unwrap_or_default();
                                    frame.apply([p[0] + d[0], p[1] + d[1]])
                                })
                                .collect()
                        }
                        MeshVertices::Weighted(v) => {
                            // Each bone's offset in turn, through the list.
                            let mut k = 0;
                            v.iter()
                                .map(|influences| {
                                    let mut sum = [0.0, 0.0];
                                    for &(b, p, w) in influences {
                                        let d = deform
                                            .and_then(|d| d.get(k))
                                            .copied()
                                            .unwrap_or_default();
                                        k += 1;
                                        let at = pose
                                            .world
                                            .get(b)
                                            .copied()
                                            .unwrap_or_default()
                                            .apply([p[0] + d[0], p[1] + d[1]]);
                                        sum[0] += at[0] * w;
                                        sum[1] += at[1] * w;
                                    }
                                    to_canvas.apply(sum)
                                })
                                .collect()
                        }
                    };
                    (points, uvs.clone(), triangles.clone())
                }
            };
            for t in tris {
                let idx = t.map(|i| i as usize);
                if idx.iter().any(|&i| i >= points.len() || i >= uvs.len()) {
                    continue;
                }
                out.push(Tri {
                    group: s,
                    p: idx.map(|i| points[i]),
                    uv: idx.map(|i| picture_point(a, uvs[i])),
                    image: a.image,
                    tint,
                    blend: slot.blend,
                });
            }
        }
        out
    }

    /// The rig in `pose` on a `width`×`height` canvas: its painted tiles
    /// (`tile_size` pixels a side).
    pub fn render(
        &self,
        pose: &Pose,
        width: usize,
        height: usize,
        tile_size: usize,
    ) -> Vec<((i32, i32), Vec<Color32>)> {
        let tris = self.triangles(pose);
        let ts = tile_size as i32;
        let (tw, th) = ((width as i32 + ts - 1) / ts, (height as i32 + ts - 1) / ts);
        // Which triangles reach each tile.
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); (tw * th).max(0) as usize];
        for (i, t) in tris.iter().enumerate() {
            let xs = t.p.map(|p| p[0]);
            let ys = t.p.map(|p| p[1]);
            let (x0, x1) = (
                xs.iter().copied().fold(f32::MAX, f32::min),
                xs.iter().copied().fold(f32::MIN, f32::max),
            );
            let (y0, y1) = (
                ys.iter().copied().fold(f32::MAX, f32::min),
                ys.iter().copied().fold(f32::MIN, f32::max),
            );
            if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
                continue;
            }
            let tx0 = ((x0 as i32) / ts).clamp(0, tw - 1);
            let tx1 = ((x1 as i32) / ts).clamp(0, tw - 1);
            let ty0 = ((y0 as i32) / ts).clamp(0, th - 1);
            let ty1 = ((y1 as i32) / ts).clamp(0, th - 1);
            if x1 < 0.0 || y1 < 0.0 || x0 >= width as f32 || y0 >= height as f32 {
                continue;
            }
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    buckets[(ty * tw + tx) as usize].push(i);
                }
            }
        }
        (0..buckets.len())
            .into_par_iter()
            .filter(|&b| !buckets[b].is_empty())
            .filter_map(|b| {
                let (tx, ty) = (b as i32 % tw, b as i32 / tw);
                let mut px = vec![[0.0f32; 4]; tile_size * tile_size];
                let list = &buckets[b];
                let mut start = 0;
                while start < list.len() {
                    let group = tris[list[start]].group;
                    let end = (start..list.len())
                        .find(|&k| tris[list[k]].group != group)
                        .unwrap_or(list.len());
                    let part: Vec<&Tri> = list[start..end].iter().map(|&i| &tris[i]).collect();
                    self.fill(&part, (tx * ts, ty * ts), tile_size, &mut px);
                    start = end;
                }
                let encoder = crate::canvas::blend::LinearEncoder::new();
                let data: Vec<Color32> = px
                    .iter()
                    .map(|&[r, g, b, a]| encoder.encode(Rgba::from_rgba_premultiplied(r, g, b, a)))
                    .collect();
                data.iter().any(|p| p.a() > 0).then_some(((tx, ty), data))
            })
            .collect()
    }

    /// Paint one attachment's triangles into a tile's linear premultiplied
    /// pixels (tile at canvas `origin`): each pixel covered by as many of
    /// its four samples as fall in any of them, coloured from the one its
    /// middle (or a sample) is in.
    fn fill(&self, tris: &[&Tri], origin: (i32, i32), side: usize, px: &mut [[f32; 4]]) {
        let Some(first) = tris.first() else {
            return;
        };
        let image = &self.images[first.image];
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for t in tris {
            for p in t.p {
                (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
            }
        }
        let lx0 = (x0.floor() as i32 - origin.0).max(0);
        let lx1 = (x1.ceil() as i32 - origin.0).min(side as i32);
        let ly0 = (y0.floor() as i32 - origin.1).max(0);
        let ly1 = (y1.ceil() as i32 - origin.1).min(side as i32);
        // Barycentric weights of a point in a triangle (`None`: degenerate).
        let weights = |t: &Tri, x: f32, y: f32| -> Option<[f32; 3]> {
            let [a, b, c] = t.p;
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if area.abs() < 1e-6 {
                return None;
            }
            let w0 = ((b[0] - x) * (c[1] - y) - (b[1] - y) * (c[0] - x)) / area;
            let w1 = ((c[0] - x) * (a[1] - y) - (c[1] - y) * (a[0] - x)) / area;
            Some([w0, w1, 1.0 - w0 - w1])
        };
        let inside = |w: &[f32; 3]| w.iter().all(|&v| v >= -1e-5);
        const SUB: [[f32; 2]; 5] = [
            [0.5, 0.5],
            [0.25, 0.25],
            [0.75, 0.25],
            [0.25, 0.75],
            [0.75, 0.75],
        ];
        for ly in ly0..ly1 {
            for lx in lx0..lx1 {
                let (x, y) = ((origin.0 + lx) as f32, (origin.1 + ly) as f32);
                let hits = SUB[1..]
                    .iter()
                    .filter(|o| {
                        tris.iter()
                            .any(|t| weights(t, x + o[0], y + o[1]).is_some_and(|w| inside(&w)))
                    })
                    .count();
                if hits == 0 {
                    continue;
                }
                // The colour from where the middle is (else a sample).
                let Some((t, w)) = SUB.iter().find_map(|o| {
                    tris.iter().find_map(|t| {
                        let w = weights(t, x + o[0], y + o[1])?;
                        inside(&w).then_some((*t, w))
                    })
                }) else {
                    continue;
                };
                let u = t.uv[0][0] * w[0] + t.uv[1][0] * w[1] + t.uv[2][0] * w[2];
                let v = t.uv[0][1] * w[0] + t.uv[1][1] * w[1] + t.uv[2][1] * w[2];
                let s = sample(image, u, v);
                let cover = hits as f32 / 4.0;
                // (The tint is premultiplied: colour and opacity at once.)
                let src: [f32; 4] = std::array::from_fn(|k| s[k] * t.tint[k] * cover);
                let i = ly as usize * side + lx as usize;
                px[i] = over(src, px[i], t.blend);
            }
        }
    }
}

/// `src` over `dst` (premultiplied, linear) with a slot's blend.
fn over(src: [f32; 4], dst: [f32; 4], blend: SlotBlend) -> [f32; 4] {
    let keep = 1.0 - src[3];
    match blend {
        SlotBlend::Normal => std::array::from_fn(|k| src[k] + dst[k] * keep),
        SlotBlend::Additive => {
            let mut out: [f32; 4] = std::array::from_fn(|k| src[k] + dst[k]);
            out[3] = src[3] + dst[3] * keep;
            out
        }
        SlotBlend::Multiply => {
            let mut out: [f32; 4] =
                std::array::from_fn(|k| src[k] * dst[k] + src[k] * (1.0 - dst[3]) + dst[k] * keep);
            out[3] = src[3] + dst[3] * keep;
            out
        }
        SlotBlend::Screen => {
            let mut out: [f32; 4] = std::array::from_fn(|k| src[k] + dst[k] - src[k] * dst[k]);
            out[3] = src[3] + dst[3] * keep;
            out
        }
    }
}

/// A picture read at pixel point `(u, v)` (bilinear), premultiplied
/// linear light; transparent outside.
fn sample(image: &super::RigImage, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (image.width as i32, image.height as i32);
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |px: i32, py: i32| -> [f32; 4] {
        if px < 0 || py < 0 || px >= w || py >= h {
            return [0.0; 4];
        }
        Rgba::from(image.pixels[(py * w + px) as usize]).to_array()
    };
    let (a, b, c, d) = (
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    );
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        top + (bottom - top) * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::rig::{Bone, RigImage, Slot};
    use std::sync::Arc;

    /// A rig with one bone and an 8×4 picture, red on its left half and
    /// blue on its right, as a region at the origin.
    pub(crate) fn rig(y_up: bool, shape: Shape) -> Rig {
        let pixels = (0..32)
            .map(|i| {
                if i % 8 < 4 {
                    Color32::RED
                } else {
                    Color32::BLUE
                }
            })
            .collect();
        Rig {
            bones: vec![Bone {
                name: "root".into(),
                ..Default::default()
            }],
            slots: vec![Slot {
                name: "body".into(),
                attachment: Some("pic".into()),
                attachments: vec![Attachment {
                    name: "pic".into(),
                    image: 0,
                    region: [0.0, 0.0, 8.0, 4.0],
                    rotated: false,
                    shape,
                }],
                ..Default::default()
            }],
            images: vec![RigImage {
                name: "pic".into(),
                width: 8,
                height: 4,
                pixels: Arc::new(pixels),
            }],
            origin: [32.0, 32.0],
            scale: 4.0,
            y_up,
            ..Default::default()
        }
    }

    fn region() -> Shape {
        Shape::Region {
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            scale: [1.0, 1.0],
            width: 8.0,
            height: 4.0,
            trim: [0.0, 0.0, 1.0, 1.0],
        }
    }

    fn at(tiles: &[((i32, i32), Vec<Color32>)], x: usize, y: usize) -> Color32 {
        tiles
            .iter()
            .find(|(k, _)| *k == ((x / 64) as i32, (y / 64) as i32))
            .map_or(Color32::TRANSPARENT, |(_, d)| d[(y % 64) * 64 + x % 64])
    }

    #[test]
    fn a_region_lands_where_its_bone_puts_it() {
        let rig = rig(true, region());
        let tiles = rig.render(&rig.pose(0.0), 128, 128, 64);
        // 32 px wide (8 × 4), 16 tall, centred on (32, 32).
        assert_eq!(at(&tiles, 20, 32), Color32::RED);
        assert_eq!(at(&tiles, 44, 32), Color32::BLUE);
        assert_eq!(at(&tiles, 32, 45), Color32::TRANSPARENT, "below it");
        assert_eq!(at(&tiles, 10, 32), Color32::TRANSPARENT, "left of it");
        // Turned half round: blue on the left.
        let mut turned = rig.clone();
        turned.bones[0].rotation = 180.0;
        let tiles = turned.render(&turned.pose(0.0), 128, 128, 64);
        assert_eq!(at(&tiles, 20, 32), Color32::BLUE);
    }

    #[test]
    fn a_mesh_bends_with_its_bones_and_the_tint_shows() {
        // A mesh of the same rectangle, its right side on a second bone.
        let mut r = rig(
            false,
            Shape::Mesh {
                uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                triangles: vec![[0, 1, 2], [0, 2, 3]],
                vertices: MeshVertices::Weighted(vec![
                    vec![(0, [-4.0, -2.0], 1.0)],
                    vec![(1, [0.0, -2.0], 1.0)],
                    vec![(1, [0.0, 2.0], 1.0)],
                    vec![(0, [-4.0, 2.0], 1.0)],
                ]),
            },
        );
        r.bones.push(Bone {
            name: "tip".into(),
            parent: Some(0),
            x: 4.0,
            ..Default::default()
        });
        let tiles = r.render(&r.pose(0.0), 128, 128, 64);
        assert_eq!(at(&tiles, 44, 32), Color32::BLUE);
        // The tip bone moved down 4 rig units: the right end follows.
        r.bones[1].y = 4.0;
        let tiles = r.render(&r.pose(0.0), 128, 128, 64);
        assert_eq!(at(&tiles, 46, 26), Color32::TRANSPARENT);
        assert_eq!(at(&tiles, 44, 48), Color32::BLUE);
        // Half-transparent tint.
        r.slots[0].color = [1.0, 1.0, 1.0, 0.5];
        let tiles = r.render(&r.pose(0.0), 128, 128, 64);
        let a = at(&tiles, 20, 32).a();
        assert!((120..=135).contains(&a), "{a}");
    }
}
