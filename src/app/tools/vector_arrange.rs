//! Edit Vectors on several items at once: picking them with a box (those
//! it touches, or those wholly inside), lining them up and spreading them
//! out, flipping them, combining shapes (union, intersect, subtract,
//! exclude), points snapping to other items' points, and a line becoming
//! a curve ruler to draw along.

use super::vector::{LineEdit, VectorSession};
use crate::app::PainterApp;
use crate::canvas::path::{Knot, Path, Subpath};
use crate::canvas::vector::{self, PathShape, VectorItem};
use eframe::egui::Vec2;

/// What a box picks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoxPick {
    /// Every item it touches (Clip Studio's "overlapping vectors").
    #[default]
    Touching,
    /// Only items wholly inside it ("vectors in area").
    Inside,
}

/// Which way to line up picked items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
    Right,
    Top,
    Middle,
    Bottom,
}

/// How shapes combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Union,
    Intersect,
    /// The bottom shape less the others.
    Subtract,
    /// Where an odd number of them overlap.
    Exclude,
}

impl Combine {
    pub const ALL: [Combine; 4] = [
        Combine::Union,
        Combine::Intersect,
        Combine::Subtract,
        Combine::Exclude,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Combine::Union => "Union",
            Combine::Intersect => "Intersect",
            Combine::Subtract => "Subtract",
            Combine::Exclude => "Exclude",
        }
    }
}

/// An item (by index) and its bounds.
type Boxed = (usize, [f32; 4]);

/// `[x0, y0, x1, y1]` as floats.
fn rect_f(b: [i32; 4]) -> [f32; 4] {
    b.map(|v| v as f32)
}

