//! What a vector layer holds besides brush lines: shapes (Bézier paths
//! filled and outlined, as SVG draws them) and groups of either. A brush
//! line keeps its points and the width at each; a shape keeps its path and
//! how it's painted: fill, outline (width, ends, corners, dashes, markers),
//! each a colour or a gradient.

use super::VectorStroke;
use crate::canvas::gradient::{Gradient, GradientRepeat, GradientShape, GradientStops, Ramp};
use crate::canvas::path::Path;
use crate::canvas::raster::marker::{self, Marker};
use crate::canvas::raster::stroke::{self, Cap, Join, Style};
use crate::canvas::raster::{Edges, FillRule};
use eframe::egui::{Color32, Vec2};

/// One thing on a vector layer, drawn over those before it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VectorItem {
    /// A brush line.
    Line(VectorStroke),
    Shape(PathShape),
    Group(VectorGroup),
}

/// A path filled and outlined.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathShape {
    pub path: Path,
    #[serde(default)]
    pub fill: Option<Fill>,
    #[serde(default)]
    pub stroke: Option<StrokeStyle>,
    /// 0..=1, over fill and outline together.
    #[serde(default = "one")]
    pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fill {
    pub paint: Paint,
    #[serde(default)]
    pub rule: FillRule,
}

/// A shape's outline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StrokeStyle {
    pub paint: Paint,
    pub width: f32,
    #[serde(default)]
    pub cap: Cap,
    #[serde(default)]
    pub join: Join,
    #[serde(default = "four")]
    pub miter_limit: f32,
    /// Dash lengths, on then off (none: solid).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dash: Vec<f32>,
    #[serde(default)]
    pub dash_offset: f32,
    #[serde(default)]
    pub start: Marker,
    #[serde(default)]
    pub end: Marker,
}

impl StrokeStyle {
    /// A plain outline: `colour`, `width` wide, round ends and corners.
    pub fn solid(colour: [u8; 4], width: f32) -> Self {
        StrokeStyle {
            paint: Paint::Colour(colour),
            width,
            cap: Cap::Round,
            join: Join::Round,
            miter_limit: 4.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            start: Marker::None,
            end: Marker::None,
        }
    }

    /// How far past the path it can reach (corners, ends, markers).
    fn reach(&self) -> f32 {
        let hw = self.width * 0.5;
        let corners = match self.join {
            Join::Miter => hw * self.miter_limit.max(1.0),
            _ => hw,
        };
        let ends = match self.cap {
            Cap::Square => hw * std::f32::consts::SQRT_2,
            _ => hw,
        };
        let markers = if self.start != Marker::None || self.end != Marker::None {
            (self.width * 3.0).max(6.0)
        } else {
            0.0
        };
        corners.max(ends).max(markers)
    }
}

/// What fills an area: one colour, or a gradient.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Paint {
    /// sRGB and alpha.
    Colour([u8; 4]),
    Gradient(Box<PaintGradient>),
}

/// A gradient as a shape's paint: its line (or centre and radius) in
/// canvas pixels, or in a space of its own that `transform` places on the
/// canvas (an imported SVG gradient's).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PaintGradient {
    pub shape: GradientShape,
    #[serde(default)]
    pub repeat: GradientRepeat,
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub stops: GradientStops,
    /// `[a, b, c, d, e, f]`: (x, y) maps to (ax + cy + e, bx + dy + f).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<[f32; 6]>,
}

/// Shapes (and lines) drawn together.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorGroup {
    #[serde(default)]
    pub name: String,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "yes")]
    pub visible: bool,
    pub items: Vec<VectorItem>,
}

fn one() -> f32 {
    1.0
}

fn four() -> f32 {
    4.0
}

fn yes() -> bool {
    true
}

/// How finely curves are cut into straight pieces: within this of the curve.
const FLATNESS: f32 = 0.1;

impl PathShape {
    /// Each subpath as a polyline within [`FLATNESS`] of it, and whether
    /// it's closed.
    pub fn flattened(&self) -> Vec<(Vec<Vec2>, bool)> {
        self.path
            .subpaths
            .iter()
            .map(|s| (s.flatten_tol(FLATNESS), s.closed))
            .filter(|(p, _)| !p.is_empty())
            .collect()
    }

