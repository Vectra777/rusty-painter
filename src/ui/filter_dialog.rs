//! The filter dialog: the settings of the filter being previewed, with OK
//! and Cancel (Enter and Esc).

use crate::PainterApp;
use crate::canvas::filters::{Filter, MAX_REACH};
use crate::ui::widgets::{percent_of_unit, slider_row};
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
        Filter::Invert | Filter::Desaturate => {}
    }
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
        app.canvas_mut().layers[idx].adjustment = Some(filter);
        app.mark_all_tiles_dirty();
        app.layer_state.thumbnails_dirty = true;
    }
    if done || !open {
        app.workspace.filter.editing = None;
    }
}
