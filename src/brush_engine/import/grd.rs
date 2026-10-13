//! Photoshop gradients (`.grd`, version 5: Photoshop 6 and later), as the
//! gradient tool's own gradients. Adobe doesn't document the format; it's
//! `8BGR`, the version (5), a descriptor version (16) and an action
//! descriptor (as `.abr` brushes have) whose `GrdL` list holds the
//! gradients: each a `Grad` object with its name (`Nm  `), form (`GrdF`:
//! `CstS` stops, or `ClNs` noise), smoothness (`Intr`), colour stops
//! (`Clrs`: `Clr `, `Type` own/foreground/background, `Lctn` 0..4096,
//! `Mdpn` %) and transparency stops (`Trns`: `Opct` %, `Lctn`, `Mdpn`).
//!
//! A stop's midpoint is the one before it (between it and the stop to its
//! left); here a stop keeps the one after it. Noise gradients are random
//! colours with no stops, and are left out. Smoothness is read as none:
//! Photoshop's smooth interpolation eases between stops a little.

use super::Reader;
use super::abr::{Get, Object, Value, descriptor};
use crate::app::tools::gradient_colors::{CustomGradient, EditStop, StopColor};
use crate::canvas::gradient::OpacityStop;

/// Most stops read per gradient, and gradients per file.
const MAX: usize = 1000;

/// The gradients in a `.grd` file, and how many noise gradients were left
/// out.
pub fn read(bytes: &[u8]) -> Result<(Vec<CustomGradient>, usize), String> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != b"8BGR" {
        return Err("Not a Photoshop gradient file".into());
    }
    let version = r.u16_be()?;
    if version != 5 {
        return Err(format!(
            "Photoshop gradient files of version {version} aren't supported (only those from Photoshop 6 on)"
        ));
    }
    let _descriptor_version = r.u32_be()?;
    let root = descriptor(&mut r)?;
    let list = root
        .get("GrdL")
        .and_then(Value::list)
        .ok_or("No gradients in the file")?;
    let (mut out, mut noise) = (Vec::new(), 0);
    for item in list.iter().take(MAX) {
        // Each entry is an object holding the gradient as `Grad`.
        let Some(g) = item
            .object()
            .and_then(|o| o.get("Grad"))
            .and_then(Value::object)
        else {
            continue;
        };
        if g.get("GrdF").and_then(Value::text) == Some("ClNs") {
            noise += 1;
            continue;
        }
        if let Some(gradient) = gradient(g) {
            out.push(gradient);
        }
    }
    Ok((out, noise))
}

fn number(o: &Object, key: &str) -> Option<f32> {
    o.get(key)
        .and_then(Value::number)
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
}

/// Position 0..4096 as 0..1.
fn location(o: &Object) -> f32 {
    (number(o, "Lctn").unwrap_or(0.0) / 4096.0).clamp(0.0, 1.0)
}

/// Midpoint %, as 0.05..=0.95.
fn midpoint(o: &Object) -> f32 {
    (number(o, "Mdpn").unwrap_or(50.0) / 100.0).clamp(0.05, 0.95)
}

fn gradient(g: &Object) -> Option<CustomGradient> {
    let name = g
        .get("Nm  ")
        .and_then(Value::text)
        .unwrap_or("Photoshop gradient")
        .trim_start_matches("$$$/")
        .to_string();
    // (position, colour, midpoint before it)
    let mut stops: Vec<(f32, StopColor, f32)> = g
        .get("Clrs")?
        .list()?
        .iter()
        .take(MAX)
        .filter_map(Value::object)
        .map(|s| {
            let color = match s.get("Type").and_then(Value::text) {
                Some("FrgC") => StopColor::Primary,
                Some("BckC") => StopColor::Secondary,
                _ => StopColor::Fixed(s.get("Clr ").and_then(Value::object).map_or([0, 0, 0], rgb)),
            };
            (location(s), color, midpoint(s))
        })
        .collect();
    if stops.is_empty() {
        return None;
    }
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut opacity: Vec<(f32, f32, f32)> = g
        .get("Trns")
        .and_then(Value::list)
        .map(|l| {
            l.iter()
                .take(MAX)
                .filter_map(Value::object)
                .map(|s| {
                    let o = (number(s, "Opct").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0);
                    (location(s), o, midpoint(s))
                })
                .collect()
        })
        .unwrap_or_default();
    opacity.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Opaque all along: no opacity stops.
    if opacity.iter().all(|&(_, o, _)| o >= 1.0) {
        opacity.clear();
    }
    // Each stop keeps the midpoint after it: the next one's.
    let after = |i: usize, mids: &dyn Fn(usize) -> Option<f32>| mids(i + 1).unwrap_or(0.5);
    let colour_mid = |i: usize| stops.get(i).map(|s| s.2);
    let opacity_mid = |i: usize| opacity.get(i).map(|s| s.2);
    Some(CustomGradient {
        name,
        stops: (0..stops.len())
            .map(|i| EditStop {
                mid: after(i, &colour_mid),
                ..EditStop::new(stops[i].0, stops[i].1)
            })
            .collect(),
        opacity: (0..opacity.len())
            .map(|i| OpacityStop {
                pos: opacity[i].0,
                opacity: opacity[i].1,
                mid: after(i, &opacity_mid),
            })
            .collect(),
    })
}

