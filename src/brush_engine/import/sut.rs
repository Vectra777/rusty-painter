//! Clip Studio Paint sub tools (`.sut`): an SQLite database. `Node` names
//! the tools and points at their settings in `Variant` (one column per
//! setting, which columns there are depending on Clip Studio's version);
//! `MaterialFile` holds the tip pictures, packed in Clip Studio's own
//! containers, from which the last complete PNG is taken.
//!
//! Size, spacing, thickness (squash), angle, hardness and opacity come
//! across; Clip Studio's dynamics are stored in an undocumented binary form
//! and aren't read, and the notes say so.

use super::{Imported, tip_preset};
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::tip::TipMask;
use eframe::egui::Color32;
use rusqlite::types::ValueRef;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) fn import(bytes: &[u8], stem: &str) -> Result<Imported, String> {
    if !bytes.starts_with(b"SQLite format 3\0") {
        return Err("Not a Clip Studio sub tool".into());
    }
    // SQLite reads files: a private copy in the temporary folder.
    let path = std::env::temp_dir().join(format!(
        "rusty-painter-import-{}-{}.sut",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let result = read(&path, stem);
    let _ = std::fs::remove_file(&path);
    result
}

fn read(path: &std::path::Path, stem: &str) -> Result<Imported, String> {
    let bad = |e: rusqlite::Error| format!("Damaged Clip Studio sub tool ({e})");
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(bad)?;
    let mut out = Imported::default();
    let tips = tips(&db);
    let variants = rows(&db, "Variant").map_err(bad)?;
    let nodes = rows(&db, "Node").map_err(bad)?;
    let get = |row: &HashMap<String, Cell>, key: &str| {
        row.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.clone())
    };
    for node in &nodes {
        let Some(Cell::Int(variant_id)) = get(node, "NodeVariantID") else {
            continue;
        };
        let Some(variant) = variants
            .iter()
            .find(|v| matches!(get(v, "VariantID"), Some(Cell::Int(id)) if id == variant_id))
        else {
            continue;
        };
        let name = match get(node, "NodeName") {
            Some(Cell::Text(t)) if !t.is_empty() => t,
            _ => stem.to_string(),
        };
        let num = |k: &str| match get(variant, k) {
            Some(Cell::Int(i)) => Some(i as f32),
            Some(Cell::Real(r)) => Some(r as f32),
            _ => None,
        };
        let mut preset = match tips.first() {
            Some(tip) if tips.len() == 1 => tip_preset(&name, tip.clone(), 10.0),
            _ => BrushPreset {
                name: name.clone(),
                brush: Brush::new(20.0, 100.0, Color32::BLACK, 10.0),
                file: None,
            },
        };
        if tips.len() > 1 {
            preset.brush.brush_options.pixel_shape = PixelBrushShape::Custom(tips[0].clone());
            preset.brush.brush_options.extra_tips = tips[1..].to_vec();
        }
        let b = &mut preset.brush;
        if let Some(size) = num("BrushSize") {
            b.brush_options.diameter = size.clamp(1.0, 3000.0);
        }
        if let Some(interval) = num("BrushInterval") {
            b.brush_options.spacing = interval.clamp(1.0, 1000.0);
        }
        if let Some(thickness) = num("BrushThickness") {
            b.dynamics.tip.ratio = (thickness / 100.0).clamp(0.02, 1.0);
        }
        if let Some(angle) = num("BrushRotation") {
            b.dynamics.tip.angle = angle;
        }
        if let Some(hardness) = num("BrushHardness") {
            b.brush_options.hardness = hardness.clamp(0.0, 100.0);
        }
        if let Some(opacity) = num("Opacity") {
            b.brush_options.opacity = (opacity / 100.0).clamp(0.0, 1.0);
        }
        b.brush_options.tip_colors = false;
        out.notes.push(format!(
            "{name}: Clip Studio's pressure and other dynamics weren't imported"
        ));
        out.presets.push(preset);
    }
    Ok(out)
}

/// A table cell.
#[derive(Clone, Debug)]
enum Cell {
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    Null,
}

/// Every row of `table`, column name → cell (the columns vary between
/// versions, so they're found by name).
fn rows(db: &rusqlite::Connection, table: &str) -> rusqlite::Result<Vec<HashMap<String, Cell>>> {
    let mut stmt = db.prepare(&format!("SELECT * FROM \"{table}\""))?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let mut query = stmt.query([])?;
    while let Some(row) = query.next()? {
        let mut cells = HashMap::new();
        for (i, name) in names.iter().enumerate() {
            let cell = match row.get_ref(i)? {
                ValueRef::Integer(v) => Cell::Int(v),
                ValueRef::Real(v) => Cell::Real(v),
                ValueRef::Text(t) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => Cell::Blob(b.to_vec()),
                ValueRef::Null => Cell::Null,
            };
            cells.insert(name.clone(), cell);
        }
        out.push(cells);
    }
    Ok(out)
}

/// The tip pictures: the last PNG in each material's data.
fn tips(db: &rusqlite::Connection) -> Vec<Arc<TipMask>> {
    let Ok(materials) = rows(db, "MaterialFile") else {
        return Vec::new();
    };
    materials
        .iter()
        .filter_map(|m| {
            m.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("FileData"))
                .and_then(|(_, v)| match v {
                    Cell::Blob(b) => last_png(b),
                    _ => None,
                })
        })
        .filter_map(|png| image::load_from_memory(png).ok())
        .map(|img| TipMask::from_image(&img))
        .collect()
}

