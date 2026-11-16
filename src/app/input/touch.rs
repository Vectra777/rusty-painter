//! Touch-screen gestures and pen pressure.
//!
//! On Android a stylus arrives as mouse events and fingers as touch events,
//! so the split is natural: the pen (or one finger, if finger painting is
//! on) paints, and two or more fingers drive the view:
//! - pinch to zoom, drag to pan, twist to rotate (about the fingers);
//! - a quick two-finger tap undoes, a three-finger tap redoes;
//! - two fingers held still undo again and again until they lift;
//! - three fingers held still open the QuickMenu;
//! - one finger held still on the canvas picks a colour (a temporary
//!   Eyedropper) until it lifts, if Settings says so.
//!
//! egui also turns the first finger into pointer events, which the regular
//! input handler would paint with; [`TouchState::suppress_pointer`] tells it
//! when those events belong to a gesture instead.
//!
//! While the pen is in use, touches are the hand holding it (a resting palm)
//! and are ignored until they lift; whatever they started just before the
//! pen landed is taken back.

use crate::app::PainterApp;
use crate::app::tools::Tool;
use crate::selection::transform::TransformState;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Picking from a held finger works with these tools while one finger
/// paints (it would cut into other tools' drags).
fn hold_pick_tool(tool: Tool) -> bool {
    matches!(tool, Tool::Brush | Tool::Smudge | Tool::Blur)
}

/// A multi-finger touch shorter than this (seconds), that barely moved, is
/// a tap.
const TAP_MAX_DURATION: f64 = 0.3;
const TAP_MAX_TRAVEL: f32 = 24.0;
/// Two fingers held still this long (seconds) undo, then again every
/// [`HOLD_UNDO_REPEAT`] while they stay.
const HOLD_UNDO_DELAY: f64 = 0.5;
const HOLD_UNDO_REPEAT: f64 = 0.25;
/// Three fingers held still this long open the QuickMenu.
const HOLD_MENU_DELAY: f64 = 0.5;
/// One finger held within this many points of where it landed, this long
/// (seconds), picks a colour.
const HOLD_PICK_SLOP: f32 = 8.0;
const HOLD_PICK_DELAY: f64 = 0.4;
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
    /// events (Android, X11) or touches (Windows), which the canvas ignores in
    /// favor of the pen's own samples.
    pub(crate) pen_active: bool,
    /// The pen moves the mouse pointer (Android, X11), so the pointer
    /// events are the pen's while it touches.
    pub(crate) pen_is_pointer: bool,
    /// How the pen leans at the latest sample (canvas terms); `None` for
    /// the mouse, fingers and tablets without tilt.
    pub(crate) pen_tilt: Option<crate::brush_engine::dynamics::PenTilt>,
    /// Its barrel rotation and wheel, in canvas terms.
    pub(crate) pen_barrel: crate::brush_engine::dynamics::PenBarrel,
    /// When the latest pen sample was taken, where the platform says
    /// (seconds, its own clock); `None` for the mouse and fingers.
    pub(crate) pen_time: Option<f64>,
    /// The latest pen sample's pressure is an estimate, the measured value
    /// to come with this index (iOS); `None` otherwise.
    pub(crate) pen_awaiting: Option<u64>,
    /// The mouse's (and fingers') moves have no time of their own: each
    /// frame's are spread over the time since the last frame (egui's clock,
    /// seconds), so a slow frame's moves don't all land at one instant.
    pub(crate) pointer_time: Option<f64>,
    /// When the last frame with pointer moves was.
    pub(crate) pointer_frame: Option<f64>,
    /// The current pen contact started on the canvas.
    pub(crate) pen_on_canvas: bool,
    /// Where the history stood when the current stroke began, to take the
    /// stroke back if it's cancelled.
    pub(crate) action_mark: Option<ActionMark>,
    /// A single finger on the canvas that hasn't moved off yet: it may
    /// become a colour pick.
    hold: Option<FingerHold>,
    /// Picking with a held finger: the tool to go back to as it lifts.
    picking: Option<Tool>,
}

