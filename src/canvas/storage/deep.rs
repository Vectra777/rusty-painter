//! Colour depth: a document can keep its pixels at 16 bits per channel or
//! as 32-bit floats as well as 8 bits.
//!
//! Every tile always has its 8-bit pixels, which the display, the 8-bit
//! tools and the tests read. A deeper document's tiles also carry a
//! [`DeepTile`]: the same pixels at full precision. The 8-bit pixels are
//! always that tile rounded ([`DeepTile::narrow`]); tools that work at full
//! precision write the deep pixels and their rounding together, and any
//! other write to a tile's 8-bit pixels drops its deep ones (see
//! `TileCell`), so they can never disagree. A tile without deep pixels in a
//! deep document is exactly its 8-bit pixels, widened.
//!
//! - 16-bit pixels are stored like the 8-bit ones (premultiplied, sRGB
//!   encoded), with 257 steps for each 8-bit one.
//! - 32-bit float pixels are premultiplied linear light, as in Krita's
//!   32-bit float documents, and keep values above 1.

use eframe::egui::Color32;
use eframe::egui::ecolor::{gamma_from_linear, linear_f32_from_gamma_u8, linear_from_gamma};
use std::sync::OnceLock;

/// How many bits a document keeps for each channel.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Depth {
    #[default]
    U8,
    U16,
    F32,
}

impl Depth {
    pub const ALL: [Depth; 3] = [Depth::U8, Depth::U16, Depth::F32];

    pub fn label(self) -> &'static str {
        match self {
            Depth::U8 => "8-bit",
            Depth::U16 => "16-bit",
            Depth::F32 => "32-bit float",
        }
    }

    /// Whether tiles carry deep pixels.
    pub fn is_deep(self) -> bool {
        self != Depth::U8
    }
}

/// A tile's pixels at its document's full depth (row-major, like its 8-bit
/// pixels).
#[derive(Clone, Debug, PartialEq)]
pub enum DeepTile {
    /// Premultiplied, sRGB encoded, 0..=65535.
    U16(Vec<[u16; 4]>),
    /// Premultiplied, linear light.
    F32(Vec<[f32; 4]>),
}

/// 16-bit sRGB-encoded value → linear light, for every value.
fn u16_linear() -> &'static [f32] {
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=u16::MAX)
            .map(|v| linear_from_gamma(v as f32 / 65535.0))
            .collect()
    })
}

#[inline]
fn to_u16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

#[inline]
fn u16_to_u8(v: u16) -> u8 {
    ((v as u32 * 255 + 32767) / 65535) as u8
}

/// `[0, 0, 0, 0]` when the alpha is gone: transparent pixels are all zeros,
/// which tile storage relies on.
#[inline]
fn tidy_u16(p: [u16; 4]) -> [u16; 4] {
    if p[3] == 0 { [0; 4] } else { p }
}

impl DeepTile {
    /// `pixels` at `depth` (`None` for 8-bit, which has no deep pixels).
    pub fn widen(depth: Depth, pixels: &[Color32]) -> Option<Self> {
        match depth {
            Depth::U8 => None,
            Depth::U16 => Some(DeepTile::U16(
                pixels
                    .iter()
                    .map(|c| c.to_array().map(|v| v as u16 * 257))
                    .collect(),
            )),
            Depth::F32 => Some(DeepTile::F32(
                pixels
                    .iter()
                    .map(|c| {
                        [
                            linear_f32_from_gamma_u8(c.r()),
                            linear_f32_from_gamma_u8(c.g()),
                            linear_f32_from_gamma_u8(c.b()),
                            c.a() as f32 / 255.0,
                        ]
                    })
                    .collect(),
            )),
        }
    }

    /// A transparent tile of `len` pixels at `depth`.
    pub fn transparent(depth: Depth, len: usize) -> Option<Self> {
        match depth {
            Depth::U8 => None,
            Depth::U16 => Some(DeepTile::U16(vec![[0; 4]; len])),
            Depth::F32 => Some(DeepTile::F32(vec![[0.0; 4]; len])),
        }
    }

