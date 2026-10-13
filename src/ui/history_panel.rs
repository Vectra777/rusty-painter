//! Edit → History: every step that can be undone or redone, by what it
//! did. Clicking one undoes (or redoes) up to it; the steps after it stay,
//! dimmed, until something new is done.

use crate::PainterApp;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use eframe::egui::{self, RichText};

pub fn history_window(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_history {
        return;
    }
    let (undo, redo) = app.layer_state.history.labels();
    let current = undo.len();
    // Oldest first, then what can be redone, next first.
    let rows: Vec<(String, bool)> = undo
        .iter()
        .map(|l| (l.clone(), true))
        .chain(redo.iter().rev().map(|l| (l.clone(), false)))
        .collect();
    // Scroll to the current step when it moves.
    let seen_id = egui::Id::new("history_panel_seen");
    let moved = ctx.data(|d| d.get_temp::<(usize, usize)>(seen_id)) != Some((current, rows.len()));
    ctx.data_mut(|d| d.insert_temp(seen_id, (current, rows.len())));

    let mut open = true;
    let mut jump = None;
    egui::Window::new("History")
        .fit_screen_size(ctx)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([240.0, 360.0])
        .default_pos(ctx.screen_rect().right_top() + egui::vec2(-320.0, 60.0))
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Click a step to go back to it.")
                    .small()
                    .color(TEXT_DIM),
            );
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let start = ui.selectable_label(current == 0, "Start");
                    if start.clicked() {
                        jump = Some(0);
                    }
                    if moved && current == 0 {
                        start.scroll_to_me(None);
                    }
                    for (i, (label, done)) in rows.iter().enumerate() {
                        let n = i + 1;
                        let text = RichText::new(format!("{n}. {label}"));
                        let text = if *done {
                            text
                        } else {
                            text.color(TEXT_DIM).italics()
                        };
                        let row = ui.selectable_label(n == current, text);
                        if row.clicked() {
                            jump = Some(n);
                        }
                        if moved && n == current {
                            row.scroll_to_me(None);
                        }
                    }
                    if rows.is_empty() {
                        ui.label(RichText::new("Nothing done yet.").color(TEXT_DIM));
                    }
                });
        });
    if let Some(n) = jump {
        app.history_jump(n);
    }
    app.modal_state.show_history = open;
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use eframe::egui::{self, Color32, Vec2};

    fn app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.selection_manager.canvas_size = [64, 64];
        app
    }

    fn stroke(app: &mut crate::PainterApp, y: f32) {
        app.brush_state.brush.brush_options.diameter = 6.0;
        app.start_stroke_with_pressure(Vec2::new(5.0, y), 1.0);
        for x in 6..50 {
            app.add_stroke_point(Vec2::new(x as f32, y), 1.0);
        }
        app.finish_stroke();
        app.release_canvas();
    }

    fn labels(app: &crate::PainterApp) -> (Vec<String>, Vec<String>) {
        let (u, r) = app.layer_state.history.labels();
        (u.to_vec(), r.to_vec())
    }

    #[test]
    fn steps_are_named_and_a_click_goes_back_and_forth_to_one() {
        let mut app = app();
        app.layer_state.history.set_tool_label("Brush");
        stroke(&mut app, 10.0);
        app.add_layer_and_select();
        app.layer_state.history.set_tool_label("Eraser");
        stroke(&mut app, 30.0);
        let steps = labels(&app).0;
        assert_eq!(steps, ["Brush", "New layer", "Eraser"]);
        let (undo, redo) = app.layer_state.history.stacks();
        assert_eq!((undo.len(), redo.len()), (3, 0));

        // Back to the first step: two undone, kept to redo (next first).
        app.history_jump(1);
        assert_eq!(
            labels(&app),
            (
                vec!["Brush".into()],
                vec!["Eraser".into(), "New layer".into()]
            )
        );
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        // Forward again to the end, and back to the start.
        app.history_jump(3);
        assert_eq!(labels(&app).0, steps);
        app.history_jump(0);
        assert!(labels(&app).0.is_empty());
        assert_eq!(labels(&app).1.len(), 3);
        // Something new drops what could be redone.
        stroke(&mut app, 50.0);
        assert_eq!(labels(&app).1.len(), 0);
        assert_eq!(labels(&app).0.len(), 1);
    }

    #[test]
    fn commands_name_their_own_steps() {
        let mut app = app();
        app.layer_state.history.set_tool_label("Brush");
        stroke(&mut app, 10.0);
        app.filter_open(crate::canvas::filters::Filter::Invert);
        app.filter_commit();
        app.select_all();
        app.delete_selection_contents();
        let steps = labels(&app).0;
        assert_eq!(steps.last().map(String::as_str), Some("Delete"));
        assert!(steps.iter().any(|s| s == "Invert"), "{steps:?}");
        assert!(steps.iter().any(|s| s == "Selection"), "{steps:?}");
    }

    #[test]
    fn the_window_lists_every_step() {
        let mut app = app();
        app.layer_state.history.set_tool_label("Brush");
        stroke(&mut app, 10.0);
        stroke(&mut app, 20.0);
        app.apply_history(false);
        app.modal_state.show_history = true;
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            super::history_window(&mut app, ctx);
        });
        // A second frame, once the window has its size.
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            super::history_window(&mut app, ctx);
        });
        for shape in out.shapes {
            if let egui::epaint::Shape::Text(t) = shape.shape {
                texts.push(t.galley.text().to_string());
            }
        }
        for want in ["Start", "1. Brush", "2. Brush"] {
            assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
        }
    }
}
