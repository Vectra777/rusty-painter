use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::transform;
use crate::selection::SelectionMode;
use crate::tablet::TabletPhase;
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
) {
    handle_tablet(app, ctx, response, origin, canvas_center);
    let placement = CanvasPlacement {
        origin,
        center: canvas_center,
    };
    handle_events(app, ctx, response, placement);
}

fn handle_tablet(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    origin: egui::Pos2,
    canvas_center: egui::Pos2,
) {
    if let Some(tablet) = &mut app.tablet {
        let scale = ctx.input(|i| i.pixels_per_point());
        for sample in tablet.poll(scale) {
            let pos = egui::Pos2::new(sample.pos[0], sample.pos[1]);
            let (clamped, inside) = app.screen_to_canvas(pos, origin, canvas_center);
            // The brush works off the canvas too (dabs are clipped to it): it
            // keeps drawing when the pen leaves, and a stroke may start
            // anywhere on the canvas panel. Other tools stay on the canvas.
            let brush = matches!(app.active_tool, Tool::Brush);
            let on_panel = response.rect.contains(pos);
            let allowed = inside
                || (brush
                    && (app.brush_state.is_drawing
                        || (on_panel && sample.phase == TabletPhase::Down)));
            if !allowed {
                continue;
            }
            let canvas_pos = if brush {
                app.screen_to_canvas_raw(pos, origin, canvas_center)
            } else {
                clamped
            };
            match sample.phase {
                TabletPhase::Down => {
                    if matches!(app.active_tool, Tool::Brush) {
                        app.sync_pen_eraser(sample.is_eraser);
                    }
                    handle_tablet_down(app, canvas_pos)
                }
                TabletPhase::Move => {
                    let pressure = app.map_pressure(sample.pressure);
                    handle_tablet_move(app, canvas_pos, pressure)
                }
                TabletPhase::Up => handle_tablet_up(app),
            }
        }
    }
}

fn handle_tablet_down(app: &mut PainterApp, pos: Vec2) {
    match app.active_tool {
        Tool::Brush => app.start_stroke(pos),
        Tool::Select(t) => app.selection_manager.start_selection(pos, t),
        Tool::Transform(_) => transform::transform_press(app, pos),
        Tool::Eyedropper => app.pick_color(pos),
        Tool::Fill => app.fill_press(pos),
        Tool::Liquify => app.liquify_press(pos),
        Tool::Smudge | Tool::Blur => app.blend_press(pos, 1.0),
    }
}

fn handle_tablet_move(app: &mut PainterApp, pos: Vec2, pressure: f32) {
    match app.active_tool {
        Tool::Brush => {
            if app.brush_state.is_drawing {
                app.add_stroke_point(pos, pressure);
            } else {
                app.start_stroke_with_pressure(pos, pressure);
            }
        }
        Tool::Select(_) => {
            app.selection_manager.update_selection(pos);
        }
        Tool::Transform(_) => transform::transform_drag(app, pos, false),
        Tool::Fill => app.fill_drag(pos),
        Tool::Liquify => app.liquify_drag(pos),
        Tool::Smudge | Tool::Blur => app.blend_drag(pos, pressure),
        Tool::Eyedropper => {
            if pressure > 0.0 {
                app.pick_color(pos);
            }
        }
    }
}

fn handle_tablet_up(app: &mut PainterApp) {
    match app.active_tool {
        Tool::Brush => app.finish_stroke(),
        Tool::Select(_) => app.selection_manager.end_selection(),
        Tool::Transform(_) => transform::transform_release(app),
        Tool::Eyedropper => {}
        Tool::Fill => app.fill_release(),
        Tool::Liquify => app.liquify_release(),
        Tool::Smudge | Tool::Blur => app.blend_release(),
    }
}

