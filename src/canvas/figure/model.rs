//! A figure's model: one skinned mesh (every part of the model merged,
//! each vertex weighted to up to four joints), its skeleton and its morph
//! targets, read from glTF 2.0 (`.gltf` with its buffers inline, or `.glb`).
//!
//! Only what a posable reference figure needs is read: positions, normals,
//! the material's base colour (as vertex colour), the first skin, and morph
//! targets. Textures, cameras, lights and animations are left out.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use glam::{Mat4, Quat, Vec3};

use super::skeleton::{Joint, JointLimit, RestTransform, Skeleton};

/// Most joints a model may have: the GPU takes the skinning matrices as
/// one uniform array, and 256 of them fill the 16 KiB every GPU allows.
pub const MAX_JOINTS: usize = 256;

/// A mesh, every vertex skinned.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Straight sRGB colour.
    pub colors: Vec<[u8; 4]>,
    /// Up to four joints per vertex and their weights (summing to 1).
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    /// Triangles.
    pub indices: Vec<u32>,
}

/// A morph target: how far it moves the vertices it moves, at weight 1.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphTarget {
    pub name: String,
    pub deltas: Vec<(u32, [f32; 3])>,
}

/// A loaded model.
#[derive(Debug)]
pub struct FigureModel {
    /// Different for every model loaded (GPU caches key on it).
    pub uid: u64,
    pub name: String,
    pub skeleton: Skeleton,
    pub mesh: Mesh,
    pub morphs: Vec<MorphTarget>,
}

fn next_uid() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl FigureModel {
    /// A model of `skeleton` and `mesh`; its skeleton's joints are mapped
    /// to the canonical humanoid ones by name, and those get human limits.
    pub fn new(
        name: impl Into<String>,
        mut skeleton: Skeleton,
        mesh: Mesh,
        morphs: Vec<MorphTarget>,
    ) -> Self {
        skeleton.map_human_names();
        skeleton.apply_human_limits();
        Self {
            uid: next_uid(),
            name: name.into(),
            skeleton,
            mesh,
            morphs,
        }
    }

    /// The mesh's positions with the morph targets applied at `weights`
    /// (by index; missing ones are 0).
    pub fn morphed_positions(&self, weights: &[f32]) -> Vec<[f32; 3]> {
        let mut positions = self.mesh.positions.clone();
        for (target, &w) in self.morphs.iter().zip(weights) {
            if w == 0.0 {
                continue;
            }
            for &(v, d) in &target.deltas {
                if let Some(p) = positions.get_mut(v as usize) {
                    for k in 0..3 {
                        p[k] += d[k] * w;
                    }
                }
            }
        }
        positions
    }

    /// The rest pose's bounds (lowest and highest corner).
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for p in &self.mesh.positions {
            lo = lo.min(Vec3::from(*p));
            hi = hi.max(Vec3::from(*p));
        }
        if lo.x > hi.x {
            (Vec3::ZERO, Vec3::ZERO)
        } else {
            (lo, hi)
        }
    }
}