    pub fn depth(&self) -> Depth {
        match self {
            DeepTile::U16(_) => Depth::U16,
            DeepTile::F32(_) => Depth::F32,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            DeepTile::U16(p) => p.len(),
            DeepTile::F32(p) => p.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of pixels held.
    pub fn bytes(&self) -> usize {
        match self {
            DeepTile::U16(p) => p.len() * 8,
            DeepTile::F32(p) => p.len() * 16,
        }
    }

    /// Pixel `i`, premultiplied, in linear light.
    #[inline]
    pub fn linear(&self, i: usize) -> [f32; 4] {
        match self {
            DeepTile::U16(p) => {
                let lut = u16_linear();
                let [r, g, b, a] = p[i];
                [
                    lut[r as usize],
                    lut[g as usize],
                    lut[b as usize],
                    a as f32 / 65535.0,
                ]
            }
            DeepTile::F32(p) => p[i],
        }
    }

    /// Set pixel `i` from premultiplied linear light.
    #[inline]
    pub fn set_linear(&mut self, i: usize, v: [f32; 4]) {
        match self {
            DeepTile::U16(p) => {
                let a = to_u16(v[3]);
                p[i] = tidy_u16([
                    to_u16(gamma_from_linear(v[0].max(0.0))),
                    to_u16(gamma_from_linear(v[1].max(0.0))),
                    to_u16(gamma_from_linear(v[2].max(0.0))),
                    a,
                ]);
            }
            DeepTile::F32(p) => {
                p[i] = if v[3] <= 0.0 {
                    [0.0; 4]
                } else {
                    [v[0].max(0.0), v[1].max(0.0), v[2].max(0.0), v[3].min(1.0)]
                };
            }
        }
    }

    /// Pixel `i`, premultiplied, as stored values (sRGB encoded, 0..1): what
    /// gamma-space documents blend.
    #[inline]
    pub fn gamma(&self, i: usize) -> [f32; 4] {
        match self {
            DeepTile::U16(p) => p[i].map(|v| v as f32 / 65535.0),
            DeepTile::F32(p) => {
                let [r, g, b, a] = p[i];
                [
                    gamma_from_linear(r.max(0.0)),
                    gamma_from_linear(g.max(0.0)),
                    gamma_from_linear(b.max(0.0)),
                    a,
                ]
            }
        }
    }

    /// Set pixel `i` from premultiplied stored values (sRGB encoded, 0..1).
    #[inline]
    pub fn set_gamma(&mut self, i: usize, v: [f32; 4]) {
        match self {
            DeepTile::U16(p) => p[i] = tidy_u16(v.map(to_u16)),
            DeepTile::F32(_) => self.set_linear(
                i,
                [
                    linear_from_gamma(v[0].max(0.0)),
                    linear_from_gamma(v[1].max(0.0)),
                    linear_from_gamma(v[2].max(0.0)),
                    v[3],
                ],
            ),
        }
    }

    /// Pixel `i` rounded to 8 bits.
    #[inline]
    pub fn narrow(&self, i: usize) -> Color32 {
        match self {
            DeepTile::U16(p) => {
                let [r, g, b, a] = p[i].map(u16_to_u8);
                if a == 0 {
                    Color32::TRANSPARENT
                } else {
                    Color32::from_rgba_premultiplied(r, g, b, a)
                }
            }
            DeepTile::F32(p) => {
                let [r, g, b, a] = p[i];
                let a8 = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                if a8 == 0 {
                    return Color32::TRANSPARENT;
                }
                let c = |v: f32| eframe::egui::ecolor::gamma_u8_from_linear_f32(v.clamp(0.0, 1.0));
                Color32::from_rgba_premultiplied(c(r), c(g), c(b), a8)
            }
        }
    }

    /// Every pixel rounded to 8 bits.
    pub fn narrow_all(&self) -> Vec<Color32> {
        (0..self.len()).map(|i| self.narrow(i)).collect()
    }

    /// Whether every pixel is transparent.
    pub fn is_transparent(&self) -> bool {
        match self {
            DeepTile::U16(p) => p.iter().all(|v| v[3] == 0),
            DeepTile::F32(p) => p.iter().all(|v| v[3] <= 0.0),
        }
    }

    /// The pixels as little-endian bytes (see [`DeepTile::from_bytes`]).
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            DeepTile::U16(p) => p.iter().flatten().flat_map(|v| v.to_le_bytes()).collect(),
            DeepTile::F32(p) => p.iter().flatten().flat_map(|v| v.to_le_bytes()).collect(),
        }
    }

