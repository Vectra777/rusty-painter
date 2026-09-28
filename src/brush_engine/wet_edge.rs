//! Watercolour edges: when the pen lifts, the stroke's paint pulls to its
//! rim, as a wash does drying on paper (Clip Studio's "watercolor edge").
//!
//! The stroke's coverage is compared with a blurred copy of itself: in the
//! middle of a wash the two match, and the paint there thins by the edge's
//! strength; near the edge the coverage exceeds its blur, and the paint
//! keeps its full strength. Thin lines, all edge, stay as they were.

/// Coverage past its blur, times this, is "at the edge" (1 = full paint).
const EDGE_GAIN: f32 = 3.0;

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
}
