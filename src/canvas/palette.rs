//! Colour palettes: extract the main colours of an image (k-means in
//! Oklab, where distances match how different colours look) and recolour
//! pixels to the nearest palette colour, optionally with ordered dithering.

use crate::canvas::blend::Unmultiply;
use eframe::egui::Color32;
use rayon::prelude::*;

/// A colour in Oklab (L, a, b).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lab(pub [f32; 3]);

fn srgb_to_linear(v: u8) -> f32 {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        std::array::from_fn(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
    })[v as usize]
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0 + 0.5) as u8
}

impl Lab {
    /// From an opaque (unmultiplied) sRGB colour.
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        let (r, g, b) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
        let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        Lab([
            0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        ])
    }

    pub fn to_rgb(self) -> [u8; 3] {
        self.to_linear().map(linear_to_srgb)
    }

    /// Linear sRGB, unclamped (outside 0..1 when out of gamut).
    fn to_linear(self) -> [f32; 3] {
        let [lo, a, b] = self.0;
        let l = (lo + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
        let m = (lo - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
        let s = (lo - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
        [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ]
    }

    /// Lightness, chroma and hue (radians).
    pub fn lch(self) -> (f32, f32, f32) {
        let [l, a, b] = self.0;
        (l, a.hypot(b), b.atan2(a))
    }

    /// The colour of lightness `l` and hue `h` with chroma `c`, or as
    /// much of it as sRGB can show.
    pub fn from_lch_in_gamut(l: f32, c: f32, h: f32) -> Self {
        let at = |c: f32| Lab([l, c * h.cos(), c * h.sin()]);
        let fits = |lab: Lab| lab.to_linear().iter().all(|v| (-1e-4..=1.0001).contains(v));
        if fits(at(c)) {
            return at(c);
        }
        let (mut lo, mut hi) = (0.0, c);
        for _ in 0..16 {
            let mid = (lo + hi) / 2.0;
            if fits(at(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        at(lo)
    }

    fn to_color(self) -> Color32 {
        let [r, g, b] = self.to_rgb();
        Color32::from_rgb(r, g, b)
    }

    #[inline]
    fn dist2(self, o: Lab) -> f32 {
        let d = [self.0[0] - o.0[0], self.0[1] - o.0[1], self.0[2] - o.0[2]];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    }
}

pub fn lab_of(c: Color32) -> Lab {
    let [r, g, b, _] = crate::canvas::blend::unmultiply(c);
    Lab::from_rgb(r, g, b)
}

/// The `n` main colours of `pixels` (premultiplied; mostly transparent
/// pixels are ignored), darkest first.
pub fn extract_palette(pixels: &[Color32], n: usize) -> Vec<Color32> {
    let n = n.clamp(1, 256);
    // Enough samples for stable clusters, spread evenly over the image.
    let visible: Vec<Color32> = pixels.iter().copied().filter(|p| p.a() >= 128).collect();
    if visible.is_empty() {
        return Vec::new();
    }
    let step = (visible.len() / 60_000).max(1);
    let samples: Vec<Lab> = visible.iter().step_by(step).map(|&c| lab_of(c)).collect();

    // k-means++ seeding, deterministic: start from the mean, then keep
    // adding the sample farthest from every centre so far.
    let mean = samples.iter().fold([0.0f32; 3], |acc, l| {
        [acc[0] + l.0[0], acc[1] + l.0[1], acc[2] + l.0[2]]
    });
    let count = samples.len() as f32;
    let mean = Lab([mean[0] / count, mean[1] / count, mean[2] / count]);
    let mut centres = vec![samples[nearest_index(&samples, mean)]];
    let mut nearest: Vec<f32> = samples.iter().map(|s| s.dist2(centres[0])).collect();
    while centres.len() < n {
        let (i, &d) = nearest
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap_or((0, &0.0));
        if d <= 1e-7 {
            break; // Fewer distinct colours than asked for.
        }
        let c = samples[i];
        centres.push(c);
        for (s, nd) in samples.iter().zip(nearest.iter_mut()) {
            *nd = nd.min(s.dist2(c));
        }
    }

    for _ in 0..16 {
        let sums = samples
            .par_iter()
            .fold(
                || vec![([0.0f32; 3], 0u32); centres.len()],
                |mut acc, s| {
                    let k = nearest_index(&centres, *s);
                    let e = &mut acc[k];
                    e.0[0] += s.0[0];
                    e.0[1] += s.0[1];
                    e.0[2] += s.0[2];
                    e.1 += 1;
                    acc
                },
            )
            .reduce(
                || vec![([0.0f32; 3], 0u32); centres.len()],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        x.0[0] += y.0[0];
                        x.0[1] += y.0[1];
                        x.0[2] += y.0[2];
                        x.1 += y.1;
                    }
                    a
                },
            );
        let mut moved = 0.0f32;
        for (c, (sum, n)) in centres.iter_mut().zip(sums) {
            if n > 0 {
                let new = Lab([sum[0] / n as f32, sum[1] / n as f32, sum[2] / n as f32]);
                moved = moved.max(c.dist2(new));
                *c = new;
            }
        }
        if moved < 1e-8 {
            break;
        }
    }
    // Drop centres no sample ended up nearest to.
    let mut used = vec![false; centres.len()];
    for s in &samples {
        used[nearest_index(&centres, *s)] = true;
    }
    let mut centres: Vec<Lab> = centres
        .into_iter()
        .zip(used)
        .filter(|(_, u)| *u)
        .map(|(c, _)| c)
        .collect();
    centres.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
    centres.into_iter().map(Lab::to_color).collect()
}

/// What to sort colours by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// Dark to light.
    Lightness,
    /// The greys (dark to light), then round the colour wheel from red.
    Hue,
    /// Greyest to most vivid.
    Chroma,
}

/// Below this chroma a colour reads as a grey.
const GREY_CHROMA: f32 = 0.035;
/// Where the wheel starts: a little before Oklab's hue of red (about
/// 29°), so reds come first.
const RED_HUE: f32 = 0.35;

/// How far round the wheel from red `h` is (0..2π).
fn from_red(h: f32) -> f32 {
    (h - RED_HUE).rem_euclid(std::f32::consts::TAU)
}

/// The order to put `colors` in, sorted by `key`: indices into `colors`.
pub fn sort_order(colors: &[Color32], key: SortKey) -> Vec<usize> {
    let lch: Vec<_> = colors.iter().map(|&c| lab_of(c).lch()).collect();
    let mut order: Vec<usize> = (0..colors.len()).collect();
    order.sort_by(|&i, &j| {
        let ((l1, c1, h1), (l2, c2, h2)) = (lch[i], lch[j]);
        match key {
            SortKey::Lightness => l1.total_cmp(&l2),
            SortKey::Chroma => {
                // Greys' chroma is rounding noise: they tie, dark to light.
                let c = |c: f32| (c * 1000.0).round();
                c(c1).total_cmp(&c(c2)).then(l1.total_cmp(&l2))
            }
            SortKey::Hue => {
                let (g1, g2) = (c1 < GREY_CHROMA, c2 < GREY_CHROMA);
                g2.cmp(&g1).then_with(|| {
                    if g1 {
                        l1.total_cmp(&l2)
                    } else {
                        from_red(h1)
                            .total_cmp(&from_red(h2))
                            .then(l1.total_cmp(&l2))
                    }
                })
            }
        }
    });
    order
}

/// `colors` in families of a hue each, as indices: the greys first, then
/// round the wheel from red; each family dark to light. A family ends at
/// a gap in hue, or once it spans too much of the wheel to read as one.
pub fn hue_families(colors: &[Color32]) -> Vec<Vec<usize>> {
    const GAP: f32 = 0.35; // About 20°.
    const SPAN: f32 = 0.9; // About 50°.
    let lch: Vec<_> = colors.iter().map(|&c| lab_of(c).lch()).collect();
    let (greys, mut hued): (Vec<usize>, Vec<usize>) =
        (0..colors.len()).partition(|&i| lch[i].1 < GREY_CHROMA);
    let mut families = Vec::new();
    if !greys.is_empty() {
        families.push(greys);
    }
    hued.sort_by(|&i, &j| from_red(lch[i].2).total_cmp(&from_red(lch[j].2)));
    // Start after the widest gap, so no family wraps round past red.
    let hue = |k: usize| from_red(lch[hued[k % hued.len()]].2);
    let start = (0..hued.len())
        .max_by(|&a, &b| {
            let gap = |k: usize| (hue(k + 1) - hue(k)).rem_euclid(std::f32::consts::TAU);
            gap(a).total_cmp(&gap(b))
        })
        .map_or(0, |k| k + 1);
    let mut family: Vec<usize> = Vec::new();
    let mut first = 0.0;
    for k in start..start + hued.len() {
        let h = hue(k);
        if let Some(&last) = family.last() {
            let prev = from_red(lch[last].2);
            let turn = |a: f32, b: f32| (b - a).rem_euclid(std::f32::consts::TAU);
            if turn(prev, h) > GAP || turn(first, h) > SPAN {
                families.push(std::mem::take(&mut family));
            }
        }
        if family.is_empty() {
            first = h;
        }
        family.push(hued[k % hued.len()]);
    }
    if !family.is_empty() {
        families.push(family);
    }
    // Greys stay first; the rest round the wheel from red.
    let grey_first = usize::from(families.first().is_some_and(|f| lch[f[0]].1 < GREY_CHROMA));
    let mean_hue = |f: &Vec<usize>| {
        let (x, y) = f.iter().fold((0.0, 0.0), |(x, y), &i| {
            (x + lch[i].2.cos(), y + lch[i].2.sin())
        });
        from_red(y.atan2(x))
    };
    families[grey_first..].sort_by(|a, b| mean_hue(a).total_cmp(&mean_hue(b)));
    for f in &mut families {
        f.sort_by(|&i, &j| lch[i].0.total_cmp(&lch[j].0));
    }
    families
}

/// `n` colours from `a` to `b`, both included, evenly spaced in Oklab
/// (so the steps look even).
pub fn ramp(a: Color32, b: Color32, n: usize) -> Vec<Color32> {
    let (a, b) = (lab_of(a).0, lab_of(b).0);
    let last = n.saturating_sub(1).max(1) as f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / last;
            Lab(std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)).to_color()
        })
        .collect()
}

