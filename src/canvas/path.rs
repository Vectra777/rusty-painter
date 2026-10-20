//! Bézier paths, as the Pen tool draws them: subpaths of anchors, each with
//! the handle the curve leaves it by (`out`) and the one it arrives by
//! (`in_`). A corner's handles sit on its anchor; a smooth point's line up
//! through it. A closed subpath curves from its last anchor back to its
//! first.
//!
//! Everything here is geometry on canvas points: flattening to polylines,
//! finding what's under the pointer, and the Pen's edits (add a point
//! without changing the curve, delete one, turn a corner smooth or back).

use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Knot {
    #[serde(with = "vec2")]
    pub anchor: Vec2,
    /// The handle the curve arrives by.
    #[serde(with = "vec2")]
    pub in_: Vec2,
    /// The handle the curve leaves by.
    #[serde(with = "vec2")]
    pub out: Vec2,
}

impl Knot {
    /// A corner at `p`: its handles on it.
    pub fn corner(p: Vec2) -> Self {
        Self {
            anchor: p,
            in_: p,
            out: p,
        }
    }

    /// A smooth point at `p` leaving towards `out`, arriving from its
    /// mirror image.
    pub fn smooth(p: Vec2, out: Vec2) -> Self {
        Self {
            anchor: p,
            in_: p * 2.0 - out,
            out,
        }
    }

    pub fn is_corner(&self) -> bool {
        (self.in_ - self.anchor).length() < 0.01 && (self.out - self.anchor).length() < 0.01
    }

    /// The anchor and both handles moved by `d`.
    pub fn moved(mut self, d: Vec2) -> Self {
        self.anchor += d;
        self.in_ += d;
        self.out += d;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Subpath {
    pub knots: Vec<Knot>,
    pub closed: bool,
}

impl Subpath {
    /// Its segments as cubic Béziers (start, its out handle, the end's in
    /// handle, end), the closing one too.
    pub fn segments(&self) -> impl Iterator<Item = [Vec2; 4]> + '_ {
        let n = self.knots.len();
        let count = if self.closed && n > 1 {
            n
        } else {
            n.saturating_sub(1)
        };
        (0..count).map(move |i| {
            let (a, b) = (self.knots[i], self.knots[(i + 1) % n]);
            [a.anchor, a.out, b.in_, b.anchor]
        })
    }

    /// The curve as points about `step` apart along it: the anchors and
    /// pieces of each segment in between. A closed one ends back at its
    /// start.
    pub fn flatten(&self, step: f32) -> Vec<Vec2> {
        let Some(first) = self.knots.first() else {
            return Vec::new();
        };
        let mut out = vec![first.anchor];
        for [a, b, c, d] in self.segments() {
            if b == a && c == d {
                // Straight: its end is enough (spacing is the brush's job).
                out.push(d);
                continue;
            }
            let hull = (b - a).length() + (c - b).length() + (d - c).length();
            let n = ((hull / step.max(0.1)).ceil() as usize).clamp(1, 1024);
            for i in 1..=n {
                out.push(bezier([a, b, c, d], i as f32 / n as f32));
            }
        }
        out
    }

    /// The curve as a polyline within `tol` of it everywhere: each curved
    /// segment in as many equal steps as its bend needs (Wang's bound), a
    /// straight one as its end. A closed one doesn't repeat its start.
    pub fn flatten_tol(&self, tol: f32) -> Vec<Vec2> {
        let Some(first) = self.knots.first() else {
            return Vec::new();
        };
        let mut out = vec![first.anchor];
        for [a, b, c, d] in self.segments() {
            if b == a && c == d {
                out.push(d);
                continue;
            }
            let bend = (a - b * 2.0 + c).length().max((b - c * 2.0 + d).length());
            let n = ((0.75 * bend / tol.max(1e-3)).sqrt().ceil() as usize).clamp(1, 1024);
            for i in 1..=n {
                out.push(bezier([a, b, c, d], i as f32 / n as f32));
            }
        }
        if self.closed && out.len() > 1 && (out[0] - out[out.len() - 1]).length_sq() < 1e-8 {
            out.pop();
        }
        out
    }
}

/// The point at `t` (0..=1) along a cubic Bézier.
pub fn bezier([a, b, c, d]: [Vec2; 4], t: f32) -> Vec2 {
    let u = 1.0 - t;
    a * (u * u * u) + b * (3.0 * u * u * t) + c * (3.0 * u * t * t) + d * (t * t * t)
}

/// A point of a path: subpath and knot.
pub type KnotRef = (usize, usize);

/// A handle of a knot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    In,
    Out,
}

