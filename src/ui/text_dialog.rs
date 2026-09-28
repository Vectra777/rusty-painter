//! The Text tool's dialog (the text being typed) and its settings, shared
//! with the options bar.

use crate::PainterApp;
use crate::canvas::text::TextAlign;
use crate::ui::widgets::segmented;
use eframe::egui;

/// Font, size, alignment and spacing. Returns whether anything changed.
pub(crate) fn text_controls(app: &mut PainterApp, ui: &mut egui::Ui) -> bool {
    let state = &mut app.workspace.text;
    state.ensure_fonts();
    let mut changed = false;
    let current = state
        .fonts
        .get(state.font)
        .map_or("Font", |f| f.name.as_str())
        .to_string();
    egui::ComboBox::from_id_salt("text_font")
        .selected_text(current)
        .width(170.0)
        .height(400.0)
        .show_ui(ui, |ui| {
            for (i, f) in state.fonts.iter().enumerate() {
                changed |= ui.selectable_value(&mut state.font, i, &f.name).changed();
            }
        });
    let style = &mut state.style;
    changed |= ui
        .add(
            egui::DragValue::new(&mut style.size)
                .range(4.0..=2000.0)
                .speed(0.5)
                .suffix(" px"),
        )
        .on_hover_text("Font size")
        .changed();
    changed |= segmented(
        ui,
        &mut style.align,
        &[
            (TextAlign::Left, "Left"),
            (TextAlign::Center, "Centre"),
            (TextAlign::Right, "Right"),
        ],
        true,
    );
    changed |= ui
        .add(
            egui::DragValue::new(&mut style.line_spacing)
                .range(0.5..=3.0)
                .speed(0.01)
                .prefix("Lines ×"),
        )
        .on_hover_text("Line spacing")
        .changed();
    changed |= ui
        .add(
            egui::DragValue::new(&mut style.letter_spacing)
                .range(-50.0..=200.0)
                .speed(0.2)
                .prefix("Letters ")
                .suffix(" px"),
        )
        .on_hover_text("Space between letters")
        .changed();
    changed
}

pub fn text_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(session) = app.workspace.text.session.as_mut() else {
        return;
    };
    let focus = std::mem::take(&mut session.focus);
    let mut text = std::mem::take(&mut session.text);
    let (mut ok, mut cancel) = (false, false);
    let mut open = true;
    egui::Window::new("Text")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .show(ctx, |ui| {
            let edit = ui.add(
                egui::TextEdit::multiline(&mut text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Type here; drag the text on the canvas to move it"),
            );
            if focus {
                edit.request_focus();
            }
            ui.horizontal_wrapped(|ui| {
                text_controls(app, ui);
            });
            ui.label(
                egui::RichText::new("In the brush colour. Kept as pixels on a new layer.")
                    .small()
                    .color(crate::ui::style::TEXT_DIM),
            );
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if let Some(s) = app.workspace.text.session.as_mut() {
        s.text = text;
    }
    // Esc cancels; Ctrl+Enter keeps (Enter types a new line).
    let (esc, keep) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::Escape),
            i.key_pressed(egui::Key::Enter) && i.modifiers.command,
        )
    });
    if ok || keep {
        app.text_commit();
    } else if cancel || esc || !open {
        app.text_cancel();
    }
}
