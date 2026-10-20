//! A line's outline as polygons to fill: a band along each segment, the
//! joins between them and the caps at its ends, all turning the same way
//! so they add up into one shape ([`super::Edges`]). Dashes cut the line
//! first, each dash capped.

use super::Edges;
use eframe::egui::Vec2;

/// How an open line's ends look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Cap {
    /// Square, at the end.
    #[default]
    Butt,
    Round,
    /// Square, half the width past the end.
    Square,
}

/// How a line turns a corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Join {
    /// Pointed (cut off square when longer than the miter limit allows).
    #[default]
    Miter,
    Round,
    /// Cut off square.
    Bevel,
}

/// The outline's look: width and how ends and corners are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub width: f32,
    pub cap: Cap,
    pub join: Join,
    /// Longest a miter may be, in widths, before it's cut off (as SVG's
    /// `stroke-miterlimit`).
    pub miter_limit: f32,
}

/// Turns less sharp than this (the sine of the angle between segments)
/// are joined by a bevel whatever the style: a smooth curve's many short
/// segments, where the difference is far under a pixel.
const SLIGHT: f32 = 0.08;

/// Add the outline of `line` (`closed`: its last point joined back to the
/// first) to `edges`.
pub fn outline(edges: &mut Edges, line: &[Vec2], closed: bool, style: &Style) {
    let hw = style.width * 0.5;
    if hw <= 0.0 || !hw.is_finite() {
        return;
    }
    let mut pts: Vec<Vec2> = Vec::with_capacity(line.len());
    for &p in line {
        if pts
            .last()
            .is_none_or(|&q: &Vec2| (p - q).length_sq() > 1e-8)
        {
            pts.push(p);
        }
    }
    if closed && pts.len() > 2 && (pts[0] - pts[pts.len() - 1]).length_sq() <= 1e-8 {
        pts.pop();
    }
    if pts.is_empty() {
        return;
    }
    if pts.len() == 1 {
        // A dot: what its caps would make of a line of no length.
        let p = pts[0];
        match style.cap {
            Cap::Round => edges.polygon_positive(&circle(p, hw)),
            Cap::Square => edges.polygon_positive(&[
                p + Vec2::new(-hw, -hw),
                p + Vec2::new(hw, -hw),
                p + Vec2::new(hw, hw),
                p + Vec2::new(-hw, hw),
            ]),
            Cap::Butt => {}
        }
        return;
    }
    let n = pts.len();
    let segments = if closed { n } else { n - 1 };
    let dir = |i: usize| (pts[(i + 1) % n] - pts[i]).normalized();
    for i in 0..segments {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let side = normal(dir(i)) * hw;
        edges.polygon_positive(&[a + side, b + side, b - side, a - side]);
    }
    // Joins where segments meet.
    let corners = if closed { 0..n } else { 1..n - 1 };
    for i in corners {
        let (d0, d1) = (dir((i + n - 1) % n), dir(i % n));
        join(edges, pts[i], d0, d1, hw, style);
    }
    if !closed {
        cap(edges, pts[0], -dir(0), hw, style.cap);
        cap(edges, pts[n - 1], dir(n - 2), hw, style.cap);
    }
}

/// The left of `d` (as seen going along it).
fn normal(d: Vec2) -> Vec2 {
    Vec2::new(-d.y, d.x)
}

/// The corner at `p`, coming along `d0` and leaving along `d1`.
fn join(edges: &mut Edges, p: Vec2, d0: Vec2, d1: Vec2, hw: f32, style: &Style) {
    let cross = d0.x * d1.y - d0.y * d1.x;
    let dot = d0.dot(d1);
    if cross.abs() < 1e-6 && dot > 0.0 {
        return;
    }
    // The outside of the turn: away from where it turns to.
    let s = if cross > 0.0 { -1.0 } else { 1.0 };
    let (o0, o1) = (p + normal(d0) * hw * s, p + normal(d1) * hw * s);
    let slight = cross.abs() < SLIGHT && dot > 0.0;
    match style.join {
        _ if slight => edges.polygon_positive(&[p, o0, o1]),
        Join::Bevel => edges.polygon_positive(&[p, o0, o1]),
        Join::Round => edges.polygon_positive(&circle(p, hw)),
        Join::Miter => {
            // The tip is where the two outer edges meet, 1/cos(half the
            // turn) widths out.
            let mid = (normal(d0) + normal(d1)) * s;
            let cos_half = mid.normalized().dot(normal(d0) * s);
            if mid.length_sq() < 1e-12 || cos_half <= 0.0 || 1.0 / cos_half > style.miter_limit {
                edges.polygon_positive(&[p, o0, o1]);
            } else {
                let tip = p + mid.normalized() * (hw / cos_half);
                edges.polygon_positive(&[p, o0, tip, o1]);
            }
        }
    }
}

