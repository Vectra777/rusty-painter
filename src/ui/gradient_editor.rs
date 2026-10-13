//! Editing a gradient's stops, as Photoshop's Gradient Editor does: colour
//! stops under the strip, opacity stops over it, and between each pair a
//! midpoint (a diamond) saying where they're mixed half and half. Click a
//! lane to add a stop where the gradient already is, drag a marker to move
//! it or off the strip to remove it, and set the selected one below.
//!
//! [`edit_gradient`] is the editor itself, used by the Gradient Editor
//! window (the gradient tool's own gradients, kept in the library), the
//! gradient map and gradient fill layers.

use crate::PainterApp;
use crate::app::tools::gradient_colors::{CustomGradient, EditStop, GradientLibrary, StopColor};
use crate::canvas::gradient::GradientStops;
#[cfg(test)]
use crate::canvas::gradient::OpacityStop;
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

/// A part of the gradient picked in the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Color(usize),
    Opacity(usize),
    /// The midpoint after colour stop `i`.
    ColorMid(usize),
    /// The midpoint after opacity stop `i`.
    OpacityMid(usize),
}

impl Default for Part {
    fn default() -> Self {
        Part::Color(0)
    }
}

/// The editor's selection, kept in egui's memory by its id.
#[derive(Clone, Copy, Debug, Default)]
struct EditorState {
    selected: Part,
    dragging: Option<Part>,
}

/// What [`edit_gradient`] offers.
#[derive(Clone, Copy)]
pub struct EditorOptions {
    /// Colour stops may follow the brush's primary or secondary colour (a
    /// library gradient, resolved when used); else they're fixed colours.
    pub brush_colors: bool,
    /// Opacity stops (a gradient map has none: it maps colours).
    pub opacity: bool,
}

/// How far below or above the strip (in points) a marker is dragged to
/// take it off.
const TEAR_OFF: f32 = 36.0;

/// The editor for `g`: the strip with its markers, and the selected part's
/// settings. Returns whether `g` changed.
pub fn edit_gradient(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    g: &mut CustomGradient,
    colors: (Color32, Color32),
    options: EditorOptions,
) -> bool {
    let id = ui.make_persistent_id(id_salt);
    let mut state: EditorState = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut changed = strip(ui, g, &mut state, colors, options);
    ui.add_space(6.0);
    // A stop moved past another (by its slider) goes back in order once
    // let go.
    if state.dragging.is_none() && !ui.input(|i| i.pointer.any_down()) {
        state.selected = match state.selected {
            Part::Color(i) => Part::Color(g.sort_stops(i)),
            Part::Opacity(i) => Part::Opacity(g.sort_opacity(i)),
            other => other,
        };
    }
    state.selected = clamp_part(state.selected, g);
    changed |= selected_part(ui, g, &mut state.selected, colors, options);
    ui.data_mut(|d| d.insert_temp(id, state));
    changed
}

/// `part`, or the nearest that still exists.
fn clamp_part(part: Part, g: &CustomGradient) -> Part {
    let (colors, opacity) = (g.stops.len(), g.opacity.len());
    match part {
        Part::Color(i) => Part::Color(i.min(colors.saturating_sub(1))),
        Part::Opacity(i) if i < opacity => Part::Opacity(i),
        Part::ColorMid(i) if i + 1 < colors => Part::ColorMid(i),
        Part::OpacityMid(i) if i + 1 < opacity => Part::OpacityMid(i),
        _ => Part::Color(0),
    }
}

