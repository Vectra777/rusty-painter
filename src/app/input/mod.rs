//! Input: this frame's pointer, pen and keyboard events, routed to the
//! active tool. Touch gestures are in [`touch`], shortcuts in
//! [`shortcuts`].
pub(crate) mod keyboard;
pub(crate) mod keymap;
pub(crate) mod shortcuts;
pub(crate) mod touch;

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::transform;
use crate::selection::SelectionMode;
use crate::tablet::{TabletPhase, TabletSample};
use eframe::egui;
use eframe::egui::Vec2;

#[derive(Clone, Copy)]
struct CanvasPlacement {
    origin: egui::Pos2,
    center: egui::Pos2,
}

pub fn handle_input(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    origin: egui::Pos2,
    canvas_center: egui::Pos2,
    pen: &[TabletSample],
) {
    let placement = CanvasPlacement {
        origin,
        center: canvas_center,
    };
    handle_pen(app, ctx, response, placement, pen);
    settle_lost_release(app, ctx);
    handle_events(app, ctx, response, placement, !pen.is_empty());
    settle_lost_release(app, ctx);
}

/// The canvas thinks the mouse button is down but it isn't: the release
/// went to a frame that didn't pass input to the canvas (a dialog opened by
/// that very click, a fill running). End the press now, before a pointer
/// move could paint with a button that's up.
fn settle_lost_release(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.viewport.is_primary_down {
        return;
    }
    let (down, button_event) = ctx.input(|i| {
        let event = i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    ..
                }
            )
        });
        (i.pointer.primary_down(), event)
    });
    // A release among this frame's events is handled with them.
    if !down && !button_event {
        app.viewport.is_primary_down = false;
        handle_primary_release(app);
    }
}

/// Pen contact samples (with pressure), in order.
fn handle_pen(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    placement: CanvasPlacement,
    samples: &[TabletSample],
) {
    // The pen's lift got lost (focus change, app paused): end its contact
    // rather than keep ignoring every other input.
    let touch = &mut app.viewport.touch;
    if touch.pen_on_canvas && samples.is_empty() && !touch.pen_active {
        touch.pen_on_canvas = false;
        handle_primary_release(app);
    }
    for sample in samples {
        let pos = egui::Pos2::new(sample.pos[0], sample.pos[1]);
        let (clamped, inside) = app.screen_to_canvas(pos, placement.origin, placement.center);
        // The brush works off the canvas too (dabs are clipped to it).
        let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
        let pressure = app.map_pressure(sample.pressure);
        // The lean in canvas terms: a short screen vector mapped through the
        // view (zoom, rotation, flip), like the pen's position.
        app.viewport.touch.pen_tilt = sample.tilt.map(|[x, y]| {
            let lean = (x * x + y * y).sqrt().min(1.0);
            let tip = pos + egui::vec2(x, y) * (20.0 / lean.max(1e-3));
            let to = app.screen_to_canvas_raw(tip, placement.origin, placement.center);
            let direction = crate::brush_engine::dynamics::direction(raw, to).unwrap_or(0.0);
            crate::brush_engine::dynamics::PenTilt { lean, direction }
        });
        // The barrel's turn, through the view like the lean (a screen
        // angle, y down, mapped to the canvas).
        app.viewport.touch.pen_barrel = crate::brush_engine::dynamics::PenBarrel {
            rotation: sample.roll.map(|r| {
                let tip = pos + egui::vec2(r.cos(), -r.sin()) * 20.0;
                let to = app.screen_to_canvas_raw(tip, placement.origin, placement.center);
                crate::brush_engine::dynamics::direction(raw, to).unwrap_or(0.0)
            }),
            wheel: sample.wheel,
        };
        app.viewport.cursor_canvas = inside.then_some(clamped);
        let touch = &mut app.viewport.touch;
        match sample.phase {
            TabletPhase::Down => {
                // Only a pen landing on the canvas panel acts on it (not on a
                // window, menu or fader over it); dragging in from the UI
                // doesn't paint.
                let over =
                    response.rect.contains(pos) && ctx.layer_id_at(pos) == Some(response.layer_id);
                touch.pen_on_canvas = over;
                if !over {
                    continue;
                }
                // Flipping a pen to its eraser end switches to the eraser.
                if matches!(app.active_tool, Tool::Brush) {
                    app.sync_pen_eraser(sample.is_eraser);
                }
                handle_primary_press(app, response, true, (clamped, inside), raw, pressure);
            }
            TabletPhase::Move if touch.pen_on_canvas => {
                handle_pen_drag(app, ctx, clamped, inside, raw, pressure);
            }
            TabletPhase::Up if touch.pen_on_canvas => {
                touch.pen_on_canvas = false;
                handle_primary_release(app);
            }
            TabletPhase::Cancel if touch.pen_on_canvas => {
                touch.pen_on_canvas = false;
                // Not intended input (a palm, a system gesture).
                app.discard_current_action();
                handle_primary_release(app);
            }
            _ => {}
        }
        ctx.request_repaint();
    }
}