/// A grid of `rows` rows of `cols` colours between four corners (top-left,
/// top-right, bottom-left, bottom-right), mixed evenly in Oklab: Clip
/// Studio's intermediate colours. Row after row.
pub fn grid(corners: [Color32; 4], cols: usize, rows: usize) -> Vec<Color32> {
    let [tl, tr, bl, br] = corners.map(|c| lab_of(c).0);
    let at = |n: usize, i: usize| i as f32 / n.saturating_sub(1).max(1) as f32;
    (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (at(cols, c), at(rows, r))))
        .map(|(x, y)| {
            Lab(std::array::from_fn(|k| {
                let top = tl[k] + (tr[k] - tl[k]) * x;
                let bottom = bl[k] + (br[k] - bl[k]) * x;
                top + (bottom - top) * y
            }))
            .to_color()
        })
        .collect()
}

/// Colours around `c`: `rows` rows from lighter to darker (by up to
/// `light` in Oklab lightness either way), and along each, `cols` hues
/// turning by up to `hue` radians either way, with `c` in the middle;
/// chroma kept where the screen can show it. Clip Studio's approximate
/// colours.
pub fn variations(c: Color32, cols: usize, rows: usize, hue: f32, light: f32) -> Vec<Color32> {
    let (l, chroma, h) = lab_of(c).lch();
    // -1..=1 across `n` steps (0 for one).
    let step = |n: usize, i: usize| {
        if n <= 1 {
            0.0
        } else {
            i as f32 / (n - 1) as f32 * 2.0 - 1.0
        }
    };
    (0..rows)
        .flat_map(|r| (0..cols).map(move |k| (step(cols, k), step(rows, r))))
        .map(|(x, y)| {
            let li = (l - y * light).clamp(0.0, 1.0);
            Lab::from_lch_in_gamut(li, chroma, h + x * hue).to_color()
        })
        .collect()
}