/// The strip, the opacity lane over it and the colour lane under it.
fn strip(
    ui: &mut egui::Ui,
    g: &mut CustomGradient,
    state: &mut EditorState,
    (primary, secondary): (Color32, Color32),
    options: EditorOptions,
) -> bool {
    let touch = metrics(ui.ctx()).touch;
    let lane = if touch { 26.0 } else { 18.0 };
    let top_lane = if options.opacity { lane } else { 0.0 };
    let width = ui.available_width().max(260.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, top_lane + 36.0 + lane),
        egui::Sense::click_and_drag(),
    );
    // Room at the sides so the end markers can be grabbed.
    let inset = lane / 2.0;
    let bar = egui::Rect::from_min_max(
        egui::pos2(rect.left() + inset, rect.top() + top_lane),
        egui::pos2(rect.right() - inset, rect.top() + top_lane + 36.0),
    );
    let x_of = |pos: f32| egui::lerp(bar.left()..=bar.right(), pos);
    let pos_of = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0);
    let mut changed = false;
    g.sort_stops(0);
    g.sort_opacity(0);
    let resolved = g.resolve(primary, secondary);

    // What a press at `p` picks: a marker or midpoint in that lane.
    let hit = |p: egui::Pos2| -> Option<Part> {
        let near = |pos: f32| (x_of(pos) - p.x).abs() <= lane * 0.6;
        let between = |a: f32, b: f32, m: f32| a + (b - a) * m;
        if p.y > bar.bottom() - 4.0 {
            let stops = &g.stops;
            if let Some(i) = (0..stops.len()).find(|&i| near(stops[i].pos)) {
                return Some(Part::Color(i));
            }
            return (0..stops.len().saturating_sub(1))
                .find(|&i| near(between(stops[i].pos, stops[i + 1].pos, stops[i].mid)))
                .map(Part::ColorMid);
        }
        if options.opacity && p.y < bar.top() + 4.0 {
            let stops = &g.opacity;
            if let Some(i) = (0..stops.len()).find(|&i| near(stops[i].pos)) {
                return Some(Part::Opacity(i));
            }
            return (0..stops.len().saturating_sub(1))
                .find(|&i| near(between(stops[i].pos, stops[i + 1].pos, stops[i].mid)))
                .map(Part::OpacityMid);
        }
        None
    };

    let pressed = response.drag_started() || response.clicked();
    if pressed && let Some(p) = response.interact_pointer_pos() {
        match hit(p) {
            Some(part) => {
                state.selected = part;
                state.dragging = response.drag_started().then_some(part);
            }
            None if response.clicked() => {
                // A click on the strip or the colour lane adds a colour
                // stop; on the opacity lane, an opacity stop.
                if options.opacity && p.y < bar.top() {
                    let i = g.add_opacity_stop(pos_of(p.x));
                    state.selected = Part::Opacity(g.sort_opacity(i));
                } else {
                    let i = g.add_stop(pos_of(p.x), primary, secondary);
                    state.selected = Part::Color(g.sort_stops(i));
                }
                changed = true;
            }
            None => {}
        }
    }
    if let (Some(part), Some(p)) = (state.dragging, response.interact_pointer_pos())
        && response.dragged()
    {
        let pos = pos_of(p.x);
        let mid_of = |a: f32, b: f32| ((pos - a) / (b - a).max(1e-6)).clamp(0.05, 0.95);
        match part {
            Part::Color(i) => {
                if let Some(s) = g.stops.get_mut(i) {
                    changed |= s.pos != pos;
                    s.pos = pos;
                }
            }
            Part::Opacity(i) => {
                if let Some(s) = g.opacity.get_mut(i) {
                    changed |= s.pos != pos;
                    s.pos = pos;
                }
            }
            Part::ColorMid(i) if i + 1 < g.stops.len() => {
                let m = mid_of(g.stops[i].pos, g.stops[i + 1].pos);
                changed |= g.stops[i].mid != m;
                g.stops[i].mid = m;
            }
            Part::OpacityMid(i) if i + 1 < g.opacity.len() => {
                let m = mid_of(g.opacity[i].pos, g.opacity[i + 1].pos);
                changed |= g.opacity[i].mid != m;
                g.opacity[i].mid = m;
            }
            _ => {}
        }
    }
    if response.drag_stopped()
        && let Some(part) = state.dragging.take()
    {
        // Dragged well off the strip: taken off (a gradient keeps two
        // colour stops; it may lose every opacity stop, and is opaque).
        let off = response.interact_pointer_pos().is_some_and(|p| {
            p.y > bar.bottom() + lane + TEAR_OFF || p.y < bar.top() - top_lane - TEAR_OFF
        });
        state.selected = match part {
            Part::Color(i) if off && g.stops.len() > 2 => {
                g.stops.remove(i);
                changed = true;
                Part::Color(i.saturating_sub(1))
            }
            Part::Opacity(i) if off => {
                g.opacity.remove(i);
                changed = true;
                Part::Color(0)
            }
            Part::Color(i) => Part::Color(g.sort_stops(i)),
            Part::Opacity(i) => Part::Opacity(g.sort_opacity(i)),
            other => other,
        };
    }

    let painter = ui.painter_at(rect.expand(2.0));
    paint_gradient_strip(&painter, bar, &resolved);
    let marker =
        |x: f32, y: f32, down: bool, fill: Color32, selected: bool, letter: Option<&str>| {
            let outline = if selected { accent() } else { TEXT_DIM };
            // A notch pointing at the strip, and the stop's colour beside it.
            let (tip, dir) = if down {
                (egui::pos2(x, y - 1.0), -1.0)
            } else {
                (egui::pos2(x, y + 1.0), 1.0)
            };
            painter.add(egui::Shape::convex_polygon(
                vec![
                    tip,
                    egui::pos2(x + 5.0, tip.y + 6.0 * dir),
                    egui::pos2(x - 5.0, tip.y + 6.0 * dir),
                ],
                outline,
                Stroke::NONE,
            ));
            let size = lane - 8.0;
            let swatch = egui::Rect::from_center_size(
                egui::pos2(x, tip.y + (6.0 + size / 2.0) * dir),
                egui::vec2(size, size),
            );
            paint_swatch(&painter, swatch, fill);
            painter.rect_stroke(
                swatch.expand(1.0),
                1.0,
                Stroke::new(if selected { 2.0_f32 } else { 1.0 }, outline),
            );
            if let Some(letter) = letter {
                let light = fill.r() as u32 + fill.g() as u32 + fill.b() as u32 > 380;
                painter.text(
                    swatch.center(),
                    egui::Align2::CENTER_CENTER,
                    letter,
                    egui::FontId::proportional(size * 0.7),
                    if light {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    },
                );
            }
        };
    let diamond = |x: f32, y: f32, selected: bool| {
        let r = 4.0;
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x, y - r),
                egui::pos2(x + r, y),
                egui::pos2(x, y + r),
                egui::pos2(x - r, y),
            ],
            if selected {
                accent()
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(1.0_f32, if selected { accent() } else { TEXT_DIM }),
        ));
    };
    for (i, s) in g.stops.iter().enumerate() {
        let fill = match s.color {
            StopColor::Primary => primary,
            StopColor::Secondary => secondary,
            StopColor::Fixed([r, g, b]) => Color32::from_rgb(r, g, b),
        };
        let letter = match s.color {
            StopColor::Primary => Some("1"),
            StopColor::Secondary => Some("2"),
            StopColor::Fixed(_) => None,
        };
        marker(
            x_of(s.pos),
            bar.bottom(),
            false,
            fill,
            state.selected == Part::Color(i),
            letter,
        );
    }
    for (i, w) in g.stops.windows(2).enumerate() {
        let x = x_of(w[0].pos + (w[1].pos - w[0].pos) * w[0].mid);
        diamond(
            x,
            bar.bottom() + lane - 4.0,
            state.selected == Part::ColorMid(i),
        );
    }
    if options.opacity {
        for (i, s) in g.opacity.iter().enumerate() {
            let v = (s.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
            let fill = Color32::from_gray(v);
            marker(
                x_of(s.pos),
                bar.top(),
                true,
                fill,
                state.selected == Part::Opacity(i),
                None,
            );
        }
        for (i, w) in g.opacity.windows(2).enumerate() {
            let x = x_of(w[0].pos + (w[1].pos - w[0].pos) * w[0].mid);
            diamond(
                x,
                bar.top() - top_lane + 4.0,
                state.selected == Part::OpacityMid(i),
            );
        }
    }
    response.on_hover_text(if options.opacity {
        "Click under the strip to add a colour stop, over it for an opacity stop · drag a marker \
         to move it, or off the strip to remove it · drag a diamond to move the midpoint"
    } else {
        "Click the strip to add a stop · drag a marker to move it, or off the strip to remove it \
         · drag a diamond to move the midpoint"
    });
    changed
}

