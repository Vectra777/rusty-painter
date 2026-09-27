//! Touch-screen gestures and pen pressure.
//!
//! On Android a stylus arrives as mouse events and fingers as touch events,
//! so the split is natural: the pen (or one finger, if finger painting is
//! on) paints, and two or more fingers drive the view:
//! - pinch to zoom, drag to pan, twist to rotate (about the fingers);
//! - a quick two-finger tap undoes, a three-finger tap redoes.
//!
//! egui also turns the first finger into pointer events, which the regular
//! input handler would paint with; [`TouchState::suppress_pointer`] tells it
//! when those events belong to a gesture instead.
//!
//! While the pen is in use, touches are the hand holding it (a resting palm)
//! and are ignored until they lift; whatever they started just before the
//! pen landed is taken back.

use super::PainterApp;
use super::tools::Tool;
use crate::selection::transform::TransformState;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A multi-finger touch shorter than this, that barely moved, is a tap.
const TAP_MAX_DURATION: Duration = Duration::from_millis(300);
const TAP_MAX_TRAVEL: f32 = 24.0;
/// A stroke this young when a second finger lands was the gesture's first
/// finger, not intended paint, and is undone.
const STROKE_CANCEL_WINDOW: Duration = Duration::from_millis(500);
/// Rotation within this many degrees of upright snaps back to 0 on release.
const ROTATION_SNAP_DEG: f32 = 4.0;

#[derive(Default)]
pub struct TouchState {
    /// Active fingers by touch id, with their last position.
    touches: HashMap<u64, egui::Pos2>,
    gesture: Option<Gesture>,
    /// Pointer events this frame come from fingers that must not paint.
    pub(crate) suppress_pointer: bool,
    /// When the current stroke started, to cancel it if it turns out to be
    /// the first finger of a gesture.
    pub(crate) stroke_started: Option<Instant>,
    /// Eraser-tip state of the pen at its last press, so flipping the pen
    /// switches brush/eraser once without overriding manual choices.
    pen_was_eraser: Option<bool>,
    /// Touches from the hand holding the pen, ignored until they lift.
    ignored: HashSet<u64>,
    /// The pen is in use this frame: its input also arrives as pointer
    /// events (Android) or touches (Windows), which the canvas ignores in
    /// favor of the pen's own samples.
    pub(crate) pen_active: bool,
    /// The current pen contact started on the canvas.
    pub(crate) pen_on_canvas: bool,
    /// Where the active layer's history stood when the current stroke began
    /// (layer index, push count), to take the stroke back if it's cancelled.
    pub(crate) action_mark: Option<(usize, u64)>,
}

struct Gesture {
    start: Instant,
    max_touches: usize,
    /// Accumulated movement (points) used to tell taps from gestures.
    travel: f32,
}

impl TouchState {
    pub(crate) fn finger_down(&self) -> bool {
        !self.touches.is_empty()
    }
}

impl PainterApp {
    /// Apply the pen's pressure curve (a device setting) to raw pressure
    /// (0..=1). What pressure then drives (size, opacity, flow) is per brush.
    pub(crate) fn map_pressure(&self, raw: f32) -> f32 {
        raw.clamp(0.0, 1.0).powf(self.workspace.pressure_curve)
    }

    /// Switch brush/eraser when the pen's eraser end starts or stops being used.
    pub(crate) fn sync_pen_eraser(&mut self, is_eraser: bool) {
        let touch = &mut self.viewport.touch;
        if touch.pen_was_eraser == Some(is_eraser) {
            return;
        }
        let first = touch.pen_was_eraser.is_none();
        touch.pen_was_eraser = Some(is_eraser);
        // The first pen contact only switches if it's the eraser end.
        if !first || is_eraser {
            self.set_brush_tool(is_eraser);
        }
    }

