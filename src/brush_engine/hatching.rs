//! Hatching brush: wherever the brush passes, it
//! paints parallel lines at a fixed angle and spacing instead of solid
//! paint. The lines are pinned to the canvas, so strokes laid next to or
//! over each other join into one even hatch; pressing harder adds more
//! directions (cross-hatching).

/// A hatching brush's lines.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Hatching {
    /// Angle of the lines, degrees counter-clockwise from horizontal.
    pub angle: f32,
    /// Distance between the lines, canvas pixels.
    pub separation: f32,
    /// Line thickness, canvas pixels.
    pub thickness: f32,
    /// Pressing harder adds a second direction, then a third.
    pub crosshatch: bool,
}

impl Default for Hatching {
    fn default() -> Self {
        Self {
            angle: 45.0,
            separation: 6.0,
            thickness: 1.2,
            crosshatch: true,
        }
    }
}

/// Pressure past which a second, then a third direction joins.
const CROSS_LEVELS: [f32; 2] = [0.45, 0.8];

impl Hatching {
    /// How many directions a dab at `pressure` hatches in (1..=3).
    pub fn level(&self, pressure: f32) -> u8 {
        if !self.crosshatch {
            return 1;
        }
        1 + CROSS_LEVELS.iter().filter(|&&p| pressure >= p).count() as u8
    }

    /// The lines' coverage over a row of dab alphas at canvas row `y`,
    /// columns `x0..`, in `level` directions: each alpha is scaled by how
    /// much line is on its pixel.
    pub fn apply_row(&self, y: usize, x0: usize, level: u8, alphas: &mut [f32]) {
        let sep = self.separation.max(1.0);
        let half = (self.thickness * 0.5).max(0.25);
        let base = self.angle.to_radians();
        // The second direction crosses the first square on, the third at
        // 45° between them.
        let angles = [
            base,
            base + std::f32::consts::FRAC_PI_2,
            base + std::f32::consts::FRAC_PI_4,
        ];
        let dirs: Vec<(f32, f32)> = angles[..level.clamp(1, 3) as usize]
            .iter()
            .map(|a| {
                // Distance across the lines: along their normal.
                let (s, c) = a.sin_cos();
                (-s, -c)
            })
            .collect();
        // Pixel centres at whole numbers here, so a line on a multiple of
        // the separation runs down the middle of a pixel row.
        let py = y as f32;
        for (i, a) in alphas.iter_mut().enumerate() {
            if *a <= 0.0 {
                continue;
            }
            let px = (x0 + i) as f32;
            let mut line = 0.0f32;
            for &(nx, ny) in &dirs {
                // Signed distance to the nearest line, 0..sep/2.
                let d = (px * nx + py * ny).rem_euclid(sep);
                let d = d.min(sep - d);
                // One pixel of anti-aliasing at the line's edges.
                line = line.max((half + 0.5 - d).clamp(0.0, 1.0));
            }
            *a *= line;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(h: &Hatching, level: u8) -> Vec<f32> {
        let mut out = Vec::new();
        for y in 0..60 {
            let mut row = vec![1.0; 60];
            h.apply_row(y, 0, level, &mut row);
            out.extend(row);
        }
        out
    }

    #[test]
    fn horizontal_lines_repeat_at_their_separation() {
        let h = Hatching {
            angle: 0.0,
            separation: 6.0,
            thickness: 1.0,
            crosshatch: false,
        };
        let c = coverage(&h, 1);
        let column: Vec<f32> = (0..60).map(|y| c[y * 60 + 10]).collect();
        let lines = column
            .windows(2)
            .filter(|w| w[1] > 0.5 && w[0] <= 0.5)
            .count();
        assert!((9..=10).contains(&lines), "{lines} lines in 60 px");
        // Horizontal: every column the same.
        for y in 0..60 {
            assert_eq!(c[y * 60 + 10], c[y * 60 + 40]);
        }
    }

    #[test]
    fn pressing_harder_adds_directions_and_more_ink() {
        let h = Hatching::default();
        assert_eq!((h.level(0.1), h.level(0.5), h.level(0.95)), (1, 2, 3));
        let ink = |level| coverage(&h, level).iter().sum::<f32>();
        assert!(ink(1) < ink(2) && ink(2) < ink(3));
        let plain = Hatching {
            crosshatch: false,
            ..h
        };
        assert_eq!(plain.level(1.0), 1);
    }
}
