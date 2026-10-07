//! Keyed motion and animated layers in the app: keys set, moved, eased and
//! taken away (each gesture one undo step), new animated layers, empty
//! exposures, and painting on a moved layer where it shows. The model is
//! in [`crate::canvas::motion`].

use crate::PainterApp;
use crate::canvas::motion::{Motion, Prop};
use crate::canvas::storage::{Anim, LayerId, LayerKind};
use crate::selection::{SelectionManager, SelectionMask, SelectionShape};
use eframe::egui::Vec2;
use std::collections::HashMap;

/// Where a motion edit stands: a gesture already has its undo step.
#[derive(Default)]
pub struct MotionEditState {
    pub in_gesture: bool,
    /// The Animate tool's drag.
    pub drag: Option<crate::app::tools::animate::AnimateDrag>,
}

impl PainterApp {
    /// A point on the canvas as a point on the selected layer's own pixels
    /// (where it is before it's moved).
    pub(crate) fn to_layer_space(&self, pos: Vec2) -> Vec2 {
        let i = self.canvas.active_layer_idx;
        if !self.canvas.layers.get(i).is_some_and(|l| l.is_posed()) {
            return pos;
        }
        let (affine, _) = self.canvas.world_motion(i);
        match affine.inverse() {
            Some(inv) => {
                let [x, y] = inv.apply([pos.x, pos.y]);
                Vec2::new(x, y)
            }
            None => pos,
        }
    }

    /// A point on the selected layer's own pixels as a point on the canvas
    /// (where it shows, if it's moved): for drawing guides over it.
    pub(crate) fn to_canvas_space(&self, pos: Vec2) -> Vec2 {
        let i = self.canvas.active_layer_idx;
        if !self.canvas.layers.get(i).is_some_and(|l| l.is_posed()) {
            return pos;
        }
        let [x, y] = self.canvas.world_motion(i).0.apply([pos.x, pos.y]);
        Vec2::new(x, y)
    }

