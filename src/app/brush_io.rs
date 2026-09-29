//! Loading brush tips: the grey images in the brushes folder become
//! textured tips for the brush list.

use crate::app::PainterApp;
use crate::app::state::LoadedTip;
use crate::brush_engine::brush::BrushPreset;
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::brush_engine::preset_file;
use crate::brush_engine::tip::TipMask;
use eframe::egui::{self, Color32, TextureOptions};

const MAX_BRUSH_TIP_PIXELS: u32 = 4_194_304;
/// Most tips taken from one folder.
const MAX_TIPS_PER_SET: usize = 64;

impl PainterApp {
    pub fn load_brush_tips(&mut self, ctx: egui::Context) {
        self.ensure_brushes_directory_exists();
        self.brush_state.loaded_brush_tips.clear();
        self.scan_and_load_brush_images(ctx.clone());
        self.sort_loaded_brushes();
        // The built-in tips and sets first, then the folder's.
        let mut builtin: Vec<LoadedTip> = crate::brush_engine::tip::builtin()
            .iter()
            .map(|(name, tip)| LoadedTip {
                name: name.to_string(),
                shape: PixelBrushShape::Custom(tip.clone()),
                extra: Vec::new(),
                texture: Some(Self::create_brush_texture(tip, &ctx)),
            })
            .collect();
        for (name, mut tips) in crate::brush_engine::tip::builtin_sets() {
            if tips.is_empty() {
                continue;
            }
            let first = tips.remove(0);
            builtin.push(LoadedTip {
                name: name.to_string(),
                texture: Some(Self::create_brush_texture(&first, &ctx)),
                shape: PixelBrushShape::Custom(first),
                extra: tips,
            });
        }
        self.brush_state.loaded_brush_tips.splice(0..0, builtin);
        self.load_textures();
    }

