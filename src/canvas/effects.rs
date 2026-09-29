//! The Filter menu's effects that read around each pixel or depend on where
//! it is: glow, chromatic aberration, halftone, emboss, edges, clouds,
//! median, oil paint, vignette, zoom and spin blur, dither. Pure functions
//! over a row-major buffer of premultiplied sRGB pixels whose top-left pixel
//! is at canvas point `origin`, like the rest of [`crate::canvas::filters`].

use crate::canvas::blend::Unmultiply;
use eframe::egui::Color32;
use rayon::prelude::*;

/// Where the canvas is, for effects that work from its middle: its centre
/// and half its size, in the same pixels as the buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub centre: [f32; 2],
    pub half: [f32; 2],
}

impl Frame {
    /// Not set up yet ([`crate::canvas::filters::Filter::fitted`] does).
    pub const NONE: Frame = Frame {
        centre: [0.0; 2],
        half: [0.0; 2],
    };

    pub fn of_canvas(w: usize, h: usize) -> Self {
        let half = [w as f32 / 2.0, h as f32 / 2.0];
        Self { centre: half, half }
    }

    /// The same frame on a picture `k` times smaller.
    pub fn shrunk(self, k: f32) -> Self {
        Self {
            centre: self.centre.map(|v| v / k),
            half: self.half.map(|v| v / k),
        }
    }
}

/// Premultiplied pixel as floats 0..=1.
#[inline]
fn f4(c: Color32) -> [f32; 4] {
    c.to_array().map(|v| v as f32 / 255.0)
}

/// Back to a pixel. (egui premultiplies in linear light, so a stored
/// channel may be above alpha: only 0..=1 is enforced.)
#[inline]
fn c4(v: [f32; 4]) -> Color32 {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_premultiplied(q(v[0]), q(v[1]), q(v[2]), q(v[3]))
}

#[inline]
fn luma(c: Color32) -> f32 {
    let [r, g, b, a] = c.unmultiplied();
    if a == 0 {
        return 1.0;
    }
    (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0
}

/// The buffer at `(x, y)` (buffer pixels), bilinear, clamped to its edges.
fn sample(src: &[Color32], w: usize, h: usize, x: f32, y: f32) -> [f32; 4] {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |x: usize, y: usize| f4(src[y * w + x]);
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    [0, 1, 2, 3].map(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

/// [`sample`] in whole numbers, for summing many: each channel times
/// 65536 (weights in 1/256ths each way).
#[inline]
fn sample_fixed(src: &[Color32], w: usize, h: usize, x: f32, y: f32) -> [u32; 4] {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let fx = ((x - x0 as f32) * 256.0) as u32;
    let fy = ((y - y0 as f32) * 256.0) as u32;
    let (a, b) = (src[y0 * w + x0].to_array(), src[y0 * w + x1].to_array());
    let (c, d) = (src[y1 * w + x0].to_array(), src[y1 * w + x1].to_array());
    std::array::from_fn(|i| {
        let top = a[i] as u32 * (256 - fx) + b[i] as u32 * fx;
        let bottom = c[i] as u32 * (256 - fx) + d[i] as u32 * fx;
        top * (256 - fy) + bottom * fy
    })
}

/// Rows in parallel: `pixel(x, y)` for each pixel of the `w`×`h` buffer.
fn per_pixel(w: usize, h: usize, pixel: impl Fn(usize, usize) -> Color32 + Sync) -> Vec<Color32> {
    let mut out = vec![Color32::TRANSPARENT; w * h];
    out.par_chunks_mut(w.max(1))
        .enumerate()
        .for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = pixel(x, y);
            }
        });
    out
}

/// Light bleeding out of the bright parts: those brighter than `threshold`
/// blurred by `radius` and screened over the picture, `strength` of it.
pub fn glow(
    src: &[Color32],
    w: usize,
    h: usize,
    radius: f32,
    strength: f32,
    threshold: f32,
) -> Vec<Color32> {
    let bright: Vec<Color32> = src
        .par_iter()
        .map(|&c| {
            let l = luma(c);
            let k = ((l - threshold) / (1.0 - threshold).max(0.05)).clamp(0.0, 1.0);
            let v = f4(c).map(|x| x * k);
            c4(v)
        })
        .collect();
    let halo = crate::canvas::filters::gaussian_blur(&bright, w, h, radius);
    src.par_iter()
        .zip(halo.par_iter())
        .map(|(&s, &g)| {
            if g.a() == 0 || strength <= 0.0 {
                return s;
            }
            let (s, g) = (f4(s), f4(g));
            // Screen, colour and alpha alike: the glow reaches past the paint.
            c4([0, 1, 2, 3].map(|i| s[i] + g[i] * strength * (1.0 - s[i])))
        })
        .collect()
}