    /// `len` pixels at `depth` from [`DeepTile::to_bytes`]'s bytes; `None` if
    /// they don't fit (a damaged file) or for 8-bit.
    pub fn from_bytes(depth: Depth, bytes: &[u8], len: usize) -> Option<Self> {
        match depth {
            Depth::U8 => None,
            Depth::U16 => {
                if Some(bytes.len()) != len.checked_mul(8) {
                    return None;
                }
                let values = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes(*b));
                let flat: Vec<u16> = values.collect();
                Some(DeepTile::U16(
                    flat.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|p| tidy_u16(*p))
                        .collect(),
                ))
            }
            Depth::F32 => {
                if Some(bytes.len()) != len.checked_mul(16) {
                    return None;
                }
                let flat: Vec<f32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .map(|v| if v.is_finite() { v } else { 0.0 })
                    .collect();
                let mut out = DeepTile::F32(vec![[0.0; 4]; len]);
                for (i, p) in flat.as_chunks::<4>().0.iter().enumerate() {
                    out.set_linear(i, *p);
                }
                Some(out)
            }
        }
    }

    /// Copy the `w`×`h` block at `(x0, y0)` of this tile (`side` pixels wide).
    pub fn block(&self, side: usize, (x0, y0, w, h): (usize, usize, usize, usize)) -> Self {
        fn rows<T: Copy>(
            p: &[T],
            side: usize,
            (x0, y0, w, h): (usize, usize, usize, usize),
        ) -> Vec<T> {
            (0..h)
                .flat_map(|r| p[(y0 + r) * side + x0..][..w].iter().copied())
                .collect()
        }
        match self {
            DeepTile::U16(p) => DeepTile::U16(rows(p, side, (x0, y0, w, h))),
            DeepTile::F32(p) => DeepTile::F32(rows(p, side, (x0, y0, w, h))),
        }
    }

    /// Write `block` (`w` pixels wide) into this tile (`side` pixels wide) at
    /// `(x0, y0)`, converting it to this tile's depth if it differs.
    pub fn put_block(&mut self, side: usize, (x0, y0, w): (usize, usize, usize), block: &DeepTile) {
        if block.depth() != self.depth() {
            for i in 0..block.len() {
                let at = (y0 + i / w) * side + x0 + i % w;
                self.set_linear(at, block.linear(i));
            }
            return;
        }
        fn put<T: Copy>(p: &mut [T], b: &[T], side: usize, (x0, y0, w): (usize, usize, usize)) {
            for (r, row) in b.chunks(w).enumerate() {
                p[(y0 + r) * side + x0..][..row.len()].copy_from_slice(row);
            }
        }
        match (self, block) {
            (DeepTile::U16(p), DeepTile::U16(b)) => put(p, b, side, (x0, y0, w)),
            (DeepTile::F32(p), DeepTile::F32(b)) => put(p, b, side, (x0, y0, w)),
            _ => unreachable!("depths compared above"),
        }
    }

    /// The same pixels at `depth` (`None` for 8-bit).
    pub fn convert(&self, depth: Depth) -> Option<Self> {
        if depth == self.depth() {
            return Some(self.clone());
        }
        let mut out = Self::transparent(depth, self.len())?;
        for i in 0..self.len() {
            out.set_linear(i, self.linear(i));
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_8bit_pixel() -> Vec<Color32> {
        let mut out = Vec::new();
        for a in [0u8, 1, 2, 17, 128, 254, 255] {
            for v in 0..=a {
                out.push(Color32::from_rgba_premultiplied(v, a - v, v / 2, a));
            }
        }
        out
    }

    #[test]
    fn widening_and_narrowing_8bit_pixels_gives_them_back() {
        let pixels = every_8bit_pixel();
        for depth in [Depth::U16, Depth::F32] {
            let deep = DeepTile::widen(depth, &pixels).unwrap();
            assert_eq!(deep.narrow_all(), pixels, "{depth:?}");
        }
        assert!(DeepTile::widen(Depth::U8, &pixels).is_none());
    }

    #[test]
    fn linear_and_gamma_round_trip_at_each_depth() {
        let pixels = every_8bit_pixel();
        for depth in [Depth::U16, Depth::F32] {
            let deep = DeepTile::widen(depth, &pixels).unwrap();
            let mut copy = DeepTile::transparent(depth, deep.len()).unwrap();
            for i in 0..deep.len() {
                copy.set_linear(i, deep.linear(i));
            }
            assert_eq!(copy.narrow_all(), pixels, "{depth:?} through linear");
            for i in 0..deep.len() {
                copy.set_gamma(i, deep.gamma(i));
            }
            assert_eq!(copy.narrow_all(), pixels, "{depth:?} through gamma");
        }
    }

    #[test]
    fn deep_pixels_keep_steps_8_bits_lose() {
        // A dark ramp: a few 8-bit steps, many deep ones.
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0 * 0.02).collect();
        for depth in [Depth::U16, Depth::F32] {
            let mut deep = DeepTile::transparent(depth, ramp.len()).unwrap();
            for (i, &v) in ramp.iter().enumerate() {
                deep.set_linear(i, [v, v, v, 1.0]);
            }
            let distinct = |values: Vec<u32>| {
                let mut v = values;
                v.dedup();
                v.len()
            };
            let deep_steps = distinct(
                (0..ramp.len())
                    .map(|i| (deep.linear(i)[0] * 1e7) as u32)
                    .collect(),
            );
            let eight_steps =
                distinct((0..ramp.len()).map(|i| deep.narrow(i).r() as u32).collect());
            assert!(eight_steps <= 41, "{depth:?}: {eight_steps}");
            assert!(deep_steps > 400, "{depth:?}: {deep_steps} vs {eight_steps}");
        }
    }

    #[test]
    fn float_keeps_values_above_one_and_converts_between_depths() {
        let mut deep = DeepTile::transparent(Depth::F32, 2).unwrap();
        deep.set_linear(0, [2.5, 0.5, 0.0, 1.0]);
        assert_eq!(deep.linear(0)[0], 2.5);
        assert_eq!(deep.narrow(0).r(), 255);
        let as_u16 = deep.convert(Depth::U16).unwrap();
        assert_eq!(as_u16.narrow(0), deep.narrow(0));
        assert_eq!(as_u16.narrow(1), Color32::TRANSPARENT);
    }

    #[test]
    fn bytes_and_blocks_round_trip() {
        let pixels = every_8bit_pixel();
        for depth in [Depth::U16, Depth::F32] {
            let deep = DeepTile::widen(depth, &pixels).unwrap();
            let back = DeepTile::from_bytes(depth, &deep.to_bytes(), deep.len()).unwrap();
            assert_eq!(back, deep);
            assert!(DeepTile::from_bytes(depth, &deep.to_bytes()[1..], deep.len()).is_none());
        }
        let side = 4;
        let tile = DeepTile::widen(
            Depth::U16,
            &(0..16u8)
                .map(|v| Color32::from_gray(v * 10))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let block = tile.block(side, (1, 2, 2, 2));
        assert_eq!(
            block.narrow_all(),
            [90, 100, 130, 140].map(Color32::from_gray)
        );
        let mut other = DeepTile::transparent(Depth::F32, 16).unwrap();
        other.put_block(side, (1, 2, 2), &block);
        assert_eq!(other.narrow(9), Color32::from_gray(90));
        assert_eq!(other.narrow(14), Color32::from_gray(140));
        assert_eq!(other.narrow(0), Color32::TRANSPARENT);
    }

    #[test]
    fn transparent_pixels_are_zero() {
        for depth in [Depth::U16, Depth::F32] {
            let mut deep = DeepTile::transparent(depth, 1).unwrap();
            deep.set_linear(0, [0.3, 0.2, 0.1, 0.0]);
            assert!(deep.is_transparent());
            assert_eq!(deep.narrow(0), Color32::TRANSPARENT);
            assert_eq!(deep.linear(0), [0.0; 4]);
        }
    }
}
