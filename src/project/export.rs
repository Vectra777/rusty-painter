//! Image export: encoding the flattened canvas as PNG, JPEG or TIFF and
//! writing it to disk.

use eframe::egui::ColorImage;
use image::ImageFormat;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Png,
    /// PNG with 16 bits per channel, composited at full precision (a
    /// 16-bit or float document keeps its depth).
    Png16,
    Jpeg,
    Tiff,
    /// TIFF with 16 bits per channel, like [`ExportFormat::Png16`].
    Tiff16,
    /// TIFF with 32-bit float channels in linear light, as float documents
    /// are kept.
    Tiff32F,
    /// TIFF in CMYK for print, through the print profile.
    TiffCmyk,
    /// Lossless WebP.
    WebP,
    /// Layered: written from the document, not the flattened picture.
    Psd,
    /// Layered vector and pictures (see [`crate::project::svg`]).
    Svg,
}

impl ExportFormat {
    /// Every format, in the order the export dialog lists them.
    pub const ALL: [ExportFormat; 10] = [
        ExportFormat::Png,
        ExportFormat::Png16,
        ExportFormat::Jpeg,
        ExportFormat::Tiff,
        ExportFormat::Tiff16,
        ExportFormat::Tiff32F,
        ExportFormat::TiffCmyk,
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
            ExportFormat::Tiff16 => "TIFF (16-bit)",
            ExportFormat::Tiff32F => "TIFF (32-bit float, linear)",
            ExportFormat::TiffCmyk => "TIFF (CMYK, for print)",
            ExportFormat::WebP => "WebP (lossless)",
            ExportFormat::Psd => "PSD (layers)",
            ExportFormat::Svg => "SVG (layers, vector lines)",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Png | ExportFormat::Png16 => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Tiff
            | ExportFormat::Tiff16
            | ExportFormat::Tiff32F
            | ExportFormat::TiffCmyk => "tiff",
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
            ExportFormat::Tiff
            | ExportFormat::Tiff16
            | ExportFormat::Tiff32F
            | ExportFormat::TiffCmyk => "image/tiff",
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

    /// Written from the picture composited at full precision (see
    /// [`LinearImage`]) rather than its 8-bit pixels.
    pub fn is_deep(&self) -> bool {
        matches!(
            self,
            ExportFormat::Png16 | ExportFormat::Tiff16 | ExportFormat::Tiff32F
        )
    }

    /// `None` for the layered formats (see [`save_psd`], [`save_svg`]).
    fn image_format(&self) -> Option<ImageFormat> {
        Some(match self {
            ExportFormat::Png | ExportFormat::Png16 => ImageFormat::Png,
            ExportFormat::Jpeg => ImageFormat::Jpeg,
            ExportFormat::Tiff
            | ExportFormat::Tiff16
            | ExportFormat::Tiff32F
            | ExportFormat::TiffCmyk => ImageFormat::Tiff,
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

/// A flattened picture at full precision: premultiplied linear light,
/// row-major (see `Canvas::flatten_final_linear`).
pub struct LinearImage {
    pub size: [usize; 2],
    pub pixels: Vec<[f32; 4]>,
}

/// `p` unmultiplied (still linear); fully transparent is all zeros.
#[inline]
fn unmultiplied_linear(p: [f32; 4]) -> [f32; 4] {
    let a = p[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return [0.0; 4];
    }
    [p[0] / a, p[1] / a, p[2] / a, a]
}

/// 16 bits per channel, unmultiplied, sRGB encoded.
fn linear_to_rgba16(
    img: &LinearImage,
) -> Result<image::ImageBuffer<image::Rgba<u16>, Vec<u16>>, String> {
    use eframe::egui::ecolor::gamma_from_linear;
    use rayon::prelude::*;
    let [width, height] = img.size;
    let to = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    let mut values = vec![0u16; img.pixels.len() * 4];
    values
        .par_chunks_mut(4 * 4096)
        .zip(img.pixels.par_chunks(4096))
        .for_each(|(out, px)| {
            for (o, &p) in out.as_chunks_mut::<4>().0.iter_mut().zip(px) {
                let [r, g, b, a] = unmultiplied_linear(p);
                *o = [
                    to(gamma_from_linear(r.clamp(0.0, 1.0))),
                    to(gamma_from_linear(g.clamp(0.0, 1.0))),
                    to(gamma_from_linear(b.clamp(0.0, 1.0))),
                    to(a),
                ];
            }
        });
    image::ImageBuffer::from_raw(width as u32, height as u32, values)
        .ok_or_else(|| "Failed to build 16-bit image".to_string())
}

/// 32-bit float channels, unmultiplied, linear light (values above 1 kept).
fn linear_to_rgba32f(img: &LinearImage) -> Result<image::Rgba32FImage, String> {
    let [width, height] = img.size;
    let values: Vec<f32> = img
        .pixels
        .iter()
        .flat_map(|&p| unmultiplied_linear(p))
        .collect();
    image::Rgba32FImage::from_raw(width as u32, height as u32, values)
        .ok_or_else(|| "Failed to build float image".to_string())
}

/// Encode a full-precision picture as one of the deep formats into `out`,
/// with `icc` (the document's profile) embedded if given.
fn encode_linear_into<W: std::io::Write + std::io::Seek>(
    img: &LinearImage,
    format: ExportFormat,
    out: &mut W,
    icc: Option<&[u8]>,
) -> Result<(), String> {
    if !format.is_deep() {
        return Err(format!(
            "{} isn't written at full precision",
            format.label()
        ));
    }
    let [w, h] = img.size;
    match format {
        ExportFormat::Tiff32F => write_tiff(
            out,
            w,
            h,
            TiffPixels::Rgba32F(linear_to_rgba32f(img)?.into_raw()),
            icc,
        ),
        ExportFormat::Tiff16 => write_tiff(
            out,
            w,
            h,
            TiffPixels::Rgba16(linear_to_rgba16(img)?.into_raw()),
            icc,
        ),
        _ => {
            let bytes: Vec<u8> = (linear_to_rgba16(img)?.into_raw().iter())
                .flat_map(|v| v.to_be_bytes())
                .collect();
            let mut png = image::codecs::png::PngEncoder::new(out);
            if let Some(icc) = icc {
                let _ = image::ImageEncoder::set_icc_profile(&mut png, icc.to_vec());
            }
            image::ImageEncoder::write_image(
                png,
                &bytes,
                w as u32,
                h as u32,
                image::ExtendedColorType::Rgba16,
            )
            .map_err(|e| e.to_string())
        }
    }
}

/// Save a full-precision picture as one of the deep formats.
pub fn save_linear_image(
    img: &LinearImage,
    path: impl Into<PathBuf>,
    format: ExportFormat,
    icc: Option<&[u8]>,
) -> Result<(), String> {
    let file = std::fs::File::create(path.into()).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    encode_linear_into(img, format, &mut out, icc)?;
    std::io::Write::flush(&mut out).map_err(|e| e.to_string())
}

/// Pixels for [`write_tiff`].
enum TiffPixels {
    Rgba8(Vec<u8>),
    Rgba16(Vec<u16>),
    Rgba32F(Vec<f32>),
    Cmyk8(Vec<u8>),
}

/// A TIFF (Deflate-compressed), with an ICC profile if given.
fn write_tiff<W: std::io::Write + std::io::Seek>(
    out: &mut W,
    w: usize,
    h: usize,
    pixels: TiffPixels,
    icc: Option<&[u8]>,
) -> Result<(), String> {
    use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
    use tiff::tags::Tag;
    let err = |e: tiff::TiffError| e.to_string();
    let mut tiff = TiffEncoder::new(out)
        .map_err(err)?
        .with_compression(Compression::Deflate(DeflateLevel::Balanced));
    let (w, h) = (w as u32, h as u32);
    macro_rules! write {
        ($ty:ty, $data:expr) => {{
            let mut image = tiff.new_image::<$ty>(w, h).map_err(err)?;
            if let Some(icc) = icc {
                image
                    .encoder()
                    .write_tag(Tag::IccProfile, icc)
                    .map_err(err)?;
            }
            image.write_data(&$data).map_err(err)
        }};
    }
    match pixels {
        TiffPixels::Rgba8(p) => write!(colortype::RGBA8, p),
        TiffPixels::Rgba16(p) => write!(colortype::RGBA16, p),
        TiffPixels::Rgba32F(p) => write!(colortype::RGBA32Float, p),
        TiffPixels::Cmyk8(p) => write!(colortype::CMYK8, p),
    }
}

/// The colour a picture is exported with: its profile (embedded), and for
/// CMYK the print profile and how colours are fitted into it.
#[derive(Clone, Default)]
pub struct ExportColor {
    pub profile: crate::canvas::color_profile::ColorProfile,
    pub cmyk: Option<crate::canvas::color_profile::CmykProfile>,
    pub intent: crate::canvas::color_profile::RenderingIntent,
}

/// Export `canvas`'s picture as `format` to `path` (the flattened formats;
/// PSD and SVG are written from the layers).
pub fn export_canvas(
    canvas: &crate::canvas::Canvas,
    path: impl Into<PathBuf>,
    format: ExportFormat,
    color: &ExportColor,
) -> Result<(), String> {
    // sRGB is what files without a profile mean: none is embedded for it.
    let icc = (color.profile != crate::canvas::color_profile::ColorProfile::Srgb)
        .then(|| color.profile.icc());
    let path = path.into();
    if format.is_deep() {
        let img = LinearImage {
            size: [canvas.width(), canvas.height()],
            pixels: canvas.flatten_final_linear(),
        };
        return save_linear_image(&img, path, format, icc.as_deref());
    }
    let img = canvas.flatten_final();
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    if format == ExportFormat::TiffCmyk {
        let cmyk = color
            .cmyk
            .as_ref()
            .ok_or("No print (CMYK) profile: pick one in View → Colour Management")?;
        let inks =
            crate::canvas::color_profile::to_cmyk(&img.pixels, &color.profile, cmyk, color.intent)?;
        let [w, h] = img.size;
        write_tiff(
            &mut out,
            w,
            h,
            TiffPixels::Cmyk8(inks.into_iter().flatten().collect()),
            Some(&cmyk.data),
        )?;
    } else {
        encode_into(img, format, &mut out, icc.as_deref())?;
    }
    std::io::Write::flush(&mut out).map_err(|e| e.to_string())
}

/// Encode a full-precision picture in memory.
#[cfg(test)]
pub fn encode_linear_image(img: &LinearImage, format: ExportFormat) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    encode_linear_into(img, format, &mut std::io::Cursor::new(&mut bytes), None)?;
    Ok(bytes)
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

/// Encode `img` as `format` into `out`, with `icc` (the document's profile)
/// embedded if given.
fn encode_into<W: std::io::Write + std::io::Seek>(
    img: ColorImage,
    format: ExportFormat,
    out: &mut W,
    icc: Option<&[u8]>,
) -> Result<(), String> {
    use image::ImageEncoder;
    let Some(image_format) = format.image_format() else {
        return Err("PSD and SVG are written from the layers, not a flattened picture".into());
    };
    let [w, h] = img.size;
    let (w32, h32) = (w as u32, h as u32);
    let with_icc = |encoder: &mut dyn FnMut(Vec<u8>)| {
        if let Some(icc) = icc {
            encoder(icc.to_vec());
        }
    };
    let result = match format {
        ExportFormat::Png => {
            let rgba = to_rgba_image(img)?;
            let mut e = image::codecs::png::PngEncoder::new(out);
            with_icc(&mut |icc| {
                let _ = e.set_icc_profile(icc);
            });
            e.write_image(rgba.as_raw(), w32, h32, image::ExtendedColorType::Rgba8)
        }
        ExportFormat::Jpeg => {
            let rgb = to_rgb_on_white(&img)?;
            let mut e = image::codecs::jpeg::JpegEncoder::new(out);
            with_icc(&mut |icc| {
                let _ = e.set_icc_profile(icc);
            });
            e.write_image(rgb.as_raw(), w32, h32, image::ExtendedColorType::Rgb8)
        }
        ExportFormat::WebP => {
            let rgba = to_rgba_image(img)?;
            let mut e = image::codecs::webp::WebPEncoder::new_lossless(out);
            with_icc(&mut |icc| {
                let _ = e.set_icc_profile(icc);
            });
            e.write_image(rgba.as_raw(), w32, h32, image::ExtendedColorType::Rgba8)
        }
        ExportFormat::Tiff => {
            return write_tiff(
                out,
                w,
                h,
                TiffPixels::Rgba8(to_rgba_image(img)?.into_raw()),
                icc,
            );
        }
        ExportFormat::Png16 => {
            let bytes: Vec<u8> = (to_rgba16_image(&img)?.into_raw().iter())
                .flat_map(|v| v.to_be_bytes())
                .collect();
            let mut e = image::codecs::png::PngEncoder::new(out);
            with_icc(&mut |icc| {
                let _ = e.set_icc_profile(icc);
            });
            e.write_image(&bytes, w32, h32, image::ExtendedColorType::Rgba16)
        }
        ExportFormat::Tiff16 => {
            return write_tiff(
                out,
                w,
                h,
                TiffPixels::Rgba16(to_rgba16_image(&img)?.into_raw()),
                icc,
            );
        }
        ExportFormat::Tiff32F => {
            let linear = LinearImage {
                size: img.size,
                pixels: img
                    .pixels
                    .iter()
                    .map(|&c| eframe::egui::Rgba::from(c).to_array())
                    .collect(),
            };
            let values = linear_to_rgba32f(&linear)?.into_raw();
            return write_tiff(out, w, h, TiffPixels::Rgba32F(values), icc);
        }
        ExportFormat::TiffCmyk => {
            return Err("CMYK is written with a print profile (see `export_canvas`)".into());
        }
        ExportFormat::Psd | ExportFormat::Svg => unreachable!("no image format: returned above"),
    };
    let _ = image_format;
    result.map_err(|e| e.to_string())
}

/// Save a precomputed color image to disk.
#[cfg(test)]
pub fn save_color_image(
    img: ColorImage,
    path: impl Into<PathBuf>,
    format: ExportFormat,
) -> Result<(), String> {
    let file = std::fs::File::create(path.into()).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    encode_into(img, format, &mut out, None)?;
    std::io::Write::flush(&mut out).map_err(|e| e.to_string())
}

/// Encode a color image in memory.
pub fn encode_color_image(img: ColorImage, format: ExportFormat) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    encode_into(img, format, &mut std::io::Cursor::new(&mut bytes), None)?;
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
    fn deep_formats_keep_what_8_bits_lose() {
        // A dark ramp: few 8-bit steps, many deep ones.
        let n = 512;
        let img = LinearImage {
            size: [n, 1],
            pixels: (0..n)
                .map(|i| {
                    let v = i as f32 / n as f32 * 0.01;
                    [v, v, v, 1.0]
                })
                .collect(),
        };
        let distinct = |mut v: Vec<u32>| {
            v.dedup();
            v.len()
        };
        for (f, format) in [
            (ExportFormat::Png16, ImageFormat::Png),
            (ExportFormat::Tiff16, ImageFormat::Tiff),
        ] {
            let bytes = encode_linear_image(&img, f).unwrap();
            let back = image::load_from_memory_with_format(&bytes, format)
                .unwrap()
                .to_rgba16();
            let steps = distinct(back.pixels().map(|p| p.0[0] as u32).collect());
            assert!(steps > 400, "{}: {steps}", f.label());
            assert!(back.pixels().all(|p| p.0[3] == 65535));
        }
        let bytes = encode_linear_image(&img, ExportFormat::Tiff32F).unwrap();
        let back = image::load_from_memory_with_format(&bytes, ImageFormat::Tiff)
            .unwrap()
            .to_rgba32f();
        for (got, want) in back.pixels().zip(&img.pixels) {
            assert_eq!(got.0, *want);
        }
        assert!(encode_linear_image(&img, ExportFormat::Png).is_err());
    }

    #[test]
    fn half_transparent_deep_pixels_are_written_unmultiplied() {
        let img = LinearImage {
            size: [1, 1],
            pixels: vec![[0.25, 0.0, 0.5, 0.5]],
        };
        let bytes = encode_linear_image(&img, ExportFormat::Tiff32F).unwrap();
        let back = image::load_from_memory_with_format(&bytes, ImageFormat::Tiff)
            .unwrap()
            .to_rgba32f();
        assert_eq!(back.get_pixel(0, 0).0, [0.5, 0.0, 1.0, 0.5]);
    }

    /// Exports of a Display P3 document carry its profile (PNG, JPEG, WebP,
    /// TIFF at every depth); an sRGB one's carry none.
    #[test]
    fn exports_embed_the_documents_profile() {
        use crate::canvas::color_profile::{ColorProfile, RenderingIntent};
        let canvas = crate::canvas::Canvas::new(40, 30, Color32::WHITE, 64);
        let dir = std::env::temp_dir().join(format!("rp-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let read_icc = |path: &std::path::Path| -> Option<Vec<u8>> {
            use image::ImageDecoder;
            let bytes = std::fs::read(path).unwrap();
            let cursor = std::io::Cursor::new(bytes);
            match path.extension().unwrap().to_str().unwrap() {
                "png" => image::codecs::png::PngDecoder::new(cursor)
                    .unwrap()
                    .icc_profile()
                    .unwrap(),
                "jpg" => image::codecs::jpeg::JpegDecoder::new(cursor)
                    .unwrap()
                    .icc_profile()
                    .unwrap(),
                "webp" => image::codecs::webp::WebPDecoder::new(cursor)
                    .unwrap()
                    .icc_profile()
                    .unwrap(),
                _ => image::codecs::tiff::TiffDecoder::new(cursor)
                    .unwrap()
                    .icc_profile()
                    .unwrap(),
            }
        };
        for (profile, want) in [(ColorProfile::DisplayP3, true), (ColorProfile::Srgb, false)] {
            let color = ExportColor {
                profile: profile.clone(),
                cmyk: None,
                intent: RenderingIntent::Perceptual,
            };
            for f in [
                ExportFormat::Png,
                ExportFormat::Png16,
                ExportFormat::Jpeg,
                ExportFormat::WebP,
                ExportFormat::Tiff,
                ExportFormat::Tiff16,
                ExportFormat::Tiff32F,
            ] {
                let path = dir.join(format!("x-{}.{}", f.label().len(), f.extension()));
                export_canvas(&canvas, &path, f, &color).unwrap();
                let icc = read_icc(&path);
                assert_eq!(icc.is_some(), want, "{} {}", profile.label(), f.label());
                if let Some(icc) = icc {
                    let back = ColorProfile::from_icc(icc, "x").unwrap();
                    assert_eq!(back.label(), "Display P3", "{}", f.label());
                }
                // Still a picture of the canvas.
                let img = image::open(&path).unwrap();
                assert_eq!((img.width(), img.height()), (40, 30), "{}", f.label());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cmyk_tiff_holds_ink_and_its_profile() {
        use crate::canvas::color_profile::CmykProfile;
        let Some(cmyk) = CmykProfile::system_default() else {
            eprintln!("no CMYK profile on this system; skipping");
            return;
        };
        let canvas = crate::canvas::Canvas::new(8, 4, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::BLACK; 64 * 64]);
        let path = std::env::temp_dir().join(format!("rp-cmyk-{}.tiff", std::process::id()));
        let color = ExportColor {
            cmyk: Some(cmyk.clone()),
            ..Default::default()
        };
        export_canvas(&canvas, &path, ExportFormat::TiffCmyk, &color).unwrap();
        let mut decoder = tiff::decoder::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).unwrap(),
        ))
        .unwrap();
        assert_eq!(decoder.colortype().unwrap(), tiff::ColorType::CMYK(8));
        let icc = decoder.get_tag_u8_vec(tiff::tags::Tag::IccProfile).unwrap();
        assert_eq!(icc, *cmyk.data);
        let tiff::decoder::DecodingResult::U8(inks) = decoder.read_image().unwrap() else {
            panic!("8-bit inks");
        };
        assert!(inks[3] > 150, "black ink for black: {:?}", &inks[..4]);
        let _ = std::fs::remove_file(&path);
        // Without a print profile there's nothing to convert with.
        let none = ExportColor::default();
        assert!(export_canvas(&canvas, &path, ExportFormat::TiffCmyk, &none).is_err());
    }

    #[test]
    fn every_format_has_a_name_extension_and_type() {
        for f in ExportFormat::ALL {
            assert!(!f.label().is_empty() && !f.extension().is_empty());
            assert!(f.mime_type().contains('/'));
            assert_eq!(f.is_layered(), f.image_format().is_none(), "{}", f.label());
        }
    }

    /// A picture with smooth gradients, hard edges and alpha.
    fn photo(w: usize, h: usize) -> ColorImage {
        let mut img = ColorImage::new([w, h], Color32::TRANSPARENT);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let edge = if (x / 16 + y / 16) % 2 == 0 { 40 } else { 0 };
            *p = Color32::from_rgba_unmultiplied(
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                (100 + edge) as u8,
                if y < h / 4 { 140 } else { 255 },
            );
        }
        img
    }

    #[test]
    fn lossless_formats_give_back_exactly_the_picture() {
        let img = photo(97, 61);
        let want = to_rgba_image(img.clone()).unwrap();
        for (f, format) in [
            (ExportFormat::Png, ImageFormat::Png),
            (ExportFormat::Tiff, ImageFormat::Tiff),
            (ExportFormat::WebP, ImageFormat::WebP),
        ] {
            let bytes = encode_color_image(img.clone(), f).unwrap();
            let back = image::load_from_memory_with_format(&bytes, format)
                .unwrap()
                .to_rgba8();
            assert_eq!(back.as_raw(), want.as_raw(), "{}", f.label());
        }
    }

    #[test]
    fn jpeg_stays_close_to_the_picture_on_white() {
        let img = photo(128, 96);
        let want = to_rgb_on_white(&img).unwrap();
        let bytes = encode_color_image(img, ExportFormat::Jpeg).unwrap();
        let back = image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg)
            .unwrap()
            .to_rgb8();
        let mse = back
            .as_raw()
            .iter()
            .zip(want.as_raw())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / want.as_raw().len() as f64;
        let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10();
        assert!(psnr > 32.0, "PSNR {psnr:.1} dB");
    }
}
