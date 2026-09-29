//! The filter dialog: the settings of the filter being previewed, with OK
//! and Cancel (Enter and Esc).

use crate::PainterApp;
use crate::canvas::filters::{Filter, GradientMap, MAP_STOPS, MAX_REACH, ToneCurve};
use crate::ui::curve_editor::{CurvePreset, curve_editor_with};
use crate::ui::widgets::{paint_gradient_strip, percent_of_unit, segmented, slider_row};
use eframe::egui::{self, Key};

pub fn filter_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(session) = app.workspace.filter.session.as_mut() else {
        return;
    };
    let mut filter = session.filter;
    let mut open = true;
    let (mut ok, mut cancel) = (false, false);
    egui::Window::new(filter.name())
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            settings(ui, &mut filter);
            ui.separator();
            ui.horizontal(|ui| {
                ok = ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if filter != session.filter {
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

pub(crate) fn settings(ui: &mut egui::Ui, filter: &mut Filter) {
    let reach = MAX_REACH as f32;
    match filter {
        Filter::BrightnessContrast {
            brightness,
            contrast,
        } => {
            slider_row(
                ui,
                "Brightness",
                percent_of_unit(egui::Slider::new(brightness, -1.0..=1.0)),
            );
            slider_row(
                ui,
                "Contrast",
                percent_of_unit(egui::Slider::new(contrast, -1.0..=1.0)),
            );
        }
        Filter::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            slider_row(
                ui,
                "Hue",
                egui::Slider::new(hue, -180.0..=180.0).suffix("°"),
            );
            slider_row(
                ui,
                "Saturation",
                percent_of_unit(egui::Slider::new(saturation, -1.0..=1.0)),
            );
            slider_row(
                ui,
                "Lightness",
                percent_of_unit(egui::Slider::new(lightness, -1.0..=1.0)),
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
                percent_of_unit(egui::Slider::new(black, 0.0..=1.0)),
            );
            slider_row(
                ui,
                "White point",
                percent_of_unit(egui::Slider::new(white, 0.0..=1.0)),
            );
            slider_row(
                ui,
                "Midtones",
                egui::Slider::new(gamma, 0.1..=5.0).logarithmic(true),
            );
            *white = white.max(*black + 0.01);
        }
        Filter::Posterize { levels } => {
            slider_row(ui, "Levels", egui::Slider::new(levels, 2..=32));
        }
        Filter::Threshold { level } => {
            slider_row(
                ui,
                "Level",
                percent_of_unit(egui::Slider::new(level, 0.0..=1.0)),
            );
        }
        Filter::GaussianBlur { radius } => {
            slider_row(
                ui,
                "Radius",
                egui::Slider::new(radius, 0.0..=reach / 3.0)
                    .logarithmic(true)
                    .suffix(" px"),
            );
        }
        Filter::MotionBlur { angle, distance } => {
            slider_row(
                ui,
                "Angle",
                egui::Slider::new(angle, -180.0..=180.0).suffix("°"),
            );
            slider_row(
                ui,
                "Distance",
                egui::Slider::new(distance, 0.0..=reach)
                    .logarithmic(true)
                    .suffix(" px"),
            );
        }
        Filter::Sharpen { radius, amount } => {
            slider_row(
                ui,
                "Radius",
                egui::Slider::new(radius, 0.3..=reach / 6.0)
                    .logarithmic(true)
                    .suffix(" px"),
            );
            slider_row(
                ui,
                "Amount",
                percent_of_unit(egui::Slider::new(amount, 0.0..=5.0)),
            );
        }
        Filter::Noise { amount, mono } => {
            slider_row(
                ui,
                "Amount",
                percent_of_unit(egui::Slider::new(amount, 0.0..=1.0)),
            );
            ui.checkbox(mono, "Monochrome");
        }
        Filter::Pixelate { size } => {
            slider_row(
                ui,
                "Cell size",
                egui::Slider::new(size, 1..=MAX_REACH as u32)
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
                percent_of_unit(egui::Slider::new(black, 0.0..=1.0)),
            )
            .on_hover_text("As dark as this or darker stays fully opaque");
            slider_row(
                ui,
                "Paper from",
                percent_of_unit(egui::Slider::new(white, 0.0..=1.0)),
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
                        egui::Slider::new(value, -1.0..=1.0)
                            .custom_formatter(|v, _| format!("{:+.0}", v * 100.0))
                            .custom_parser(|t| t.trim().parse::<f64>().ok().map(|v| v / 100.0)),
                    );
                    ui.label(to);
                });
            }
            ui.checkbox(preserve_luminosity, "Preserve luminosity")
                .on_hover_text("Shift the colours without making them lighter or darker");
        }
        Filter::GradientMap(map) => gradient_map_settings(ui, map),
        Filter::Invert | Filter::Desaturate => {}
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

/// The map's colours as a strip, each stop's colour and place, and presets.
fn gradient_map_settings(ui: &mut egui::Ui, map: &mut GradientMap) {
    let stops: Vec<crate::canvas::gradient::Stop> = map
        .stops()
        .iter()
        .map(|&(pos, [r, g, b])| crate::canvas::gradient::Stop {
            pos,
            color: egui::Color32::from_rgb(r, g, b),
        })
        .collect();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::hover());
    paint_gradient_strip(ui.painter(), rect, &stops);
    ui.horizontal(|ui| {
        ui.label("Shadows");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label("Highlights");
        });
    });

    let mut remove = None;
    let len = map.stops().len();
    for i in 0..len {
        ui.horizontal(|ui| {
            let (pos, color) = &mut map.stops[i];
            ui.color_edit_button_srgb(color);
            ui.add(
                egui::Slider::new(pos, 0.0..=1.0)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                    .custom_parser(|t| {
                        t.trim()
                            .trim_end_matches('%')
                            .parse::<f64>()
                            .ok()
                            .map(|v| v / 100.0)
                    }),
            );
            if len > 2
                && ui
                    .small_button("✕")
                    .on_hover_text("Remove this colour")
                    .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        let mut stops = map.stops().to_vec();
        stops.remove(i);
        *map = GradientMap::from_stops(&stops);
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(len < MAP_STOPS, egui::Button::new("Add colour"))
            .on_hover_text("A colour half way along the widest gap")
            .clicked()
        {
            let stops = map.stops();
            let (mut at, mut gap) = (0.5, 0.0);
            for pair in stops.windows(2) {
                if pair[1].0 - pair[0].0 > gap {
                    gap = pair[1].0 - pair[0].0;
                    at = (pair[0].0 + pair[1].0) * 0.5;
                }
            }
            let c = map.color_at(at).map(|v| (v * 255.0).round() as u8);
            let mut stops = stops.to_vec();
            stops.push((at, c));
            *map = GradientMap::from_stops(&stops);
        }
        if ui.button("Reverse").clicked() {
            *map = map.reversed();
        }
    });
    map.sort();
    ui.horizontal_wrapped(|ui| {
        for (name, stops) in GradientMap::PRESETS {
            if ui.small_button(name).clicked() {
                *map = GradientMap::from_stops(stops);
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
    let Some(mut filter) = app.canvas.layers[idx].adjustment else {
        app.workspace.filter.editing = None;
        return;
    };
    let title = format!("Adjustment: {}", app.canvas.layers[idx].name);
    let mut open = true;
    let mut done = false;
    egui::Window::new(title)
        .id(egui::Id::new("adjustment_dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            settings(ui, &mut filter);
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
    if app.canvas.layers[idx].adjustment != Some(filter) {
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
