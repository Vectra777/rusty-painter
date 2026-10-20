//! Shapes to coverage: how much of each pixel a set of polygons covers,
//! anti-aliased by the exact area, as fonts are drawn. Edges add up the
//! area they sweep into an accumulation row; summed along the row that's
//! each pixel's winding, which the fill rule turns into coverage. Polygons
//! that overlap add up, so pieces drawn together (a stroke's segments,
//! joins and caps) meet without seams.
//!
//! [`stroke`] turns lines into such polygons (caps, joins, dashes) and
//! [`marker`] the shapes at their ends.

pub mod marker;
pub mod stroke;

use eframe::egui::Vec2;

/// How overlapping polygons fill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FillRule {
    /// Inside wherever the edges wind around the point at all.
    #[default]
    NonZero,
    /// Inside where they wind round an odd number of times (holes where
    /// shapes overlap).
    EvenOdd,
}

/// Edges of polygons, ready to be swept a band of rows at a time.
#[derive(Clone, Debug, Default)]
pub struct Edges {
    /// Each edge top to bottom: `[x0, y0, x1, y1]` and its direction.
    edges: Vec<([f32; 4], f32)>,
    /// `[x0, y0, x1, y1]` around them.
    bounds: [f32; 4],
    /// The polygons themselves, when asked to keep them (see
    /// [`Edges::recording`]).
    polygons: Option<Vec<Vec<Vec2>>>,
}