/// A stop's colour as sRGB, from whichever model it was picked in (told
/// apart by its fields: the class name isn't kept).
fn rgb(c: &Object) -> [u8; 3] {
    let f = |k| number(c, k).unwrap_or(0.0);
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let rgb: [f32; 3] = if c.get("Rd  ").is_some() {
        [f("Rd  ") / 255.0, f("Grn ") / 255.0, f("Bl  ") / 255.0]
    } else if c.get("H   ").is_some() {
        hsb(f("H   "), f("Strt") / 100.0, f("Brgh") / 100.0)
    } else if c.get("Cyn ").is_some() {
        let [cy, m, y, k] =
            ["Cyn ", "Mgnt", "Ylw ", "Blck"].map(|k| (f(k) / 100.0).clamp(0.0, 1.0));
        // Naively, without a profile.
        [
            (1.0 - cy) * (1.0 - k),
            (1.0 - m) * (1.0 - k),
            (1.0 - y) * (1.0 - k),
        ]
    } else if c.get("Lmnc").is_some() {
        lab(f("Lmnc"), f("A   "), f("B   "))
    } else if c.get("Gry ").is_some() {
        // Ink: 0 % is white.
        [1.0 - (f("Gry ") / 100.0).clamp(0.0, 1.0); 3]
    } else {
        [0.0; 3]
    };
    rgb.map(q)
}

