use super::{PainterApp, painter_helpers::BrushData};
use crate::brush_engine::brush_options::PixelBrushShape;
use eframe::egui::{self, Color32, TextureOptions};

const MAX_BRUSH_TIP_PIXELS: u32 = 4_194_304;

impl PainterApp {
    pub fn load_brush_tips(&mut self, ctx: egui::Context) {
        self.ensure_brushes_directory_exists();
        self.brush_state.loaded_brush_tips.clear();
        self.scan_and_load_brush_images(ctx);
        self.sort_loaded_brushes();
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
        let img = image::open(&path).ok()?.to_luma8();
        let brush_data = Self::extract_brush_data(&img);
        let texture = Self::create_brush_texture(&brush_data, ctx);
        Some(brush_data.into_brush_tip(texture))
    }

    fn is_valid_image_extension(path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .map(|ext| ["png", "jpg", "jpeg", "bmp"].contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
    }

    fn extract_brush_data(img: &image::GrayImage) -> BrushData {
        BrushData {
            width: img.width() as usize,
            height: img.height() as usize,
            data: img.clone().into_raw(),
        }
    }

    fn create_brush_texture(brush_data: &BrushData, ctx: &egui::Context) -> egui::TextureHandle {
        let pixels: Vec<Color32> = brush_data
            .data
            .iter()
            .map(|&alpha| Color32::from_white_alpha(alpha))
            .collect();
        let texture_img = egui::ColorImage {
            size: [brush_data.width, brush_data.height],
            pixels,
        };
        ctx.load_texture(
            format!("brush_tip_{}", brush_data.width),
            texture_img,
            TextureOptions::NEAREST,
        )
    }

    fn sort_loaded_brushes(&mut self) {
        self.brush_state
            .loaded_brush_tips
            .sort_by(|a, b| a.0.cmp(&b.0));
    }
}
