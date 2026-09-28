//! Photoshop brushes (`.abr`). Adobe doesn't document the format; this
//! follows what GIMP and Krita read (tips) and what brush tools have worked
//! out about the settings (the `desc` block, an "action descriptor").
//!
//! - Versions 1 and 2 (Photoshop 6 and older): a list of brushes, each a
//!   computed round tip (size, hardness, roundness, angle) or a sampled
//!   picture.
//! - Version 6 and later: `8BIM` blocks: `samp` holds the pictures (each
//!   named by an id), `desc` the presets (name, tip, size, spacing and the
//!   shape, scatter and transfer dynamics).

use super::{Imported, Reader, unpack_bits};
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::tip::TipMask;
use eframe::egui::Color32;
use std::sync::Arc;

/// Largest tip side read.
const MAX_SIDE: i32 = 8192;

pub(super) fn import(bytes: &[u8], stem: &str) -> Result<Imported, String> {
    let mut r = Reader::new(bytes);
    match r.u16_be()? {
        v @ (1 | 2) => old_versions(&mut r, v, stem),
        6..=10 => new_versions(&mut r, stem),
        v => Err(format!("Photoshop brush version {v} isn't supported")),
    }
}

/// Versions 1 and 2.
fn old_versions(r: &mut Reader<'_>, version: u16, stem: &str) -> Result<Imported, String> {
    let count = r.u16_be()?;
    let mut out = Imported::default();
    for i in 0..count {
        let kind = r.u16_be()?;
        let size = r.u32_be()? as usize;
        let end = r.pos() + size;
        let name = format!("{stem} {}", i + 1);
        match kind {
            1 => {
                let _misc = r.u32_be()?;
                let spacing = r.u16_be()? as f32;
                let diameter = r.u16_be()? as f32;
                let roundness = r.u16_be()? as f32;
                let angle = r.u16_be()? as i16 as f32;
                let hardness = r.u16_be()? as f32;
                let mut b = Brush::new(
                    diameter.max(1.0),
                    hardness,
                    Color32::BLACK,
                    spacing.max(1.0),
                );
                b.dynamics.tip.ratio = (roundness / 100.0).clamp(0.02, 1.0);
                b.dynamics.tip.angle = angle;
                out.presets.push(BrushPreset {
                    name,
                    brush: b,
                    file: None,
                });
            }
            2 => {
                let _misc = r.u32_be()?;
                let spacing = r.u16_be()? as f32;
                let name = if version == 2 {
                    let len = r.u32_be()? as usize;
                    utf16(r.take(len * 2)?)
                } else {
                    name
                };
                let _antialias = r.u8()?;
                r.skip(8)?; // the bounds as shorts
                let tip = read_sample(r)?;
                let mut preset = super::tip_preset(&name, tip, spacing.max(1.0));
                preset.brush.brush_options.tip_colors = false;
                out.presets.push(preset);
            }
            _ => out
                .notes
                .push(format!("Skipped a brush of an unknown kind ({kind})")),
        }
        r.seek(end)?;
    }
    Ok(out)
}

/// A sampled tip: its bounds, depth, compression and pixels.
fn read_sample(r: &mut Reader<'_>) -> Result<Arc<TipMask>, String> {
    let (top, left, bottom, right) = (r.i32_be()?, r.i32_be()?, r.i32_be()?, r.i32_be()?);
    let depth = r.u16_be()?;
    let compression = r.u8()?;
    let (w, h) = (right - left, bottom - top);
    if w <= 0 || h <= 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err("A Photoshop brush tip has a bad size".into());
    }
    let (w, h) = (w as usize, h as usize);
    let pixels = match (compression, depth) {
        (0, 8) => r.take(w * h)?.to_vec(),
        // 16-bit: the high bytes.
        (0, 16) => r
            .take(w * h * 2)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| p[0])
            .collect(),
        (1, 8) => {
            let lengths: Vec<usize> = (0..h)
                .map(|_| r.u16_be().map(usize::from))
                .collect::<Result<_, _>>()?;
            let mut out = Vec::with_capacity(w * h);
            for len in lengths {
                let before = out.len();
                unpack_bits(r, len, &mut out)?;
                out.resize(before + w, 0);
            }
            out
        }
        _ => {
            return Err(format!(
                "Photoshop tips of depth {depth} (compression {compression}) aren't supported"
            ));
        }
    };
    Ok(TipMask::from_mask(w, h, pixels))
}

