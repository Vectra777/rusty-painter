use crate::PainterApp;
use crate::app::painter::Tool;
use crate::tablet::TabletPhase;
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
                app.start_stroke(canvas_pos);
            } else if sample.phase == TabletPhase::Move {
                if let Some(stroke) = &mut app.stroke {
                    let base_diam = app.brush.brush_options.diameter;
                    app.brush.brush_options.diameter = (base_diam * sample.pressure).max(1.0);
                    let prev = stroke.last_pos.unwrap_or(canvas_pos);
                    stroke.add_point(
                        &app.pool,
                        &app.canvas,
                        &mut app.brush,
                        canvas_pos,
                        app.current_undo_action.as_mut().unwrap(),
                        &mut app.modified_tiles,
                    );
                    app.mark_segment_dirty(prev, canvas_pos, app.brush.brush_options.diameter / 2.0);
                    app.brush.brush_options.diameter = base_diam;
                } else {
                    app.start_stroke(canvas_pos);
                }
            } else if sample.phase == TabletPhase::Up {
                app.finish_stroke();
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
                                match app.active_tool {
                                    Tool::Brush => app.start_stroke(canvas_pos.0),
                                    Tool::Select(t) => {
                                        app.selection_manager.start_selection(canvas_pos.0, t)
                                    }
                                }
                            }
                        } else if !pressed {
                            match app.active_tool {
                                Tool::Brush => app.finish_stroke(),
                                Tool::Select(_) => app.selection_manager.end_selection(),
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
                    match app.active_tool {
                        Tool::Brush => {
                            if app.is_drawing {
                                let (clamped, _is_inside) =
                                    app.screen_to_canvas(pos, origin, canvas_center);
                                if let Some(stroke) = &mut app.stroke {
                                    let prev = stroke.last_pos.unwrap_or(clamped);
                                    stroke.add_point(
                                        &app.pool,
                                        &app.canvas,
                                        &mut app.brush,
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
                                let (clamped, is_inside) =
                                    app.screen_to_canvas(pos, origin, canvas_center);
                                if is_inside {
                                    app.start_stroke(clamped);
                                }
                            }
                        }
                        Tool::Select(_) => {
                            if app.selection_manager.is_dragging {
                                let (clamped, _) =
                                    app.screen_to_canvas(pos, origin, canvas_center);
                                app.selection_manager.update_selection(clamped);
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
