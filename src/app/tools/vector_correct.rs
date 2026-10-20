//! Edit Vectors' correcting brushes, as Clip Studio's Correct Line tools:
//! drag over brush lines to simplify them, join their ends, draw part of
//! one again, pull part of one (pinch), thicken or thin them, or set their
//! width from the pen's pressure. Each drag is one undo step.

use super::vector::VectorSession;
use crate::app::PainterApp;
use crate::canvas::vector::{self, VectorItem, VectorStroke};
use eframe::egui::Vec2;

/// What a drag with Edit Vectors does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CorrectMode {
    /// Pick, move and bend items.
    #[default]
    Edit,
    /// Fewer points on the lines brushed, smoother.
    Simplify,
    /// Lines whose ends the drag passes near join into one.
    Connect,
    /// A line drawn along part of one replaces that part.
    Redraw,
    /// The part of a line pressed is pulled along with the pointer.
    Pinch,
    /// Lines brushed get thicker (Shift: thinner).
    Width,
    /// Lines brushed take the width the pen's pressure gives the brush.
    PenWidth,
}

impl CorrectMode {
    pub const ALL: [CorrectMode; 7] = [
        CorrectMode::Edit,
        CorrectMode::Simplify,
        CorrectMode::Connect,
        CorrectMode::Redraw,
        CorrectMode::Pinch,
        CorrectMode::Width,
        CorrectMode::PenWidth,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CorrectMode::Edit => "Edit",
            CorrectMode::Simplify => "Simplify",
            CorrectMode::Connect => "Connect",
            CorrectMode::Redraw => "Redraw",
            CorrectMode::Pinch => "Pinch",
            CorrectMode::Width => "Width",
            CorrectMode::PenWidth => "Width from pen",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            CorrectMode::Edit => "",
            CorrectMode::Simplify => "Brush over lines: fewer points, smoother",
            CorrectMode::Connect => "Brush over two line ends to join them",
            CorrectMode::Redraw => {
                "Draw from one point of a line to another: that part is drawn again"
            }
            CorrectMode::Pinch => "Drag part of a line: it bends along, less further away",
            CorrectMode::Width => "Brush over lines to thicken them, Shift to thin",
            CorrectMode::PenWidth => {
                "Draw along lines: they take the width the pen's pressure gives"
            }
        }
    }
}

/// The correcting brushes' settings, and the drag going on.
pub struct CorrectState {
    /// The brush's radius, canvas pixels.
    pub size: f32,
    /// How far Simplify may move a line, canvas pixels.
    pub simplify: f32,
    /// The pen's pressure now (1 for a mouse).
    pub pressure: f32,
    /// The drag's points, and their pressures.
    pub(crate) path: Vec<(Vec2, f32)>,
    /// Thinning rather than thickening (Shift held).
    invert: bool,
    /// Lines simplified in this drag (each once).
    done: Vec<usize>,
    /// Pinch: the line pressed, the point, and its points then.
    grab: Option<(usize, usize, Vec<[f32; 3]>)>,
}

impl Default for CorrectState {
    fn default() -> Self {
        Self {
            size: 16.0,
            simplify: 2.0,
            pressure: 1.0,
            path: Vec::new(),
            invert: false,
            done: Vec::new(),
            grab: None,
        }
    }
}

/// The line `item` is, if it's one.
fn line_of(item: &VectorItem) -> Option<&VectorStroke> {
    item.as_line()
}

/// Lengths along `points` from point `k` (both ways).
fn arc_from(points: &[[f32; 3]], k: usize) -> Vec<f32> {
    let p = |i: usize| Vec2::new(points[i][0], points[i][1]);
    let mut d = vec![0.0; points.len()];
    for i in k + 1..points.len() {
        d[i] = d[i - 1] + (p(i) - p(i - 1)).length();
    }
    for i in (0..k).rev() {
        d[i] = d[i + 1] + (p(i + 1) - p(i)).length();
    }
    d
}

