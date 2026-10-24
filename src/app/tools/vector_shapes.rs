//! Edit Vectors on shapes and groups (brush lines are edited as in
//! `vector`): a shape's points and handles dragged as the Pen's are, the
//! picked items moved, grouped, stacked, deleted, and their look changed
//! (fill, outline, dashes, markers) as one undo step per change.

use super::vector::{LineEdit, VectorSession};
use crate::app::PainterApp;
use crate::canvas::path::{Handle, Hit, KnotRef};
use crate::canvas::vector::{self, VectorGroup, VectorItem};
use eframe::egui::{Color32, Vec2};

/// A drag on picked items going on.
pub struct ShapeDrag {
    start: Vec2,
    /// The items as the drag found them (index, item).
    originals: Vec<(usize, VectorItem)>,
    grab: Grab,
}

#[derive(Clone, Copy)]
enum Grab {
    /// The picked shape's point, with its handles.
    Knot(KnotRef),
    /// One of its handles (`alone`: without turning the other).
    Handle(KnotRef, Handle, bool),
    /// The picked items, moved.
    Body,
}

impl PainterApp {
    /// The items Edit Vectors has picked on the active layer: the first
    /// one, then those added.
    pub(crate) fn picked_items(&self) -> Option<(usize, Vec<usize>)> {
        let edit = self.workspace.vector.line_edit.as_ref()?;
        let idx = self.canvas.layer_index_of(edit.layer)?;
        let v = self.canvas.layers[idx].vector.as_ref()?;
        let all: Vec<usize> = std::iter::once(edit.line)
            .chain(edit.extra.iter().copied())
            .filter(|&i| i < v.len())
            .collect();
        (!all.is_empty()).then_some((idx, all))
    }

    /// The picked item, if it's a shape: its layer and index, and a copy.
    fn picked_shape(&self) -> Option<(usize, usize, vector::PathShape)> {
        let edit = self.workspace.vector.line_edit.as_ref()?;
        let idx = self.canvas.layer_index_of(edit.layer)?;
        let item = self.canvas.layers[idx]
            .vector
            .as_ref()?
            .items()
            .get(edit.line)?;
        match &**item {
            VectorItem::Shape(s) => Some((idx, edit.line, s.clone())),
            _ => None,
        }
    }

