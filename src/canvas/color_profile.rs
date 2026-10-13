//! Colour management: the colour space a document's numbers mean (its
//! profile), converting between profiles, showing the document on the
//! monitor's profile, proofing it against a print (CMYK) profile, and
//! writing CMYK. Built on Little CMS.
//!
//! A document's pixels are stored as before (sRGB-style encoded numbers);
//! the profile says which colours those numbers are. The app's "linear
//! light" decodes them with the sRGB curve whatever the profile, which is
//! exact for sRGB and Display P3 (they share it) and close for the others.

use eframe::egui::Color32;
use lcms2::{
    CIExyY, CIExyYTRIPLE, DisallowCache, Flags, GlobalContext, Intent, PixelFormat, Profile,
    ToneCurve, Transform,
};
use std::sync::Arc;

/// Which colours a document's numbers are.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ColorProfile {
    #[default]
    Srgb,
    /// Wide gamut, with sRGB's curve (Apple's displays).
    DisplayP3,
    /// Adobe RGB (1998).
    AdobeRgb,
    /// ITU-R BT.2020, with BT.709's curve.
    Rec2020,
    /// An RGB ICC profile, from a file or a document.
    Icc { name: String, data: Arc<Vec<u8>> },
}

/// What [`ColorProfile`] uses for its built-in profiles.
const D65: CIExyY = CIExyY {
    x: 0.3127,
    y: 0.3290,
    Y: 1.0,
};

fn primaries(r: (f64, f64), g: (f64, f64), b: (f64, f64)) -> CIExyYTRIPLE {
    let p = |(x, y)| CIExyY { x, y, Y: 1.0 };
    CIExyYTRIPLE {
        Red: p(r),
        Green: p(g),
        Blue: p(b),
    }
}

/// An RGB profile with D65 white, these primaries and one curve.
fn rgb_profile(p: CIExyYTRIPLE, curve: &ToneCurve) -> Profile {
    Profile::new_rgb(&D65, &p, &[curve, curve, curve]).unwrap_or_else(|_| Profile::new_srgb())
}

/// sRGB's piecewise curve.
fn srgb_curve() -> ToneCurve {
    ToneCurve::new_parametric(4, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])
        .unwrap_or_else(|_| ToneCurve::new(2.2))
}

impl ColorProfile {
    /// The built-in profiles, in the order menus list them.
    pub const BUILT_IN: [ColorProfile; 4] = [
        ColorProfile::Srgb,
        ColorProfile::DisplayP3,
        ColorProfile::AdobeRgb,
        ColorProfile::Rec2020,
    ];

    pub fn label(&self) -> &str {
        match self {
            ColorProfile::Srgb => "sRGB",
            ColorProfile::DisplayP3 => "Display P3",
            ColorProfile::AdobeRgb => "Adobe RGB (1998)",
            ColorProfile::Rec2020 => "Rec. 2020",
            ColorProfile::Icc { name, .. } => name,
        }
    }

