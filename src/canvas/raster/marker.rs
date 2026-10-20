//! Shapes at a line's ends (arrowheads and the like), sized by the line's
//! width and turned along it. Drawn with the line's paint, as part of it.

use super::Edges;
use super::stroke::{self, Cap, Join, Style};
use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Marker {
    #[default]
    None,
    /// A filled triangle, its point at the end.
    Arrow,
    /// Two strokes meeting at the end.
    OpenArrow,
    /// A filled dot on the end.
    Circle,
    /// A filled square on the end.
    Square,
    /// A short stroke across the end.
    Bar,
}

impl Marker {
    /// How long a filled arrowhead is for a line `width` wide.
    fn arrow_length(width: f32) -> f32 {
        (width * 3.0).max(6.0)
    }

    /// How far back from the end the line stops, so it doesn't show
    /// through the marker's point.
    pub fn inset(self, width: f32) -> f32 {
        match self {
            Marker::Arrow => Self::arrow_length(width) * 0.6,
            _ => 0.0,
        }
    }

    /// Add the marker at `tip`, the line arriving there along `dir`
    /// (unit), to `edges`.
    pub fn add(self, edges: &mut Edges, tip: Vec2, dir: Vec2, width: f32) {
        let side = Vec2::new(-dir.y, dir.x);
        match self {
            Marker::None => {}
            Marker::Arrow => {
                let len = Self::arrow_length(width);
                let base = tip - dir * len;
                edges.polygon_positive(&[tip, base + side * len * 0.5, base - side * len * 0.5]);
            }
            Marker::OpenArrow => {
                let len = Self::arrow_length(width);
                let base = tip - dir * len;
                let style = Style {
                    width,
                    cap: Cap::Round,
                    join: Join::Miter,
                    miter_limit: 10.0,
                };
                let chevron = [base + side * len * 0.5, tip, base - side * len * 0.5];
                stroke::outline(edges, &chevron, false, &style);
            }
            Marker::Circle => {
                edges.polygon_positive(&stroke::circle(tip, (width * 1.5).max(3.0)));
            }
            Marker::Square => {
                let h = (width * 1.5).max(3.0);
                let (a, b) = (dir * h, side * h);
                edges.polygon_positive(&[tip + a + b, tip - a + b, tip - a - b, tip + a - b]);
            }
            Marker::Bar => {
                let h = (width * 2.0).max(4.0);
                let style = Style {
                    width,
                    cap: Cap::Butt,
                    join: Join::Miter,
                    miter_limit: 4.0,
                };
                stroke::outline(edges, &[tip + side * h, tip - side * h], false, &style);
            }
        }
    }
}

/// `line` with its ends drawn back by `start` and `end` (along it), so the
/// line stops under its markers' points. A line too short keeps a point.
pub fn shorten(line: &[Vec2], start: f32, end: f32) -> Vec<Vec2> {
    let mut pts = line.to_vec();
    for (amount, from_end) in [(start, false), (end, true)] {
        if amount <= 0.0 || pts.len() < 2 {
            continue;
        }
        if from_end {
            pts.reverse();
        }
        let mut left = amount;
        while pts.len() > 1 {
            let seg = (pts[1] - pts[0]).length();
            if seg > left {
                pts[0] = pts[0] + (pts[1] - pts[0]) * (left / seg);
                break;
            }
            left -= seg;
            pts.remove(0);
        }
        if from_end {
            pts.reverse();
        }
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::raster::FillRule;

    #[test]
    fn an_arrowhead_points_along_the_line_and_the_line_stops_under_it() {
        let line = [Vec2::new(5.0, 20.0), Vec2::new(45.0, 20.0)];
        let short = shorten(&line, 0.0, Marker::Arrow.inset(4.0));
        assert!((short[1].x - (45.0 - 7.2)).abs() < 1e-4);
        let mut e = Edges::new();
        Marker::Arrow.add(&mut e, line[1], Vec2::new(1.0, 0.0), 4.0);
        let mut c = vec![0.0; 60 * 40];
        e.coverage(FillRule::NonZero, [0, 0, 60, 40], &mut c);
        let area: f32 = c.iter().sum();
        // A triangle 12 long, 12 across its base.
        assert!((area - 72.0).abs() < 0.05, "{area}");
        assert!(c[20 * 60 + 40] > 0.9, "filled behind the tip");
        assert_eq!(c[20 * 60 + 46], 0.0, "nothing past it");
    }

    #[test]
    fn shortening_past_a_corner_walks_round_it() {
        let line = [
            Vec2::new(0.0, 0.0),
            Vec2::new(3.0, 0.0),
            Vec2::new(3.0, 10.0),
        ];
        let s = shorten(&line, 5.0, 2.0);
        assert_eq!(s, [Vec2::new(3.0, 2.0), Vec2::new(3.0, 8.0)]);
    }
}
