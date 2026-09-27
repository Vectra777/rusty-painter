//! Loading brush tips: the grey images in the brushes folder become
//! textured tips for the brush list.

use crate::app::PainterApp;
use crate::brush_engine::brush_options::PixelBrushShape;
use eframe::egui::{self, Color32, TextureOptions};

const MAX_BRUSH_TIP_PIXELS: u32 = 4_194_304;

impl PainterApp {
    pub fn load_brush_tips(&mut self, ctx: egui::Context) {
        self.ensure_brushes_directory_exists();
        self.brush_state.loaded_brush_tips.clear();
        self.scan_and_load_brush_images(ctx.clone());
        self.sort_loaded_brushes();
        // The built-in tips first, then the folder's.
        let builtin: Vec<_> = crate::brush_engine::tip::builtin()
            .iter()
            .map(|(name, tip)| {
                let texture = Self::create_brush_texture(tip, &ctx);
                (
                    name.to_string(),
                    PixelBrushShape::Custom(tip.clone()),
                    Some(texture),
                )
            })
            .collect();
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

    fn scan_and_load_brush_images(&mut self, ctx: egui::Context) {
        if let Ok(entries) = std::fs::read_dir(&self.brush_state.brushes_path) {
            for entry in entries.flatten() {
                if let Some(brush_tip) = self.try_load_brush_from_path(entry.path(), &ctx) {
                    self.brush_state.loaded_brush_tips.push(brush_tip);
                }
            }
        }
    }

    fn try_load_brush_from_path(
        &self,
        path: std::path::PathBuf,
        ctx: &egui::Context,
    ) -> Option<(String, PixelBrushShape, Option<egui::TextureHandle>)> {
        if !path.is_file() || !Self::is_valid_image_extension(&path) {
            return None;
        }
        let reader = image::ImageReader::open(&path)
            .ok()?
            .with_guessed_format()
            .ok()?;
        let (width, height) = reader.into_dimensions().ok()?;
        if width == 0 || height == 0 || width.checked_mul(height)? > MAX_BRUSH_TIP_PIXELS {
            log::warn!("Skipping oversized brush tip: {}", path.display());
            return None;
        }
        let img = image::open(&path).ok()?;
        let tip = crate::brush_engine::tip::TipMask::from_image(&img);
        let texture = Self::create_brush_texture(&tip, ctx);
        let name = path.file_stem().map_or_else(
            || format!("{}×{}", tip.width, tip.height),
            |s| s.to_string_lossy().into_owned(),
        );
        Some((name, PixelBrushShape::Custom(tip), Some(texture)))
    }

    fn is_valid_image_extension(path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .map(|ext| ["png", "jpg", "jpeg", "bmp"].contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
    }

    fn create_brush_texture(
        tip: &crate::brush_engine::tip::TipMask,
        ctx: &egui::Context,
    ) -> egui::TextureHandle {
        let pixels: Vec<Color32> = tip
            .pixels
            .iter()
            .map(|&alpha| Color32::from_white_alpha(alpha))
            .collect();
        let texture_img = egui::ColorImage {
            size: [tip.width, tip.height],
            pixels,
        };
        ctx.load_texture("brush_tip", texture_img, TextureOptions::LINEAR)
    }

    fn sort_loaded_brushes(&mut self) {
        self.brush_state
            .loaded_brush_tips
            .sort_by(|a, b| a.0.cmp(&b.0));
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
}