    /// A short key for a built-in profile (saved in files); `None` for an
    /// ICC one (saved as its bytes).
    pub fn key(&self) -> Option<&'static str> {
        Some(match self {
            ColorProfile::Srgb => "srgb",
            ColorProfile::DisplayP3 => "display-p3",
            ColorProfile::AdobeRgb => "adobe-rgb",
            ColorProfile::Rec2020 => "rec2020",
            ColorProfile::Icc { .. } => return None,
        })
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::BUILT_IN.into_iter().find(|p| p.key() == Some(key))
    }

    /// An RGB ICC profile's bytes as a profile (`name` if it has no
    /// description). A built-in one when it's the same as one of them.
    pub fn from_icc(data: Vec<u8>, name: &str) -> Result<Self, String> {
        let profile = Profile::new_icc(&data).map_err(|_| "Not an ICC profile".to_string())?;
        if profile.color_space() != lcms2::ColorSpaceSignature::RgbData {
            return Err("Not an RGB profile".into());
        }
        let description = profile
            .info(lcms2::InfoType::Description, lcms2::Locale::none())
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| name.to_string());
        // The usual sRGB profiles (many apps embed one) are ours.
        if description.to_ascii_lowercase().starts_with("srgb")
            && same_colours(&profile, &Self::Srgb.lcms())
        {
            return Ok(Self::Srgb);
        }
        Ok(Self::Icc {
            name: description,
            data: Arc::new(data),
        })
    }

    /// The profile for Little CMS.
    pub fn lcms(&self) -> Profile {
        match self {
            ColorProfile::Srgb => Profile::new_srgb(),
            ColorProfile::DisplayP3 => rgb_profile(
                primaries((0.680, 0.320), (0.265, 0.690), (0.150, 0.060)),
                &srgb_curve(),
            ),
            ColorProfile::AdobeRgb => rgb_profile(
                primaries((0.640, 0.330), (0.210, 0.710), (0.150, 0.060)),
                &ToneCurve::new(563.0 / 256.0),
            ),
            ColorProfile::Rec2020 => rgb_profile(
                primaries((0.708, 0.292), (0.170, 0.797), (0.131, 0.046)),
                &ToneCurve::new_parametric(
                    4,
                    &[
                        1.0 / 0.45,
                        1.0 / 1.099_3,
                        0.099_3 / 1.099_3,
                        1.0 / 4.5,
                        0.081,
                    ],
                )
                .unwrap_or_else(|_| ToneCurve::new(2.4)),
            ),
            ColorProfile::Icc { data, .. } => {
                Profile::new_icc(data).unwrap_or_else(|_| Profile::new_srgb())
            }
        }
    }

    /// The profile's ICC bytes, to embed in an exported file.
    pub fn icc(&self) -> Vec<u8> {
        match self {
            ColorProfile::Icc { data, .. } => data.to_vec(),
            _ => {
                let mut profile = self.lcms();
                set_description(&mut profile, self.label());
                profile.icc().unwrap_or_default()
            }
        }
    }
}

/// Write `text` as the profile's description (what other apps show).
fn set_description(profile: &mut Profile, text: &str) {
    let mut mlu = lcms2::MLU::new(1);
    if mlu.set_text(text, lcms2::Locale::none()) {
        profile.write_tag(
            lcms2::TagSignature::ProfileDescriptionTag,
            lcms2::Tag::MLU(&mlu),
        );
    }
}

/// Whether two RGB profiles give the same colours (to within a little), on
/// a few test colours.
fn same_colours(a: &Profile, b: &Profile) -> bool {
    let Ok(t) = Transform::<[f32; 3], [f32; 3]>::new(
        a,
        PixelFormat::RGB_FLT,
        b,
        PixelFormat::RGB_FLT,
        Intent::RelativeColorimetric,
    ) else {
        return false;
    };
    let tests = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.5, 0.5, 0.5],
        [0.2, 0.7, 0.4],
    ];
    let mut out = [[0.0f32; 3]; 5];
    t.transform_pixels(&tests, &mut out);
    tests
        .iter()
        .zip(&out)
        .all(|(i, o)| (0..3).all(|c| (i[c] - o[c]).abs() < 0.01))
}

/// How colours are mapped into a smaller gamut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RenderingIntent {
    /// Squeezes the whole gamut in, keeping how colours relate.
    #[default]
    Perceptual,
    /// Keeps colours that fit exactly, clipping the others.
    RelativeColorimetric,
    /// Keeps colours bright and vivid.
    Saturation,
}

impl RenderingIntent {
    pub const ALL: [RenderingIntent; 3] = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RenderingIntent::Perceptual => "Perceptual",
            RenderingIntent::RelativeColorimetric => "Relative colorimetric",
            RenderingIntent::Saturation => "Saturation",
        }
    }

    fn lcms(self) -> Intent {
        match self {
            RenderingIntent::Perceptual => Intent::Perceptual,
            RenderingIntent::RelativeColorimetric => Intent::RelativeColorimetric,
            RenderingIntent::Saturation => Intent::Saturation,
        }
    }
}

/// Unmultiplied RGB (encoded, 0..1) from one profile to another, at full
/// precision. Usable from many threads at once.
pub struct RgbTransform(Transform<[f32; 3], [f32; 3], GlobalContext, DisallowCache>);

