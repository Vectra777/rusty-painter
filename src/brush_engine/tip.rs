//! Image brush tips: a mask built from a picture, kept with pre-shrunk
//! copies (mipmaps) so it samples cleanly at any brush size.
//!
//! - **The mask**: the picture's transparency when it has some, otherwise
//!   its darkness on a light background (or lightness on a dark one), so a
//!   black shape on transparent, black on white and white on black all
//!   paint the shape. Empty borders are trimmed.
//! - **Proportions** are kept: the longest side spans the brush size.
//! - **Colours**: a colour picture on transparency keeps its colours as
//!   well, for brushes that paint with them (decorations, ribbons).
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
    /// A colour picture's colours (unmultiplied sRGB), full size; `None`
    /// for a grey tip. Brushes paint with them when asked to.
    pub colors: Option<Vec<[u8; 3]>>,
    /// The colours' mipmaps: red, green and blue, premultiplied by the
    /// mask, one set per level of `levels`.
    color_levels: Vec<[Level; 3]>,
}

impl PartialEq for TipMask {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.pixels == other.pixels
            && self.colors == other.colors
    }
}

impl TipMask {
    /// From a mask (255 = full paint), trimmed to what it covers.
    pub fn from_mask(width: usize, height: usize, pixels: Vec<u8>) -> Arc<Self> {
        Self::from_parts(width, height, pixels, None)
    }

    /// From a mask and its colours (unmultiplied sRGB, one per pixel).
    pub fn from_colored(
        width: usize,
        height: usize,
        pixels: Vec<u8>,
        colors: Vec<[u8; 3]>,
    ) -> Arc<Self> {
        Self::from_parts(width, height, pixels, Some(colors))
    }

    fn from_parts(
        width: usize,
        height: usize,
        pixels: Vec<u8>,
        colors: Option<Vec<[u8; 3]>>,
    ) -> Arc<Self> {
        let rect = trim_rect(width, height, &pixels);
        let (pixels, colors) = (
            crop(&pixels, width, rect),
            colors.map(|c| crop(&c, width, rect)),
        );
        let [x0, y0, x1, y1] = rect;
        let (width, height) = (x1 - x0, y1 - y0);
        let alpha: Vec<f32> = pixels.iter().map(|&v| v as f32 / 255.0).collect();
        let mut levels = vec![Level::new(width, height, alpha.clone())];
        while let Some(last) = levels.last()
            && (last.w > 1 || last.h > 1)
        {
            let next = halve(last);
            levels.push(next);
        }
        let mut color_levels = Vec::new();
        if let Some(colors) = &colors {
            // Premultiplied, so a transparent texel's colour doesn't bleed.
            let plane = |k: usize| {
                let data = colors
                    .iter()
                    .zip(&alpha)
                    .map(|(c, a)| c[k] as f32 / 255.0 * a)
                    .collect();
                Level::new(width, height, data)
            };
            color_levels.push([plane(0), plane(1), plane(2)]);
            while color_levels.len() < levels.len() {
                let last = color_levels.last().expect("one level at least");
                let next = [halve(&last[0]), halve(&last[1]), halve(&last[2])];
                color_levels.push(next);
            }
        }
        Arc::new(Self {
            width,
            height,
            pixels,
            levels,
            colors,
            color_levels,
        })
    }

    /// Whether the tip has colours of its own.
    pub fn has_colors(&self) -> bool {
        self.colors.is_some()
    }

    /// The colours along a row sampled like [`Self::row`] (same sampler,
    /// start and step), unmultiplied sRGB 0..1, given that row's coverage
    /// from [`Self::row`] times `alpha_scale`. A grey tip gives black.
    pub fn color_row(
        &self,
        s: &TipSampler,
        start: (f32, f32),
        step: (f32, f32),
        alpha: &[f32],
        alpha_scale: f32,
        out: &mut [[f32; 3]],
    ) {
        if self.color_levels.is_empty() {
            out.fill([0.0; 3]);
            return;
        }
        let (cx, cy) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
        let (tx0, ty0) = (cx + start.0 * s.per_px, cy + start.1 * s.per_px);
        let (dtx, dty) = (step.0 * s.per_px, step.1 * s.per_px);
        self.color_run(s, (tx0, ty0), (dtx, dty), alpha, alpha_scale, out);
    }