    /// The fill's edges and the outline's (markers included).
    pub fn edges(&self) -> (Option<Edges>, Option<Edges>) {
        let lines = self.flattened();
        let fill = self.fill.as_ref().map(|_| {
            let mut e = Edges::new();
            for (pts, _) in &lines {
                // Open subpaths fill as if closed, as in SVG.
                e.polygon(pts);
            }
            e
        });
        let outline = self.stroke.as_ref().map(|s| {
            let mut e = Edges::new();
            let style = Style {
                width: s.width,
                cap: s.cap,
                join: s.join,
                miter_limit: s.miter_limit,
            };
            for (pts, closed) in &lines {
                let ends = (!closed).then(|| ends(pts)).flatten();
                let body = match ends {
                    Some(_) => marker::shorten(pts, s.start.inset(s.width), s.end.inset(s.width)),
                    None => pts.clone(),
                };
                for dash in stroke::dashes(&body, *closed, &s.dash, s.dash_offset) {
                    let whole = s.dash.is_empty() && *closed;
                    stroke::outline(&mut e, &dash, whole, &style);
                }
                if let Some(((a, da), (b, db))) = ends {
                    s.start.add(&mut e, a, -da, s.width);
                    s.end.add(&mut e, b, db, s.width);
                }
            }
            e
        });
        (fill, outline)
    }

    pub fn bounds(&self) -> [i32; 4] {
        let reach = self.stroke.as_ref().map_or(0.0, |s| s.reach());
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for sub in &self.path.subpaths {
            // The hull of each segment holds it.
            for k in &sub.knots {
                for p in [k.anchor, k.in_, k.out] {
                    b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
                }
            }
        }
        if b[0] > b[2] {
            return [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        }
        [
            (b[0] - reach).floor() as i32 - 1,
            (b[1] - reach).floor() as i32 - 1,
            (b[2] + reach).ceil() as i32 + 2,
            (b[3] + reach).ceil() as i32 + 2,
        ]
    }

    /// Whether a circle at `c` of radius `r` touches what's drawn: inside
    /// the fill, or on the outline.
    pub fn touches(&self, c: Vec2, r: f32) -> bool {
        let lines = self.flattened();
        if self.fill.is_some() && winding(&lines, c) != 0 {
            return true;
        }
        let reach = r + self.stroke.as_ref().map_or(0.5, |s| s.width * 0.5);
        lines.iter().any(|(pts, closed)| {
            let n = pts.len();
            let segs = if *closed { n } else { n.saturating_sub(1) };
            (n == 1 && (pts[0] - c).length() <= reach)
                || (0..segs).any(|i| {
                    let (a, b) = (pts[i], pts[(i + 1) % n]);
                    segment_distance(c, a, b) <= reach
                })
        })
    }
}

/// An open line's ends and the directions it leaves and arrives by.
fn ends(pts: &[Vec2]) -> Option<((Vec2, Vec2), (Vec2, Vec2))> {
    let first = pts.iter().find(|&&p| (p - pts[0]).length_sq() > 1e-8)?;
    let n = pts.len();
    let last = pts
        .iter()
        .rev()
        .find(|&&p| (p - pts[n - 1]).length_sq() > 1e-8)?;
    Some((
        (pts[0], (*first - pts[0]).normalized()),
        (pts[n - 1], (pts[n - 1] - *last).normalized()),
    ))
}

fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let d = b - a;
    let len2 = d.length_sq();
    let t = if len2 > 1e-12 {
        ((p - a).dot(d) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + d * t - p).length()
}

/// How many times `lines` (each closed) wind around `p`.
fn winding(lines: &[(Vec<Vec2>, bool)], p: Vec2) -> i32 {
    let mut w = 0;
    for (pts, _) in lines {
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            if (a.y <= p.y) != (b.y <= p.y) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if x > p.x {
                    w += if b.y > a.y { 1 } else { -1 };
                }
            }
        }
    }
    w
}

impl VectorItem {
    /// `[x0, y0, x1, y1)` around what it draws.
    pub fn bounds(&self) -> [i32; 4] {
        match self {
            VectorItem::Line(l) => l.bounds(),
            VectorItem::Shape(s) => s.bounds(),
            VectorItem::Group(g) => super::union(g.items.iter().map(|i| i.bounds())).unwrap_or([
                i32::MAX,
                i32::MAX,
                i32::MIN,
                i32::MIN,
            ]),
        }
    }