    /// The selection as the selected layer's own pixels see it: on a moved
    /// layer, carried back from where the layer shows onto its pixels, so
    /// what's selected on screen is what's painted.
    pub(crate) fn layer_selection_shape(&self) -> Option<SelectionShape> {
        let shape = self.selection_manager.current_shape.clone();
        let i = self.canvas.active_layer_idx;
        if !self.canvas.layers.get(i).is_some_and(|l| l.is_posed()) {
            return shape;
        }
        let mask = self.selection_manager.current_mask()?;
        let (world, _) = self.canvas.world_motion(i);
        let inverse = world.inverse()?;
        let shown = [
            mask.x0 as f32,
            mask.y0 as f32,
            (mask.x0 + mask.w as i32) as f32,
            (mask.y0 + mask.h as i32) as f32,
        ];
        let [x0, y0, x1, y1] = crate::canvas::storage::transformed_rect(&inverse, shown);
        let (x0, y0) = (x0.floor() as i32 - 1, y0.floor() as i32 - 1);
        let (w, h) = (
            (x1.ceil() as i32 + 1 - x0).max(0) as usize,
            (y1.ceil() as i32 + 1 - y0).max(0) as usize,
        );
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let [cx, cy] =
                    world.apply([(x0 + x as i32) as f32 + 0.5, (y0 + y as i32) as f32 + 0.5]);
                data[y * w + x] = mask.value(cx.floor() as i32, cy.floor() as i32);
            }
        }
        Some(SelectionShape::Mask(std::sync::Arc::new(
            SelectionMask::new(x0, y0, w, h, data),
        )))
    }

    /// [`Self::layer_selection_shape`] in a selection of its own.
    pub(crate) fn layer_selection(&self) -> SelectionManager {
        let mut selection = SelectionManager::with_shape(self.layer_selection_shape());
        selection.canvas_size = self.selection_manager.canvas_size;
        selection
    }

    /// The selected layer was painted at `dirty` (display tiles and the
    /// part of each): where it's moved, its shown copy follows.
    pub(crate) fn repose_painted(&mut self, dirty: &HashMap<(usize, usize), [usize; 4]>) {
        let i = self.canvas.active_layer_idx;
        if dirty.is_empty() || !self.canvas.layers.get(i).is_some_and(|l| l.is_posed()) {
            return;
        }
        let ts = crate::app::document::TILE_SIZE as f32;
        let mut rect = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for (&(tx, ty), r) in dirty {
            let (ox, oy) = (tx as f32 * ts, ty as f32 * ts);
            rect = [
                rect[0].min(ox + r[0] as f32),
                rect[1].min(oy + r[1] as f32),
                rect[2].max(ox + r[2] as f32),
                rect[3].max(oy + r[3] as f32),
            ];
        }
        if let Some(shown) = self.canvas.repose_region(i, rect) {
            self.mark_rect_damage([
                shown[0].floor() as i32 - 1,
                shown[1].floor() as i32 - 1,
                shown[2].ceil() as i32 + 1,
                shown[3].ceil() as i32 + 1,
            ]);
        }
    }

    /// The layer motion keys go on for layer `i`: a drawing's animated
    /// layer (its drawings move together), else the layer itself.
    pub(crate) fn motion_target(&self, i: usize) -> Option<usize> {
        let layer = self.canvas.layers.get(i)?;
        match (layer.anim, layer.kind) {
            (Some(Anim::Frame(_)), _) => layer.parent.and_then(|p| self.canvas.layer_index_of(p)),
            (_, LayerKind::Mask { owner }) => self.canvas.layer_index_of(owner),
            _ if i == 0 => None,
            _ => Some(i),
        }
    }

    /// Change layer `i`'s motion with `edit` (made if it has none, about
    /// the middle of what it shows): the first change of a gesture is an
    /// undo step, the rest join it. End the gesture with
    /// [`Self::motion_edit_done`].
    pub(crate) fn motion_edit(&mut self, i: usize, edit: impl FnOnce(&mut Motion)) {
        if i == 0 || i >= self.canvas.layers.len() {
            return;
        }
        if !self.workspace.motion.in_gesture {
            self.workspace.motion.in_gesture = true;
            self.document_step(|_| true);
        }
        self.release_canvas();
        let anchor = self.default_anchor(i);
        let canvas = crate::app::stroke_ops::exclusive(&mut self.canvas);
        let motion = canvas.layers[i]
            .motion
            .get_or_insert_with(|| Box::new(Motion::new(anchor)));
        edit(motion);
        canvas.pose_motions();
        self.workspace.animation.edits += 1;
        self.mark_all_tiles_dirty();
        self.mark_unsaved();
    }

    /// The gesture ended: the next change is a new undo step.
    pub(crate) fn motion_edit_done(&mut self) {
        self.workspace.motion.in_gesture = false;
    }

    /// One whole motion change as its own undo step.
    pub(crate) fn motion_step(&mut self, i: usize, edit: impl FnOnce(&mut Motion)) {
        self.motion_edit_done();
        self.motion_edit(i, edit);
        self.motion_edit_done();
    }

    /// The middle of what layer `i` shows (or of the canvas).
    fn default_anchor(&self, i: usize) -> [f32; 2] {
        match self.canvas.content_rect(i) {
            Some([x0, y0, x1, y1]) => [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
            None => [
                self.canvas.width() as f32 / 2.0,
                self.canvas.height() as f32 / 2.0,
            ],
        }
    }

    /// `p`'s value on layer `i` at the frame showing.
    pub(crate) fn motion_value(&self, i: usize, p: Prop) -> [f32; 2] {
        match (self.canvas.layers.get(i)).and_then(|l| l.motion.as_ref()) {
            Some(m) => m.value(p, self.canvas.time as f32),
            None if p == Prop::Anchor => self.default_anchor(i),
            None => p.rest(),
        }
    }

    /// Key every property of layer `i` at the frame showing, as it is now.
    pub(crate) fn key_all_here(&mut self, i: usize) {
        let t = self.canvas.time;
        // Where it is, and the pivot and effects if they have keys.
        let others = (self.canvas.layers.get(i))
            .and_then(|l| l.motion.as_ref())
            .map_or_else(Vec::new, |m| m.keyed(&Prop::ALL[4..]));
        let props: Vec<Prop> = Prop::TRANSFORM[..4].iter().copied().chain(others).collect();
        let values: Vec<(Prop, [f32; 2])> = props
            .iter()
            .map(|&p| (p, self.motion_value(i, p)))
            .collect();
        self.motion_step(i, |m| {
            for (p, v) in values {
                m.set(p, t, v);
            }
        });
    }

    pub(crate) fn remove_key(&mut self, i: usize, p: Prop, frame: u32) {
        self.motion_step(i, |m| {
            m.remove(p, frame);
        });
    }

    /// Take away every key of layer `i` at `frame`.
    pub(crate) fn remove_keys_at(&mut self, i: usize, frame: u32) {
        self.motion_step(i, |m| {
            for p in Prop::ALL {
                m.remove(p, frame);
            }
        });
    }

    /// Layer `i` follows `parent` (or nothing): it keeps showing where it
    /// was, its own keys going on from there.
    pub(crate) fn set_motion_parent(&mut self, i: usize, parent: Option<LayerId>) {
        // No loops: not a layer that follows this one.
        if let Some(p) = parent.and_then(|p| self.canvas.layer_index_of(p))
            && (p == i || self.follows(p, i))
        {
            self.report("A layer can't follow one that follows it".to_string());
            return;
        }
        self.motion_step(i, |m| m.parent = parent.map(|p| p.0));
    }

    /// Whether layer `a` follows `b`, however far up.
    pub(crate) fn follows(&self, a: usize, b: usize) -> bool {
        let mut at = a;
        for _ in 0..32 {
            let Some(next) = (self.canvas.layers.get(at))
                .and_then(|l| l.motion.as_ref())
                .and_then(|m| m.parent)
                .and_then(|id| self.canvas.layer_index_of(LayerId(id)))
            else {
                return false;
            };
            if next == b {
                return true;
            }
            at = next;
        }
        false
    }

    /// Layer `i` without keys (it shows where it was painted).
    pub(crate) fn clear_motion(&mut self, i: usize) {
        if self.canvas.layers.get(i).is_none_or(|l| l.motion.is_none()) {
            return;
        }
        self.motion_edit_done();
        self.document_step(|canvas| {
            canvas.layers[i].motion = None;
            true
        });
    }

    /// A new animated layer above the selected one (beside its animated
    /// layer, if it's a drawing), with a blank drawing at the frame
    /// showing, selected to draw on.
    pub(crate) fn new_animation_layer(&mut self) {
        let (index, parent) = self.sibling_insertion_point();
        let name = self.next_layer_name("Animation");
        let time = self.canvas.time;
        self.document_step(move |canvas| {
            let track = canvas.insert_new_layer(index, name, LayerKind::Group, parent);
            let Some(t) = canvas.layer_index_of(track) else {
                return false;
            };
            canvas.layers[t].anim = Some(Anim::Track);
            canvas.layers[t].expanded = false;
            match canvas.add_frame(track, time, false) {
                Some(d) => {
                    canvas.active_layer_idx = d;
                    true
                }
                None => false,
            }
        });
        self.workspace.animation.show_timeline = true;
    }

    /// Where a new layer goes beside the selected one: above it, or above
    /// its animated layer if it's a drawing.
    fn sibling_insertion_point(&self) -> (usize, Option<LayerId>) {
        let active = self
            .canvas
            .active_layer_idx
            .min(self.canvas.layers.len() - 1);
        let layer = &self.canvas.layers[active];
        if matches!(layer.anim, Some(Anim::Frame(_)))
            && let Some(t) = layer.parent.and_then(|p| self.canvas.layer_index_of(p))
        {
            return (t + 1, self.canvas.layers[t].parent);
        }
        if layer.anim == Some(Anim::Track) {
            return (active + 1, layer.parent);
        }
        self.insertion_point(false)
    }

    /// A drawing at frame `t` on layer `i`: on an animated layer, a new
    /// one (blank, or a copy of the one showing); a plain layer becomes
    /// animated first (its picture the drawing from the range's start).
    pub(crate) fn drawing_at(&mut self, i: usize, t: u32, copy: bool) {
        let Some(layer) = self.canvas.layers.get(i) else {
            return;
        };
        let track = match layer.anim {
            Some(Anim::Track) => layer.id,
            Some(Anim::Frame(_)) => match layer.parent {
                Some(p) => p,
                None => return,
            },
            None => {
                if layer.kind != LayerKind::Paint || i == 0 || layer.rig.is_some() {
                    return;
                }
                let start = self.canvas.timeline.start.min(t);
                let blank = t != start;
                self.document_step(move |canvas| {
                    let Some(track) = animate_at(canvas, i, start) else {
                        return false;
                    };
                    if blank && let Some(d) = canvas.add_frame(track, t, copy) {
                        canvas.active_layer_idx = d;
                    }
                    true
                });
                self.workspace.animation.show_timeline = true;
                return;
            }
        };
        self.document_step(move |canvas| match canvas.add_frame(track, t, copy) {
            Some(d) => {
                canvas.active_layer_idx = d;
                true
            }
            None => false,
        });
    }

    /// Animated layer `track` shows nothing from frame `t` (until its next
    /// drawing): an empty drawing starts there.
    pub(crate) fn empty_from(&mut self, track: LayerId, t: u32) {
        self.document_step(move |canvas| canvas.add_frame(track, t, false).is_some());
    }
}