/// Versions 6 and later: the `8BIM` blocks.
fn new_versions(r: &mut Reader<'_>, stem: &str) -> Result<Imported, String> {
    let subversion = r.u16_be()?;
    let mut samples: Vec<(String, Arc<TipMask>)> = Vec::new();
    let mut desc = None;
    let mut out = Imported::default();
    while r.pos() + 12 <= r.len() {
        if r.take(4)? != b"8BIM" {
            break;
        }
        let key = r.take(4)?.to_vec();
        let len = r.u32_be()? as usize;
        let start = r.pos();
        let end = start + len;
        match &key[..] {
            b"samp" => {
                while r.pos() + 4 <= end {
                    let size = r.u32_be()? as usize;
                    let sample_end = r.pos() + size;
                    let id_len = r.u8()? as usize;
                    let id = String::from_utf8_lossy(r.take(id_len)?).into_owned();
                    // What comes between the id and the bounds differs by
                    // subversion (as GIMP reads it).
                    r.skip(if subversion == 1 { 10 } else { 264 })?;
                    match read_sample(r) {
                        Ok(tip) => samples.push((id, tip)),
                        Err(err) => out.notes.push(err),
                    }
                    // Samples are padded to 4 bytes.
                    r.seek(((sample_end + 3) & !3).min(end))?;
                }
            }
            b"desc" => {
                let mut d = Reader::new(r.take(len)?);
                // A version number, then the descriptor.
                let _ = d.u32_be();
                desc = descriptor(&mut d).ok();
            }
            _ => {}
        }
        // Blocks are padded to an even length (the last one may not be).
        r.seek(((end + 1) & !1).min(r.len()))?;
    }
    let presets = desc
        .as_ref()
        .and_then(|d| d.get("Brsh"))
        .and_then(Value::list)
        .map(|list| {
            list.iter()
                .filter_map(Value::object)
                .filter_map(|p| preset_from(p, &samples, &mut out.notes))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if presets.is_empty() {
        // No settings (or unreadable ones): the tips on their own.
        if desc.is_none() && !samples.is_empty() {
            out.notes.push(format!(
                "{stem}: only the tips were imported (no readable settings)"
            ));
        }
        for (i, (_, tip)) in samples.into_iter().enumerate() {
            let mut preset = super::tip_preset(&format!("{stem} {}", i + 1), tip, 25.0);
            preset.brush.brush_options.tip_colors = false;
            out.presets.push(preset);
        }
    } else {
        out.presets = presets;
    }
    Ok(out)
}

/// A preset from one entry of `desc`'s brush list.
fn preset_from(
    p: &Object,
    samples: &[(String, Arc<TipMask>)],
    notes: &mut Vec<String>,
) -> Option<BrushPreset> {
    let name = p
        .get("Nm  ")
        .and_then(Value::text)
        .unwrap_or("Brush")
        .to_string();
    let tip = p.get("Brsh").and_then(Value::object)?;
    let num = |o: &Object, k: &str| o.get(k).and_then(Value::number);
    let diameter = num(tip, "Dmtr").unwrap_or(25.0).clamp(1.0, 3000.0) as f32;
    let spacing = num(tip, "Spcn").unwrap_or(25.0).clamp(1.0, 1000.0) as f32;
    let hardness = num(tip, "Hrdn").unwrap_or(100.0).clamp(0.0, 100.0) as f32;
    let mut b = Brush::new(diameter, hardness, Color32::BLACK, spacing);
    b.brush_options.pressure_size = false;
    b.dynamics.tip.angle = num(tip, "Angl").unwrap_or(0.0) as f32;
    b.dynamics.tip.ratio = (num(tip, "Rndn").unwrap_or(100.0) as f32 / 100.0).clamp(0.02, 1.0);
    if let Some(id) = tip.get("sampledData").and_then(Value::text) {
        match samples
            .iter()
            .find(|(s, _)| s.trim_start_matches('$') == id.trim_start_matches('$'))
        {
            Some((_, t)) => b.brush_options.pixel_shape = PixelBrushShape::Custom(t.clone()),
            None => notes.push(format!("{name}: its tip picture is missing")),
        }
    }
    let flag = |k: &str| p.get(k).and_then(Value::boolean).unwrap_or(false);
    // Which control drives a dynamic (2: pen pressure), and its jitter.
    let dynamic = |k: &str| {
        let o = p.get(k).and_then(Value::object)?;
        let control = o.get("bVTy").and_then(Value::number).unwrap_or(0.0) as i32;
        let jitter = o.get("jitter").and_then(Value::number).unwrap_or(0.0) as f32 / 100.0;
        Some((control, jitter))
    };
    if flag("useTipDynamics") {
        if let Some((control, jitter)) = dynamic("szVr") {
            b.brush_options.pressure_size = control == 2;
            b.dynamics.random.size = jitter.clamp(0.0, 1.0);
        }
        b.brush_options.pressure_min_size =
            (num(p, "minimumDiameter").unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0);
        if let Some((_, jitter)) = dynamic("angleDynamics") {
            b.dynamics.tip.random_angle = (jitter * 180.0).clamp(0.0, 180.0);
        }
    }
    if flag("useScatter") {
        if let Some((_, jitter)) = dynamic("scatterDynamics") {
            // Photoshop's scatter is a share of the size either way.
            b.jitter = (jitter * 50.0).clamp(0.0, 500.0);
        }
        if let Some(count) = num(p, "Cnt ") {
            b.dynamics.random.count = (count as u32).clamp(1, 16);
        }
    }
    if flag("usePaintDynamics") {
        if let Some((control, jitter)) = dynamic("opVr") {
            b.brush_options.pressure_opacity = control == 2;
            b.dynamics.random.opacity = jitter.clamp(0.0, 1.0);
        }
        if let Some((control, _)) = dynamic("prVr") {
            b.brush_options.pressure_flow = control == 2;
        }
    }
    for (key, what) in [
        ("useTexture", "texture"),
        ("useDualBrush", "dual brush"),
        ("useColorDynamics", "colour dynamics"),
        ("Wtdg", "wet edges"),
    ] {
        if flag(key) {
            notes.push(format!("{name}: its {what} wasn't imported"));
        }
    }
    Some(BrushPreset {
        name,
        brush: b,
        file: None,
    })
}

/// An action descriptor's values.
#[derive(Debug)]
enum Value {
    Object(Object),
    List(Vec<Value>),
    Number(f64),
    Bool(bool),
    Text(String),
    Other,
}

type Object = Vec<(String, Value)>;

trait Get {
    fn get(&self, key: &str) -> Option<&Value>;
}

impl Get for Object {
    fn get(&self, key: &str) -> Option<&Value> {
        self.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl Value {
    fn object(&self) -> Option<&Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    fn list(&self) -> Option<&Vec<Value>> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    fn boolean(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

fn unicode(r: &mut Reader<'_>) -> Result<String, String> {
    let len = r.u32_be()? as usize;
    if len > 1 << 20 {
        return Err("Damaged descriptor".into());
    }
    Ok(utf16(r.take(len * 2)?))
}

/// A key: 4 characters, or a longer name.
fn key(r: &mut Reader<'_>) -> Result<String, String> {
    let len = r.u32_be()? as usize;
    let len = if len == 0 { 4 } else { len };
    if len > 1 << 16 {
        return Err("Damaged descriptor".into());
    }
    Ok(String::from_utf8_lossy(r.take(len)?).into_owned())
}

fn descriptor(r: &mut Reader<'_>) -> Result<Object, String> {
    let _name = unicode(r)?;
    let _class = key(r)?;
    let count = r.u32_be()?;
    if count > 1 << 16 {
        return Err("Damaged descriptor".into());
    }
    (0..count).map(|_| Ok((key(r)?, value(r)?))).collect()
}

fn value(r: &mut Reader<'_>) -> Result<Value, String> {
    let kind = r.take(4)?;
    Ok(match kind {
        b"Objc" | b"GlbO" => Value::Object(descriptor(r)?),
        b"VlLs" => {
            let count = r.u32_be()?;
            if count > 1 << 16 {
                return Err("Damaged descriptor".into());
            }
            Value::List((0..count).map(|_| value(r)).collect::<Result<_, _>>()?)
        }
        b"doub" => Value::Number(r.f64_be()?),
        b"UntF" => {
            r.skip(4)?;
            Value::Number(r.f64_be()?)
        }
        b"UnFl" => {
            r.skip(4)?;
            let count = r.u32_be()? as usize;
            let first = r.f64_be()?;
            r.skip(count.saturating_sub(1) * 8)?;
            Value::Number(first)
        }
        b"long" => Value::Number(r.i32_be()? as f64),
        b"comp" => {
            let v = i64::from_be_bytes(r.take(8)?.try_into().expect("8 bytes"));
            Value::Number(v as f64)
        }
        b"bool" => Value::Bool(r.u8()? != 0),
        b"TEXT" => Value::Text(unicode(r)?),
        b"enum" => {
            let _type = key(r)?;
            Value::Text(key(r)?)
        }
        b"type" | b"GlbC" => {
            let _name = unicode(r)?;
            let _class = key(r)?;
            Value::Other
        }
        b"tdta" | b"alis" => {
            let len = r.u32_be()? as usize;
            r.skip(len)?;
            Value::Other
        }
        other => {
            return Err(format!(
                "Unsupported descriptor value {}",
                String::from_utf8_lossy(other)
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_key(out: &mut Vec<u8>, k: &str) {
        if k.len() == 4 {
            out.extend_from_slice(&0u32.to_be_bytes());
        } else {
            out.extend_from_slice(&(k.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(k.as_bytes());
    }

    fn put_text(out: &mut Vec<u8>, t: &str) {
        let units: Vec<u16> = t.encode_utf16().chain([0]).collect();
        out.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for u in units {
            out.extend_from_slice(&u.to_be_bytes());
        }
    }

    fn put_untf(out: &mut Vec<u8>, k: &str, v: f64) {
        put_key(out, k);
        out.extend_from_slice(b"UntF#Pxl");
        out.extend_from_slice(&v.to_be_bytes());
    }

    fn sample(id: &str, w: i32, h: i32) -> Vec<u8> {
        let mut s = Vec::new();
        s.push(id.len() as u8);
        s.extend_from_slice(id.as_bytes());
        s.extend_from_slice(&[0; 10]);
        for v in [0, 0, h, w] {
            s.extend_from_slice(&v.to_be_bytes());
        }
        s.extend_from_slice(&8u16.to_be_bytes());
        s.push(0);
        s.extend((0..w * h).map(|i| if i % 2 == 0 { 255 } else { 0 }));
        s
    }

    /// A version 6 file: one 20×10 sample, and a preset using it at size
    /// 60 with pressure on size and 30% scatter.
    pub fn abr6() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&6u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        let s = sample("$abc", 20, 10);
        let mut samp = (s.len() as u32).to_be_bytes().to_vec();
        samp.extend_from_slice(&s);
        while !samp.len().is_multiple_of(4) {
            samp.push(0);
        }
        out.extend_from_slice(b"8BIMsamp");
        out.extend_from_slice(&(samp.len() as u32).to_be_bytes());
        out.extend_from_slice(&samp);
        // desc: { Brsh: [ { Nm: "Chalk", Brsh: {Dmtr 60, Spcn 30,
        // sampledData "$abc"}, useTipDynamics, szVr: {bVTy 2},
        // useScatter, scatterDynamics: {jitter 30} } ] }
        let mut d = 16u32.to_be_bytes().to_vec();
        put_text(&mut d, "");
        put_key(&mut d, "null");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_key(&mut d, "Brsh");
        d.extend_from_slice(b"VlLs");
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brushPreset");
        d.extend_from_slice(&6u32.to_be_bytes());
        put_key(&mut d, "Nm  ");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "Chalk");
        put_key(&mut d, "Brsh");
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "sampledBrush");
        d.extend_from_slice(&3u32.to_be_bytes());
        put_untf(&mut d, "Dmtr", 60.0);
        put_untf(&mut d, "Spcn", 30.0);
        put_key(&mut d, "sampledData");
        d.extend_from_slice(b"TEXT");
        put_text(&mut d, "$abc");
        put_key(&mut d, "useTipDynamics");
        d.extend_from_slice(b"bool\x01");
        put_key(&mut d, "szVr");
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brVr");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_key(&mut d, "bVTy");
        d.extend_from_slice(b"long");
        d.extend_from_slice(&2i32.to_be_bytes());
        put_key(&mut d, "useScatter");
        d.extend_from_slice(b"bool\x01");
        put_key(&mut d, "scatterDynamics");
        d.extend_from_slice(b"Objc");
        put_text(&mut d, "");
        put_key(&mut d, "brVr");
        d.extend_from_slice(&1u32.to_be_bytes());
        put_untf(&mut d, "jitter", 30.0);
        out.extend_from_slice(b"8BIMdesc");
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        out.extend_from_slice(&d);
        out
    }

    #[test]
    fn a_version_6_brush_brings_its_tip_and_settings() {
        let imported = import(&abr6(), "set").unwrap();
        assert_eq!(imported.presets.len(), 1, "{:?}", imported.notes);
        let p = &imported.presets[0];
        assert_eq!(p.name, "Chalk");
        let o = &p.brush.brush_options;
        assert_eq!((o.diameter, o.spacing), (60.0, 30.0));
        assert!(o.pressure_size);
        assert!((p.brush.jitter - 15.0).abs() < 1e-3);
        let PixelBrushShape::Custom(tip) = &o.pixel_shape else {
            panic!("the sampled tip");
        };
        assert_eq!((tip.width, tip.height), (19, 10));
    }

    #[test]
    fn a_version_1_file_brings_computed_and_sampled_brushes() {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        // Computed: size 40, roundness 50, angle 30, hardness 80, spacing 20.
        let mut c = 0u32.to_be_bytes().to_vec();
        for v in [20u16, 40, 50, 30, 80] {
            c.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(c.len() as u32).to_be_bytes());
        out.extend_from_slice(&c);
        // Sampled, 8×8, run-length encoded rows of full paint.
        let mut s = 0u32.to_be_bytes().to_vec();
        s.extend_from_slice(&25u16.to_be_bytes());
        s.push(1);
        s.extend_from_slice(&[0; 8]);
        for v in [0i32, 0, 8, 8] {
            s.extend_from_slice(&v.to_be_bytes());
        }
        s.extend_from_slice(&8u16.to_be_bytes());
        s.push(1);
        for _ in 0..8 {
            s.extend_from_slice(&2u16.to_be_bytes());
        }
        for _ in 0..8 {
            s.extend_from_slice(&[0xF9, 255]);
        }
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        out.extend_from_slice(&s);
        let imported = import(&out, "old").unwrap();
        assert_eq!(imported.presets.len(), 2);
        let computed = &imported.presets[0].brush;
        assert_eq!(computed.brush_options.diameter, 40.0);
        assert_eq!(computed.dynamics.tip.ratio, 0.5);
        let PixelBrushShape::Custom(tip) = &imported.presets[1].brush.brush_options.pixel_shape
        else {
            panic!("a sampled tip");
        };
        assert_eq!((tip.width, tip.height), (8, 8));
        assert!(tip.pixels.iter().all(|&p| p == 255));
    }

    #[test]
    fn damaged_photoshop_brushes_are_refused_not_panicked_on() {
        let good = abr6();
        for cut in (0..good.len()).step_by(7) {
            let _ = import(&good[..cut], "cut");
        }
        assert!(import(&[0, 99], "bad").is_err());
    }
}