/// A smooth fall from 1 at 0 to 0 at 1.
fn falloff(t: f32) -> f32 {
    let t = (1.0 - t).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl PainterApp {
    fn correct_radius(&self) -> f32 {
        self.workspace.vector.correct.size.max(1.0)
    }

    pub(crate) fn correct_press(&mut self, idx: usize, pos: Vec2, shift: bool) {
        self.release_canvas_for_lines();
        self.mark_action();
        let layer = &self.canvas.layers[idx];
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: pos,
        });
        let pressure = self.workspace.vector.correct.pressure;
        let r = self.correct_radius();
        // Pinch: the line pressed, with points close enough together to
        // bend smoothly, and one where it's pressed.
        let grab = (self.workspace.vector.mode == CorrectMode::Pinch)
            .then(|| self.nearest_on_line(idx, pos, r))
            .flatten()
            .and_then(|(i, seg, t)| {
                let line = layer.vector.as_ref()?.line(i)?;
                let (points, k) = split_dense(&line.points, seg, t, (r * 0.5).max(1.0));
                Some((i, k, points))
            });
        let c = &mut self.workspace.vector.correct;
        c.path = vec![(pos, pressure)];
        c.invert = shift;
        c.done.clear();
        c.grab = grab;
        self.correct_brush(idx, pos, pos);
    }

    pub(crate) fn correct_drag(&mut self, pos: Vec2) {
        let Some(session) = &self.workspace.vector.session else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(session.layer) else {
            return;
        };
        let pressure = self.workspace.vector.correct.pressure;
        let c = &mut self.workspace.vector.correct;
        let Some(&(last, _)) = c.path.last() else {
            return;
        };
        if (pos - last).length() < 0.5 {
            return;
        }
        c.path.push((pos, pressure));
        self.correct_brush(idx, last, pos);
    }

    pub(crate) fn correct_release(&mut self) {
        let Some(session) = &self.workspace.vector.session else {
            return;
        };
        let idx = self.canvas.layer_index_of(session.layer);
        let mode = self.workspace.vector.mode;
        if let Some(idx) = idx {
            match mode {
                CorrectMode::Connect => self.connect_lines(idx),
                CorrectMode::Redraw => self.redraw_line_part(idx),
                _ => {}
            }
        }
        let c = &mut self.workspace.vector.correct;
        c.path.clear();
        c.grab = None;
        c.done.clear();
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, mode.label());
        }
    }

    /// The brush moved from `from` to `to`: what it does on the way.
    fn correct_brush(&mut self, idx: usize, from: Vec2, to: Vec2) {
        let r = self.correct_radius();
        match self.workspace.vector.mode {
            CorrectMode::Simplify => {
                let tol = self.workspace.vector.correct.simplify.max(0.1);
                let touched = self.lines_near(idx, from, to, r);
                let done = &self.workspace.vector.correct.done;
                let new: Vec<usize> = touched.into_iter().filter(|i| !done.contains(i)).collect();
                self.workspace.vector.correct.done.extend(&new);
                self.change_lines(idx, &new, |_, l| {
                    l.points = vector::simplify(&l.points, tol);
                });
            }
            CorrectMode::Width | CorrectMode::PenWidth => {
                let pen = self.workspace.vector.mode == CorrectMode::PenWidth;
                let sign = if self.workspace.vector.correct.invert {
                    -1.0
                } else {
                    1.0
                };
                let width = self.pen_width(self.workspace.vector.correct.pressure);
                let touched = self.lines_near(idx, from, to, r);
                self.change_lines(idx, &touched, |_, l| {
                    for p in &mut l.points {
                        let d = distance_to_segment(Vec2::new(p[0], p[1]), from, to);
                        if d > r {
                            continue;
                        }
                        let k = falloff(d / r);
                        p[2] = if pen {
                            p[2] + (width - p[2]) * k
                        } else {
                            p[2] * (1.0 + sign * 0.1 * k)
                        }
                        .clamp(0.1, vector::MAX_WIDTH);
                    }
                });
            }
            CorrectMode::Pinch => {
                let Some((i, k, original)) = self.workspace.vector.correct.grab.clone() else {
                    return;
                };
                let start = self.workspace.vector.correct.path[0].0;
                let delta = to - start;
                let reach = r * 4.0;
                let arc = arc_from(&original, k);
                self.change_lines(idx, &[i], |_, l| {
                    l.points = (original.iter().zip(&arc))
                        .map(|(&[x, y, w], &d)| {
                            let m = falloff(d / reach);
                            [x + delta.x * m, y + delta.y * m, w]
                        })
                        .collect();
                });
            }
            CorrectMode::Edit | CorrectMode::Connect | CorrectMode::Redraw => {}
        }
    }

    /// The width the brush gives at `pressure`.
    fn pen_width(&self, pressure: f32) -> f32 {
        let brush = &self.brush_state.brush;
        let k = brush.pressure_scale(crate::brush_engine::dynamics::DabSetting::Size, pressure);
        (brush.brush_options.diameter * k).clamp(0.1, vector::MAX_WIDTH)
    }

    /// The lines of layer `idx` within `r` of segment `a`→`b`.
    fn lines_near(&self, idx: usize, a: Vec2, b: Vec2, r: f32) -> Vec<usize> {
        let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
            return Vec::new();
        };
        let bx = vector::segment_box(a, b);
        let ri = r.ceil() as i32 + 1;
        v.candidates([bx[0] - ri, bx[1] - ri, bx[2] + ri, bx[3] + ri])
            .into_iter()
            .filter(|&i| {
                line_of(&v.items()[i]).is_some_and(|l| {
                    let steps = ((b - a).length() / (r * 0.5).max(0.5)).ceil().max(1.0) as usize;
                    (0..=steps).any(|s| l.touches(a + (b - a) * (s as f32 / steps as f32), r))
                })
            })
            .collect()
    }

    /// The place on a line of layer `idx` nearest `pos`, within `r`: the
    /// line, the segment (from point `seg` to the next) and how far along.
    fn nearest_on_line(&self, idx: usize, pos: Vec2, r: f32) -> Option<(usize, usize, f32)> {
        let v = self.canvas.layers[idx].vector.as_ref()?;
        let ri = r.ceil() as i32 + 1;
        let (x, y) = (pos.x.floor() as i32, pos.y.floor() as i32);
        let mut best: Option<(f32, usize, usize, f32)> = None;
        for i in v.candidates([x - ri, y - ri, x + ri + 1, y + ri + 1]) {
            let Some(l) = line_of(&v.items()[i]) else {
                continue;
            };
            let (seg, t, d) = project_on(&l.points, pos);
            if d <= r && best.is_none_or(|(b, ..)| d < b) {
                best = Some((d, i, seg, t));
            }
        }
        best.map(|(_, i, seg, t)| (i, seg, t))
    }

    /// Change lines `which` of layer `idx` with `change`, drawing them
    /// again where they were and are.
    fn change_lines(
        &mut self,
        idx: usize,
        which: &[usize],
        change: impl Fn(usize, &mut VectorStroke),
    ) {
        if which.is_empty() {
            return;
        }
        let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
            return;
        };
        let old = vector::union(which.iter().map(|&i| v.bounds(i)));
        let mut new = None;
        self.edit_vector(idx, |v| {
            for &i in which {
                if v.line(i).is_some() {
                    v.edit(i, |l| change(i, l));
                }
            }
            new = vector::union(which.iter().map(|&i| v.bounds(i)));
        });
        if let Some(region) = vector::union(old.into_iter().chain(new)) {
            self.redraw_vector(idx, region);
        }
    }

    /// Connect: the line ends the drag passed near join, nearest pairs
    /// first (each end once).
    fn connect_lines(&mut self, idx: usize) {
        let r = self.correct_radius();
        let path: Vec<Vec2> = self
            .workspace
            .vector
            .correct
            .path
            .iter()
            .map(|p| p.0)
            .collect();
        let near_path = |p: Vec2| {
            path.windows(2)
                .any(|w| distance_to_segment(p, w[0], w[1]) <= r)
                || path.first().is_some_and(|&q| (q - p).length() <= r)
        };
        loop {
            let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
                return;
            };
            // Every end near the drag: (line, at its start?, where).
            let ends: Vec<(usize, bool, Vec2)> = (0..v.len())
                .filter_map(|i| v.line(i).map(|l| (i, l)))
                .flat_map(|(i, l)| {
                    let first = l.points.first().map(|p| (i, true, Vec2::new(p[0], p[1])));
                    let last = l.points.last().map(|p| (i, false, Vec2::new(p[0], p[1])));
                    [first, last].into_iter().flatten()
                })
                .filter(|&(_, _, p)| near_path(p))
                .collect();
            // (How far apart, and the two ends: line, at its start?)
            type End = (usize, bool);
            let mut best: Option<(f32, End, End)> = None;
            for (n, a) in ends.iter().enumerate() {
                for b in &ends[n + 1..] {
                    if a.0 == b.0 {
                        continue;
                    }
                    let d = (a.2 - b.2).length();
                    if best.is_none_or(|(bd, ..)| d < bd) {
                        best = Some((d, (a.0, a.1), (b.0, b.1)));
                    }
                }
            }
            let Some((_, (ia, a_start), (ib, b_start))) = best else {
                return;
            };
            let (Some(la), Some(lb)) = (v.line(ia), v.line(ib)) else {
                return;
            };
            let mut a = la.points.clone();
            let mut b = lb.points.clone();
            if a_start {
                a.reverse();
            }
            if !b_start {
                b.reverse();
            }
            a.extend(b);
            let region = vector::union([v.bounds(ia), v.bounds(ib)]);
            let (keep, gone) = (ia.min(ib), ia.max(ib));
            let joined = VectorStroke {
                points: a,
                ..(if keep == ia { la } else { lb }).clone()
            };
            self.edit_vector(idx, |v| {
                v.splice(vec![
                    (keep, vec![VectorItem::Line(joined)]),
                    (gone, Vec::new()),
                ]);
            });
            if let Some(region) = region {
                self.redraw_vector(idx, region);
            }
            self.workspace.vector.line_edit = None;
        }
    }

    /// Redraw: the drag went from near one place on a line to near another
    /// on the same line; the line between them becomes the drag.
    fn redraw_line_part(&mut self, idx: usize) {
        let r = self.correct_radius();
        let drawn = self.workspace.vector.correct.path.clone();
        let (Some(&(first, _)), Some(&(last, _))) = (drawn.first(), drawn.last()) else {
            return;
        };
        if drawn.len() < 2 {
            return;
        }
        let (Some((ia, sa, ta)), Some((ib, sb, tb))) = (
            self.nearest_on_line(idx, first, r),
            self.nearest_on_line(idx, last, r),
        ) else {
            return;
        };
        if ia != ib || (sa as f32 + ta - sb as f32 - tb).abs() < 1e-3 {
            return;
        }
        self.change_lines(idx, &[ia], |_, l| {
            let width_at = |seg: usize, t: f32| {
                let a = l.points[seg][2];
                let b = l.points.get(seg + 1).map_or(a, |p| p[2]);
                a + (b - a) * t
            };
            let (w0, w1) = (width_at(sa, ta), width_at(sb, tb));
            let n = drawn.len().max(2) - 1;
            let mut middle: Vec<[f32; 3]> = drawn
                .iter()
                .enumerate()
                .map(|(i, (p, _))| [p.x, p.y, w0 + (w1 - w0) * i as f32 / n as f32])
                .collect();
            // Along the line: from the earlier place to the later.
            let ((lo, _), (hi, _)) = if (sa as f32 + ta) <= (sb as f32 + tb) {
                ((sa, ta), (sb, tb))
            } else {
                middle.reverse();
                ((sb, tb), (sa, ta))
            };
            let middle = vector::simplify(&middle, 0.3);
            let mut points = l.points[..=lo].to_vec();
            points.extend(middle);
            points.extend_from_slice(&l.points[(hi + 1).min(l.points.len())..]);
            l.points = points;
        });
    }
}