impl RgbTransform {
    pub fn new(
        from: &ColorProfile,
        to: &ColorProfile,
        intent: RenderingIntent,
    ) -> Result<Self, String> {
        Transform::new_flags_context(
            GlobalContext::new(),
            &from.lcms(),
            PixelFormat::RGB_FLT,
            &to.lcms(),
            PixelFormat::RGB_FLT,
            intent.lcms(),
            Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION,
        )
        .map(Self)
        .map_err(|e| format!("Can't convert between these profiles: {e}"))
    }

    /// Convert `pixels` in place.
    pub fn convert(&self, pixels: &mut [[f32; 3]]) {
        let src = pixels.to_vec();
        self.0.transform_pixels(&src, pixels);
    }

    /// One premultiplied linear-light pixel (see `DeepTile::linear`).
    pub fn convert_linear(&self, px: [f32; 4]) -> [f32; 4] {
        use eframe::egui::ecolor::{gamma_from_linear, linear_from_gamma};
        let a = px[3];
        if a <= 0.0 {
            return [0.0; 4];
        }
        let mut rgb = [[0, 1, 2].map(|c| gamma_from_linear((px[c] / a).max(0.0)))];
        self.convert(&mut rgb);
        let [r, g, b] = rgb[0].map(|v| linear_from_gamma(v.max(0.0)) * a);
        [r, g, b, a]
    }

    /// 8-bit premultiplied pixels in place.
    pub fn convert_pixels(&self, pixels: &mut [Color32]) {
        let mut rgb: Vec<[f32; 3]> = pixels
            .iter()
            .map(|p| {
                let [r, g, b, _] = crate::canvas::blend::unmultiply(*p);
                [r, g, b].map(|v| v as f32 / 255.0)
            })
            .collect();
        self.convert(&mut rgb);
        for (p, c) in pixels.iter_mut().zip(rgb) {
            if p.a() == 0 {
                continue;
            }
            let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            *p = Color32::from_rgba_unmultiplied(r, g, b, p.a());
        }
    }
}

/// How the canvas is shown when the document's colours need converting for
/// the monitor (or proofing): 8-bit premultiplied pixels, transformed as
/// they go to the GPU. Usable from many threads at once.
pub struct DisplayTransform {
    /// (Locked: a proofing transform can't be shared without.)
    transform: std::sync::Mutex<Transform<[u8; 3], [u8; 3]>>,
}

impl DisplayTransform {
    /// From `document` to `monitor`, or `None` when they're the same (the
    /// pixels show as they are).
    pub fn new(document: &ColorProfile, monitor: &ColorProfile) -> Option<Self> {
        if document == monitor {
            return None;
        }
        let transform = Transform::new_flags(
            &document.lcms(),
            PixelFormat::RGB_8,
            &monitor.lcms(),
            PixelFormat::RGB_8,
            Intent::RelativeColorimetric,
            Flags::BLACKPOINT_COMPENSATION,
        )
        .ok()?;
        Some(Self {
            transform: std::sync::Mutex::new(transform),
        })
    }

    /// Showing `document` on `monitor` as it would print on `proof` (a
    /// CMYK profile); with `gamut_warning`, colours it can't print show in
    /// grey.
    pub fn proofing(
        document: &ColorProfile,
        monitor: &ColorProfile,
        proof: &CmykProfile,
        intent: RenderingIntent,
        gamut_warning: bool,
    ) -> Option<Self> {
        let mut flags = Flags::SOFT_PROOFING;
        if gamut_warning {
            flags = flags | Flags::GAMUT_CHECK;
            // (Little CMS's channel count.)
            let mut alarm = [0u16; 16];
            alarm[..3].copy_from_slice(&[0x8000, 0x8000, 0x8000]);
            // (Global: proofing transforms take Little CMS's own context.)
            #[allow(deprecated)]
            Transform::<[u8; 3], [u8; 3]>::set_global_alarm_codes(alarm);
        }
        let transform = Transform::new_proofing(
            &document.lcms(),
            PixelFormat::RGB_8,
            &monitor.lcms(),
            PixelFormat::RGB_8,
            &proof.lcms().ok()?,
            intent.lcms(),
            Intent::RelativeColorimetric,
            flags,
        )
        .ok()?;
        Some(Self {
            transform: std::sync::Mutex::new(transform),
        })
    }

