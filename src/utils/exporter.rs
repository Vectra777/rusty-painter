use eframe::egui::ColorImage;
use image::ImageFormat;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Jpeg,
    Tiff,
}

impl ExportFormat {
    pub fn label(&self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG",
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Tiff => "TIFF",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Tiff => "tiff",
        }
    }

    fn image_format(&self) -> ImageFormat {
        match self {
            ExportFormat::Png => ImageFormat::Png,
            ExportFormat::Jpeg => ImageFormat::Jpeg,
            ExportFormat::Tiff => ImageFormat::Tiff,
        }
    }
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
    let byte_len = width
        .checked_mul(height)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "Image is too large to export".to_string())?;

    // Convert egui ColorImage to raw RGBA bytes
    let mut bytes = Vec::with_capacity(byte_len);
    for px in &img.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        bytes.extend_from_slice(&[r, g, b, a]);
    }

    let rgba = image::RgbaImage::from_raw(width as u32, height as u32, bytes)
        .ok_or_else(|| "Failed to build RGBA image".to_string())?;

    rgba.save_with_format(path, format.image_format())
        .map_err(|e| e.to_string())
}
