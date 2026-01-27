use crate::PainterApp;
use crate::app::tools::Tool;
use crate::tablet::TabletPhase;
use crate::selection::transform::TransformState;
use crate::utils::vector::Vec2;
use crate::canvas::history::UndoAction;
use eframe::egui;

pub fn handle_input(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    origin: egui::Pos2,
    canvas_center: egui::Pos2,
) {
    handle_tablet(app, ctx, origin, canvas_center);
    handle_events(app, ctx, response, origin, canvas_center);
}

fn handle_tablet(app: &mut PainterApp, ctx: &egui::Context, origin: egui::Pos2, canvas_center: egui::Pos2) {
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
            if let Some(stroke) = &mut app.brush_state.stroke {
                let base = app.brush_state.brush.brush_options.diameter;
                app.brush_state.brush.brush_options.diameter = (base * pressure).max(1.0);
                let prev = stroke.last_pos.unwrap_or(pos);
                add_stroke_point(app, pos);
                app.mark_segment_dirty(prev, pos, app.brush_state.brush.brush_options.diameter / 2.0);
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
                info.offset = info.offset + delta;
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
                apply_simple_transform(app, offset);
            }
        }
    }
}

fn handle_events(app: &mut PainterApp, ctx: &egui::Context, response: &egui::Response, origin: egui::Pos2, canvas_center: egui::Pos2) {
    let events = ctx.input(|i| i.events.clone());

    for event in events {
        match event {
            egui::Event::PointerButton { pos, button, pressed, .. } => {
                handle_mouse_button(app, ctx, response, pos, button, pressed, origin, canvas_center);
            }
            egui::Event::Key { key, pressed, .. } => {
                handle_keyboard(app, key, pressed);
            }
            egui::Event::PointerMoved(pos) => {
                handle_pointer_move(app, ctx, response, pos, origin, canvas_center);
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
    origin: egui::Pos2,
    canvas_center: egui::Pos2,
) {
    let canvas_pos = app.screen_to_canvas(pos, origin, canvas_center);
    
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
        (i.key_down(egui::Key::Space), i.pointer.button_down(egui::PointerButton::Secondary))
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
        create_floating_layer(app);
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
            
            if has_transform(info) {
                let center = get_transform_center(info);
                let offset = info.offset;
                let rotation = info.rotation;
                let scale = info.scale;
                let captured = *info;
                
                if let Some(buffer) = &app.layer_state.floating_buffer {
                    if let Some(idx) = app.layer_state.floating_layer_idx {
                        let params = crate::canvas::canvas::TransformParams::new(offset, rotation, scale, center);
                        app.canvas.preview_transform(idx, buffer, params);
                        
                        // Mark both source and destination tiles dirty
                        if let Some(src_bounds) = info.bounds {
                            // Mark original position (to clear old pixels)
                            app.mark_tiles_in_bounds_dirty(src_bounds);
                            // Mark new position (to show transformed pixels)
                            let dst_bounds = calc_transformed_bounds(src_bounds, offset, rotation, scale, center);
                            app.mark_tiles_in_bounds_dirty(dst_bounds);
                        } else {
                            app.mark_all_tiles_dirty();
                        }
                    }
                } else {
                    reset_transform(info);
                    
                    let mut action = UndoAction {
                        tiles: Vec::new(),
                        selection: Some(app.selection_manager.current_shape.clone()),
                        transform: Some(captured),
                    };
                    
                    let has_selection = app.selection_manager.has_selection();
                    let selection = if has_selection { Some(&app.selection_manager) } else { None };
                    
                    let params = crate::canvas::canvas::TransformParams::new(offset, rotation, scale, center);
                    app.canvas.apply_transform(params, selection, Some(&mut action));
                    
                    if !action.tiles.is_empty() {
                        if let Some(history) = app.layer_state.histories.get_mut(app.canvas.active_layer_idx) {
                            history.push_action(action);
                        }
                    }
                    
                    // Mark both source and destination tiles dirty
                    if let Some(src_bounds) = captured.bounds {
                        // Mark original position (to clear old pixels)
                        app.mark_tiles_in_bounds_dirty(src_bounds);
                        // Mark new position (to show transformed pixels)
                        let dst_bounds = calc_transformed_bounds(src_bounds, offset, rotation, scale, center);
                        app.mark_tiles_in_bounds_dirty(dst_bounds);
                    } else {
                        app.mark_all_tiles_dirty();
                    }
                    app.selection_manager.apply_transform(offset, rotation, scale, center);
                }
            }
        }
    }
}

fn handle_keyboard(app: &mut PainterApp, key: egui::Key, pressed: bool) {
    if pressed && key == egui::Key::Enter {
        commit_floating_layer(app);
    }
}

fn handle_pointer_move(
    app: &mut PainterApp,
    ctx: &egui::Context,
    response: &egui::Response,
    pos: egui::Pos2,
    origin: egui::Pos2,
    canvas_center: egui::Pos2,
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
        let (clamped, is_inside) = app.screen_to_canvas(pos, origin, canvas_center);
        handle_tool_move(app, ctx, response, clamped, is_inside);
    }
}

