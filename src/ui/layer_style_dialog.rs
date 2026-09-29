//! The fill layer and border dialogs: a layer's generated content or its
//! outline, changed live.

use crate::PainterApp;
use crate::canvas::gradient::GradientShape;
use crate::canvas::layer_style::{Border, LayerFill, MAX_BORDER};
use crate::canvas::storage::LayerId;
use crate::ui::style::*;
use crate::ui::widgets::{percent_of_unit, segmented, slider_row};
use eframe::egui::{self, RichText};

/// The layer `id` points at, or `None` (and the dialog closes) once it's
/// gone.
fn layer_of(app: &PainterApp, id: Option<LayerId>) -> Option<usize> {
    app.canvas.layer_index_of(id?)
}

/// A fill layer's colour or gradient.
pub fn fill_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(idx) = layer_of(app, app.workspace.filter.fill_editing) else {
        app.workspace.filter.fill_editing = None;
        return;
    };
    let mut style = app.canvas.layers[idx].style;
    let Some(mut fill) = style.fill else {
        app.workspace.filter.fill_editing = None;
        return;
    };
    let size = [app.canvas.width() as f32, app.canvas.height() as f32];
    let brush = app.brush_state.brush.brush_options.color;
    let title = format!("Fill: {}", app.canvas.layers[idx].name);
    let (mut open, mut done) = (true, false);
    egui::Window::new(title)
        .id(egui::Id::new("fill_dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            fill_settings(ui, &mut fill, size, brush);
            ui.label(
                RichText::new(
                    "Covers the canvas: add a mask to paint where it shows, or clip it \
                     to the layer below.",
                )
                .small()
                .color(TEXT_DIM),
            );
            ui.separator();
            done = ui.button("Done").clicked();
        });
    style.fill = Some(fill);
    app.workspace.filter.adjusting = ctx.input(|i| i.pointer.any_down());
    app.set_layer_style(idx, style);
    if done || !open {
        app.workspace.filter.fill_editing = None;
    }
}

fn fill_settings(ui: &mut egui::Ui, fill: &mut LayerFill, size: [f32; 2], brush: egui::Color32) {
    let [r, g, b, _] = brush.to_srgba_unmultiplied();
    let mut kind = matches!(fill, LayerFill::Gradient { .. });
    if segmented(
        ui,
        &mut kind,
        &[(false, "Colour"), (true, "Gradient")],
        false,
    ) {
        *fill = match *fill {
            LayerFill::Colour(c) => {
                let colours =
                    crate::canvas::filters::GradientMap::from_stops(&[(0.0, c), (1.0, [255; 3])]);
                LayerFill::Gradient {
                    colours,
                    shape: GradientShape::Linear,
                    start: [0.0, size[1] / 2.0],
                    end: [size[0], size[1] / 2.0],
                }
            }
            LayerFill::Gradient { colours, .. } => {
                LayerFill::Colour(colours.stops().first().map_or([r, g, b], |&(_, c)| c))
            }
        };
    }
    ui.add_space(4.0);
    match fill {
        LayerFill::Colour(colour) => {
            ui.horizontal(|ui| {
                ui.label("Colour");
                ui.color_edit_button_srgb(colour);
                if ui.button("Use brush colour").clicked() {
                    *colour = [r, g, b];
                }
            });
        }
        LayerFill::Gradient {
            colours,
            shape,
            start,
            end,
        } => {
            crate::ui::filter_dialog::gradient_map_settings(ui, colours);
            ui.add_space(4.0);
            let mut new_shape = *shape;
            segmented(
                ui,
                &mut new_shape,
                &[
                    (GradientShape::Linear, "Linear"),
                    (GradientShape::Radial, "Radial"),
                    (GradientShape::Reflected, "Reflected"),
                    (GradientShape::Angle, "Angle"),
                ],
                false,
            );
            let d = egui::vec2(end[0] - start[0], end[1] - start[1]);
            let mut angle = d.y.atan2(d.x).to_degrees().round();
            let turned = slider_row(
                ui,
                "Angle",
                egui::Slider::new(&mut angle, -180.0..=180.0).suffix("°"),
            )
            .changed();
            if turned || new_shape != *shape {
                *shape = new_shape;
                (*start, *end) = across(*shape, angle.to_radians(), size);
            }
        }
    }
}

