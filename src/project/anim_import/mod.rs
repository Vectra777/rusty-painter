//! Opening other apps' skeletal and vector animations as rig layers, each
//! written from its format's published description (not from any of their
//! runtimes' code):
//!
//! - [`spine`]: Spine's JSON export, with its texture atlas (or loose
//!   pictures);
//! - [`dragonbones`]: DragonBones' JSON, with its texture atlas;
//! - [`lottie`]: Lottie (Bodymovin) JSON: its layers become bones, their
//!   pictures attachments.
//!
//! What can't come across is listed in the import's notes.

pub mod dragonbones;
pub mod lottie;
pub mod spine;

use crate::canvas::rig::{Rig, RigImage};
use eframe::egui::Color32;
use std::path::Path;

/// A rig read from a file, where its pictures sit in its own frame
/// (`[x, y, width, height]`, for placing it), and notes on what was left
/// out.
pub struct ImportedRig {
    pub rig: Rig,
    pub bounds: Option<[f32; 4]>,
    pub notes: Vec<String>,
    /// Frames a second it was made at (Lottie, DragonBones).
    pub fps: Option<u32>,
}

/// Where a format's other files (atlases, pictures) come from: next to the
/// file opened, by name.
pub trait Siblings {
    fn read(&self, name: &str) -> Option<Vec<u8>>;
    /// Every file there with this extension (lower case), by name.
    fn with_extension(&self, ext: &str) -> Vec<String>;
}

/// Files in a folder.
pub struct Folder<'a>(pub &'a Path);

impl Siblings for Folder<'_> {
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        // Names come from the file: never leave the folder.
        let rel = Path::new(name);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
        std::fs::read(self.0.join(rel)).ok()
    }

    fn with_extension(&self, ext: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.0) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.to_ascii_lowercase()
                    .ends_with(&format!(".{ext}"))
                    .then_some(name)
            })
            .collect();
        out.sort();
        out
    }
}

/// Files in memory.
#[cfg(test)]
#[derive(Default)]
pub struct InMemory(pub Vec<(String, Vec<u8>)>);

#[cfg(test)]
impl Siblings for InMemory {
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
    }

    fn with_extension(&self, ext: &str) -> Vec<String> {
        (self.0.iter())
            .filter(|(n, _)| n.to_ascii_lowercase().ends_with(&format!(".{ext}")))
            .map(|(n, _)| n.clone())
            .collect()
    }
}

/// Whether `json` looks like one of the formats here.
pub fn is_rig_json(json: &[u8]) -> bool {
    detect(json).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Spine,
    DragonBones,
    Lottie,
}

fn detect(json: &[u8]) -> Option<Format> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let o = v.as_object()?;
    if o.contains_key("skeleton") && o.contains_key("bones") {
        Some(Format::Spine)
    } else if o.contains_key("armature") {
        Some(Format::DragonBones)
    } else if o.contains_key("layers") && (o.contains_key("fr") || o.contains_key("v")) {
        Some(Format::Lottie)
    } else {
        None
    }
}

/// Read the animation in `json` (its other files from `siblings`; `stem` is
/// its name without the extension, for finding them).
pub fn import(json: &[u8], stem: &str, siblings: &dyn Siblings) -> Result<ImportedRig, String> {
    match detect(json) {
        Some(Format::Spine) => spine::import(json, stem, siblings),
        Some(Format::DragonBones) => dragonbones::import(json, stem, siblings),
        Some(Format::Lottie) => lottie::import(json, siblings),
        None => Err("Not a Spine, DragonBones or Lottie animation".into()),
    }
}

/// A PNG (or other picture) as a rig picture, premultiplied; `premultiplied`
/// when its pixels already are.
pub(crate) fn decode_picture(
    name: &str,
    bytes: &[u8],
    premultiplied: bool,
) -> Result<RigImage, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("{name}: {e}"))?
        .to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w * h > crate::app::document::MAX_CANVAS_PIXELS {
        return Err(format!("{name}: too large"));
    }
    let pixels = img
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            if premultiplied {
                Color32::from_rgba_premultiplied(r, g, b, a)
            } else {
                Color32::from_rgba_unmultiplied(r, g, b, a)
            }
        })
        .collect();
    Ok(RigImage {
        name: name.to_string(),
        width: w,
        height: h,
        pixels: std::sync::Arc::new(pixels),
    })
}

/// A JSON number (or its default).
pub(crate) fn num(v: &serde_json::Value, key: &str, default: f32) -> f32 {
    v.get(key)
        .and_then(serde_json::Value::as_f64)
        .map_or(default, |n| n as f32)
}

/// `RRGGBB[AA]` as unmultiplied colour 0..1.
pub(crate) fn hex_color(s: &str) -> Option<[f32; 4]> {
    let s = s.trim_start_matches('#');
    if !(s.len() == 6 || s.len() == 8) || !s.is_ascii() {
        return None;
    }
    let ch = |i: usize| {
        u8::from_str_radix(&s[i..i + 2], 16)
            .ok()
            .map(|v| v as f32 / 255.0)
    };
    Some([
        ch(0)?,
        ch(2)?,
        ch(4)?,
        if s.len() == 8 { ch(6)? } else { 1.0 },
    ])
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_spine() {
        let files = spine::tests::files();
        crate::fuzz::fuzz(
            "spine",
            spine::tests::SKELETON.as_bytes(),
            std::time::Duration::from_secs(2),
            |b| {
                let _ = import(b, "arm", &files);
            },
        );
        let atlas = b"page.png\nsize:64,64\nhead\nbounds:2,4,10,20\noffsets:1,2,12,24\nrotate:90\n";
        crate::fuzz::fuzz(
            "spine-atlas",
            atlas,
            std::time::Duration::from_secs(2),
            |b| {
                let _ = spine::parse_atlas(&String::from_utf8_lossy(b));
            },
        );
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_lottie() {
        let seed = br#"{ "fr": 10, "w": 40, "h": 40, "op": 10, "layers": [
            { "ty": 4, "ind": 1, "ks": { "p": { "a": 1, "k": [ { "t": 0, "s": [5, 5] }, { "t": 10, "s": [30, 30] } ] } },
              "shapes": [ { "ty": "el", "p": { "k": [0, 0] }, "s": { "k": [8, 8] } }, { "ty": "st", "c": { "k": [1, 0, 0, 1] }, "w": { "k": 2 } } ] } ] }"#;
        crate::fuzz::fuzz("lottie", seed, std::time::Duration::from_secs(2), |b| {
            if let Ok(rig) = import(b, "x", &InMemory::default()) {
                let _ = rig.rig.render(&rig.rig.pose(0.3), 64, 64, 64);
            }
        });
    }
}