/// Decode standard base64 (padding optional), for `data:` URIs.
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err("Invalid base64 in a glTF buffer".into()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// The data of each of the document's buffers: the GLB's own chunk or an
/// inline `data:` URI (files beside it aren't read: a figure's model is
/// one file).
fn buffers(gltf: &gltf::Gltf) -> Result<Vec<Vec<u8>>, String> {
    gltf.buffers()
        .map(|buffer| {
            let data = match buffer.source() {
                gltf::buffer::Source::Bin => {
                    gltf.blob.clone().ok_or("The GLB has no binary chunk")?
                }
                gltf::buffer::Source::Uri(uri) => {
                    let payload = uri
                        .strip_prefix("data:")
                        .and_then(|rest| rest.split_once(";base64,"))
                        .map(|(_, data)| data)
                        .ok_or_else(|| {
                            format!(
                                "The model refers to a separate file ({uri}): save it as one \
                                 .glb"
                            )
                        })?;
                    base64_decode(payload)?
                }
            };
            if data.len() < buffer.length() {
                return Err("A glTF buffer is shorter than it says".to_string());
            }
            Ok(data)
        })
        .collect()
}

/// Straight sRGB bytes of a linear colour (glTF's base colour factor).
fn srgb_bytes(linear: [f32; 4]) -> [u8; 4] {
    let encode = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    };
    [
        encode(linear[0]),
        encode(linear[1]),
        encode(linear[2]),
        (linear[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// Read a glTF / GLB model.
pub fn load_gltf(bytes: &[u8], name: &str) -> Result<FigureModel, String> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(|e| format!("Not a glTF model: {e}"))?;
    let data = buffers(&gltf)?;
    let get = |b: gltf::Buffer<'_>| data.get(b.index()).map(|d| d.as_slice());
    let nodes: Vec<gltf::Node<'_>> = gltf.nodes().collect();
    let count = nodes.len();

    // The scene graph: each node's parent and its matrix in the scene.
    let mut parent: Vec<Option<usize>> = vec![None; count];
    for node in &nodes {
        for child in node.children() {
            parent[child.index()] = Some(node.index());
        }
    }
    let local: Vec<Mat4> = nodes
        .iter()
        .map(|n| Mat4::from_cols_array_2d(&n.transform().matrix()))
        .collect();
    let mut world: Vec<Option<Mat4>> = vec![None; count];
    fn world_of(
        i: usize,
        parent: &[Option<usize>],
        local: &[Mat4],
        world: &mut [Option<Mat4>],
        depth: usize,
    ) -> Mat4 {
        if let Some(m) = world[i] {
            return m;
        }
        // A cycle (an invalid file) ends at the node itself.
        let m = match parent[i] {
            Some(p) if depth < parent.len() => {
                world_of(p, parent, local, world, depth + 1) * local[i]
            }
            _ => local[i],
        };
        world[i] = Some(m);
        m
    }
    let world: Vec<Mat4> = (0..count)
        .map(|i| world_of(i, &parent, &local, &mut world, 0))
        .collect();

    // The skeleton: the first skin's joints, parents first.
    let skin = gltf.skins().next();
    let skin_nodes: Vec<usize> = skin
        .as_ref()
        .map(|s| s.joints().map(|j| j.index()).collect())
        .unwrap_or_default();
    let skin_binds: Vec<Mat4> = match &skin {
        Some(s) => match s.reader(get).read_inverse_bind_matrices() {
            Some(m) => m.map(|m| Mat4::from_cols_array_2d(&m)).collect(),
            None => vec![Mat4::IDENTITY; skin_nodes.len()],
        },
        None => Vec::new(),
    };
    if skin_binds.len() < skin_nodes.len() {
        return Err("The skin has fewer bind matrices than joints".into());
    }
    if skin_nodes.len() > MAX_JOINTS {
        return Err(format!(
            "The model has {} joints; figures can have at most {MAX_JOINTS}",
            skin_nodes.len()
        ));
    }
    // A joint's parent joint: its nearest ancestor that is a joint.
    let joint_parent = |node: usize| -> Option<usize> {
        let mut at = parent[node];
        for _ in 0..count {
            let p = at?;
            if let Some(j) = skin_nodes.iter().position(|&n| n == p) {
                return Some(j);
            }
            at = parent[p];
        }
        None
    };
    let skin_parents: Vec<Option<usize>> = skin_nodes.iter().map(|&n| joint_parent(n)).collect();
    // Parents before children: by depth in the joint tree.
    let depth_of = |mut j: usize| {
        let mut d = 0;
        while let Some(p) = skin_parents[j] {
            j = p;
            d += 1;
            if d > skin_nodes.len() {
                break;
            }
        }
        d
    };
    let mut order: Vec<usize> = (0..skin_nodes.len()).collect();
    order.sort_by_key(|&j| (depth_of(j), j));
    let mut new_index = vec![0usize; skin_nodes.len()];
    for (new, &old) in order.iter().enumerate() {
        new_index[old] = new;
    }
    let mut joints: Vec<Joint> = order
        .iter()
        .map(|&old| {
            let node = skin_nodes[old];
            let parent = skin_parents[old];
            // In its parent joint's frame (nodes between them included);
            // a root joint keeps the scene placement above it.
            let local = match parent {
                Some(p) => world[skin_nodes[p]].inverse() * world[node],
                None => world[node],
            };
            let (scale, rotation, translation) = local.to_scale_rotation_translation();
            Joint {
                name: nodes[node]
                    .name()
                    .map_or_else(|| format!("joint {old}"), str::to_string),
                parent: parent.map(|p| new_index[p]),
                rest: RestTransform {
                    translation,
                    rotation: rotation.normalize(),
                    scale,
                },
                inverse_bind: skin_binds[old],
                human: None,
                limit: JointLimit::FREE,
            }
        })
        .collect();
    // A model without a skin moves as one piece, on a joint of its own.
    if joints.is_empty() {
        joints.push(Joint {
            name: "root".into(),
            parent: None,
            rest: RestTransform::default(),
            inverse_bind: Mat4::IDENTITY,
            human: None,
            limit: JointLimit::FREE,
        });
    }
    let skeleton = Skeleton { joints };
    let rest_world = skeleton.world(Mat4::IDENTITY, &[]);

    let mut mesh = Mesh::default();
    let mut morphs: Vec<MorphTarget> = Vec::new();
    let skin_index = skin.as_ref().map(|s| s.index());
    for node in &nodes {
        let Some(gmesh) = node.mesh() else { continue };
        let skinned = node.skin().map(|s| s.index()) == skin_index && skin_index.is_some();
        // Unskinned parts ride on their nearest joint, rigidly: placed
        // where they show at rest, in that joint's bind frame.
        let rigid = if skinned {
            None
        } else {
            let j = joint_parent(node.index())
                .or_else(|| skin_nodes.iter().position(|&n| n == node.index()))
                .map_or(0, |j| new_index[j]);
            let bind = rest_world[j] * skeleton.joints[j].inverse_bind;
            Some((j as u16, bind.inverse() * world[node.index()]))
        };
        let target_names: Vec<String> = gmesh
            .extras()
            .as_ref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.get()).ok())
            .and_then(|v| {
                v.get("targetNames")?.as_array().map(|names| {
                    names
                        .iter()
                        .map(|n| n.as_str().unwrap_or_default().to_string())
                        .collect()
                })
            })
            .unwrap_or_default();
        for primitive in gmesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = primitive.reader(get);
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let base = mesh.positions.len() as u32;
            let positions: Vec<[f32; 3]> = positions.collect();
            let n = positions.len();
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|i| i.collect());
            let color = srgb_bytes(
                primitive
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_factor(),
            );
            let (vjoints, vweights): (Vec<[u16; 4]>, Vec<[f32; 4]>) = match rigid {
                Some((j, _)) => (vec![[j, 0, 0, 0]; n], vec![[1.0, 0.0, 0.0, 0.0]; n]),
                None => {
                    let joints: Vec<[u16; 4]> = reader
                        .read_joints(0)
                        .map(|j| j.into_u16().collect())
                        .unwrap_or_else(|| vec![[0; 4]; n]);
                    let weights: Vec<[f32; 4]> = reader
                        .read_weights(0)
                        .map(|w| w.into_f32().collect())
                        .unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]; n]);
                    if joints.len() != n || weights.len() != n {
                        return Err("A mesh's joints or weights don't match its vertices".into());
                    }
                    joints
                        .into_iter()
                        .zip(weights)
                        .map(|(j, w)| normalized_influences(j, w, &new_index))
                        .unzip()
                }
            };
            let (place, turn) = match rigid {
                Some((_, m)) => (m, m.inverse().transpose()),
                None => (Mat4::IDENTITY, Mat4::IDENTITY),
            };
            mesh.positions.extend(
                positions
                    .iter()
                    .map(|p| place.transform_point3(Vec3::from(*p)).to_array()),
            );
            mesh.colors.extend(std::iter::repeat_n(color, n));
            mesh.joints.extend(vjoints);
            mesh.weights.extend(vweights);
            let indices: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..n as u32).collect(),
            };
            if indices.iter().any(|&i| i as usize >= n) {
                return Err("A mesh's triangles refer to vertices it doesn't have".into());
            }
            match normals.filter(|v| v.len() == n) {
                Some(normals) => mesh.normals.extend(normals.iter().map(|v| {
                    turn.transform_vector3(Vec3::from(*v))
                        .normalize_or(Vec3::Y)
                        .to_array()
                })),
                None => {
                    let placed = &mesh.positions[base as usize..];
                    mesh.normals.extend(smooth_normals(placed, &indices));
                }
            }
            mesh.indices.extend(indices.iter().map(|i| i + base));
            for (t, (moved, _, _)) in reader.read_morph_targets().enumerate() {
                let Some(moved) = moved else { continue };
                let name = target_names
                    .get(t)
                    .filter(|n| !n.is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("Morph {}", t + 1));
                let deltas = moved
                    .enumerate()
                    .filter(|(_, d)| d.iter().any(|&x| x != 0.0))
                    .map(|(v, d)| {
                        (
                            base + v as u32,
                            place.transform_vector3(Vec3::from(d)).into(),
                        )
                    });
                match morphs.iter_mut().find(|m| m.name == name) {
                    Some(target) => target.deltas.extend(deltas),
                    None => morphs.push(MorphTarget {
                        name,
                        deltas: deltas.collect(),
                    }),
                }
            }
        }
    }
    if mesh.indices.is_empty() {
        return Err("The model has no triangles".into());
    }
    if mesh.positions.len() > u32::MAX as usize {
        return Err("The model is too large".into());
    }
    Ok(FigureModel::new(name, skeleton, mesh, morphs))
}

