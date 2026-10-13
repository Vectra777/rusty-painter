//! The Gradient Editor: change a user gradient's stops. Click the strip to
//! add a stop with the colour already there, drag the markers under it to
//! move them, and set the selected stop's colour (fixed, or the brush's
//! primary or secondary colour), opacity and position. Changes show on a
//! gradient being placed right away, and are saved as they're made.

use crate::PainterApp;
use crate::app::tools::gradient_colors::{CustomGradient, StopColor};
use crate::ui::bar_slider::BarSlider;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use crate::ui::widgets::{paint_gradient_strip, paint_swatch, percent_of_unit, segmented};
use eframe::egui::{self, Color32, RichText, Stroke};

/// What a stop's colour follows, for the selector.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Fixed,
    Primary,
    Secondary,
}

pub fn gradient_editor_window(app: &mut PainterApp, ctx: &egui::Context) {
    // Save once a change is finished (not on every step of a drag).
    if !ctx.input(|i| i.pointer.any_down()) {
        app.workspace.gradient.library.save_if_changed();
    }
    let Some(mut editor) = app.workspace.gradient.editor else {
        return;
    };
    let colors = (
        app.brush_state.brush.brush_options.color,
        app.brush_state.secondary_color,
    );
    let state = &mut app.workspace.gradient;
    if editor.index >= state.library.custom.len() {
        state.editor = None;
        return;
    }
    let mut open = true;
    let mut changed = false;
    let mut delete = false;
    egui::Window::new("Gradient Editor")
        .fit_screen(ctx)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            let g = &mut state.library.custom[editor.index];
            ui.horizontal(|ui| {
                ui.label(RichText::new("Name").color(TEXT_DIM));
                changed |= ui.text_edit_singleline(&mut g.name).changed();
            });
            ui.add_space(4.0);
            changed |= stops_strip(ui, g, &mut editor, colors);
            ui.add_space(6.0);
            // A stop moved past another (by its slider) goes back in order
            // once let go.
            if editor.dragging.is_none() && !ui.input(|i| i.pointer.any_down()) {
                editor.selected = g.sort_stops(editor.selected);
            }
            editor.selected = editor.selected.min(g.stops.len().saturating_sub(1));
            changed |= selected_stop(ui, g, &mut editor.selected, colors);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button("Reverse")
                    .on_hover_text("Mirror the stops end to end")
                    .clicked()
                {
                    g.reverse();
                    changed = true;
                }
                if ui
                    .button("Spread evenly")
                    .on_hover_text("Space the stops evenly, keeping their order")
                    .clicked()
                {
                    g.spread();
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    delete = ui
                        .button("Delete gradient")
                        .on_hover_text("Remove it from your gradients")
                        .clicked();
                });
            });
        });

    let state = &mut app.workspace.gradient;
    if delete {
        state.settings.colors = state.library.remove(editor.index, state.settings.colors);
        state.editor = None;
        app.gradient_settings_changed();
        return;
    }
    state.editor = open.then_some(editor);
    if changed {
        state.library.mark_changed();
        let shown = state.settings.colors
            == crate::app::tools::gradient::GradientColors::Custom(editor.index);
        if shown {
            app.gradient_settings_changed();
        }
    }
}

