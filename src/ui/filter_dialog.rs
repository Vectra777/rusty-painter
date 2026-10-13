//! The filter dialog: the settings of the filter being previewed, with OK
//! and Cancel (Enter and Esc).

use crate::PainterApp;
use crate::canvas::filters::{Filter, GradientMap, MAP_PRESETS, MAX_REACH, ToneCurve};
use crate::ui::bar_slider::BarSlider;
use crate::ui::curve_editor::{CurvePreset, curve_editor_with};
use crate::ui::widgets::FitScreen;
use crate::ui::widgets::{percent_of_unit, segmented, slider_row};
use eframe::egui::{self, Key};

pub fn filter_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(session) = app.workspace.filter.session.as_ref() else {
        return;
    };
    let mut filter = session.filter.clone();
    let mut open = true;
    let (mut ok, mut cancel) = (false, false);
    let mut gradients = Gradients::of(app);
    egui::Window::new(filter.name())
        .fit_screen(ctx)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            settings(ui, &mut filter, &mut gradients);
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if let Some(session) = app.workspace.filter.session.as_mut()
        && filter != session.filter
    {
        session.filter = filter;
        session.dirty = true;
    }
    let (enter, esc) = ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    if ok || enter {
        app.filter_commit();
    } else if cancel || esc || !open {
        app.filter_cancel();
    }
}

