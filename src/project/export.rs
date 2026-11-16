//! Image export: encoding the flattened canvas as PNG, JPEG or TIFF and
//! writing it to disk.

use eframe::egui::{Color32, ColorImage};
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
    #[cfg_attr(not(mobile), allow(dead_code))]
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
    /// JPEG's quality, 1–100 (`None`: [`DEFAULT_JPEG_QUALITY`]).
    pub jpeg_quality: Option<u8>,
}

/// JPEG's quality when none is chosen: little loss you'd see, a quarter of
/// a PNG's size or less.
pub const DEFAULT_JPEG_QUALITY: u8 = 90;

/// Export `canvas`'s picture as `format` to `path` (the flattened formats;
/// PSD and SVG are written from the layers).
#[cfg(test)]
pub fn export_canvas(
    canvas: &crate::canvas::Canvas,
    path: impl Into<PathBuf>,
    format: ExportFormat,
    color: &ExportColor,
) -> Result<(), String> {
    export_picture(canvas, path.into(), format, color, Reshape::NONE)
}

/// The picture of `canvas` as `format` to `path`, trimmed and scaled as
/// `reshape` says (the flattened formats; PSD and SVG are written from the
/// layers).
fn export_picture(
    canvas: &crate::canvas::Canvas,
    path: PathBuf,
    format: ExportFormat,
    color: &ExportColor,
    reshape: Reshape,
) -> Result<(), String> {
    // sRGB is what files without a profile mean: none is embedded for it.
    let icc = (color.profile != crate::canvas::color_profile::ColorProfile::Srgb)
        .then(|| color.profile.icc());
    if format.is_deep() {
        let img = LinearImage {
            size: [canvas.width(), canvas.height()],
            pixels: canvas.flatten_final_linear(),
        };
        let img = reshape.linear(img)?;
        return save_linear_image(&img, path, format, icc.as_deref());
    }
    let img = reshape.color(canvas.flatten_final())?;
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
        let quality = color.jpeg_quality.unwrap_or(DEFAULT_JPEG_QUALITY);
        encode_into(img, format, &mut out, icc.as_deref(), quality)?;
    }
    std::io::Write::flush(&mut out).map_err(|e| e.to_string())
}

/// How a picture is written: its format, size and edges. The export
/// dialog's choices, and what an export preset keeps.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// Size in % of the canvas's (see [`SCALE_RANGE`]).
    pub scale_percent: u32,
    /// Cut the transparent edges away.
    pub trim: bool,
    /// JPEG's quality, 1–100.
    pub jpeg_quality: u8,
}

/// The sizes an export can be, in % of the canvas's.
pub const SCALE_RANGE: std::ops::RangeInclusive<u32> = 1..=400;

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Png,
            scale_percent: 100,
            trim: false,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
        }
    }
}

impl ExportOptions {
    /// Read from settings: values in range.
    pub fn tidy(&mut self) {
        self.scale_percent = self
            .scale_percent
            .clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end());
        self.jpeg_quality = self.jpeg_quality.clamp(1, 100);
    }

    /// The size and trim apply (the layered formats are written as the
    /// document is).
    pub fn can_reshape(&self) -> bool {
        !self.format.is_layered()
    }

    /// A `w`×`h` canvas's size as exported (before any trim).
    pub fn size_of(&self, w: usize, h: usize) -> [usize; 2] {
        if self.can_reshape() {
            scaled([w, h], self.scale_percent)
        } else {
            [w, h]
        }
    }

    /// One line saying what it writes ("JPEG 90, 50%, trimmed").
    pub fn summary(&self) -> String {
        let mut s = self.format.label().to_string();
        if self.format == ExportFormat::Jpeg {
            s += &format!(" {}", self.jpeg_quality);
        }
        if self.can_reshape() {
            if self.scale_percent != 100 {
                s += &format!(", {}%", self.scale_percent);
            }
            if self.trim {
                s += ", trimmed";
            }
        }
        s
    }
}

/// Named export options, kept in the app's settings.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub options: ExportOptions,
}