    /// Premultiplied colours from texel `start` by `step`, divided by the
    /// coverage (`alpha / alpha_scale`).
    fn color_run(
        &self,
        s: &TipSampler,
        start: (f32, f32),
        step: (f32, f32),
        alpha: &[f32],
        alpha_scale: f32,
        out: &mut [[f32; 3]],
    ) {
        let inv_scale = 1.0 / alpha_scale.max(1e-6);
        let blend = if s.blend < 0.02 { 0.0 } else { s.blend };
        let mut plane = vec![0.0f32; out.len()];
        for k in 0..3 {
            let run = RowRun {
                lo: &self.color_levels[s.lo][k],
                hi: &self.color_levels[s.hi][k],
                blend,
                scale_lo: s.scale_lo,
                scale_hi: s.scale_hi,
                start,
                step,
            };
            run.run(&mut plane);
            for ((o, &v), &a) in out.iter_mut().zip(&plane).zip(alpha) {
                let a = a * inv_scale;
                o[k] = if a > 1e-4 {
                    (v / a).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            }
        }
    }

    /// Coverage at full-size texel `(tx, ty)` (texel centres at +0.5),
    /// sampled for `s`: for ribbons, which map the picture themselves.
    #[inline]
    pub fn sample_texel(&self, s: &TipSampler, tx: f32, ty: f32) -> f32 {
        let mut out = [0.0];
        let blend = if s.blend < 0.02 { 0.0 } else { s.blend };
        RowRun {
            lo: &self.levels[s.lo],
            hi: &self.levels[s.hi],
            blend,
            scale_lo: s.scale_lo,
            scale_hi: s.scale_hi,
            start: (tx, ty),
            step: (0.0, 0.0),
        }
        .run(&mut out);
        out[0]
    }

    /// The colour at full-size texel `(tx, ty)` whose coverage is `alpha`
    /// (unmultiplied sRGB 0..1; black for a grey tip).
    #[inline]
    pub fn color_texel(&self, s: &TipSampler, tx: f32, ty: f32, alpha: f32) -> [f32; 3] {
        if self.color_levels.is_empty() {
            return [0.0; 3];
        }
        let mut out = [[0.0; 3]];
        self.color_run(s, (tx, ty), (0.0, 0.0), &[alpha], 1.0, &mut out);
        out[0]
    }

    /// How a ribbon `width` pixels across samples this tip (its height
    /// spans the width).
    pub fn ribbon_sampler(&self, width: f32) -> TipSampler {
        // `sampler` fits the longest side to 2r; fit the height instead.
        let longest = self.width.max(self.height) as f32;
        let r = width * longest / self.height.max(1) as f32 / 2.0;
        self.sampler(r)
    }

    /// From a picture (see the module notes for which pixels paint).
    pub fn from_image(img: &image::DynamicImage) -> Arc<Self> {
        Self::from_image_with(img, None)
    }

    /// [`Self::from_image`], an opaque picture's background said rather
    /// than guessed from its edges: `Some(true)` light (dark paints, as in
    /// Krita and Photoshop), `Some(false)` dark.
    pub fn from_image_with(img: &image::DynamicImage, light_background: Option<bool>) -> Arc<Self> {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let luma = |p: &image::Rgba<u8>| {
            (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as u8
        };
        let has_alpha = rgba.pixels().any(|p| p[3] < 250);
        // A picture in colour on transparency keeps its colours too.
        let colored = has_alpha
            && rgba
                .pixels()
                .any(|p| p[3] > 8 && (p[0].abs_diff(p[1]) > 8 || p[1].abs_diff(p[2]) > 8));
        if colored {
            let pixels = rgba.pixels().map(|p| p[3]).collect();
            let colors = rgba.pixels().map(|p| [p[0], p[1], p[2]]).collect();
            return Self::from_colored(w, h, pixels, colors);
        }
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
            let light_background = light_background.unwrap_or(n == 0 || sum / n > 127);
            rgba.pixels()
                .map(|p| {
                    let l = luma(p);
                    if light_background { 255 - l } else { l }
                })
                .collect()
        };
        Self::from_mask(w, h, pixels)
    }

    /// A picture whose alpha is the mask and whose colours (grey ones
    /// too) are kept, for painting by its lightness (a lightness or
    /// gradient map); opaque pictures are opaque all over.
    pub fn from_image_keeping_colors(img: &image::DynamicImage) -> Arc<Self> {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let pixels = rgba.pixels().map(|p| p[3]).collect();
        let colors = rgba.pixels().map(|p| [p[0], p[1], p[2]]).collect();
        Self::from_colored(w, h, pixels, colors)
    }

    /// The same mask, inverted (what painted doesn't, and the other way).
    pub fn inverted(&self) -> Arc<Self> {
        Self::from_parts(
            self.width,
            self.height,
            self.pixels.iter().map(|&v| 255 - v).collect(),
            self.colors.clone(),
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

/// Tips that come with the app, generated once: bristles, a rough chalk
/// disc, spatter and a leaf.
pub fn builtin() -> &'static [(&'static str, Arc<TipMask>)] {
    static TIPS: std::sync::OnceLock<Vec<(&'static str, Arc<TipMask>)>> =
        std::sync::OnceLock::new();
    TIPS.get_or_init(|| {
        const N: usize = 128;
        let make = |f: &dyn Fn(f32, f32) -> f32| {
            // f gets coordinates in -1..1 (y down) and returns 0..1.
            let pixels = (0..N * N)
                .map(|i| {
                    let x = ((i % N) as f32 + 0.5) / N as f32 * 2.0 - 1.0;
                    let y = ((i / N) as f32 + 0.5) / N as f32 * 2.0 - 1.0;
                    (f(x, y).clamp(0.0, 1.0) * 255.0) as u8
                })
                .collect();
            TipMask::from_mask(N, N, pixels)
        };
        let dots = |count: u32, seed: u32, spread: f32, size: (f32, f32)| {
            (0..count)
                .map(|i| {
                    let (a, b, c) = (rand01(i, seed), rand01(i, seed + 1), rand01(i, seed + 2));
                    let (angle, dist) = (a * std::f32::consts::TAU, b.sqrt() * spread);
                    (
                        angle.cos() * dist,
                        angle.sin() * dist,
                        size.0 + (size.1 - size.0) * c,
                    )
                })
                .collect::<Vec<_>>()
        };
        let bristles = dots(26, 11, 0.8, (0.08, 0.16));
        let spatter = dots(40, 23, 0.9, (0.02, 0.1));
        let soft_dot = |x: f32, y: f32, (cx, cy, r): (f32, f32, f32)| {
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / r;
            (1.0 - d).clamp(0.0, 1.0).powf(0.6)
        };
        vec![
            (
                "Bristles",
                make(&|x, y| {
                    bristles
                        .iter()
                        .map(|&d| soft_dot(x, y, d))
                        .fold(0.0, f32::max)
                }),
            ),
            (
                "Rough disc",
                make(&|x, y| {
                    let r = (x * x + y * y).sqrt();
                    let edge = 0.85
                        + 0.12
                            * (rand01((x * 20.0) as u32 * 97 + (y * 20.0 + 40.0) as u32, 5) - 0.5);
                    let grain = rand01(
                        ((x + 1.0) * 60.0) as u32 * 211 + ((y + 1.0) * 60.0) as u32,
                        7,
                    );
                    if r > edge { 0.0 } else { 0.55 + 0.45 * grain }
                }),
            ),
            (
                "Spatter",
                make(&|x, y| {
                    spatter
                        .iter()
                        .map(|&d| soft_dot(x, y, d))
                        .fold(0.0, f32::max)
                }),
            ),
            (
                "Leaf",
                make(&|x, y| {
                    // Pointed at both ends: an eye shape along x, soft rim.
                    let half = (1.0 - x * x).max(0.0) * 0.42;
                    let v = 1.0 - (y.abs() / half.max(1e-3));
                    (v * 6.0).clamp(0.0, 1.0)
                }),
            ),
            (
                "Round leaf",
                make(&|x, y| {
                    // Wide and blunt, with a vein down the middle.
                    let half = (1.0 - x * x).max(0.0).sqrt() * 0.6;
                    let v = 1.0 - (y.abs() / half.max(1e-3));
                    let vein = (y.abs() * 40.0).min(1.0) * 0.4 + 0.6;
                    (v * 5.0).clamp(0.0, 1.0) * if x.abs() < 0.8 { vein } else { 1.0 }
                }),
            ),
            (
                "Long leaf",
                make(&|x, y| {
                    // A thin blade, pointed at the far end.
                    let t = (x + 1.0) * 0.5;
                    let half = (t * (1.0 - t) * 4.0).max(0.0).powf(0.7) * 0.22 * (1.2 - t);
                    let v = 1.0 - (y.abs() / half.max(1e-3));
                    (v * 6.0).clamp(0.0, 1.0)
                }),
            ),
            ("Stitch", make(&|x, y| capsule(x, y, 0.6, 0.13))),
            (
                "Chain link",
                make(&|x, y| {
                    // An oval ring seen face on.
                    let e = ((x / 0.85).powi(2) + (y / 0.45).powi(2)).sqrt();
                    ((1.0 - (e - 0.82).abs() / 0.2) * 3.0).clamp(0.0, 1.0)
                }),
            ),
            ("Chain side", make(&|x, y| capsule(x, y, 0.75, 0.15))),
            ("Lace", lace()),
            ("Flower", flower()),
            ("Striped ribbon", striped_ribbon()),
        ]
    })
}

/// A rounded bar along x, `half` long each way and `radius` thick.
fn capsule(x: f32, y: f32, half: f32, radius: f32) -> f32 {
    let dx = (x.abs() - half).max(0.0);
    let d = (dx * dx + y * y).sqrt();
    ((radius - d) / radius * 4.0).clamp(0.0, 1.0)
}

/// Sides of the wide (ribbon) tips.
const RIBBON_W: usize = 256;
const RIBBON_H: usize = 64;

/// A wide picture from `f(u, v)`: `u` 0..1 along, `v` -1..1 across (down).
fn wide(f: &dyn Fn(f32, f32) -> ([u8; 3], f32), colored: bool) -> Arc<TipMask> {
    let (w, h) = (RIBBON_W, RIBBON_H);
    let mut pixels = Vec::with_capacity(w * h);
    let mut colors = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let u = ((i % w) as f32 + 0.5) / w as f32;
        let v = ((i / w) as f32 + 0.5) / h as f32 * 2.0 - 1.0;
        let (c, a) = f(u, v);
        pixels.push((a.clamp(0.0, 1.0) * 255.0) as u8);
        colors.push(c);
    }
    if colored {
        TipMask::from_colored(w, h, pixels, colors)
    } else {
        TipMask::from_mask(w, h, pixels)
    }
}

/// Lace: a band with holes along the top, scallops below; repeats
/// seamlessly along its length.
fn lace() -> Arc<TipMask> {
    const SCALLOPS: f32 = 4.0;
    wide(
        &|u, v| {
            // Position within one scallop, -0.5..0.5, and the across
            // coordinate stretched to the same scale.
            let k = (u * SCALLOPS).fract() - 0.5;
            let (sx, sy) = (k * 2.0, (v + 0.35) / 0.65);
            let band = (v > -0.95 && v < -0.35) as u8 as f32;
            let hole = ((sx * 2.2).powi(2) + ((v + 0.65) / 0.16).powi(2)).sqrt() < 1.0;
            let ring = (sx * sx + sy * sy).sqrt();
            let scallop = v >= -0.35 && (0.62..0.98).contains(&ring);
            let petal = v >= -0.35 && ring < 0.38;
            let a = if hole {
                0.0
            } else {
                band.max((scallop || petal) as u8 as f32)
            };
            ([255; 3], a)
        },
        false,
    )
}

/// A ribbon in red and white stripes, soft at its edges.
fn striped_ribbon() -> Arc<TipMask> {
    wide(
        &|u, v| {
            let a = ((0.9 - v.abs()) * 20.0).clamp(0.0, 1.0);
            let stripe = ((u * 8.0 + v * 0.25).rem_euclid(1.0) < 0.5) as u8;
            let c = if stripe == 1 {
                [210, 40, 60]
            } else {
                [250, 245, 240]
            };
            (c, a)
        },
        true,
    )
}

/// A five-petal flower in colour: pink petals, a yellow heart.
fn flower() -> Arc<TipMask> {
    const N: usize = 128;
    let mut pixels = Vec::with_capacity(N * N);
    let mut colors = Vec::with_capacity(N * N);
    for i in 0..N * N {
        let x = ((i % N) as f32 + 0.5) / N as f32 * 2.0 - 1.0;
        let y = ((i / N) as f32 + 0.5) / N as f32 * 2.0 - 1.0;
        let (r, theta) = ((x * x + y * y).sqrt(), y.atan2(x));
        let petal = 0.6 + 0.35 * (5.0 * theta).cos();
        let a = ((petal - r) * 25.0).clamp(0.0, 1.0);
        let c = if r < 0.24 {
            [250, 205, 60]
        } else {
            let t = ((r - 0.24) / 0.7).clamp(0.0, 1.0);
            let lerp = |a: f32, b: f32| (a + (b - a) * t) as u8;
            [lerp(250.0, 225.0), lerp(170.0, 80.0), lerp(200.0, 150.0)]
        };
        pixels.push((a * 255.0) as u8);
        colors.push(c);
    }
    TipMask::from_colored(N, N, pixels, colors)
}

/// Tip sets that come with the app: the dabs take the tips in turn.
pub fn builtin_sets() -> Vec<(&'static str, Vec<Arc<TipMask>>)> {
    let named = |names: &[&str]| {
        names
            .iter()
            .filter_map(|n| builtin().iter().find(|(name, _)| name == n))
            .map(|(_, tip)| tip.clone())
            .collect()
    };
    vec![
        ("Mixed leaves", named(&["Leaf", "Round leaf", "Long leaf"])),
        ("Chain", named(&["Chain link", "Chain side"])),
    ]
}

/// A repeatable pseudo-random value in 0..1.
fn rand01(i: u32, seed: u32) -> f32 {
    let mut h = i.wrapping_mul(0x9E37_79B9) ^ seed.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65535.0
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

/// The part `[x0, y0, x1, y1)` of a `width`-wide picture.
fn crop<T: Copy>(v: &[T], width: usize, [x0, y0, x1, y1]: [usize; 4]) -> Vec<T> {
    (y0..y1)
        .flat_map(|y| v[y * width + x0..y * width + x1].iter().copied())
        .collect()
}

/// The rectangle `[x0, y0, x1, y1)` of a mask that has paint: its empty
/// rows and columns cut off the edges (all of it if it has none).
fn trim_rect(width: usize, height: usize, pixels: &[u8]) -> [usize; 4] {
    let painted = |x: usize, y: usize| pixels[y * width + x] > 2;
    let rows: Vec<usize> = (0..height)
        .filter(|&y| (0..width).any(|x| painted(x, y)))
        .collect();
    let cols: Vec<usize> = (0..width)
        .filter(|&x| (0..height).any(|y| painted(x, y)))
        .collect();
    match (rows.first(), rows.last(), cols.first(), cols.last()) {
        (Some(&y0), Some(&y1), Some(&x0), Some(&x1)) => [x0, y0, x1 + 1, y1 + 1],
        _ => [0, 0, width, height],
    }
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
    fn built_in_tips_have_paint_and_hold_steady() {
        for (name, tip) in builtin() {
            let painted = tip.pixels.iter().filter(|&&v| v > 40).count();
            assert!(painted > 100, "{name}: {painted}");
            // Thin tips (stitches, bars) are still a few pixels across.
            assert!(tip.width.max(tip.height) > 60, "{name}");
            assert!(tip.width.min(tip.height) > 8, "{name}");
        }
        // Generated once: the same tip every time.
        assert!(Arc::ptr_eq(&builtin()[0].1, &builtin()[0].1));
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
