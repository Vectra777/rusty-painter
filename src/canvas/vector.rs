//! Vector layers: lines kept as their points, so they stay sharp and can be
//! erased whole, thinned or thickened, or recoloured after they're drawn,
//! as in Clip Studio's vector layers. The layer's pixels are always these
//! lines rendered ([`render_region`]); nothing else paints on it.

use eframe::egui::{Color32, Vec2};

/// Widest line, pixels.
pub const MAX_WIDTH: f32 = 400.0;

/// One line: its points (x, y and width there), colour and opacity.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorStroke {
    /// Canvas position and width (pixels) at each point, in order.
    pub points: Vec<[f32; 3]>,
    /// sRGB colour.
    pub colour: [u8; 3],
    /// 0..=1.
    pub opacity: f32,
}

/// A vector layer's lines, bottom first.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorLayer {
    pub strokes: Vec<VectorStroke>,
}

impl VectorStroke {
    /// `[x0, y0, x1, y1)` in whole pixels, with the line's width and a
    /// pixel of anti-aliasing.
    pub fn bounds(&self) -> [i32; 4] {
        let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for &[x, y, w] in &self.points {
            let r = w * 0.5 + 1.0;
            b[0] = b[0].min((x - r).floor() as i32);
            b[1] = b[1].min((y - r).floor() as i32);
            b[2] = b[2].max((x + r).ceil() as i32 + 1);
            b[3] = b[3].max((y + r).ceil() as i32 + 1);
        }
        b
    }

    /// The line smoothed through its points (Catmull-Rom, a few steps per
    /// segment), for drawing: a hand-drawn line's corners round off.
    fn path(&self) -> Vec<[f32; 3]> {
        let p = &self.points;
        if p.len() < 3 {
            return p.clone();
        }
        let mut out = Vec::with_capacity(p.len() * 4);
        for i in 0..p.len() - 1 {
            let (a, b, c, d) = (
                p[i.saturating_sub(1)],
                p[i],
                p[i + 1],
                p[(i + 2).min(p.len() - 1)],
            );
            let len = ((c[0] - b[0]).powi(2) + (c[1] - b[1]).powi(2)).sqrt();
            let steps = ((len / 3.0).ceil() as usize).clamp(1, 16);
            for s in 0..steps {
                let t = s as f32 / steps as f32;
                let (t2, t3) = (t * t, t * t * t);
                out.push([0, 1, 2].map(|k| {
                    0.5 * (2.0 * b[k]
                        + (-a[k] + c[k]) * t
                        + (2.0 * a[k] - 5.0 * b[k] + 4.0 * c[k] - d[k]) * t2
                        + (-a[k] + 3.0 * b[k] - 3.0 * c[k] + d[k]) * t3)
                }));
            }
        }
        out.push(p[p.len() - 1]);
        // Widths never go negative between points.
        for q in &mut out {
            q[2] = q[2].max(0.0);
        }
        out
    }

    /// Whether a circle at `c` of radius `r` touches the line.
    pub fn touches(&self, c: Vec2, r: f32) -> bool {
        let path = self.path();
        if path.len() == 1 {
            let [x, y, w] = path[0];
            return (Vec2::new(x, y) - c).length() <= r + w * 0.5;
        }
        path.windows(2).any(|s| {
            let (d, t) = segment_distance(c, s[0], s[1]);
            let w = s[0][2] + (s[1][2] - s[0][2]) * t;
            d <= r + w * 0.5
        })
    }

    /// The line with what a circle at `c` of radius `r` covers taken out:
    /// the pieces left (none if it was all covered), each thinned out again.
    pub fn cut(&self, c: Vec2, r: f32) -> Vec<VectorStroke> {
        if !self.touches(c, r) {
            return vec![self.clone()];
        }
        // The line as drawn, in steps fine enough to cut cleanly.
        let step = (r * 0.25).clamp(0.25, 2.0);
        let path = self.path();
        let mut dense = Vec::with_capacity(path.len() * 2);
        for pair in path.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let n = (len / step).ceil().max(1.0) as usize;
            for i in 0..n {
                let t = i as f32 / n as f32;
                dense.push([0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t));
            }
        }
        dense.extend(path.last());
        let mut pieces = Vec::new();
        let mut current: Vec<[f32; 3]> = Vec::new();
        for &p in &dense {
            let inside = (Vec2::new(p[0], p[1]) - c).length() <= r + p[2] * 0.25;
            if inside {
                if !current.is_empty() {
                    pieces.push(std::mem::take(&mut current));
                }
            } else {
                current.push(p);
            }
        }
        if !current.is_empty() {
            pieces.push(current);
        }
        pieces
            .into_iter()
            .map(|points| VectorStroke {
                points: simplify(&points, 0.2),
                colour: self.colour,
                opacity: self.opacity,
            })
            .collect()
    }
}

