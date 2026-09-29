//! Image export: encoding the flattened canvas as PNG, JPEG or TIFF and
//! writing it to disk.

use eframe::egui::ColorImage;
use image::ImageFormat;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Png,
    /// PNG with 16 bits per channel, for tools that want them (the
    /// picture's 8-bit values, spread over the full range).
    Png16,
    Jpeg,
    Tiff,
    /// Lossless WebP.
    WebP,
    /// Layered: written from the document, not the flattened picture.
    Psd,
    /// Layered vector and pictures (see [`crate::project::svg`]).
    Svg,
}

impl ExportFormat {
    /// Every format, in the order the export dialog lists them.
    pub const ALL: [ExportFormat; 7] = [
        ExportFormat::Png,
        ExportFormat::Png16,
        ExportFormat::Jpeg,
        ExportFormat::Tiff,
        ExportFormat::WebP,
        ExportFormat::Psd,
        ExportFormat::Svg,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG",
            ExportFormat::Png16 => "PNG (16-bit)",
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::WebP => "WebP (lossless)",
            ExportFormat::Psd => "PSD (layers)",
            ExportFormat::Svg => "SVG (layers, vector lines)",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Png | ExportFormat::Png16 => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Tiff => "tiff",
            ExportFormat::WebP => "webp",
            ExportFormat::Psd => "psd",
            ExportFormat::Svg => "svg",
        }
    }

    /// MIME type, e.g. for Android's MediaStore.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn mime_type(&self) -> &'static str {
        match self {
            ExportFormat::Png | ExportFormat::Png16 => "image/png",
            ExportFormat::Jpeg => "image/jpeg",
            ExportFormat::Tiff => "image/tiff",
            ExportFormat::WebP => "image/webp",
            ExportFormat::Psd => "image/vnd.adobe.photoshop",
            ExportFormat::Svg => "image/svg+xml",
        }
    }

    /// Written from the document's layers rather than the flattened
    /// picture.
    pub fn is_layered(&self) -> bool {
        matches!(self, ExportFormat::Psd | ExportFormat::Svg)
    }

    /// `None` for the layered formats (see [`save_psd`], [`save_svg`]).
    fn image_format(&self) -> Option<ImageFormat> {
        Some(match self {
            ExportFormat::Png | ExportFormat::Png16 => ImageFormat::Png,
            ExportFormat::Jpeg => ImageFormat::Jpeg,
            ExportFormat::Tiff => ImageFormat::Tiff,
            ExportFormat::WebP => ImageFormat::WebP,
            ExportFormat::Psd | ExportFormat::Svg => return None,
        })
    }
}

/// 16 bits per channel, unmultiplied (each 8-bit value times 257, so 255
/// is full).
fn to_rgba16_image(
    img: &ColorImage,
) -> Result<image::ImageBuffer<image::Rgba<u16>, Vec<u16>>, String> {
    use rayon::prelude::*;
    let (width, height) = (img.size[0], img.size[1]);
    let mut values = vec![0u16; width * height * 4];
    values
        .par_chunks_mut(4 * 4096)
        .zip(img.pixels.par_chunks(4096))
        .for_each(|(out, px)| {
            for (o, &p) in out.as_chunks_mut::<4>().0.iter_mut().zip(px) {
                let v = crate::canvas::blend::unmultiply(p);
                *o = v.map(|c| c as u16 * 257);
            }
        });
    image::ImageBuffer::from_raw(width as u32, height as u32, values)
        .ok_or_else(|| "Failed to build 16-bit image".to_string())
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
        return Err("PSD and SVG are written from the layers, not a flattened picture".into());
    };
    let result = match format {
        ExportFormat::Jpeg => to_rgb_on_white(&img)?.write_to(out, image_format),
        ExportFormat::Png16 => to_rgba16_image(&img)?.write_to(out, image_format),
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

/// Write SVG text (built from the document on the UI thread).
pub fn save_svg(svg: &str, path: impl Into<PathBuf>) -> Result<(), String> {
    std::fs::write(path.into(), svg).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    fn picture() -> ColorImage {
        let mut img = ColorImage::new([3, 2], Color32::TRANSPARENT);
        img.pixels[0] = Color32::from_rgb(255, 0, 0);
        img.pixels[1] = Color32::from_rgba_unmultiplied(0, 128, 255, 128);
        img.pixels[5] = Color32::WHITE;
        img
    }

    #[test]
    fn webp_is_lossless() {
        let img = picture();
        let bytes = encode_color_image(img.clone(), ExportFormat::WebP).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        let back = image::load_from_memory_with_format(&bytes, ImageFormat::WebP)
            .unwrap()
            .to_rgba8();
        let want = to_rgba_image(img).unwrap();
        assert_eq!(back.as_raw(), want.as_raw());
    }

    #[test]
    fn png16_holds_the_same_picture_in_16_bits() {
        let img = picture();
        let bytes = encode_color_image(img.clone(), ExportFormat::Png16).unwrap();
        let back = image::load_from_memory_with_format(&bytes, ImageFormat::Png).unwrap();
        assert!(
            matches!(back, image::DynamicImage::ImageRgba16(_)),
            "16-bit"
        );
        let wide = back.to_rgba16();
        let want = to_rgba_image(img).unwrap();
        for (a, b) in wide.as_raw().iter().zip(want.as_raw()) {
            assert_eq!(*a, *b as u16 * 257);
        }
    }

    #[test]
    fn every_format_has_a_name_extension_and_type() {
        for f in ExportFormat::ALL {
            assert!(!f.label().is_empty() && !f.extension().is_empty());
            assert!(f.mime_type().contains('/'));
            assert_eq!(f.is_layered(), f.image_format().is_none(), "{}", f.label());
        }
    }
}