fn handle_pen_drag(
    app: &mut PainterApp,
    ctx: &egui::Context,
    pos: Vec2,
    inside: bool,
    raw: Vec2,
    pressure: f32,
) {
    let snap = ctx.input(|i| i.modifiers.shift);
    if app.guide_lines_drag(raw)
        || (app.guides_dragging() && app.guides_drag(app.snap_point(raw), snap))
    {
        return;
    }
    let (pos, raw) = (
        app.tool_snap(pos, false, true),
        app.tool_snap(raw, false, false),
    );
    match app.active_tool {
        Tool::Brush => app.add_stroke_point(raw, pressure),
        Tool::Select(_) => app.select_move(pos),
        Tool::Transform(_) => {
            let keep_aspect = ctx.input(|i| i.modifiers.shift);
            transform::transform_drag(app, pos, keep_aspect);
        }
        Tool::Fill => app.fill_drag(pos),
        Tool::Liquify => app.liquify_drag(pos),
        Tool::Smudge | Tool::Blur => app.blend_drag(pos, pressure),
        Tool::Eyedropper => {
            if inside {
                app.pick_color(pos);
            }
        }
        Tool::Shape(_) => app.shape_move(raw, shape_mods(ctx)),
        Tool::Gradient => app.gradient_drag(raw, ctx.input(|i| i.modifiers.shift)),
        Tool::Text => app.text_drag(raw),
    }
}

/// Shift keeps a shape's proportions, Alt draws it from the centre.
fn shape_mods(ctx: &egui::Context) -> crate::app::tools::shape::ShapeMods {
    let m = ctx.input(|i| i.modifiers);
    crate::app::tools::shape::ShapeMods {
        constrain: m.shift,
        from_center: m.alt,
    }
}

fn handle_events(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    placement: CanvasPlacement,
    pen_samples: bool,
) {
    let events = ctx.input(|i| i.events.clone());
    // Fingers that belong to a gesture (or that may not paint) also arrive
    // as pointer events; leave those to the touch handler.
    let suppress = app.viewport.touch.suppress_pointer;
    // The pen's own samples drive the canvas; the pointer events the same
    // pen produces are ignored. On Android and X11 the pen is the mouse
    // pointer; on Windows it arrives as touches, which egui turns into
    // pointer events right after each touch event.
    let pen_active = app.viewport.touch.pen_active || pen_samples;
    // Nothing else joins a pen stroke in progress (a mouse moved meanwhile).
    let pen_stroke = app.viewport.touch.pen_on_canvas;
    let mut from_touch = false;

    for event in events {
        let is_pointer = matches!(
            event,
            egui::Event::PointerButton {
                button: egui::PointerButton::Primary,
                ..
            } | egui::Event::PointerMoved(_)
        );
        match event {
            egui::Event::Touch { .. } => from_touch = true,
            egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } => {}
            _ => from_touch = false,
        }
        let pen_pointer =
            pen_stroke || (pen_active && (app.viewport.touch.pen_is_pointer || from_touch));
        match event {
            _ if is_pointer && (suppress || pen_pointer) => {}
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                ..
            } => {
                handle_mouse_button(app, ctx, response, pos, button, pressed, placement);
            }
            egui::Event::Key { key, pressed, .. } => {
                handle_keyboard(app, key, pressed);
            }
            egui::Event::PointerMoved(pos) => {
                handle_pointer_move(app, ctx, response, pos, placement);
            }
            egui::Event::PointerGone => {
                app.viewport.last_pointer_pos = None;
            }
            egui::Event::MouseWheel { unit, delta, .. } => {
                handle_mouse_wheel(app, ctx, response, unit, delta);
            }
            egui::Event::WindowFocused(false) => handle_focus_lost(app),
            _ => {}
        }
    }
}