    /// A press with Edit Vectors that's about shapes (a point, handle or
    /// body of the picked one), or Shift picking more. Returns whether it
    /// was (else it's left to the line editing).
    pub(crate) fn shape_edit_press(
        &mut self,
        idx: usize,
        pos: Vec2,
        shift: bool,
        alt: bool,
        hit: f32,
    ) -> bool {
        let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
            return false;
        };
        let under = v.pick(pos, hit);
        // Shift on an item picks it too (or not any more).
        let id = self.canvas.layers[idx].id;
        let on_handle = self.on_line_handle(pos, hit);
        if shift
            && !on_handle
            && let Some(i) = under
            && let Some(edit) = self.workspace.vector.line_edit.as_mut()
            && edit.layer == id
        {
            if i == edit.line {
                if let Some(next) = edit.extra.pop() {
                    edit.line = next;
                }
            } else if let Some(at) = edit.extra.iter().position(|&e| e == i) {
                edit.extra.remove(at);
            } else {
                edit.extra.push(i);
            }
            edit.knot = None;
            return true;
        }
        // The picked shape's points and handles.
        if let Some((sidx, si, shape)) = self.picked_shape().filter(|(l, ..)| *l == idx) {
            let knots: Vec<KnotRef> = (shape.path.subpaths.iter().enumerate())
                .flat_map(|(s, sub)| (0..sub.knots.len()).map(move |k| (s, k)))
                .collect();
            let grab = match shape.path.hit(pos, hit, &knots) {
                Some(Hit::Handle(r, h)) => Some(Grab::Handle(r, h, alt)),
                Some(Hit::Anchor(r)) if alt => {
                    // Alt on a point turns it smooth or back to a corner.
                    self.edit_vector_item(sidx, &[si], "Edit shape", |item| {
                        if let VectorItem::Shape(s) = item {
                            s.path.convert(r);
                        }
                    });
                    self.forget_vector_change();
                    return true;
                }
                Some(Hit::Anchor(r)) => Some(Grab::Knot(r)),
                Some(Hit::Segment(s, i, t)) if alt => {
                    // Alt on the outline adds a point there.
                    self.edit_vector_item(sidx, &[si], "Edit shape", |item| {
                        if let VectorItem::Shape(sh) = item {
                            sh.path.split(s, i, t);
                        }
                    });
                    self.forget_vector_change();
                    return true;
                }
                _ => None,
            };
            if let Some(grab) = grab {
                if let Grab::Knot(r) | Grab::Handle(r, ..) = grab
                    && let Some(edit) = self.workspace.vector.line_edit.as_mut()
                {
                    edit.knot = Some(r);
                }
                self.start_shape_drag(idx, pos, grab);
                return true;
            }
        }
        // On a picked item that isn't a line (or with others picked): its
        // body moves them all.
        let picked = self.picked_items().filter(|(l, _)| *l == idx);
        if let (Some(i), Some((_, all))) = (under, &picked)
            && all.contains(&i)
            && (all.len() > 1
                || (self.canvas.layers[idx].vector.as_ref()).is_some_and(|v| v.line(i).is_none()))
        {
            self.start_shape_drag(idx, pos, Grab::Body);
            return true;
        }
        // Picking a shape or group (a line is picked by the line editing).
        let v = self.canvas.layers[idx].vector.as_ref();
        if let Some(i) = under
            && v.is_some_and(|v| v.line(i).is_none())
        {
            self.workspace.vector.line_edit =
                Some(LineEdit::picking(self.canvas.layers[idx].id, i));
            self.start_shape_drag(idx, pos, Grab::Body);
            return true;
        }
        false
    }

    /// Whether `pos` is on a handle of the picked line (Shift there widens
    /// it rather than picking).
    fn on_line_handle(&self, pos: Vec2, hit: f32) -> bool {
        self.picked_line().is_some_and(|(_, line)| {
            self.line_handles(line).iter().any(|&k| {
                let [x, y, _] = line.points[k];
                (Vec2::new(x, y) - pos).length() <= hit
            })
        })
    }

    fn start_shape_drag(&mut self, idx: usize, pos: Vec2, grab: Grab) {
        self.release_canvas_for_lines();
        self.mark_action();
        let layer = &self.canvas.layers[idx];
        let Some(v) = layer.vector.as_ref() else {
            return;
        };
        let items = match (grab, self.picked_items()) {
            (Grab::Body, Some((_, all))) => all,
            _ => vec![
                self.workspace
                    .vector
                    .line_edit
                    .as_ref()
                    .map_or(0, |e| e.line),
            ],
        };
        let originals = items
            .iter()
            .filter_map(|&i| v.items().get(i).map(|it| (i, VectorItem::clone(it))))
            .collect();
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: pos,
        });
        if let Some(edit) = self.workspace.vector.line_edit.as_mut() {
            edit.shape_drag = Some(ShapeDrag {
                start: pos,
                originals,
                grab,
            });
        }
    }

    /// The drag on picked shapes went on to `pos`. Returns whether one is on.
    pub(crate) fn shape_edit_drag(&mut self, pos: Vec2) -> bool {
        let Some(edit) = self.workspace.vector.line_edit.as_ref() else {
            return false;
        };
        let Some(drag) = edit.shape_drag.as_ref() else {
            return false;
        };
        let Some(idx) = self.canvas.layer_index_of(edit.layer) else {
            return true;
        };
        let mut delta = pos - drag.start;
        let grab = drag.grab;
        // A shape's point snaps to other items' points.
        if let (Grab::Knot(r), Some((i, VectorItem::Shape(s)))) = (grab, drag.originals.first())
            && let Some(k) = s.path.knot(r)
        {
            let reach = 12.0 / self.viewport.zoom.max(0.01);
            delta = self.snap_to_vector_points(idx, k.anchor + delta, *i, reach) - k.anchor;
        }
        let moved: Vec<(usize, VectorItem)> = (drag.originals.iter())
            .map(|(i, original)| {
                let mut item = original.clone();
                match (grab, &mut item) {
                    (Grab::Body, item) => item.map_points(&|p| p + delta, 1.0),
                    (Grab::Knot(r), VectorItem::Shape(s)) => {
                        if let Some(k) = s.path.knot_mut(r) {
                            *k = k.moved(delta);
                        }
                    }
                    (Grab::Handle(r, h, alone), VectorItem::Shape(s)) => {
                        let at = s.path.knot(r).map(|k| match h {
                            Handle::In => k.in_,
                            Handle::Out => k.out,
                        });
                        if let Some(at) = at {
                            s.path.move_handle(r, h, at + delta, alone);
                        }
                    }
                    _ => {}
                }
                (*i, item)
            })
            .collect();
        let v = self.canvas.layers[idx].vector.as_ref();
        let old = v.map(|v| vector::union(moved.iter().map(|(i, _)| v.bounds(*i))));
        let new = vector::union(moved.iter().map(|(_, item)| item.bounds()));
        self.edit_vector(idx, |v| {
            for (i, item) in moved {
                v.edit_item(i, |it| *it = item);
            }
        });
        if let Some(region) = vector::union(old.flatten().into_iter().chain(new)) {
            self.redraw_vector(idx, region);
        }
        true
    }

    /// Delete with Edit Vectors on shapes: the picked shape's point, else
    /// the picked items (lines too, when more than one is picked). `None`
    /// leaves it to the line editing.
    pub(crate) fn shape_edit_delete(&mut self) -> Option<bool> {
        let (idx, all) = self.picked_items()?;
        let edit = self.workspace.vector.line_edit.as_ref()?;
        let (first, knot) = (edit.line, edit.knot);
        let v = self.canvas.layers[idx].vector.as_ref()?;
        let only_line = all.len() == 1 && v.line(first).is_some();
        if only_line {
            return None;
        }
        if let (Some(r), true) = (knot, all.len() == 1) {
            self.edit_vector_item(idx, &[first], "Edit shape", |item| {
                if let VectorItem::Shape(s) = item {
                    s.path.delete(r);
                }
            });
            self.forget_vector_change();
            if let Some(edit) = self.workspace.vector.line_edit.as_mut() {
                edit.knot = None;
            }
            // A shape left without a point goes.
            let empty = (self.canvas.layers[idx].vector.as_ref())
                .and_then(|v| v.items().get(first).cloned())
                .is_some_and(|i| matches!(&*i, VectorItem::Shape(s) if s.path.is_empty()));
            if !empty {
                return Some(true);
            }
        }
        self.delete_vector_items();
        Some(true)
    }

    /// Change items `items` of vector layer `idx` with `change`, as one
    /// undo step named `label`. The same change made again straight after
    /// on the same items (a slider dragged) adds to that step.
    pub(crate) fn edit_vector_item(
        &mut self,
        idx: usize,
        items: &[usize],
        label: &'static str,
        change: impl Fn(&mut VectorItem),
    ) {
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        let Some(v) = layer.vector.as_ref() else {
            return;
        };
        let id = layer.id;
        let old = vector::union(items.iter().filter(|&&i| i < v.len()).map(|&i| v.bounds(i)));
        let token = self.layer_state.history.top_token();
        let same = self
            .workspace
            .vector
            .last_change
            .as_ref()
            .is_some_and(|(l, its, name, t)| {
                *l == id && its == items && *name == label && *t == token
            });
        self.release_canvas_for_lines();
        let before = (!same).then(|| self.canvas.layers[idx].vector.clone().unwrap_or_default());
        self.edit_vector(idx, |v| {
            for &i in items {
                v.edit_item(i, &change);
            }
        });
        let v = self.canvas.layers[idx].vector.as_ref();
        let new = v.and_then(|v| {
            vector::union(items.iter().filter(|&&i| i < v.len()).map(|&i| v.bounds(i)))
        });
        let Some(region) = vector::union(old.into_iter().chain(new)) else {
            return;
        };
        match before {
            Some(before) => {
                self.workspace.vector.session = Some(VectorSession {
                    layer: id,
                    before,
                    changed: None,
                    drawing: None,
                    last: Vec2::ZERO,
                });
                self.redraw_vector(idx, region);
                if let Some(session) = self.workspace.vector.session.take() {
                    self.push_vector_step(session, label);
                }
            }
            None => {
                // Added to the step on top: what it draws again grows.
                self.redraw_vector(idx, region);
                if let Some(top) = self.layer_state.history.top_mut()
                    && let Some(crate::canvas::history::LayerHistoryOp::Vector {
                        redraw: Some(r),
                        ..
                    }) = &mut top.layer_action
                {
                    *r = vector::union([*r, region]).unwrap_or(*r);
                }
            }
        }
        self.workspace.vector.last_change = Some((
            id,
            items.to_vec(),
            label,
            self.layer_state.history.top_token(),
        ));
    }

    /// The next change is a step of its own (a slider let go).
    pub(crate) fn forget_vector_change(&mut self) {
        self.workspace.vector.last_change = None;
    }

    /// Change the picked items with `change` (see [`Self::edit_vector_item`]).
    pub(crate) fn edit_picked_items(
        &mut self,
        label: &'static str,
        change: impl Fn(&mut VectorItem),
    ) {
        if let Some((idx, all)) = self.picked_items() {
            self.edit_vector_item(idx, &all, label, change);
        }
    }

    /// Take the picked items out (one step).
    pub(crate) fn delete_vector_items(&mut self) {
        let Some((idx, mut all)) = self.picked_items() else {
            return;
        };
        all.sort_unstable();
        all.dedup();
        self.restructure_vectors(idx, "Delete", |v| {
            let region = vector::union(all.iter().map(|&i| v.bounds(i)));
            v.splice(all.iter().map(|&i| (i, Vec::new())).collect());
            (region, None)
        });
    }

    /// The picked items as one group, where the topmost of them was.
    pub(crate) fn group_vector_items(&mut self) {
        let Some((idx, mut all)) = self.picked_items() else {
            return;
        };
        all.sort_unstable();
        all.dedup();
        if all.len() < 2 {
            return;
        }
        self.restructure_vectors(idx, "Group shapes", |v| {
            let items: Vec<VectorItem> = all
                .iter()
                .map(|&i| VectorItem::clone(&v.items()[i]))
                .collect();
            let region = vector::union(all.iter().map(|&i| v.bounds(i)));
            let top = *all.last().unwrap_or(&0);
            let group = VectorItem::Group(VectorGroup {
                name: String::new(),
                opacity: 1.0,
                visible: true,
                items,
            });
            let edits = (all.iter())
                .map(|&i| {
                    (
                        i,
                        if i == top {
                            vec![group.clone()]
                        } else {
                            Vec::new()
                        },
                    )
                })
                .collect();
            v.splice(edits);
            // Where it is now: below it, the others taken out.
            (region, Some(top + 1 - all.len()))
        });
    }

    /// The picked group's items back on their own, where it was.
    pub(crate) fn ungroup_vector_items(&mut self) {
        let Some((idx, all)) = self.picked_items() else {
            return;
        };
        let Some(&i) = all.first() else {
            return;
        };
        let Some(VectorItem::Group(g)) = (self.canvas.layers[idx].vector.as_ref())
            .and_then(|v| v.items().get(i).map(|it| VectorItem::clone(it)))
        else {
            return;
        };
        self.restructure_vectors(idx, "Ungroup shapes", |v| {
            let region = Some(v.bounds(i));
            // (A faded group's items keep its fading.)
            let items = (g.items.into_iter())
                .map(|mut it| {
                    if g.opacity < 1.0 {
                        fade(&mut it, g.opacity);
                    }
                    it
                })
                .collect();
            v.splice(vec![(i, items)]);
            (region, Some(i))
        });
    }

    /// The picked item moved `by` places up the stack (down if negative;
    /// far enough: to the top or bottom).
    pub(crate) fn restack_vector_item(&mut self, by: isize) {
        let Some((idx, all)) = self.picked_items() else {
            return;
        };
        let Some(&i) = all.first() else {
            return;
        };
        let Some(len) = self.canvas.layers[idx].vector.as_ref().map(|v| v.len()) else {
            return;
        };
        let to = (i as isize + by).clamp(0, len as isize - 1) as usize;
        if to == i {
            return;
        }
        self.restructure_vectors(idx, "Restack shape", |v| {
            let (lo, hi) = (i.min(to), i.max(to));
            let region = vector::union((lo..=hi).map(|k| v.bounds(k)));
            let mut items: Vec<VectorItem> = (lo..=hi)
                .map(|k| VectorItem::clone(&v.items()[k]))
                .collect();
            if to > i {
                items.rotate_left(1);
            } else {
                items.rotate_right(1);
            }
            let mut edits: Vec<(usize, Vec<VectorItem>)> =
                (lo..=hi).map(|k| (k, Vec::new())).collect();
            edits[0].1 = items;
            v.splice(edits);
            (region, Some(to))
        });
    }

    /// A change to which items vector layer `idx` has, as one step:
    /// `change` returns where pixels change and the item to pick after.
    fn restructure_vectors(
        &mut self,
        idx: usize,
        label: &str,
        change: impl FnOnce(&mut vector::VectorLayer) -> (Option<[i32; 4]>, Option<usize>),
    ) {
        self.release_canvas_for_lines();
        self.mark_action();
        self.forget_vector_change();
        let layer = &self.canvas.layers[idx];
        let id = layer.id;
        self.workspace.vector.session = Some(VectorSession {
            layer: id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: Vec2::ZERO,
        });
        let mut result = (None, None);
        self.edit_vector(idx, |v| result = change(v));
        let (region, pick) = result;
        if let Some(region) = region {
            self.redraw_vector(idx, region);
        }
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, label);
        }
        self.workspace.vector.line_edit = pick.map(|line| LineEdit::picking(id, line));
    }
}

