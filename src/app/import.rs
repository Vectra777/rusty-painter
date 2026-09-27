//! Importing pictures as new layers (File → Import Image…, or dropping
//! files on the window). The picture lands centred, scaled down to fit if
//! it's bigger than the canvas, with the Transform tool ready to place it.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::selection::transform::TransformInfo;
use eframe::egui::Color32;

impl PainterApp {
    pub(crate) fn import_image_path(&mut self, path: &std::path::Path) -> Result<(), String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        let name = path
            .file_stem()
            .map_or_else(|| "Image".to_string(), |s| s.to_string_lossy().into_owned());
        self.import_image_bytes(&name, &bytes)
    }

    pub(crate) fn import_image_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| format!("Couldn't open the image: {e}"))?
            .to_rgba8();
        self.import_rgba(name, img);
        Ok(())
    }

    /// Put `img` on a new layer above the selected one.
    pub(crate) fn import_rgba(&mut self, name: &str, mut img: image::RgbaImage) {
        let (cw, ch) = (self.canvas.width() as u32, self.canvas.height() as u32);
        if img.width() > cw || img.height() > ch {
            let k = (cw as f32 / img.width() as f32).min(ch as f32 / img.height() as f32);
            let (w, h) = (
                ((img.width() as f32 * k).round() as u32).max(1),
                ((img.height() as f32 * k).round() as u32).max(1),
            );
            img = downscale(&img, w, h);
        }
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
    /// the open Palette window, a picture gives its colours instead.
    pub(crate) fn import_dropped_files(&mut self, ctx: &eframe::egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let to_palette = self.drop_is_on_palette(ctx);
        for file in dropped {
            let path = file.path.as_deref();
            let name = path
                .map_or_else(|| std::path::Path::new(&file.name), |p| p)
                .file_stem()
                .map_or_else(|| "Image".to_string(), |s| s.to_string_lossy().into_owned());
            let result = if path.is_some_and(|p| p.extension().is_some_and(|e| e == "rpainter")) {
                // Projects open.
                self.load_project_from_path(path.unwrap())
            } else {
                let bytes = match (&file.bytes, path) {
                    (Some(bytes), _) => Ok(bytes.to_vec()),
                    (None, Some(path)) => std::fs::read(path)
                        .map_err(|e| format!("Couldn't read {}: {e}", path.display())),
                    (None, None) => continue,
                };
                bytes.and_then(|bytes| {
                    if to_palette {
                        self.extract_palette_from_image(&name, &bytes)
                    } else {
                        self.import_image_bytes(&name, &bytes)
                    }
                })
            };
            if let Err(err) = result {
                log::error!("{err}");
                self.export_state.message = Some(err);
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

#[cfg(not(target_os = "android"))]
pub(crate) fn import_image_dialog(app: &mut PainterApp) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "tif", "tiff"])
        .pick_file()
    else {
        return;
    };
    if let Err(err) = app.import_image_path(&path) {
        log::error!("{err}");
        app.export_state.message = Some(err);
    }
}

/// No file dialog on Android: pick from the photo library instead.
#[cfg(target_os = "android")]
pub(crate) fn import_image_dialog(app: &mut PainterApp) {
    app.workspace.gallery.open();
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