/// Oklab hues the shadows and lights of [`shades`] turn toward: a blue
/// and a yellow.
const SHADOW_HUE: f32 = -1.7;
const LIGHT_HUE: f32 = 1.75;

/// `n` shades of `c`, from near black to near white, with `c` itself at
/// the step nearest its lightness. Chroma eases off away from `c`, and
/// the hue turns by up to `hue_shift` radians at the ends: toward blue in
/// the shadows and yellow in the lights, as painters shift them.
pub fn shades(c: Color32, n: usize, hue_shift: f32) -> Vec<Color32> {
    if n == 0 {
        return Vec::new();
    }
    let (l, chroma, h) = lab_of(c).lch();
    let (darkest, lightest) = (0.12f32.min(l), 0.97f32.max(l));
    let last = n.saturating_sub(1).max(1) as f32;
    let at = |i: usize| darkest + (lightest - darkest) * i as f32 / last;
    let own = (0..n)
        .min_by(|&a, &b| (at(a) - l).abs().total_cmp(&(at(b) - l).abs()))
        .unwrap_or(0);
    // Turn `h` toward `target` by at most `by`.
    let toward = |target: f32, by: f32| {
        let d = (target - h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        h + d.signum() * d.abs().min(by)
    };
    (0..n)
        .map(|i| {
            if i == own {
                return Lab::from_lch_in_gamut(l, chroma, h).to_color();
            }
            let li = at(i);
            let (t, hi) = if li < l {
                let t = (l - li) / (l - darkest).max(1e-3);
                (t, toward(SHADOW_HUE, hue_shift * t))
            } else {
                let t = (li - l) / (lightest - l).max(1e-3);
                (t, toward(LIGHT_HUE, hue_shift * t))
            };
            let t = t.clamp(0.0, 1.0);
            Lab::from_lch_in_gamut(li, chroma * (1.0 - 0.55 * t * t), hi).to_color()
        })
        .collect()
}

/// A CIE L*a*b* colour (D50, as Photoshop and Krita keep them) in sRGB.
pub fn cielab_to_rgb(l: f32, a: f32, b: f32) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let f = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.964_22 * f(fx), f(fy), 0.825_21 * f(fz));
    // XYZ (D50) to linear sRGB, Bradford-adapted to D65.
    [
        3.133_856 * x - 1.616_866_7 * y - 0.490_614_6 * z,
        -0.978_768_4 * x + 1.916_141_5 * y + 0.033_454 * z,
        0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z,
    ]
    .map(linear_to_srgb)
}