/// The window lost focus (Alt+Tab, another window clicked) mid-drag: the
/// button's release may never arrive, and egui still thinks it's down.
/// End whatever the press started, and stop panning and rotating.
fn handle_focus_lost(app: &mut PainterApp) {
    let viewport = &mut app.viewport;
    viewport.is_panning = false;
    viewport.is_rotating = false;
    viewport.last_pointer_pos = None;
    let pressed = std::mem::take(&mut viewport.is_primary_down);
    let pen = std::mem::take(&mut viewport.touch.pen_on_canvas);
    if pressed || pen {
        handle_primary_release(app);
    }
}

fn handle_mouse_button(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    pos: egui::Pos2,
    button: egui::PointerButton,
    pressed: bool,
    placement: CanvasPlacement,
) {
    let canvas_pos = app.screen_to_canvas(pos, placement.origin, placement.center);
    let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
    // Pans/rotations measure from here, not from wherever the pointer last moved.
    app.viewport.last_pointer_pos = Some(pos);

    match button {
        egui::PointerButton::Primary => {
            handle_primary_button(app, ctx, response, canvas_pos, raw, pressed);
        }
        egui::PointerButton::Secondary => {
            app.viewport.is_panning = pressed && response.hovered();
            app.radial_right_button(pos, pressed, response.hovered());
        }
        egui::PointerButton::Middle => {
            app.viewport.is_rotating = pressed && response.hovered();
        }
        _ => {}
    }
}

fn handle_primary_button(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    canvas_pos: (Vec2, bool),
    raw: Vec2,
    pressed: bool,
) {
    // Only a press on the canvas holds it down: one on a menu, panel or
    // dialog is theirs (and its release may never reach the canvas).
    app.viewport.is_primary_down = pressed && response.hovered();

    let (space_down, secondary_down) = ctx.input(|i| {
        (
            i.key_down(egui::Key::Space),
            i.pointer.button_down(egui::PointerButton::Secondary),
        )
    });

    if pressed && space_down {
        app.viewport.is_panning = true;
        return;
    }
    if !pressed && !secondary_down {
        app.viewport.is_panning = false;
    }

    if pressed {
        // Mouse and finger input paint at full pressure.
        handle_primary_press(app, response, response.hovered(), canvas_pos, raw, 1.0);
    } else {
        handle_primary_release(app);
    }
}

