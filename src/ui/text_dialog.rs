//! The Text tool's settings, for the options bar and the touch screen's
//! context bar: the font (searchable, by family and style), size,
//! alignment, spacing, the text's colour, and keeping or dropping the
//! text being typed. The text itself is typed on the canvas.

use crate::PainterApp;
use crate::canvas::text::TextAlign;
use crate::ui::widgets::segmented;
use eframe::egui;

/// Font, size, alignment and spacing. Returns whether anything changed.
pub(crate) fn text_controls(app: &mut PainterApp, ui: &mut egui::Ui) -> bool {
    if let Some(s) = app.workspace.text.session.as_mut() {
        ui.color_edit_button_srgba(&mut s.color)
            .on_hover_text("The text's colour (picking a brush colour sets it too)");
    }
    let state = &mut app.workspace.text;
    state.ensure_fonts();
    if !state.scanned {
        // The list fills in once the system's fonts are found.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    let mut changed = false;
    let current = state
        .fonts
        .get(state.font)
        .map_or("Font", |f| f.name.as_str())
        .to_string();
    egui::ComboBox::from_id_salt("text_font")
        .selected_text(current)
        .width(190.0)
        .height(420.0)
        .show_ui(ui, |ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut state.search)
                    .hint_text("Search fonts")
                    .desired_width(180.0),
            );
            if !search.has_focus() && state.search.is_empty() {
                search.request_focus();
            }
            if !state.scanned {
                ui.label(
                    egui::RichText::new("Loading fonts…")
                        .small()
                        .color(crate::ui::style::TEXT_DIM),
                );
            }
            ui.separator();
            let words: Vec<String> = state
                .search
                .to_lowercase()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let mut family = "";
            for (i, f) in state.fonts.iter().enumerate() {
                let name = f.name.to_lowercase();
                if !words.iter().all(|w| name.contains(w.as_str())) {
                    continue;
                }
                // Each family's styles together, under its name.
                if f.family != family && f.name != f.family {
                    ui.label(
                        egui::RichText::new(&f.family)
                            .small()
                            .color(crate::ui::style::TEXT_DIM),
                    );
                }
                family = &f.family;
                let label = if f.name != f.family {
                    format!(
                        "    {}",
                        f.name.strip_prefix(&f.family).unwrap_or(&f.name).trim()
                    )
                } else {
                    f.name.clone()
                };
                changed |= ui.selectable_value(&mut state.font, i, label).changed();
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
    if app.workspace.text.session.is_some() {
        crate::ui::widgets::vdivider(ui);
        if ui
            .button("Keep")
            .on_hover_text("Keep the text (Ctrl+Enter, or click away from it)")
            .clicked()
        {
            app.text_commit();
        }
        if ui.button("Cancel").on_hover_text("Esc").clicked() {
            app.text_cancel();
        }
    }
    changed
}
