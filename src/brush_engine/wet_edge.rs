//! Watercolour edges: the stroke's paint pulls to its rim (shown as it's
//! painted), as a wash does drying on paper (Clip Studio's "watercolor edge").
//!
//! The stroke's coverage is compared with a blurred copy of itself: in the
//! middle of a wash the two match, and the paint there thins by the edge's
//! strength; near the edge the coverage exceeds its blur, and the paint
//! keeps its full strength. Thin lines, all edge, stay as they were.
//!
//! Round tips do it without the blur (`WetRim`): each dab records how
//! deep inside it every pixel lies, the stroke keeps the deepest, and the
//! paint thins by that depth as the stroke is put on the layer. The blur
//! stays for image tips, whose painted outline isn't their circle.

use eframe::egui::Rgba;

/// Coverage past its blur, times this, is "at the edge" (1 = full paint).
const EDGE_GAIN: f32 = 3.0;

/// Watercolour edges from round dabs' depth: the rim is a band inside
/// each dab from the middle of its soft fade (where a soft tip's paint
/// shows; outside it there's too little paint for a rim to show), as wide
/// as half the fade (and at least `width`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct WetRim {
    /// How much the middle thins (0..0.95).
    pub strength: f32,
    /// The rim's least width, in canvas pixels.
    pub width: f32,
    /// The share of a dab's radius its soft fade takes (1 - hardness).
    pub fade: f32,
}

impl WetRim {
    /// How much a dab leaves at the edge of canvas row `gy`, columns from
    /// `x0`, into `rim` (1 = at a rim, 0 = deep inside), keeping the
    /// deepest any dab reached. `center`, `r` and `orient` are the dab's.
    pub fn record(
        &self,
        center: [f32; 2],
        r: f32,
        orient: [f32; 4],
        gy: usize,
        x0: usize,
        rim: &mut [f32],
    ) {
        let half_fade = r * self.fade * 0.5;
        let band = self.width.max(half_fade).max(1.0);
        let dy = gy as f32 + 0.5 - center[1];
        let [a, b, c, d] = orient;
        for (i, e) in rim.iter_mut().enumerate() {
            let dx = (x0 + i) as f32 + 0.5 - center[0];
            let (u, v) = (a * dx + b * dy, c * dx + d * dy);
            let depth = r - (u * u + v * v).sqrt() - half_fade;
            if depth > 0.0 {
                let t = (depth / band).min(1.0);
                *e = e.min(1.0 - t * t * (3.0 - 2.0 * t));
            }
        }
    }

    /// The share of the paint kept at a pixel whose rim value is `e`.
    #[inline]
    pub fn keep(&self, e: f32) -> f32 {
        1.0 - self.strength * (1.0 - e)
    }
}

/// A tile's coverage with watercolour edges. `patch` is the coverage of the
/// tile and `pad` pixels around it (`side` = tile + 2 × pad, row-major),
/// `radius` the blur's (at most `pad`); returns the tile's new coverage
/// (`tile` × `tile`). `strength` (0..1) is how much the middle thins.
pub fn wet_edge_tile(
    patch: &[f32],
    side: usize,
    pad: usize,
    radius: usize,
    strength: f32,
) -> Vec<f32> {
    let tile = side - 2 * pad;
    let blurred = box_blur(patch, side, radius.min(pad));
    let thin = strength.clamp(0.0, 0.95);
    let mut out = vec![0.0; tile * tile];
    for y in 0..tile {
        for x in 0..tile {
            let i = (y + pad) * side + x + pad;
            let (c, b) = (patch[i], blurred[i]);
            if c <= 0.0 {
                continue;
            }
            let edge = ((c - b) * EDGE_GAIN).clamp(0.0, 1.0);
            out[y * tile + x] = c * (1.0 - thin * (1.0 - edge));
        }
    }
    out
}