#[inline]
fn nearest_index(centres: &[Lab], c: Lab) -> usize {
    let mut best = (0, f32::MAX);
    for (i, k) in centres.iter().enumerate() {
        let d = c.dist2(*k);
        if d < best.1 {
            best = (i, d);
        }
    }
    best.0
}

/// 4×4 Bayer thresholds in 0..1.
const BAYER: [f32; 16] = [
    0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0,
];

/// Recolours pixels to a palette.
pub struct Recolor {
    labs: Vec<Lab>,
    colors: Vec<[u8; 3]>,
    dither: bool,
}

impl Recolor {
    pub fn new(palette: &[Color32], dither: bool) -> Option<Self> {
        if palette.is_empty() {
            return None;
        }
        Some(Self {
            labs: palette.iter().map(|&c| lab_of(c)).collect(),
            colors: palette
                .iter()
                .map(|c| {
                    let [r, g, b, _] = c.unmultiplied();
                    [r, g, b]
                })
                .collect(),
            dither,
        })
    }

    /// The nearest palette entry to `c`, the second nearest, and how far
    /// toward the second `c` lies (0..1, for dithering).
    fn nearest_two(&self, c: Color32) -> (u16, u16, f32) {
        let lab = lab_of(c);
        let (mut first, mut second) = ((0, f32::MAX), (0, f32::MAX));
        for (i, k) in self.labs.iter().enumerate() {
            let d = lab.dist2(*k);
            if d < first.1 {
                second = first;
                first = (i, d);
            } else if d < second.1 {
                second = (i, d);
            }
        }
        if second.1 == f32::MAX {
            return (first.0 as u16, first.0 as u16, 0.0);
        }
        let (d1, d2) = (first.1.sqrt(), second.1.sqrt());
        (first.0 as u16, second.0 as u16, d1 / (d1 + d2).max(1e-6))
    }

