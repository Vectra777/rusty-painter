use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::transform;
use crate::brush_engine::stroke::StrokeContext;
use crate::canvas::history::UndoAction;
use crate::selection::transform::TransformState;
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
    handle_tablet(app, ctx, origin, canvas_center);
    let placement = CanvasPlacement {
        origin,
        center: canvas_center,
    };
    handle_events(app, ctx, response, placement);
}

fn handle_tablet(
    app: &mut PainterApp,
    ctx: &egui::Context,
    origin: egui::Pos2,
    canvas_center: egui::Pos2,
) {
    if let Some(tablet) = &mut app.tablet {
        let scale = ctx.input(|i| i.pixels_per_point());
        for sample in tablet.poll(scale) {
            let pos = egui::Pos2::new(sample.pos[0], sample.pos[1]);
            let (canvas_pos, inside) = app.screen_to_canvas(pos, origin, canvas_center);
            if !inside {
                continue;
            }
            match sample.phase {
                TabletPhase::Down => handle_tablet_down(app, canvas_pos),
                TabletPhase::Move => handle_tablet_move(app, canvas_pos, sample.pressure),
                TabletPhase::Up => handle_tablet_up(app),
            }
        }
    }
}

fn handle_tablet_down(app: &mut PainterApp, pos: Vec2) {
    match app.active_tool {
        Tool::Brush => app.start_stroke(pos),
        Tool::Select(t) => app.selection_manager.start_selection(pos, t),
        Tool::Transform(ref mut info) => {
            info.start_pos = Some(pos);
        }
    }
}

fn handle_tablet_move(app: &mut PainterApp, pos: Vec2, pressure: f32) {
    match app.active_tool {
        Tool::Brush => {
            if app.brush_state.stroke.is_some() {
                let base = app.brush_state.brush.brush_options.diameter;
                app.brush_state.brush.brush_options.diameter = (base * pressure).max(1.0);
                add_stroke_point(app, pos);
                app.mark_modified_tiles_dirty();
                app.brush_state.brush.brush_options.diameter = base;
            } else {
                app.start_stroke(pos);
            }
        }
        Tool::Select(_) => {
            app.selection_manager.update_selection(pos);
        }
        Tool::Transform(ref mut info) => {
            if let Some(start) = info.start_pos {
                let delta = pos - start;
                info.offset += delta;
                info.start_pos = Some(pos);
            }
        }
    }
}

fn handle_tablet_up(app: &mut PainterApp) {
    match app.active_tool {
        Tool::Brush => app.finish_stroke(),
        Tool::Select(_) => app.selection_manager.end_selection(),
        Tool::Transform(ref mut info) => {
            info.start_pos = None;
            if info.offset.x != 0.0 || info.offset.y != 0.0 {
                let offset = info.offset;
                info.offset = Vec2::new(0.0, 0.0);
                transform::apply_simple_transform(app, offset);
            }
        }
    }
}