    /// Boxes together covering what it draws, `[x0, y0, x1, y1)`, added to
    /// `out`: a line's piece by piece (a long or diagonal one covers far
    /// less than its box), anything else its box.
    pub fn reach(&self, out: &mut Vec<[i32; 4]>) {
        match self {
            VectorItem::Line(l) => l.reach(out),
            _ => out.push(self.bounds()),
        }
    }

    /// Whether a circle at `c` of radius `r` touches what it draws.
    pub fn touches(&self, c: Vec2, r: f32) -> bool {
        match self {
            VectorItem::Line(l) => l.touches(c, r),
            VectorItem::Shape(s) => s.touches(c, r),
            VectorItem::Group(g) => g.visible && g.items.iter().any(|i| i.touches(c, r)),
        }
    }

    pub fn as_line(&self) -> Option<&VectorStroke> {
        match self {
            VectorItem::Line(l) => Some(l),
            _ => None,
        }
    }

    /// Every point moved through `f` and every width scaled by `scale`
    /// (a move, a crop or an affine transform's size change), lines no
    /// wider than [`super::MAX_WIDTH`].
    pub fn map_points(&mut self, f: &impl Fn(Vec2) -> Vec2, scale: f32) {
        self.map_points_within(f, scale, super::MAX_WIDTH);
    }

    /// [`Self::map_points`] for a picture of it zoomed in: lines as wide
    /// as they come out.
    pub fn map_points_for_view(&mut self, f: &impl Fn(Vec2) -> Vec2, scale: f32) {
        self.map_points_within(f, scale, f32::INFINITY);
    }

    fn map_points_within(&mut self, f: &impl Fn(Vec2) -> Vec2, scale: f32, max_width: f32) {
        match self {
            VectorItem::Line(l) => {
                for p in &mut l.points {
                    let q = f(Vec2::new(p[0], p[1]));
                    *p = [q.x, q.y, (p[2] * scale).clamp(0.0, max_width)];
                }
            }
            VectorItem::Shape(s) => {
                for sub in &mut s.path.subpaths {
                    for k in &mut sub.knots {
                        k.anchor = f(k.anchor);
                        k.in_ = f(k.in_);
                        k.out = f(k.out);
                    }
                }
                if let Some(st) = &mut s.stroke {
                    st.width *= scale;
                    for d in &mut st.dash {
                        *d *= scale;
                    }
                    st.dash_offset *= scale;
                    map_paint(&mut st.paint, f);
                }
                if let Some(fill) = &mut s.fill {
                    map_paint(&mut fill.paint, f);
                }
            }
            VectorItem::Group(g) => {
                for i in &mut g.items {
                    i.map_points_within(f, scale, max_width);
                }
            }
        }
    }

    /// Bytes its points take (for the undo history's size).
    pub fn size_bytes(&self) -> usize {
        match self {
            VectorItem::Line(l) => std::mem::size_of_val(l.points.as_slice()),
            VectorItem::Shape(s) => s
                .path
                .subpaths
                .iter()
                .map(|p| std::mem::size_of_val(p.knots.as_slice()))
                .sum(),
            VectorItem::Group(g) => g.items.iter().map(|i| i.size_bytes()).sum(),
        }
    }
}

/// A gradient's line moved with its shape (a colour stays as it is).
fn map_paint(paint: &mut Paint, f: &impl Fn(Vec2) -> Vec2) {
    if let Paint::Gradient(g) = paint {
        match &mut g.transform {
            // In a space of its own: that space is what moves.
            Some(t) => {
                let o = f(Vec2::new(t[4], t[5]));
                let x = f(Vec2::new(t[4] + t[0], t[5] + t[1])) - o;
                let y = f(Vec2::new(t[4] + t[2], t[5] + t[3])) - o;
                *t = [x.x, x.y, y.x, y.y, o.x, o.y];
            }
            None => {
                let (s, e) = (f(Vec2::from(g.start)), f(Vec2::from(g.end)));
                g.start = [s.x, s.y];
                g.end = [e.x, e.y];
            }
        }
    }
}

/// A paint ready to colour pixels.
pub enum Painter {
    Colour([u8; 4]),
    Gradient {
        gradient: Gradient,
        ramp: Ramp,
        /// Canvas to the gradient's own space.
        inverse: Option<[f32; 6]>,
    },
}