/// Red and blue pulled apart, more towards the edges of the canvas, as a
/// cheap lens does: `amount` pixels at its corners.
pub fn chromatic_aberration(
    src: &[Color32],
    w: usize,
    h: usize,
    origin: (i32, i32),
    amount: f32,
    frame: Frame,
) -> Vec<Color32> {
    let reach = (frame.half[0].max(frame.half[1])).max(1.0);
    per_pixel(w, h, |x, y| {
        let (cx, cy) = (origin.0 as f32 + x as f32, origin.1 as f32 + y as f32);
        let (dx, dy) = (
            (cx - frame.centre[0]) / reach * amount,
            (cy - frame.centre[1]) / reach * amount,
        );
        let red = sample(src, w, h, x as f32 - dx, y as f32 - dy);
        let blue = sample(src, w, h, x as f32 + dx, y as f32 + dy);
        let own = f4(src[y * w + x]);
        c4([red[0], own[1], blue[2], red[3].max(own[3]).max(blue[3])])
    })
}

/// A printed look: dots on a grid turned by `angle` degrees, `size` pixels
/// apart, as big as the picture is dark there. Black ink on white, or the
/// picture's own colours with `colour`. Transparency stays.
pub fn halftone(
    src: &[Color32],
    w: usize,
    h: usize,
    origin: (i32, i32),
    size: f32,
    angle: f32,
    colour: bool,
) -> Vec<Color32> {
    let size = size.max(2.0);
    let (s, c) = angle.to_radians().sin_cos();
    per_pixel(w, h, |x, y| {
        let own = src[y * w + x];
        if own.a() == 0 {
            return own;
        }
        let (px, py) = (
            origin.0 as f32 + x as f32 + 0.5,
            origin.1 as f32 + y as f32 + 0.5,
        );
        // Into the grid's turned space, to the middle of this cell, and back.
        let (u, v) = (px * c + py * s, -px * s + py * c);
        let (cu, cv) = (
            ((u / size).floor() + 0.5) * size,
            ((v / size).floor() + 0.5) * size,
        );
        let (mx, my) = (cu * c - cv * s, cu * s + cv * c);
        let cell = sample(
            src,
            w,
            h,
            mx - origin.0 as f32 - 0.5,
            my - origin.1 as f32 - 0.5,
        );
        let cell_c = c4(cell);
        let dark = 1.0 - luma(cell_c);
        // Dot area in step with darkness; full dots touch at the diagonal.
        let radius = size * std::f32::consts::FRAC_1_SQRT_2 * dark.sqrt();
        let d = ((u - cu).powi(2) + (v - cv).powi(2)).sqrt();
        let ink = (radius - d + 0.5).clamp(0.0, 1.0);
        let [ir, ig, ib] = if colour {
            let [r, g, b, _] = cell_c.unmultiplied();
            // The cell's hue at full strength, its darkness in the dot size.
            let m = r.max(g).max(b).max(1) as f32;
            let lift = |v: u8| (v as f32 / m * 255.0).min(255.0);
            [lift(r), lift(g), lift(b)]
        } else {
            [0.0; 3]
        };
        let paper = 255.0;
        let mix = |i: f32| (paper + (i - paper) * ink).round() as u8;
        Color32::from_rgba_unmultiplied(mix(ir), mix(ig), mix(ib), own.a())
    })
}