pub(crate) fn settings(ui: &mut egui::Ui, filter: &mut Filter, gradients: &mut Gradients<'_>) {
    let reach = MAX_REACH as f32;
    match filter {
        Filter::BrightnessContrast {
            brightness,
            contrast,
        } => {
            slider_row(
                ui,
                "Brightness",
                percent_of_unit(BarSlider::new(brightness, -1.0..=1.0)),
            );
            slider_row(
                ui,
                "Contrast",
                percent_of_unit(BarSlider::new(contrast, -1.0..=1.0)),
            );
        }
        Filter::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            slider_row(ui, "Hue", BarSlider::new(hue, -180.0..=180.0).suffix("°"));
            slider_row(
                ui,
                "Saturation",
                percent_of_unit(BarSlider::new(saturation, -1.0..=1.0)),
            );
            slider_row(
                ui,
                "Lightness",
                percent_of_unit(BarSlider::new(lightness, -1.0..=1.0)),
            );
        }
        Filter::Levels {
            black,
            white,
            gamma,
        } => {
            slider_row(
                ui,
                "Black point",
                percent_of_unit(BarSlider::new(black, 0.0..=1.0)),
            );
            slider_row(
                ui,
                "White point",
                percent_of_unit(BarSlider::new(white, 0.0..=1.0)),
            );
            slider_row(
                ui,
                "Midtones",
                BarSlider::new(gamma, 0.1..=5.0).logarithmic(true),
            );
            *white = white.max(*black + 0.01);
        }
        Filter::Posterize { levels } => {
            slider_row(ui, "Levels", BarSlider::new(levels, 2..=32));
        }
        Filter::Threshold { level } => {
            slider_row(
                ui,
                "Level",
                percent_of_unit(BarSlider::new(level, 0.0..=1.0)),
            );
        }
        Filter::GaussianBlur { radius } => {
            slider_row(
                ui,
                "Radius",
                BarSlider::new(radius, 0.0..=reach / 3.0)
                    .logarithmic(true)
                    .smallest_positive(0.3)
                    .max_decimals(1)
                    .suffix(" px"),
            );
        }
        Filter::MotionBlur { angle, distance } => {
            slider_row(
                ui,
                "Angle",
                BarSlider::new(angle, -180.0..=180.0).suffix("°"),
            );
            slider_row(
                ui,
                "Distance",
                BarSlider::new(distance, 0.0..=reach)
                    .logarithmic(true)
                    .smallest_positive(1.0)
                    .max_decimals(0)
                    .suffix(" px"),
            );
        }
        Filter::Sharpen { radius, amount } => {
            slider_row(
                ui,
                "Radius",
                BarSlider::new(radius, 0.3..=reach / 6.0)
                    .logarithmic(true)
                    .max_decimals(1)
                    .suffix(" px"),
            );
            slider_row(
                ui,
                "Amount",
                percent_of_unit(BarSlider::new(amount, 0.0..=5.0)),
            );
        }
        Filter::Noise { amount, mono, size } => {
            slider_row(
                ui,
                "Amount",
                percent_of_unit(BarSlider::new(amount, 0.0..=1.0)),
            );
            slider_row(
                ui,
                "Size",
                BarSlider::new(size, 1.0..=64.0)
                    .logarithmic(true)
                    .max_decimals(1)
                    .suffix(" px"),
            )
            .on_hover_text("How big each grain is");
            ui.checkbox(mono, "Monochrome");
        }
        Filter::Pixelate { size } => {
            slider_row(
                ui,
                "Cell size",
                BarSlider::new(size, 1..=MAX_REACH as u32)
                    .logarithmic(true)
                    .suffix(" px"),
            );
        }
        Filter::LineArt {
            black,
            white,
            keep_color,
        } => {
            ui.label("Dark pixels stay, light ones turn transparent: for scanned or photographed drawings.");
            slider_row(
                ui,
                "Lines up to",
                percent_of_unit(BarSlider::new(black, 0.0..=1.0)),
            )
            .on_hover_text("As dark as this or darker stays fully opaque");
            slider_row(
                ui,
                "Paper from",
                percent_of_unit(BarSlider::new(white, 0.0..=1.0)),
            )
            .on_hover_text("As light as this or lighter disappears");
            *white = white.max(*black + 0.01);
            ui.checkbox(keep_color, "Keep the lines' colour")
                .on_hover_text("Off: the lines become black");
        }
        Filter::Curves {
            rgb,
            red,
            green,
            blue,
        } => curves_settings(ui, [rgb, red, green, blue]),
        Filter::ColourBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => {
            let id = ui.id().with("colour_balance_range");
            let mut range: usize = ui.data(|d| d.get_temp(id)).unwrap_or(1);
            segmented(
                ui,
                &mut range,
                &[(0, "Shadows"), (1, "Midtones"), (2, "Highlights")],
                false,
            );
            ui.data_mut(|d| d.insert_temp(id, range));
            let values = match range {
                0 => shadows,
                1 => midtones,
                _ => highlights,
            };
            for (value, (from, to)) in
                values
                    .iter_mut()
                    .zip([("Cyan", "Red"), ("Magenta", "Green"), ("Yellow", "Blue")])
            {
                ui.horizontal(|ui| {
                    ui.add_sized([56.0, 18.0], egui::Label::new(from));
                    ui.add(
                        BarSlider::new(value, -1.0..=1.0)
                            .custom_formatter(|v, _| format!("{:+.0}", v * 100.0))
                            .custom_parser(|t| t.trim().parse::<f64>().ok().map(|v| v / 100.0)),
                    );
                    ui.label(to);
                });
            }
            ui.checkbox(preserve_luminosity, "Preserve luminosity")
                .on_hover_text("Shift the colours without making them lighter or darker");
        }
        Filter::GradientMap(map) => gradient_map_settings(ui, map, gradients),
        Filter::Exposure { stops } => {
            slider_row(
                ui,
                "Exposure",
                BarSlider::new(stops, -4.0..=4.0)
                    .suffix(" stops")
                    .max_decimals(2),
            );
        }
        Filter::Temperature { temperature, tint } => {
            slider_row(
                ui,
                "Cool ↔ Warm",
                percent_of_unit(BarSlider::new(temperature, -1.0..=1.0)),
            );
            slider_row(
                ui,
                "Magenta ↔ Green",
                percent_of_unit(BarSlider::new(tint, -1.0..=1.0)),
            );
        }
        Filter::Vibrance { amount } => {
            slider_row(
                ui,
                "Vibrance",
                percent_of_unit(BarSlider::new(amount, -1.0..=1.0)),
            )
            .on_hover_text("Saturation that lifts dull colours most and leaves rich ones");
        }
        Filter::Sepia { amount } => {
            slider_row(
                ui,
                "Amount",
                percent_of_unit(BarSlider::new(amount, 0.0..=1.0)),
            );
        }
        Filter::Solarize { level } => {
            slider_row(
                ui,
                "From",
                percent_of_unit(BarSlider::new(level, 0.0..=1.0)),
            )
            .on_hover_text("Tones brighter than this are inverted");
        }
        Filter::Glow {
            radius,
            strength,
            threshold,
        } => {
            slider_row(
                ui,
                "Radius",
                BarSlider::new(radius, 1.0..=reach / 3.0)
                    .logarithmic(true)
                    .max_decimals(1)
                    .suffix(" px"),
            );
            slider_row(
                ui,
                "Strength",
                percent_of_unit(BarSlider::new(strength, 0.0..=2.0)),
            );
            slider_row(
                ui,
                "From brightness",
                percent_of_unit(BarSlider::new(threshold, 0.0..=0.95)),
            )
            .on_hover_text("Only what's brighter than this glows");
        }
        Filter::ChromaticAberration { amount, .. } => {
            slider_row(
                ui,
                "Amount",
                BarSlider::new(amount, 0.0..=40.0)
                    .max_decimals(1)
                    .suffix(" px"),
            )
            .on_hover_text("How far red and blue part at the corners; none in the middle");
        }
        Filter::Halftone {
            size,
            angle,
            colour,
        } => {
            slider_row(
                ui,
                "Dot size",
                BarSlider::new(size, 2.0..=64.0)
                    .logarithmic(true)
                    .suffix(" px"),
            );
            slider_row(ui, "Angle", BarSlider::new(angle, 0.0..=90.0).suffix("°"));
            ui.checkbox(colour, "Coloured dots");
        }
        Filter::Emboss { angle, depth } => {
            slider_row(
                ui,
                "Light from",
                BarSlider::new(angle, -180.0..=180.0).suffix("°"),
            );
            slider_row(
                ui,
                "Depth",
                BarSlider::new(depth, 0.2..=10.0).logarithmic(true),
            );
        }
        Filter::Clouds { scale, detail } => {
            ui.label("Replaces the layer (or selection) with grey clouds.");
            slider_row(
                ui,
                "Scale",
                BarSlider::new(scale, 8.0..=1024.0)
                    .logarithmic(true)
                    .suffix(" px"),
            );
            slider_row(ui, "Detail", BarSlider::new(detail, 1..=8));
        }
        Filter::Median { radius } => {
            slider_row(ui, "Radius", BarSlider::new(radius, 1..=12).suffix(" px"));
        }
        Filter::OilPaint { radius } => {
            slider_row(
                ui,
                "Brush size",
                BarSlider::new(radius, 1..=16).suffix(" px"),
            );
        }
        Filter::Vignette { amount, size, .. } => {
            slider_row(
                ui,
                "Amount",
                percent_of_unit(BarSlider::new(amount, -1.0..=1.0)),
            )
            .on_hover_text("Darker edges; below zero, lighter");
            slider_row(
                ui,
                "Starts at",
                percent_of_unit(BarSlider::new(size, 0.0..=0.95)),
            )
            .on_hover_text("How far from the middle it begins");
        }
        Filter::ZoomBlur { amount, .. } => {
            slider_row(
                ui,
                "Amount",
                percent_of_unit(BarSlider::new(amount, 0.0..=1.0)),
            );
        }
        Filter::SpinBlur { angle, .. } => {
            slider_row(ui, "Angle", BarSlider::new(angle, 0.0..=90.0).suffix("°"));
        }
        Filter::Dither { levels } => {
            slider_row(ui, "Levels", BarSlider::new(levels, 2..=16))
                .on_hover_text("Shades per channel");
        }
        Filter::Invert | Filter::Desaturate | Filter::FindEdges => {}
    }
}