/// The selected part's settings.
fn selected_part(
    ui: &mut egui::Ui,
    g: &mut CustomGradient,
    selected: &mut Part,
    (primary, secondary): (Color32, Color32),
    options: EditorOptions,
) -> bool {
    let mut changed = false;
    let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(TEXT_DIM));
            add(ui);
        });
    };
    let heading = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(text).small().strong().color(TEXT_DIM));
    };
    match *selected {
        Part::Color(i) => {
            let count = g.stops.len();
            let Some(stop) = g.stops.get_mut(i) else {
                return false;
            };
            heading(ui, format!("COLOUR STOP {} OF {count}", i + 1));
            row(ui, "Colour", &mut |ui| {
                if options.brush_colors {
                    let mut source = match stop.color {
                        StopColor::Fixed(_) => Source::Fixed,
                        StopColor::Primary => Source::Primary,
                        StopColor::Secondary => Source::Secondary,
                    };
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
                }
                if let StopColor::Fixed(rgb) = &mut stop.color {
                    changed |= ui
                        .color_edit_button_srgb(rgb)
                        .on_hover_text("This stop's colour")
                        .changed();
                }
            });
            row(ui, "Position", &mut |ui| {
                changed |= ui
                    .add(percent_of_unit(BarSlider::new(&mut stop.pos, 0.0..=1.0)))
                    .changed();
            });
            if ui
                .add_enabled(count > 2, egui::Button::new("Delete stop"))
                .on_hover_text("A gradient keeps at least two colour stops")
                .clicked()
            {
                g.stops.remove(i);
                *selected = Part::Color(i.saturating_sub(1));
                changed = true;
            }
        }
        Part::Opacity(i) => {
            let count = g.opacity.len();
            let Some(stop) = g.opacity.get_mut(i) else {
                return false;
            };
            heading(ui, format!("OPACITY STOP {} OF {count}", i + 1));
            row(ui, "Opacity", &mut |ui| {
                changed |= ui
                    .add(percent_of_unit(BarSlider::new(
                        &mut stop.opacity,
                        0.0..=1.0,
                    )))
                    .changed();
            });
            row(ui, "Position", &mut |ui| {
                changed |= ui
                    .add(percent_of_unit(BarSlider::new(&mut stop.pos, 0.0..=1.0)))
                    .changed();
            });
            if ui
                .button("Delete stop")
                .on_hover_text("With no opacity stops the gradient is opaque")
                .clicked()
            {
                g.opacity.remove(i);
                *selected = Part::Color(0);
                changed = true;
            }
        }
        Part::ColorMid(i) | Part::OpacityMid(i) => {
            let mid = match *selected {
                Part::ColorMid(_) => g.stops.get_mut(i).map(|s| &mut s.mid),
                _ => g.opacity.get_mut(i).map(|s| &mut s.mid),
            };
            let Some(mid) = mid else {
                return false;
            };
            heading(ui, format!("MIDPOINT AFTER STOP {}", i + 1));
            row(ui, "Midpoint", &mut |ui| {
                changed |= ui
                    .add(crate::ui::widgets::reset(mid, |v| {
                        percent_of_unit(BarSlider::new(v, 0.05..=0.95))
                    }))
                    .on_hover_text("Where the two stops are mixed half and half")
                    .changed();
            });
        }
    }
    if !options.opacity {
        return changed;
    }
    if ui
        .small_button("Add opacity stop")
        .on_hover_text("Fade part of the gradient (or click over the strip)")
        .clicked()
    {
        let at = if g.opacity.is_empty() { 0.0 } else { 0.5 };
        if g.opacity.is_empty() {
            // Opaque at both ends to start with.
            g.add_opacity_stop(1.0);
        }
        let i = g.add_opacity_stop(at);
        *selected = Part::Opacity(g.sort_opacity(i));
        changed = true;
    }
    changed
}