    /// Pictures in `brushes/textures/` become paper textures.
    fn load_textures(&mut self) {
        let dir = self.brush_state.brushes_path.join("textures");
        let mut textures = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || !Self::is_valid_image_extension(&path) {
                    continue;
                }
                match image::open(&path) {
                    Ok(img) => {
                        let name = path
                            .file_stem()
                            .map_or_else(|| "Texture".into(), |s| s.to_string_lossy().into_owned());
                        textures.push(crate::brush_engine::texture::Pattern::from_image(
                            &name, &img,
                        ));
                    }
                    Err(err) => log::warn!("Skipping texture {}: {err}", path.display()),
                }
            }
        }
        textures.sort_by(|a, b| a.name.cmp(&b.name));
        self.brush_state.loaded_textures = textures;
    }

    fn ensure_brushes_directory_exists(&self) {
        if !self.brush_state.brushes_path.exists() {
            let _ = std::fs::create_dir_all(&self.brush_state.brushes_path);
        }
    }

    /// Pictures in the brushes folder become tips; a folder of pictures
    /// (other than `textures` and `presets`) becomes a set of tips the dabs
    /// take in turn, in file-name order.
    fn scan_and_load_brush_images(&mut self, ctx: egui::Context) {
        let Ok(entries) = std::fs::read_dir(&self.brush_state.brushes_path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skip = path
                    .file_name()
                    .is_some_and(|n| n == "textures" || n == "presets");
                if !skip && let Some(set) = Self::try_load_tip_set(&path, &ctx) {
                    self.brush_state.loaded_brush_tips.push(set);
                }
            } else if let Some(tip) = Self::try_load_tip(&path) {
                let name = path.file_stem().map_or_else(
                    || format!("{}×{}", tip.width, tip.height),
                    |s| s.to_string_lossy().into_owned(),
                );
                self.brush_state.loaded_brush_tips.push(LoadedTip {
                    name,
                    texture: Some(Self::create_brush_texture(&tip, &ctx)),
                    shape: PixelBrushShape::Custom(tip),
                    extra: Vec::new(),
                });
            }
        }
    }

    fn try_load_tip_set(dir: &std::path::Path, ctx: &egui::Context) -> Option<LoadedTip> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();
        let mut tips: Vec<_> = paths
            .iter()
            .filter_map(|p| Self::try_load_tip(p))
            .take(MAX_TIPS_PER_SET)
            .collect();
        if tips.is_empty() {
            return None;
        }
        let first = tips.remove(0);
        Some(LoadedTip {
            name: dir
                .file_name()
                .map_or_else(|| "Tips".into(), |s| s.to_string_lossy().into_owned()),
            texture: Some(Self::create_brush_texture(&first, ctx)),
            shape: PixelBrushShape::Custom(first),
            extra: tips,
        })
    }

    fn try_load_tip(path: &std::path::Path) -> Option<std::sync::Arc<TipMask>> {
        if !path.is_file() || !Self::is_valid_image_extension(path) {
            return None;
        }
        let reader = image::ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?;
        let (width, height) = reader.into_dimensions().ok()?;
        if width == 0 || height == 0 || width.checked_mul(height)? > MAX_BRUSH_TIP_PIXELS {
            log::warn!("Skipping oversized brush tip: {}", path.display());
            return None;
        }
        let img = image::open(path).ok()?;
        Some(TipMask::from_image(&img))
    }

    /// Make the selected part of the picture (everything visible) a brush
    /// tip: dark paints, light doesn't, as in Photoshop's Define Brush. It's
    /// saved in the brushes folder, joins the tip list, and the brush uses
    /// it at once. Returns the tip's file name.
    pub(crate) fn define_tip_from_selection(
        &mut self,
        ctx: &egui::Context,
    ) -> Result<String, String> {
        self.release_canvas();
        let sel = &self.selection_manager;
        let bounds = sel
            .get_bounds()
            .filter(|_| sel.has_selection())
            .ok_or("Select the part of the picture to make a tip from")?;
        let [x0, y0, x1, y1] = self.pixel_bounds(bounds);
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        if w < 2 || h < 2 || w * h > MAX_BRUSH_TIP_PIXELS as usize {
            return Err("The selection is too small or too big for a tip".into());
        }
        let pixels = self.canvas.render_reference(None, x0, y0, w, h);
        let coverage = crate::selection::SelectionMask::rasterize([x0, y0, x1, y1], |y, x, out| {
            sel.row_coverage(y, x, out)
        });
        // On white, the unselected part white too: only what's drawn paints.
        let mut rgb = image::RgbImage::new(w as u32, h as u32);
        for (i, p) in rgb.pixels_mut().enumerate() {
            let c = pixels[i];
            let cov = coverage.data[i] as u32;
            let [r, g, b, a] = crate::canvas::blend::unmultiply(c).map(|v| v as u32);
            let on_white = |v: u32| (v * a + 255 * (255 - a)) / 255;
            let keep = |v: u32| ((on_white(v) * cov + 255 * (255 - cov)) / 255) as u8;
            *p = image::Rgb([keep(r), keep(g), keep(b)]);
        }
        let img = image::DynamicImage::ImageRgb8(rgb);
        let tip = TipMask::from_image(&img);
        self.ensure_brushes_directory_exists();
        let taken: Vec<String> = self
            .brush_state
            .loaded_brush_tips
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let name = (1..)
            .map(|n| format!("Custom tip {n}"))
            .find(|n| !taken.contains(n))
            .unwrap_or_default();
        let path = self.brush_state.brushes_path.join(format!("{name}.png"));
        img.save(&path)
            .map_err(|e| format!("Couldn't save the tip: {e}"))?;
        self.brush_state.loaded_brush_tips.push(LoadedTip {
            name: name.clone(),
            texture: Some(Self::create_brush_texture(&tip, ctx)),
            shape: PixelBrushShape::Custom(tip.clone()),
            extra: Vec::new(),
        });
        let b = &mut self.brush_state.brush;
        b.brush_options.pixel_shape = PixelBrushShape::Custom(tip);
        b.brush_options.extra_tips.clear();
        b.is_changed = true;
        self.brush_state.brush_preview.dirty = true;
        Ok(name)
    }

    fn is_valid_image_extension(path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .map(|ext| ["png", "jpg", "jpeg", "bmp"].contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
    }

    fn create_brush_texture(tip: &TipMask, ctx: &egui::Context) -> egui::TextureHandle {
        // A colour tip shows its colours; a grey one, white.
        let pixels: Vec<Color32> = match &tip.colors {
            Some(colors) => tip
                .pixels
                .iter()
                .zip(colors)
                .map(|(&a, c)| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a))
                .collect(),
            None => tip
                .pixels
                .iter()
                .map(|&alpha| Color32::from_white_alpha(alpha))
                .collect(),
        };
        let texture_img = egui::ColorImage {
            size: [tip.width, tip.height],
            pixels,
        };
        ctx.load_texture("brush_tip", texture_img, TextureOptions::LINEAR)
    }

    fn sort_loaded_brushes(&mut self) {
        self.brush_state
            .loaded_brush_tips
            .sort_by(|a, b| a.name.cmp(&b.name));
    }
}