/// Quick tone curves.
const TONE_PRESETS: [CurvePreset; 5] = [
    ("Linear", &[(0.0, 0.0), (1.0, 1.0)]),
    ("Lighten", &[(0.0, 0.0), (0.45, 0.62), (1.0, 1.0)]),
    ("Darken", &[(0.0, 0.0), (0.55, 0.38), (1.0, 1.0)]),
    (
        "Contrast",
        &[(0.0, 0.0), (0.25, 0.17), (0.75, 0.83), (1.0, 1.0)],
    ),
    ("Invert", &[(0.0, 1.0), (1.0, 0.0)]),
];

/// The master curve and one per channel, picked with tabs.
fn curves_settings(ui: &mut egui::Ui, curves: [&mut ToneCurve; 4]) {
    let id = ui.id().with("curves_channel");
    let mut channel: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    segmented(
        ui,
        &mut channel,
        &[(0, "RGB"), (1, "Red"), (2, "Green"), (3, "Blue")],
        false,
    );
    ui.data_mut(|d| d.insert_temp(id, channel));
    let [rgb, red, green, blue] = curves;
    let curve = match channel {
        0 => rgb,
        1 => red,
        2 => green,
        _ => blue,
    };
    let mut editable = curve.to_softness();
    if curve_editor_with(ui, &mut editable, &TONE_PRESETS) {
        // More points than a curve holds: the newest one doesn't stay.
        editable
            .points
            .truncate(crate::canvas::filters::CURVE_POINTS);
        *curve = ToneCurve::from_softness(&editable);
    }
}