/// One finger down on the canvas, watched for a hold.
#[derive(Clone, Copy)]
struct FingerHold {
    id: u64,
    origin: egui::Pos2,
    at: egui::Pos2,
    /// When it landed (egui's clock, seconds).
    start: f64,
    /// It went further than [`HOLD_PICK_SLOP`] from where it landed.
    moved: bool,
}

/// Where the history stood as a stroke began.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ActionMark {
    /// This many pushes.
    Pushes(u64),
    /// Once the first `n` strokes the stroke worker ended are filed (earlier
    /// strokes may still be painting: the new one doesn't wait for them).
    AfterStroke(u64),
}

struct Gesture {
    /// When it began (egui's clock, seconds).
    start: f64,
    max_touches: usize,
    /// Accumulated movement (points) used to tell taps from gestures.
    travel: f32,
    /// What holding it still has done: undos run (two fingers), the
    /// QuickMenu opened (three). A gesture that did something isn't a tap.
    held: u32,
}

/// How many undos two fingers held still for `held` seconds have earned.
fn hold_undos(held: f64) -> u32 {
    if held < HOLD_UNDO_DELAY {
        0
    } else {
        1 + ((held - HOLD_UNDO_DELAY) / HOLD_UNDO_REPEAT) as u32
    }
}

impl TouchState {
    pub(crate) fn finger_down(&self) -> bool {
        !self.touches.is_empty()
    }

    /// The window lost the focus: the fingers' and the pen's lifts may
    /// never arrive. Forget them, and any gesture they made.
    pub(crate) fn forget_contacts(&mut self) {
        self.touches.clear();
        self.ignored.clear();
        self.gesture = None;
        self.hold = None;
        self.suppress_pointer = false;
        self.stroke_started = None;
        self.pen_active = false;
    }
}