/// Saving, sharing and importing brush presets (`.rpbrush` files). The
/// user's own presets live in `brushes/presets/`, one file each.
impl PainterApp {
    fn presets_dir(&self) -> std::path::PathBuf {
        self.brush_state.brushes_path.join("presets")
    }

    /// Add the presets saved in `brushes/presets/` after the built-in ones.
    pub(crate) fn load_user_presets(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.presets_dir()) else {
            return;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == preset_file::EXTENSION))
            .collect();
        paths.sort();
        for path in paths {
            let loaded = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| preset_file::decode(&bytes));
            match loaded {
                Ok(presets) => {
                    for mut preset in presets {
                        preset.name = self.unique_preset_name(&preset.name);
                        preset.file = Some(path.clone());
                        self.brush_state.presets.push(preset);
                    }
                }
                Err(err) => log::warn!("Skipping brush preset {}: {err}", path.display()),
            }
        }
    }

    /// `name`, or `name 2`, `name 3`… if a preset already has it.
    pub(crate) fn unique_preset_name(&self, name: &str) -> String {
        let taken = |n: &str| self.brush_state.presets.iter().any(|p| p.name == n);
        if !taken(name) {
            return name.to_string();
        }
        (2..)
            .map(|i| format!("{name} {i}"))
            .find(|n| !taken(n))
            .expect("some number is free")
    }

    /// Add `preset` to the list and keep it in the user's library.
    pub(crate) fn add_user_preset(&mut self, mut preset: BrushPreset) {
        preset.name = self.unique_preset_name(&preset.name);
        match self.write_library_file(&preset) {
            Ok(path) => preset.file = Some(path),
            Err(err) => {
                log::warn!("Couldn't save brush preset {}: {err}", preset.name);
                self.export_state.message = Some(format!("Couldn't save the preset: {err}"));
            }
        }
        self.brush_state.presets.push(preset);
    }

    fn write_library_file(&self, preset: &BrushPreset) -> Result<std::path::PathBuf, String> {
        let dir = self.presets_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stem = preset_file::file_stem(&preset.name);
        let path = std::iter::once(stem.clone())
            .chain((2..).map(|i| format!("{stem} {i}")))
            .map(|s| dir.join(format!("{s}.{}", preset_file::EXTENSION)))
            .find(|p| !p.exists())
            .expect("some name is free");
        let bytes = preset_file::encode(std::slice::from_ref(preset))?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Remove a preset the user saved or imported (built-in ones stay).
    pub(crate) fn delete_user_preset(&mut self, index: usize) {
        let Some(preset) = self.brush_state.presets.get(index) else {
            return;
        };
        let Some(path) = preset.file.clone() else {
            return;
        };
        if let Err(err) = std::fs::remove_file(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            self.export_state.message = Some(format!("Couldn't delete the preset: {err}"));
            return;
        }
        let preset = self.brush_state.presets.remove(index);
        self.brush_state.preset_previews.remove(&preset.name);
        self.edit_library(|lib| lib.forget(&preset.name));
        for active in [
            &mut self.brush_state.active_preset,
            &mut self.brush_state.stashed_preset,
        ] {
            if active.as_deref() == Some(preset.name.as_str()) {
                *active = None;
            }
        }
    }

    /// Import the brushes in file `name` (a `.rpbrush`, or another app's
    /// brush file) into the library; returns how many there were. Another
    /// app's brushes come with a report of what was approximated.
    pub(crate) fn import_brushes_bytes(
        &mut self,
        name: &str,
        bytes: &[u8],
    ) -> Result<usize, String> {
        let is_preset_file = std::path::Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(preset_file::EXTENSION));
        let (presets, notes) = if is_preset_file {
            (preset_file::decode(bytes)?, Vec::new())
        } else {
            let imported = crate::brush_engine::import::import(name, bytes)?;
            (imported.presets, imported.notes)
        };
        let count = presets.len();
        let app = crate::app::brush_library::source_app(name);
        for preset in presets {
            self.add_user_preset(preset);
            // Tagged so they can be found again: "Imported", and the app.
            let name = self.brush_state.presets.last().map(|p| p.name.clone());
            if let Some(name) = name {
                self.edit_library(|lib| {
                    lib.tags.remove(&name);
                    lib.add_tag(&name, "Imported");
                    if let Some(app) = app {
                        lib.add_tag(&name, app);
                    }
                });
            }
        }
        if !is_preset_file {
            let report = self
                .brush_state
                .import_report
                .get_or_insert_with(Default::default);
            report.push((name.to_string(), count, notes));
        }
        // Show them (and the report) in the presets window.
        self.brush_state.show_presets = true;
        Ok(count)
    }

    pub(crate) fn import_brushes_path(&mut self, path: &std::path::Path) -> Result<usize, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.import_brushes_bytes(&name, &bytes)
    }

    /// Whether `name` is a file of brushes this app imports.
    pub(crate) fn is_brush_file(name: &str) -> bool {
        std::path::Path::new(name).extension().is_some_and(|e| {
            let e = e.to_string_lossy().to_lowercase();
            e == preset_file::EXTENSION
                || crate::brush_engine::import::EXTENSIONS.contains(&e.as_str())
        })
    }
}

