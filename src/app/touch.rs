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

use super::PainterApp;
use super::tools::Tool;
use crate::selection::transform::TransformState;
use eframe::egui;
use std::collections::HashMap;
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

    /// Pressure for the current pointer: curved pen pressure when a stylus
    /// is drawing, otherwise 1.0 (mouse, finger).
    pub(crate) fn pointer_pressure(&self) -> f32 {
        match crate::tablet::pen_state() {
            Some(pen) if pen.is_stylus && !self.viewport.touch.finger_down() => {
                self.map_pressure(pen.pressure)
            }
            _ => 1.0,
        }
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

    /// End the in-progress stroke; undo it (without leaving a redo entry)
    /// if it only just started.
    fn cancel_recent_stroke(&mut self) {
        if !self.brush_state.is_drawing {
            return;
        }
        let young = self
            .viewport
            .touch
            .stroke_started
            .is_some_and(|t| t.elapsed() < STROKE_CANCEL_WINDOW);
        self.release_canvas();
        if young {
            self.apply_history(false);
            let active = self.canvas.active_layer_idx;
            if let Some(history) = self.layer_state.histories.get_mut(active) {
                history.discard_redo();
            }
        }
    }
}

impl PainterApp {
    /// A second finger landed: whatever the first finger started (stroke,
    /// selection drag, transform drag) was the start of a gesture.
    fn cancel_gesture_start(&mut self) {
        self.cancel_recent_stroke();
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
    let mut gesture_this_frame = app.viewport.touch.gesture.is_some();
    let mut tap: Option<usize> = None;

    for event in &events {
        let egui::Event::Touch { id, phase, pos, .. } = *event else {
            continue;
        };
        let touch = &mut app.viewport.touch;
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
                        app.cancel_gesture_start();
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
    touch.suppress_pointer = gesture_this_frame
        || touch.gesture.is_some()
        || (touch.finger_down() && !app.workspace.finger_painting);
    repaint || tap.is_some()
}