/// A vertex's four influences: joint indices in the skeleton's order,
/// weights summing to 1 (a vertex with none follows the root).
fn normalized_influences(
    joints: [u16; 4],
    weights: [f32; 4],
    new_index: &[usize],
) -> ([u16; 4], [f32; 4]) {
    let mut j = [0u16; 4];
    let mut w = [0f32; 4];
    for k in 0..4 {
        let weight = weights[k].max(0.0);
        match new_index.get(joints[k] as usize) {
            Some(&n) if weight > 0.0 => {
                j[k] = n as u16;
                w[k] = weight;
            }
            _ => {}
        }
    }
    let sum: f32 = w.iter().sum();
    if sum <= 1e-6 {
        return ([0; 4], [1.0, 0.0, 0.0, 0.0]);
    }
    (j, w.map(|x| x / sum))
}

/// Normals of a mesh without them: each vertex's faces' normals,
/// weighted by their area.
fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| Vec3::from(positions[i as usize]));
        let n = (b - a).cross(c - a);
        for &i in tri {
            normals[i as usize] += n;
        }
    }
    normals
        .into_iter()
        .map(|n| n.normalize_or(Vec3::Y).into())
        .collect()
}

/// A model's rest pose as one turn per joint (all identity).
pub fn rest_pose(model: &FigureModel) -> Vec<Quat> {
    vec![Quat::IDENTITY; model.skeleton.len()]
}

