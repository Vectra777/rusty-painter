//! Image brush tips: a mask built from a picture, kept with pre-shrunk
//! copies (mipmaps) so it samples cleanly at any brush size.
//!
//! - **The mask**: the picture's transparency when it has some, otherwise
//!   its darkness on a light background (or lightness on a dark one), so a
//!   black shape on transparent, black on white and white on black all
//!   paint the shape. Empty borders are trimmed.
//! - **Proportions** are kept: the longest side spans the brush size.
//! - **Sampling** blends the two copies nearest the dab's size, bilinearly
//!   within each (trilinear), so a large picture used as a small tip neither
//!   shimmers nor jumps as pressure changes the size.

use std::sync::Arc;

#[derive(Debug)]
struct Level {
    w: usize,
    h: usize,
    /// Coverage 0..=1, row-major.
    data: Vec<f32>,
    /// `data` with a one-texel empty border (`(w + 2) × (h + 2)`), so
    /// bilinear reads next to the edge need no bounds checks.
    padded: Vec<f32>,
}

impl Level {
    fn new(w: usize, h: usize, data: Vec<f32>) -> Self {
        let pw = w + 2;
        let mut padded = vec![0.0; pw * (h + 2)];
        for y in 0..h {
            padded[(y + 1) * pw + 1..(y + 1) * pw + 1 + w]
                .copy_from_slice(&data[y * w..(y + 1) * w]);
        }
        Self { w, h, data, padded }
    }

    /// Bilinear at texel position `(x, y)` (texel centres at +0.5).
    #[inline(always)]
    fn bilinear(&self, x: f32, y: f32) -> f32 {
        let (x, y) = (x - 0.5, y - 0.5);
        if x < -1.0 || y < -1.0 || x >= self.w as f32 || y >= self.h as f32 {
            return 0.0;
        }
        // x, y ≥ -1 here, so truncating x + 1 floors it without a libm call
        // (the baseline x86-64 target has no floor instruction). The +1 is
        // also the border's offset.
        let (ix, iy) = ((x + 1.0) as usize, (y + 1.0) as usize);
        let (fx, fy) = (x + 1.0 - ix as f32, y + 1.0 - iy as f32);
        let pw = self.w + 2;
        let i = iy * pw + ix;
        let (a, b, c, d) = (
            self.padded[i],
            self.padded[i + 1],
            self.padded[i + pw],
            self.padded[i + pw + 1],
        );
        let top = a + (b - a) * fx;
        let bottom = c + (d - c) * fx;
        top + (bottom - top) * fy
    }
}

/// How one dab samples a tip: its two mip levels and their blend, fixed
/// for the dab's size.
#[derive(Clone, Copy, Debug)]
pub struct TipSampler {
    lo: usize,
    hi: usize,
    blend: f32,
    /// Full-size texels per canvas pixel.
    per_px: f32,
    /// Level texels per full-size texel, for `lo` and `hi`.
    scale_lo: (f32, f32),
    scale_hi: (f32, f32),
}

/// A brush tip mask with its mipmaps. Shared (`Arc`) between brushes and
/// presets.
#[derive(Debug)]
pub struct TipMask {
    /// The full-size mask, 0..=255 (for previews and the tip list).
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    levels: Vec<Level>,
}

impl PartialEq for TipMask {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.pixels == other.pixels
    }
}

impl TipMask {
    /// From a mask (255 = full paint), trimmed to what it covers.
    pub fn from_mask(width: usize, height: usize, pixels: Vec<u8>) -> Arc<Self> {
        let (width, height, pixels) = trim(width, height, pixels);
        let mut levels = vec![Level::new(
            width,
            height,
            pixels.iter().map(|&v| v as f32 / 255.0).collect(),
        )];
        while let Some(last) = levels.last()
            && (last.w > 1 || last.h > 1)
        {
            let next = halve(last);
            levels.push(next);
        }
        Arc::new(Self {
            width,
            height,
            pixels,
            levels,
        })
    }