/// Write `presets` to `path` as one `.rpbrush` file.
#[cfg(not(target_os = "android"))]
pub(crate) fn export_presets(
    presets: &[BrushPreset],
    path: &std::path::Path,
) -> Result<(), String> {
    let bytes = preset_file::encode(presets)?;
    std::fs::write(path, bytes).map_err(|e| format!("Couldn't write {}: {e}", path.display()))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn export_presets_dialog(app: &mut PainterApp, indices: &[usize], name: &str) {
    let presets: Vec<BrushPreset> = indices
        .iter()
        .filter_map(|&i| app.brush_state.presets.get(i).cloned())
        .collect();
    if presets.is_empty() {
        return;
    }
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Brush presets", &[preset_file::EXTENSION])
        .set_file_name(format!(
            "{}.{}",
            preset_file::file_stem(name),
            preset_file::EXTENSION
        ))
        .save_file()
    else {
        return;
    };
    if let Err(err) = export_presets(&presets, &path) {
        log::error!("{err}");
        app.export_state.message = Some(err);
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) fn import_presets_dialog(app: &mut PainterApp) {
    let mut all: Vec<&str> = vec![preset_file::EXTENSION];
    all.extend(crate::brush_engine::import::EXTENSIONS);
    let Some(paths) = rfd::FileDialog::new()
        .add_filter("Brushes", &all)
        .add_filter("Rusty Painter presets", &[preset_file::EXTENSION])
        .add_filter("Photoshop", &["abr"])
        .add_filter("Krita", &["kpp", "bundle"])
        .add_filter("GIMP", &["gbr", "gih"])
        .add_filter("MyPaint", &["myb"])
        .pick_files()
    else {
        return;
    };
    for path in paths {
        if let Err(err) = app.import_brushes_path(&path) {
            log::error!("{err}");
            app.export_state.message = Some(err);
        }
    }
}

impl PainterApp {
    /// Where the swatches are kept: next to the brushes folder.
    fn swatches_path(&self) -> std::path::PathBuf {
        self.brush_state
            .brushes_path
            .with_file_name("swatches.json")
    }

    /// The swatches saved last time (the defaults if there are none).
    pub(crate) fn load_swatches(&mut self) {
        let Ok(bytes) = std::fs::read(self.swatches_path()) else {
            return;
        };
        match serde_json::from_slice::<Vec<String>>(&bytes) {
            Ok(hex) => {
                self.brush_state.swatches = hex.iter().filter_map(|h| parse_hex(h)).collect();
            }
            Err(err) => log::warn!("Ignoring swatches.json: {err}"),
        }
    }

    /// Keep the swatches for next time. Errors are logged: a read-only
    /// folder shouldn't get in the way of painting.
    pub(crate) fn save_swatches(&self) {
        let hex: Vec<String> = self
            .brush_state
            .swatches
            .iter()
            .map(|c| {
                let [r, g, b, a] = c.to_srgba_unmultiplied();
                format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
            })
            .collect();
        let result = serde_json::to_vec_pretty(&hex)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                std::fs::write(self.swatches_path(), bytes).map_err(|e| e.to_string())
            });
        if let Err(err) = result {
            log::warn!("Couldn't save swatches: {err}");
        }
    }
}