/// A mouse, finger or pen press. `over` is whether it landed on the canvas
/// panel; `pressure` is already curved.
fn handle_primary_press(
    app: &mut PainterApp,
    response: &egui::Response,
    over: bool,
    canvas_pos: (Vec2, bool),
    raw: Vec2,
    pressure: f32,
) {
    // The brush may start a stroke off the canvas (on the canvas panel);
    // other tools need a press on the canvas itself.
    let brush = matches!(
        app.active_tool,
        Tool::Brush | Tool::Shape(_) | Tool::Gradient | Tool::Text
    );
    if app.viewport.is_panning || !over {
        return;
    }
    // Ctrl on a guide line (or beside the canvas) moves (or pulls out) one.
    if app.guide_lines_press(raw, response.ctx.input(|i| i.modifiers.command)) {
        return;
    }
    // A guide handle under the press is the guide's, not the tool's.
    if app.guides_press(raw) {
        return;
    }
    if !(canvas_pos.1 || brush) {
        return;
    }

    // Alt+click samples a color with any painting tool.
    let alt_held = response.ctx.input(|i| i.modifiers.alt);
    if alt_held && matches!(app.active_tool, Tool::Brush) {
        if canvas_pos.1 {
            app.pick_color(canvas_pos.0);
        }
        return;
    }
    // Onto guides and the grid, when snapping to them.
    let canvas_pos = (app.tool_snap(canvas_pos.0, true, true), canvas_pos.1);
    let raw = app.tool_snap(raw, true, false);

    match app.active_tool {
        Tool::Brush => app.start_stroke_with_pressure(raw, pressure),
        Tool::Select(t) => {
            // Shift adds to the selection, Alt subtracts, for this drag.
            let mods = response.ctx.input(|i| i.modifiers);
            let mode = if mods.shift {
                SelectionMode::Add
            } else if mods.alt {
                SelectionMode::Subtract
            } else {
                app.selection_manager.mode
            };
            app.select_press(canvas_pos.0, t, mode);
        }
        Tool::Transform(_) => transform::transform_press(app, canvas_pos.0),
        Tool::Eyedropper => app.pick_color(canvas_pos.0),
        Tool::Fill => app.fill_press(canvas_pos.0),
        Tool::Liquify => app.liquify_press(canvas_pos.0),
        Tool::Smudge
            if app.workspace.blend.smudge_mode == crate::app::tools::blend::SmudgeMode::Clone
                && response.ctx.input(|i| i.modifiers.command) =>
        {
            // Ctrl+click: where to clone from.
            app.workspace.blend.set_clone_source(canvas_pos.0);
        }
        Tool::Smudge | Tool::Blur => app.blend_press(raw, pressure),
        Tool::Shape(kind) => app.shape_press(kind, raw),
        Tool::Gradient => app.gradient_press(raw),
        Tool::Text => app.text_press(raw),
    }
}

fn handle_primary_release(app: &mut PainterApp) {
    if app.guide_lines_release() || app.guides_release() {
        return;
    }
    if matches!(app.active_tool, Tool::Transform(_)) {
        app.release_canvas();
    }
    match app.active_tool {
        Tool::Brush => app.finish_stroke(),
        Tool::Select(_) => app.select_release(),
        Tool::Eyedropper => {}
        Tool::Fill => app.fill_release(),
        Tool::Liquify => app.liquify_release(),
        Tool::Smudge | Tool::Blur => app.blend_release(),
        Tool::Transform(_) => transform::transform_release(app),
        Tool::Shape(_) => app.shape_release(),
        Tool::Gradient => app.gradient_release(),
        Tool::Text => app.text_release(),
    }
}

fn handle_keyboard(app: &mut PainterApp, key: egui::Key, pressed: bool) {
    if pressed && key == egui::Key::Enter {
        transform::commit_floating_layer(app);
        app.liquify_commit();
        app.magnetic_close();
        // Enter finishes a polygon being built, then applies the shape.
        if app
            .workspace
            .shapes
            .session
            .as_ref()
            .is_some_and(|s| s.building)
        {
            app.shape_finish_polygon();
        } else {
            app.shape_commit();
        }
        app.gradient_commit();
    }
}