impl PainterApp {
    /// Apply the pen's pressure curve (a device setting) to raw pressure
    /// (0..=1). What pressure then drives (size, opacity, flow) is per brush.
    pub(crate) fn map_pressure(&mut self, raw: f32) -> f32 {
        let raw = raw.clamp(0.0, 1.0);
        let mapped = self.workspace.pressure_curve.eval(raw).clamp(0.0, 1.0);
        self.workspace.last_pressure = Some((raw, mapped));
        mapped
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

    /// Remember where the history stands, as a stroke begins.
    pub(crate) fn mark_action(&mut self) {
        // The previous strokes may still be painting: the mark settles once
        // they're filed (without waiting for them here), so none can land
        // after it and be mistaken for this one.
        self.viewport.touch.action_mark =
            Some(ActionMark::AfterStroke(self.stroke_worker.ends_sent()));
        self.sync_stroke_worker();
    }

    /// The first `ended` strokes are filed: a mark waiting on them (or on
    /// fewer) now knows where the history stands.
    pub(crate) fn settle_action_mark(&mut self, ended: u64) {
        let touch = &mut self.viewport.touch;
        if let Some(ActionMark::AfterStroke(n)) = touch.action_mark
            && n <= ended
        {
            touch.action_mark = Some(ActionMark::Pushes(self.layer_state.history.push_count()));
        }
    }

    /// End the stroke in progress (brush or smudge/blur) and take it back,
    /// without leaving a redo entry. Only what it recorded is undone: a
    /// stroke that painted nothing leaves the history alone.
    pub(crate) fn discard_current_action(&mut self) {
        // The mark belongs to the last stroke begun; once that has ended
        // there is nothing in progress to take back.
        let in_progress = self.brush_state.is_drawing || self.brush_state.blend_stroke.is_some();
        // (Finishing it files it, which settles the mark.)
        self.release_canvas();
        self.blend_release();
        let mark = self.viewport.touch.action_mark.take();
        let Some(ActionMark::Pushes(count)) = mark.filter(|_| in_progress) else {
            return;
        };
        if self.layer_state.history.push_count() > count {
            // (Now: its redo entry is dropped straight after.)
            self.apply_history_now(false);
            self.layer_state.history.discard_redo();
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
            Tool::Select(_) => self.select_cancel(),
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

impl PainterApp {
    /// A finger held still picks: the tool before is kept to come back to,
    /// and whatever the finger started (a young stroke) is taken back.
    fn start_hold_pick(&mut self) {
        if self.workspace.finger_painting {
            self.cancel_finger_action(true);
        }
        self.viewport.touch.picking = Some(self.active_tool);
        self.active_tool = Tool::Eyedropper;
        self.workspace.eyedropper.before = None;
    }

    /// The held finger lifted (or a gesture took over): back to the tool
    /// before, the colour picked kept.
    pub(crate) fn end_hold_pick(&mut self) {
        if let Some(tool) = self.viewport.touch.picking.take() {
            self.active_tool = tool;
            self.workspace.eyedropper.before = None;
        }
    }

    /// Pick under the held finger at `pos` (screen), if it's on the picture.
    fn hold_pick_at(&mut self, pos: egui::Pos2, area: egui::Rect) {
        let (origin, center) = crate::app::view::render::canvas_placement(self, area);
        let (at, inside) = self.screen_to_canvas(pos, origin, center);
        if inside {
            self.pick_color(at);
        }
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
    let (events, now) = ctx.input(|i| (i.events.clone(), i.time));
    let mut repaint = false;

    // The pen landed while fingers were down: they're the hand holding it.
    let touch = &mut app.viewport.touch;
    if touch.pen_active && !touch.touches.is_empty() {
        let fingers: Vec<u64> = touch.touches.drain().map(|(id, _)| id).collect();
        touch.ignored.extend(fingers);
        touch.gesture = None;
        touch.hold = None;
        app.end_hold_pick();
        app.cancel_finger_action(true);
        repaint = true;
    }
    let mut gesture_this_frame = app.viewport.touch.gesture.is_some();
    // A pick that ends this frame: its finger's lift isn't a release on
    // the canvas either.
    let was_picking = app.viewport.touch.picking.is_some();
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
                // Fingers landing on panels or menus are
                // the UI's; only the canvas itself starts a gesture.
                let on_canvas = area.contains(pos) && ctx.layer_id_at(pos) == Some(canvas.layer_id);
                if !on_canvas && touch.touches.is_empty() {
                    continue;
                }
                touch.touches.insert(id.0, pos);
                if touch.touches.len() == 1 {
                    touch.hold = Some(FingerHold {
                        id: id.0,
                        origin: pos,
                        at: pos,
                        start: now,
                        moved: false,
                    });
                }
                if touch.touches.len() >= 2 {
                    touch.hold = None;
                    if touch.gesture.is_none() {
                        touch.gesture = Some(Gesture {
                            start: now,
                            max_touches: touch.touches.len(),
                            travel: 0.0,
                            held: 0,
                        });
                        gesture_this_frame = true;
                        app.end_hold_pick();
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
                if let Some(h) = &mut touch.hold
                    && h.id == id.0
                {
                    h.at = pos;
                    h.moved |= h.origin.distance(pos) >= HOLD_PICK_SLOP;
                }
                if touch.picking.is_some() {
                    app.hold_pick_at(pos, area);
                    repaint = true;
                    continue;
                }
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
                if touch.hold.is_some_and(|h| h.id == id.0) {
                    touch.hold = None;
                    if touch.picking.is_some() {
                        app.end_hold_pick();
                        repaint = true;
                        continue;
                    }
                }
                let touch = &mut app.viewport.touch;
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
                    let quick = now - g.start < TAP_MAX_DURATION;
                    if quick
                        && g.travel < TAP_MAX_TRAVEL
                        && g.held == 0
                        && phase == egui::TouchPhase::End
                    {
                        tap = Some(g.max_touches);
                    }
                    let deg = app.viewport.rotation.to_degrees().rem_euclid(360.0);
                    if !(ROTATION_SNAP_DEG..=360.0 - ROTATION_SNAP_DEG).contains(&deg) {
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

    repaint |= held_gestures(app, ctx, now, area);

    match tap {
        Some(2) => app.apply_history(false),
        Some(n) if n >= 3 => app.apply_history(true),
        _ => {}
    }

    let touch = &mut app.viewport.touch;
    // egui's pointer follows the first finger, which may be an ignored palm.
    touch.suppress_pointer = gesture_this_frame
        || touch.gesture.is_some()
        || touch.picking.is_some()
        || was_picking
        || !touch.ignored.is_empty()
        || (touch.finger_down() && !app.workspace.finger_painting);
    repaint || tap.is_some()
}

/// Fingers held still: two undo (again and again), three open the
/// QuickMenu, one picks a colour. Returns whether a repaint is needed.
fn held_gestures(app: &mut PainterApp, ctx: &egui::Context, now: f64, area: egui::Rect) -> bool {
    let touch = &mut app.viewport.touch;
    let fingers = touch.touches.len();
    let wake = |secs: f64| ctx.request_repaint_after(Duration::from_secs_f64(secs.max(0.0)));
    if let Some(g) = &mut touch.gesture
        && g.travel < TAP_MAX_TRAVEL
        && fingers == g.max_touches
    {
        let held = now - g.start;
        match fingers {
            2 => {
                // At most a couple a frame: a slow frame doesn't undo a burst.
                let due = hold_undos(held).min(g.held + 2);
                let undos = due - g.held;
                g.held = due;
                // (Undo n runs at the delay plus n - 1 repeats.)
                wake(HOLD_UNDO_DELAY + due as f64 * HOLD_UNDO_REPEAT - held);
                for _ in 0..undos {
                    app.apply_history(false);
                }
                return undos > 0;
            }
            3 if g.held == 0 => {
                if held >= HOLD_MENU_DELAY {
                    g.held = 1;
                    let center = touch
                        .touches
                        .values()
                        .fold(egui::Vec2::ZERO, |sum, p| sum + p.to_vec2())
                        / 3.0;
                    crate::ui::quick_menu::open(ctx, center.to_pos2());
                    return true;
                }
                wake(HOLD_MENU_DELAY - held);
            }
            _ => {}
        }
        return false;
    }
    let Some(h) = touch.hold else {
        return false;
    };
    if touch.picking.is_some() || h.moved || fingers != 1 || touch.gesture.is_some() {
        return false;
    }
    let ws = &app.workspace;
    let allowed = ws.touch_mode
        && ws.ui.hold_eyedropper
        && !matches!(app.active_tool, Tool::Eyedropper)
        && (!ws.finger_painting || hold_pick_tool(app.active_tool));
    if !allowed {
        return false;
    }
    let held = now - h.start;
    if held < HOLD_PICK_DELAY {
        wake(HOLD_PICK_DELAY - held);
        return false;
    }
    app.start_hold_pick();
    app.hold_pick_at(h.at, area);
    true
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
        app.layer_state.history.stacks().0.len()
    }

    #[test]
    fn losing_the_focus_forgets_fingers_and_gestures() {
        let mut app = app();
        let touch = &mut app.viewport.touch;
        touch.touches.insert(1, eframe::egui::pos2(10.0, 10.0));
        touch.touches.insert(2, eframe::egui::pos2(50.0, 10.0));
        touch.gesture = Some(super::Gesture {
            start: 0.0,
            max_touches: 2,
            travel: 0.0,
            held: 0,
        });
        touch.ignored.insert(3);
        touch.pen_active = true;
        app.viewport.is_panning = true;
        crate::app::input::handle_focus_lost(&mut app);
        let touch = &app.viewport.touch;
        assert!(!touch.finger_down() && touch.gesture.is_none() && touch.ignored.is_empty());
        assert!(!touch.pen_active && !app.viewport.is_panning);
    }

    #[test]
    fn a_discarded_stroke_leaves_no_paint_and_no_undo_step() {
        let mut app = app();
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 20.0));
        app.discard_current_action();
        assert_eq!(undo_len(&app), 0);
        assert_eq!(app.layer_state.history.stacks().1.len(), 0, "no redo");
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
    fn discarding_takes_back_only_the_newest_of_strokes_still_painting() {
        let mut app = app();
        // Pressed one after the other with no frame between: the worker
        // may still be painting the earlier ones as the last begins (the
        // press doesn't wait for them).
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 20.0));
        app.finish_stroke();
        stroke(&mut app, Vec2::new(10.0, 60.0), Vec2::new(40.0, 70.0));
        app.finish_stroke();
        stroke(&mut app, Vec2::new(80.0, 80.0), Vec2::new(100.0, 90.0));
        app.discard_current_action();
        assert_eq!(undo_len(&app), 2, "the two earlier strokes stay");
        assert_eq!(app.layer_state.history.stacks().1.len(), 0, "no redo");
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
        // (Undone to transparent, the tile may be dropped.)
        let tile = app.canvas.get_layer_tile_data(1, 0, 0);
        assert!(tile.is_none_or(|t| t == blank));
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

    use eframe::egui;
    use egui::TouchPhase::{End, Move, Start};

    /// One frame of touch `events` at `t` seconds over an 800×600 canvas
    /// panel.
    fn touch_frame(
        app: &mut crate::PainterApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        t: f64,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            time: Some(t),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response =
                    ui.allocate_response(ui.available_size(), egui::Sense::click_and_drag());
                super::handle_touch(app, ctx, &response);
            });
        });
    }

    fn finger(id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
        egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(id),
            phase,
            pos,
            force: None,
        }
    }