/// Export `canvas` to `path` as `options` say, in any format.
pub fn export_with(
    canvas: &crate::canvas::Canvas,
    path: &std::path::Path,
    options: &ExportOptions,
    color: &ExportColor,
) -> Result<(), String> {
    match options.format {
        ExportFormat::Psd => save_psd(&crate::project::psd::PsdDocument::from_canvas(canvas), path),
        ExportFormat::Svg => {
            crate::project::svg::document_svg(canvas).and_then(|svg| save_svg(&svg, path))
        }
        format => {
            let color = ExportColor {
                jpeg_quality: Some(options.jpeg_quality),
                ..color.clone()
            };
            let reshape = Reshape {
                scale_percent: options.scale_percent,
                trim: options.trim,
            };
            export_picture(canvas, path.to_path_buf(), format, &color, reshape)
        }
    }
}

/// `size` at `percent`, each side at least a pixel.
fn scaled(size: [usize; 2], percent: u32) -> [usize; 2] {
    size.map(|v| ((v * percent as usize + 50) / 100).max(1))
}

/// Trimming and scaling a flattened picture.
#[derive(Clone, Copy)]
struct Reshape {
    scale_percent: u32,
    trim: bool,
}

impl Reshape {
    #[cfg(test)]
    const NONE: Self = Self {
        scale_percent: 100,
        trim: false,
    };

    /// Resampled with a tent filter: no weight is negative, so
    /// premultiplied pixels stay valid (no colour above its alpha, no dark
    /// or bright fringes ringing into edges as Lanczos gives). Lanczos with
    /// a clamp after it if exports look soft.
    const FILTER: image::imageops::FilterType = image::imageops::FilterType::Triangle;