/// Hue in degrees, saturation and brightness 0..1, as RGB 0..1.
fn hsb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let [r, g, b] = match h as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// CIE L*a*b* (D65 white) as sRGB 0..1.
fn lab(l: f32, a: f32, b: f32) -> [f32; 3] {
    let fy = (l.clamp(0.0, 100.0) + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.950_47 * inv(fx), inv(fy), 1.088_83 * inv(fz));
    let lin = [
        3.240_45 * x - 1.537_14 * y - 0.498_53 * z,
        -0.969_27 * x + 1.876_01 * y + 0.041_56 * z,
        0.055_64 * x - 0.204_03 * y + 1.057_23 * z,
    ];
    lin.map(|c| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor as Photoshop writes one.
    struct W(Vec<u8>);

    impl W {
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_be_bytes());
            self
        }
        fn key(&mut self, k: &str) -> &mut Self {
            self.u32(0);
            self.0.extend_from_slice(k.as_bytes());
            self
        }
        fn text(&mut self, t: &str) -> &mut Self {
            let units: Vec<u16> = t.encode_utf16().chain([0]).collect();
            self.u32(units.len() as u32);
            for u in units {
                self.0.extend_from_slice(&u.to_be_bytes());
            }
            self
        }
        fn object(&mut self, class: &str, n: u32) -> &mut Self {
            self.text("").key(class).u32(n)
        }
        fn doub(&mut self, key: &str, v: f64) -> &mut Self {
            self.key(key);
            self.0.extend_from_slice(b"doub");
            self.0.extend_from_slice(&v.to_be_bytes());
            self
        }
        fn long(&mut self, key: &str, v: i32) -> &mut Self {
            self.key(key);
            self.0.extend_from_slice(b"long");
            self.0.extend_from_slice(&v.to_be_bytes());
            self
        }
        fn unit(&mut self, key: &str, v: f64) -> &mut Self {
            self.key(key);
            self.0.extend_from_slice(b"UntF#Prc");
            self.0.extend_from_slice(&v.to_be_bytes());
            self
        }
        fn enumerated(&mut self, key: &str, ty: &str, v: &str) -> &mut Self {
            self.key(key);
            self.0.extend_from_slice(b"enum");
            self.key(ty).key(v)
        }
        fn begin(&mut self, key: &str, kind: &[u8]) -> &mut Self {
            self.key(key);
            self.0.extend_from_slice(kind);
            self
        }
    }

    /// A red-to-foreground gradient, half clear at 75 %, with a 30 %
    /// midpoint before the second colour stop.
    fn file(form: &str) -> Vec<u8> {
        let mut w = W(b"8BGR".to_vec());
        w.0.extend_from_slice(&5u16.to_be_bytes());
        w.u32(16).object("null", 1).begin("GrdL", b"VlLs").u32(1);
        w.0.extend_from_slice(b"Objc");
        w.object("Grdn", 1).begin("Grad", b"Objc").object("Grdn", 5);
        w.begin("Nm  ", b"TEXT").text("$$$/Sunrise");
        w.enumerated("GrdF", "GrdF", form);
        w.doub("Intr", 4096.0);
        w.begin("Clrs", b"VlLs").u32(2);
        // A red stop at 0, then the foreground colour at the end.
        w.0.extend_from_slice(b"Objc");
        w.object("Clrt", 4).begin("Clr ", b"Objc").object("RGBC", 3);
        w.doub("Rd  ", 255.0).doub("Grn ", 0.0).doub("Bl  ", 0.0);
        w.enumerated("Type", "Clry", "UsrS")
            .long("Lctn", 0)
            .long("Mdpn", 50);
        w.0.extend_from_slice(b"Objc");
        w.object("Clrt", 4).begin("Clr ", b"Objc").object("Grsc", 1);
        w.doub("Gry ", 100.0);
        w.enumerated("Type", "Clry", "FrgC")
            .long("Lctn", 4096)
            .long("Mdpn", 30);
        w.begin("Trns", b"VlLs").u32(2);
        w.0.extend_from_slice(b"Objc");
        w.object("TrnS", 3)
            .unit("Opct", 100.0)
            .long("Lctn", 0)
            .long("Mdpn", 50);
        w.0.extend_from_slice(b"Objc");
        w.object("TrnS", 3)
            .unit("Opct", 50.0)
            .long("Lctn", 3072)
            .long("Mdpn", 50);
        w.0
    }

    #[test]
    fn a_photoshop_gradient_comes_in_with_its_stops_and_midpoints() {
        let (gradients, noise) = read(&file("CstS")).unwrap();
        assert_eq!(noise, 0);
        let g = &gradients[0];
        assert_eq!(g.name, "Sunrise");
        assert_eq!(g.stops[0].color, StopColor::Fixed([255, 0, 0]));
        assert_eq!(
            g.stops[1].color,
            StopColor::Primary,
            "the foreground colour"
        );
        assert!(
            (g.stops[0].mid - 0.3).abs() < 1e-6,
            "the midpoint after the first stop"
        );
        assert_eq!(g.opacity.len(), 2);
        assert_eq!((g.opacity[1].pos, g.opacity[1].opacity), (0.75, 0.5));
    }

    #[test]
    fn noise_gradients_are_left_out_and_damage_is_refused() {
        assert_eq!(read(&file("ClNs")).unwrap(), (Vec::new(), 1));
        let whole = file("CstS");
        for cut in (0..whole.len()).step_by(7) {
            // Never a panic: an error, or what was read before the cut.
            let _ = read(&whole[..cut]);
        }
        assert!(read(b"8BIM").is_err());
    }

    #[test]
    fn imported_gradients_join_the_users() {
        let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
            32,
            32,
            eframe::egui::Color32::WHITE,
            64,
        ));
        let before = app.workspace.gradient.library.custom.len();
        app.import_gradients_in_background(
            "sunrise.grd".into(),
            crate::app::import::FileSource::Bytes(file("CstS").into()),
        );
        app.run_jobs();
        let library = &app.workspace.gradient.library;
        assert_eq!(library.custom.len(), before + 1);
        assert_eq!(library.custom[before].name, "Sunrise");
    }

    #[test]
    fn colour_models_convert_to_srgb() {
        assert_eq!(hsb(120.0, 1.0, 1.0), [0.0, 1.0, 0.0]);
        let white = lab(100.0, 0.0, 0.0);
        assert!(white.iter().all(|&c| c > 0.99), "{white:?}");
    }
}
