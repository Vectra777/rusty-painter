//! Importing pictures as new layers (File → Import Image…, or dropping
//! files on the window). The picture lands centred, scaled down to fit if
//! it's bigger than the canvas, with the Transform tool ready to place it.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::selection::transform::TransformInfo;
use eframe::egui::Color32;
use std::path::PathBuf;
use std::sync::Arc;

/// Where a file's bytes come from: read on a job's thread.
#[derive(Clone)]
pub(crate) enum FileSource {
    Path(PathBuf),
    /// Already in memory (dropped on the window on the web, Android's
    /// gallery).
    Bytes(Arc<[u8]>),
}

impl FileSource {
    pub(crate) fn read(&self) -> Result<std::borrow::Cow<'_, [u8]>, String> {
        match self {
            Self::Path(path) => std::fs::read(path)
                .map(std::borrow::Cow::Owned)
                .map_err(|e| format!("Couldn't read {}: {e}", path.display())),
            Self::Bytes(bytes) => Ok(std::borrow::Cow::Borrowed(bytes)),
        }
    }
}

/// A picture's pixels, in sRGB (converted from the profile it carries).
pub(crate) fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    decode_image_in(bytes, &crate::canvas::color_profile::ColorProfile::Srgb)
}

/// A picture's pixels, converted from the colour profile it carries (sRGB
/// if none) to `to`.
pub(crate) fn decode_image_in(
    bytes: &[u8],
    to: &crate::canvas::color_profile::ColorProfile,
) -> Result<image::RgbaImage, String> {
    use crate::canvas::color_profile::{ColorProfile, RenderingIntent, RgbTransform};
    use image::ImageDecoder;
    let failed = |e: image::ImageError| format!("Couldn't open the image: {e}");
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Couldn't open the image: {e}"))?
        .into_decoder()
        .map_err(failed)?;
    let from = (decoder.icc_profile().ok().flatten())
        .and_then(|icc| ColorProfile::from_icc(icc, "Picture's profile").ok())
        .unwrap_or_default();
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(failed)?
        .to_rgba8();
    if &from != to
        && let Ok(t) = RgbTransform::new(&from, to, RenderingIntent::Perceptual)
    {
        let mut rgb: Vec<[f32; 3]> = img
            .pixels()
            .map(|p| [p[0], p[1], p[2]].map(|v| v as f32 / 255.0))
            .collect();
        t.convert(&mut rgb);
        for (p, c) in img.pixels_mut().zip(rgb) {
            let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            p.0 = [r, g, b, p[3]];
        }
    }
    Ok(img)
}

/// `img` scaled down to fit `w`×`h` if it's bigger.
pub(crate) fn fit_image(img: image::RgbaImage, (cw, ch): (u32, u32)) -> image::RgbaImage {
    if img.width() <= cw && img.height() <= ch {
        return img;
    }
    let k = (cw as f32 / img.width() as f32).min(ch as f32 / img.height() as f32);
    let (w, h) = (
        ((img.width() as f32 * k).round() as u32).max(1),
        ((img.height() as f32 * k).round() as u32).max(1),
    );
    downscale(&img, w, h)
}

impl PainterApp {
    /// Import a picture as a new layer: read, decoded and fitted to the
    /// canvas on another thread.
    pub(crate) fn import_image_in_background(&mut self, name: String, source: FileSource) {
        let size = (self.canvas.width() as u32, self.canvas.height() as u32);
        // In the document's colours.
        let profile = self.canvas.profile.clone();
        self.spawn_job(None, move || {
            let result = source
                .read()
                .and_then(|bytes| decode_image_in(&bytes, &profile))
                .map(|img| fit_image(img, size));
            Box::new(move |app: &mut PainterApp| match result {
                // (Fitted again if the canvas changed size meanwhile.)
                Ok(img) => app.import_rgba(&name, img),
                Err(err) => app.report(err),
            })
        });
    }

