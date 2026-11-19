//! The realistic figures: a pack of glTF models (male, female, child,
//! hands) made from MakeHuman's CC0 assets by `scripts/make_figure_pack.py`,
//! and a manifest listing them. On desktop it's built into the program
//! (~3.3 MB) and written into the data folder when first wanted; on
//! Android it's in the APK's assets, copied there likewise. A `figures`
//! folder beside the program is used first if there is one, and the app
//! works without any, with the mannequin alone.
//!
//! A figure made from the pack is saved by its model's id (see
//! [`super::ModelRef::Bundled`]), not with the model itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use super::SharedModel;

/// The pack's id, and its folder in the APK's assets.
pub const PACK_ID: &str = "figures-v1";
/// Its folder in a `figures` folder (the data folder's, or beside the
/// program).
pub const PACK_DIR: &str = "v1";
const VERSION: u32 = 1;

/// What a model is: a whole body, or a hand on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Body,
    Hand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackModel {
    /// What a project saves the figure by (unique among all bundled
    /// models, the mannequin's included).
    pub id: String,
    /// Shown in the app.
    pub name: String,
    /// The `.glb` file, beside the manifest.
    pub file: String,
    pub kind: ModelKind,
    /// The model's licence (SPDX), and what it was made from.
    pub licence: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub models: Vec<PackModel>,
    /// The CRC-32 (hex) of the model files, one after the other in the
    /// manifest's order: a new pack is a new manifest, so Android unpacks
    /// it again. Keep it in step with the files (a test checks).
    #[serde(default)]
    pub content: String,
}

impl Manifest {
    pub fn model(&self, id: &str) -> Option<&PackModel> {
        self.models.iter().find(|m| m.id == id)
    }
}

/// Where the pack may be, in order: the data folder's `figures` folder (on
/// Android the APK's pack is copied there first), the `figures` folder
/// beside the program, and in builds for development the repository's.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    // Tests see the repository's pack alone, not the user's.
    if !cfg!(test) {
        let own = crate::app::init::data_dir().join("figures").join(PACK_DIR);
        #[cfg(target_os = "android")]
        if let Err(err) = crate::android::extract_figure_pack(&own) {
            log::warn!("Couldn't unpack the realistic figures: {err}");
        }
        out.push(own);
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("figures").join(PACK_DIR)))
        {
            out.push(dir);
        }
    }
    if cfg!(debug_assertions) {
        out.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/figure-pack")
                .join(PACK_DIR),
        );
    }
    out
}

/// Whether `root` holds a pack whose models are there (not just Git LFS
/// pointers to them, as in a checkout without LFS).
fn is_pack(root: &Path) -> bool {
    manifest(root).is_ok_and(|m| {
        m.models.iter().all(|model| {
            let mut head = [0u8; 4];
            std::fs::File::open(root.join(&model.file))
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
                .is_ok_and(|()| &head == b"glTF")
        })
    })
}

/// The installed pack: its folder and manifest (looked for once).
pub fn installed() -> Option<&'static (PathBuf, Manifest)> {
    static PACK: OnceLock<Option<(PathBuf, Manifest)>> = OnceLock::new();
    PACK.get_or_init(|| {
        let root = (candidates().into_iter().find(|root| is_pack(root))).or_else(unpack_bundled)?;
        let manifest = manifest(&root).ok()?;
        log::info!("Realistic figures from {}", root.display());
        Some((root, manifest))
    })
    .as_ref()
}

/// The models the installed pack has (none without it).
pub fn models() -> &'static [PackModel] {
    installed().map_or(&[], |(_, m)| &m.models)
}

/// The pack built into the program (desktop; Android has it in the
/// APK), written into the data folder when no pack is found, so a build
/// run from anywhere has the realistic figures. Its folder, if that
/// worked (a checkout without Git LFS builds in pointers: not a pack).
#[cfg(not(any(test, target_os = "android", target_arch = "wasm32")))]
fn unpack_bundled() -> Option<PathBuf> {
    macro_rules! files {
        ($($name:literal),*) => {
            [$(($name, include_bytes!(concat!("../../../assets/figure-pack/v1/", $name)).as_slice())),*]
        };
    }
    const FILES: [(&str, &[u8]); 7] = files!(
        "male.glb",
        "female.glb",
        "child.glb",
        "hand-left.glb",
        "hand-right.glb",
        "LICENSE.md",
        "manifest.json"
    );
    let root = crate::app::init::data_dir().join("figures").join(PACK_DIR);
    let written = std::fs::create_dir_all(&root).and_then(|()| {
        // The manifest last: a pack half written isn't taken for one.
        FILES
            .iter()
            .try_for_each(|(name, bytes)| std::fs::write(root.join(name), bytes))
    });
    if let Err(err) = written {
        log::warn!("Couldn't write the realistic figures: {err}");
        return None;
    }
    is_pack(&root).then_some(root)
}

#[cfg(any(test, target_os = "android", target_arch = "wasm32"))]
fn unpack_bundled() -> Option<PathBuf> {
    None
}