/// A document gradient (a gradient map's or fill layer's) as an editable
/// one: fixed colours.
pub fn editable(stops: &GradientStops) -> CustomGradient {
    CustomGradient {
        name: String::new(),
        stops: stops
            .colors
            .iter()
            .map(|s| EditStop {
                mid: s.mid,
                ..EditStop::new(s.pos, StopColor::Fixed(s.color))
            })
            .collect(),
        opacity: stops.opacity.clone(),
    }
}

/// [`edit_gradient`] on a document gradient (fixed colours), with a menu
/// to start from any library gradient and a button to keep it in the
/// library. Returns whether `stops` changed.
pub fn edit_document_gradient(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + Copy,
    stops: &mut GradientStops,
    library: &mut GradientLibrary,
    colors: (Color32, Color32),
    opacity: bool,
) -> bool {
    let mut g = editable(stops);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.menu_button("Gradients ▾", |ui| {
            ui.set_min_width(220.0);
            for choice in library.choices().collect::<Vec<_>>() {
                let preview = library.stops(choice, colors.0, colors.1);
                let clicked = ui
                    .horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(60.0, 14.0), egui::Sense::hover());
                        paint_gradient_strip(ui.painter(), rect, &preview);
                        ui.selectable_label(false, library.name(choice)).clicked()
                    })
                    .inner;
                if clicked {
                    let mut chosen = preview;
                    if !opacity {
                        chosen.opacity.clear();
                    }
                    g = editable(&chosen);
                    changed = true;
                    ui.close_menu();
                }
            }
        })
        .response
        .on_hover_text("Start from a preset or one of your gradients");
        if ui
            .button("Reverse")
            .on_hover_text("Mirror the stops end to end")
            .clicked()
        {
            g.reverse();
            changed = true;
        }
        if ui
            .button("Save to my gradients")
            .on_hover_text("Keep it with the gradient tool's gradients")
            .clicked()
        {
            let mut keep = g.clone();
            keep.name = format!("Gradient {}", library.custom.len() + 1);
            library.add(keep);
            library.save_if_changed();
        }
    });
    changed |= edit_gradient(
        ui,
        id_salt,
        &mut g,
        colors,
        EditorOptions {
            brush_colors: false,
            opacity,
        },
    );
    if changed {
        let mut resolved = g.resolve(colors.0, colors.1);
        if !opacity {
            resolved.opacity.clear();
        }
        *stops = resolved;
    }
    changed
}