/// The cap at an end `p`, the line leaving it along `out`.
fn cap(edges: &mut Edges, p: Vec2, out: Vec2, hw: f32, cap: Cap) {
    match cap {
        Cap::Butt => {}
        Cap::Round => edges.polygon_positive(&circle(p, hw)),
        Cap::Square => {
            let side = normal(out) * hw;
            let ahead = out * hw;
            edges.polygon_positive(&[p + side, p + side + ahead, p - side + ahead, p - side]);
        }
    }
}

/// A circle of radius `r` around `c`, its sides within a fortieth of a
/// pixel of the curve.
pub fn circle(c: Vec2, r: f32) -> Vec<Vec2> {
    // A chord's sagitta is r(1 - cos(θ/2)) ≈ rθ²/8: within 0.025 px.
    let step = (0.2 / r.max(0.1)).sqrt().min(std::f32::consts::FRAC_PI_4);
    let n = ((std::f32::consts::TAU / step).ceil() as usize).clamp(16, 1024);
    // Pushed out a touch, so the polygon covers the circle's area.
    let a = std::f32::consts::TAU / n as f32;
    let r = r * (a / a.sin()).sqrt();
    (0..n).map(|i| c + Vec2::angled(i as f32 * a) * r).collect()
}

/// `line` cut into dashes by `pattern` (lengths on, off, on, ...: an odd
/// number of them is used twice, as SVG does), starting `offset` into it.
/// Each dash is an open line. An empty or zero pattern leaves it whole.
pub fn dashes(line: &[Vec2], closed: bool, pattern: &[f32], offset: f32) -> Vec<Vec<Vec2>> {
    let mut pattern: Vec<f32> = pattern.iter().map(|d| d.max(0.0)).collect();
    if pattern.len() % 2 == 1 {
        pattern.extend_from_within(..);
    }
    let total: f32 = pattern.iter().sum();
    let mut pts = line.to_vec();
    if closed && pts.len() > 1 {
        pts.push(pts[0]);
    }
    if pattern.is_empty() || total <= 1e-3 || pts.len() < 2 {
        return vec![pts];
    }
    // Where in the pattern the line starts.
    let mut k = 0;
    let mut left = pattern[0];
    let mut skip = offset.rem_euclid(total);
    while skip > 0.0 {
        if skip >= left {
            skip -= left;
            k = (k + 1) % pattern.len();
            left = pattern[k];
        } else {
            left -= skip;
            skip = 0.0;
        }
    }
    let mut out = Vec::new();
    let mut current: Vec<Vec2> = if k % 2 == 0 { vec![pts[0]] } else { Vec::new() };
    for w in pts.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut seg = (b - a).length();
        while seg > 0.0 {
            if left >= seg {
                left -= seg;
                if k % 2 == 0 {
                    current.push(b);
                }
                seg = 0.0;
            } else {
                let cut = a + (b - a).normalized() * left;
                seg -= left;
                a = cut;
                if k % 2 == 0 {
                    current.push(cut);
                    out.push(std::mem::take(&mut current));
                } else {
                    current = vec![cut];
                }
                k = (k + 1) % pattern.len();
                left = pattern[k];
            }
        }
    }
    if k % 2 == 0 && current.len() > 1 {
        out.push(current);
    }
    // Zero-length "on" parts are dots (they show with round or square caps).
    out.retain(|d| !d.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::raster::FillRule;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    fn style(cap: Cap, join: Join) -> Style {
        Style {
            width: 4.0,
            cap,
            join,
            miter_limit: 4.0,
        }
    }

    fn covered(line: &[Vec2], closed: bool, style: Style, band: [i32; 4]) -> Vec<f32> {
        let mut e = Edges::new();
        outline(&mut e, line, closed, &style);
        let mut out = vec![0.0; ((band[2] - band[0]) * (band[3] - band[1])) as usize];
        e.coverage(FillRule::NonZero, band, &mut out);
        out
    }

    #[test]
    fn a_straight_line_is_as_wide_as_its_width_and_caps_add_on() {
        let line = [v(10.0, 10.0), v(30.0, 10.0)];
        let area = |cap| {
            covered(&line, false, style(cap, Join::Miter), [0, 0, 40, 20])
                .iter()
                .sum::<f32>()
        };
        assert!(
            (area(Cap::Butt) - 80.0).abs() < 0.01,
            "butt {}",
            area(Cap::Butt)
        );
        assert!((area(Cap::Square) - 96.0).abs() < 0.01);
        let round = 80.0 + std::f32::consts::PI * 4.0;
        assert!(
            (area(Cap::Round) - round).abs() < 0.1,
            "{}",
            area(Cap::Round)
        );
    }

    #[test]
    fn a_corner_has_no_seam_and_its_join_shows_as_asked() {
        // An L: along, then down.
        let line = [v(5.0, 10.0), v(25.0, 10.0), v(25.0, 30.0)];
        let band = [0, 0, 40, 40];
        let miter = covered(&line, false, style(Cap::Butt, Join::Miter), band);
        let bevel = covered(&line, false, style(Cap::Butt, Join::Bevel), band);
        let round = covered(&line, false, style(Cap::Butt, Join::Round), band);
        // Inside the line, fully covered: no seam where the pieces meet.
        for (x, y) in [(15, 10), (25, 20), (25, 10), (24, 11)] {
            assert_eq!(miter[y * 40 + x], 1.0, "({x}, {y})");
        }
        // The outer corner: the miter fills it, a bevel cuts it.
        assert_eq!(miter[8 * 40 + 26], 1.0);
        assert!(bevel[8 * 40 + 26] < 0.5);
        let (m, b, r): (f32, f32, f32) =
            (miter.iter().sum(), bevel.iter().sum(), round.iter().sum());
        assert!(m > r && r > b, "{m} {r} {b}");
        // Miter: two 20×4 bands overlapping in a 2×2, plus the 2×2 tip.
        assert!(
            (m - (80.0 + 80.0 - 4.0 + 4.0 - 4.0 + 4.0)).abs() < 0.05,
            "{m}"
        );
    }

    #[test]
    fn a_sharp_turn_past_the_miter_limit_is_bevelled() {
        let line = [v(5.0, 10.0), v(30.0, 10.0), v(5.0, 12.0)];
        let band = [0, 0, 60, 30];
        let miter = covered(&line, false, style(Cap::Butt, Join::Miter), band);
        let bevel = covered(&line, false, style(Cap::Butt, Join::Bevel), band);
        assert_eq!(miter, bevel);
    }

    #[test]
    fn dashes_add_up_to_the_pattern() {
        let line = [v(0.0, 0.0), v(100.0, 0.0)];
        let d = dashes(&line, false, &[10.0, 5.0], 0.0);
        assert_eq!(d.len(), 7);
        let len = |p: &Vec<Vec2>| p.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f32>();
        assert!(d[..6].iter().all(|p| (len(p) - 10.0).abs() < 1e-4));
        assert!((len(&d[6]) - 10.0).abs() < 1e-4, "100 = 6×15 + 10");
        // Offset into the pattern, across corners, and an odd pattern.
        let bent = [v(0.0, 0.0), v(7.0, 0.0), v(7.0, 50.0)];
        let d = dashes(&bent, false, &[4.0], 2.0);
        let on: f32 = d.iter().map(len).sum();
        assert!((on - 29.0).abs() < 1e-3, "{on}");
        assert!((len(&d[0]) - 2.0).abs() < 1e-4);
        assert_eq!(dashes(&line, false, &[], 0.0), vec![line.to_vec()]);
    }
}