    #[test]
    fn holding_two_fingers_undoes_again_and_again_and_a_tap_still_undoes_once() {
        let mut app = app();
        for y in [10.0, 40.0, 70.0, 100.0] {
            stroke(&mut app, Vec2::new(10.0, y), Vec2::new(100.0, y));
            app.release_canvas();
        }
        assert_eq!(undo_len(&app), 4);
        let ctx = egui::Context::default();
        let (a, b) = (egui::pos2(300.0, 300.0), egui::pos2(400.0, 300.0));
        touch_frame(&mut app, &ctx, vec![], 0.0);
        let both = |phase, ids: (u64, u64)| vec![finger(ids.0, phase, a), finger(ids.1, phase, b)];
        touch_frame(&mut app, &ctx, both(Start, (1, 2)), 0.1);
        touch_frame(&mut app, &ctx, vec![], 0.4);
        assert_eq!(undo_len(&app), 4, "not yet");
        let t = 0.1 + super::HOLD_UNDO_DELAY + 0.01;
        touch_frame(&mut app, &ctx, vec![], t);
        assert_eq!(undo_len(&app), 3, "the hold undoes");
        touch_frame(&mut app, &ctx, vec![], t + super::HOLD_UNDO_REPEAT);
        assert_eq!(undo_len(&app), 2, "and again while held");
        touch_frame(&mut app, &ctx, both(End, (1, 2)), 1.0);
        assert_eq!(undo_len(&app), 2, "lifting isn't a tap");

        touch_frame(&mut app, &ctx, both(Start, (3, 4)), 2.0);
        touch_frame(&mut app, &ctx, both(End, (3, 4)), 2.1);
        assert_eq!(undo_len(&app), 1, "a quick tap undoes once");
    }