/// Whether segment `a`→`b` reaches into rectangle `r` (Liang–Barsky).
fn segment_meets_rect(a: Vec2, b: Vec2, r: [f32; 4]) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-d.x, a.x - r[0]),
        (d.x, r[2] - a.x),
        (-d.y, a.y - r[1]),
        (d.y, r[3] - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Whether `item` touches rectangle `r` (its line or outline crosses it,
/// or the rectangle is inside its fill).
fn item_meets_rect(item: &VectorItem, r: [f32; 4]) -> bool {
    match item {
        VectorItem::Line(l) => {
            let pts: Vec<Vec2> = l.smoothed().iter().map(|p| Vec2::new(p[0], p[1])).collect();
            match pts.as_slice() {
                [p] => segment_meets_rect(*p, *p, r),
                _ => pts.windows(2).any(|w| segment_meets_rect(w[0], w[1], r)),
            }
        }
        VectorItem::Shape(s) => {
            let centre = Vec2::new((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5);
            s.flattened().iter().any(|(pts, closed)| {
                let n = pts.len();
                let segs = if *closed { n } else { n.saturating_sub(1) };
                (0..segs).any(|i| segment_meets_rect(pts[i], pts[(i + 1) % n], r))
            }) || (s.fill.is_some() && s.touches(centre, 0.0))
        }
        VectorItem::Group(g) => g.visible && g.items.iter().any(|i| item_meets_rect(i, r)),
    }
}

impl PainterApp {
    /// The box being dragged out with Edit Vectors, canvas points.
    pub(crate) fn vector_box(&self) -> Option<[Vec2; 2]> {
        self.workspace.vector.marquee
    }

    pub(crate) fn vector_box_start(&mut self, pos: Vec2) {
        self.workspace.vector.marquee = Some([pos, pos]);
    }

    pub(crate) fn vector_box_drag(&mut self, pos: Vec2) -> bool {
        match self.workspace.vector.marquee.as_mut() {
            Some(m) => {
                m[1] = pos;
                true
            }
            None => false,
        }
    }

    /// The box let go: what it picks is picked (added to the picked items
    /// with `add`). Returns whether there was a box.
    pub(crate) fn vector_box_end(&mut self, add: bool) -> bool {
        let Some([a, b]) = self.workspace.vector.marquee.take() else {
            return false;
        };
        let idx = self.canvas.active_layer_idx;
        let Some(v) = self.canvas.layers.get(idx).and_then(|l| l.vector.as_ref()) else {
            return true;
        };
        let r = [a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)];
        // A click rather than a drag: nothing.
        if (r[2] - r[0]).max(r[3] - r[1]) * self.viewport.zoom < 3.0 {
            if !add {
                self.workspace.vector.line_edit = None;
            }
            return true;
        }
        let near = v.candidates([
            r[0].floor() as i32,
            r[1].floor() as i32,
            r[2].ceil() as i32 + 1,
            r[3].ceil() as i32 + 1,
        ]);
        let inside = self.workspace.vector.box_pick == BoxPick::Inside;
        let mut picked: Vec<usize> = near
            .into_iter()
            .filter(|&i| {
                let item = &v.items()[i];
                if inside {
                    let ib = rect_f(item.bounds());
                    ib[0] >= r[0] && ib[1] >= r[1] && ib[2] <= r[2] && ib[3] <= r[3]
                } else {
                    item_meets_rect(item, r)
                }
            })
            .collect();
        let id = self.canvas.layers[idx].id;
        if add
            && let Some(edit) = &self.workspace.vector.line_edit
            && edit.layer == id
        {
            let mut had: Vec<usize> = std::iter::once(edit.line)
                .chain(edit.extra.iter().copied())
                .collect();
            for i in picked {
                if !had.contains(&i) {
                    had.push(i);
                }
            }
            picked = had;
        }
        self.workspace.vector.line_edit = picked.split_first().map(|(&first, rest)| {
            let mut edit = LineEdit::picking(id, first);
            edit.extra = rest.to_vec();
            edit
        });
        true
    }

    /// Change each picked item with `change` (given its index, it returns
    /// the item's new form, or `None` to leave it), as one undo step
    /// called `label`; they stay picked.
    pub(crate) fn change_picked_each(
        &mut self,
        label: &str,
        mut change: impl FnMut(usize, &VectorItem) -> Option<VectorItem>,
    ) {
        let Some((idx, all)) = self.picked_items() else {
            return;
        };
        self.release_canvas_for_lines();
        self.mark_action();
        self.forget_vector_change();
        let layer = &self.canvas.layers[idx];
        let Some(v) = layer.vector.as_ref() else {
            return;
        };
        let changes: Vec<(usize, VectorItem)> = all
            .iter()
            .filter_map(|&i| change(i, &v.items()[i]).map(|new| (i, new)))
            .collect();
        if changes.is_empty() {
            return;
        }
        // Drawn again where they were and are, piece by piece.
        let mut boxes = Vec::new();
        for (i, item) in &changes {
            v.items()[*i].reach(&mut boxes);
            item.reach(&mut boxes);
        }
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: Vec2::ZERO,
        });
        self.edit_vector(idx, |v| {
            for (i, item) in changes {
                v.edit_item(i, |it| *it = item);
            }
        });
        self.redraw_vector_boxes(idx, &boxes);
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, label);
        }
    }

    /// The picked items' bounds together, and each one's.
    fn picked_bounds(&self) -> Option<([f32; 4], Vec<Boxed>)> {
        let (idx, all) = self.picked_items()?;
        let v = self.canvas.layers[idx].vector.as_ref()?;
        let each: Vec<(usize, [f32; 4])> = all.iter().map(|&i| (i, rect_f(v.bounds(i)))).collect();
        let total = rect_f(vector::union(all.iter().map(|&i| v.bounds(i)))?);
        Some((total, each))
    }

    /// Line the picked items up along `side` of the box round them all
    /// (one item alone: of the canvas).
    pub(crate) fn align_vector_items(&mut self, side: Align) {
        let Some((mut total, each)) = self.picked_bounds() else {
            return;
        };
        if each.len() == 1 {
            total = [
                0.0,
                0.0,
                self.canvas.width() as f32,
                self.canvas.height() as f32,
            ];
        }
        let shift: Vec<(usize, Vec2)> = each
            .iter()
            .map(|&(i, b)| {
                let d = match side {
                    Align::Left => Vec2::new(total[0] - b[0], 0.0),
                    Align::Right => Vec2::new(total[2] - b[2], 0.0),
                    Align::Centre => Vec2::new((total[0] + total[2] - b[0] - b[2]) * 0.5, 0.0),
                    Align::Top => Vec2::new(0.0, total[1] - b[1]),
                    Align::Bottom => Vec2::new(0.0, total[3] - b[3]),
                    Align::Middle => Vec2::new(0.0, (total[1] + total[3] - b[1] - b[3]) * 0.5),
                };
                (i, d)
            })
            .collect();
        self.move_each(&shift, "Align");
    }

    /// Spread the picked items out evenly between the outermost ones,
    /// across (`horizontal`) or down: equal gaps between them.
    pub(crate) fn distribute_vector_items(&mut self, horizontal: bool) {
        let Some((_, mut each)) = self.picked_bounds() else {
            return;
        };
        if each.len() < 3 {
            return;
        }
        let (lo, hi) = if horizontal { (0, 2) } else { (1, 3) };
        each.sort_by(|a, b| (a.1[lo] + a.1[hi]).total_cmp(&(b.1[lo] + b.1[hi])));
        let start = each.iter().map(|(_, b)| b[lo]).fold(f32::MAX, f32::min);
        let end = each.iter().map(|(_, b)| b[hi]).fold(f32::MIN, f32::max);
        let sizes: f32 = each.iter().map(|(_, b)| b[hi] - b[lo]).sum();
        let gap = (end - start - sizes) / (each.len() - 1) as f32;
        let mut at = start;
        let shift: Vec<(usize, Vec2)> = each
            .iter()
            .map(|&(i, b)| {
                let d = at - b[lo];
                at += b[hi] - b[lo] + gap;
                (
                    i,
                    if horizontal {
                        Vec2::new(d, 0.0)
                    } else {
                        Vec2::new(0.0, d)
                    },
                )
            })
            .collect();
        self.move_each(&shift, "Distribute");
    }

    fn move_each(&mut self, shift: &[(usize, Vec2)], label: &str) {
        self.change_picked_each(label, |i, item| {
            let d = shift.iter().find(|(k, _)| *k == i)?.1;
            (d != Vec2::ZERO).then(|| {
                let mut item = item.clone();
                item.map_points(&|p| p + d, 1.0);
                item
            })
        });
    }

    /// Flip the picked items about the middle of the box round them all,
    /// side to side (`horizontal`) or upside down.
    pub(crate) fn flip_vector_items(&mut self, horizontal: bool) {
        let Some((total, _)) = self.picked_bounds() else {
            return;
        };
        let (cx, cy) = ((total[0] + total[2]) * 0.5, (total[1] + total[3]) * 0.5);
        let flip = move |p: Vec2| {
            if horizontal {
                Vec2::new(2.0 * cx - p.x, p.y)
            } else {
                Vec2::new(p.x, 2.0 * cy - p.y)
            }
        };
        self.change_picked_each(
            if horizontal {
                "Flip across"
            } else {
                "Flip down"
            },
            |_, item| {
                let mut item = item.clone();
                item.map_points(&flip, 1.0);
                Some(item)
            },
        );
    }

    /// The picked shapes made one by `how`, in the place (and with the look)
    /// of the bottom one. Lines and groups picked are left as they are.
    /// Returns whether there were two shapes to combine.
    pub(crate) fn combine_vector_shapes(&mut self, how: Combine) -> bool {
        use i_overlay::core::fill_rule::FillRule as Rule;
        use i_overlay::core::overlay_rule::OverlayRule;
        use i_overlay::float::single::SingleFloatOverlay;
        let Some((idx, mut all)) = self.picked_items() else {
            return false;
        };
        let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
            return false;
        };
        all.sort_unstable();
        all.dedup();
        let shapes: Vec<(usize, PathShape)> = all
            .iter()
            .filter_map(|&i| match &*v.items()[i] {
                VectorItem::Shape(s) => Some((i, s.clone())),
                _ => None,
            })
            .collect();
        if shapes.len() < 2 {
            return false;
        }
        let contours = |s: &PathShape| -> Vec<Vec<[f64; 2]>> {
            s.flattened()
                .into_iter()
                .filter(|(p, _)| p.len() >= 3)
                .map(|(p, _)| p.iter().map(|q| [q.x as f64, q.y as f64]).collect())
                .collect()
        };
        let (bottom, base) = &shapes[0];
        let mut result: Vec<Vec<[f64; 2]>> = contours(base);
        for (_, s) in &shapes[1..] {
            let rule = match how {
                Combine::Union => OverlayRule::Union,
                Combine::Intersect => OverlayRule::Intersect,
                Combine::Subtract => OverlayRule::Difference,
                Combine::Exclude => OverlayRule::Xor,
            };
            result = result
                .overlay(&contours(s), rule, Rule::NonZero)
                .into_iter()
                .flatten()
                .collect();
        }
        let path = Path {
            subpaths: result
                .into_iter()
                .map(|c| Subpath {
                    knots: c
                        .into_iter()
                        .map(|[x, y]| Knot::corner(Vec2::new(x as f32, y as f32)))
                        .collect(),
                    closed: true,
                })
                .collect(),
        };
        let combined = VectorItem::Shape(PathShape {
            path,
            ..base.clone()
        });
        let bottom = *bottom;
        let others: Vec<usize> = shapes[1..].iter().map(|(i, _)| *i).collect();
        let empty = matches!(&combined, VectorItem::Shape(s) if s.path.is_empty());
        let label = how.label();
        let id = self.canvas.layers[idx].id;
        self.release_canvas_for_lines();
        self.mark_action();
        self.forget_vector_change();
        let layer = &self.canvas.layers[idx];
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: Vec2::ZERO,
        });
        let mut region = None;
        self.edit_vector(idx, |v| {
            region = vector::union(shapes.iter().map(|(i, _)| v.bounds(*i)));
            let mut edits: Vec<(usize, Vec<VectorItem>)> =
                vec![(bottom, if empty { Vec::new() } else { vec![combined] })];
            edits.extend(others.iter().map(|&i| (i, Vec::new())));
            v.splice(edits);
        });
        if let Some(region) = region {
            self.redraw_vector(idx, region);
        }
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, label);
        }
        self.workspace.vector.line_edit = (!empty).then(|| LineEdit::picking(id, bottom));
        true
    }

    /// `pos` onto the nearest end of a line, or point of a shape, of vector
    /// layer `idx` (other than item `not`) within `reach`, when snapping is
    /// on; else as it is.
    pub(crate) fn snap_to_vector_points(
        &self,
        idx: usize,
        pos: Vec2,
        not: usize,
        reach: f32,
    ) -> Vec2 {
        if !self.workspace.vector.snap_points {
            return pos;
        }
        let Some(v) = self.canvas.layers.get(idx).and_then(|l| l.vector.as_ref()) else {
            return pos;
        };
        let r = reach.ceil() as i32 + 1;
        let (x, y) = (pos.x.floor() as i32, pos.y.floor() as i32);
        let mut best: Option<(f32, Vec2)> = None;
        for i in v.candidates([x - r, y - r, x + r + 1, y + r + 1]) {
            if i == not {
                continue;
            }
            let mut consider = |p: Vec2| {
                let d = (p - pos).length();
                if d <= reach && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, p));
                }
            };
            match &*v.items()[i] {
                VectorItem::Line(l) => {
                    for p in [l.points.first(), l.points.last()].into_iter().flatten() {
                        consider(Vec2::new(p[0], p[1]));
                    }
                }
                VectorItem::Shape(s) => {
                    for sub in &s.path.subpaths {
                        for k in &sub.knots {
                            consider(k.anchor);
                        }
                    }
                }
                VectorItem::Group(_) => {}
            }
        }
        best.map_or(pos, |(_, p)| p)
    }

    /// The picked line (or shape's outline) becomes the curve ruler: strokes
    /// started near it run along it.
    pub(crate) fn picked_to_curve_ruler(&mut self) -> bool {
        let Some((idx, all)) = self.picked_items() else {
            return false;
        };
        let Some(item) = self.canvas.layers[idx]
            .vector
            .as_ref()
            .and_then(|v| v.items().get(all[0]).cloned())
        else {
            return false;
        };
        let points: Vec<Vec2> = match &*item {
            VectorItem::Line(l) => l.smoothed().iter().map(|p| Vec2::new(p[0], p[1])).collect(),
            VectorItem::Shape(s) => {
                s.flattened()
                    .into_iter()
                    .next()
                    .map_or(Vec::new(), |(mut p, closed)| {
                        if closed && let Some(&first) = p.first() {
                            p.push(first);
                        }
                        p
                    })
            }
            VectorItem::Group(_) => Vec::new(),
        };
        if points.len() < 2 {
            return false;
        }
        let guides = &mut self.workspace.guides;
        guides.curve_ruler = Some(std::sync::Arc::new(points));
        guides.curve_enabled = true;
        true
    }
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
        app.active_tool = crate::app::tools::Tool::VectorEdit;
        app
    }

    fn square(app: &mut PainterApp, x: f32, y: f32, s: f32) {
        let idx = app.canvas.active_layer_idx;
        let path = Path {
            subpaths: vec![Subpath {
                knots: [(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
                    .map(|(x, y)| Knot::corner(Vec2::new(x, y)))
                    .to_vec(),
                closed: true,
            }],
        };
        app.add_vector_shape(idx, path, true, false);
    }

    fn items(app: &PainterApp) -> Vec<[i32; 4]> {
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        (0..v.len()).map(|i| v.bounds(i)).collect()
    }

    fn pick_all_by_box(app: &mut PainterApp) {
        app.line_edit_press(Vec2::new(1.0, 118.0), false, false);
        app.line_edit_drag(Vec2::new(199.0, 1.0));
        app.line_edit_release();
    }

    #[test]
    fn a_box_picks_what_it_touches_or_only_what_is_inside() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, 20.0);
        square(&mut app, 60.0, 10.0, 20.0);
        square(&mut app, 120.0, 10.0, 20.0);
        // Over the first two, half over the second.
        app.line_edit_press(Vec2::new(2.0, 48.0), false, false);
        app.line_edit_drag(Vec2::new(70.0, 2.0));
        app.line_edit_release();
        assert_eq!(app.picked_items().unwrap().1, vec![0, 1]);
        app.workspace.vector.box_pick = BoxPick::Inside;
        app.workspace.vector.line_edit = None;
        app.line_edit_press(Vec2::new(2.0, 48.0), false, false);
        app.line_edit_drag(Vec2::new(70.0, 2.0));
        app.line_edit_release();
        assert_eq!(app.picked_items().unwrap().1, vec![0]);
    }

    #[test]
    fn align_distribute_and_flip_move_the_picked_items() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, 20.0);
        square(&mut app, 50.0, 30.0, 20.0);
        square(&mut app, 150.0, 60.0, 20.0);
        pick_all_by_box(&mut app);
        app.align_vector_items(Align::Top);
        let tops: Vec<i32> = items(&app).iter().map(|b| b[1]).collect();
        assert!(tops.iter().all(|&t| t == tops[0]), "{tops:?}");
        app.distribute_vector_items(true);
        let b = items(&app);
        let (g1, g2) = (b[1][0] - b[0][2], b[2][0] - b[1][2]);
        assert!((g1 - g2).abs() <= 1, "{g1} {g2}");
        let before = items(&app);
        app.flip_vector_items(true);
        let after = items(&app);
        assert!(
            (after[0][0] - before[2][0]).abs() <= 1,
            "the first is where the last was"
        );
        app.apply_history(false);
        assert_eq!(items(&app), before, "undo");
    }

    #[test]
    fn shapes_combine_into_one() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, 40.0);
        square(&mut app, 30.0, 30.0, 40.0);
        pick_all_by_box(&mut app);
        assert!(app.combine_vector_shapes(Combine::Union));
        assert_eq!(items(&app).len(), 1);
        let b = items(&app)[0];
        assert!(b[0] <= 10 && b[2] >= 70, "{b:?}");
        app.apply_history(false);
        assert_eq!(items(&app).len(), 2);
        pick_all_by_box(&mut app);
        assert!(app.combine_vector_shapes(Combine::Intersect));
        let b = items(&app)[0];
        assert!(
            (28..=31).contains(&b[0]) && (49..=52).contains(&b[2]),
            "{b:?}"
        );
        app.apply_history(false);
        pick_all_by_box(&mut app);
        assert!(app.combine_vector_shapes(Combine::Subtract));
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        let VectorItem::Shape(s) = &*v.items()[0] else {
            panic!("a shape")
        };
        assert!(s.touches(Vec2::new(15.0, 15.0), 0.0));
        assert!(
            !s.touches(Vec2::new(40.0, 40.0), 0.0),
            "the overlap is cut out"
        );
    }

    #[test]
    fn a_dragged_point_snaps_to_another_items_point() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, 20.0);
        let idx = app.canvas.active_layer_idx;
        assert_eq!(
            app.snap_to_vector_points(idx, Vec2::new(32.0, 11.0), 99, 5.0),
            Vec2::new(30.0, 10.0)
        );
        assert_eq!(
            app.snap_to_vector_points(idx, Vec2::new(32.0, 11.0), 0, 5.0),
            Vec2::new(32.0, 11.0)
        );
        app.workspace.vector.snap_points = false;
        assert_eq!(
            app.snap_to_vector_points(idx, Vec2::new(32.0, 11.0), 99, 5.0),
            Vec2::new(32.0, 11.0)
        );
    }
}