/// A model ready to share between layers.
pub type SharedModel = Arc<FigureModel>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::figure::skeleton::{HumanJoint, Side};

    fn fixture() -> FigureModel {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/figure-tiny.gltf");
        let bytes = std::fs::read(path).expect("the fixture");
        load_gltf(&bytes, "tiny").expect("loads")
    }

    #[test]
    fn a_skinned_gltf_loads_its_skeleton() {
        let model = fixture();
        let joints = &model.skeleton.joints;
        assert_eq!(joints.len(), 3);
        let names: Vec<&str> = joints.iter().map(|j| j.name.as_str()).collect();
        // The file lists the arm first: parents come first once loaded.
        assert_eq!(
            names,
            ["mixamorig:Hips", "mixamorig:Spine", "mixamorig:LeftArm"]
        );
        assert_eq!(joints[0].parent, None);
        assert_eq!(joints[1].parent, Some(0));
        assert_eq!(joints[2].parent, Some(1));
        assert_eq!(joints[0].human, Some(HumanJoint::Hips));
        assert_eq!(joints[1].human, Some(HumanJoint::Spine));
        assert_eq!(joints[2].human, Some(HumanJoint::UpperArm(Side::Left)));
        // The root joint sits under a node raising it by 1: that stays.
        assert!(
            joints[0]
                .rest
                .translation
                .abs_diff_eq(Vec3::new(0.0, 1.0, 0.0), 1e-5)
        );
        assert!(
            joints[1]
                .rest
                .translation
                .abs_diff_eq(Vec3::new(0.0, 0.5, 0.0), 1e-5)
        );
        // Inverse binds are the joints' rest placements, inverted.
        let rest = model.skeleton.world(Mat4::IDENTITY, &[]);
        for (j, w) in joints.iter().zip(&rest) {
            assert!(
                (j.inverse_bind * *w).abs_diff_eq(Mat4::IDENTITY, 1e-4),
                "{}",
                j.name
            );
        }
        let spine_bind = Mat4::from_translation(Vec3::new(0.0, -1.5, 0.0));
        assert!(joints[1].inverse_bind.abs_diff_eq(spine_bind, 1e-5));
    }

    #[test]
    fn a_skinned_gltf_loads_its_mesh_and_morphs() {
        let model = fixture();
        let mesh = &model.mesh;
        assert_eq!(mesh.positions.len(), 6);
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(mesh.normals.len(), 6);
        // Weights are normalised and the joints renumbered parents first:
        // the file's joint 0 (the arm) is joint 2 now.
        for (j, w) in mesh.joints.iter().zip(&mesh.weights) {
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5);
            assert!(j.iter().all(|&j| j < 3));
        }
        assert_eq!(mesh.joints[4][0], 2);
        // The material's colour (linear 0.5 grey) as sRGB.
        assert_eq!(mesh.colors[0], [188, 188, 188, 255]);
        assert_eq!(model.morphs.len(), 1);
        assert_eq!(model.morphs[0].name, "Wide");
        let moved = model.morphed_positions(&[1.0]);
        assert!((moved[0][0] - (mesh.positions[0][0] - 0.25)).abs() < 1e-5);
        assert_eq!(model.morphed_positions(&[]), mesh.positions);
    }

    #[test]
    fn posing_the_spine_moves_the_vertices_it_owns() {
        let model = fixture();
        let pose = [Quat::IDENTITY, Quat::from_rotation_z(std::f32::consts::PI)];
        let world = model.skeleton.world(Mat4::IDENTITY, &pose);
        let skin = model.skeleton.skin(&world);
        let place = |v: usize| {
            let p = Vec3::from(model.mesh.positions[v]);
            let j = model.mesh.joints[v];
            let w = model.mesh.weights[v];
            (0..4)
                .map(|k| skin[j[k] as usize].transform_point3(p) * w[k])
                .sum::<Vec3>()
        };
        // Vertex 2 (at y = 2, on the spine) turns half round the spine
        // joint at y = 1.5.
        let p = place(2);
        assert!(p.abs_diff_eq(Vec3::new(0.0, 1.0, 0.0), 1e-4), "{p}");
        // Vertex 0 (on the hips) stays.
        assert!(place(0).abs_diff_eq(Vec3::from(model.mesh.positions[0]), 1e-5));
    }

    #[test]
    fn a_gltf_without_a_skin_is_one_rigid_piece() {
        let json = r#"{"asset":{"version":"2.0"},
            "buffers":[{"byteLength":36,"uri":"data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA"}],
            "bufferViews":[{"buffer":0,"byteLength":36}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "nodes":[{"mesh":0,"translation":[0,0,2]}],
            "scenes":[{"nodes":[0]}]}"#;
        let model = load_gltf(json.as_bytes(), "tri").unwrap();
        assert_eq!(model.skeleton.len(), 1);
        // The node's placement is baked into the vertices.
        assert_eq!(model.mesh.positions[1], [1.0, 0.0, 2.0]);
        assert!(Vec3::from(model.mesh.normals[0]).abs_diff_eq(Vec3::Z, 1e-5));
    }

    #[test]
    fn broken_files_are_refused() {
        assert!(load_gltf(b"not a model", "x").is_err());
        let external =
            r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":4,"uri":"mesh.bin"}]}"#;
        let err = load_gltf(external.as_bytes(), "x").unwrap_err();
        assert!(err.contains(".glb"), "{err}");
    }
}