    /// The part of a picture to keep: the box around every pixel `shows`
    /// (`x0, y0, x1, y1`, ends excluded), one transparent pixel when none
    /// does; all of it without trimming.
    fn keep(&self, size: [usize; 2], shows: impl Fn(usize) -> bool) -> [usize; 4] {
        let [w, h] = size;
        if !self.trim {
            return [0, 0, w, h];
        }
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if shows(y * w + x) {
                    (x0, y0) = (x0.min(x), y0.min(y));
                    (x1, y1) = (x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
        if x1 == 0 {
            [0, 0, 1, 1]
        } else {
            [x0, y0, x1, y1]
        }
    }

    /// The size a `cut` picture is written at, if a picture can be that big.
    fn size(&self, cut: [usize; 2]) -> Result<[usize; 2], String> {
        let size = scaled(cut, self.scale_percent);
        crate::app::document::validate_canvas_size(size[0], size[1])
            .map_err(|err| format!("At {}%: {err}", self.scale_percent))?;
        Ok(size)
    }

    /// `pixels` (`w` wide) cut to `b`.
    fn crop<T: Copy>(pixels: Vec<T>, w: usize, b: [usize; 4]) -> Vec<T> {
        if b == [0, 0, w, b[3]] && pixels.len() == w * b[3] {
            return pixels;
        }
        (b[1]..b[3])
            .flat_map(|y| pixels[y * w + b[0]..y * w + b[2]].iter().copied())
            .collect()
    }

    fn color(&self, img: ColorImage) -> Result<ColorImage, String> {
        let b = self.keep(img.size, |i| img.pixels[i].a() > 0);
        let cut = [b[2] - b[0], b[3] - b[1]];
        let size = self.size(cut)?;
        let pixels = Self::crop(img.pixels, img.size[0], b);
        if size == cut {
            return Ok(ColorImage { size, pixels });
        }
        let raw: Vec<u8> = bytemuck::cast_slice(&pixels).to_vec();
        let from = image::RgbaImage::from_raw(cut[0] as u32, cut[1] as u32, raw)
            .ok_or("Couldn't scale the picture")?;
        let to = image::imageops::resize(&from, size[0] as u32, size[1] as u32, Self::FILTER);
        let pixels = (to.as_raw().as_chunks::<4>().0.iter())
            .map(|&[r, g, b, a]| Color32::from_rgba_premultiplied(r.min(a), g.min(a), b.min(a), a))
            .collect();
        Ok(ColorImage { size, pixels })
    }

    fn linear(&self, img: LinearImage) -> Result<LinearImage, String> {
        let b = self.keep(img.size, |i| img.pixels[i][3] > 0.0);
        let cut = [b[2] - b[0], b[3] - b[1]];
        let size = self.size(cut)?;
        let pixels = Self::crop(img.pixels, img.size[0], b);
        if size == cut {
            return Ok(LinearImage { size, pixels });
        }
        let raw: Vec<f32> = pixels.into_iter().flatten().collect();
        let from = image::Rgba32FImage::from_raw(cut[0] as u32, cut[1] as u32, raw)
            .ok_or("Couldn't scale the picture")?;
        let to = image::imageops::resize(&from, size[0] as u32, size[1] as u32, Self::FILTER);
        let pixels = to.as_raw().as_chunks::<4>().0.to_vec();
        Ok(LinearImage { size, pixels })
    }
}

/// What a batch export writes, picked in the export dialog.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BatchChoice {
    /// The whole picture once with each export preset.
    pub presets: bool,
    /// Each layer comp, with the dialog's options.
    pub comps: bool,
    /// Each visible top-level layer or folder alone, with the dialog's
    /// options.
    pub layers: bool,
}

/// What one file of a batch shows.
#[derive(Clone, Debug)]
pub enum BatchView {
    Whole,
    Comp(crate::app::comps::LayerComp),
    Layer(crate::canvas::storage::LayerId),
}

/// One file of a batch export.
#[derive(Clone, Debug)]
pub struct BatchFile {
    /// Its file name, extension included.
    pub name: String,
    pub view: BatchView,
    pub options: ExportOptions,
}

/// The files a batch export of `canvas` writes: `<base>-<preset>`,
/// `<base>-<comp>` and `<base>-<layer>`, each name once.
pub fn batch_files(
    canvas: &crate::canvas::Canvas,
    base: &str,
    current: &ExportOptions,
    presets: &[ExportPreset],
    comps: &[crate::app::comps::LayerComp],
    choice: BatchChoice,
) -> Vec<BatchFile> {
    let mut taken = std::collections::HashSet::new();
    let base = file_name_part(base);
    let mut file = |part: &str, view: BatchView, options: &ExportOptions| {
        let stem = format!("{base}-{}", file_name_part(part));
        let ext = options.format.extension();
        let name = (1..)
            .map(|n| match n {
                1 => format!("{stem}.{ext}"),
                n => format!("{stem} {n}.{ext}"),
            })
            // Folders on Windows and macOS don't tell case apart.
            .find(|n| taken.insert(n.to_lowercase()))
            .unwrap_or_default();
        BatchFile {
            name,
            view,
            options: options.clone(),
        }
    };
    let mut files = Vec::new();
    if choice.presets {
        for p in presets {
            files.push(file(&p.name, BatchView::Whole, &p.options));
        }
    }
    if choice.comps {
        for c in comps {
            files.push(file(&c.name, BatchView::Comp(c.clone()), current));
        }
    }
    if choice.layers {
        for i in batch_layers(canvas) {
            let l = &canvas.layers[i];
            files.push(file(&l.name, BatchView::Layer(l.id), current));
        }
    }
    files
}

/// The layers exported one by one: the visible top-level layers and
/// folders (a clipped layer goes with the one it's clipped to, a mask with
/// its layer).
pub fn batch_layers(canvas: &crate::canvas::Canvas) -> Vec<usize> {
    top_level(canvas)
        .into_iter()
        .filter(|&i| canvas.layers[i].visible && !canvas.layers[i].clipped)
        .collect()
}

/// The top-level entries other than masks, bottom first.
fn top_level(canvas: &crate::canvas::Canvas) -> Vec<usize> {
    (0..canvas.layers.len())
        .filter(|&i| {
            let l = &canvas.layers[i];
            l.parent.is_none() && !matches!(l.kind, crate::canvas::storage::LayerKind::Mask { .. })
        })
        .collect()
}

/// `name` as part of a file name: no separators or characters some file
/// systems refuse.
fn file_name_part(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let clean = clean.trim().trim_matches('.').trim();
    if clean.is_empty() {
        "untitled".to_string()
    } else {
        clean.to_string()
    }
}

/// `canvas` as `view` shows it: a copy sharing its pixels, with the layers'
/// visibility (and a comp's opacities and blend modes) changed.
pub fn batch_canvas(canvas: &crate::canvas::Canvas, view: &BatchView) -> crate::canvas::Canvas {
    let mut copy = canvas.shared_copy();
    match view {
        BatchView::Whole => {}
        BatchView::Comp(comp) => {
            comp.apply_to(&mut copy.layers);
        }
        BatchView::Layer(id) => {
            let top = top_level(&copy);
            if let Some(at) = copy
                .layer_index_of(*id)
                .and_then(|i| top.iter().position(|&j| j == i))
            {
                // It and the layers clipped to it, right above.
                let clipped = top[at + 1..]
                    .iter()
                    .take_while(|&&j| copy.layers[j].clipped)
                    .count();
                for (k, &j) in top.iter().enumerate() {
                    if k < at || k > at + clipped {
                        copy.layers[j].visible = false;
                    }
                }
            }
        }
    }
    copy
}

/// Write `files` of `canvas` into `dir`, calling `written` after each
/// (an `Err` stops the batch); how many were written.
pub fn run_batch(
    canvas: &crate::canvas::Canvas,
    files: &[BatchFile],
    dir: &std::path::Path,
    color: &ExportColor,
    mut written: impl FnMut(usize, &BatchFile, &std::path::Path) -> Result<(), String>,
) -> Result<usize, String> {
    for (k, file) in files.iter().enumerate() {
        let path = dir.join(&file.name);
        export_with(
            &batch_canvas(canvas, &file.view),
            &path,
            &file.options,
            color,
        )
        .map_err(|err| format!("{}: {err}", file.name))?;
        written(k, file, &path)?;
    }
    Ok(files.len())
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
    jpeg_quality: u8,
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
            let mut e = fast_png(out);
            with_icc(&mut |icc| {
                let _ = e.set_icc_profile(icc);
            });
            e.write_image(rgba.as_raw(), w32, h32, image::ExtendedColorType::Rgba8)
        }
        ExportFormat::Jpeg => {
            let rgb = to_rgb_on_white(&img)?;
            let quality = jpeg_quality.clamp(1, 100);
            let mut e = image::codecs::jpeg::JpegEncoder::new_with_quality(out, quality);
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
            let mut e = fast_png(out);
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
    encode_into(img, format, &mut out, None, DEFAULT_JPEG_QUALITY)?;
    std::io::Write::flush(&mut out).map_err(|e| e.to_string())
}

/// Encode a color image in memory.
/// PNG as the project file writes its picture: fast deflate, every row
/// Paeth-filtered (a few % bigger than the default, several times faster;
/// see `preview::encode_png`).
fn fast_png<W: std::io::Write>(out: W) -> image::codecs::png::PngEncoder<W> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    PngEncoder::new_with_quality(out, CompressionType::Fast, FilterType::Paeth)
}