/// The last complete PNG inside `data` (from its signature to its end).
fn last_png(data: &[u8]) -> Option<&[u8]> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let start = data.windows(8).rposition(|w| w == SIGNATURE)?;
    let rest = &data[start..];
    // The IEND chunk: its length (0), type, and CRC.
    let end = rest.windows(4).position(|w| w == b"IEND")? + 8;
    rest.get(..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img =
            image::GrayImage::from_fn(w, h, |x, _| image::Luma([if x < w / 2 { 0 } else { 255 }]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn sut(size: f64, tip: Option<Vec<u8>>) -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("rp-sut-test-{}.sut", rand::random::<u64>()));
        {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE Node (NodeName TEXT, NodeVariantID INTEGER);
                 CREATE TABLE Variant (VariantID INTEGER, BrushSize REAL, BrushInterval REAL,
                     BrushThickness REAL, BrushRotation REAL, Opacity INTEGER);
                 CREATE TABLE MaterialFile (FileData BLOB);
                 INSERT INTO Node VALUES ('G Pen', 7);",
            )
            .unwrap();
            db.execute(
                "INSERT INTO Variant VALUES (7, ?1, 15.0, 60.0, 30.0, 80)",
                [size],
            )
            .unwrap();
            if let Some(tip) = tip {
                // Packed in some container: junk around the PNG.
                let mut blob = b"CSFCHUNK junk".to_vec();
                blob.extend(tip);
                blob.extend(b"more junk");
                db.execute("INSERT INTO MaterialFile VALUES (?1)", [blob])
                    .unwrap();
            }
        }
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        bytes
    }

    #[test]
    fn a_sub_tool_brings_its_size_shape_and_tip() {
        let imported = import(&sut(42.0, Some(png(20, 10))), "file").unwrap();
        let p = &imported.presets[0];
        assert_eq!(p.name, "G Pen");
        let o = &p.brush.brush_options;
        assert_eq!((o.diameter, o.spacing), (42.0, 15.0));
        assert!((o.opacity - 0.8).abs() < 1e-6);
        assert!((p.brush.dynamics.tip.ratio - 0.6).abs() < 1e-6);
        let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
            panic!("the tip picture");
        };
        // The black half paints (on white).
        assert_eq!((tip.width, tip.height), (10, 10));
        // Without a picture: a round brush of that size.
        let plain = import(&sut(12.0, None), "file").unwrap();
        assert!(matches!(
            plain.presets[0].brush.brush_options.pixel_shape,
            PixelBrushShape::Circle
        ));
    }

    #[test]
    fn damaged_sub_tools_are_refused() {
        assert!(import(b"not sqlite", "x").is_err());
        let good = sut(10.0, None);
        assert!(import(&good[..good.len() / 2], "x").is_err());
    }
}