fn handle_events(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    placement: CanvasPlacement,
) {
    let events = ctx.input(|i| i.events.clone());
    // Fingers that belong to a gesture (or that may not paint) also arrive
    // as pointer events; leave those to the touch handler.
    let suppress = app.viewport.touch.suppress_pointer;

    for event in events {
        match event {
            egui::Event::PointerButton {
                button: egui::PointerButton::Primary,
                ..
            }
            | egui::Event::PointerMoved(_)
                if suppress => {}
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
        handle_primary_press(app, response, canvas_pos, raw);
    } else {
        handle_primary_release(app);
    }
}

fn handle_primary_press(
    app: &mut PainterApp,
    response: &egui::Response,
    canvas_pos: (Vec2, bool),
    raw: Vec2,
) {
    // The brush may start a stroke off the canvas (on the canvas panel);
    // other tools need a press on the canvas itself.
    let brush = matches!(app.active_tool, Tool::Brush);
    if app.viewport.is_panning || !response.hovered() || !(canvas_pos.1 || brush) {
        return;
    }

    // Alt+click samples a color with any painting tool.
    let alt_held = response.ctx.input(|i| i.modifiers.alt);
    if alt_held && brush {
        if canvas_pos.1 {
            app.pick_color(canvas_pos.0);
        }
        return;
    }

    // Flipping a pen to its eraser end switches to the eraser.
    if matches!(app.active_tool, Tool::Brush)
        && let Some(pen) = crate::tablet::pen_state()
        && pen.is_stylus
        && !app.viewport.touch.finger_down()
    {
        app.sync_pen_eraser(pen.is_eraser);
    }

    match app.active_tool {
        Tool::Brush => {
            let pressure = app.pointer_pressure();
            app.start_stroke_with_pressure(raw, pressure);
        }
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
            app.selection_manager
                .start_selection_with_mode(canvas_pos.0, t, mode);
        }
        Tool::Transform(_) => transform::transform_press(app, canvas_pos.0),
        Tool::Eyedropper => app.pick_color(canvas_pos.0),
        Tool::Fill => app.fill_press(canvas_pos.0),
        Tool::Liquify => app.liquify_press(canvas_pos.0),
        Tool::Smudge | Tool::Blur => {
            let pressure = app.pointer_pressure();
            app.blend_press(raw, pressure);
        }
    }
}

fn handle_primary_release(app: &mut PainterApp) {
    if matches!(app.active_tool, Tool::Transform(_)) {
        app.release_canvas();
    }
    match app.active_tool {
        Tool::Brush => app.finish_stroke(),
        Tool::Select(_) => app.selection_manager.end_selection(),
        Tool::Eyedropper => {}
        Tool::Fill => app.fill_release(),
        Tool::Liquify => app.liquify_release(),
        Tool::Smudge | Tool::Blur => app.blend_release(),
        Tool::Transform(_) => transform::transform_release(app),
    }
}

fn handle_keyboard(app: &mut PainterApp, key: egui::Key, pressed: bool) {
    if pressed && key == egui::Key::Enter {
        transform::commit_floating_layer(app);
        app.liquify_commit();
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
    } else {
        let (clamped, is_inside) = app.screen_to_canvas(pos, placement.origin, placement.center);
        if matches!(app.active_tool, Tool::Brush) {
            let raw = app.screen_to_canvas_raw(pos, placement.origin, placement.center);
            handle_brush_move(app, response, raw);
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
                let pressure = app.pointer_pressure();
                app.blend_drag(pos, pressure);
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
    }
}

/// `pos` is unclamped: off-canvas points keep the stroke's real path.
fn handle_brush_move(app: &mut PainterApp, response: &egui::Response, pos: Vec2) {
    // Pen pressure where the platform provides it out of band (Android
    // stylus); mouse and finger input paint at full size.
    let pressure = app.pointer_pressure();
    if app.brush_state.is_drawing {
        app.add_stroke_point(pos, pressure);
    } else if app.viewport.is_primary_down && !app.viewport.is_panning && response.hovered() {
        app.start_stroke_with_pressure(pos, pressure);
    }
}

fn handle_select_move(app: &mut PainterApp, ctx: &egui::Context, pos: Vec2) {
    if app.selection_manager.is_dragging {
        app.selection_manager.update_selection(pos);
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
