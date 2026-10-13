//! Binding a rig's mesh parts to its bones: each point weighted between
//! the bones nearest it *along the mesh* (so an arm's points don't follow
//! a body bone across the gap between them), and moving points' offsets
//! (deforms) between the bones' frames.

use super::eval::Pose;
use super::{Affine, MeshVertices, Rig, Shape};

/// Most bones a point follows.
const MOST: usize = 4;

/// Each point's bones and weights (summing to 1): `bones` are (bone, head,
/// tail), points and bones in one frame. A bone's distance to a point is
/// along the mesh's edges from the points nearest the bone.
pub fn auto_weights(
    points: &[[f32; 2]],
    tris: &[[u32; 3]],
    bones: &[(usize, [f32; 2], [f32; 2])],
) -> Vec<Vec<(usize, f32)>> {
    let n = points.len();
    let mut nbrs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for t in tris {
        let t = t.map(|k| k as usize);
        if t.iter().any(|&k| k >= n) {
            continue;
        }
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            nbrs[a].push(b);
            nbrs[b].push(a);
        }
    }
    let len = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    // The mesh's usual edge, for how near "on the bone" is.
    let edge = tris
        .iter()
        .filter(|t| t.iter().all(|&k| (k as usize) < n))
        .map(|t| len(points[t[0] as usize], points[t[1] as usize]))
        .sum::<f32>()
        / tris.len().max(1) as f32;
    let dists: Vec<Vec<f32>> = bones
        .iter()
        .map(|&(_, head, tail)| {
            let straight: Vec<f32> = points.iter().map(|&p| to_segment(p, head, tail)).collect();
            // From the points of the triangles the bone runs over (else
            // the points nearest it), along the mesh.
            let mut dist = vec![f32::INFINITY; n];
            let mut heap = std::collections::BinaryHeap::new();
            let steps = ((len(head, tail) / (edge.max(1.0) * 0.5)).ceil() as usize).clamp(1, 512);
            let mut seeded = false;
            for k in 0..=steps {
                let u = k as f32 / steps as f32;
                let q = [
                    head[0] + (tail[0] - head[0]) * u,
                    head[1] + (tail[1] - head[1]) * u,
                ];
                for t in tris.iter().filter(|t| t.iter().all(|&k| (k as usize) < n)) {
                    let [a, b, c] = t.map(|k| points[k as usize]);
                    if !inside(q, a, b, c) {
                        continue;
                    }
                    for &v in t {
                        let v = v as usize;
                        if straight[v] < dist[v] {
                            dist[v] = straight[v];
                            heap.push((std::cmp::Reverse(Ord(straight[v])), v));
                            seeded = true;
                        }
                    }
                }
            }
            if !seeded {
                let nearest = straight.iter().copied().fold(f32::INFINITY, f32::min);
                for (i, &d) in straight.iter().enumerate() {
                    if d <= nearest + edge * 0.5 {
                        dist[i] = d;
                        heap.push((std::cmp::Reverse(Ord(d)), i));
                    }
                }
            }
            while let Some((std::cmp::Reverse(Ord(d)), v)) = heap.pop() {
                if d > dist[v] {
                    continue;
                }
                for &u in &nbrs[v] {
                    let nd = d + len(points[v], points[u]);
                    if nd < dist[u] {
                        dist[u] = nd;
                        heap.push((std::cmp::Reverse(Ord(nd)), u));
                    }
                }
            }
            // A piece no bone is near: straight distance.
            for (d, s) in dist.iter_mut().zip(&straight) {
                if !d.is_finite() {
                    *d = s + 1e6;
                }
            }
            dist
        })
        .collect();
    let eps = (edge * 0.25).max(0.5).powi(2);
    (0..n)
        .map(|i| {
            let mut w: Vec<(usize, f32)> = (bones.iter().zip(&dists))
                .map(|(&(b, ..), d)| (b, 1.0 / (d[i] * d[i] + eps)))
                .collect();
            w.sort_by(|a, b| b.1.total_cmp(&a.1));
            w.truncate(MOST);
            // Next to nothing: dropped.
            let top = w.first().map_or(0.0, |x| x.1);
            w.retain(|x| x.1 >= top * 0.01);
            let sum: f32 = w.iter().map(|x| x.1).sum();
            w.iter_mut().for_each(|x| x.1 /= sum.max(1e-12));
            w
        })
        .collect()
}

/// Whether `p` is in triangle `a b c` (edges included).
fn inside(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let cross =
        |u: [f32; 2], v: [f32; 2]| (v[0] - u[0]) * (p[1] - u[1]) - (v[1] - u[1]) * (p[0] - u[0]);
    let (d0, d1, d2) = (cross(a, b), cross(b, c), cross(c, a));
    !((d0 < 0.0 || d1 < 0.0 || d2 < 0.0) && (d0 > 0.0 || d1 > 0.0 || d2 > 0.0))
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 1e-12 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
}