impl Painter {
    pub(super) fn new(paint: &Paint) -> Self {
        match paint {
            Paint::Colour(c) => Painter::Colour(*c),
            Paint::Gradient(g) => Painter::Gradient {
                gradient: Gradient {
                    shape: g.shape,
                    repeat: g.repeat,
                    start: Vec2::from(g.start),
                    end: Vec2::from(g.end),
                    reverse: false,
                },
                // SVG mixes gradients in the stored sRGB values.
                ramp: Ramp::new(&g.stops, crate::canvas::blend_modes::BlendSpace::Gamma, 1.0),
                inverse: g.transform.and_then(invert),
            },
        }
    }

    /// The colour at pixel (`x`, `y`) with `coverage` (0..=1) of it.
    pub(super) fn at(&self, x: i32, y: i32, coverage: f32) -> Color32 {
        match self {
            Painter::Colour([r, g, b, a]) => {
                let a = (*a as f32 * coverage + 0.5) as u8;
                Color32::from_rgba_unmultiplied(*r, *g, *b, a)
            }
            Painter::Gradient {
                gradient,
                ramp,
                inverse,
            } => {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let p = match inverse {
                    Some(m) => Vec2::new(
                        m[0] * p.x + m[2] * p.y + m[4],
                        m[1] * p.x + m[3] * p.y + m[5],
                    ),
                    None => p,
                };
                let cov = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
                ramp.pixel_covered(gradient.position(p), None, cov)
            }
        }
    }
}

/// The inverse of an affine `[a, b, c, d, e, f]`, if it has one.
fn invert(m: [f32; 6]) -> Option<[f32; 6]> {
    let [a, b, c, d, e, f] = m;
    let det = a * d - b * c;
    if det.abs() < 1e-12 {
        return None;
    }
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    Some([ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::path::{Knot, Subpath};

    pub(crate) fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Path {
        Path {
            subpaths: vec![Subpath {
                knots: [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
                    .map(|(x, y)| Knot::corner(Vec2::new(x, y)))
                    .to_vec(),
                closed: true,
            }],
        }
    }

    #[test]
    fn a_shape_touches_inside_its_fill_and_along_its_outline_only() {
        let mut s = PathShape {
            path: rect(10.0, 10.0, 50.0, 30.0),
            fill: Some(Fill {
                paint: Paint::Colour([255, 0, 0, 255]),
                rule: FillRule::NonZero,
            }),
            stroke: Some(StrokeStyle::solid([0, 0, 0, 255], 4.0)),
            opacity: 1.0,
        };
        assert!(s.touches(Vec2::new(30.0, 20.0), 1.0));
        assert!(s.touches(Vec2::new(52.5, 20.0), 1.0));
        assert!(!s.touches(Vec2::new(60.0, 20.0), 1.0));
        s.fill = None;
        assert!(
            !s.touches(Vec2::new(30.0, 20.0), 1.0),
            "an outline is hollow"
        );
        let b = s.bounds();
        assert!(b[0] <= 8 && b[2] >= 52 && b[1] <= 8 && b[3] >= 32, "{b:?}");
    }

    #[test]
    fn items_save_as_tagged_json_and_load_back() {
        let item = VectorItem::Group(VectorGroup {
            name: "Pair".into(),
            opacity: 0.5,
            visible: true,
            items: vec![
                VectorItem::Shape(PathShape {
                    path: rect(0.0, 0.0, 4.0, 4.0),
                    fill: None,
                    stroke: Some(StrokeStyle::solid([1, 2, 3, 255], 2.0)),
                    opacity: 1.0,
                }),
                VectorItem::Line(VectorStroke {
                    points: vec![[0.0, 0.0, 2.0], [5.0, 5.0, 3.0]],
                    colour: [9, 9, 9],
                    opacity: 1.0,
                }),
            ],
        });
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "group");
        assert_eq!(json["items"][0]["kind"], "shape");
        assert_eq!(serde_json::from_value::<VectorItem>(json).unwrap(), item);
    }

    #[test]
    fn an_affine_maps_back_by_its_inverse() {
        let m = [2.0, 0.5, -0.3, 1.5, 10.0, -4.0];
        let inv = invert(m).unwrap();
        let p = Vec2::new(3.0, 7.0);
        let q = Vec2::new(
            m[0] * p.x + m[2] * p.y + m[4],
            m[1] * p.x + m[3] * p.y + m[5],
        );
        let back = Vec2::new(
            inv[0] * q.x + inv[2] * q.y + inv[4],
            inv[1] * q.x + inv[3] * q.y + inv[5],
        );
        assert!((back - p).length() < 1e-4);
    }
}
