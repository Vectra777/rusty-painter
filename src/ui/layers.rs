use crate::PainterApp;
use eframe::egui;

/// Sidebar that manages the canvas layer stack.
pub fn layers_panel(ctx: &egui::Context, ui: &mut egui::Ui, app: &mut PainterApp) {
    let mut add_layer = false;
    let mut to_delete = None;
    let mut active_idx = app.canvas.active_layer_idx;
    let mut needs_refresh = false;
    let mut item_rects: Vec<(usize, egui::Rect)> = Vec::new();

    egui::ScrollArea::vertical()
        .id_salt("layers_scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("New Layer").clicked() {
                    add_layer = true;
                }
            });
            ui.separator();

            // Iterate in reverse so top layers are at the top of the list
            for i in (0..app.canvas.layers.len()).rev() {
                let mut vis_changed = false;
                let mut opacity_released = false;
                let mut delete_clicked = false;
                ui.horizontal(|ui| {
                    let layer = &mut app.canvas.layers[i];
                    if ui.checkbox(&mut layer.visible, "").changed() {
                        vis_changed = true;
                    }
                    ui.checkbox(&mut layer.locked, "🔒");

                    let is_active = i == active_idx;
                    let desired = egui::vec2(ui.available_width() - 40.0, 60.0);
                    let (rect, block_response) =
                        ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
                    item_rects.push((i, rect));

                    let fill = app
                        .layer_state
                        .layer_ui_colors
                        .get(i)
                        .copied()
                        .unwrap_or(ui.visuals().extreme_bg_color);
                    ui.painter().rect_filled(rect.shrink(2.0), 6.0, fill);
                    if is_active {
                        let stroke = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
                        ui.painter().rect_stroke(rect.shrink(1.0), 8.0, stroke);
                    }

                    let mut content = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect.shrink2(egui::vec2(10.0, 8.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );

                    let field_width = (rect.width() - 70.0).max(140.0);
                    if is_active {
                        let resp = content.add(
                            egui::TextEdit::singleline(&mut layer.name)
                                .desired_width(field_width - 140.0)
                                .hint_text("Layer name"),
                        );
                        if resp.clicked() {
                            active_idx = i;
                        }
                    } else {
                        let resp = content.add_sized(
                            egui::vec2(field_width - 140.0, 24.0),
                            egui::Label::new(layer.name.as_str()),
                        );
                        if resp.clicked() {
                            active_idx = i;
                        }
                    }

                    let response = content
                        .add(egui::Slider::new(&mut layer.opacity, 0.0..=1.0).show_value(false));
                    opacity_released =
                        response.drag_stopped() || (response.changed() && !response.dragged());

                    if let Some(color) = app.layer_state.layer_ui_colors.get_mut(i)
                        && content.color_edit_button_srgba(color).clicked()
                    {
                        active_idx = i;
                    }

                    if app.canvas.layers.len() > 1 && i != 0 {
                        content.add_space(30.0);
                        let response =
                            content.add_sized(egui::vec2(20.0, 24.0), egui::Button::new("🗑"));
                        if response.clicked() {
                            delete_clicked = true;
                        }
                    }

                    if block_response.clicked() {
                        active_idx = i;
                    }

                    if block_response.drag_started() {
                        app.layer_state.layer_dragging = Some(i);
                    }

                    if block_response.drag_stopped()
                        && let Some(from) = app.layer_state.layer_dragging.take()
                        && let Some(pointer) = ctx.input(|i| i.pointer.hover_pos())
                    {
                        let mut target = from;
                        for (idx, rect) in &item_rects {
                            if rect.contains(pointer) {
                                target = *idx;
                                break;
                            }
                            if pointer.y < rect.top() {
                                target = *idx;
                            }
                        }
                        app.reorder_layers(from, target);
                        needs_refresh = true;
                        active_idx = app.canvas.active_layer_idx;
                    }

                    block_response.context_menu(|ui| {
                        if let Some(color) = app.layer_state.layer_ui_colors.get_mut(i) {
                            ui.menu_button("Layer color", |ui| {
                                ui.color_edit_button_srgba(color);
                            });
                        }
                    });
                });

                if vis_changed {
                    needs_refresh = true;
                    app.mark_layer_tiles_with_data_dirty(i);
                }
                if opacity_released {
                    needs_refresh = true;
                    app.mark_layer_tiles_with_data_dirty(i);
                }
                if delete_clicked {
                    to_delete = Some(i);
                }
            }

            if let Some(drag_idx) = app.layer_state.layer_dragging
                && let Some(pointer) = ctx.input(|i| i.pointer.hover_pos())
                && let Some((_, first_rect)) = item_rects.first()
            {
                let last_rect = item_rects.last().map(|(_, r)| *r).unwrap_or(*first_rect);
                let list_left = first_rect.left();
                let list_width = first_rect.width();
                let item_height = first_rect.height();
                let clamped_y = pointer.y.clamp(first_rect.top(), last_rect.bottom());
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(list_left, clamped_y - item_height * 0.5),
                    egui::vec2(list_width, item_height),
                );
                let color = app
                    .layer_state
                    .layer_ui_colors
                    .get(drag_idx)
                    .copied()
                    .unwrap_or(ui.visuals().extreme_bg_color);
                ui.painter()
                    .rect_filled(ghost_rect, 6.0, color.linear_multiply(0.7));
                ui.painter().rect_stroke(
                    ghost_rect.shrink(2.0),
                    6.0,
                    egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                );
                let name = app
                    .canvas
                    .layers
                    .get(drag_idx)
                    .map(|l| l.name.as_str())
                    .unwrap_or("Layer");
                ui.painter().text(
                    ghost_rect.left_top() + egui::vec2(12.0, 18.0),
                    egui::Align2::LEFT_TOP,
                    name,
                    egui::FontId::proportional(14.0),
                    ui.visuals().text_color(),
                );
            }
        });

    if add_layer {
        app.add_paint_layer();
        active_idx = app.canvas.layers.len().saturating_sub(1);
    }

    if let Some(idx) = to_delete {
        app.remove_paint_layer(idx);
        active_idx = app.canvas.active_layer_idx;
        needs_refresh = true;
    }

    app.canvas.active_layer_idx = active_idx;
    if needs_refresh {
        app.mark_all_tiles_dirty();
        ctx.request_repaint();
    }
}