impl Edges {
    pub fn new() -> Self {
        Edges {
            edges: Vec::new(),
            bounds: [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
            polygons: None,
        }
    }

    /// Edges that also keep the polygons they're given, to write them out
    /// (an SVG export's markers).
    pub fn recording() -> Self {
        Edges {
            polygons: Some(Vec::new()),
            ..Self::new()
        }
    }

    /// The polygons given, if recording.
    pub fn polygons(&self) -> &[Vec<Vec2>] {
        self.polygons.as_deref().unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// `[x0, y0, x1, y1]` the edges reach (whole pixels, outwards).
    pub fn pixel_bounds(&self) -> [i32; 4] {
        let b = self.bounds;
        if b[0] > b[2] {
            return [0; 4];
        }
        [
            b[0].floor() as i32,
            b[1].floor() as i32,
            b[2].ceil() as i32 + 1,
            b[3].ceil() as i32 + 1,
        ]
    }

    /// A closed polygon through `points` (the last joined to the first).
    pub fn polygon(&mut self, points: &[Vec2]) {
        if points.len() < 3 {
            return;
        }
        if let Some(p) = &mut self.polygons {
            p.push(points.to_vec());
        }
        for i in 0..points.len() {
            self.edge(points[i], points[(i + 1) % points.len()]);
        }
    }

    /// A polygon with its winding made positive (counter-clockwise on
    /// screen): pieces of one stroke must all turn the same way, or where
    /// they overlap they'd cancel out.
    pub fn polygon_positive(&mut self, points: &[Vec2]) {
        if signed_area(points) < 0.0 {
            let mut reversed = points.to_vec();
            reversed.reverse();
            self.polygon(&reversed);
        } else {
            self.polygon(points);
        }
    }

    pub fn edge(&mut self, a: Vec2, b: Vec2) {
        if a.y == b.y || !(a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite())
        {
            return;
        }
        let (top, bottom, dir) = if a.y < b.y { (a, b, 1.0) } else { (b, a, -1.0) };
        self.edges.push(([top.x, top.y, bottom.x, bottom.y], dir));
        let b = &mut self.bounds;
        *b = [
            b[0].min(a.x.min(bottom.x).min(top.x)),
            b[1].min(top.y),
            b[2].max(a.x.max(bottom.x).max(top.x)),
            b[3].max(bottom.y),
        ];
    }

    /// Coverage (0..=1) of the pixels `[x0, y0, x1, y1)` under `rule`, row
    /// by row into `out` (`(x1 - x0) * (y1 - y0)`, added to: callers clear
    /// it). Returns whether any pixel is covered.
    pub fn coverage(&self, rule: FillRule, band: [i32; 4], out: &mut [f32]) -> bool {
        let [x0, y0, x1, y1] = band;
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        if w == 0 || h == 0 || self.edges.is_empty() {
            return false;
        }
        let b = self.bounds;
        if b[1] >= y1 as f32 || b[3] <= y0 as f32 || b[0] >= x1 as f32 {
            return false;
        }
        // One more column than the row: what's swept past the right edge.
        let stride = w + 1;
        let mut acc = vec![0.0f32; stride * h];
        let (left, right) = (x0 as f32, x1 as f32);
        let mut row_pieces = |ax: f32, ay: f32, bx: f32, by: f32, dir: f32| {
            // Within the band's rows, row by row.
            let dxdy = (bx - ax) / (by - ay);
            let top = ay.max(y0 as f32);
            let bottom = by.min(y1 as f32);
            let mut x = ax + (top - ay) * dxdy;
            let mut y = top;
            while y < bottom {
                let row = y.floor();
                let next_y = (row + 1.0).min(bottom);
                let next_x = x + dxdy * (next_y - y);
                let r = (row as i32 - y0) as usize * stride;
                sweep(
                    &mut acc[r..r + stride],
                    x - left,
                    next_x - left,
                    (next_y - y) * dir,
                );
                x = next_x;
                y = next_y;
            }
        };
        for &([ax, ay, bx, by], dir) in &self.edges {
            if by <= y0 as f32 || ay >= y1 as f32 {
                continue;
            }
            // Cut where it crosses the band's sides: left of it, an edge
            // counts as one along the side (it covers the whole row from
            // there); right of it, it's off the band.
            let mut cuts = [ay, by, by, by];
            let mut n = 1;
            for side in [left, right] {
                if (ax - side) * (bx - side) < 0.0 {
                    cuts[n] = ay + (side - ax) / (bx - ax) * (by - ay);
                    n += 1;
                }
            }
            cuts[1..n].sort_by(f32::total_cmp);
            cuts[n] = by;
            let at = |y: f32| ax + (y - ay) / (by - ay) * (bx - ax);
            for k in 0..n {
                let (ya, yb) = (cuts[k], cuts[k + 1]);
                if yb <= ya {
                    continue;
                }
                let (xa, xb) = (at(ya).clamp(left, right), at(yb).clamp(left, right));
                row_pieces(xa, ya, xb, yb, dir);
            }
        }
        let mut any = false;
        for row in 0..h {
            let mut sum = 0.0f32;
            let acc = &acc[row * stride..row * stride + w];
            let out = &mut out[row * w..row * w + w];
            for (o, &a) in out.iter_mut().zip(acc) {
                sum += a;
                let c = match rule {
                    FillRule::NonZero => sum.abs().min(1.0),
                    FillRule::EvenOdd => {
                        let m = sum.abs() % 2.0;
                        if m > 1.0 { 2.0 - m } else { m }
                    }
                };
                if c > 1e-4 {
                    *o = (*o + c).min(1.0);
                    any = true;
                }
            }
        }
        any
    }
}

/// An edge crossing one row from `x0` to `x1` (columns of `acc`), `d`
/// high (signed by direction): the area it leaves to its right in each
/// pixel it crosses, put where the row's running sum picks it up. (What
/// lands past the last column is never summed: it's off the band.)
fn sweep(acc: &mut [f32], x0: f32, x1: f32, d: f32) {
    let mut add = |i: i32, v: f32| {
        if let Some(a) = usize::try_from(i).ok().and_then(|i| acc.get_mut(i)) {
            *a += v;
        }
    };
    let (lo, hi) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
    let lo_floor = lo.floor();
    let (i0, i1) = (lo_floor as i32, hi.ceil() as i32);
    if i1 <= i0 + 1 {
        // Within one pixel: the part of it right of the edge's middle.
        let mid = 0.5 * (x0 + x1) - lo_floor;
        add(i0, d - d * mid);
        add(i0 + 1, d * mid);
        return;
    }
    // Across several: the area under the ramp in each.
    let s = 1.0 / (hi - lo);
    let f0 = lo - lo_floor;
    let a0 = 0.5 * s * (1.0 - f0) * (1.0 - f0);
    let f1 = hi - hi.ceil() + 1.0;
    let am = 0.5 * s * f1 * f1;
    add(i0, d * a0);
    if i1 == i0 + 2 {
        add(i0 + 1, d * (1.0 - a0 - am));
    } else {
        let a1 = s * (1.5 - f0);
        add(i0 + 1, d * (a1 - a0));
        for i in i0 + 2..i1 - 1 {
            add(i, d * s);
        }
        let a2 = a1 + (i1 - i0 - 3) as f32 * s;
        add(i1 - 1, d * (1.0 - a2 - am));
    }
    add(i1, d * am);
}

/// Twice the signed area of a polygon (positive: counter-clockwise with y
/// down, as the screen has it... or the other way; only the sign's
/// consistency matters).
pub fn signed_area(points: &[Vec2]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage by sampling `n`×`n` points in each pixel: the reference.
    fn sampled(polys: &[Vec<Vec2>], rule: FillRule, band: [i32; 4], n: usize) -> Vec<f32> {
        let [x0, y0, x1, y1] = band;
        let winding = |p: Vec2| -> i32 {
            let mut w = 0;
            for poly in polys {
                for i in 0..poly.len() {
                    let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                    if (a.y <= p.y) != (b.y <= p.y) {
                        let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                        if x > p.x {
                            w += if b.y > a.y { 1 } else { -1 };
                        }
                    }
                }
            }
            w
        };
        let mut out = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let mut inside = 0;
                for sy in 0..n {
                    for sx in 0..n {
                        let p = Vec2::new(
                            x as f32 + (sx as f32 + 0.5) / n as f32,
                            y as f32 + (sy as f32 + 0.5) / n as f32,
                        );
                        let w = winding(p);
                        let hit = match rule {
                            FillRule::NonZero => w != 0,
                            FillRule::EvenOdd => w % 2 != 0,
                        };
                        inside += hit as usize;
                    }
                }
                out.push(inside as f32 / (n * n) as f32);
            }
        }
        out
    }