/// How a stroke's watercolour edge is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WetEdgeStyle {
    /// The paint pools at the stroke's rim (from its blur, or round dabs'
    /// depth): this app's own.
    #[default]
    Rim,
    /// Procreate's: a function of each pixel's coverage alone, so the soft
    /// band of every dab turns solid and full coverage thins a little. No
    /// blur, so it costs nothing extra; it follows each dab rather than
    /// the whole stroke.
    Ring,
}

/// Coverage `c` with Procreate's wet edge at `strength` (0..1): mixed
/// towards 1 − |sin(4.5c) / 4.5c|, which is 0 at no paint, 1 around
/// c ≈ 0.7 and 0.78 at full paint.
#[inline]
pub fn ring(c: f32, strength: f32) -> f32 {
    if c <= 0.0 {
        return c;
    }
    let x = 4.5 * c;
    let edge = 1.0 - (x.sin() / x).abs();
    c + (edge - c) * strength.clamp(0.0, 1.0)
}

/// Burnt edges (Procreate's): where a stroke is thin, it's laid with
/// another blend mode, so its rim darkens (or lightens) what's under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BurntEdges {
    /// How strongly (0..1).
    pub amount: f32,
    pub mode: crate::canvas::blend_modes::LayerBlend,
}

impl BurntEdges {
    /// A pixel with burnt edges: `normal` is the stroke laid as usual
    /// over `original`, `paint` the stroke's own colour (premultiplied, at
    /// its coverage `c`). The thinner the paint, the more the burnt
    /// blend shows: by (1 − c) × amount.
    #[inline]
    pub fn pixel(&self, normal: Rgba, original: Rgba, paint: Rgba, c: f32) -> Rgba {
        let w = (1.0 - c).abs() * self.amount;
        if w <= 0.0 || c <= 0.0 {
            return normal;
        }
        let burnt = crate::canvas::blend_modes::composite(self.mode, paint, original, 0.0);
        let w = w.min(1.0);
        normal * (1.0 - w) + burnt * w
    }
}