pub fn gradient_editor_window(app: &mut PainterApp, ctx: &egui::Context) {
    // Save once a change is finished (not on every step of a drag).
    if !ctx.input(|i| i.pointer.any_down()) {
        app.workspace.gradient.library.save_if_changed();
    }
    let Some(editor) = app.workspace.gradient.editor else {
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
            changed |= edit_gradient(
                ui,
                ("gradient editor", editor.index),
                g,
                colors,
                EditorOptions {
                    brush_colors: true,
                    opacity: true,
                },
            );
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
                    .on_hover_text("Space the colour stops evenly, keeping their order")
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
    // A selected gradient fill it changed: one undo step once let go.
    app.settings_record(ctx.input(|i| i.pointer.any_down()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_gradient_edits_as_fixed_colours_and_back() {
        let mut stops = GradientStops::from_colors(&[(0.0, [10, 20, 30]), (1.0, [40, 50, 60])]);
        stops.colors[0].mid = 0.3;
        stops.opacity = vec![OpacityStop {
            pos: 0.5,
            opacity: 0.25,
            mid: 0.5,
        }];
        let g = editable(&stops);
        assert!(matches!(g.stops[0].color, StopColor::Fixed([10, 20, 30])));
        assert_eq!(g.resolve(Color32::RED, Color32::BLUE), stops);
    }
}