    #[cfg(any(test, feature = "bench"))]
    pub(crate) fn import_image_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| format!("Couldn't open the image: {e}"))?
            .to_rgba8();
        self.import_rgba(name, img);
        Ok(())
    }

    /// Put `img` on a new layer above the selected one.
    pub(crate) fn import_rgba(&mut self, name: &str, img: image::RgbaImage) {
        let (cw, ch) = (self.canvas.width() as u32, self.canvas.height() as u32);
        let img = fit_image(img, (cw, ch));
        let (w, h) = (img.width() as i32, img.height() as i32);
        let (ox, oy) = ((cw as i32 - w) / 2, (ch as i32 - h) / 2);

        let ts = self.canvas.tile_size() as i32;
        let tiles = pixels_to_tiles(ts, (ox, oy), (w, h), |x, y| {
            let [r, g, b, a] = img.get_pixel(x as u32, y as u32).0;
            Color32::from_rgba_unmultiplied(r, g, b, a)
        });
        if self
            .add_layer_with_tiles(name.to_string(), tiles, |_| {})
            .is_none()
        {
            return;
        }
        self.selection_manager.clear_selection();
        self.active_tool = Tool::Transform(TransformInfo::default());
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Import every image file dropped on the window this frame. Dropped on
    /// the open Palette window, a picture gives its colours instead; on the
    /// Reference window, it becomes the reference image.
    pub(crate) fn import_dropped_files(&mut self, ctx: &eframe::egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let to_palette = self.drop_is_on_palette(ctx);
        let to_reference = self.drop_is_on_reference(ctx);
        for file in dropped {
            let path = file.path.as_deref();
            let name = path
                .unwrap_or_else(|| std::path::Path::new(&file.name))
                .file_stem()
                .map_or_else(|| "Image".to_string(), |s| s.to_string_lossy().into_owned());
            let extension = |ext: &str| {
                path.unwrap_or_else(|| std::path::Path::new(&file.name))
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(ext))
            };
            let project = path.filter(|_| {
                extension("rpainter")
                    || crate::project::FOREIGN_EXTENSIONS
                        .iter()
                        .any(|e| extension(e))
            });
            let source = match (&file.bytes, path) {
                (Some(bytes), _) => FileSource::Bytes(Arc::clone(bytes)),
                (None, Some(path)) => FileSource::Path(path.to_path_buf()),
                (None, None) => continue,
            };
            // All read and decoded on other threads.
            if let Some(project) = project {
                // Projects open (as File → Open does).
                self.open_picked(
                    &crate::app::files::OpenFor::Document,
                    file.name.clone(),
                    source,
                    Some(project.to_path_buf()),
                );
            } else if Self::is_brush_file(&file.name)
                || path.is_some_and(|p| Self::is_brush_file(&p.to_string_lossy()))
            {
                // Brushes (ours or another app's) join the library.
                let name = path
                    .and_then(|p| p.file_name())
                    .map_or_else(|| file.name.clone(), |n| n.to_string_lossy().into_owned());
                self.import_brushes_in_background(name, source);
            } else if let Some(path) = path.filter(|p| {
                extension("json")
                    && std::fs::read(p).is_ok_and(|b| crate::project::anim_import::is_rig_json(&b))
            }) {
                // Spine, DragonBones or Lottie: a rig layer.
                self.import_animation_in_background(path.to_path_buf());
            } else if let Some(path) = path.filter(|_| {
                ["mp4", "webm", "mov", "mkv", "avi"]
                    .iter()
                    .any(|e| extension(e))
            }) {
                // Videos: an animated layer.
                self.import_frames_in_background(path.to_path_buf());
            } else if to_reference {
                self.open_reference_in_background(file.name.clone(), source);
            } else if to_palette {
                self.palette_from_image_in_background(name, source);
            } else {
                self.import_image_in_background(name, source);
            }
        }
    }
}

/// Shrink `src` to `w`×`h` by averaging the source area under each output
/// pixel (weighted by alpha, so transparent pixels don't darken edges).
/// Parallel over output rows; the right filter for downscaling, with no
/// ringing or aliasing.
pub(crate) fn downscale(src: &image::RgbaImage, w: u32, h: u32) -> image::RgbaImage {
    use rayon::prelude::*;
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (w as usize, h as usize);
    // For each output column / row: the source pixels it covers, weighted.
    let spans = |from: usize, to: usize| -> Vec<Vec<(usize, f32)>> {
        let k = from as f64 / to as f64;
        (0..to)
            .map(|o| {
                let (a, b) = (o as f64 * k, (o + 1) as f64 * k);
                (a.floor() as usize..(b.ceil() as usize).min(from))
                    .map(|i| {
                        let cover = (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0);
                        (i, cover as f32)
                    })
                    .filter(|(_, c)| *c > 0.0)
                    .collect()
            })
            .collect()
    };
    let (cols, rows) = (spans(sw, w), spans(sh, h));
    let raw = src.as_raw();
    let mut out = vec![0u8; w * h * 4];
    out.par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(oy, out_row)| {
            // Vertical pass into one premultiplied row, then horizontal.
            let mut acc = vec![0.0f32; sw * 4];
            for &(sy, wy) in &rows[oy] {
                let row = &raw[sy * sw * 4..(sy + 1) * sw * 4];
                for (a, p) in acc
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(row.as_chunks::<4>().0.iter())
                {
                    let alpha = p[3] as f32 * wy;
                    a[0] += p[0] as f32 * alpha;
                    a[1] += p[1] as f32 * alpha;
                    a[2] += p[2] as f32 * alpha;
                    a[3] += alpha;
                }
            }
            let row_weight: f32 = rows[oy].iter().map(|r| r.1).sum();
            for (ox, o) in out_row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let mut sum = [0.0f32; 4];
                let mut weight = 0.0;
                for &(sx, wx) in &cols[ox] {
                    let a = &acc[sx * 4..sx * 4 + 4];
                    for c in 0..4 {
                        sum[c] += a[c] * wx;
                    }
                    weight += wx;
                }
                let alpha = sum[3];
                if alpha > 0.0 {
                    for c in 0..3 {
                        o[c] = (sum[c] / alpha + 0.5).min(255.0) as u8;
                    }
                }
                o[3] = (alpha / (weight * row_weight) + 0.5).min(255.0) as u8;
            }
        });
    image::RgbaImage::from_raw(w as u32, h as u32, out).expect("sized to w×h")
}

