use crate::PainterApp;
use crate::app::tools::Tool;
use crate::tablet::TabletPhase;
use crate::selection::transform::TransformState;
use eframe::egui;

pub fn handle_input(
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
            let (canvas_pos, inside) = app.screen_to_canvas(pos, origin, canvas_center);
            if !inside {
                continue;
            }
            if sample.phase == TabletPhase::Down {
                match app.active_tool {
                    Tool::Brush => app.start_stroke(canvas_pos),
                    Tool::Select(t) => app.selection_manager.start_selection(canvas_pos, t),
                    Tool::Transform(ref mut info) => {
                        info.start_pos = Some(canvas_pos);
                    }
                }
            } else if sample.phase == TabletPhase::Move {
                match app.active_tool {
                    Tool::Brush => {
                        if let Some(stroke) = &mut app.stroke {
                            let base_diam = app.brush.brush_options.diameter;
                            app.brush.brush_options.diameter = (base_diam * sample.pressure).max(1.0);
                            let prev = stroke.last_pos.unwrap_or(canvas_pos);
                            stroke.add_point(
                                &app.pool,
                                &app.canvas,
                                &mut app.brush,
                                if app.selection_manager.has_selection() { Some(&app.selection_manager) } else { None },
                                canvas_pos,
                                app.current_undo_action.as_mut().unwrap(),
                                &mut app.modified_tiles,
                            );
                            app.mark_segment_dirty(prev, canvas_pos, app.brush.brush_options.diameter / 2.0);
                            app.brush.brush_options.diameter = base_diam;
                        } else {
                            app.start_stroke(canvas_pos);
                        }
                    }
                    Tool::Select(_) => {
                        app.selection_manager.update_selection(canvas_pos);
                    }
                    Tool::Transform(ref mut info) => {
                        if let Some(start) = info.start_pos {
                             let delta = canvas_pos - start;
                             info.offset = info.offset + delta;
                             info.start_pos = Some(canvas_pos);
                        }
                    }
                }
            } else if sample.phase == TabletPhase::Up {
                let mut transform_to_apply = None;
                match app.active_tool {
                    Tool::Brush => app.finish_stroke(),
                    Tool::Select(_) => app.selection_manager.end_selection(),
                    Tool::Transform(ref mut info) => {
                        info.start_pos = None;
                        if info.offset.x != 0.0 || info.offset.y != 0.0 {
                            transform_to_apply = Some(info.offset);
                            info.offset = crate::utils::vector::Vec2::new(0.0, 0.0);
                        }
                    }
                }
                if let Some(offset) = transform_to_apply {
                     let mut action = crate::canvas::history::UndoAction { 
                         tiles: Vec::new(),
                         selection: Some(app.selection_manager.current_shape.clone()),
                         transform: None,
                     };
                     app.canvas.apply_transform(offset, 0.0, crate::utils::vector::Vec2::new(1.0, 1.0), crate::utils::vector::Vec2::new(0.0, 0.0), if app.selection_manager.has_selection() { Some(&app.selection_manager) } else { None }, Some(&mut action));
                     if !action.tiles.is_empty() {
                         if let Some(history) = app.histories.get_mut(app.canvas.active_layer_idx) {
                             history.push_action(action);
                         }
                     }
                     app.mark_all_tiles_dirty();
                     app.selection_manager.apply_transform(offset, 0.0, crate::utils::vector::Vec2::new(1.0, 1.0), crate::utils::vector::Vec2::new(0.0, 0.0));
                }
            }
        }
    }

    let events = ctx.input(|i| i.events.clone());

    for event in events {
        match event {
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                ..
            } => {
                let canvas_pos = app.screen_to_canvas(pos, origin, canvas_center);
                match button {
                    egui::PointerButton::Primary => {
                        app.is_primary_down = pressed;
                        let (space_down, secondary_down) = ctx.input(|i| {
                            (
                                i.key_down(egui::Key::Space),
                                i.pointer.button_down(egui::PointerButton::Secondary),
                            )
                        });
                        if pressed && space_down {
                            app.is_panning = true;
                        }
                        if !pressed && !secondary_down {
                            app.is_panning = false;
                        }

                        if pressed && !app.is_panning && response.hovered() {
                            if canvas_pos.1 {
                                if let Tool::Transform(_) = app.active_tool {
                                    if app.selection_manager.has_selection() && app.floating_layer_idx.is_none() {
                                        if let Some(idx) = app.canvas.float_selection(&app.selection_manager) {
                                            app.floating_layer_idx = Some(idx);
                                            
                                            // Capture original pixels
                                            app.floating_buffer = Some(app.canvas.capture_layer_pixels(idx));

                                            // Sync app state with new layer
                                            app.histories.push(crate::canvas::history::History::new());
                                            app.layer_caches.push(std::collections::HashMap::new());
                                            app.layer_cache_dirty.push(std::collections::HashSet::new());
                                            app.layer_ui_colors.push(eframe::egui::Color32::from_gray(40));

                                            app.mark_all_tiles_dirty();
                                        }
                                    }
                                }

                                match app.active_tool {
                                    Tool::Brush => app.start_stroke(canvas_pos.0),
                                    Tool::Select(t) => {
                                        app.selection_manager.start_selection(canvas_pos.0, t)
                                    }
                                    Tool::Transform(ref mut info) => {
                                        info.start_pos = Some(canvas_pos.0);
                                        info.state = info.hit_test(canvas_pos.0, app.zoom);
                                    }
                                }
                            }
                        } else if !pressed {
                            let mut transform_to_apply = None;
                            match app.active_tool {
                                Tool::Brush => app.finish_stroke(),
                                Tool::Select(_) => app.selection_manager.end_selection(),
                                Tool::Transform(ref mut info) => {
                                    info.start_pos = None;
                                    info.state = TransformState::None;
                                    if info.offset.x != 0.0 || info.offset.y != 0.0 || info.rotation != 0.0 || info.scale.x != 1.0 || info.scale.y != 1.0 {
                                        let center = if let Some(b) = info.bounds { crate::utils::vector::Vec2::new(b.center().x, b.center().y) } else { crate::utils::vector::Vec2::new(0.0, 0.0) };
                                        
                                        // If we have a floating buffer, we use preview_transform instead of apply_transform
                                        // And we DO NOT reset the info
                                        if let Some(buffer) = &app.floating_buffer {
                                            if let Some(idx) = app.floating_layer_idx {
                                                app.canvas.preview_transform(idx, buffer, info.offset, info.rotation, info.scale, center);
                                                app.mark_all_tiles_dirty();
                                                // Do not reset info
                                            }
                                        } else {
                                            // Fallback for non-floating transforms (if any)
                                            let captured_info = *info;
                                            transform_to_apply = Some((info.offset, info.rotation, info.scale, center, captured_info));
                                            
                                            info.offset = crate::utils::vector::Vec2::new(0.0, 0.0);
                                            info.rotation = 0.0;
                                            info.scale = crate::utils::vector::Vec2::new(1.0, 1.0);
                                            info.bounds = None;
                                        }
                                    }
                                }
                            }
                            if let Some((offset, rotation, scale, center, captured_info)) = transform_to_apply {
                                 let mut action = crate::canvas::history::UndoAction { 
                                     tiles: Vec::new(),
                                     selection: Some(app.selection_manager.current_shape.clone()),
                                     transform: Some(captured_info),
                                 };
                                 app.canvas.apply_transform(offset, rotation, scale, center, if app.selection_manager.has_selection() { Some(&app.selection_manager) } else { None }, Some(&mut action));
                                 if !action.tiles.is_empty() {
                                     if let Some(history) = app.histories.get_mut(app.canvas.active_layer_idx) {
                                         history.push_action(action);
                                     }
                                 }
                                 app.mark_all_tiles_dirty();
                                 app.selection_manager.apply_transform(offset, rotation, scale, center);
                            }
                        }
                    }
                    egui::PointerButton::Secondary => {
                        if pressed && response.hovered() {
                            app.is_panning = true;
                        }
                        if !pressed {
                            app.is_panning = false;
                        }
                    }
                    egui::PointerButton::Middle => {
                        if pressed && response.hovered() {
                            app.is_rotating = true;
                        }
                        if !pressed {
                            app.is_rotating = false;
                        }
                    }
                    _ => {}
                }
            }

            egui::Event::Key { key, pressed, .. } => {
                if pressed && key == egui::Key::Enter {
                     if let Some(idx) = app.floating_layer_idx {
                         // Apply final transform if needed (though preview should have done it)
                         // Actually, we need to record the undo action here!
                         // Since we didn't record it during drag/release.
                         
                         let mut action = crate::canvas::history::UndoAction { 
                             tiles: Vec::new(),
                             selection: Some(app.selection_manager.current_shape.clone()),
                             transform: None, // We are committing, so transform is reset
                         };
                         
                         // We need to capture the state BEFORE merge for undo?
                         // Actually, merging destroys the layer.
                         // If we undo the merge, we want the floating layer back?
                         // That's complex.
                         // For now, let's just merge.
                         
                         app.canvas.merge_layer_down(idx);
                         app.floating_layer_idx = None;
                         app.floating_buffer = None; // Clear buffer
                         app.selection_manager.clear_selection();
                         
                         // Reset transform tool
                         if let Tool::Transform(ref mut info) = app.active_tool {
                             *info = crate::selection::transform::TransformInfo::default();
                         }
                         
                         // Sync app state with removed layer
                         if idx < app.histories.len() {
                             app.histories.remove(idx);
                             app.layer_caches.remove(idx);
                             app.layer_cache_dirty.remove(idx);
                             app.layer_ui_colors.remove(idx);
                         }

                         app.mark_all_tiles_dirty();
                     }
                }
            }

            egui::Event::PointerMoved(pos) => {
                let delta = ctx.input(|i| i.pointer.delta());
                if app.is_rotating {
                    app.rotation += delta.x * -0.005;
                    ctx.request_repaint();
                } else if app.is_panning {
                    app.offset.x += delta.x;
                    app.offset.y += delta.y;
                    ctx.request_repaint();
                } else {
                    let (clamped, is_inside) = app.screen_to_canvas(pos, origin, canvas_center);
                    match app.active_tool {
                        Tool::Brush => {
                            if app.is_drawing {
                                if let Some(stroke) = &mut app.stroke {
                                    let prev = stroke.last_pos.unwrap_or(clamped);
                                    stroke.add_point(
                                        &app.pool,
                                        &app.canvas,
                                        &mut app.brush,
                                        if app.selection_manager.has_selection() { Some(&app.selection_manager) } else { None },
                                        clamped,
                                        app.current_undo_action.as_mut().unwrap(),
                                        &mut app.modified_tiles,
                                    );
                                    app.mark_segment_dirty(
                                        prev,
                                        clamped,
                                        app.brush.brush_options.diameter / 2.0,
                                    );
                                }
                            } else if app.is_primary_down
                                && !app.is_panning
                                && response.hovered()
                            {
                                if is_inside {
                                    app.start_stroke(clamped);
                                }
                            }
                        }
                        Tool::Select(_) => {
                            if app.selection_manager.is_dragging {
                                app.selection_manager.update_selection(clamped);
                                ctx.request_repaint();
                            }
                        }

                        Tool::Transform(ref mut info) => {
                            if let Some(start) = info.start_pos {
                                let current = clamped;
                                let delta = current - start;
                                
                                match info.state {
                                    TransformState::Moving => {
                                        info.offset = info.offset + delta;
                                    }
                                    TransformState::Rotating => {
                                        if let Some(bounds) = info.bounds {
                                            let center = crate::utils::vector::Vec2::new(bounds.center().x, bounds.center().y) + info.offset;
                                            let start_vec = start - center;
                                            let current_vec = current - center;
                                            let angle = current_vec.y.atan2(current_vec.x) - start_vec.y.atan2(start_vec.x);
                                            info.rotation += angle;
                                        }
                                    }
                                    TransformState::Scaling(idx) => {
                                        if let Some(bounds) = info.bounds {
                                            let (sin_r, cos_r) = info.rotation.sin_cos();
                                            let dx = delta.x * cos_r + delta.y * sin_r;
                                            let dy = -delta.x * sin_r + delta.y * cos_r;
                                            
                                            let mut scale_delta = crate::utils::vector::Vec2::new(0.0, 0.0);
                                            match idx {
                                                0 => { scale_delta.x = -dx; scale_delta.y = -dy; }
                                                1 => { scale_delta.y = -dy; }
                                                2 => { scale_delta.x = dx; scale_delta.y = -dy; }
                                                3 => { scale_delta.x = dx; }
                                                4 => { scale_delta.x = dx; scale_delta.y = dy; }
                                                5 => { scale_delta.y = dy; }
                                                6 => { scale_delta.x = -dx; scale_delta.y = dy; }
                                                7 => { scale_delta.x = -dx; }
                                                _ => {}
                                            }
                                            
                                            let w = bounds.width();
                                            let h = bounds.height();
                                            if w > 0.0 { info.scale.x += scale_delta.x / (w * 0.5); }
                                            if h > 0.0 { info.scale.y += scale_delta.y / (h * 0.5); }
                                        }
                                    }
                                    _ => {}
                                }
                                info.start_pos = Some(current);
                                ctx.request_repaint();
                            }
                        }
                    }
                }
            }

            egui::Event::MouseWheel { unit, delta, .. } => {
                if response.hovered() {
                    let scroll = match unit {
                        egui::MouseWheelUnit::Point => delta.y / 120.0_f32,
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Page => delta.y * 10.0_f32,
                    };
                    let zoom_factor = (1.0 - scroll * 0.1_f32).clamp(0.5_f32, 2.0_f32);
                    app.zoom = (app.zoom * zoom_factor).clamp(0.1, 20.0);
                    ctx.request_repaint();
                }
            }

            _ => {}
        }
    }
}
