//! Colour management in the app: the monitor's profile, proofing against a
//! print profile, keeping the canvas display converted to match, and the
//! Image menu's Assign / Convert Profile (one undo step each).

use crate::PainterApp;
use crate::canvas::color_profile::{
    CmykProfile, ColorProfile, DisplayTransform, RenderingIntent, RgbTransform,
};
use crate::canvas::history::{LayerHistoryOp, UndoAction};
use std::sync::Arc;

/// The monitor profile (a built-in one's key, or ICC bytes) and the print
/// profile, in [`profiles_dir`].
const MONITOR_FILE: &str = "monitor.icc";
const PRINT_FILE: &str = "print.icc";

fn profiles_dir() -> std::path::PathBuf {
    crate::app::init::data_dir().join("profiles")
}

/// Keep `bytes` as `file` in the profiles folder (`None` removes it).
fn save_profile_file(file: &str, bytes: Option<&[u8]>) {
    let path = profiles_dir().join(file);
    let result = match bytes {
        Some(bytes) => {
            std::fs::create_dir_all(profiles_dir()).and_then(|_| std::fs::write(&path, bytes))
        }
        None => std::fs::remove_file(&path).or(Ok(())),
    };
    if let Err(err) = result {
        log::warn!("Couldn't keep the colour profile {}: {err}", path.display());
    }
}

/// How the canvas is shown, and what CMYK means.
#[derive(Default)]
pub struct ColorSettings {
    /// The monitor's profile (sRGB unless one was picked).
    pub monitor: ColorProfile,
    /// The print profile proofing and CMYK export use (one found on the
    /// system if none was picked).
    pub cmyk: Option<CmykProfile>,
    /// Show the document as it would print on `cmyk`.
    pub proofing: bool,
    /// While proofing, show colours that can't print in grey.
    pub gamut_warning: bool,
    pub intent: RenderingIntent,
    /// What the display was last set up for (see [`DisplayKey`]).
    shown: Option<DisplayKey>,
}

/// Everything the display transform depends on.
#[derive(Clone, PartialEq)]
struct DisplayKey {
    document: ColorProfile,
    monitor: ColorProfile,
    proof: Option<(CmykProfile, RenderingIntent, bool)>,
    /// The render cache it was put in (a new canvas has a new one).
    generation: u64,
}

impl ColorSettings {
    /// The print profile to use: the one picked, else one on the system.
    pub fn cmyk_profile(&mut self) -> Option<CmykProfile> {
        if self.cmyk.is_none() {
            self.cmyk = CmykProfile::system_default();
        }
        self.cmyk.clone()
    }
}

impl PainterApp {
    /// Set the canvas display up for the document's profile, the monitor's
    /// and proofing, when any of them changed: every tile is drawn again.
    pub(crate) fn refresh_display_transform(&mut self) {
        let proof = if self.workspace.color.proofing {
            let gamut = self.workspace.color.gamut_warning;
            let intent = self.workspace.color.intent;
            self.workspace
                .color
                .cmyk_profile()
                .map(|p| (p, intent, gamut))
        } else {
            None
        };
        let key = DisplayKey {
            document: self.canvas.profile.clone(),
            monitor: self.workspace.color.monitor.clone(),
            proof,
            generation: self.render_cache.texture_generation,
        };
        if self.workspace.color.shown.as_ref() == Some(&key) {
            return;
        }
        let transform = match &key.proof {
            Some((cmyk, intent, gamut)) => {
                DisplayTransform::proofing(&key.document, &key.monitor, cmyk, *intent, *gamut)
            }
            None => DisplayTransform::new(&key.document, &key.monitor),
        };
        let had = self.render_cache.display.is_some();
        self.render_cache.display = transform.map(Arc::new);
        // Nothing to redraw going from shown-as-is to shown-as-is.
        if had || self.render_cache.display.is_some() {
            self.mark_all_tiles_dirty();
        }
        self.workspace.color.shown = Some(key);
    }

    /// Image → Assign Profile: the same numbers, read as `profile`'s
    /// colours.
    pub(crate) fn assign_profile(&mut self, profile: ColorProfile) {
        if self.canvas.profile == profile {
            return;
        }
        self.release_canvas();
        let before = crate::app::stroke_ops::exclusive(&mut self.canvas).assign_profile(profile);
        self.push_document_step(before);
    }

    /// Image → Convert to Profile: the same colours, as `profile`'s numbers.
    pub(crate) fn convert_profile(&mut self, profile: ColorProfile) {
        if self.canvas.profile == profile {
            return;
        }
        let intent = self.workspace.color.intent;
        let transform = match RgbTransform::new(&self.canvas.profile, &profile, intent) {
            Ok(t) => t,
            Err(err) => {
                self.export_state.message = Some(err);
                return;
            }
        };
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.gradient_commit();
        self.shape_commit();
        self.filter_cancel();
        self.release_canvas();
        let pool = Arc::clone(&self.workspace.pool);
        let canvas = crate::app::stroke_ops::exclusive(&mut self.canvas);
        let before = pool.install(|| canvas.convert_profile(profile, &transform));
        self.push_document_step(before);
    }

    /// A picked ICC profile's bytes, used as `target` says.
    pub(crate) fn use_profile(
        &mut self,
        target: crate::app::files::ProfileUse,
        bytes: Vec<u8>,
        name: &str,
    ) {
        use crate::app::files::ProfileUse;
        let result = match target {
            ProfileUse::Print => CmykProfile::from_icc(bytes, name).map(|p| {
                save_profile_file(PRINT_FILE, Some(&p.data));
                self.workspace.color.cmyk = Some(p);
            }),
            _ => ColorProfile::from_icc(bytes, name).map(|p| match target {
                ProfileUse::Assign => self.assign_profile(p),
                ProfileUse::Convert => self.convert_profile(p),
                _ => self.set_monitor_profile(p),
            }),
        };
        if let Err(err) = result {
            self.report(err);
        }
    }