impl PainterApp {
    /// SVG's shapes onto a vector layer, as one step: the selected one if
    /// `into_selected` and it is one, else a new one named `name`. They're
    /// picked, with Edit Vectors, to move or change.
    pub(crate) fn import_svg_text(
        &mut self,
        name: &str,
        text: &str,
        into_selected: bool,
    ) -> Result<(), String> {
        let (layer, _, report) = crate::project::svg_import::import_svg(text)?;
        if layer.is_empty() {
            return Err(report
                .message()
                .unwrap_or_else(|| "The SVG draws nothing".into()));
        }
        let active = self.canvas.active_layer_idx;
        if !(into_selected && self.is_vector_layer(active)) {
            self.add_vector_layer();
            let idx = self.canvas.active_layer_idx;
            self.canvas_mut().layers[idx].name = name.to_string();
        }
        let idx = self.canvas.active_layer_idx;
        let first = self.canvas.layers[idx]
            .vector
            .as_ref()
            .map_or(0, |v| v.len());
        let count = layer.len();
        let items: Vec<VectorItem> = layer.items().iter().map(|i| VectorItem::clone(i)).collect();
        self.restructure_vectors(idx, "Paste shapes", |v| {
            for item in items {
                v.push_item(item);
            }
            let region = vector::union((first..v.len()).map(|i| v.bounds(i)));
            (region, Some(first))
        });
        if let Some(edit) = self.workspace.vector.line_edit.as_mut() {
            edit.extra = (first + 1..first + count).collect();
        }
        self.active_tool = crate::app::tools::Tool::VectorEdit;
        if let Some(message) = report.message() {
            self.report(message);
        }
        Ok(())
    }
}

