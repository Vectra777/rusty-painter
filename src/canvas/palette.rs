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
        let [lo, a, b] = self.0;
        let l = (lo + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
        let m = (lo - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
        let s = (lo - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
        [
            linear_to_srgb(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
            linear_to_srgb(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
            linear_to_srgb(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
        ]
    }

    #[inline]
    fn dist2(self, o: Lab) -> f32 {
        let d = [self.0[0] - o.0[0], self.0[1] - o.0[1], self.0[2] - o.0[2]];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    }
}

fn lab_of(c: Color32) -> Lab {
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
    centres
        .into_iter()
        .map(|l| {
            let [r, g, b] = l.to_rgb();
            Color32::from_rgb(r, g, b)
        })
        .collect()
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
}