    /// Remember where the active layer's history stands, as a stroke begins.
    pub(crate) fn mark_action(&mut self) {
        // File the previous stroke first, so it can't land after the mark
        // and be mistaken for this one.
        self.settle_strokes();
        let layer = self.canvas.active_layer_idx;
        self.viewport.touch.action_mark = self
            .layer_state
            .histories
            .get(layer)
            .map(|h| (layer, h.push_count()));
    }

    /// End the stroke in progress (brush or smudge/blur) and take it back,
    /// without leaving a redo entry. Only what it recorded is undone: a
    /// stroke that painted nothing leaves the history alone.
    pub(crate) fn discard_current_action(&mut self) {
        // The mark belongs to the last stroke begun; once that has ended
        // there is nothing in progress to take back.
        let in_progress = self.brush_state.is_drawing || self.brush_state.blend_stroke.is_some();
        let mark = self.viewport.touch.action_mark.take();
        self.release_canvas();
        self.blend_release();
        let Some((layer, count)) = mark.filter(|_| in_progress) else {
            return;
        };
        let recorded = self
            .layer_state
            .histories
            .get(layer)
            .is_some_and(|h| h.push_count() > count);
        if recorded && layer == self.canvas.active_layer_idx {
            self.apply_history(false);
            if let Some(history) = self.layer_state.histories.get_mut(layer) {
                history.discard_redo();
            }
        }
    }

    /// End the in-progress stroke; take it back if it only just started.
    fn cancel_recent_stroke(&mut self) {
        let young = self
            .viewport
            .touch
            .stroke_started
            .is_some_and(|t| t.elapsed() < STROKE_CANCEL_WINDOW);
        if young {
            self.discard_current_action();
        } else {
            self.release_canvas();
            self.blend_release();
        }
    }
}

impl PainterApp {
    /// Whatever the first finger started (stroke, selection drag, transform
    /// drag) wasn't meant: a second finger landed (`discard` false: only a
    /// young stroke is taken back), or the touch was cancelled or turned out
    /// to be the hand holding the pen (`discard` true).
    fn cancel_finger_action(&mut self, discard: bool) {
        if discard {
            self.discard_current_action();
        } else {
            self.cancel_recent_stroke();
        }
        match self.active_tool {
            Tool::Select(_) if self.selection_manager.is_dragging => {
                self.selection_manager.clear_selection();
            }
            Tool::Transform(ref mut info) => {
                info.start_pos = None;
                info.state = TransformState::None;
            }
            _ => {}
        }
        self.viewport.is_primary_down = false;
        self.viewport.is_panning = false;
    }
}