/// Where on polyline `points` is nearest `p`: the segment (from that point
/// to the next), how far along it, and how far away.
fn project_on(points: &[[f32; 3]], p: Vec2) -> (usize, f32, f32) {
    let at = |i: usize| Vec2::new(points[i][0], points[i][1]);
    if points.len() < 2 {
        return (
            0,
            0.0,
            points.first().map_or(f32::MAX, |_| (at(0) - p).length()),
        );
    }
    let mut best = (0, 0.0, f32::MAX);
    for i in 0..points.len() - 1 {
        let (a, d) = (at(i), at(i + 1) - at(i));
        let t = if d.length_sq() < 1e-9 {
            0.0
        } else {
            ((p - a).dot(d) / d.length_sq()).clamp(0.0, 1.0)
        };
        let dist = (a + d * t - p).length();
        if dist < best.2 {
            best = (i, t, dist);
        }
    }
    best
}

/// `points` with points added so none is more than `step` from the next,
/// and one at `t` along segment `seg`; and that one's index.
fn split_dense(points: &[[f32; 3]], seg: usize, t: f32, step: f32) -> (Vec<[f32; 3]>, usize) {
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t);
    let mut out = Vec::new();
    let mut k = 0;
    for i in 0..points.len() {
        out.push(points[i]);
        let Some(&next) = points.get(i + 1) else {
            break;
        };
        let len = Vec2::new(next[0] - points[i][0], next[1] - points[i][1]).length();
        let n = (len / step).ceil().max(1.0) as usize;
        let mut ts: Vec<f32> = (1..n).map(|j| j as f32 / n as f32).collect();
        if i == seg && t > 0.0 && t < 1.0 {
            ts.push(t);
            ts.sort_by(f32::total_cmp);
        }
        for u in ts {
            if i == seg && u == t {
                k = out.len();
            }
            out.push(lerp(points[i], next, u));
        }
        if i == seg && t >= 1.0 {
            k = out.len();
        }
    }
    if seg == 0 && t <= 0.0 {
        k = 0;
    }
    (out, k)
}