    /// Pixel `c` (premultiplied) at canvas `(x, y)`, recoloured; its alpha
    /// is kept. `cache` remembers colours already matched: use one per
    /// thread or tile, since neighbouring pixels repeat a lot.
    pub fn apply_cached(&self, c: Color32, x: i32, y: i32, cache: &mut RecolorCache) -> Color32 {
        if c.a() == 0 {
            return c;
        }
        let key = u32::from_le_bytes(c.to_array());
        let slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - CACHE_BITS)) as usize;
        let (first, second, t) = match cache.0[slot] {
            Some((k, a, b, t)) if k == key => (a, b, t),
            _ => {
                let v = self.nearest_two(c);
                cache.0[slot] = Some((key, v.0, v.1, v.2));
                v
            }
        };
        let mut pick = first as usize;
        if self.dither {
            // Mix the two nearest in proportion to how close each is.
            let threshold = (BAYER[((y & 3) * 4 + (x & 3)) as usize] + 0.5) / 16.0;
            if t > threshold {
                pick = second as usize;
            }
        }
        let [r, g, b] = self.colors[pick];
        Color32::from_rgba_unmultiplied(r, g, b, c.a())
    }

    #[cfg(test)]
    pub fn apply(&self, c: Color32, x: i32, y: i32) -> Color32 {
        self.apply_cached(c, x, y, &mut RecolorCache::default())
    }
}

const CACHE_BITS: u32 = 12;

/// Recently matched colours: (colour, nearest, second nearest, mix).
pub struct RecolorCache(Vec<Option<(u32, u16, u16, f32)>>);