/// Distance from `p` to the segment `a`→`b` (x, y of each), and where along
/// it (0..=1) the nearest point is.
fn segment_distance(p: Vec2, a: [f32; 3], b: [f32; 3]) -> (f32, f32) {
    let (a2, b2) = (Vec2::new(a[0], a[1]), Vec2::new(b[0], b[1]));
    let d = b2 - a2;
    let len2 = d.length_sq();
    let t = if len2 > 1e-9 {
        ((p - a2).dot(d) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((a2 + d * t - p).length(), t)
}

/// Drop points closer than `min_gap` to the last one kept (the first and
/// last always stay), and those that lie on a straight run to within
/// `tolerance` pixels (Ramer–Douglas–Peucker): a hand-drawn line keeps
/// its shape in far fewer points.
pub fn simplify(points: &[[f32; 3]], tolerance: f32) -> Vec<[f32; 3]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((i, j)) = stack.pop() {
        let (mut worst, mut at) = (0.0, i);
        for k in i + 1..j {
            let (d, _) =
                segment_distance(Vec2::new(points[k][0], points[k][1]), points[i], points[j]);
            // A width change counts too, so pressure stays.
            let t = (k - i) as f32 / (j - i) as f32;
            let w = points[i][2] + (points[j][2] - points[i][2]) * t;
            let d = d.max((points[k][2] - w).abs() * 0.5);
            if d > worst {
                (worst, at) = (d, k);
            }
        }
        if worst > tolerance {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// `strokes` drawn over transparent, for the canvas pixels
/// `[x0, y0, x1, y1)`: row-major, premultiplied, as layer tiles hold them.
pub fn render_region(strokes: &[VectorStroke], region: [i32; 4]) -> Vec<Color32> {
    let [x0, y0, x1, y1] = region;
    let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
    let mut out = vec![Color32::TRANSPARENT; w * h];
    let mut coverage = vec![0.0f32; w * h];
    for stroke in strokes {
        let b = stroke.bounds();
        let [bx0, by0, bx1, by1] = [b[0].max(x0), b[1].max(y0), b[2].min(x1), b[3].min(y1)];
        if bx0 >= bx1 || by0 >= by1 || stroke.points.is_empty() {
            continue;
        }
        // The line's coverage of each pixel (the most any part of it gives,
        // so where it overlaps itself it doesn't darken).
        for y in by0..by1 {
            let row = (y - y0) as usize * w;
            coverage[row + (bx0 - x0) as usize..row + (bx1 - x0) as usize].fill(0.0);
        }
        let path = stroke.path();
        let segments: Vec<([f32; 3], [f32; 3])> = if path.len() == 1 {
            vec![(path[0], path[0])]
        } else {
            path.windows(2).map(|s| (s[0], s[1])).collect()
        };
        for (a, c) in segments {
            let r = a[2].max(c[2]) * 0.5 + 1.0;
            let sx0 = ((a[0].min(c[0]) - r).floor() as i32).max(bx0);
            let sy0 = ((a[1].min(c[1]) - r).floor() as i32).max(by0);
            let sx1 = ((a[0].max(c[0]) + r).ceil() as i32 + 1).min(bx1);
            let sy1 = ((a[1].max(c[1]) + r).ceil() as i32 + 1).min(by1);
            for y in sy0..sy1 {
                let row = (y - y0) as usize * w;
                for x in sx0..sx1 {
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let (d, t) = segment_distance(p, a, c);
                    let half = (a[2] + (c[2] - a[2]) * t) * 0.5;
                    // Thinner than a pixel: fainter rather than vanishing.
                    let (half, fade) = if half < 0.5 {
                        (0.5, (half * 2.0).max(0.0))
                    } else {
                        (half, 1.0)
                    };
                    let v = (half + 0.5 - d).clamp(0.0, 1.0) * fade;
                    let slot = &mut coverage[row + (x - x0) as usize];
                    if v > *slot {
                        *slot = v;
                    }
                }
            }
        }
        let [r, g, b] = stroke.colour;
        let opacity = stroke.opacity.clamp(0.0, 1.0);
        for y in by0..by1 {
            let row = (y - y0) as usize * w;
            for x in bx0..bx1 {
                let i = row + (x - x0) as usize;
                let a = coverage[i] * opacity;
                if a <= 0.0 {
                    continue;
                }
                let src = Color32::from_rgba_unmultiplied(r, g, b, (a * 255.0).round() as u8);
                out[i] = crate::canvas::storage::gamma_over(src, out[i]);
            }
        }
    }
    out
}

/// The union of `[x0, y0, x1, y1)` boxes (`None` if there are none).
pub fn union(boxes: impl IntoIterator<Item = [i32; 4]>) -> Option<[i32; 4]> {
    boxes.into_iter().reduce(|a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[[f32; 3]]) -> VectorStroke {
        VectorStroke {
            points: points.to_vec(),
            colour: [0, 0, 0],
            opacity: 1.0,
        }
    }

    #[test]
    fn a_line_is_as_wide_as_its_points_say() {
        // Off the pixel grid, so its edges fall inside pixels.
        let s = line(&[[2.0, 10.3, 4.0], [30.0, 10.3, 4.0]]);
        let px = render_region(&[s], [0, 0, 32, 20]);
        let at = |x: usize, y: usize| px[y * 32 + x];
        assert_eq!(at(15, 10), Color32::BLACK, "on the line");
        assert_eq!(at(15, 14), Color32::TRANSPARENT, "past its edge");
        // Across it, the coverage adds up to its width, the edges soft.
        let across: f32 = (0..20).map(|y| at(15, y).a() as f32 / 255.0).sum();
        assert!((across - 4.0).abs() < 0.1, "{across}");
        assert!(
            (0..20).any(|y| (1..255).contains(&at(15, y).a())),
            "soft edges"
        );
    }

    #[test]
    fn a_line_crossing_itself_doesnt_darken_and_opacity_applies() {
        let mut s = line(&[[2.0, 10.0, 6.0], [30.0, 10.0, 6.0], [2.0, 10.0, 6.0]]);
        s.opacity = 0.5;
        let px = render_region(&[s], [0, 0, 32, 20]);
        assert_eq!(px[10 * 32 + 15].a(), 128);
    }

    #[test]
    fn a_region_shows_the_same_pixels_as_the_whole() {
        let strokes = vec![
            line(&[[3.0, 3.0, 5.0], [40.0, 30.0, 9.0], [10.0, 50.0, 2.0]]),
            VectorStroke {
                points: vec![[0.0, 40.0, 12.0], [60.0, 10.0, 3.0]],
                colour: [200, 30, 30],
                opacity: 0.7,
            },
        ];
        let whole = render_region(&strokes, [0, 0, 64, 64]);
        let part = render_region(&strokes, [20, 16, 44, 40]);
        for y in 16..40 {
            for x in 20..44 {
                assert_eq!(part[(y - 16) * 24 + (x - 20)], whole[y * 64 + x], "{x},{y}");
            }
        }
    }

    #[test]
    fn erasing_splits_or_removes_a_line() {
        let s = line(
            &(0..=20)
                .map(|i| [i as f32 * 2.0, 5.0, 2.0])
                .collect::<Vec<_>>(),
        );
        assert!(s.touches(Vec2::new(20.0, 7.0), 1.5));
        assert!(!s.touches(Vec2::new(20.0, 12.0), 1.5));
        let pieces = s.cut(Vec2::new(20.0, 5.0), 3.0);
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].points.last().unwrap()[0] < 17.5);
        assert!(pieces[1].points[0][0] > 22.5);
        assert!(s.cut(Vec2::new(20.0, 5.0), 100.0).is_empty());
        // A line of two far-apart points is cut in the middle too.
        let long = line(&[[0.0, 5.0, 2.0], [100.0, 5.0, 2.0]]);
        let pieces = long.cut(Vec2::new(50.0, 5.0), 4.0);
        assert_eq!(pieces.len(), 2);
        assert!(
            pieces[0].points.last().unwrap()[0] > 44.0,
            "cut near, not at a point"
        );
        // Missed, it's left as it was.
        assert_eq!(long.cut(Vec2::new(50.0, 30.0), 4.0), vec![long.clone()]);
    }

    #[test]
    fn simplifying_keeps_the_shape_in_fewer_points() {
        // A straight run with jitter below the tolerance, then a corner.
        let mut pts: Vec<[f32; 3]> = (0..50)
            .map(|i| [i as f32, if i % 2 == 0 { 0.1 } else { -0.1 }, 3.0])
            .collect();
        pts.extend((1..20).map(|i| [49.0, i as f32, 3.0]));
        let s = simplify(&pts, 0.5);
        assert!(s.len() <= 4, "{}", s.len());
        assert_eq!(s.first(), pts.first());
        assert_eq!(s.last(), pts.last());
        assert!(
            s.iter()
                .any(|p| (p[0] - 49.0).abs() < 1.0 && p[1].abs() < 1.0),
            "the corner"
        );
        // A pressure swell is kept.
        let swell: Vec<[f32; 3]> = (0..30)
            .map(|i| [i as f32, 0.0, if i == 15 { 12.0 } else { 2.0 }])
            .collect();
        assert!(simplify(&swell, 0.5).iter().any(|p| p[2] == 12.0));
    }

    #[test]
    fn layers_round_trip_as_json() {
        let layer = VectorLayer {
            strokes: vec![line(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])],
        };
        let json = serde_json::to_string(&layer).unwrap();
        assert_eq!(serde_json::from_str::<VectorLayer>(&json).unwrap(), layer);
    }
}