fn handle_pointer_move(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    pos: egui::Pos2,
    placement: CanvasPlacement,
) {
    // Movement since the previous pointer event. `pointer.delta()` is the
    // whole frame's movement, and a frame can carry several move events, so
    // using it here panned/rotated several times too far.
    let delta = app
        .viewport
        .last_pointer_pos
        .map_or(egui::Vec2::ZERO, |last| pos - last);
    app.viewport.last_pointer_pos = Some(pos);

    let (canvas_point, inside) = app.screen_to_canvas(pos, placement.origin, placement.center);
    app.viewport.cursor_canvas = (inside && response.hovered()).then_some(canvas_point);

    if app.viewport.is_rotating {
        app.workspace.auto_fit = false;
        app.viewport.rotation += delta.x * -0.005;
        ctx.request_repaint();
    } else if app.viewport.is_panning {
        app.workspace.auto_fit = false;
        app.viewport.offset.x += delta.x;
        app.viewport.offset.y += delta.y;
        ctx.request_repaint();
    } else if app.guide_lines_dragging() {
        let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
        app.guide_lines_drag(raw);
        ctx.request_repaint();
    } else if app.guides_dragging() {
        let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
        app.guides_drag(app.snap_point(raw), ctx.input(|i| i.modifiers.shift));
        ctx.request_repaint();
    } else {
        let (clamped, is_inside) = app.screen_to_canvas(pos, placement.origin, placement.center);
        let clamped = app.tool_snap(clamped, false, true);
        if matches!(app.active_tool, Tool::Brush) {
            let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
            handle_brush_move(app, response, raw);
        } else if matches!(app.active_tool, Tool::Shape(_)) {
            // Shapes may reach off the canvas, like strokes.
            let raw = app.tool_snap(
                app.screen_to_canvas_raw(pos, placement.origin, placement.center),
                false,
                false,
            );
            app.shape_move(raw, shape_mods(ctx));
            ctx.request_repaint();
        } else if matches!(app.active_tool, Tool::Gradient) {
            if app.viewport.is_primary_down {
                let raw = app.tool_snap(
                    app.screen_to_canvas_raw(pos, placement.origin, placement.center),
                    false,
                    false,
                );
                app.gradient_drag(raw, ctx.input(|i| i.modifiers.shift));
                ctx.request_repaint();
            }
        } else if matches!(app.active_tool, Tool::Text) {
            if app.viewport.is_primary_down {
                let raw = app.tool_snap(
                    app.screen_to_canvas_raw(pos, placement.origin, placement.center),
                    false,
                    false,
                );
                app.text_drag(raw);
                ctx.request_repaint();
            }
        } else {
            handle_tool_move(app, ctx, response, clamped, is_inside);
        }
    }
}

fn handle_tool_move(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    pos: Vec2,
    is_inside: bool,
) {
    match app.active_tool {
        Tool::Brush => handle_brush_move(app, response, pos),
        Tool::Select(_) => handle_select_move(app, ctx, pos),
        Tool::Fill => {
            if app.viewport.is_primary_down {
                app.fill_drag(pos);
                ctx.request_repaint();
            }
        }
        Tool::Liquify => {
            if app.viewport.is_primary_down {
                app.liquify_drag(pos);
                ctx.request_repaint();
            }
        }
        Tool::Smudge | Tool::Blur => {
            if app.viewport.is_primary_down {
                app.blend_drag(pos, 1.0);
                ctx.request_repaint();
            }
        }
        Tool::Eyedropper => {
            if app.viewport.is_primary_down && is_inside && response.hovered() {
                app.pick_color(pos);
            }
        }
        Tool::Transform(_) => {
            let keep_aspect = ctx.input(|i| i.modifiers.shift);
            transform::transform_drag(app, pos, keep_aspect);
            ctx.request_repaint();
        }
        Tool::Shape(_) | Tool::Gradient | Tool::Text => {}
    }
}

/// `pos` is unclamped: off-canvas points keep the stroke's real path.
fn handle_brush_move(app: &mut PainterApp, response: &egui::Response, pos: Vec2) {
    // Mouse and finger input paint at full pressure (the pen has its own path).
    app.viewport.touch.pen_tilt = None;
    app.viewport.touch.pen_barrel = Default::default();
    if app.brush_state.is_drawing {
        app.add_stroke_point(pos, 1.0);
    } else if app.viewport.is_primary_down && !app.viewport.is_panning && response.hovered() {
        app.start_stroke_with_pressure(pos, 1.0);
    }
}