    /// From a picture (see the module notes for which pixels paint).
    pub fn from_image(img: &image::DynamicImage) -> Arc<Self> {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let luma = |p: &image::Rgba<u8>| {
            (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as u8
        };
        let has_alpha = rgba.pixels().any(|p| p[3] < 250);
        let pixels: Vec<u8> = if has_alpha {
            rgba.pixels().map(|p| p[3]).collect()
        } else {
            // Opaque: the edges tell the background.
            let mut sum = 0u64;
            let mut n = 0u64;
            for (x, y, p) in rgba.enumerate_pixels() {
                if x == 0 || y == 0 || x as usize == w - 1 || y as usize == h - 1 {
                    sum += luma(p) as u64;
                    n += 1;
                }
            }
            let light_background = n == 0 || sum / n > 127;
            rgba.pixels()
                .map(|p| {
                    let l = luma(p);
                    if light_background { 255 - l } else { l }
                })
                .collect()
        };
        Self::from_mask(w, h, pixels)
    }

    /// The same mask, inverted (what painted doesn't, and the other way).
    pub fn inverted(&self) -> Arc<Self> {
        Self::from_mask(
            self.width,
            self.height,
            self.pixels.iter().map(|&v| 255 - v).collect(),
        )
    }

    /// Longest side over the tip's width and height: `(w, h)` fractions of
    /// the brush size.
    pub fn extent(&self) -> (f32, f32) {
        let longest = self.width.max(self.height).max(1) as f32;
        (self.width as f32 / longest, self.height as f32 / longest)
    }

    /// How far a turned tip's corners reach, as a share of its radius.
    pub fn corner_reach(&self) -> f32 {
        let (w, h) = self.extent();
        (w * w + h * h).sqrt()
    }

    /// How a dab of radius `r` samples this tip.
    pub fn sampler(&self, r: f32) -> TipSampler {
        let longest = self.width.max(self.height) as f32;
        let per_px = longest / (2.0 * r).max(1e-3);
        // Mip level from how many texels one canvas pixel spans.
        let lod = per_px.max(1.0).log2().min((self.levels.len() - 1) as f32);
        let lo = lod.floor() as usize;
        let hi = (lo + 1).min(self.levels.len() - 1);
        let scale = |k: usize| {
            let l = &self.levels[k];
            (
                l.w as f32 / self.width as f32,
                l.h as f32 / self.height as f32,
            )
        };
        TipSampler {
            lo,
            hi,
            blend: if hi == lo { 0.0 } else { lod - lo as f32 },
            per_px,
            scale_lo: scale(lo),
            scale_hi: scale(hi),
        }
    }

    /// Smooth coverage at tip offset `(dx, dy)` for a dab of radius `r`.
    pub fn sample(&self, dx: f32, dy: f32, r: f32) -> f32 {
        let mut out = [0.0];
        self.row(&self.sampler(r), (dx, dy), (0.0, 0.0), &mut out);
        out[0]
    }

    /// Smooth coverage along a row of pixels: pixel `i` at tip offset
    /// `start + i × step` (canvas pixels in the tip's frame). Only the part
    /// of the row that crosses the tip is sampled; the rest is zero.
    #[inline]
    pub fn row(&self, s: &TipSampler, start: (f32, f32), step: (f32, f32), out: &mut [f32]) {
        let (cx, cy) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
        let (tx0, ty0) = (cx + start.0 * s.per_px, cy + start.1 * s.per_px);
        let (dtx, dty) = (step.0 * s.per_px, step.1 * s.per_px);
        // Where the row is inside the tip (a texel's reach beyond its edge).
        let mut lo = 0.0f32;
        let mut hi = out.len() as f32;
        for (p0, d, max) in [
            (tx0, dtx, self.width as f32),
            (ty0, dty, self.height as f32),
        ] {
            let (min, max) = (-1.0, max + 1.0);
            if d.abs() < 1e-9 {
                if p0 < min || p0 > max {
                    hi = lo;
                }
            } else {
                let (a, b) = ((min - p0) / d, (max - p0) / d);
                lo = lo.max(a.min(b));
                hi = hi.min(a.max(b));
            }
        }
        let first = (lo.floor().max(0.0) as usize).min(out.len());
        let last = (hi.ceil().max(0.0) as usize).clamp(first, out.len());
        out[..first].fill(0.0);
        out[last..].fill(0.0);
        let (lo_level, hi_level) = (&self.levels[s.lo], &self.levels[s.hi]);
        // A blend this close to one level isn't worth the second.
        let blend = if s.blend < 0.02 { 0.0 } else { s.blend };
        let run = RowRun {
            lo: lo_level,
            hi: hi_level,
            blend,
            scale_lo: s.scale_lo,
            scale_hi: s.scale_hi,
            start: (tx0 + dtx * first as f32, ty0 + dty * first as f32),
            step: (dtx, dty),
        };
        run.run(&mut out[first..last]);
    }

    /// Hard coverage (the nearest texel of the full-size mask), for aliased
    /// and pixel brushes.
    #[inline]
    pub fn sample_nearest(&self, dx: f32, dy: f32, r: f32) -> f32 {
        let longest = self.width.max(self.height) as f32;
        let per_px = longest / (2.0 * r).max(1e-3);
        let tx = self.width as f32 * 0.5 + dx * per_px;
        let ty = self.height as f32 * 0.5 + dy * per_px;
        if tx < 0.0 || ty < 0.0 {
            return 0.0;
        }
        let (x, y) = (tx as usize, ty as usize);
        let level = &self.levels[0];
        if x >= level.w || y >= level.h {
            return 0.0;
        }
        level.data[y * level.w + x]
    }
}

/// One row's samples: pixel `i` at full-size texel `start + i × step`.
struct RowRun<'a> {
    lo: &'a Level,
    hi: &'a Level,
    blend: f32,
    scale_lo: (f32, f32),
    scale_hi: (f32, f32),
    start: (f32, f32),
    step: (f32, f32),
}