/// How far `p` is from segment `a`→`b`.
fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let d = b - a;
    let t = if d.length_sq() < 1e-9 {
        0.0
    } else {
        ((p - a).dot(d) / d.length_sq()).clamp(0.0, 1.0)
    };
    (a + d * t - p).length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use eframe::egui::Color32;

    fn app() -> PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(200, 120, Color32::WHITE, 64));
        app.selection_manager.canvas_size = [200, 120];
        app.canvas_mut().active_layer_idx = 1;
        app.add_vector_layer();
        app.viewport.zoom = 1.0;
        let b = &mut app.brush_state.brush.brush_options;
        b.color = Color32::BLACK;
        b.diameter = 4.0;
        b.pressure_size = false;
        app
    }

    fn line(app: &mut PainterApp, from: Vec2, to: Vec2) {
        app.active_tool = crate::app::tools::Tool::Brush;
        app.start_stroke_with_pressure(from, 1.0);
        for i in 1..=20 {
            app.add_stroke_point(from + (to - from) * (i as f32 / 20.0), 1.0);
        }
        app.finish_stroke();
    }

    fn drag(app: &mut PainterApp, mode: CorrectMode, points: &[Vec2], shift: bool) {
        app.active_tool = crate::app::tools::Tool::VectorEdit;
        app.workspace.vector.mode = mode;
        app.line_edit_press(points[0], shift, false);
        for &p in &points[1..] {
            app.line_edit_drag(p);
        }
        app.line_edit_release();
    }

    fn lines(app: &PainterApp) -> Vec<VectorStroke> {
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        (0..v.len()).filter_map(|i| v.line(i).cloned()).collect()
    }

    #[test]
    fn connect_joins_two_lines_whose_ends_it_passes() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 50.0), Vec2::new(80.0, 50.0));
        line(&mut app, Vec2::new(90.0, 52.0), Vec2::new(180.0, 52.0));
        drag(
            &mut app,
            CorrectMode::Connect,
            &[Vec2::new(78.0, 51.0), Vec2::new(92.0, 51.0)],
            false,
        );
        let l = lines(&app);
        assert_eq!(l.len(), 1, "joined");
        let (a, b) = (l[0].points[0], *l[0].points.last().unwrap());
        assert!(
            (a[0] - 10.0).abs() < 2.0 && (b[0] - 180.0).abs() < 2.0,
            "{a:?} {b:?}"
        );
        app.apply_history(false);
        assert_eq!(lines(&app).len(), 2, "undo");
    }

    #[test]
    fn width_thickens_and_thins_where_brushed() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 50.0), Vec2::new(180.0, 50.0));
        let w0 = lines(&app)[0].points[0][2];
        let over: Vec<Vec2> = (0..10).map(|i| Vec2::new(5.0 + i as f32, 50.0)).collect();
        drag(&mut app, CorrectMode::Width, &over, false);
        let l = &lines(&app)[0];
        assert!(l.points[0][2] > w0, "thicker at the start");
        assert!(
            (l.points.last().unwrap()[2] - w0).abs() < 1e-3,
            "not at the far end"
        );
        drag(&mut app, CorrectMode::Width, &over, true);
        drag(&mut app, CorrectMode::Width, &over, true);
        assert!(
            lines(&app)[0].points[0][2] < l.points[0][2],
            "thinner again"
        );
    }

    #[test]
    fn pinch_pulls_the_line_most_where_pressed() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 50.0), Vec2::new(190.0, 50.0));
        drag(
            &mut app,
            CorrectMode::Pinch,
            &[Vec2::new(100.0, 50.0), Vec2::new(100.0, 80.0)],
            false,
        );
        let l = &lines(&app)[0];
        let max_y = l.points.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert!((max_y - 80.0).abs() < 3.0, "{max_y}");
        assert!((l.points[0][1] - 50.0).abs() < 1e-3, "the ends stay");
    }

    #[test]
    fn redraw_replaces_the_part_between_where_it_starts_and_ends() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 50.0), Vec2::new(190.0, 50.0));
        let bump: Vec<Vec2> = (0..=20)
            .map(|i| {
                let x = 60.0 + i as f32 * 4.0;
                Vec2::new(
                    x,
                    50.0 - 30.0 * (std::f32::consts::PI * i as f32 / 20.0).sin(),
                )
            })
            .collect();
        drag(&mut app, CorrectMode::Redraw, &bump, false);
        let l = &lines(&app)[0];
        let min_y = l.points.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        assert!(min_y < 25.0, "the bump is in: {min_y}");
        assert!(
            (l.points[0][0] - 10.0).abs() < 2.0
                && (l.points.last().unwrap()[0] - 190.0).abs() < 2.0
        );
    }

    #[test]
    fn simplify_takes_points_out() {
        let mut app = app();
        // A wobbly line: many points.
        app.active_tool = crate::app::tools::Tool::Brush;
        app.start_stroke_with_pressure(Vec2::new(10.0, 50.0), 1.0);
        for i in 1..=80 {
            let x = 10.0 + i as f32 * 2.0;
            app.add_stroke_point(
                Vec2::new(x, 50.0 + if i % 2 == 0 { 1.0 } else { -1.0 }),
                1.0,
            );
        }
        app.finish_stroke();
        let before = lines(&app)[0].points.len();
        app.workspace.vector.correct.simplify = 3.0;
        drag(
            &mut app,
            CorrectMode::Simplify,
            &[Vec2::new(90.0, 50.0), Vec2::new(91.0, 50.0)],
            false,
        );
        let after = lines(&app)[0].points.len();
        assert!(after < before / 2, "{before} → {after}");
    }
}