    /// Premultiplied RGBA8 pixels (as uploaded) in place.
    pub fn apply(&self, rgba: &mut [u8]) {
        let mut rgb: Vec<[u8; 3]> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                crate::canvas::blend::unmultiply(Color32::from_rgba_premultiplied(
                    p[0], p[1], p[2], p[3],
                ))
            })
            .map(|[r, g, b, _]| [r, g, b])
            .collect();
        let src = rgb.clone();
        (self.transform.lock().unwrap_or_else(|e| e.into_inner())).transform_pixels(&src, &mut rgb);
        for (p, [r, g, b]) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(rgb) {
            if p[3] == 0 {
                continue;
            }
            let c = Color32::from_rgba_unmultiplied(r, g, b, p[3]);
            p.copy_from_slice(&c.to_array());
        }
    }
}

/// A print (CMYK) profile, for proofing and CMYK export.
#[derive(Clone, Debug, PartialEq)]
pub struct CmykProfile {
    pub name: String,
    pub data: Arc<Vec<u8>>,
}

impl CmykProfile {
    pub fn from_icc(data: Vec<u8>, name: &str) -> Result<Self, String> {
        let profile = Profile::new_icc(&data).map_err(|_| "Not an ICC profile".to_string())?;
        if profile.color_space() != lcms2::ColorSpaceSignature::CmykData {
            return Err("Not a CMYK profile".into());
        }
        let description = profile
            .info(lcms2::InfoType::Description, lcms2::Locale::none())
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| name.to_string());
        Ok(Self {
            name: description,
            data: Arc::new(data),
        })
    }

    pub fn lcms(&self) -> Result<Profile, String> {
        Profile::new_icc(&self.data).map_err(|_| "Damaged CMYK profile".to_string())
    }

    /// A CMYK profile found on this system (Ghostscript's, colord's...),
    /// for when none was picked.
    pub fn system_default() -> Option<Self> {
        let dirs = [
            "/usr/share/color/icc",
            "/usr/share/ghostscript/iccprofiles",
            "/usr/local/share/color/icc",
            "/Library/ColorSync/Profiles",
            "C:\\Windows\\System32\\spool\\drivers\\color",
        ];
        let mut found: Vec<std::path::PathBuf> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let is_icc = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    e.eq_ignore_ascii_case("icc") || e.eq_ignore_ascii_case("icm")
                });
                if is_icc {
                    found.push(path);
                }
            }
        }
        // Coated press profiles first, then anything CMYK.
        found.sort_by_key(|p| {
            let n = p.to_string_lossy().to_ascii_lowercase();
            !(n.contains("coated")
                || n.contains("fogra")
                || n.contains("swop")
                || n.contains("cmyk"))
        });
        found.into_iter().find_map(|path| {
            let data = std::fs::read(&path).ok()?;
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Self::from_icc(data, &name).ok()
        })
    }
}