fn handle_select_move(app: &mut PainterApp, ctx: &egui::Context, pos: Vec2) {
    // The magnetic lasso follows the pointer between clicks too.
    if app.selection_manager.is_dragging || app.workspace.select.magnetic.is_some() {
        app.select_move(pos);
        ctx.request_repaint();
    }
}

fn handle_mouse_wheel(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    unit: egui::MouseWheelUnit,
    delta: egui::Vec2,
) {
    if response.hovered() {
        let scroll = match unit {
            egui::MouseWheelUnit::Point => delta.y / 120.0,
            egui::MouseWheelUnit::Line => delta.y,
            egui::MouseWheelUnit::Page => delta.y * 10.0,
        };
        let factor = (1.0 - scroll * 0.1).clamp(0.5, 2.0);
        let anchor = ctx
            .input(|i| i.pointer.hover_pos())
            .unwrap_or(response.rect.center());
        app.zoom_about(anchor, response.rect.min, app.viewport.zoom * factor);
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use eframe::egui::{self, Color32};

    /// One frame: `events` go in, the canvas fills the window (unless
    /// `covered`, when a window lies over it), and input is handled.
    fn frame(
        app: &mut crate::PainterApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        covered: bool,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response =
                    ui.allocate_response(ui.available_size(), egui::Sense::click_and_drag());
                if covered {
                    egui::Window::new("dialog")
                        .fixed_pos(egui::pos2(100.0, 100.0))
                        .fixed_size(egui::vec2(400.0, 300.0))
                        .show(ctx, |ui| ui.label("OK"));
                }
                let rect = response.rect;
                super::handle_input(app, ctx, &response, rect.min, rect.center(), &[]);
            });
        });
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn app() -> crate::PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(400, 300, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app
    }

    #[test]
    fn a_release_the_canvas_never_saw_doesnt_leave_it_painting() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let at = egui::pos2(300.0, 300.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)], false);
        // The press reached the canvas; its release went to a frame the
        // canvas didn't get (a dialog opened by it, say).
        frame(&mut app, &ctx, vec![press(at, true)], false);
        app.finish_stroke();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![press(at, false)],
                ..Default::default()
            },
            |_| {},
        );
        // Later: the mouse moves over the canvas, button up.
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(egui::pos2(320.0, 310.0))],
            false,
        );
        assert!(!app.viewport.is_primary_down, "the press is over");
        assert!(!app.brush_state.is_drawing, "moving doesn't paint");
    }

    #[test]
    fn a_press_on_a_dialog_isnt_a_press_on_the_canvas() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let on_dialog = egui::pos2(200.0, 200.0);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(on_dialog)],
            true,
        );
        frame(&mut app, &ctx, vec![press(on_dialog, true)], true);
        assert!(!app.viewport.is_primary_down);
        assert!(!app.brush_state.is_drawing);
    }

    #[test]
    fn losing_focus_mid_drag_ends_it() {
        use crate::app::tools::Tool;
        use crate::selection::SelectionType;
        for tool in [Tool::Brush, Tool::Select(SelectionType::Rectangle)] {
            let (mut app, ctx) = (app(), egui::Context::default());
            app.active_tool = tool;
            let at = egui::pos2(300.0, 200.0);
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)], false);
            frame(&mut app, &ctx, vec![press(at, true)], false);
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(egui::pos2(340.0, 230.0))],
                false,
            );
            assert!(app.viewport.is_primary_down, "{tool:?}: dragging");
            app.viewport.is_panning = true;
            app.viewport.is_rotating = true;
            // Alt+Tab: the release never comes.
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::WindowFocused(false)],
                false,
            );
            assert!(!app.viewport.is_primary_down, "{tool:?}");
            assert!(!app.viewport.is_panning && !app.viewport.is_rotating);
            assert!(!app.brush_state.is_drawing, "{tool:?}: stroke ended");
            assert!(
                !app.selection_manager.is_dragging,
                "{tool:?}: selection ended"
            );
            app.release_canvas();
        }
    }
}