    #[test]
    fn hold_undos_come_at_the_delay_then_every_repeat() {
        use super::{HOLD_UNDO_DELAY as D, HOLD_UNDO_REPEAT as R, hold_undos};
        assert_eq!(hold_undos(0.0), 0);
        assert_eq!(hold_undos(D - 0.01), 0);
        assert_eq!(hold_undos(D), 1);
        assert_eq!(hold_undos(D + R - 0.01), 1);
        assert_eq!(hold_undos(D + 3.0 * R), 4);
    }

    /// A canvas all red, touch mode with finger painting, the brush black;
    /// and where the canvas's centre is on screen.
    fn red_canvas() -> (crate::PainterApp, egui::Pos2) {
        let mut app = app();
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, ty, vec![Color32::RED; TILE * TILE]);
        }
        app.workspace.touch_mode = true;
        app.workspace.finger_painting = true;
        app.brush_state.brush.brush_options.color = Color32::BLACK;
        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let (_, center) = crate::app::view::render::canvas_placement(&app, area);
        (app, center)
    }

    #[test]
    fn a_finger_held_still_picks_a_colour_until_it_lifts() {
        use crate::app::tools::Tool;
        let (mut app, at) = red_canvas();
        let ctx = egui::Context::default();
        touch_frame(&mut app, &ctx, vec![], 0.0);
        touch_frame(&mut app, &ctx, vec![finger(1, Start, at)], 0.1);
        let nudge = at + egui::vec2(3.0, 0.0);
        touch_frame(&mut app, &ctx, vec![finger(1, Move, nudge)], 0.3);
        assert!(matches!(app.active_tool, Tool::Brush), "not yet");
        touch_frame(&mut app, &ctx, vec![], 0.1 + super::HOLD_PICK_DELAY + 0.01);
        assert!(matches!(app.active_tool, Tool::Eyedropper), "picking");
        assert!(
            app.viewport.touch.suppress_pointer,
            "the finger doesn't paint"
        );
        let picked = app.brush_state.brush.brush_options.color;
        assert_eq!((picked.r(), picked.g(), picked.b()), (255, 0, 0));
        touch_frame(&mut app, &ctx, vec![finger(1, End, nudge)], 1.0);
        assert!(matches!(app.active_tool, Tool::Brush), "back to the brush");
        assert_eq!(app.workspace.eyedropper.before, None, "the ring is gone");
        assert_eq!(app.brush_state.brush.brush_options.color.r(), 255);
    }

    #[test]
    fn losing_the_focus_mid_pick_brings_the_tool_back() {
        use crate::app::tools::Tool;
        let (mut app, at) = red_canvas();
        let ctx = egui::Context::default();
        touch_frame(&mut app, &ctx, vec![], 0.0);
        touch_frame(&mut app, &ctx, vec![finger(1, Start, at)], 0.1);
        touch_frame(&mut app, &ctx, vec![], 0.1 + super::HOLD_PICK_DELAY + 0.01);
        assert!(matches!(app.active_tool, Tool::Eyedropper), "picking");
        crate::app::input::handle_focus_lost(&mut app);
        assert!(matches!(app.active_tool, Tool::Brush));
        assert!(app.viewport.touch.picking.is_none());
    }

    #[test]
    fn a_finger_that_moved_or_a_setting_turned_off_picks_nothing() {
        use crate::app::tools::Tool;
        let (mut app, at) = red_canvas();
        let ctx = egui::Context::default();
        touch_frame(&mut app, &ctx, vec![], 0.0);
        touch_frame(&mut app, &ctx, vec![finger(1, Start, at)], 0.1);
        // A stroke that came back to where it began.
        let away = at + egui::vec2(30.0, 0.0);
        touch_frame(&mut app, &ctx, vec![finger(1, Move, away)], 0.2);
        touch_frame(&mut app, &ctx, vec![finger(1, Move, at)], 0.3);
        touch_frame(&mut app, &ctx, vec![], 1.0);
        assert!(matches!(app.active_tool, Tool::Brush));
        touch_frame(&mut app, &ctx, vec![finger(1, End, at)], 1.1);

        app.workspace.ui.hold_eyedropper = false;
        touch_frame(&mut app, &ctx, vec![finger(2, Start, at)], 2.0);
        touch_frame(&mut app, &ctx, vec![], 3.0);
        assert!(matches!(app.active_tool, Tool::Brush), "turned off");
        assert_eq!(app.brush_state.brush.brush_options.color, Color32::BLACK);
    }

    #[test]
    fn three_fingers_held_still_open_the_quick_menu() {
        let mut app = app();
        let ctx = egui::Context::default();
        let three = |phase| {
            [300.0, 350.0, 400.0]
                .into_iter()
                .enumerate()
                .map(|(i, x)| finger(i as u64 + 1, phase, egui::pos2(x, 300.0)))
                .collect::<Vec<_>>()
        };
        touch_frame(&mut app, &ctx, vec![], 0.0);
        touch_frame(&mut app, &ctx, three(Start), 0.1);
        assert!(!crate::ui::quick_menu::is_open(&ctx));
        touch_frame(&mut app, &ctx, vec![], 0.1 + super::HOLD_MENU_DELAY + 0.01);
        assert!(crate::ui::quick_menu::is_open(&ctx));
        let redo = app.layer_state.history.stacks().1.len();
        touch_frame(&mut app, &ctx, three(End), 1.0);
        assert_eq!(
            app.layer_state.history.stacks().1.len(),
            redo,
            "not a redo tap"
        );
    }
}