/// A box blur of `radius` over a `side` × `side` grid (separable; outside
/// the grid counts as nothing).
fn box_blur(src: &[f32], side: usize, radius: usize) -> Vec<f32> {
    if radius == 0 {
        return src.to_vec();
    }
    let norm = 1.0 / (2 * radius + 1) as f32;
    let mut across = vec![0.0; src.len()];
    for y in 0..side {
        let row = &src[y * side..(y + 1) * side];
        let mut sum: f32 = row[..radius.min(side)].iter().sum();
        for x in 0..side {
            if x + radius < side {
                sum += row[x + radius];
            }
            across[y * side + x] = sum * norm;
            if x >= radius {
                sum -= row[x - radius];
            }
        }
    }
    let mut out = vec![0.0; src.len()];
    for x in 0..side {
        let mut sum: f32 = (0..radius.min(side)).map(|y| across[y * side + x]).sum();
        for y in 0..side {
            if y + radius < side {
                sum += across[(y + radius) * side + x];
            }
            out[y * side + x] = sum * norm;
            if y >= radius {
                sum -= across[(y - radius) * side + x];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blur_keeps_a_flat_field_flat_inside_and_matches_a_direct_sum() {
        let side = 12;
        let src: Vec<f32> = (0..side * side)
            .map(|i| ((i * 37) % 11) as f32 / 10.0)
            .collect();
        let blurred = box_blur(&src, side, 2);
        for y in 0..side {
            for x in 0..side {
                let mut sum = 0.0;
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                        if (0..side as i32).contains(&sx) && (0..side as i32).contains(&sy) {
                            sum += src[sy as usize * side + sx as usize];
                        }
                    }
                }
                assert!((blurred[y * side + x] - sum / 25.0).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn a_round_dab_thins_inside_its_rim_and_a_line_keeps_its_rim_where_dabs_overlap() {
        let rim = WetRim {
            strength: 0.6,
            width: 4.0,
            fade: 0.5,
        };
        // One dab of radius 20 at (30, 30): a row through its middle.
        let mut row = vec![1.0; 64];
        rim.record([30.0, 30.0], 20.0, [1.0, 0.0, 0.0, 1.0], 29, 0, &mut row);
        assert!((rim.keep(row[29]) - 0.4).abs() < 0.01, "middle {}", row[29]);
        assert!(rim.keep(row[11]) > 0.9, "rim {}", row[11]);
        assert_eq!(row[5], 1.0, "outside");
        // A second dab beside it: the first's rim between them is now
        // inside the second, so it thins (no rim inside a line); the
        // second's own rim keeps its paint.
        rim.record([45.0, 30.0], 20.0, [1.0, 0.0, 0.0, 1.0], 29, 0, &mut row);
        assert!(
            rim.keep(row[49]) < 0.5,
            "the first dab's right rim {}",
            row[49]
        );
        assert!(rim.keep(row[63]) > 0.9, "the second's rim {}", row[63]);
        // Squashed to half its height: a pixel 12 px above the centre is
        // past its rim (12 × 2 > 20).
        let mut col = vec![1.0; 1];
        rim.record([30.0, 30.0], 20.0, [1.0, 0.0, 0.0, 2.0], 17, 29, &mut col);
        assert_eq!(col[0], 1.0);
    }

    #[test]
    fn a_wash_thins_in_the_middle_and_keeps_its_rim() {
        // A disc of full coverage, radius 20, in a 64 px tile.
        let (tile, pad) = (64, 8);
        let side = tile + 2 * pad;
        let patch: Vec<f32> = (0..side * side)
            .map(|i| {
                let (x, y) = ((i % side) as f32 - 40.0, (i / side) as f32 - 40.0);
                if x * x + y * y <= 400.0 { 1.0 } else { 0.0 }
            })
            .collect();
        let out = wet_edge_tile(&patch, side, pad, 6, 0.6);
        let at = |x: usize, y: usize| out[(y - pad) * tile + x - pad];
        let (middle, rim) = (at(40, 40), at(59, 40));
        assert!((middle - 0.4).abs() < 0.01, "middle {middle}");
        assert!(rim > 0.9, "rim {rim}");
        // Nothing appears outside the stroke.
        assert_eq!(at(70, 70), 0.0);
        // No strength: unchanged.
        let same = wet_edge_tile(&patch, side, pad, 6, 0.0);
        for y in 0..tile {
            for x in 0..tile {
                assert_eq!(same[y * tile + x], patch[(y + pad) * side + x + pad]);
            }
        }
    }

    #[test]
    fn the_ring_turns_the_soft_band_solid_and_thins_full_paint() {
        assert_eq!(ring(0.0, 1.0), 0.0);
        assert_eq!(ring(0.4, 0.0), 0.4, "off at strength 0");
        // Around 0.7 the ring peaks; full paint keeps about 0.78.
        assert!(ring(0.7, 1.0) > 0.99);
        assert!((ring(1.0, 1.0) - 0.783).abs() < 0.01);
        assert!(ring(0.05, 1.0) < 0.05, "a faint wisp stays faint");
    }

    #[test]
    fn burnt_edges_show_where_the_paint_is_thin() {
        use crate::canvas::blend_modes::LayerBlend;
        let burnt = BurntEdges {
            amount: 1.0,
            mode: LayerBlend::Multiply,
        };
        let original = Rgba::from_rgb(0.5, 0.5, 0.5);
        let paint = Rgba::from_rgb(0.2, 0.4, 0.8);
        // Full coverage: the stroke as laid.
        let full = paint;
        assert_eq!(burnt.pixel(full, original, paint, 1.0), full);
        // Thin paint: towards the multiplied colour, darker than normal.
        let c = 0.25;
        let normal = paint * c + original * (1.0 - c);
        let out = burnt.pixel(normal, original, paint * c, c);
        assert!(
            out.r() < normal.r() && out.g() < normal.g(),
            "{out:?} vs {normal:?}"
        );
        let none = BurntEdges {
            amount: 0.0,
            ..burnt
        };
        assert_eq!(none.pixel(normal, original, paint * c, c), normal);
    }
}