fn handle_tool_move(app: &mut PainterApp, ctx: &egui::Context, response: &egui::Response, pos: Vec2, is_inside: bool) {
    match app.active_tool {
        Tool::Brush => handle_brush_move(app, response, pos, is_inside),
        Tool::Select(_) => handle_select_move(app, ctx, pos),
        Tool::Transform(ref mut info) => {
            if let Some(start) = info.start_pos {
                let delta = pos - start;
                
                match info.state {
                    TransformState::Moving => {
                        info.offset = info.offset + delta;
                    }
                    TransformState::Rotating => {
                        update_rotation(info, start, pos);
                    }
                    TransformState::Scaling(idx) => {
                        update_scaling(info, delta, idx);
                    }
                    _ => {}
                }
                
                info.start_pos = Some(pos);
            }
            
            // Now we can borrow app mutably for preview
            if let Tool::Transform(info) = app.active_tool {
                if has_transform(&info) {
                    apply_live_transform_preview(app, &info);
                }
            }
            
            ctx.request_repaint();
        }
    }
}

fn handle_brush_move(app: &mut PainterApp, response: &egui::Response, pos: Vec2, is_inside: bool) {
    if app.brush_state.is_drawing {
        if let Some(stroke) = &mut app.brush_state.stroke {
            let prev = stroke.last_pos.unwrap_or(pos);
            add_stroke_point(app, pos);
            app.mark_segment_dirty(prev, pos, app.brush_state.brush.brush_options.diameter / 2.0);
        }
    } else if app.viewport.is_primary_down && !app.viewport.is_panning && response.hovered() && is_inside {
        app.start_stroke(pos);
    }
}

fn handle_select_move(app: &mut PainterApp, ctx: &egui::Context, pos: Vec2) {
    if app.selection_manager.is_dragging {
        app.selection_manager.update_selection(pos);
        ctx.request_repaint();
    }
}

fn handle_mouse_wheel(app: &mut PainterApp, ctx: &egui::Context, response: &egui::Response, unit: egui::MouseWheelUnit, delta: egui::Vec2) {
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
        let selection = if has_selection { Some(&app.selection_manager) } else { None };
        stroke.add_point(
            &app.workspace.pool,
            &app.canvas,
            &mut app.brush_state.brush,
            selection,
            pos,
            app.layer_state.current_undo_action.as_mut().unwrap(),
            &mut app.render_cache.modified_tiles,
        );
    }
}