    fn swept(polys: &[Vec<Vec2>], rule: FillRule, band: [i32; 4]) -> Vec<f32> {
        let mut e = Edges::new();
        for p in polys {
            e.polygon(p);
        }
        let mut out = vec![0.0; ((band[2] - band[0]) * (band[3] - band[1])) as usize];
        e.coverage(rule, band, &mut out);
        out
    }

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    fn worst(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_rect_half_a_pixel_off_covers_its_edges_half() {
        let rect = vec![v(2.5, 2.5), v(7.5, 2.5), v(7.5, 6.5), v(2.5, 6.5)];
        let c = swept(&[rect], FillRule::NonZero, [0, 0, 10, 10]);
        let at = |x: usize, y: usize| c[y * 10 + x];
        assert!((at(2, 4) - 0.5).abs() < 1e-5, "{}", at(2, 4));
        assert!((at(7, 4) - 0.5).abs() < 1e-5);
        assert!((at(4, 2) - 0.5).abs() < 1e-5);
        assert!((at(2, 2) - 0.25).abs() < 1e-5, "corner {}", at(2, 2));
        assert!((at(4, 4) - 1.0).abs() < 1e-5);
        assert_eq!(at(1, 4), 0.0);
    }

    #[test]
    fn a_circle_covers_its_area() {
        let (c, r) = (v(20.3, 19.7), 13.4);
        let circle: Vec<Vec2> = (0..2000)
            .map(|i| c + Vec2::angled(i as f32 / 2000.0 * std::f32::consts::TAU) * r)
            .collect();
        let cov = swept(&[circle], FillRule::NonZero, [0, 0, 40, 40]);
        let total: f32 = cov.iter().sum();
        let area = std::f32::consts::PI * r * r;
        assert!((total - area).abs() / area < 0.003, "{total} vs {area}");
    }

    #[test]
    fn coverage_is_what_sampling_finds_for_any_edges() {
        let mut seed = 0x51ed_270b_u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        for _ in 0..30 {
            // Random polygons, steep and shallow, crossing themselves.
            let polys: Vec<Vec<Vec2>> = (0..3)
                .map(|_| {
                    (0..5)
                        .map(|_| v(rand() * 30.0 - 3.0, rand() * 24.0 - 2.0))
                        .collect()
                })
                .collect();
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                let band = [0, 0, 26, 20];
                let fast = swept(&polys, rule, band);
                let slow = sampled(&polys, rule, band, 16);
                let mean = fast
                    .iter()
                    .zip(&slow)
                    .map(|(a, b)| (a - b).abs())
                    .sum::<f32>()
                    / fast.len() as f32;
                // Polygons crossing themselves all over: exact but for the
                // pixels where edges cross inside them (there adding up
                // areas is only an estimate); simple ones are exact (see
                // below).
                assert!(mean < 0.02, "{rule:?} mean {mean}");
            }
        }
    }