/// An `f32` that sorts (distances, never NaN here).
#[derive(Clone, Copy, PartialEq)]
struct Ord(f32);
impl Eq for Ord {}
impl PartialOrd for Ord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(std::cmp::Ord::cmp(self, other))
    }
}
impl std::cmp::Ord for Ord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// `a`'s linear part (no translation).
fn linear(a: &Affine) -> Affine {
    Affine {
        t: [0.0, 0.0],
        ..*a
    }
}

/// How far each point of a mesh is moved (rig frame) by `offsets` (its
/// deform), its bones placed as `world` has them.
pub fn offsets_to_moves(
    world: &[Affine],
    vertices: &MeshVertices,
    slot_bone: usize,
    offsets: &[[f32; 2]],
) -> Vec<[f32; 2]> {
    let frame = |b: usize| linear(&world.get(b).copied().unwrap_or_default());
    match vertices {
        MeshVertices::Plain(v) => {
            let f = frame(slot_bone);
            (0..v.len())
                .map(|i| f.apply(offsets.get(i).copied().unwrap_or_default()))
                .collect()
        }
        MeshVertices::Weighted(v) => {
            let mut k = 0;
            v.iter()
                .map(|influences| {
                    let mut sum = [0.0, 0.0];
                    for &(b, _, w) in influences {
                        let d = frame(b).apply(offsets.get(k).copied().unwrap_or_default());
                        k += 1;
                        sum = [sum[0] + d[0] * w, sum[1] + d[1] * w];
                    }
                    sum
                })
                .collect()
        }
    }
}

/// The deform that moves each point of a mesh by `moves` (rig frame), its
/// bones placed as `world` has them: one offset a point (plain), or one
/// for each of its bones (weighted).
pub fn moves_to_offsets(
    world: &[Affine],
    vertices: &MeshVertices,
    slot_bone: usize,
    moves: &[[f32; 2]],
) -> Vec<[f32; 2]> {
    let back = |b: usize| {
        linear(&world.get(b).copied().unwrap_or_default())
            .inverse()
            .unwrap_or(Affine::IDENTITY)
    };
    match vertices {
        MeshVertices::Plain(v) => {
            let f = back(slot_bone);
            (0..v.len())
                .map(|i| f.apply(moves.get(i).copied().unwrap_or_default()))
                .collect()
        }
        MeshVertices::Weighted(v) => {
            let mut out = Vec::new();
            for (i, influences) in v.iter().enumerate() {
                let total: f32 = influences.iter().map(|x| x.2).sum();
                let m = moves.get(i).copied().unwrap_or_default();
                // Each bone's share moves it the whole way: the weights sum
                // to one.
                let m = if total > 1e-6 && (total - 1.0).abs() > 1e-4 {
                    [m[0] / total, m[1] / total]
                } else {
                    m
                };
                out.extend(influences.iter().map(|&(b, ..)| back(b).apply(m)));
            }
            out
        }
    }
}

impl Rig {
    /// The rig in its setup pose (no animation, no IK).
    pub fn setup_pose(&self) -> Pose {
        Pose {
            world: self.setup_world(),
            colors: self.slots.iter().map(|s| s.color).collect(),
            attachments: (self.slots.iter())
                .map(|s| {
                    let name = s.attachment.as_ref()?;
                    s.attachments.iter().position(|a| &a.name == name)
                })
                .collect(),
            deforms: Vec::new(),
        }
    }