#[cfg(not(mobile))]
pub(crate) fn import_image_dialog(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog()
        .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "tif", "tiff"]);
    app.file_dialog_job(dialog, crate::app::jobs::Pick::File, |app, paths| {
        let path = paths[0].clone();
        let name = path
            .file_stem()
            .map_or_else(|| "Image".to_string(), |s| s.to_string_lossy().into_owned());
        app.import_image_in_background(name, FileSource::Path(path));
    });
}

/// No file dialog on Android: pick from the photo library instead.
#[cfg(target_os = "android")]
pub(crate) fn import_image_dialog(app: &mut PainterApp) {
    app.workspace.gallery.open();
}

/// iOS: the system's photo picker.
#[cfg(target_os = "ios")]
pub(crate) fn import_image_dialog(app: &mut PainterApp) {
    app.pick_open(crate::app::files::OpenFor::Image);
}

/// The `w`×`h` pixels `pixel(x, y)` returns (premultiplied), placed with
/// their top-left at canvas `origin`, cut into whole tiles of `ts`; tiles
/// left fully transparent are skipped.
pub(crate) fn pixels_to_tiles(
    ts: i32,
    (ox, oy): (i32, i32),
    (w, h): (i32, i32),
    pixel: impl Fn(i32, i32) -> Color32 + Sync,
) -> Vec<((i32, i32), Vec<Color32>)> {
    use rayon::prelude::*;
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    let keys: Vec<(i32, i32)> = (oy.div_euclid(ts)..=(oy + h - 1).div_euclid(ts))
        .flat_map(|ty| (ox.div_euclid(ts)..=(ox + w - 1).div_euclid(ts)).map(move |tx| (tx, ty)))
        .collect();
    keys.par_iter()
        .filter_map(|&(tx, ty)| {
            let mut tile = vec![Color32::TRANSPARENT; (ts * ts) as usize];
            let mut any = false;
            for ly in 0..ts {
                let y = ty * ts + ly - oy;
                if y < 0 || y >= h {
                    continue;
                }
                for lx in 0..ts {
                    let x = tx * ts + lx - ox;
                    if x < 0 || x >= w {
                        continue;
                    }
                    let p = pixel(x, y);
                    if p.a() > 0 {
                        tile[(ly * ts + lx) as usize] = p;
                        any = true;
                    }
                }
            }
            any.then_some(((tx, ty), tile))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages_areas() {
        // 4×2 image: left half red, right half blue, top-right transparent.
        let mut img = image::RgbaImage::new(4, 2);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = match (x < 2, y) {
                (true, _) => image::Rgba([255, 0, 0, 255]),
                (false, 0) => image::Rgba([0, 0, 0, 0]),
                (false, _) => image::Rgba([0, 0, 255, 255]),
            };
        }
        let out = downscale(&img, 2, 1);
        assert_eq!(out.get_pixel(0, 0).0, [255, 0, 0, 255]);
        // Half transparent, and the transparent black doesn't darken it.
        assert_eq!(out.get_pixel(1, 0).0, [0, 0, 255, 128]);
        // Non-integer ratio keeps full coverage opaque.
        let flat = image::RgbaImage::from_pixel(7, 5, image::Rgba([10, 20, 30, 255]));
        assert!(
            downscale(&flat, 3, 2)
                .pixels()
                .all(|p| p.0 == [10, 20, 30, 255])
        );
    }
}
