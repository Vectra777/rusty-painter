//! The pictures saved next to the project data: the flattened image and a
//! thumbnail of at most 256 px, as file managers expect in OpenRaster files.

use eframe::egui::{Color32, ColorImage};
use image::{
    ImageEncoder,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
pub(super) const THUMBNAIL_MAX_EDGE: usize = 256;

/// `img` shrunk to fit `max_edge`, each pixel the average of the source
/// pixels it covers (premultiplied, so transparent edges don't darken).
pub(super) fn thumbnail(img: &ColorImage, max_edge: usize) -> ColorImage {
    let [w, h] = img.size;
    let longest = w.max(h).max(1);
    if longest <= max_edge {
        return img.clone();
    }
    let dst_w = (w * max_edge).div_ceil(longest).max(1);
    let dst_h = (h * max_edge).div_ceil(longest).max(1);
    let mut out = ColorImage::new([dst_w, dst_h], Color32::TRANSPARENT);
    for y in 0..dst_h {
        let (y0, y1) = (y * h / dst_h, ((y + 1) * h / dst_h).max(y * h / dst_h + 1));
        for x in 0..dst_w {
            let (x0, x1) = (x * w / dst_w, ((x + 1) * w / dst_w).max(x * w / dst_w + 1));
            let mut sum = [0u32; 4];
            for yy in y0..y1 {
                for p in &img.pixels[yy * w + x0..yy * w + x1] {
                    for (s, c) in sum.iter_mut().zip(p.to_array()) {
                        *s += u32::from(c);
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let [r, g, b, a] = sum.map(|s| ((s + n / 2) / n) as u8);
            out.pixels[y * dst_w + x] = Color32::from_rgba_premultiplied(r, g, b, a);
        }
    }
    out
}

/// PNG with fast compression: saving a large canvas shouldn't stall.
pub(super) fn encode_png(img: ColorImage) -> Result<Vec<u8>, String> {
    let (w, h) = (img.size[0] as u32, img.size[1] as u32);
    let rgba = super::export::to_rgba_image(img)?;
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .map_err(|err| format!("Image encode failed: {err}"))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_averages_and_keeps_the_shape() {
        let mut img = ColorImage::new([1000, 500], Color32::WHITE);
        // Left half black.
        for row in img.pixels.as_chunks_mut::<1000>().0 {
            row[..500].fill(Color32::BLACK);
        }
        let t = thumbnail(&img, 256);
        assert_eq!(t.size, [256, 128]);
        assert_eq!(t.pixels[0], Color32::BLACK);
        assert_eq!(t.pixels[255], Color32::WHITE);
        let small = ColorImage::new([10, 20], Color32::RED);
        assert_eq!(thumbnail(&small, 256).size, [10, 20]);
    }
}