impl Default for RecolorCache {
    fn default() -> Self {
        Self(vec![None; 1 << CACHE_BITS])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_round_trips() {
        for c in [[0, 0, 0], [255, 255, 255], [200, 30, 90], [12, 180, 240]] {
            let back = Lab::from_rgb(c[0], c[1], c[2]).to_rgb();
            for i in 0..3 {
                assert!(
                    (back[i] as i32 - c[i] as i32).abs() <= 1,
                    "{c:?} -> {back:?}"
                );
            }
        }
    }

    #[test]
    fn finds_the_colours_an_image_is_made_of() {
        let mut px = vec![Color32::from_rgb(220, 20, 20); 5000];
        px.extend(vec![Color32::from_rgb(20, 20, 220); 3000]);
        px.extend(vec![Color32::from_rgb(240, 240, 240); 2000]);
        px.extend(vec![Color32::TRANSPARENT; 4000]);
        let pal = extract_palette(&px, 3);
        assert_eq!(pal.len(), 3);
        for want in [[220, 20, 20], [20, 20, 220], [240, 240, 240]] {
            assert!(
                pal.iter().any(|c| {
                    let d = (c.r() as i32 - want[0]).abs()
                        + (c.g() as i32 - want[1]).abs()
                        + (c.b() as i32 - want[2]).abs();
                    d <= 6
                }),
                "{want:?} missing from {pal:?}"
            );
        }
        // Asking for more colours than exist gives just the distinct ones.
        assert_eq!(extract_palette(&px, 10).len(), 3);
    }

    #[test]
    fn sorting_and_hue_families() {
        let red = Color32::from_rgb(220, 30, 30);
        let dark_red = Color32::from_rgb(100, 15, 20);
        let orange = Color32::from_rgb(240, 140, 30);
        let blue = Color32::from_rgb(30, 60, 220);
        let grey = Color32::from_rgb(120, 120, 120);
        let colors = [blue, grey, red, Color32::WHITE, dark_red, orange];
        let by = |key| -> Vec<Color32> {
            sort_order(&colors, key)
                .iter()
                .map(|&i| colors[i])
                .collect()
        };
        assert_eq!(by(SortKey::Lightness)[0], dark_red);
        assert_eq!(*by(SortKey::Lightness).last().unwrap(), Color32::WHITE);
        assert_eq!(&by(SortKey::Hue)[..2], &[grey, Color32::WHITE]);
        assert_eq!(by(SortKey::Chroma)[0], grey);
        let families: Vec<Vec<Color32>> = hue_families(&colors)
            .into_iter()
            .map(|f| f.into_iter().map(|i| colors[i]).collect())
            .collect();
        assert_eq!(
            families,
            vec![
                vec![grey, Color32::WHITE],
                vec![dark_red, red],
                vec![orange],
                vec![blue]
            ]
        );
        assert!(hue_families(&[]).is_empty());
    }

    #[test]
    fn ramps_go_end_to_end_and_shades_dark_to_light() {
        let (a, b) = (
            Color32::from_rgb(200, 40, 40),
            Color32::from_rgb(40, 40, 200),
        );
        let r = ramp(a, b, 5);
        assert_eq!(r.len(), 5);
        let close = |x: Color32, y: Color32| {
            x.r().abs_diff(y.r()) <= 1 && x.g().abs_diff(y.g()) <= 1 && x.b().abs_diff(y.b()) <= 1
        };
        assert!(close(r[0], a) && close(r[4], b), "{r:?}");

        let c = Color32::from_rgb(60, 150, 90);
        for shift in [0.0, 0.5] {
            let s = shades(c, 7, shift);
            assert_eq!(s.len(), 7);
            assert!(s.iter().any(|&x| close(x, c)), "the colour itself: {s:?}");
            let l: Vec<f32> = s.iter().map(|&x| lab_of(x).lch().0).collect();
            assert!(l.windows(2).all(|w| w[0] < w[1]), "dark to light: {l:?}");
        }
        // Black's go up to near white, no step twice.
        let s = shades(Color32::BLACK, 5, 0.3);
        assert_eq!(s[0], Color32::BLACK);
        assert!(s.windows(2).all(|w| w[0] != w[1]), "{s:?}");
        assert_eq!(shades(c, 1, 0.3).len(), 1);
        assert!(shades(c, 0, 0.3).is_empty());
    }

    #[test]
    fn cielab_colours() {
        assert_eq!(cielab_to_rgb(100.0, 0.0, 0.0), [255, 255, 255]);
        assert_eq!(cielab_to_rgb(0.0, 0.0, 0.0), [0, 0, 0]);
        let [r, g, b] = cielab_to_rgb(54.3, 80.8, 69.9);
        assert!(r > 245 && g < 30 && b < 30, "{r} {g} {b}");
    }

    #[test]
    fn recolour_snaps_to_the_palette_and_keeps_alpha() {
        let pal = [Color32::BLACK, Color32::WHITE];
        let r = Recolor::new(&pal, false).unwrap();
        assert_eq!(r.apply(Color32::from_rgb(40, 40, 40), 0, 0), Color32::BLACK);
        assert_eq!(
            r.apply(Color32::from_rgb(220, 220, 220), 0, 0),
            Color32::WHITE
        );
        let half = Color32::from_rgba_unmultiplied(230, 230, 230, 128);
        assert_eq!(r.apply(half, 0, 0).a(), 128);
        // Dithering a mid grey mixes both.
        let d = Recolor::new(&pal, true).unwrap();
        let grey = Color32::from_rgb(128, 128, 128);
        let whites = (0..16)
            .filter(|i| d.apply(grey, i % 4, i / 4) == Color32::WHITE)
            .count();
        assert!((4..=12).contains(&whites), "{whites}");
    }

    #[test]
    fn a_grid_has_its_corners_and_mixes_between_them() {
        let (r, g, b, w) = (
            Color32::from_rgb(255, 0, 0),
            Color32::from_rgb(0, 255, 0),
            Color32::from_rgb(0, 0, 255),
            Color32::WHITE,
        );
        let cells = grid([r, g, b, w], 4, 3);
        assert_eq!(cells.len(), 12);
        let near = |a: Color32, b: Color32| {
            a.to_array()
                .iter()
                .zip(b.to_array())
                .all(|(x, y)| x.abs_diff(y) <= 2)
        };
        assert!(near(cells[0], r) && near(cells[3], g));
        assert!(near(cells[8], b) && near(cells[11], w));
        // Two corners alike: that side is one colour.
        let side = grid([r, r, b, b], 3, 2);
        assert!(near(side[1], r) && near(side[4], b));
    }

    #[test]
    fn variations_have_the_colour_in_the_middle_lighter_above() {
        let c = Color32::from_rgb(180, 90, 60);
        let v = variations(c, 5, 3, 0.4, 0.2);
        assert_eq!(v.len(), 15);
        let mid = v[7];
        assert!(
            mid.to_array()
                .iter()
                .zip(c.to_array())
                .all(|(x, y)| x.abs_diff(y) <= 2)
        );
        let l = |c: Color32| lab_of(c).0[0];
        assert!(l(v[2]) > l(mid) && l(mid) > l(v[12]));
        // The hue turns across a row.
        assert_ne!(v[5], v[9]);
    }
}