    /// Show the canvas for `profile`'s monitor from now on (kept for the
    /// next start).
    pub(crate) fn set_monitor_profile(&mut self, profile: ColorProfile) {
        let saved = match &profile {
            ColorProfile::Srgb => None,
            ColorProfile::Icc { data, .. } => Some(data.as_slice().to_vec()),
            built_in => Some(built_in.key().unwrap_or_default().as_bytes().to_vec()),
        };
        save_profile_file(MONITOR_FILE, saved.as_deref());
        self.workspace.color.monitor = profile;
    }

    /// The monitor and print profiles picked before (at start-up).
    pub(crate) fn load_color_profiles(&mut self) {
        let dir = profiles_dir();
        if let Ok(bytes) = std::fs::read(dir.join(MONITOR_FILE)) {
            let built_in = std::str::from_utf8(&bytes)
                .ok()
                .and_then(ColorProfile::from_key);
            if let Some(p) = built_in.or_else(|| ColorProfile::from_icc(bytes, "Monitor").ok()) {
                self.workspace.color.monitor = p;
            }
        }
        if let Ok(bytes) = std::fs::read(dir.join(PRINT_FILE)) {
            self.workspace.color.cmyk = CmykProfile::from_icc(bytes, "Print").ok();
        }
    }

    /// One undo step swapping the whole document back to `before`.
    fn push_document_step(&mut self, before: crate::canvas::storage::DocumentState) {
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Document(Arc::new(std::sync::Mutex::new(
                before,
            )))),
        });
        self.after_document_swap();
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::color_profile::ColorProfile;
    use eframe::egui::Color32;

    fn app() -> crate::PainterApp {
        let canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::from_rgb(200, 30, 30); 64 * 64]);
        crate::project::tests::test_app_pub(canvas)
    }

    #[test]
    fn assigning_keeps_the_numbers_and_converting_keeps_the_colours() {
        let mut app = app();
        let before = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        app.assign_profile(ColorProfile::DisplayP3);
        assert_eq!(app.canvas.profile, ColorProfile::DisplayP3);
        assert_eq!(app.canvas.get_layer_tile_data(1, 0, 0).unwrap(), before);
        app.apply_history(false);
        assert_eq!(app.canvas.profile, ColorProfile::Srgb);

        // sRGB red is a less saturated red in P3's numbers.
        app.convert_profile(ColorProfile::DisplayP3);
        let p3 = app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[0];
        assert!(p3.r() < 200 && p3.g() > 30, "{p3:?}");
        // The background (white) stays white.
        assert_eq!(app.canvas.profile, ColorProfile::DisplayP3);
        app.apply_history(false);
        assert_eq!(app.canvas.get_layer_tile_data(1, 0, 0).unwrap(), before);
        app.apply_history(true);
        assert_eq!(app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[0], p3);
    }

    #[test]
    fn the_display_converts_only_when_the_profiles_differ() {
        let mut app = app();
        app.refresh_display_transform();
        assert!(app.render_cache.display.is_none());
        app.assign_profile(ColorProfile::AdobeRgb);
        app.refresh_display_transform();
        assert!(app.render_cache.display.is_some());
        assert!(app.render_cache.tiles.iter().all(|t| t.dirty));
        app.workspace.color.monitor = ColorProfile::AdobeRgb;
        app.refresh_display_transform();
        assert!(app.render_cache.display.is_none());
    }
}

#[cfg(test)]
mod import_tests {
    use crate::canvas::color_profile::ColorProfile;

    /// A PNG of one pixel, carrying `profile` if given.
    fn png(rgb: [u8; 3], profile: Option<&ColorProfile>) -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        let mut e = image::codecs::png::PngEncoder::new(&mut out);
        if let Some(p) = profile {
            e.set_icc_profile(p.icc()).unwrap();
        }
        e.write_image(
            &[rgb[0], rgb[1], rgb[2], 255],
            1,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        out
    }

    #[test]
    fn pictures_come_in_converted_to_the_documents_colours() {
        let decode = crate::app::import::decode_image_in;
        // No profile: sRGB, as it is in an sRGB document.
        let plain = decode(&png([200, 30, 30], None), &ColorProfile::Srgb).unwrap();
        assert_eq!(plain.get_pixel(0, 0).0, [200, 30, 30, 255]);
        // The same sRGB red in a P3 document: smaller numbers for red.
        let in_p3 = decode(&png([200, 30, 30], None), &ColorProfile::DisplayP3).unwrap();
        assert!(in_p3.get_pixel(0, 0).0[0] < 200);
        // A P3 picture in a P3 document: as it is.
        let p3 = png([200, 30, 30], Some(&ColorProfile::DisplayP3));
        assert_eq!(
            decode(&p3, &ColorProfile::DisplayP3)
                .unwrap()
                .get_pixel(0, 0)
                .0,
            [200, 30, 30, 255]
        );
        // And in sRGB: more saturated red than its numbers say.
        let srgb = decode(&p3, &ColorProfile::Srgb).unwrap().get_pixel(0, 0).0;
        assert!(srgb[0] > 200 && srgb[1] < 30, "{srgb:?}");
    }
}