pub fn encode_color_image(img: ColorImage, format: ExportFormat) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut out = std::io::Cursor::new(&mut bytes);
    encode_into(img, format, &mut out, None, DEFAULT_JPEG_QUALITY)?;
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
                jpeg_quality: None,
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
    fn jpeg_quality_is_the_one_chosen() {
        let canvas = crate::canvas::Canvas::new(64, 64, Color32::WHITE, 64);
        let noise: Vec<Color32> = (0..64 * 64u32)
            .map(|i| {
                let v = (i.wrapping_mul(2_654_435_761) >> 24) as u8;
                Color32::from_rgb(v, v / 2, 255 - v)
            })
            .collect();
        canvas.set_layer_tile_data(1, 0, 0, noise);
        let dir = std::env::temp_dir();
        let size = |q: u8| {
            let path = dir.join(format!("rp-q{q}-{}.jpg", std::process::id()));
            let color = ExportColor {
                jpeg_quality: Some(q),
                ..Default::default()
            };
            export_canvas(&canvas, &path, ExportFormat::Jpeg, &color).unwrap();
            let len = std::fs::metadata(&path).unwrap().len();
            let _ = std::fs::remove_file(&path);
            len
        };
        assert!(size(20) < size(60) && size(60) < size(98));
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

    /// A 100×80 white canvas: "Ink" (layer 1) has a red block at x 10..30,
    /// y 20..30, "ink" (layer 2) a blue one at x 70..90, y 50..60.
    fn two_blocks() -> crate::canvas::Canvas {
        let mut canvas = crate::canvas::Canvas::new(100, 80, Color32::WHITE, 64);
        canvas.add_layer();
        canvas.layers[1].name = "Ink".into();
        canvas.layers[2].name = "ink".into();
        let block = |x0: i32, y0: i32, w: i32, h: i32, c: Color32| {
            move |tx: i32, ty: i32| -> Vec<Color32> {
                (0..64 * 64)
                    .map(|k| {
                        let (x, y) = (tx * 64 + k % 64, ty * 64 + k / 64);
                        let inside = (x0..x0 + w).contains(&x) && (y0..y0 + h).contains(&y);
                        if inside { c } else { Color32::TRANSPARENT }
                    })
                    .collect()
            }
        };
        let red = block(10, 20, 20, 10, Color32::RED);
        let blue = block(70, 50, 20, 10, Color32::BLUE);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            canvas.set_layer_tile_data(1, tx, ty, red(tx, ty));
            canvas.set_layer_tile_data(2, tx, ty, blue(tx, ty));
        }
        canvas
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scale_and_trim_shape_the_picture() {
        let mut canvas = two_blocks();
        canvas.layers[0].visible = false;
        canvas.layers[2].visible = false;
        let dir = temp_dir("reshape");
        let color = ExportColor::default();
        let export = |canvas: &crate::canvas::Canvas, format, scale_percent, trim| {
            let options = ExportOptions {
                format,
                scale_percent,
                trim,
                ..Default::default()
            };
            let path = dir.join(format!("x.{}", format.extension()));
            export_with(canvas, &path, &options, &color).unwrap();
            image::open(&path).unwrap()
        };
        let full = export(&canvas, ExportFormat::Png, 100, false).to_rgba8();
        assert_eq!(full.dimensions(), (100, 80));
        let half = export(&canvas, ExportFormat::Png, 50, false).to_rgba8();
        assert_eq!(half.dimensions(), (50, 40));
        // Scaled in place: the block's middle is red, a corner transparent.
        assert_eq!(half.get_pixel(10, 12).0, [255, 0, 0, 255]);
        assert_eq!(half.get_pixel(45, 35).0[3], 0);
        let trimmed = export(&canvas, ExportFormat::Png, 100, true).to_rgba8();
        assert_eq!(trimmed.dimensions(), (20, 10));
        assert!(trimmed.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        let both = export(&canvas, ExportFormat::Png, 50, true).to_rgba8();
        assert_eq!(both.dimensions(), (10, 5));
        assert!(both.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        let twice = export(&canvas, ExportFormat::Png, 200, true).to_rgba8();
        assert_eq!(twice.dimensions(), (40, 20));
        assert!(twice.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        // The deep formats too.
        let deep = export(&canvas, ExportFormat::Png16, 50, true).to_rgba16();
        assert_eq!(deep.dimensions(), (10, 5));
        assert!(deep.pixels().all(|p| p.0 == [65535, 0, 0, 65535]));
        // Nothing showing: one transparent pixel.
        canvas.layers[1].visible = false;
        let empty = export(&canvas, ExportFormat::Png, 100, true).to_rgba8();
        assert_eq!(empty.dimensions(), (1, 1));
        assert_eq!(empty.get_pixel(0, 0).0[3], 0);
        // The layered formats keep the canvas's size whatever is asked.
        let opts = ExportOptions {
            format: ExportFormat::Psd,
            scale_percent: 50,
            trim: true,
            ..Default::default()
        };
        assert_eq!(opts.size_of(100, 80), [100, 80]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_batch_writes_each_preset_comp_and_layer() {
        use crate::app::comps::LayerComp;
        let mut canvas = two_blocks();
        canvas.layers[2].visible = false;
        let red_only = LayerComp::capture("Red only".into(), &canvas.layers);
        canvas.layers[2].visible = true;
        canvas.layers[1].visible = false;
        let blue_only = LayerComp::capture("Blue/only".into(), &canvas.layers);
        canvas.layers[1].visible = true;
        let presets = [
            ExportPreset {
                name: "Web".into(),
                options: ExportOptions {
                    format: ExportFormat::Jpeg,
                    scale_percent: 50,
                    ..Default::default()
                },
            },
            ExportPreset {
                name: "Big".into(),
                options: ExportOptions {
                    scale_percent: 200,
                    ..Default::default()
                },
            },
        ];
        let current = ExportOptions {
            trim: true,
            ..Default::default()
        };
        let all = BatchChoice {
            presets: true,
            comps: true,
            layers: true,
        };
        let comps = [red_only, blue_only];
        let files = batch_files(&canvas, "pic", &current, &presets, &comps, all);
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "pic-Web.jpg",
                "pic-Big.png",
                "pic-Red only.png",
                "pic-Blue_only.png",
                "pic-Background.png",
                "pic-Ink.png",
                "pic-ink 2.png",
            ]
        );
        let only_layers = BatchChoice {
            layers: true,
            ..Default::default()
        };
        canvas.layers[2].visible = false;
        let some = batch_files(&canvas, "pic", &current, &presets, &comps, only_layers);
        assert_eq!(some.len(), 2, "hidden layers are left out");
        canvas.layers[2].visible = true;

        let dir = temp_dir("batch");
        let mut seen = Vec::new();
        let n = run_batch(
            &canvas,
            &files,
            &dir,
            &ExportColor::default(),
            |k, f, path| {
                assert!(path.exists());
                seen.push((k, f.name.clone()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(n, 7);
        assert_eq!(seen.len(), 7);
        let open = |name: &str| image::open(dir.join(name)).unwrap().to_rgba8();
        assert_eq!(open("pic-Web.jpg").dimensions(), (50, 40));
        assert_eq!(open("pic-Big.png").dimensions(), (200, 160));
        // A comp shows its layers, on the paper (nothing to trim).
        let red = open("pic-Red only.png");
        assert_eq!(red.dimensions(), (100, 80));
        assert_eq!(red.get_pixel(15, 25).0, [255, 0, 0, 255]);
        assert_eq!(red.get_pixel(75, 55).0, [255, 255, 255, 255]);
        let blue = open("pic-Blue_only.png");
        assert_eq!(blue.get_pixel(15, 25).0, [255, 255, 255, 255]);
        assert_eq!(blue.get_pixel(75, 55).0, [0, 0, 255, 255]);
        // A layer alone, trimmed to its paint.
        let ink = open("pic-Ink.png");
        assert_eq!(ink.dimensions(), (20, 10));
        assert!(ink.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        assert!(
            open("pic-ink 2.png")
                .pixels()
                .all(|p| p.0 == [0, 0, 255, 255])
        );
        let paper = open("pic-Background.png");
        assert_eq!(paper.dimensions(), (100, 80));
        assert!(paper.pixels().all(|p| p.0 == [255, 255, 255, 255]));
        // The document itself is as it was.
        assert!(canvas.layers.iter().all(|l| l.visible));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_layer_exports_with_what_is_clipped_to_it() {
        let mut canvas = two_blocks();
        canvas.layers[2].clipped = true;
        let current = ExportOptions::default();
        let choice = BatchChoice {
            layers: true,
            ..Default::default()
        };
        let files = batch_files(&canvas, "pic", &current, &[], &[], choice);
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["pic-Background.png", "pic-Ink.png"]);
        let shown = batch_canvas(&canvas, &files[1].view);
        let visible: Vec<bool> = shown.layers.iter().map(|l| l.visible).collect();
        assert_eq!(visible, [false, true, true]);
    }

    #[test]
    fn export_options_read_back_and_stay_in_range() {
        let preset = ExportPreset {
            name: "Web".into(),
            options: ExportOptions {
                format: ExportFormat::WebP,
                scale_percent: 25,
                trim: true,
                jpeg_quality: 80,
            },
        };
        let json = serde_json::to_string(&preset).unwrap();
        let back: ExportPreset = serde_json::from_str(&json).unwrap();
        assert_eq!(back, preset);
        // Missing fields take the defaults; values out of range are tidied.
        let mut partial: ExportOptions =
            serde_json::from_str(r#"{"format":"Jpeg","scale_percent":9000}"#).unwrap();
        assert_eq!(partial.jpeg_quality, DEFAULT_JPEG_QUALITY);
        partial.tidy();
        assert_eq!(partial.scale_percent, *SCALE_RANGE.end());
        assert_eq!(partial.summary(), "JPEG 90, 400%");
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