/// The gradients a gradient map can start from, and the brush colours
/// they may use.
pub(crate) struct Gradients<'a> {
    pub library: &'a mut crate::app::tools::gradient_colors::GradientLibrary,
    pub colors: (egui::Color32, egui::Color32),
}

impl<'a> Gradients<'a> {
    pub fn of(app: &'a mut PainterApp) -> Self {
        Self {
            colors: (
                app.brush_state.brush.brush_options.color,
                app.brush_state.secondary_color,
            ),
            library: &mut app.workspace.gradient.library,
        }
    }
}

/// The map's colours, shadows to highlights, in the gradient editor, and
/// the ready-made maps.
pub(crate) fn gradient_map_settings(
    ui: &mut egui::Ui,
    map: &mut GradientMap,
    gradients: &mut Gradients<'_>,
) {
    if map.colors.is_empty() {
        *map = GradientMap::default();
    }
    crate::ui::gradient_editor::edit_document_gradient(
        ui,
        "gradient map",
        map,
        gradients.library,
        gradients.colors,
        false,
    );
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Shadows").small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("Highlights").small());
        });
    });
    ui.horizontal_wrapped(|ui| {
        for (name, stops) in MAP_PRESETS {
            if ui.small_button(name).clicked() {
                *map = GradientMap::from_colors(stops);
            }
        }
    });
}

/// An adjustment layer's settings, applied live.
pub fn adjustment_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(id) = app.workspace.filter.editing else {
        return;
    };
    let Some(idx) = app.canvas.layer_index_of(id) else {
        app.workspace.filter.editing = None;
        return;
    };
    let Some(mut filter) = app.canvas.layers[idx].adjustment.clone() else {
        app.workspace.filter.editing = None;
        return;
    };
    let title = format!("Adjustment: {}", app.canvas.layers[idx].name);
    let mut open = true;
    let mut done = false;
    egui::Window::new(title)
        .fit_screen(ctx)
        .id(egui::Id::new("adjustment_dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            settings(ui, &mut filter, &mut Gradients::of(app));
            ui.label(
                egui::RichText::new(
                    "Applies to everything below. Add a layer mask to paint where; use the \
                     layer's opacity to soften it.",
                )
                .small()
                .color(crate::ui::style::TEXT_DIM),
            );
            ui.separator();
            done = ui.button("Done").clicked();
        });
    if app.canvas.layers[idx].adjustment.as_ref() != Some(&filter) {
        // Dragged: the screen previews at its own resolution until let go.
        app.workspace.filter.adjusting = ctx.input(|i| i.pointer.any_down());
        app.canvas_mut().layers[idx].adjustment = Some(filter);
        app.mark_all_tiles_dirty();
        app.layer_state.thumbnails_dirty = true;
    }
    if done || !open {
        app.workspace.filter.editing = None;
    }
}
