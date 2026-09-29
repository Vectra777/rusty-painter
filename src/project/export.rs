//! Image export: encoding the flattened canvas as PNG, JPEG or TIFF and
//! writing it to disk.

use eframe::egui::ColorImage;
use image::ImageFormat;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Png,
    Jpeg,
    Tiff,
    /// Layered: written from the document, not the flattened picture.
    Psd,
}

impl ExportFormat {
    pub fn label(&self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG",
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::Psd => "PSD (layers)",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Tiff => "tiff",
            ExportFormat::Psd => "psd",
        }
    }

    /// MIME type, e.g. for Android's MediaStore.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn mime_type(&self) -> &'static str {
        match self {
            ExportFormat::Png => "image/png",
            ExportFormat::Jpeg => "image/jpeg",
            ExportFormat::Tiff => "image/tiff",
            ExportFormat::Psd => "image/vnd.adobe.photoshop",
        }
    }

    /// `None` for PSD, which is written from the layers (see [`save_psd`]).
    fn image_format(&self) -> Option<ImageFormat> {
        Some(match self {
            ExportFormat::Png => ImageFormat::Png,
            ExportFormat::Jpeg => ImageFormat::Jpeg,
            ExportFormat::Tiff => ImageFormat::Tiff,
            ExportFormat::Psd => return None,
        })
    }
}

/// Convert an egui image into an `image` RGBA buffer.
pub(crate) fn to_rgba_image(img: ColorImage) -> Result<image::RgbaImage, String> {
    let width = img.size[0];
    let height = img.size[1];
    let byte_len = width
        .checked_mul(height)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "Image is too large to export".to_string())?;

    // Unpremultiply to raw RGBA bytes, in parallel.
    use rayon::prelude::*;
    let mut bytes = vec![0u8; byte_len];
    bytes
        .par_chunks_mut(4 * 4096)
        .zip(img.pixels.par_chunks(4096))
        .for_each(|(out, px)| {
            for (o, &p) in out.as_chunks_mut::<4>().0.iter_mut().zip(px) {
                o.copy_from_slice(&crate::canvas::blend::unmultiply(p));
            }
        });

    image::RgbaImage::from_raw(width as u32, height as u32, bytes)
        .ok_or_else(|| "Failed to build RGBA image".to_string())
}

/// JPEG has no transparency: flatten onto white. The pixels are
/// premultiplied, so "over white" is adding the uncovered part of white.
fn to_rgb_on_white(img: &ColorImage) -> Result<image::RgbImage, String> {
    use rayon::prelude::*;
    let (width, height) = (img.size[0], img.size[1]);
    let mut bytes = vec![0u8; width * height * 3];
    bytes
        .par_chunks_mut(3 * 4096)
        .zip(img.pixels.par_chunks(4096))
        .for_each(|(out, px)| {
            for (o, &p) in out.as_chunks_mut::<3>().0.iter_mut().zip(px) {
                let white = 255 - p.a();
                o.copy_from_slice(&[
                    p.r().saturating_add(white),
                    p.g().saturating_add(white),
                    p.b().saturating_add(white),
                ]);
            }
        });
    image::RgbImage::from_raw(width as u32, height as u32, bytes)
        .ok_or_else(|| "Failed to build RGB image".to_string())
}

/// Encode `img` as `format` into `out`.
fn encode_into<W: std::io::Write + std::io::Seek>(
    img: ColorImage,
    format: ExportFormat,
    out: &mut W,
) -> Result<(), String> {
    let Some(image_format) = format.image_format() else {
        return Err("PSD is written from the layers, not a flattened picture".into());
    };
    let result = match format {
        ExportFormat::Jpeg => to_rgb_on_white(&img)?.write_to(out, image_format),
        _ => to_rgba_image(img)?.write_to(out, image_format),
    };
    result.map_err(|e| e.to_string())
}

/// Save a precomputed color image to disk.
pub fn save_color_image(
    img: ColorImage,
    path: impl Into<PathBuf>,
    format: ExportFormat,
) -> Result<(), String> {
    let file = std::fs::File::create(path.into()).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    encode_into(img, format, &mut out)?;
    std::io::Write::flush(&mut out).map_err(|e| e.to_string())
}

/// Encode a color image in memory (for Android's MediaStore).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn encode_color_image(img: ColorImage, format: ExportFormat) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    encode_into(img, format, &mut std::io::Cursor::new(&mut bytes))?;
    Ok(bytes)
}

/// Write a layered PSD (built from the document on the UI thread, encoded
/// here, off it).
pub fn save_psd(
    doc: &crate::project::psd::PsdDocument,
    path: impl Into<PathBuf>,
) -> Result<(), String> {
    let bytes = crate::project::psd::encode_psd(doc)?;
    std::fs::write(path.into(), bytes).map_err(|e| e.to_string())
}