impl PainterApp {
    /// The picked items as an SVG document (to paste into a vector
    /// editor, or back here).
    pub(crate) fn picked_items_svg(&self) -> Option<String> {
        let (idx, mut all) = self.picked_items()?;
        all.sort_unstable();
        let v = self.canvas.layers[idx].vector.as_ref()?;
        let (mut body, mut defs, mut ids) = (String::new(), String::new(), 0);
        crate::project::svg::items_svg(
            all.iter().map(|&i| &*v.items()[i]),
            &mut body,
            &mut defs,
            &mut ids,
        );
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let defs = if defs.is_empty() {
            String::new()
        } else {
            format!("<defs>\n{defs}</defs>\n")
        };
        Some(format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n{defs}{body}</svg>\n"
        ))
    }
}

/// `item` faded to `opacity` of how it shows.
fn fade(item: &mut VectorItem, opacity: f32) {
    match item {
        VectorItem::Line(l) => l.opacity *= opacity,
        VectorItem::Shape(s) => s.opacity *= opacity,
        VectorItem::Group(g) => g.opacity *= opacity,
    }
}

/// The picked shape's path, points and handles, and the other picked
/// items' boxes, over the canvas.
pub(crate) fn draw_shape_edit(
    app: &PainterApp,
    painter: &eframe::egui::Painter,
    map: &crate::app::view::render::ScreenMap,
) {
    use eframe::egui::{Rect, Stroke};
    let Some((idx, all)) = app.picked_items() else {
        return;
    };
    let Some(v) = app.canvas.layers[idx].vector.as_ref() else {
        return;
    };
    let accent = crate::ui::style::accent();
    // Boxes round the picked items that aren't the line being edited.
    for &i in &all {
        if all.len() == 1 && v.line(i).is_some() {
            continue;
        }
        let b = v.bounds(i);
        let r = Rect::from_two_pos(
            map.to_screen(Vec2::new(b[0] as f32, b[1] as f32)),
            map.to_screen(Vec2::new(b[2] as f32, b[3] as f32)),
        );
        painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, accent.gamma_multiply(0.6)));
    }
    let Some((_, _, shape)) = app.picked_shape() else {
        return;
    };
    let knot = app.workspace.vector.line_edit.as_ref().and_then(|e| e.knot);
    for (s, sub) in shape.path.subpaths.iter().enumerate() {
        let line: Vec<_> = (sub.flatten_tol(0.25).into_iter())
            .map(|p| map.to_screen(p))
            .collect();
        let mut line = line;
        if sub.closed
            && let Some(&first) = line.first()
        {
            line.push(first);
        }
        painter.add(eframe::egui::Shape::line(
            line.clone(),
            Stroke::new(3.0_f32, Color32::from_black_alpha(120)),
        ));
        painter.add(eframe::egui::Shape::line(
            line,
            Stroke::new(1.0_f32, accent),
        ));
        for (k, kn) in sub.knots.iter().enumerate() {
            let a = map.to_screen(kn.anchor);
            for h in [kn.in_, kn.out] {
                if (h - kn.anchor).length() > 0.01 {
                    let hp = map.to_screen(h);
                    painter.line_segment([a, hp], Stroke::new(1.0_f32, accent));
                    painter.circle_filled(hp, 3.5, Color32::BLACK);
                    painter.circle_filled(hp, 2.5, Color32::WHITE);
                }
            }
            let r = Rect::from_center_size(a, eframe::egui::vec2(8.0, 8.0));
            painter.rect_filled(r.expand(1.0), 0.0, Color32::BLACK);
            let fill = if knot == Some((s, k)) {
                accent
            } else {
                Color32::WHITE
            };
            painter.rect_filled(r, 0.0, fill);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::canvas::path::{Knot, Path, Subpath};
    use crate::canvas::vector::{Fill, Paint, PathShape};

    fn app() -> PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 96, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.add_vector_layer();
        app.active_tool = crate::app::tools::Tool::VectorEdit;
        app.viewport.zoom = 1.0;
        app
    }

    fn square(app: &mut PainterApp, x: f32, y: f32, colour: [u8; 4]) {
        let idx = app.canvas.active_layer_idx;
        let path = Path {
            subpaths: vec![Subpath {
                knots: [(x, y), (x + 40.0, y), (x + 40.0, y + 40.0), (x, y + 40.0)]
                    .map(|(a, b)| Knot::corner(Vec2::new(a, b)))
                    .to_vec(),
                closed: true,
            }],
        };
        let shape = VectorItem::Shape(PathShape {
            path,
            fill: Some(Fill {
                paint: Paint::Colour(colour),
                rule: Default::default(),
            }),
            stroke: None,
            opacity: 1.0,
        });
        app.restructure_vectors(idx, "Square", |v| {
            v.push_item(shape);
            (Some(v.bounds(v.len() - 1)), Some(v.len() - 1))
        });
    }

    fn pixels(app: &mut PainterApp) -> Vec<Color32> {
        app.settle_vectors();
        let idx = app.canvas.active_layer_idx;
        (0..96)
            .flat_map(|y| (0..128).map(move |x| (x, y)))
            .map(|(x, y)| {
                app.canvas
                    .get_layer_tile_data(idx, x / 64, y / 64)
                    .map_or(Color32::TRANSPARENT, |t| {
                        t[((y % 64) * 64 + x % 64) as usize]
                    })
            })
            .collect()
    }

    fn at(p: &[Color32], x: usize, y: usize) -> Color32 {
        p[y * 128 + x]
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn dragging_a_shapes_point_moves_it_as_one_step() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, RED);
        let steps = app.layer_state.history.push_count();
        // Picked by `square`: its corner at (50, 10).
        app.line_edit_press(Vec2::new(50.0, 10.0), false, false);
        app.line_edit_drag(Vec2::new(60.0, 5.0));
        app.line_edit_drag(Vec2::new(70.0, 0.0));
        app.line_edit_release();
        assert_eq!(app.layer_state.history.push_count(), steps + 1);
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        let VectorItem::Shape(s) = &*v.items()[0] else {
            panic!()
        };
        assert_eq!(s.path.subpaths[0].knots[1].anchor, Vec2::new(70.0, 0.0));
        app.apply_history(false);
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        let VectorItem::Shape(s) = &*v.items()[0] else {
            panic!()
        };
        assert_eq!(s.path.subpaths[0].knots[1].anchor, Vec2::new(50.0, 10.0));
    }

    #[test]
    fn grouping_keeps_the_picture_and_restacking_changes_the_overlap() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, RED);
        square(&mut app, 30.0, 30.0, BLUE);
        let before = pixels(&mut app);
        assert_eq!(at(&before, 40, 40), Color32::BLUE, "blue on top");
        // Pick both, group them.
        app.line_edit_press(Vec2::new(15.0, 15.0), false, false);
        app.line_edit_release();
        app.line_edit_press(Vec2::new(65.0, 65.0), true, false);
        app.group_vector_items();
        let v = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap();
        assert_eq!(v.len(), 1);
        assert!(matches!(&*v.items()[0], VectorItem::Group(g) if g.items.len() == 2));
        assert_eq!(pixels(&mut app), before);
        app.ungroup_vector_items();
        assert_eq!(
            app.canvas.layers[app.canvas.active_layer_idx]
                .vector
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(pixels(&mut app), before);
        // Red (picked first after ungrouping) to the top.
        app.restack_vector_item(1);
        assert_eq!(at(&pixels(&mut app), 40, 40), Color32::RED);
        app.apply_history(false);
        assert_eq!(at(&pixels(&mut app), 40, 40), Color32::BLUE);
    }

    #[test]
    fn the_eraser_takes_a_shape_out_whole() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, RED);
        app.set_brush_tool(true);
        app.brush_state.brush.brush_options.diameter = 4.0;
        app.workspace.vector.erase = super::super::vector::VectorErase::Touched;
        app.start_stroke_with_pressure(Vec2::new(30.0, 30.0), 1.0);
        app.add_stroke_point(Vec2::new(31.0, 31.0), 1.0);
        app.finish_stroke();
        assert!(
            app.canvas.layers[app.canvas.active_layer_idx]
                .vector
                .as_ref()
                .unwrap()
                .is_empty()
        );
        assert_eq!(at(&pixels(&mut app), 20, 20), Color32::TRANSPARENT);
    }

    #[test]
    fn a_dragged_control_is_one_step_a_new_one_another() {
        let mut app = app();
        square(&mut app, 10.0, 10.0, RED);
        let steps = app.layer_state.history.push_count();
        for o in [0.9, 0.7, 0.5] {
            app.edit_picked_items("Shape opacity", move |item| {
                if let VectorItem::Shape(s) = item {
                    s.opacity = o;
                }
            });
        }
        assert_eq!(app.layer_state.history.push_count(), steps + 1);
        app.forget_vector_change();
        app.edit_picked_items("Shape opacity", |item| {
            if let VectorItem::Shape(s) = item {
                s.opacity = 0.2;
            }
        });
        assert_eq!(app.layer_state.history.push_count(), steps + 2);
        let faint = pixels(&mut app);
        app.apply_history(false);
        app.apply_history(false);
        assert_eq!(at(&pixels(&mut app), 20, 20), Color32::RED, "both undone");
        assert_ne!(at(&faint, 20, 20), Color32::RED);
    }

    #[test]
    fn pasted_svg_lands_picked_on_the_vector_layer() {
        let mut app = app();
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect x="5" y="5" width="20" height="20" fill="#00ff00"/><circle cx="60" cy="60" r="10" fill="blue"/></svg>"##;
        app.import_svg_text("Pasted", svg, true).unwrap();
        let (idx, picked) = app.picked_items().unwrap();
        assert_eq!(idx, app.canvas.active_layer_idx);
        assert_eq!(picked, [0, 1]);
        assert_eq!(at(&pixels(&mut app), 15, 15), Color32::from_rgb(0, 255, 0));
        // Copied back out as SVG, it reads in again the same.
        let copied = app.picked_items_svg().unwrap();
        let (layer, _, _) = crate::project::svg_import::import_svg(&copied).unwrap();
        assert_eq!(layer.len(), 2);
    }
}
