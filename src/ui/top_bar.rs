use crate::PainterApp;
use crate::app::tools::Tool;
use crate::selection::SelectionType;
use eframe::egui;

pub fn top_bar(app: &mut PainterApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("quick_settings").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut app.active_tool, Tool::Brush, "🖌 Brush");

            let is_select = matches!(app.active_tool, Tool::Select(_));
            let current_select_type = if let Tool::Select(t) = app.active_tool {
                t
            } else {
                SelectionType::Rectangle // Default for display if not active
            };

            ui.menu_button(
                if is_select {
                    match current_select_type {
                        SelectionType::Rectangle => "⬚ Rect",
                        SelectionType::Circle => "◯ Circle",
                        SelectionType::Lasso => "〰 Lasso",
                    }
                } else {
                    "Select"
                },
                |ui| {
                    if ui
                        .selectable_label(
                            is_select && current_select_type == SelectionType::Rectangle,
                            "Rectangle",
                        )
                        .clicked()
                    {
                        app.active_tool = Tool::Select(SelectionType::Rectangle);
                        ui.close_menu();
                    }
                    if ui
                        .selectable_label(
                            is_select && current_select_type == SelectionType::Circle,
                            "Circle",
                        )
                        .clicked()
                    {
                        app.active_tool = Tool::Select(SelectionType::Circle);
                        ui.close_menu();
                    }
                    if ui
                        .selectable_label(
                            is_select && current_select_type == SelectionType::Lasso,
                            "Lasso",
                        )
                        .clicked()
                    {
                        app.active_tool = Tool::Select(SelectionType::Lasso);
                        ui.close_menu();
                    }
                },
            );

            if ui
                .selectable_label(matches!(app.active_tool, Tool::Transform(_)), "Transform")
                .clicked()
            {
                app.active_tool =
                    Tool::Transform(crate::selection::transform::TransformInfo::default());
            }

            if ui.button("New Canvas").clicked() {
                app.modal_state.new_canvas.sync_from_canvas(&app.canvas);
                app.modal_state.new_canvas.color_model = app.workspace.color_model;
                app.modal_state.show_new_canvas_modal = true;
            }
            ui.add(egui::Slider::new(
                &mut app.brush_state.brush.brush_options.diameter,
                1.0..=3000.0,
            ));
            if ui.button("Export").clicked() {
                app.export_state.settings.chosen_path = None;
                app.export_state.message = None;
                app.export_state.show_modal = true;
            }
            if ui.button("Settings").clicked() {
                app.modal_state.show_general_settings = true;
                ctx.request_repaint();
            }
        });
    });
}
