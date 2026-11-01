//! The default brush pack: one `.rpbrush` file holding every brush (each
//! tip and grain stored once), and a manifest listing its collections.
//! Its brushes are copied into the library once (see `install_local_pack`)
//! and read again to reset or restore them. Nothing of it is compiled in:
//! it ships beside the program (in the APK's assets on Android).
use super::{brush::BrushPreset, preset_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PACK_ID: &str = "default-v1";
/// The library folder all of the pack's collections go in.
pub const LIBRARY_NAME: &str = "Default brushes";
const VERSION: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
pub struct Collection {
    pub name: String,
    pub brushes: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    /// The `.rpbrush` file, beside the manifest.
    pub file: String,
    pub collections: Vec<Collection>,
}

/// Where the pack is: the data folder's (`brushes`), else the one shipped
/// beside the program, in a `brushes` folder. On Android the APK's is
/// copied into the data folder first (brushes read their tips from files).
pub fn root(brushes: &Path) -> Option<PathBuf> {
    let own = brushes.join("packs").join(PACK_ID);
    #[cfg(target_os = "android")]
    if let Err(err) = crate::android::extract_pack(&own) {
        log::warn!("Couldn't unpack the default brushes: {err}");
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("brushes").join(PACK_ID)));
    std::iter::once(own)
        .chain(beside)
        .find(|root| root.join("manifest.json").is_file())
}

/// The manifest and every collection's presets, in its order.
pub fn load(root: &Path) -> Result<(Manifest, Vec<BrushPreset>), String> {
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if manifest.version != VERSION || manifest.id != PACK_ID {
        return Err("Unsupported brush pack".into());
    }
    if Path::new(&manifest.file).components().count() != 1 {
        return Err("Invalid brush pack file".into());
    }
    let path = root.join(&manifest.file);
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let mut all: std::collections::HashMap<_, _> = preset_file::decode_file(&path, &bytes)?
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    let presets = (manifest.collections.iter())
        .flat_map(|c| &c.brushes)
        .map(|name| {
            all.remove(name)
                .ok_or_else(|| format!("The brush pack has no brush {name}"))
        })
        .collect::<Result<_, _>>()?;
    Ok((manifest, presets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_loads_its_collections_in_order_and_refuses_a_missing_brush() {
        let dir = std::env::temp_dir().join(format!("rp-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let preset = |name: &str| BrushPreset {
            name: name.into(),
            brush: crate::brush_engine::brush::Brush::new(
                9.0,
                80.0,
                eframe::egui::Color32::BLACK,
                10.0,
            ),
            file: None,
        };
        let bytes = preset_file::encode(&[preset("A"), preset("B"), preset("C")]).unwrap();
        std::fs::write(dir.join("brushes.rpbrush"), bytes).unwrap();
        let mut manifest = Manifest {
            version: VERSION,
            id: PACK_ID.into(),
            file: "brushes.rpbrush".into(),
            collections: vec![
                Collection {
                    name: "Inks".into(),
                    brushes: vec!["C".into()],
                },
                Collection {
                    name: "Pens".into(),
                    brushes: vec!["A".into(), "B".into()],
                },
            ],
        };
        let write = |m: &Manifest| {
            std::fs::write(dir.join("manifest.json"), serde_json::to_vec(m).unwrap()).unwrap()
        };
        write(&manifest);
        assert_eq!(root(&dir.join("elsewhere")), None);
        let names: Vec<_> = load(&dir).unwrap().1.into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["C", "A", "B"]);
        manifest.collections[0].brushes.push("Z".into());
        write(&manifest);
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