    /// Bind slot `s`'s mesh pictures to the bones, as they are in the
    /// setup pose: `smooth`, each point weighted between the bones near it;
    /// else all of it on the one bone nearest most of it. Their deform keys
    /// are carried over (the drawing doesn't move).
    pub fn bind(&mut self, s: usize, smooth: bool) {
        let world = self.setup_world();
        let pose = self.setup_pose();
        let Some(back) = self.to_canvas().inverse() else {
            return;
        };
        // The bones as segments, rig frame (a bone with no length only
        // when there's no other).
        let mut bones: Vec<(usize, [f32; 2], [f32; 2])> = (self.bones.iter().enumerate())
            .filter(|(_, b)| b.length > 0.0)
            .map(|(i, b)| (i, world[i].t, world[i].apply([b.length, 0.0])))
            .collect();
        if bones.is_empty() {
            bones = (0..self.bones.len())
                .map(|i| (i, world[i].t, world[i].t))
                .collect();
        }
        if bones.is_empty() {
            return;
        }
        let Some(slot) = self.slots.get(s) else {
            return;
        };
        let slot_bone = slot.bone;
        for ai in 0..slot.attachments.len() {
            let a = &self.slots[s].attachments[ai];
            let Shape::Mesh {
                triangles,
                vertices,
                ..
            } = &a.shape
            else {
                continue;
            };
            let Some((canvas, _, _)) = self.attachment_mesh(&pose, s, ai, false) else {
                continue;
            };
            let points: Vec<[f32; 2]> = canvas.iter().map(|&p| back.apply(p)).collect();
            let weights = auto_weights(&points, triangles, &bones);
            let old = vertices.clone();
            let name = a.name.clone();
            let (new, new_bone) = if smooth {
                let v = (points.iter().zip(&weights))
                    .map(|(&p, ws)| {
                        ws.iter()
                            .map(|&(b, w)| {
                                let inv = world[b].inverse().unwrap_or(Affine::IDENTITY);
                                (b, inv.apply(p), w)
                            })
                            .collect()
                    })
                    .collect();
                (MeshVertices::Weighted(v), slot_bone)
            } else {
                let mut total = vec![0.0f32; self.bones.len()];
                for ws in &weights {
                    for &(b, w) in ws {
                        total[b] += w;
                    }
                }
                let b = (0..total.len())
                    .max_by(|&x, &y| total[x].total_cmp(&total[y]))
                    .unwrap_or(slot_bone);
                let inv = world[b].inverse().unwrap_or(Affine::IDENTITY);
                (
                    MeshVertices::Plain(points.iter().map(|&p| inv.apply(p)).collect()),
                    b,
                )
            };
            // The deform keys, moved over to the new frames.
            for anim in &mut self.animations {
                for track in
                    (anim.deforms.iter_mut()).filter(|t| t.slot == s && t.attachment == name)
                {
                    for key in &mut track.keys {
                        let moves = offsets_to_moves(&world, &old, slot_bone, &key.value);
                        key.value = moves_to_offsets(&world, &new, new_bone, &moves);
                    }
                }
            }
            if let Shape::Mesh { vertices, .. } = &mut self.slots[s].attachments[ai].shape {
                *vertices = new;
            }
            if !smooth {
                self.slots[s].bone = new_bone;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar of points, `x0..x1` long, two points tall, in triangles.
    fn bar(x0: f32, x1: f32, n: usize) -> (Vec<[f32; 2]>, Vec<[u32; 3]>) {
        let mut points = Vec::new();
        for i in 0..=n {
            let x = x0 + (x1 - x0) * i as f32 / n as f32;
            points.push([x, 0.0]);
            points.push([x, 10.0]);
        }
        let tris = (0..n as u32)
            .flat_map(|i| {
                [
                    [2 * i, 2 * i + 2, 2 * i + 1],
                    [2 * i + 1, 2 * i + 2, 2 * i + 3],
                ]
            })
            .collect();
        (points, tris)
    }

    #[test]
    fn weights_sum_to_one_and_a_point_on_a_bone_follows_it() {
        let (points, tris) = bar(0.0, 100.0, 10);
        let bones = [(0, [0.0, 5.0], [50.0, 5.0]), (1, [50.0, 5.0], [100.0, 5.0])];
        let w = auto_weights(&points, &tris, &bones);
        for ws in &w {
            let sum: f32 = ws.iter().map(|x| x.1).sum();
            assert!((sum - 1.0).abs() < 1e-4);
        }
        let first = |ws: &Vec<(usize, f32)>| ws.iter().find(|x| x.0 == 0).map_or(0.0, |x| x.1);
        assert!(first(&w[0]) > 0.95, "{:?}", w[0]);
        assert!(first(&w[20]) < 0.05, "{:?}", w[20]);
    }

    #[test]
    fn across_a_gap_a_part_ignores_the_other_parts_bone() {
        // Two bars a few pixels apart, a bone along each.
        let (mut points, mut tris) = bar(0.0, 100.0, 10);
        let (p2, t2) = bar(0.0, 100.0, 10);
        let off = points.len() as u32;
        points.extend(p2.iter().map(|p| [p[0], p[1] + 14.0]));
        tris.extend(t2.iter().map(|t| t.map(|k| k + off)));
        let bones = [
            (0, [0.0, 5.0], [100.0, 5.0]),
            (1, [0.0, 19.0], [100.0, 19.0]),
        ];
        let w = auto_weights(&points, &tris, &bones);
        // The top bar's lower edge is 4 px from the bottom bar: still
        // its own bone's.
        for ws in &w[..off as usize] {
            assert!(ws.iter().all(|x| x.0 == 0), "{ws:?}");
        }
        for ws in &w[off as usize..] {
            assert!(ws.iter().all(|x| x.0 == 1), "{ws:?}");
        }
    }

    #[test]
    fn moves_and_offsets_go_back_and_forth() {
        let world = vec![
            Affine::local(3.0, 4.0, 30.0, [2.0, 1.0], [0.0, 0.0]),
            Affine::local(-5.0, 1.0, -70.0, [1.0, 1.0], [0.0, 0.0]),
        ];
        let moves = vec![[1.0, 2.0], [-3.0, 0.5]];
        for v in [
            MeshVertices::Plain(vec![[0.0, 0.0], [1.0, 1.0]]),
            MeshVertices::Weighted(vec![
                vec![(0, [0.0, 0.0], 0.25), (1, [1.0, 0.0], 0.75)],
                vec![(1, [0.0, 2.0], 1.0)],
            ]),
        ] {
            let offsets = moves_to_offsets(&world, &v, 0, &moves);
            let back = offsets_to_moves(&world, &v, 0, &offsets);
            for (a, b) in back.iter().zip(&moves) {
                assert!((a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4);
            }
        }
    }
}