fn apply_simple_transform(app: &mut PainterApp, offset: Vec2) {
    let mut action = UndoAction {
        tiles: Vec::new(),
        selection: Some(app.selection_manager.current_shape.clone()),
        transform: None,
    };
    
    let has_selection = app.selection_manager.has_selection();
    let selection = if has_selection { Some(&app.selection_manager) } else { None };
    
    // Get content bounds before transform
    let src_bounds = app.canvas.get_content_bounds(app.canvas.active_layer_idx, selection);
    
    let params = crate::canvas::canvas::TransformParams::new(
        offset,
        0.0,
        Vec2::new(1.0, 1.0),
        Vec2::new(0.0, 0.0),
    );
    app.canvas.apply_transform(params, selection, Some(&mut action));
    
    if !action.tiles.is_empty() {
        if let Some(history) = app.layer_state.histories.get_mut(app.canvas.active_layer_idx) {
            history.push_action(action);
        }
    }
    
    // Mark both source and destination tiles dirty
    if let Some(bounds) = src_bounds {
        // Mark original position (to clear old pixels)
        app.mark_tiles_in_bounds_dirty(bounds);
        // Mark new position (to show transformed pixels)
        let dst_bounds = calc_transformed_bounds(bounds, offset, 0.0, Vec2::new(1.0, 1.0), Vec2::new(0.0, 0.0));
        app.mark_tiles_in_bounds_dirty(dst_bounds);
    } else {
        app.mark_all_tiles_dirty();
    }
    app.selection_manager.apply_transform(offset, 0.0, Vec2::new(1.0, 1.0), Vec2::new(0.0, 0.0));
}

fn create_floating_layer(app: &mut PainterApp) {
    if app.selection_manager.has_selection() && app.layer_state.floating_layer_idx.is_none() {
        // Get selection bounds before floating
        let sel_bounds = app.selection_manager.get_bounds();
        
        if let Some(idx) = app.canvas.float_selection(&app.selection_manager) {
            app.layer_state.floating_layer_idx = Some(idx);
            app.layer_state.floating_buffer = Some(app.canvas.capture_layer_pixels(idx));
            
            // Sync state
            app.layer_state.histories.push(crate::canvas::history::History::new());
            app.render_cache.layer_caches.push(std::collections::HashMap::new());
            app.render_cache.layer_cache_dirty.push(std::collections::HashSet::new());
            app.layer_state.layer_ui_colors.push(eframe::egui::Color32::from_gray(40));
            
            // Mark only selection bounds dirty
            if let Some(bounds) = sel_bounds {
                app.mark_tiles_in_bounds_dirty(bounds);
            } else {
                app.mark_all_tiles_dirty();
            }
        }
    }
}

fn commit_floating_layer(app: &mut PainterApp) {
    if let Some(idx) = app.layer_state.floating_layer_idx {
        // Get bounds of both layers before merge
        let float_bounds = app.canvas.get_content_bounds(idx, None);
        let base_bounds = if idx > 0 {
            app.canvas.get_content_bounds(idx - 1, None)
        } else {
            None
        };
        
        app.canvas.merge_layer_down(idx);
        app.layer_state.floating_layer_idx = None;
        app.layer_state.floating_buffer = None;
        app.selection_manager.clear_selection();
        
        // Reset transform tool
        if let Tool::Transform(ref mut info) = app.active_tool {
            *info = crate::selection::transform::TransformInfo::default();
        }
        
        // Sync state
        if idx < app.layer_state.histories.len() {
            app.layer_state.histories.remove(idx);
            app.render_cache.layer_caches.remove(idx);
            app.render_cache.layer_cache_dirty.remove(idx);
            app.layer_state.layer_ui_colors.remove(idx);
        }
        
        // Mark only affected tiles dirty (union of both layers)
        if let Some(b1) = float_bounds {
            if let Some(b2) = base_bounds {
                let affected = b1.union(b2);
                app.mark_tiles_in_bounds_dirty(affected);
            } else {
                app.mark_tiles_in_bounds_dirty(b1);
            }
        } else if let Some(b2) = base_bounds {
            app.mark_tiles_in_bounds_dirty(b2);
        } else {
            app.mark_all_tiles_dirty();
        }
    }
}

fn has_transform(info: &crate::selection::transform::TransformInfo) -> bool {
    info.offset.x != 0.0 || info.offset.y != 0.0 || info.rotation != 0.0 || info.scale.x != 1.0 || info.scale.y != 1.0
}

fn get_transform_center(info: &crate::selection::transform::TransformInfo) -> Vec2 {
    if let Some(b) = info.bounds {
        Vec2::new(b.center().x, b.center().y)
    } else {
        Vec2::new(0.0, 0.0)
    }
}