/// `#RRGGBB` or `#RRGGBBAA`.
fn parse_hex(hex: &str) -> Option<Color32> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 && h.len() != 8 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let a = if h.len() == 8 { byte(6)? } else { 255 };
    Some(Color32::from_rgba_unmultiplied(
        byte(0)?,
        byte(2)?,
        byte(4)?,
        a,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swatch_colours_read_back_as_written() {
        assert_eq!(parse_hex("#FF8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(
            parse_hex("#10203080"),
            Some(Color32::from_rgba_unmultiplied(16, 32, 48, 128))
        );
        assert_eq!(parse_hex("FF8000"), None);
        assert_eq!(parse_hex("#GG0000"), None);
    }

    #[test]
    fn a_selection_becomes_a_brush_tip_the_brush_uses() {
        use crate::canvas::Canvas;
        use crate::selection::{SelectionMode, SelectionShape};
        let dir = std::env::temp_dir().join(format!("rp-tip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        app.selection_manager.canvas_size = [128, 128];
        // A black dot in the middle of the selection.
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        for y in 20..40 {
            for x in 20..40 {
                tile[y * 64 + x] = Color32::BLACK;
            }
        }
        app.canvas_mut().set_layer_tile_data(1, 0, 0, tile);
        let ctx = egui::Context::default();
        assert!(
            app.define_tip_from_selection(&ctx).is_err(),
            "needs a selection"
        );
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: eframe::egui::Vec2::new(10.0, 10.0),
                end: eframe::egui::Vec2::new(50.0, 50.0),
            },
            SelectionMode::Replace,
        );
        let name = app.define_tip_from_selection(&ctx).unwrap();
        assert!(dir.join("brushes").join(format!("{name}.png")).exists());
        let PixelBrushShape::Custom(tip) = &app.brush_state.brush.brush_options.pixel_shape else {
            panic!("the brush uses the new tip");
        };
        let r = tip.width.max(tip.height) as f32 / 2.0;
        assert!(tip.sample(0.0, 0.0, r) > 0.9, "the dot paints");
        assert_eq!((tip.width, tip.height), (20, 20), "trimmed to what's drawn");
        let _ = std::fs::remove_dir_all(dir);
    }
}
