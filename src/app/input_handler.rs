use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::transform;
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
    handle_events(app, ctx, response, placement, !pen.is_empty());
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
    if app.guides_drag(raw, snap) {
        return;
    }
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
    }
}

/// Shift keeps a shape's proportions, Alt draws it from the centre.
fn shape_mods(ctx: &egui::Context) -> crate::app::shape_tool::ShapeMods {
    let m = ctx.input(|i| i.modifiers);
    crate::app::shape_tool::ShapeMods {
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
    // pen produces are ignored. On Android the pen is the mouse pointer; on
    // Windows it arrives as touches, which egui turns into pointer events
    // right after each touch event.
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
        let pen_pointer = pen_stroke || (pen_active && (cfg!(target_os = "android") || from_touch));
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
            _ => {}
        }
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
    app.viewport.is_primary_down = pressed;

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
        Tool::Brush | Tool::Shape(_) | Tool::Gradient
    );
    if app.viewport.is_panning || !over {
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
        Tool::Smudge | Tool::Blur => app.blend_press(raw, pressure),
        Tool::Shape(kind) => app.shape_press(kind, raw),
        Tool::Gradient => app.gradient_press(raw),
    }
}

fn handle_primary_release(app: &mut PainterApp) {
    if app.guides_release() {
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
    } else if app.guides_dragging() {
        let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
        app.guides_drag(raw, ctx.input(|i| i.modifiers.shift));
        ctx.request_repaint();
    } else {
        let (clamped, is_inside) = app.screen_to_canvas(pos, placement.origin, placement.center);
        if matches!(app.active_tool, Tool::Brush) {
            let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
            handle_brush_move(app, response, raw);
        } else if matches!(app.active_tool, Tool::Shape(_)) {
            // Shapes may reach off the canvas, like strokes.
            let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
            app.shape_move(raw, shape_mods(ctx));
            ctx.request_repaint();
        } else if matches!(app.active_tool, Tool::Gradient) {
            if app.viewport.is_primary_down {
                let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
                app.gradient_drag(raw, ctx.input(|i| i.modifiers.shift));
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
        Tool::Shape(_) | Tool::Gradient => {}
    }
}

/// `pos` is unclamped: off-canvas points keep the stroke's real path.
fn handle_brush_move(app: &mut PainterApp, response: &egui::Response, pos: Vec2) {
    // Mouse and finger input paint at full pressure (the pen has its own path).
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