/// Unmultiplied RGB pixels (8-bit) from `from` to CMYK on `to`, for export.
/// Transparent areas are left as paper (no ink).
pub fn to_cmyk(
    pixels: &[Color32],
    from: &ColorProfile,
    to: &CmykProfile,
    intent: RenderingIntent,
) -> Result<Vec<[u8; 4]>, String> {
    let transform = Transform::<[u8; 3], [u8; 4], GlobalContext, DisallowCache>::new_flags_context(
        GlobalContext::new(),
        &from.lcms(),
        PixelFormat::RGB_8,
        &to.lcms()?,
        PixelFormat::CMYK_8,
        intent.lcms(),
        Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION,
    )
    .map_err(|e| format!("Can't convert to this CMYK profile: {e}"))?;
    // Composited over white paper first: CMYK has no transparency.
    let rgb: Vec<[u8; 3]> = pixels
        .iter()
        .map(|p| {
            let white = 255 - p.a();
            [
                p.r().saturating_add(white),
                p.g().saturating_add(white),
                p.b().saturating_add(white),
            ]
        })
        .collect();
    let mut out = vec![[0u8; 4]; rgb.len()];
    use rayon::prelude::*;
    out.par_chunks_mut(4096)
        .zip(rgb.par_chunks(4096))
        .for_each(|(o, i)| transform.transform_pixels(i, o));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_profiles_round_trip_through_their_icc_bytes_and_keys() {
        for p in ColorProfile::BUILT_IN {
            let bytes = p.icc();
            assert!(bytes.len() > 100, "{}", p.label());
            let back = ColorProfile::from_icc(bytes, "x").unwrap();
            match &p {
                ColorProfile::Srgb => assert_eq!(back, ColorProfile::Srgb),
                _ => {
                    assert_eq!(back.label(), p.label());
                    assert!(same_colours(&back.lcms(), &p.lcms()));
                }
            }
            assert_eq!(ColorProfile::from_key(p.key().unwrap()), Some(p.clone()));
        }
    }

    #[test]
    fn wider_gamuts_hold_srgb_colours_in_smaller_numbers() {
        let t = RgbTransform::new(
            &ColorProfile::Srgb,
            &ColorProfile::DisplayP3,
            RenderingIntent::RelativeColorimetric,
        )
        .unwrap();
        let mut px = [[1.0, 0.0, 0.0], [0.5, 0.5, 0.5]];
        t.convert(&mut px);
        // sRGB red is inside P3: less than full P3 red, with some green.
        assert!(px[0][0] < 0.95 && px[0][1] > 0.1, "{:?}", px[0]);
        // Greys stay grey.
        assert!((px[1][0] - 0.5).abs() < 0.01 && (px[1][1] - px[1][2]).abs() < 0.005);
        // And back.
        let back = RgbTransform::new(
            &ColorProfile::DisplayP3,
            &ColorProfile::Srgb,
            RenderingIntent::RelativeColorimetric,
        )
        .unwrap();
        back.convert(&mut px);
        assert!(
            (px[0][0] - 1.0).abs() < 0.01 && px[0][1].abs() < 0.01,
            "{:?}",
            px[0]
        );
    }

    #[test]
    fn the_display_transform_is_none_for_the_same_profile_and_maps_premultiplied_pixels() {
        assert!(DisplayTransform::new(&ColorProfile::Srgb, &ColorProfile::Srgb).is_none());
        let t = DisplayTransform::new(&ColorProfile::DisplayP3, &ColorProfile::Srgb).unwrap();
        let mut px = [0, 255, 0, 255, 0, 0, 0, 0, 0, 120, 0, 128];
        t.apply(&mut px);
        // P3 green is past sRGB's: clipped to sRGB green.
        assert_eq!(&px[..4], &[0, 255, 0, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
        assert_eq!(px[11], 128, "alpha kept");
    }

    #[test]
    fn rgb_profiles_are_refused_as_cmyk_and_back() {
        let srgb = ColorProfile::Srgb.icc();
        assert!(CmykProfile::from_icc(srgb, "x").is_err());
        assert!(ColorProfile::from_icc(b"nonsense".to_vec(), "x").is_err());
    }

    #[test]
    fn cmyk_conversion_with_a_system_profile() {
        let Some(cmyk) = CmykProfile::system_default() else {
            eprintln!("no CMYK profile on this system; skipping");
            return;
        };
        let pixels = [
            Color32::WHITE,
            Color32::BLACK,
            Color32::from_rgb(0, 255, 255),
            Color32::TRANSPARENT,
        ];
        let out = to_cmyk(
            &pixels,
            &ColorProfile::Srgb,
            &cmyk,
            RenderingIntent::Perceptual,
        )
        .unwrap();
        assert!(
            out[0].iter().all(|&v| v < 10),
            "white is paper: {:?}",
            out[0]
        );
        assert!(out[1][3] > 150, "black uses black ink: {:?}", out[1]);
        let c = out[2];
        assert!(
            c[0] > 100 && c[1] < 20 && c[2] < c[0] / 2 && c[3] < 20,
            "cyan is mostly cyan ink: {c:?}"
        );
        assert_eq!(out[3], out[0], "transparent is paper");
        // Proofing on that profile shows something.
        let proof = DisplayTransform::proofing(
            &ColorProfile::Srgb,
            &ColorProfile::Srgb,
            &cmyk,
            RenderingIntent::Perceptual,
            false,
        );
        assert!(proof.is_some());
    }
}