/// What's under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Anchor(KnotRef),
    Handle(KnotRef, Handle),
    /// Segment `i` of a subpath (from its knot `i`), at `t` along it.
    Segment(usize, usize, f32),
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Path {
    pub subpaths: Vec<Subpath>,
}

impl Path {
    pub fn is_empty(&self) -> bool {
        self.subpaths.iter().all(|s| s.knots.is_empty())
    }

    pub fn knot(&self, (s, k): KnotRef) -> Option<&Knot> {
        self.subpaths.get(s)?.knots.get(k)
    }

    pub fn knot_mut(&mut self, (s, k): KnotRef) -> Option<&mut Knot> {
        self.subpaths.get_mut(s)?.knots.get_mut(k)
    }

    /// Each subpath as points `step` apart.
    pub fn flatten(&self, step: f32) -> Vec<Vec<Vec2>> {
        self.subpaths
            .iter()
            .map(|s| s.flatten(step))
            .filter(|p| p.len() > 1)
            .collect()
    }

    /// What's within `tol` of `p`: a handle shown (`handles_of`'s, those
    /// apart from their anchor), else an anchor, else a segment; nearest
    /// first in each.
    pub fn hit(&self, p: Vec2, tol: f32, handles_of: &[KnotRef]) -> Option<Hit> {
        let near = |q: Vec2| (q - p).length() <= tol;
        for &r in handles_of {
            let Some(k) = self.knot(r) else { continue };
            for (h, at) in [(Handle::Out, k.out), (Handle::In, k.in_)] {
                if at != k.anchor && near(at) {
                    return Some(Hit::Handle(r, h));
                }
            }
        }
        let mut best: Option<(f32, Hit)> = None;
        for (s, sub) in self.subpaths.iter().enumerate() {
            for (k, knot) in sub.knots.iter().enumerate() {
                let d = (knot.anchor - p).length();
                if d <= tol && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, Hit::Anchor((s, k))));
                }
            }
        }
        if best.is_some() {
            return best.map(|(_, h)| h);
        }
        for (s, sub) in self.subpaths.iter().enumerate() {
            for (i, seg) in sub.segments().enumerate() {
                let (d, t) = nearest_on(seg, p);
                if d <= tol && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, Hit::Segment(s, i, t)));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// A smooth point added on segment `i` of subpath `s` at `t`, the curve
    /// keeping its shape (de Casteljau); returns it.
    pub fn split(&mut self, s: usize, i: usize, t: f32) -> Option<KnotRef> {
        let sub = self.subpaths.get_mut(s)?;
        let n = sub.knots.len();
        let j = (i + 1) % n;
        let [a, b, c, d] = sub.segments().nth(i)?;
        let lerp = |p: Vec2, q: Vec2| p + (q - p) * t;
        let (ab, bc, cd) = (lerp(a, b), lerp(b, c), lerp(c, d));
        let (abc, bcd) = (lerp(ab, bc), lerp(bc, cd));
        let mid = lerp(abc, bcd);
        sub.knots[i].out = ab;
        sub.knots[j].in_ = cd;
        let new = Knot {
            anchor: mid,
            in_: abc,
            out: bcd,
        };
        sub.knots.insert(i + 1, new);
        Some((s, i + 1))
    }

    /// Remove a point, its neighbours joined by the handles they had; a
    /// subpath left with one point goes.
    pub fn delete(&mut self, (s, k): KnotRef) {
        let Some(sub) = self.subpaths.get_mut(s) else {
            return;
        };
        if k >= sub.knots.len() {
            return;
        }
        sub.knots.remove(k);
        if sub.knots.len() < 2 {
            self.subpaths.remove(s);
        }
    }

    /// A smooth point becomes a corner; a corner, smooth (its handles
    /// along the line between its neighbours, a third of the way to them).
    pub fn convert(&mut self, (s, k): KnotRef) {
        let Some(sub) = self.subpaths.get_mut(s) else {
            return;
        };
        let n = sub.knots.len();
        let Some(knot) = sub.knots.get(k).copied() else {
            return;
        };
        if !knot.is_corner() {
            sub.knots[k] = Knot::corner(knot.anchor);
            return;
        }
        let prev = (k > 0 || sub.closed).then(|| sub.knots[(k + n - 1) % n].anchor);
        let next = (k + 1 < n || sub.closed).then(|| sub.knots[(k + 1) % n].anchor);
        let (dir, reach) = match (prev, next) {
            (Some(p), Some(q)) => (
                q - p,
                ((q - knot.anchor).length() + (knot.anchor - p).length()) / 6.0,
            ),
            (Some(p), None) => (knot.anchor - p, (knot.anchor - p).length() / 3.0),
            (None, Some(q)) => (q - knot.anchor, (q - knot.anchor).length() / 3.0),
            (None, None) => return,
        };
        let dir = dir.normalized();
        if !dir.x.is_finite() {
            return;
        }
        sub.knots[k] = Knot::smooth(knot.anchor, knot.anchor + dir * reach);
    }

    /// Move a handle to `to`; on a smooth point the other turns with it
    /// (keeping its length), unless `alone` (Alt).
    pub fn move_handle(&mut self, r: KnotRef, h: Handle, to: Vec2, alone: bool) {
        let Some(k) = self.knot_mut(r) else {
            return;
        };
        let smooth = !k.is_corner() && {
            let (a, b) = (k.in_ - k.anchor, k.out - k.anchor);
            a.length() > 0.01 && b.length() > 0.01 && a.normalized().dot(b.normalized()) < -0.99
        };
        let (this, other) = match h {
            Handle::In => (&mut k.in_, &mut k.out),
            Handle::Out => (&mut k.out, &mut k.in_),
        };
        *this = to;
        if smooth && !alone {
            let d = to - k.anchor;
            if d.length() > 0.01 {
                let len = (*other - k.anchor).length();
                *other = k.anchor - d.normalized() * len;
            }
        }
    }
}