fn handle_events(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    placement: CanvasPlacement,
) {
    let events = ctx.input(|i| i.events.clone());

    for event in events {
        match event {
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

    match button {
        egui::PointerButton::Primary => {
            handle_primary_button(app, ctx, response, canvas_pos, pressed);
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
        handle_primary_press(app, response, canvas_pos);
    } else {
        handle_primary_release(app);
    }
}

fn handle_primary_press(app: &mut PainterApp, response: &egui::Response, canvas_pos: (Vec2, bool)) {
    if app.viewport.is_panning || !response.hovered() || !canvas_pos.1 {
        return;
    }

    // Create floating layer for transform tool
    if let Tool::Transform(_) = app.active_tool {
        transform::create_floating_layer(app);
    }

    match app.active_tool {
        Tool::Brush => app.start_stroke(canvas_pos.0),
        Tool::Select(t) => app.selection_manager.start_selection(canvas_pos.0, t),
        Tool::Transform(ref mut info) => {
            info.start_pos = Some(canvas_pos.0);
            info.state = info.hit_test(canvas_pos.0, app.viewport.zoom);
        }
    }
}

fn handle_primary_release(app: &mut PainterApp) {
    match app.active_tool {
        Tool::Brush => app.finish_stroke(),
        Tool::Select(_) => app.selection_manager.end_selection(),
        Tool::Transform(ref mut info) => {
            info.start_pos = None;
            info.state = TransformState::None;

            if transform::has_transform(info) {
                let center = transform::get_transform_center(info);
                let offset = info.offset;
                let rotation = info.rotation;
                let scale = info.scale;
                let captured = *info;

                if let Some(buffer) = &app.layer_state.floating_buffer {
                    if let Some(idx) = app.layer_state.floating_layer_idx {
                        let params = crate::canvas::storage::TransformParams::new(
                            offset, rotation, scale, center,
                        );
                        app.canvas.preview_transform(idx, buffer, params);
                        transform::mark_transform_dirty(app, captured.bounds, &params);
                    }
                } else {
                    transform::reset_transform(info);

                    let mut action = UndoAction {
                        tiles: Vec::new(),
                        selection: Some(app.selection_manager.current_shape.clone()),
                        transform: Some(captured),
                    };

                    let has_selection = app.selection_manager.has_selection();
                    let selection = if has_selection {
                        Some(&app.selection_manager)
                    } else {
                        None
                    };

                    let params = crate::canvas::storage::TransformParams::new(
                        offset, rotation, scale, center,
                    );
                    app.canvas
                        .apply_transform(params, selection, Some(&mut action));

                    transform::push_history_if_changed(app, action);
                    transform::mark_transform_dirty(app, captured.bounds, &params);
                    app.selection_manager
                        .apply_transform(offset, rotation, scale, center);
                }
            }
        }
    }
}

fn handle_keyboard(app: &mut PainterApp, key: egui::Key, pressed: bool) {
    if pressed && key == egui::Key::Enter {
        transform::commit_floating_layer(app);
    }
}

fn handle_pointer_move(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    pos: egui::Pos2,
    placement: CanvasPlacement,
) {
    let delta = ctx.input(|i| i.pointer.delta());

    if app.viewport.is_rotating {
        app.viewport.rotation += delta.x * -0.005;
        ctx.request_repaint();
    } else if app.viewport.is_panning {
        app.viewport.offset.x += delta.x;
        app.viewport.offset.y += delta.y;
        ctx.request_repaint();
    } else {
        let (clamped, is_inside) = app.screen_to_canvas(pos, placement.origin, placement.center);
        handle_tool_move(app, ctx, response, clamped, is_inside);
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
        Tool::Brush => handle_brush_move(app, response, pos, is_inside),
        Tool::Select(_) => handle_select_move(app, ctx, pos),
        Tool::Transform(ref mut info) => {
            if let Some(start) = info.start_pos {
                let delta = pos - start;

                match info.state {
                    TransformState::Moving => {
                        info.offset += delta;
                    }
                    TransformState::Rotating => {
                        transform::update_rotation(info, start, pos);
                    }
                    TransformState::Scaling(idx) => {
                        transform::update_scaling(info, delta, idx);
                    }
                    _ => {}
                }

                info.start_pos = Some(pos);
            }

            // Now we can borrow app mutably for preview
            if let Tool::Transform(info) = app.active_tool
                && transform::has_transform(&info)
            {
                transform::apply_live_transform_preview(app, &info);
            }

            ctx.request_repaint();
        }
    }
}

fn handle_brush_move(app: &mut PainterApp, response: &egui::Response, pos: Vec2, is_inside: bool) {
    if app.brush_state.is_drawing {
        if app.brush_state.stroke.is_some() {
            add_stroke_point(app, pos);
            app.mark_modified_tiles_dirty();
        }
    } else if app.viewport.is_primary_down
        && !app.viewport.is_panning
        && response.hovered()
        && is_inside
    {
        app.start_stroke(pos);
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
        app.viewport.zoom = (app.viewport.zoom * factor).clamp(0.1, 20.0);
        ctx.request_repaint();
    }
}

// Helper functions

fn add_stroke_point(app: &mut PainterApp, pos: Vec2) {
    if let Some(stroke) = &mut app.brush_state.stroke {
        let has_selection = app.selection_manager.has_selection();
        let selection = if has_selection {
            Some(&app.selection_manager)
        } else {
            None
        };
        let mut context = StrokeContext::new(
            &app.workspace.pool,
            &app.canvas,
            selection,
            app.layer_state.current_undo_action.as_mut().unwrap(),
            &mut app.render_cache.modified_tiles,
        );
        stroke.add_point(&mut app.brush_state.brush, pos, &mut context);
    }
}