/// Process this frame's touch events. `canvas` is the canvas panel's
/// response. Returns whether a repaint is needed.
pub(crate) fn handle_touch(
    app: &mut PainterApp,
    ctx: &egui::Context,
    canvas: &egui::Response,
) -> bool {
    let area = canvas.rect;
    let events = ctx.input(|i| i.events.clone());
    let mut repaint = false;

    // The pen landed while fingers were down: they're the hand holding it.
    let touch = &mut app.viewport.touch;
    if touch.pen_active && !touch.touches.is_empty() {
        let fingers: Vec<u64> = touch.touches.drain().map(|(id, _)| id).collect();
        touch.ignored.extend(fingers);
        touch.gesture = None;
        app.cancel_finger_action(true);
        repaint = true;
    }
    let mut gesture_this_frame = app.viewport.touch.gesture.is_some();
    let mut tap: Option<usize> = None;

    for event in &events {
        let egui::Event::Touch { id, phase, pos, .. } = *event else {
            continue;
        };
        let touch = &mut app.viewport.touch;
        if touch.ignored.contains(&id.0) {
            if matches!(phase, egui::TouchPhase::End | egui::TouchPhase::Cancel) {
                touch.ignored.remove(&id.0);
            }
            continue;
        }
        if touch.pen_active && phase == egui::TouchPhase::Start {
            touch.ignored.insert(id.0);
            continue;
        }
        match phase {
            egui::TouchPhase::Start => {
                // Fingers landing on panels, menus or the canvas faders are
                // the UI's; only the canvas itself starts a gesture.
                let on_canvas = area.contains(pos) && ctx.layer_id_at(pos) == Some(canvas.layer_id);
                if !on_canvas && touch.touches.is_empty() {
                    continue;
                }
                touch.touches.insert(id.0, pos);
                if touch.touches.len() >= 2 {
                    if touch.gesture.is_none() {
                        touch.gesture = Some(Gesture {
                            start: Instant::now(),
                            max_touches: touch.touches.len(),
                            travel: 0.0,
                        });
                        gesture_this_frame = true;
                        app.cancel_finger_action(false);
                    }
                    if let Some(g) = &mut app.viewport.touch.gesture {
                        g.max_touches = g.max_touches.max(app.viewport.touch.touches.len());
                    }
                }
            }
            egui::TouchPhase::Move => {
                let Some(last) = touch.touches.get_mut(&id.0) else {
                    continue;
                };
                let delta = pos - *last;
                *last = pos;
                let single = touch.touches.len() == 1 && touch.gesture.is_none();
                if single && !app.workspace.finger_painting {
                    // One finger pans when only the pen paints.
                    app.viewport.offset += delta;
                    app.workspace.auto_fit = false;
                    repaint = true;
                }
            }
            egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                if touch.touches.remove(&id.0).is_none() {
                    continue;
                }
                // A cancelled single finger (palm rejection, a system
                // gesture) takes back what it drew; egui ends its pointer
                // without a release, which would leave the stroke running.
                if phase == egui::TouchPhase::Cancel
                    && touch.touches.is_empty()
                    && touch.gesture.is_none()
                {
                    app.cancel_finger_action(true);
                    repaint = true;
                    continue;
                }
                if touch.touches.is_empty()
                    && let Some(g) = touch.gesture.take()
                {
                    let quick = g.start.elapsed() < TAP_MAX_DURATION;
                    if quick && g.travel < TAP_MAX_TRAVEL && phase == egui::TouchPhase::End {
                        tap = Some(g.max_touches);
                    }
                    let deg = app.viewport.rotation.to_degrees().rem_euclid(360.0);
                    if deg < ROTATION_SNAP_DEG || deg > 360.0 - ROTATION_SNAP_DEG {
                        app.viewport.rotation = 0.0;
                    }
                    repaint = true;
                }
            }
        }
    }

    // Pinch / pan / rotate while two or more fingers are down.
    if app.viewport.touch.gesture.is_some()
        && let Some(mt) = ctx.multi_touch()
        && mt.num_touches >= 2
    {
        let touches = &app.viewport.touch.touches;
        let anchor = touches
            .values()
            .fold(egui::Vec2::ZERO, |sum, p| sum + p.to_vec2())
            / touches.len().max(1) as f32;
        let anchor = anchor.to_pos2();
        if (mt.zoom_delta - 1.0).abs() > f32::EPSILON {
            app.zoom_about(anchor, area.min, app.viewport.zoom * mt.zoom_delta);
        }
        app.viewport.offset += mt.translation_delta;
        if mt.rotation_delta.abs() > f32::EPSILON {
            app.rotate_about(anchor, area.min, mt.rotation_delta);
        }
        app.workspace.auto_fit = false;
        if let Some(g) = &mut app.viewport.touch.gesture {
            g.travel += mt.translation_delta.length()
                + (mt.zoom_delta - 1.0).abs() * 200.0
                + mt.rotation_delta.abs() * 100.0;
        }
        repaint = true;
    }

    match tap {
        Some(2) => app.apply_history(false),
        Some(n) if n >= 3 => app.apply_history(true),
        _ => {}
    }

    let touch = &mut app.viewport.touch;
    // egui's pointer follows the first finger, which may be an ignored palm.
    touch.suppress_pointer = gesture_this_frame
        || touch.gesture.is_some()
        || !touch.ignored.is_empty()
        || (touch.finger_down() && !app.workspace.finger_painting);
    repaint || tap.is_some()
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::project::tests::test_app_pub;
    use eframe::egui::{Color32, Vec2};

    const TILE: usize = 64;

    fn app() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(128, 128, Color32::WHITE, TILE));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.diameter = 8.0;
        app
    }

    fn stroke(app: &mut crate::PainterApp, from: Vec2, to: Vec2) {
        app.start_stroke_with_pressure(from, 1.0);
        app.add_stroke_point(to, 1.0);
    }

    fn undo_len(app: &crate::PainterApp) -> usize {
        app.layer_state.histories[1].stacks().0.len()
    }

    #[test]
    fn a_discarded_stroke_leaves_no_paint_and_no_undo_step() {
        let mut app = app();
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 20.0));
        app.discard_current_action();
        assert_eq!(undo_len(&app), 0);
        assert_eq!(app.layer_state.histories[1].stacks().1.len(), 0, "no redo");
        let tile = app.canvas.get_layer_tile_data(1, 0, 0).unwrap_or_default();
        assert!(
            tile.iter().all(|&c| c == Color32::TRANSPARENT),
            "paint removed"
        );
    }

    #[test]
    fn discarding_with_no_stroke_running_keeps_the_last_one() {
        let mut app = app();
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 20.0));
        app.release_canvas();
        assert_eq!(undo_len(&app), 1);
        app.discard_current_action();
        assert_eq!(undo_len(&app), 1, "the finished stroke stays");
    }

    #[test]
    fn discarding_a_stroke_that_painted_nothing_keeps_the_previous_one() {
        let mut app = app();
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 20.0));
        // Off the canvas: no tile touched, nothing recorded.
        stroke(
            &mut app,
            Vec2::new(-500.0, -500.0),
            Vec2::new(-400.0, -500.0),
        );
        app.discard_current_action();
        assert_eq!(undo_len(&app), 1, "the earlier stroke stays");
    }

    #[test]
    fn a_second_press_keeps_the_first_strokes_undo_step() {
        let mut app = app();
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 20.0));
        // Pressed again without a release (a duplicate press from another
        // input path): the first stroke must stay undoable.
        stroke(&mut app, Vec2::new(80.0, 80.0), Vec2::new(100.0, 90.0));
        app.release_canvas();
        assert_eq!(undo_len(&app), 2);
        let blank = vec![Color32::TRANSPARENT; TILE * TILE];
        app.apply_history(false);
        app.apply_history(false);
        assert_eq!(app.canvas.get_layer_tile_data(1, 0, 0), Some(blank));
    }

    /// Painted pixels in column `x` of layer 1 (tiles are `TILE` wide).
    fn column_coverage(app: &crate::PainterApp, x: usize) -> usize {
        (0..2)
            .filter_map(|ty| app.canvas.get_layer_tile_data(1, (x / TILE) as i32, ty))
            .flat_map(|tile| (0..TILE).map(move |y| tile[y * TILE + x % TILE]))
            .filter(|c| c.a() > 0)
            .count()
    }

    #[test]
    fn pressure_blends_along_a_segment() {
        let mut app = app();
        let o = &mut app.brush_state.brush.brush_options;
        o.diameter = 24.0;
        o.pressure_size = true;
        o.pressure_min_size = 0.0;
        app.start_stroke_with_pressure(Vec2::new(10.0, 64.0), 0.05);
        app.add_stroke_point(Vec2::new(118.0, 64.0), 1.0);
        app.release_canvas();
        let (near_start, middle, near_end) = (
            column_coverage(&app, 20),
            column_coverage(&app, 64),
            column_coverage(&app, 110),
        );
        assert!(
            near_start < middle && middle < near_end,
            "thickness should grow along the stroke: {near_start} {middle} {near_end}"
        );
    }
}