/// A pack model's name (None if the pack doesn't have it).
pub fn name_of(id: &str) -> Option<&'static str> {
    Some(installed()?.1.model(id)?.name.as_str())
}

/// The pack's manifest, checked.
pub fn manifest(root: &Path) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if manifest.version != VERSION || manifest.id != PACK_ID {
        return Err("Unsupported figure pack".into());
    }
    if (manifest.models.iter()).any(|m| Path::new(&m.file).components().count() != 1) {
        return Err("Invalid figure pack file".into());
    }
    Ok(manifest)
}

/// The stamp of the pack's model files (see [`Manifest::content`]).
pub fn content_stamp(root: &Path, manifest: &Manifest) -> Result<String, String> {
    let mut crc = crc32fast::Hasher::new();
    for model in &manifest.models {
        crc.update(&std::fs::read(root.join(&model.file)).map_err(|e| e.to_string())?);
    }
    Ok(format!("{:08x}", crc.finalize()))
}

/// Read model `id` of the pack in `root`.
pub fn load_from(root: &Path, manifest: &Manifest, id: &str) -> Result<SharedModel, String> {
    let entry = manifest
        .model(id)
        .ok_or_else(|| format!("The figure pack has no model \"{id}\""))?;
    let bytes = std::fs::read(root.join(&entry.file)).map_err(|e| e.to_string())?;
    super::model::load_gltf(&bytes, &entry.name).map(Arc::new)
}

/// The installed pack's model `id`, read once (later calls share it).
pub fn model(id: &str) -> Result<SharedModel, String> {
    static LOADED: OnceLock<Mutex<HashMap<String, Result<SharedModel, String>>>> = OnceLock::new();
    let Some((root, manifest)) = installed() else {
        return Err(NOT_INSTALLED.into());
    };
    let mut loaded = LOADED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    loaded
        .entry(id.to_string())
        .or_insert_with(|| load_from(root, manifest, id))
        .clone()
}