/// A grey relief lit from `angle` degrees, `depth` its strength.
pub fn emboss(src: &[Color32], w: usize, h: usize, angle: f32, depth: f32) -> Vec<Color32> {
    let (s, c) = angle.to_radians().sin_cos();
    // Brightness once per pixel, then read between pixels from that.
    let l: Vec<f32> = src.par_iter().map(|&c| luma(c)).collect();
    let at = |x: f32, y: f32| {
        let x = x.clamp(0.0, (w - 1) as f32);
        let y = y.clamp(0.0, (h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let top = l[y0 * w + x0] + (l[y0 * w + x1] - l[y0 * w + x0]) * fx;
        let bottom = l[y1 * w + x0] + (l[y1 * w + x1] - l[y1 * w + x0]) * fx;
        top + (bottom - top) * fy
    };
    per_pixel(w, h, |x, y| {
        let (fx, fy) = (x as f32, y as f32);
        let slope = at(fx + c, fy + s) - at(fx - c, fy - s);
        let v = ((0.5 + slope * depth) * 255.0).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgba_unmultiplied(v, v, v, src[y * w + x].a())
    })
}

/// Edges as dark lines on white (Sobel on brightness); transparency stays.
pub fn find_edges(src: &[Color32], w: usize, h: usize) -> Vec<Color32> {
    let l: Vec<f32> = src.par_iter().map(|&c| luma(c)).collect();
    per_pixel(w, h, |x, y| {
        let at = |dx: i32, dy: i32| {
            let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
            let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
            l[yy * w + xx]
        };
        let gx = at(1, -1) + 2.0 * at(1, 0) + at(1, 1) - at(-1, -1) - 2.0 * at(-1, 0) - at(-1, 1);
        let gy = at(-1, 1) + 2.0 * at(0, 1) + at(1, 1) - at(-1, -1) - 2.0 * at(0, -1) - at(1, -1);
        let edge = (gx * gx + gy * gy).sqrt().min(1.0);
        let v = ((1.0 - edge) * 255.0).round() as u8;
        Color32::from_rgba_unmultiplied(v, v, v, src[y * w + x].a())
    })
}

/// A pseudo-random value in 0..1 for a lattice point.
#[inline]
fn lattice(x: i32, y: i32, octave: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ octave.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

/// Smooth value noise at `(x, y)` on a lattice `cell` pixels wide.
fn value_noise(x: f32, y: f32, cell: f32, octave: u32) -> f32 {
    let (u, v) = (x / cell, y / cell);
    let (x0, y0) = (u.floor() as i32, v.floor() as i32);
    let (fx, fy) = (u - x0 as f32, v - y0 as f32);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let a = lattice(x0, y0, octave);
    let b = lattice(x0 + 1, y0, octave);
    let c = lattice(x0, y0 + 1, octave);
    let d = lattice(x0 + 1, y0 + 1, octave);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// Grey clouds (fractal noise), `scale` pixels for the largest puffs,
/// `detail` octaves; pinned to the canvas and opaque.
pub fn clouds(w: usize, h: usize, origin: (i32, i32), scale: f32, detail: u8) -> Vec<Color32> {
    let octaves = detail.clamp(1, 8) as u32;
    per_pixel(w, h, |x, y| {
        let (px, py) = (origin.0 as f32 + x as f32, origin.1 as f32 + y as f32);
        let (mut sum, mut weight, mut amp, mut cell) = (0.0, 0.0, 1.0, scale.max(2.0));
        for o in 0..octaves {
            sum += value_noise(px, py, cell, o) * amp;
            weight += amp;
            amp *= 0.5;
            cell = (cell * 0.5).max(1.0);
        }
        let v = (sum / weight * 255.0).round() as u8;
        Color32::from_rgb(v, v, v)
    })
}

/// Each channel's median over a square `radius` around each pixel (Huang's
/// sliding histogram): specks go, edges stay.
pub fn median(src: &[Color32], w: usize, h: usize, radius: u32) -> Vec<Color32> {
    let r = radius.max(1) as i32;
    let mut out = vec![Color32::TRANSPARENT; w * h];
    out.par_chunks_mut(w.max(1))
        .enumerate()
        .for_each(|(y, row)| {
            let mut hist = [[0u32; 256]; 4];
            let mut count = 0u32;
            let add = |hist: &mut [[u32; 256]; 4], x: i32, sign: i32, count: &mut u32| {
                if x < 0 || x >= w as i32 {
                    return;
                }
                for dy in -r..=r {
                    let yy = y as i32 + dy;
                    if yy < 0 || yy >= h as i32 {
                        continue;
                    }
                    let c = src[yy as usize * w + x as usize].to_array();
                    for ch in 0..4 {
                        let slot = &mut hist[ch][c[ch] as usize];
                        *slot = (*slot as i32 + sign) as u32;
                    }
                    *count = (*count as i32 + sign) as u32;
                }
            };
            for x in -r..r {
                add(&mut hist, x, 1, &mut count);
            }
            for (x, o) in row.iter_mut().enumerate() {
                let x = x as i32;
                add(&mut hist, x + r, 1, &mut count);
                let half = count.div_ceil(2);
                let mut v = [0u8; 4];
                for ch in 0..4 {
                    let mut seen = 0;
                    for (value, &n) in hist[ch].iter().enumerate() {
                        seen += n;
                        if seen >= half {
                            v[ch] = value as u8;
                            break;
                        }
                    }
                }
                *o = Color32::from_rgba_premultiplied(v[0], v[1], v[2], v[3]);
                add(&mut hist, x - r, -1, &mut count);
            }
        });
    out
}

/// Summed-area table of `f` over the buffer, (w+1)×(h+1).
fn summed(w: usize, h: usize, f: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut s = vec![0.0; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0;
        for x in 0..w {
            row += f(y * w + x);
            s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
        }
    }
    s
}

/// Brush-stroke flat areas with crisp edges (Kuwahara): each pixel takes the
/// average of whichever of its four `radius` quadrants varies least.
pub fn oil_paint(src: &[Color32], w: usize, h: usize, radius: u32) -> Vec<Color32> {
    let r = radius.max(1) as i32;
    let px: Vec<[f32; 4]> = src.iter().map(|&c| f4(c)).collect();
    let l: Vec<f64> = src.iter().map(|&c| luma(c) as f64).collect();
    let sl = summed(w, h, |i| l[i]);
    let sl2 = summed(w, h, |i| l[i] * l[i]);
    let sc: Vec<Vec<f64>> = (0..4)
        .map(|ch| summed(w, h, |i| px[i][ch] as f64))
        .collect();
    let rect = |s: &[f64], x0: i32, y0: i32, x1: i32, y1: i32| {
        // Inclusive box, clamped.
        let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
        let (x1, y1) = (
            (x1 + 1).min(w as i32) as usize,
            (y1 + 1).min(h as i32) as usize,
        );
        let sw = w + 1;
        s[y1 * sw + x1] - s[y0 * sw + x1] - s[y1 * sw + x0] + s[y0 * sw + x0]
    };
    per_pixel(w, h, |x, y| {
        let (x, y) = (x as i32, y as i32);
        let mut best = (f64::MAX, [0.0f32; 4]);
        for (qx, qy) in [(-r, -r), (0, -r), (-r, 0), (0, 0)] {
            let (x0, y0, x1, y1) = (x + qx, y + qy, x + qx + r, y + qy + r);
            let n = ((x1.min(w as i32 - 1) - x0.max(0) + 1)
                * (y1.min(h as i32 - 1) - y0.max(0) + 1))
                .max(1) as f64;
            let mean = rect(&sl, x0, y0, x1, y1) / n;
            let var = rect(&sl2, x0, y0, x1, y1) / n - mean * mean;
            if var < best.0 {
                let c = [0, 1, 2, 3].map(|ch| (rect(&sc[ch], x0, y0, x1, y1) / n) as f32);
                best = (var, c);
            }
        }
        c4(best.1)
    })
}

/// Darker (or, with a negative `amount`, lighter) towards the canvas's
/// edges, starting `size` of the way out (0 centre, 1 edge).
pub fn vignette(
    src: &[Color32],
    w: usize,
    h: usize,
    origin: (i32, i32),
    amount: f32,
    size: f32,
    frame: Frame,
) -> Vec<Color32> {
    per_pixel(w, h, |x, y| {
        let c = src[y * w + x];
        if c.a() == 0 {
            return c;
        }
        let (px, py) = (
            origin.0 as f32 + x as f32 + 0.5,
            origin.1 as f32 + y as f32 + 0.5,
        );
        let ex = (px - frame.centre[0]) / frame.half[0].max(1.0);
        let ey = (py - frame.centre[1]) / frame.half[1].max(1.0);
        let e = (ex * ex + ey * ey).sqrt() / std::f32::consts::SQRT_2;
        let t = ((e - size) / (1.0 - size).max(0.05)).clamp(0.0, 1.0);
        let k = t * t * (3.0 - 2.0 * t) * amount.abs();
        let [r, g, b, a] = c.unmultiplied();
        let to = if amount >= 0.0 { 0.0 } else { 255.0 };
        let m = |v: u8| (v as f32 + (to - v as f32) * k).round() as u8;
        Color32::from_rgba_unmultiplied(m(r), m(g), m(b), a)
    })
}

/// Most a zoom or spin blur reaches, pixels (see `filters::MAX_REACH`).
// Three resampling passes each read a pixel further.
const BLUR_REACH: f32 = (crate::canvas::filters::MAX_REACH - 3) as f32;

/// A blur along a path through each pixel as three passes of four
/// samples: `path(x, y, t, dt)` gives where the path through buffer pixel
/// `(x, y)` is at `t`, `t + dt`, `t + 2dt` and `t + 3dt` (`t` 0..=1 of
/// its length), or `None` when it's under a pixel long. The 64 evenly
/// spaced points of the path are the sums of one step from each pass
/// (`t = (a + 4b + 16c) / 63`), which holds exactly when the paths
/// compose (rotations about a point do, and so do scalings spaced
/// geometrically), so the result is the 64-point average for a fifth of
/// the samples, most of them close by. `centred` paths run from half their
/// length back (`t` from −0.5).
pub(crate) fn path_blur(
    src: &[Color32],
    w: usize,
    h: usize,
    centred: bool,
    path: impl Fn(f32, f32, f32, f32) -> Option<[(f32, f32); 4]> + Sync,
) -> Vec<Color32> {
    let mut buf = src.to_vec();
    for pass in 0..3 {
        let step = (1 << (2 * pass)) as f32 / 63.0;
        let back = if centred && pass == 0 { -0.5 } else { 0.0 };
        let from = &buf;
        buf = per_pixel(w, h, |x, y| {
            let Some(at) = path(x as f32, y as f32, back, step) else {
                return from[y * w + x];
            };
            // Under a third of a pixel from end to end: nothing to do.
            let ((ax, ay), (ex, ey)) = (at[0], at[3]);
            if (ex - ax).abs() + (ey - ay).abs() < 0.3 {
                return from[y * w + x];
            }
            let mut sum = [0u32; 4];
            for (sx, sy) in at {
                let s = sample_fixed(from, w, h, sx, sy);
                for k in 0..4 {
                    sum[k] += s[k];
                }
            }
            // Four samples of 65536ths: round to the nearest level.
            let [r, g, b, a] = sum.map(|v| ((v + (1 << 17)) >> 18).min(255) as u8);
            Color32::from_rgba_premultiplied(r, g, b, a)
        });
    }
    buf
}

/// Rushing towards the canvas's centre: each pixel averages the line
/// towards the centre, `amount` of its distance long (at most
/// `BLUR_REACH`).
pub fn zoom_blur(
    src: &[Color32],
    w: usize,
    h: usize,
    origin: (i32, i32),
    amount: f32,
    frame: Frame,
) -> Vec<Color32> {
    // Buffer coordinates of the centre.
    let (cx, cy) = (
        frame.centre[0] - origin.0 as f32,
        frame.centre[1] - origin.1 as f32,
    );
    path_blur(src, w, h, false, |x, y, t, dt| {
        let (vx, vy) = (x - cx, y - cy);
        let dist = (vx * vx + vy * vy).sqrt();
        let shrink = (amount.min(BLUR_REACH / dist.max(1e-3))).clamp(0.0, 0.97);
        if dist * shrink < 0.5 {
            return None;
        }
        // Scaling towards the centre by (1 − shrink)^t: spaced
        // geometrically, so steps add.
        let l = (1.0 - shrink).log2();
        let (mut k, r) = ((t * l).exp2(), (dt * l).exp2());
        Some(std::array::from_fn(|_| {
            let p = (cx + vx * k, cy + vy * k);
            k *= r;
            p
        }))
    })
}

/// Turning around the canvas's centre: each pixel averages the arc of
/// `angle` degrees through it (at most `BLUR_REACH` long).
pub fn spin_blur(
    src: &[Color32],
    w: usize,
    h: usize,
    origin: (i32, i32),
    angle: f32,
    frame: Frame,
) -> Vec<Color32> {
    let (cx, cy) = (
        frame.centre[0] - origin.0 as f32,
        frame.centre[1] - origin.1 as f32,
    );
    let angle = angle.to_radians();
    path_blur(src, w, h, true, |x, y, t, dt| {
        let (vx, vy) = (x - cx, y - cy);
        let dist = (vx * vx + vy * vy).sqrt();
        let arc = (angle * dist).min(BLUR_REACH);
        if arc < 0.5 {
            return None;
        }
        // The span depends only on the distance, which turning keeps, so
        // the passes compose. Turn to the first point, then step.
        let span = arc / dist;
        let (s, c) = (span * t).sin_cos();
        let (ds, dc) = (span * dt).sin_cos();
        let (mut ux, mut uy) = (vx * c - vy * s, vx * s + vy * c);
        Some(std::array::from_fn(|_| {
            let p = (cx + ux, cy + uy);
            (ux, uy) = (ux * dc - uy * ds, ux * ds + uy * dc);
            p
        }))
    })
}

/// The 8×8 Bayer matrix, thresholds 0..64.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Down to `levels` per channel with an ordered (Bayer) pattern pinned to
/// the canvas, like old screens and printers: gradients stay gradients.
pub fn dither(src: &[Color32], w: usize, h: usize, origin: (i32, i32), levels: u8) -> Vec<Color32> {
    let steps = (levels.max(2) - 1) as f32;
    per_pixel(w, h, |x, y| {
        let c = src[y * w + x];
        if c.a() == 0 {
            return c;
        }
        let (gx, gy) = (
            (origin.0 + x as i32).rem_euclid(8) as usize,
            (origin.1 + y as i32).rem_euclid(8) as usize,
        );
        let t = (BAYER[gy][gx] as f32 + 0.5) / 64.0;
        let [r, g, b, a] = c.unmultiplied();
        let q = |v: u8| {
            let s = v as f32 / 255.0 * steps;
            let level = if s - s.floor() > t {
                s.ceil()
            } else {
                s.floor()
            };
            (level / steps * 255.0).round() as u8
        };
        Color32::from_rgba_unmultiplied(q(r), q(g), q(b), a)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(c: Color32, n: usize) -> Vec<Color32> {
        vec![c; n]
    }

    /// A white square on transparent in a `n`×`n` buffer.
    fn square(n: usize, from: usize, to: usize) -> Vec<Color32> {
        (0..n * n)
            .map(|i| {
                let (x, y) = (i % n, i / n);
                if (from..to).contains(&x) && (from..to).contains(&y) {
                    Color32::WHITE
                } else {
                    Color32::TRANSPARENT
                }
            })
            .collect()
    }

    #[test]
    fn glow_spreads_light_past_the_bright_parts() {
        let src = square(32, 12, 20);
        let out = glow(&src, 32, 32, 3.0, 1.0, 0.5);
        assert_eq!(out[16 * 32 + 16], Color32::WHITE, "the bright part stays");
        assert!(out[16 * 32 + 22].a() > 20, "light past its edge");
        assert_eq!(out[0], Color32::TRANSPARENT, "not far away");
        // Dark paint doesn't glow.
        let dark = solid(Color32::from_rgb(20, 20, 20), 16);
        assert_eq!(glow(&dark, 4, 4, 2.0, 1.0, 0.5), dark);
    }

    #[test]
    fn chromatic_aberration_leaves_the_centre_and_splits_the_edges() {
        let src = square(33, 20, 26);
        let frame = Frame::of_canvas(33, 33);
        let out = chromatic_aberration(&src, 33, 33, (0, 0), 3.0, frame);
        // At the middle nothing moves.
        let flat = solid(Color32::from_rgb(100, 150, 200), 9);
        let centred = chromatic_aberration(&flat, 3, 3, (15, 15), 3.0, frame);
        assert_eq!(centred[4], flat[4]);
        // Off centre, red and blue land in different places: the square's
        // edge shows a colour fringe.
        let fringe = (18..28).any(|x| {
            let c = out[23 * 33 + x];
            c.r() != c.b()
        });
        assert!(fringe);
    }

    #[test]
    fn halftone_dots_grow_with_darkness() {
        let ink = |grey: u8| {
            let src = solid(Color32::from_gray(grey), 32 * 32);
            let out = halftone(&src, 32, 32, (0, 0), 8.0, 0.0, false);
            out.iter().map(|c| 255 - c.r() as u32).sum::<u32>()
        };
        assert!(
            ink(40) > ink(128) && ink(128) > ink(220),
            "{} {} {}",
            ink(40),
            ink(128),
            ink(220)
        );
        assert_eq!(ink(255), 0, "white stays paper");
        // Transparency stays.
        let clear = solid(Color32::TRANSPARENT, 16);
        assert_eq!(halftone(&clear, 4, 4, (0, 0), 4.0, 45.0, true), clear);
    }

    #[test]
    fn emboss_and_edges_find_the_square() {
        let mut src = solid(Color32::BLACK, 16 * 16);
        for y in 4..12 {
            for x in 4..12 {
                src[y * 16 + x] = Color32::WHITE;
            }
        }
        let e = find_edges(&src, 16, 16);
        assert!(e[8 * 16 + 4].r() < 60, "dark on the edge");
        assert_eq!(e[8 * 16 + 8], Color32::WHITE, "white inside");
        assert_eq!(e[0], Color32::WHITE, "white outside");
        let m = emboss(&src, 16, 16, 0.0, 1.0);
        assert!(
            m[8 * 16 + 3].r() > 150 && m[8 * 16 + 11].r() < 100,
            "lit on one side"
        );
        assert_eq!(m[0].r(), 128, "flat is mid grey");
    }

    #[test]
    fn clouds_are_repeatable_smooth_and_pinned_to_the_canvas() {
        let a = clouds(16, 16, (40, 40), 32.0, 4);
        assert_eq!(a, clouds(16, 16, (40, 40), 32.0, 4));
        // The same canvas point from a different buffer.
        let b = clouds(8, 8, (44, 44), 32.0, 4);
        assert_eq!(a[4 * 16 + 4], b[0]);
        let spread =
            a.iter().map(|c| c.r()).max().unwrap() - a.iter().map(|c| c.r()).min().unwrap();
        assert!(spread > 5, "not flat");
        assert!(
            a.windows(2).all(|p| p[0].r().abs_diff(p[1].r()) < 40),
            "smooth"
        );
    }

    #[test]
    fn median_removes_specks_and_keeps_edges() {
        let mut src = solid(Color32::BLACK, 15 * 15);
        src[7 * 15 + 7] = Color32::WHITE;
        let out = median(&src, 15, 15, 1);
        assert_eq!(out[7 * 15 + 7], Color32::BLACK, "speck gone");
        let half: Vec<Color32> = (0..15 * 15)
            .map(|i| {
                if i % 15 < 7 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                }
            })
            .collect();
        assert_eq!(median(&half, 15, 15, 2), half, "a straight edge stays");
    }

    #[test]
    fn oil_paint_flattens_noise_and_keeps_edges() {
        let src: Vec<Color32> = (0..24 * 24)
            .map(|i| {
                let base = if i % 24 < 12 { 60 } else { 200 };
                let jitter = ((i * 7919) % 21) as u8;
                Color32::from_gray(base + jitter)
            })
            .collect();
        let out = oil_paint(&src, 24, 24, 3);
        let row: Vec<u8> = (0..24).map(|x| out[12 * 24 + x].r()).collect();
        assert!(row[..10].iter().all(|&v| v < 90), "{row:?}");
        assert!(row[14..].iter().all(|&v| v > 190), "{row:?}");
        let spread = |r: &[u8]| r.iter().max().unwrap() - r.iter().min().unwrap();
        let before: Vec<u8> = (0..10).map(|x| src[12 * 24 + x].r()).collect();
        assert!(spread(&row[..10]) < spread(&before), "flatter");
    }

    #[test]
    fn vignette_darkens_the_corners_only() {
        let src = solid(Color32::from_gray(200), 20 * 20);
        let out = vignette(&src, 20, 20, (0, 0), 1.0, 0.3, Frame::of_canvas(20, 20));
        assert_eq!(out[10 * 20 + 10], src[0], "the middle");
        assert!(out[0].r() < 60, "a corner: {:?}", out[0]);
        let light = vignette(&src, 20, 20, (0, 0), -1.0, 0.3, Frame::of_canvas(20, 20));
        assert!(light[0].r() > 240);
    }

    /// The blurs as the plain `n`-point average along each pixel's path.
    fn brute(
        src: &[Color32],
        w: usize,
        h: usize,
        zoom: bool,
        amount: f32,
        c: (f32, f32),
        n: usize,
    ) -> Vec<Color32> {
        per_pixel(w, h, |x, y| {
            let (vx, vy) = (x as f32 - c.0, y as f32 - c.1);
            let dist = (vx * vx + vy * vy).sqrt().max(1e-3);
            let mut sum = [0.0f32; 4];
            for i in 0..n {
                let t = i as f32 / (n - 1) as f32;
                let (sx, sy) = if zoom {
                    let shrink = amount.min(BLUR_REACH / dist).min(0.97);
                    let k = (1.0 - shrink).powf(t);
                    (c.0 + vx * k, c.1 + vy * k)
                } else {
                    let a = (amount.to_radians() * dist).min(BLUR_REACH) / dist * (t - 0.5);
                    let (s, co) = a.sin_cos();
                    (c.0 + vx * co - vy * s, c.1 + vx * s + vy * co)
                };
                let p = sample(src, w, h, sx, sy);
                for k in 0..4 {
                    sum[k] += p[k];
                }
            }
            c4(sum.map(|v| v / n as f32))
        })
    }

    #[test]
    fn zoom_and_spin_blur_stay_close_to_the_true_average() {
        let (w, h) = (160, 120);
        let src: Vec<Color32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let v = if (x / 8 + y / 8) % 2 == 0 { 230 } else { 20 };
                Color32::from_rgb(v, (x * 255 / w) as u8, (y * 2) as u8)
            })
            .collect();
        let frame = Frame::of_canvas(w, h);
        let c = (frame.centre[0], frame.centre[1]);
        for (zoom, amount) in [(true, 0.3), (true, 0.9), (false, 20.0), (false, 90.0)] {
            let fast = if zoom {
                zoom_blur(&src, w, h, (0, 0), amount, frame)
            } else {
                spin_blur(&src, w, h, (0, 0), amount, frame)
            };
            // Against the (nearly) continuous average: within two levels
            // (three resamplings soften a little more than one; on this
            // hard checkerboard the plain 64-point one is within half).
            let truth = brute(&src, w, h, zoom, amount, c, 1024);
            let plain = brute(&src, w, h, zoom, amount, c, 64);
            // Only where the whole path stays on the canvas: off it, both
            // make up what's there (clamping to the edge), differently.
            let inside = |i: usize| {
                let (dx, dy) = ((i % w) as f32 - c.0, (i / w) as f32 - c.1);
                (dx * dx + dy * dy).sqrt() < c.1 - 2.0
            };
            let mean = |got: &[Color32]| {
                let (mut total, mut n) = (0, 0);
                for (i, (a, b)) in got.iter().zip(&truth).enumerate() {
                    if inside(i) {
                        n += 4;
                        total += (0..4)
                            .map(|k| (a.to_array()[k] as i32 - b.to_array()[k] as i32).abs())
                            .sum::<i32>();
                    }
                }
                total as f32 / n as f32
            };
            let (fast, plain) = (mean(&fast), mean(&plain));
            assert!(
                fast < 2.0,
                "zoom {zoom} {amount}: off by {fast} on average (64 points: {plain})"
            );
        }
    }

    #[test]
    fn zoom_and_spin_blur_keep_the_centre_and_smear_outside() {
        let src: Vec<Color32> = (0..41 * 41)
            .map(|i| {
                if (i % 41 / 3) % 2 == 0 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                }
            })
            .collect();
        let frame = Frame::of_canvas(41, 41);
        let z = zoom_blur(&src, 41, 41, (0, 0), 0.5, frame);
        assert_eq!(z[20 * 41 + 20], src[20 * 41 + 20], "the centre");
        let s = spin_blur(&src, 41, 41, (0, 0), 30.0, frame);
        // Vertical stripes: far above the centre a spin smears across them.
        let greys = |v: &[Color32]| {
            (0..41)
                .filter(|&x| (40..216).contains(&v[2 * 41 + x].r()))
                .count()
        };
        assert!(greys(&s) > greys(&src) + 10, "spun");
        // A flat colour stays flat.
        let flat = solid(Color32::from_rgb(10, 100, 200), 41 * 41);
        assert_eq!(zoom_blur(&flat, 41, 41, (0, 0), 0.8, frame), flat);
        assert_eq!(spin_blur(&flat, 41, 41, (0, 0), 90.0, frame), flat);
    }

    #[test]
    fn dither_uses_only_its_levels_and_keeps_the_average() {
        let src = solid(Color32::from_gray(100), 64);
        let out = dither(&src, 8, 8, (0, 0), 2);
        assert!(out.iter().all(|c| c.r() == 0 || c.r() == 255));
        let mean = out.iter().map(|c| c.r() as f32).sum::<f32>() / 64.0;
        assert!((mean - 100.0).abs() < 10.0, "{mean}");
    }
}