/// How far `p` is from a cubic Bézier, and where along it (0..=1) the
/// nearest point is: sampled, then refined.
fn nearest_on(seg: [Vec2; 4], p: Vec2) -> (f32, f32) {
    let dist = |t: f32| (bezier(seg, t) - p).length();
    let mut best = (f32::MAX, 0.0);
    for i in 0..=64 {
        let t = i as f32 / 64.0;
        let d = dist(t);
        if d < best.0 {
            best = (d, t);
        }
    }
    // Halve the step around the best a few times.
    let mut step = 1.0 / 64.0;
    for _ in 0..8 {
        step /= 2.0;
        for t in [best.1 - step, best.1 + step] {
            let t = t.clamp(0.0, 1.0);
            let d = dist(t);
            if d < best.0 {
                best = (d, t);
            }
        }
    }
    best
}

/// `Vec2` as `[x, y]` in saved files.
mod vec2 {
    use eframe::egui::Vec2;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec2, s: S) -> Result<S::Ok, S::Error> {
        [v.x, v.y].serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec2, D::Error> {
        let [x, y] = <[f32; 2]>::deserialize(d)?;
        Ok(Vec2::new(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    /// A smooth arch from (0, 0) to (100, 0) through about (50, -38).
    fn arch() -> Path {
        Path {
            subpaths: vec![Subpath {
                knots: vec![
                    Knot {
                        anchor: v(0.0, 0.0),
                        in_: v(0.0, 0.0),
                        out: v(0.0, -50.0),
                    },
                    Knot {
                        anchor: v(100.0, 0.0),
                        in_: v(100.0, -50.0),
                        out: v(100.0, 0.0),
                    },
                ],
                closed: false,
            }],
        }
    }

    #[test]
    fn adding_a_point_keeps_the_curve() {
        let mut p = arch();
        let before = p.flatten(1.0)[0].clone();
        let (s, k) = p.split(0, 0, 0.5).unwrap();
        assert_eq!((s, k), (0, 1));
        let added = p.knot((0, 1)).unwrap();
        assert!((added.anchor - v(50.0, -37.5)).length() < 1e-3, "{added:?}");
        // Every point of the old curve is on the new one.
        let after = p.flatten(0.5)[0].clone();
        for q in before {
            let d = after
                .iter()
                .map(|a| (*a - q).length())
                .fold(f32::MAX, f32::min);
            assert!(d < 0.6, "{q:?} is {d} away");
        }
    }

    #[test]
    fn hits_prefer_handles_then_anchors_then_segments() {
        let p = arch();
        assert_eq!(
            p.hit(v(0.0, -50.0), 4.0, &[(0, 0)]),
            Some(Hit::Handle((0, 0), Handle::Out))
        );
        assert_eq!(p.hit(v(0.0, -50.0), 4.0, &[]), None, "a handle not shown");
        assert_eq!(p.hit(v(99.0, 1.0), 4.0, &[]), Some(Hit::Anchor((0, 1))));
        let Some(Hit::Segment(0, 0, t)) = p.hit(v(50.0, -36.0), 4.0, &[]) else {
            panic!("on the curve");
        };
        assert!((t - 0.5).abs() < 0.05);
    }

    #[test]
    fn points_convert_delete_and_their_handles_turn_together() {
        let mut p = arch();
        p.convert((0, 1));
        assert!(p.knot((0, 1)).unwrap().is_corner(), "smooth to corner");
        p.convert((0, 1));
        assert!(!p.knot((0, 1)).unwrap().is_corner(), "and back");
        // A smooth point's handles stay opposite unless dragged alone.
        let mut q = Path {
            subpaths: vec![Subpath {
                knots: vec![Knot::smooth(v(0.0, 0.0), v(10.0, 0.0))],
                closed: false,
            }],
        };
        q.move_handle((0, 0), Handle::Out, v(0.0, 20.0), false);
        assert!((q.knot((0, 0)).unwrap().in_ - v(0.0, -10.0)).length() < 1e-4);
        q.move_handle((0, 0), Handle::Out, v(20.0, 0.0), true);
        assert!(
            (q.knot((0, 0)).unwrap().in_ - v(0.0, -10.0)).length() < 1e-4,
            "broken"
        );
        // Deleting down to one point drops the subpath.
        p.delete((0, 0));
        assert!(p.subpaths.is_empty());
    }

    #[test]
    fn closed_subpaths_flatten_back_to_their_start_and_save() {
        let p = Path {
            subpaths: vec![Subpath {
                knots: vec![
                    Knot::corner(v(0.0, 0.0)),
                    Knot::corner(v(10.0, 0.0)),
                    Knot::corner(v(0.0, 10.0)),
                ],
                closed: true,
            }],
        };
        let pts = &p.flatten(1.0)[0];
        assert_eq!(
            (pts.len(), pts[0], pts[pts.len() - 1]),
            (4, v(0.0, 0.0), v(0.0, 0.0))
        );
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Path>(&json).unwrap(), p);
    }
}
