//! Importing other apps' brushes as presets: GIMP (`.gbr`, `.gih`),
//! Photoshop (`.abr`), Krita (`.kpp` presets and `.bundle` sets), MyPaint
//! (`.myb`) and Clip Studio Paint (`.sut`).
//!
//! None of these formats has an official specification (GIMP's excepted),
//! and each app's brushes can do things this engine doesn't, so imports are
//! best effort: the tips come across as they are, the settings that have a
//! counterpart here are carried over, and [`Imported::notes`] says what was
//! approximated or left out. ibisPaint brushes can't be imported: the app
//! only shares them as QR codes through its own online library.

mod abr;
mod gimp;
pub(crate) mod krita;
mod mypaint;
#[cfg(feature = "sut-import")]
pub(crate) mod sut;

use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::tip::TipMask;
use eframe::egui::Color32;
use std::sync::Arc;

/// File name extensions that can be imported.
pub const EXTENSIONS: &[&str] = &[
    "gbr",
    "gih",
    "abr",
    "kpp",
    "bundle",
    "myb",
    #[cfg(feature = "sut-import")]
    "sut",
];

/// What an import brought in.
#[derive(Default)]
pub struct Imported {
    pub presets: Vec<BrushPreset>,
    /// What was approximated or left out, for the person importing.
    pub notes: Vec<String>,
}

/// Import the brushes in file `file_name` (its extension says the format)
/// with contents `bytes`.
pub fn import(file_name: &str, bytes: &[u8]) -> Result<Imported, String> {
    let path = std::path::Path::new(file_name);
    let stem = path.file_stem().map_or_else(
        || "Imported".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let imported = match extension.as_str() {
        "gbr" => gimp::import_gbr(bytes, &stem),
        "gih" => gimp::import_gih(bytes, &stem),
        "abr" => abr::import(bytes, &stem),
        "kpp" => krita::import_kpp(bytes, &stem),
        "bundle" => krita::import_bundle(bytes),
        "myb" => mypaint::import(bytes, &stem),
        #[cfg(feature = "sut-import")]
        "sut" => sut::import(bytes, &stem),
        _ => Err(format!("{file_name}: not a brush file this app can import")),
    }?;
    if imported.presets.is_empty() {
        return Err(format!("{file_name} has no brushes this app can use"));
    }
    Ok(imported)
}

/// A preset painting with `tip` (spaced by `spacing` percent of its size),
/// in the tip's own colours when it has some.
fn tip_preset(name: &str, tip: Arc<TipMask>, spacing: f32) -> BrushPreset {
    let diameter = tip.width.max(tip.height).clamp(1, 3000) as f32;
    let mut brush = Brush::new(diameter, 100.0, Color32::BLACK, spacing.clamp(1.0, 1000.0));
    brush.brush_options.tip_colors = tip.has_colors();
    brush.brush_options.pixel_shape = PixelBrushShape::Custom(tip);
    BrushPreset {
        name: name.to_string(),
        brush,
        file: None,
    }
}

/// Reading binary files, bounds-checked (a damaged file is an error, never
/// a panic).
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

fn short() -> String {
    "The file ends too soon (damaged?)".to_string()
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub fn pos(&self) -> usize {
        self.at
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn seek(&mut self, at: usize) -> Result<(), String> {
        if at > self.bytes.len() {
            return Err(short());
        }
        self.at = at;
        Ok(())
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or_else(short)?;
        let out = self.bytes.get(self.at..end).ok_or_else(short)?;
        self.at = end;
        Ok(out)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn u16_be(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    pub fn u32_be(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    pub fn i32_be(&mut self) -> Result<i32, String> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    pub fn f64_be(&mut self) -> Result<f64, String> {
        Ok(f64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    /// Up to the next newline (which is skipped), as text.
    pub fn line(&mut self) -> Result<String, String> {
        let rest = &self.bytes[self.at..];
        let end = rest.iter().position(|&b| b == b'\n').ok_or_else(short)?;
        let line = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.at += end + 1;
        Ok(line)
    }
}

/// PackBits (Photoshop's run-length encoding) of one row into `out`.
fn unpack_bits(r: &mut Reader<'_>, packed_len: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let end = r.pos() + packed_len;
    while r.pos() < end {
        let n = r.u8()? as i8;
        if n >= 0 {
            out.extend_from_slice(r.take(n as usize + 1)?);
        } else if n != -128 {
            let v = r.u8()?;
            out.extend(std::iter::repeat_n(v, (1 - n as i32) as usize));
        }
    }
    Ok(())
}

/// Standard base64 (as in Krita presets' embedded resources); whitespace
/// ignored.
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        let v = value(c).ok_or("Bad base64 data")?;
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_and_base64_decode() {
        // PackBits: a literal run of 3, then 4 repeats of 7.
        let data = [2u8, 1, 2, 3, 0xFD, 7];
        let mut out = Vec::new();
        unpack_bits(&mut Reader::new(&data), data.len(), &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 7, 7, 7, 7]);
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8h").unwrap(), b"hello!");
        assert!(base64_decode("a$b").is_err());
    }

    #[test]
    fn unknown_files_are_refused() {
        assert!(import("brush.xyz", b"whatever").is_err());
        assert!(import("brush.gbr", b"").is_err());
    }

    #[test]
    fn every_import_paints() {
        // A GIMP brush through the whole import, then painted.
        let pixels = vec![255u8; 32 * 32];
        let bytes = gimp::tests::gbr("Square", 32, 32, 1, 25, &pixels);
        let imported = import("square.gbr", &bytes).unwrap();
        let mut brush = imported.presets[0].brush.clone();
        let image = crate::brush_engine::preview::stroke_preview_image(
            &mut brush,
            &rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap(),
            [128, 64],
            64,
            Color32::BLACK,
            20.0,
        );
        assert!(image.pixels.iter().any(|p| p.a() > 0));
    }
}