/// Make paint layer `i` animated with its picture as the drawing from
/// frame `start`. Returns the animated layer's id.
pub(crate) fn animate_at(
    canvas: &mut crate::canvas::Canvas,
    i: usize,
    start: u32,
) -> Option<LayerId> {
    let track = canvas.animate_layer(i)?;
    let d = canvas
        .frame_at(track, canvas.time)
        .or_else(|| (canvas.frames_of(track).first()).map(|&(_, d)| d))?;
    canvas.layers[d].anim = Some(Anim::Frame(start));
    canvas.layers[d].name = format!("Frame {start}");
    // Its motion goes to the animated layer, moving every drawing.
    if let Some(t) = canvas.layer_index_of(track) {
        let motion = canvas.layers[d].motion.take();
        canvas.layers[t].motion = motion;
    }
    Some(track)
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::motion::Prop;
    use eframe::egui::Color32;

    fn app() -> crate::PainterApp {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app
    }

    #[test]
    fn a_plain_layer_becomes_animated_where_a_drawing_is_added() {
        let mut app = app();
        app.go_to_frame(3);
        app.drawing_at(1, 6, false);
        let track = app.active_track().expect("animated");
        let starts: Vec<u32> = app.canvas.frames_of(track).iter().map(|f| f.0).collect();
        assert_eq!(starts, [0, 6], "its picture from the start, a blank at 6");
        // An empty exposure from frame 9.
        app.empty_from(track, 9);
        app.go_to_frame(10);
        assert_eq!(app.canvas.flatten_final().pixels[0], Color32::WHITE);
        app.go_to_frame(2);
        assert_eq!(app.canvas.flatten_final().pixels[0], Color32::RED);
    }

    #[test]
    fn new_animation_layers_go_above_and_keys_undo() {
        let mut app = app();
        let before = app.canvas.layers.len();
        app.new_animation_layer();
        assert_eq!(
            app.canvas.layers.len(),
            before + 2,
            "a layer and its drawing"
        );
        assert!(app.active_track().is_some());
        // Keys on layer 1, one undo step.
        app.go_to_frame(5);
        app.motion_step(1, |m| m.set(Prop::Position, 5, [10.0, 0.0]));
        assert!(app.canvas.layers[1].is_posed());
        app.apply_history(false);
        assert!(app.canvas.layers[1].motion.is_none());
        assert!(!app.canvas.layers[1].is_posed());
    }

    #[test]
    fn the_exported_frames_show_the_motion() {
        let canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().timeline = crate::canvas::animation::Timeline {
            fps: 12,
            start: 0,
            end: 2,
        };
        app.motion_step(1, |m| {
            m.set(Prop::Position, 0, [0.0, 0.0]);
            m.set(Prop::Position, 2, [64.0, 0.0]);
        });
        let frames: Vec<_> =
            crate::app::animation::animation_frames(app.canvas.detached_copy()).collect();
        let at = |f: usize, x: usize| frames[f].rgba[(10 * 128 + x) * 4..][..4].to_vec();
        assert_eq!(at(0, 10), [255, 0, 0, 255]);
        assert_eq!(at(0, 100), [255, 255, 255, 255]);
        assert_eq!(at(2, 10), [255, 255, 255, 255], "moved away");
        assert_eq!(at(2, 100), [255, 0, 0, 255], "moved here");
    }

    #[test]
    fn a_selection_over_a_moved_layer_covers_its_own_pixels_there() {
        use crate::selection::{SelectionMode, SelectionShape};
        let mut app = app();
        app.selection_manager.canvas_size = [64, 64];
        app.motion_step(1, |m| m.set(Prop::Position, 0, [20.0, 0.0]));
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: eframe::egui::Vec2::new(20.0, 0.0),
                end: eframe::egui::Vec2::new(40.0, 20.0),
            },
            SelectionMode::Replace,
        );
        let selection = app.layer_selection();
        assert!(
            selection.contains_coords(5.0, 5.0),
            "shown at 25: its pixel 5"
        );
        assert!(!selection.contains_coords(30.0, 5.0));
    }

    #[test]
    fn a_moved_layer_changed_by_any_step_shows_its_new_pixels_on_the_next_frame() {
        let mut app = app();
        // Moved, the same at every frame (so its shown copy is kept).
        app.motion_step(1, |m| m.set(Prop::Position, 0, [0.0, 0.0]));
        app.motion_step(1, |m| m.set(Prop::Opacity, 0, [0.5, 0.0]));
        app.canvas_mut().active_layer_idx = 0;
        app.go_to_frame(1);
        let before = app.canvas.flatten_final().pixels[0];
        // Repainted blue by a step that doesn't pose anything itself.
        app.canvas
            .set_layer_tile_data(1, 0, 0, vec![Color32::BLUE; 64 * 64]);
        app.layer_state
            .history
            .push_action(crate::canvas::history::UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            });
        app.go_to_frame(2);
        let after = app.canvas.flatten_final().pixels[0];
        assert_ne!(before, after);
        assert!(after.b() > after.r(), "{after:?}");
    }

    #[test]
    fn painting_on_a_moved_layer_lands_where_it_shows() {
        let mut app = app();
        app.motion_step(1, |m| m.set(Prop::Position, 0, [20.0, 0.0]));
        let p = app.to_layer_space(eframe::egui::Vec2::new(30.0, 10.0));
        assert_eq!((p.x, p.y), (10.0, 10.0));
    }
}