/// The gradient with a marker under it per stop. A click on the strip adds
/// a stop; a marker is selected by clicking it and moved by dragging.
fn stops_strip(
    ui: &mut egui::Ui,
    g: &mut CustomGradient,
    editor: &mut crate::app::tools::gradient::GradientEditor,
    (primary, secondary): (Color32, Color32),
) -> bool {
    let touch = metrics(ui.ctx()).touch;
    let lane = if touch { 26.0 } else { 18.0 };
    let width = ui.available_width().max(300.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, 40.0 + lane),
        egui::Sense::click_and_drag(),
    );
    // Room at the sides so the end markers can be grabbed.
    let inset = lane / 2.0;
    let strip = egui::Rect::from_min_max(
        rect.min + egui::vec2(inset, 0.0),
        egui::pos2(rect.right() - inset, rect.top() + 40.0),
    );
    let x_of = |pos: f32| egui::lerp(strip.left()..=strip.right(), pos);
    let pos_of = |x: f32| ((x - strip.left()) / strip.width()).clamp(0.0, 1.0);
    let mut changed = false;

    // Pick a marker (or add a stop) where the press starts.
    let pressed = response.drag_started() || response.clicked();
    if pressed && let Some(p) = response.interact_pointer_pos() {
        let nearest = g
            .stops
            .iter()
            .enumerate()
            .map(|(i, s)| (i, (x_of(s.pos) - p.x).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match nearest {
            Some((i, d)) if d <= lane * 0.6 && p.y > strip.bottom() - 4.0 => {
                editor.selected = i;
                editor.dragging = response.drag_started().then_some(i);
            }
            _ if p.y <= strip.bottom() && response.clicked() => {
                editor.selected = g.add_stop(pos_of(p.x), primary, secondary);
                editor.selected = g.sort_stops(editor.selected);
                changed = true;
            }
            _ => {}
        }
    }
    if let (Some(i), Some(p)) = (editor.dragging, response.interact_pointer_pos())
        && response.dragged()
        && let Some(stop) = g.stops.get_mut(i)
    {
        let pos = pos_of(p.x);
        if pos != stop.pos {
            stop.pos = pos;
            changed = true;
        }
    }
    if response.drag_stopped()
        && let Some(i) = editor.dragging.take()
    {
        editor.selected = g.sort_stops(i);
    }

    let painter = ui.painter_at(rect.expand(2.0));
    paint_gradient_strip(&painter, strip, &g.resolve(primary, secondary));
    for (i, stop) in g.stops.iter().enumerate() {
        let x = x_of(stop.pos);
        let color = match stop.color {
            StopColor::Primary => primary,
            StopColor::Secondary => secondary,
            StopColor::Fixed([r, g, b]) => Color32::from_rgb(r, g, b),
        };
        let [r, gr, b, _] = color.to_srgba_unmultiplied();
        let a = (stop.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        let selected = i == editor.selected;
        let outline = if selected { accent() } else { TEXT_DIM };
        // A notch pointing at the strip, and the stop's colour below it.
        let tip = egui::pos2(x, strip.bottom() + 1.0);
        painter.add(egui::Shape::convex_polygon(
            vec![
                tip,
                egui::pos2(x + 5.0, tip.y + 6.0),
                egui::pos2(x - 5.0, tip.y + 6.0),
            ],
            outline,
            Stroke::NONE,
        ));
        let size = lane - 8.0;
        let swatch = egui::Rect::from_center_size(
            egui::pos2(x, tip.y + 6.0 + size / 2.0),
            egui::vec2(size, size),
        );
        paint_swatch(
            &painter,
            swatch,
            Color32::from_rgba_unmultiplied(r, gr, b, a),
        );
        painter.rect_stroke(
            swatch.expand(1.0),
            1.0,
            Stroke::new(if selected { 2.0_f32 } else { 1.0 }, outline),
        );
        if matches!(stop.color, StopColor::Primary | StopColor::Secondary) {
            let letter = if stop.color == StopColor::Primary {
                "1"
            } else {
                "2"
            };
            painter.text(
                swatch.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                egui::FontId::proportional(size * 0.7),
                if color.r() as u32 + color.g() as u32 + color.b() as u32 > 380 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                },
            );
        }
    }
    response.on_hover_text(
        "Click the strip to add a stop · drag a marker to move it · click a marker to edit it",
    );
    changed
}

/// Colour, opacity and position of the selected stop, and deleting it.
fn selected_stop(
    ui: &mut egui::Ui,
    g: &mut CustomGradient,
    selected: &mut usize,
    (primary, secondary): (Color32, Color32),
) -> bool {
    let count = g.stops.len();
    let Some(stop) = g.stops.get_mut(*selected) else {
        return false;
    };
    let mut changed = false;
    ui.label(
        RichText::new(format!("STOP {} OF {count}", *selected + 1))
            .small()
            .strong()
            .color(TEXT_DIM),
    );
    let mut source = match stop.color {
        StopColor::Fixed(_) => Source::Fixed,
        StopColor::Primary => Source::Primary,
        StopColor::Secondary => Source::Secondary,
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Colour").color(TEXT_DIM));
        if segmented(
            ui,
            &mut source,
            &[
                (Source::Fixed, "Fixed"),
                (Source::Primary, "Primary"),
                (Source::Secondary, "Secondary"),
            ],
            true,
        ) {
            let rgb = |c: Color32| {
                let [r, g, b, _] = c.to_srgba_unmultiplied();
                [r, g, b]
            };
            stop.color = match source {
                // Start from the colour it showed.
                Source::Fixed => StopColor::Fixed(match stop.color {
                    StopColor::Secondary => rgb(secondary),
                    _ => rgb(primary),
                }),
                Source::Primary => StopColor::Primary,
                Source::Secondary => StopColor::Secondary,
            };
            changed = true;
        }
        if let StopColor::Fixed(rgb) = &mut stop.color {
            // With alpha: the picker's is the stop's opacity, so a stop can
            // be made see-through where its colour is chosen.
            let alpha = (stop.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
            let mut rgba = [rgb[0], rgb[1], rgb[2], alpha];
            if ui
                .color_edit_button_srgba_unmultiplied(&mut rgba)
                .on_hover_text("Colour and opacity of this stop")
                .changed()
            {
                *rgb = [rgba[0], rgba[1], rgba[2]];
                if rgba[3] != alpha {
                    stop.opacity = rgba[3] as f32 / 255.0;
                }
                changed = true;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Opacity").color(TEXT_DIM));
        changed |= ui
            .add(crate::ui::widgets::reset(&mut stop.opacity, |v| {
                percent_of_unit(BarSlider::new(v, 0.0..=1.0))
            }))
            .changed();
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Position").color(TEXT_DIM));
        let moved = ui
            .add(crate::ui::widgets::reset(&mut stop.pos, |v| {
                percent_of_unit(BarSlider::new(v, 0.0..=1.0))
            }))
            .changed();
        changed |= moved;
    });
    if ui
        .add_enabled(count > 2, egui::Button::new("Delete stop"))
        .on_hover_text("A gradient keeps at least two stops")
        .clicked()
    {
        g.stops.remove(*selected);
        *selected = selected.saturating_sub(1);
        changed = true;
    }
    changed
}