fn reset_transform(info: &mut crate::selection::transform::TransformInfo) {
    info.offset = Vec2::new(0.0, 0.0);
    info.rotation = 0.0;
    info.scale = Vec2::new(1.0, 1.0);
    info.bounds = None;
}

/// Calculate the bounding box after applying a transform to source bounds.
/// Expands to include all four transformed corners plus some padding.
fn calc_transformed_bounds(src_bounds: eframe::egui::Rect, offset: Vec2, rotation: f32, scale: Vec2, center: Vec2) -> eframe::egui::Rect {
    let corners = [
        src_bounds.min,
        eframe::egui::pos2(src_bounds.max.x, src_bounds.min.y),
        src_bounds.max,
        eframe::egui::pos2(src_bounds.min.x, src_bounds.max.y),
    ];
    
    let (sin_r, cos_r) = rotation.sin_cos();
    
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    
    for corner in corners {
        let dx = corner.x - center.x;
        let dy = corner.y - center.y;
        let sx = dx * scale.x;
        let sy = dy * scale.y;
        let rx = sx * cos_r - sy * sin_r;
        let ry = sx * sin_r + sy * cos_r;
        let tx = rx + center.x + offset.x;
        let ty = ry + center.y + offset.y;
        
        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx);
        max_y = max_y.max(ty);
    }
    
    // Add padding to ensure we cover affected area
    let padding = 2.0;
    eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(min_x - padding, min_y - padding),
        eframe::egui::pos2(max_x + padding, max_y + padding),
    )
}

fn update_rotation(info: &mut crate::selection::transform::TransformInfo, start: Vec2, current: Vec2) {
    if let Some(bounds) = info.bounds {
        let center = Vec2::new(bounds.center().x, bounds.center().y) + info.offset;
        let start_vec = start - center;
        let current_vec = current - center;
        let angle = current_vec.y.atan2(current_vec.x) - start_vec.y.atan2(start_vec.x);
        info.rotation += angle;
    }
}

fn update_scaling(info: &mut crate::selection::transform::TransformInfo, delta: Vec2, handle_idx: usize) {
    if let Some(bounds) = info.bounds {
        let (sin_r, cos_r) = info.rotation.sin_cos();
        let dx = delta.x * cos_r + delta.y * sin_r;
        let dy = -delta.x * sin_r + delta.y * cos_r;
        
        let scale_delta = match handle_idx {
            0 => Vec2::new(-dx, -dy),
            1 => Vec2::new(0.0, -dy),
            2 => Vec2::new(dx, -dy),
            3 => Vec2::new(dx, 0.0),
            4 => Vec2::new(dx, dy),
            5 => Vec2::new(0.0, dy),
            6 => Vec2::new(-dx, dy),
            7 => Vec2::new(-dx, 0.0),
            _ => Vec2::new(0.0, 0.0),
        };
        
        let w = bounds.width();
        let h = bounds.height();
        if w > 0.0 {
            info.scale.x += scale_delta.x / (w * 0.5);
        }
        if h > 0.0 {
            info.scale.y += scale_delta.y / (h * 0.5);
        }
        info.scale.x = info.scale.x.max(0.1);
        info.scale.y = info.scale.y.max(0.1);
    }
}

fn apply_live_transform_preview(app: &mut PainterApp, info: &crate::selection::transform::TransformInfo) {
    if let Some(buffer) = &app.layer_state.floating_buffer {
        if let Some(idx) = app.layer_state.floating_layer_idx {
            let center = get_transform_center(info);
            let params = crate::canvas::canvas::TransformParams::new(
                info.offset,
                info.rotation,
                info.scale,
                center,
            );
            
            // Apply preview transform
            app.canvas.preview_transform(idx, buffer, params);
            
            // Mark affected tiles dirty
            if let Some(src_bounds) = info.bounds {
                // Mark original position
                app.mark_tiles_in_bounds_dirty(src_bounds);
                // Mark new transformed position
                let dst_bounds = calc_transformed_bounds(
                    src_bounds,
                    info.offset,
                    info.rotation,
                    info.scale,
                    center,
                );
                app.mark_tiles_in_bounds_dirty(dst_bounds);
            } else {
                app.mark_all_tiles_dirty();
            }
        }
    }
}