    #[test]
    fn coverage_of_a_simple_polygon_is_its_area_in_each_pixel() {
        let mut seed = 0x0bad_5eed_u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        for _ in 0..40 {
            // A triangle never crosses itself: no pixel has edges crossing.
            let tri: Vec<Vec2> = (0..3)
                .map(|_| v(rand() * 30.0 - 3.0, rand() * 24.0 - 2.0))
                .collect();
            let band = [0, 0, 26, 20];
            let fast = swept(std::slice::from_ref(&tri), FillRule::NonZero, band);
            let slow = sampled(&[tri], FillRule::NonZero, band, 32);
            assert!(worst(&fast, &slow) < 0.04, "{}", worst(&fast, &slow));
        }
    }

    #[test]
    fn nested_squares_fill_or_leave_a_hole_by_the_rule() {
        let outer = vec![v(2.0, 2.0), v(18.0, 2.0), v(18.0, 18.0), v(2.0, 18.0)];
        let inner = vec![v(6.0, 6.0), v(14.0, 6.0), v(14.0, 14.0), v(6.0, 14.0)];
        let both = [outer, inner];
        let at = |c: &[f32], x: usize, y: usize| c[y * 20 + x];
        let nz = swept(&both, FillRule::NonZero, [0, 0, 20, 20]);
        let eo = swept(&both, FillRule::EvenOdd, [0, 0, 20, 20]);
        assert_eq!(at(&nz, 10, 10), 1.0);
        assert_eq!(at(&eo, 10, 10), 0.0);
        assert_eq!(at(&eo, 4, 10), 1.0);
    }

    #[test]
    fn bands_cover_as_the_whole_does() {
        let poly = vec![v(1.3, 0.2), v(37.0, 9.9), v(12.2, 31.7), v(-4.0, 20.0)];
        let whole = swept(
            std::slice::from_ref(&poly),
            FillRule::NonZero,
            [0, 0, 32, 32],
        );
        let mut parts = Vec::new();
        for y in (0..32).step_by(7) {
            parts.extend(swept(
                std::slice::from_ref(&poly),
                FillRule::NonZero,
                [0, y, 32, (y + 7).min(32)],
            ));
        }
        assert!(worst(&whole, &parts) < 1e-5);
        // Columns too: a band off to the right.
        let right = swept(&[poly], FillRule::NonZero, [10, 0, 32, 32]);
        for y in 0..32 {
            for x in 10..32 {
                assert!((right[y * 22 + x - 10] - whole[y * 32 + x]).abs() < 1e-5);
            }
        }
    }
}
