use crate::canvas::canvas::Canvas;
use eframe::egui::Color32;
use eframe::egui::ColorImage;
use image::ImageFormat;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    PNG,
    JPEG,
    TIFF,
}

impl ExportFormat {
    pub fn label(&self) -> &'static str {
        match self {
            ExportFormat::PNG => "PNG",
            ExportFormat::JPEG => "JPEG",
            ExportFormat::TIFF => "TIFF",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::PNG => "png",
            ExportFormat::JPEG => "jpg",
            ExportFormat::TIFF => "tiff",
        }
    }

    fn image_format(&self) -> ImageFormat {
        match self {
            ExportFormat::PNG => ImageFormat::Png,
            ExportFormat::JPEG => ImageFormat::Jpeg,
            ExportFormat::TIFF => ImageFormat::Tiff,
        }
    }
}

/// Export the flattened canvas (all visible layers composited) to an image file.
#[allow(dead_code)]
pub fn export_canvas(canvas: &Canvas, path: &Path, format: ExportFormat) -> Result<(), String> {
    let width = canvas.width();
    let height = canvas.height();
    let mut img = eframe::egui::ColorImage::new([width, height], Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, width, height, &mut img, 1);

    save_color_image(img, path, format)
}

/// Save a precomputed color image to disk.
pub fn save_color_image(
    img: ColorImage,
    path: impl Into<PathBuf>,
    format: ExportFormat,
) -> Result<(), String> {
    let path = path.into();
    let width = img.size[0];
    let height = img.size[1];

    // Convert egui ColorImage to raw RGBA bytes
    let mut bytes = Vec::with_capacity(width * height * 4);
    for px in &img.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        bytes.extend_from_slice(&[r, g, b, a]);
    }

    let rgba = image::RgbaImage::from_raw(width as u32, height as u32, bytes)
        .ok_or_else(|| "Failed to build RGBA image".to_string())?;

    rgba.save_with_format(path, format.image_format())
        .map_err(|e| e.to_string())
}