/// What the app says when the pack isn't there.
pub const NOT_INSTALLED: &str = "The realistic figures aren't installed";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::figure::presets::{BODY_PRESETS, HAND_PRESETS, hand_pose};
    use crate::canvas::figure::skeleton::{HumanJoint, Side};
    use crate::canvas::figure::{FigureInstance, ModelRef};
    use glam::{Mat4, Quat, Vec3};

    fn shipped() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/figure-pack/v1")
    }

    /// The shipped pack, if Git LFS has pulled its models (a checkout
    /// without LFS has their pointers alone).
    fn pulled() -> Option<(PathBuf, Manifest)> {
        let root = shipped();
        if !is_pack(&root) {
            eprintln!("the figure pack's models aren't pulled from Git LFS; skipping");
            return None;
        }
        let manifest = manifest(&root).unwrap();
        Some((root, manifest))
    }

    #[test]
    fn the_shipped_manifest_lists_the_figures_under_a_free_licence() {
        let manifest = manifest(&shipped()).unwrap();
        let ids: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["male", "female", "child", "hand-left", "hand-right"]);
        for model in &manifest.models {
            assert_eq!(model.licence, "CC0-1.0", "{}", model.id);
            assert!(!model.source.is_empty());
            assert_ne!(model.id, super::super::mannequin::ID);
        }
        assert!(shipped().join("LICENSE.md").is_file());
    }

    /// The shipped pack's stamp is its files' (skipped where they are only
    /// their Git LFS pointers).
    #[test]
    fn the_shipped_pack_s_stamp_matches_its_files() {
        let Some((root, manifest)) = pulled() else {
            return;
        };
        assert_eq!(
            manifest.content,
            content_stamp(&root, &manifest).unwrap(),
            "update the manifest's content after changing the pack's models"
        );
    }

    #[test]
    fn a_pack_with_a_model_missing_or_a_wrong_version_is_refused() {
        let dir = std::env::temp_dir().join(format!("rp-figure-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = manifest(&shipped()).unwrap();
        let write = |m: &Manifest| {
            std::fs::write(dir.join("manifest.json"), serde_json::to_vec(m).unwrap()).unwrap()
        };
        write(&m);
        // The manifest without its models isn't a pack.
        assert!(manifest(&dir).is_ok());
        assert!(!is_pack(&dir));
        m.version = 99;
        write(&m);
        assert!(manifest(&dir).is_err());
        m.version = VERSION;
        m.models[0].file = "../male.glb".into();
        write(&m);
        assert!(manifest(&dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_shipped_model_has_the_whole_human_skeleton_in_t_pose() {
        let Some((root, manifest)) = pulled() else {
            return;
        };
        for entry in &manifest.models {
            let model = load_from(&root, &manifest, &entry.id).unwrap();
            let skeleton = &model.skeleton;
            assert!(skeleton.len() <= super::super::model::MAX_JOINTS);
            let wanted: Vec<HumanJoint> = match entry.kind {
                ModelKind::Body => HumanJoint::all(),
                ModelKind::Hand => {
                    let side = if entry.id.ends_with("left") {
                        Side::Left
                    } else {
                        Side::Right
                    };
                    (HumanJoint::all().into_iter())
                        .filter(|h| match h {
                            HumanJoint::Hand(s) | HumanJoint::Finger(s, ..) => *s == side,
                            _ => false,
                        })
                        .collect()
                }
            };
            for h in &wanted {
                assert!(skeleton.find(*h).is_some(), "{}: {h:?}", entry.id);
            }
            let rest = skeleton.world(Mat4::IDENTITY, &[]);
            let at = |h: HumanJoint| rest[skeleton.find(h).unwrap()].transform_point3(Vec3::ZERO);
            for side in [Side::Left, Side::Right] {
                let sign = if side == Side::Left { 1.0 } else { -1.0 };
                // Fingers straight out from the hand, palms down: the
                // middle finger's joints in a horizontal line along X.
                let hand = HumanJoint::Hand(side);
                if !wanted.contains(&hand) {
                    continue;
                }
                let tip = at(HumanJoint::Finger(
                    side,
                    super::super::skeleton::Finger::Middle,
                    2,
                ));
                let out = (tip - at(hand)).normalize();
                assert!(out.x * sign > 0.97, "{}: {side:?} hand {out}", entry.id);
                assert!(out.y.abs() < 0.08, "{}: {side:?} hand {out}", entry.id);
                if entry.kind == ModelKind::Body {
                    // Arms out level along X.
                    let shoulder = at(HumanJoint::UpperArm(side));
                    let arm = (at(hand) - shoulder).normalize();
                    assert!(arm.x * sign > 0.99, "{}: {side:?} arm {arm}", entry.id);
                    assert!(arm.y.abs() < 0.03, "{}: {side:?} arm {arm}", entry.id);
                }
            }
            if entry.kind == ModelKind::Body {
                // Standing on the ground, facing +Z (the nose in front of
                // the head joint), its left at +X.
                let (lo, hi) = model.bounds();
                assert!(lo.y.abs() < 0.02, "{}: feet at {}", entry.id, lo.y);
                assert!(hi.y > 1.0 && hi.y < 2.0, "{}: {}", entry.id, hi.y);
                let head = at(HumanJoint::Head);
                let front = model
                    .mesh
                    .positions
                    .iter()
                    .map(|p| Vec3::from(*p))
                    .filter(|p| p.y > head.y)
                    .map(|p| p.z - head.z)
                    .fold(f32::MIN, f32::max);
                assert!(front > 0.05, "{}: the face is at +Z", entry.id);
                assert!(
                    at(HumanJoint::UpperLeg(Side::Left)).x > 0.0,
                    "{}: the left leg is at +X",
                    entry.id
                );
                assert!(!model.morphs.is_empty(), "{}: body shapes", entry.id);
            }
        }
    }

    #[test]
    fn presets_pose_every_shipped_model_without_nans() {
        let Some((root, manifest)) = pulled() else {
            return;
        };
        let fist = HAND_PRESETS.iter().find(|p| p.name == "Fist").unwrap();
        for entry in &manifest.models {
            let model = load_from(&root, &manifest, &entry.id).unwrap();
            let skeleton = &model.skeleton;
            let mut instance = FigureInstance::new(ModelRef::Bundled(entry.id.clone()));
            instance.morphs = vec![1.0; model.morphs.len()];
            let poses: Vec<_> = BODY_PRESETS.iter().map(|p| p.pose()).collect();
            for pose in &poses {
                pose.apply(skeleton, &mut instance.pose, |_| true, true);
                for side in [Side::Left, Side::Right] {
                    hand_pose(skeleton, side, &fist.shape, |_| true).apply(
                        skeleton,
                        &mut instance.pose,
                        |_| true,
                        false,
                    );
                }
                let points = instance.skinned_positions(&model);
                assert!(
                    points.iter().all(|p| p.is_finite()),
                    "{}: NaN in a preset",
                    entry.id
                );
            }
            // A fist brings the fingertips towards the wrist.
            for side in [Side::Left, Side::Right] {
                let (Some(hand), Some(tip)) = (
                    skeleton.find(HumanJoint::Hand(side)),
                    skeleton.find(HumanJoint::Finger(
                        side,
                        super::super::skeleton::Finger::Middle,
                        2,
                    )),
                ) else {
                    continue;
                };
                let rest = skeleton.world(Mat4::IDENTITY, &[]);
                let mut pose = vec![Quat::IDENTITY.to_array(); skeleton.len()];
                hand_pose(skeleton, side, &fist.shape, |_| true).apply(
                    skeleton,
                    &mut pose,
                    |_| true,
                    false,
                );
                let turns: Vec<Quat> = pose.iter().map(|q| Quat::from_array(*q)).collect();
                let posed = skeleton.world(Mat4::IDENTITY, &turns);
                let span = |w: &[Mat4]| {
                    (w[tip].transform_point3(Vec3::ZERO) - w[hand].transform_point3(Vec3::ZERO))
                        .length()
                };
                assert!(
                    span(&posed) < span(&rest) * 0.75,
                    "{}: {side:?} fist {} vs {}",
                    entry.id,
                    span(&posed),
                    span(&rest)
                );
            }
        }
    }
}