/// A gradient's ends for `shape` turned by `angle` across a canvas of
/// `size`: linear from edge to edge, the others from the centre.
fn across(shape: GradientShape, angle: f32, size: [f32; 2]) -> ([f32; 2], [f32; 2]) {
    let (w, h) = (size[0], size[1]);
    let c = egui::vec2(w / 2.0, h / 2.0);
    let dir = egui::vec2(angle.cos(), angle.sin());
    // Half the canvas's extent along the direction.
    let half = (w * dir.x.abs() + h * dir.y.abs()) / 2.0;
    let (from, to) = match shape {
        GradientShape::Linear => (c - dir * half, c + dir * half),
        GradientShape::Reflected => (c, c + dir * half),
        GradientShape::Radial => (c, c + dir * (w.hypot(h) / 2.0)),
        GradientShape::Angle => (c, c + dir * half.max(1.0)),
    };
    ([from.x, from.y], [to.x, to.y])
}

/// A layer's border: on or off, width, colour and opacity.
pub fn border_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(idx) = layer_of(app, app.workspace.filter.border_editing) else {
        app.workspace.filter.border_editing = None;
        return;
    };
    let mut style = app.canvas.layers[idx].style;
    let title = format!("Border: {}", app.canvas.layers[idx].name);
    let (mut open, mut done) = (true, false);
    egui::Window::new(title)
        .id(egui::Id::new("border_dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .show(ctx, |ui| {
            let mut on = style.border.is_some();
            if ui.checkbox(&mut on, "Border around the paint").changed() {
                style.border = on.then(Border::default);
            }
            if let Some(border) = &mut style.border {
                slider_row(
                    ui,
                    "Width",
                    egui::Slider::new(&mut border.width, 1.0..=MAX_BORDER)
                        .logarithmic(true)
                        .suffix(" px"),
                );
                slider_row(
                    ui,
                    "Opacity",
                    percent_of_unit(egui::Slider::new(&mut border.opacity, 0.0..=1.0)),
                );
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    ui.color_edit_button_srgb(&mut border.colour);
                });
            }
            ui.label(
                RichText::new(
                    "Drawn under the layer's paint and follows it as you paint. \
                     Merging or exporting keeps it as pixels.",
                )
                .small()
                .color(TEXT_DIM),
            );
            ui.separator();
            done = ui.button("Done").clicked();
        });
    app.workspace.filter.adjusting = ctx.input(|i| i.pointer.any_down());
    app.set_layer_style(idx, style);
    if done || !open {
        app.workspace.filter.border_editing = None;
    }
}

/// Layer → Vector → Line Width: every line of a vector layer thicker or
/// thinner, shown as the slider moves.
pub fn line_width_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some((id, _, scale)) = &app.workspace.vector.width_editing else {
        return;
    };
    let (id, mut new_scale) = (*id, *scale);
    let Some(idx) = app.canvas.layer_index_of(id) else {
        app.workspace.vector.width_editing = None;
        return;
    };
    let title = format!("Line Width: {}", app.canvas.layers[idx].name);
    let (mut open, mut ok, mut cancel) = (true, false, false);
    egui::Window::new(title)
        .id(egui::Id::new("line_width_dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(300.0)
        .show(ctx, |ui| {
            slider_row(
                ui,
                "Width",
                egui::Slider::new(&mut new_scale, 0.1..=5.0)
                    .logarithmic(true)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                    .custom_parser(|t| {
                        t.trim()
                            .trim_end_matches('%')
                            .parse::<f64>()
                            .ok()
                            .map(|v| v / 100.0)
                    }),
            );
            ui.label(
                RichText::new("Every line on the layer, thicker or thinner, pressure kept.")
                    .small()
                    .color(TEXT_DIM),
            );
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if new_scale != *scale_of(app) {
        app.line_width_set(new_scale);
    }
    let (enter, esc) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if ok || enter {
        app.line_width_done(true);
    } else if cancel || esc || !open {
        app.line_width_done(false);
    }
}

fn scale_of(app: &PainterApp) -> &f32 {
    app.workspace
        .vector
        .width_editing
        .as_ref()
        .map_or(&1.0, |(_, _, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turned_linear_fill_spans_the_canvas() {
        let (from, to) = across(GradientShape::Linear, 0.0, [200.0, 100.0]);
        assert_eq!((from, to), ([0.0, 50.0], [200.0, 50.0]));
        let (from, to) = across(
            GradientShape::Linear,
            std::f32::consts::FRAC_PI_2,
            [200.0, 100.0],
        );
        assert!((from[1] - 0.0).abs() < 1e-3 && (to[1] - 100.0).abs() < 1e-3);
        // Radial: from the centre out to a corner's distance.
        let (from, to) = across(GradientShape::Radial, 0.0, [200.0, 100.0]);
        assert_eq!(from, [100.0, 50.0]);
        assert!((to[0] - from[0] - 200f32.hypot(100.0) / 2.0).abs() < 1e-3);
    }
}