impl RowRun<'_> {
    #[inline(always)]
    fn run(&self, out: &mut [f32]) {
        let (mut tx, mut ty) = self.start;
        for slot in out.iter_mut() {
            let a = self.lo.bilinear(tx * self.scale_lo.0, ty * self.scale_lo.1);
            *slot = if self.blend > 0.0 {
                a + (self.hi.bilinear(tx * self.scale_hi.0, ty * self.scale_hi.1) - a) * self.blend
            } else {
                a
            };
            tx += self.step.0;
            ty += self.step.1;
        }
    }
}

/// Half the size, each texel the average of the (up to) four it covers.
fn halve(level: &Level) -> Level {
    let (w, h) = (level.w.div_ceil(2).max(1), level.h.div_ceil(2).max(1));
    let mut data = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0;
            let mut n = 0.0;
            for (sx, sy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (px, py) = (2 * x + sx, 2 * y + sy);
                if px < level.w && py < level.h {
                    sum += level.data[py * level.w + px];
                    n += 1.0;
                }
            }
            data[y * w + x] = sum / n;
        }
    }
    Level::new(w, h, data)
}

/// Cut the rows and columns with no paint off the edges.
fn trim(width: usize, height: usize, pixels: Vec<u8>) -> (usize, usize, Vec<u8>) {
    let painted = |x: usize, y: usize| pixels[y * width + x] > 2;
    let rows: Vec<usize> = (0..height)
        .filter(|&y| (0..width).any(|x| painted(x, y)))
        .collect();
    let cols: Vec<usize> = (0..width)
        .filter(|&x| (0..height).any(|y| painted(x, y)))
        .collect();
    let (Some(&y0), Some(&y1), Some(&x0), Some(&x1)) =
        (rows.first(), rows.last(), cols.first(), cols.last())
    else {
        return (width, height, pixels);
    };
    if (x0, y0, x1, y1) == (0, 0, width - 1, height - 1) {
        return (width, height, pixels);
    }
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let out = (y0..=y1)
        .flat_map(|y| pixels[y * width + x0..=y * width + x1].iter().copied())
        .collect();
    (w, h, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(size: usize) -> Arc<TipMask> {
        let c = size as f32 / 2.0;
        let pixels = (0..size * size)
            .map(|i| {
                let (x, y) = ((i % size) as f32 + 0.5, (i / size) as f32 + 0.5);
                if (x - c).hypot(y - c) < c * 0.9 {
                    255
                } else {
                    0
                }
            })
            .collect();
        TipMask::from_mask(size, size, pixels)
    }

    #[test]
    fn a_shrunk_tip_is_smooth_where_nearest_would_alias() {
        // A 1-texel checkerboard seen at 1/16 size: every canvas pixel
        // covers 16×16 texels, so it should read an even grey.
        let size = 256;
        let pixels = (0..size * size)
            .map(|i| {
                if (i % size + i / size) % 2 == 0 {
                    255
                } else {
                    0
                }
            })
            .collect();
        let tip = TipMask::from_mask(size, size, pixels);
        for (dx, dy) in [(0.0, 0.0), (1.3, -2.7), (-3.1, 4.4)] {
            let v = tip.sample(dx, dy, 8.0);
            assert!((v - 0.5).abs() < 0.05, "even grey, got {v}");
        }
    }

    #[test]
    fn proportions_are_kept() {
        // A wide bar: 40×10.
        let tip = TipMask::from_mask(40, 10, vec![255; 400]);
        assert_eq!(tip.extent(), (1.0, 0.25));
        // At radius 20 it's 40 wide and 10 tall on the canvas.
        assert!(tip.sample(18.0, 0.0, 20.0) > 0.9);
        assert!(tip.sample(0.0, 4.0, 20.0) > 0.9);
        assert!(
            tip.sample(0.0, 7.0, 20.0) < 0.05,
            "not stretched to a square"
        );
    }

    #[test]
    fn borders_are_trimmed() {
        let mut pixels = vec![0u8; 100];
        for y in 3..6 {
            for x in 2..8 {
                pixels[y * 10 + x] = 255;
            }
        }
        let tip = TipMask::from_mask(10, 10, pixels);
        assert_eq!((tip.width, tip.height), (6, 3));
    }

    #[test]
    fn pictures_paint_their_shape_whatever_the_background() {
        use image::{DynamicImage, Rgba, RgbaImage};
        let shape = |bg: Rgba<u8>, ink: Rgba<u8>| {
            let mut img = RgbaImage::from_pixel(8, 8, bg);
            for y in 2..6 {
                for x in 2..6 {
                    img.put_pixel(x, y, ink);
                }
            }
            TipMask::from_image(&DynamicImage::ImageRgba8(img))
        };
        let black = Rgba([0, 0, 0, 255]);
        let white = Rgba([255, 255, 255, 255]);
        for tip in [
            shape(Rgba([0, 0, 0, 0]), black), // black on transparent
            shape(white, black),              // black on white
            shape(black, white),              // white on black
        ] {
            assert_eq!((tip.width, tip.height), (4, 4), "only the shape");
            assert!(tip.pixels.iter().all(|&v| v == 255));
        }
    }

    #[test]
    fn sizes_blend_smoothly() {
        // Coverage at the centre changes little between nearby sizes (no
        // jump from one mip level to the next).
        let tip = disc(512);
        let mut last = tip.sample(3.0, 3.0, 40.0);
        for i in 1..60 {
            let r = 40.0 - i as f32 * 0.5;
            let v = tip.sample(3.0, 3.0, r);
            assert!((v - last).abs() < 0.08, "r {r}: {last} → {v}");
            last = v;
        }
    }
}
