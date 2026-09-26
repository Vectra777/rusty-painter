use crate::app::state::{MAX_CANVAS_DIMENSION, MAX_CANVAS_DPI};
use crate::canvas::blend_modes::BlendSpace;
use crate::{BackgroundChoice, CanvasUnit, ColorModel, NewCanvasSettings, Orientation, PainterApp};
use eframe::egui;

/// Modal dialog to configure and create a new canvas, inspired by Krita's new file window.
pub fn canvas_creation_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_new_canvas_modal {
        return;
    }

    let mut open = app.modal_state.show_new_canvas_modal;
    egui::Window::new("New Canvas")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let settings: &mut NewCanvasSettings = &mut app.modal_state.new_canvas;

            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut settings.name);
            });

            ui.separator();
            ui.heading("Dimensions");
            ui.horizontal(|ui| {
                ui.label("Width");
                ui.add(
                    egui::DragValue::new(&mut settings.width)
                        .speed(1.0)
                        .range(1.0..=MAX_CANVAS_DIMENSION as f32)
                        .suffix(settings.unit.label()),
                );
                ui.label("Height");
                ui.add(
                    egui::DragValue::new(&mut settings.height)
                        .speed(1.0)
                        .range(1.0..=MAX_CANVAS_DIMENSION as f32)
                        .suffix(settings.unit.label()),
                );
                egui::ComboBox::from_label("Units")
                    .selected_text(settings.unit.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.unit, CanvasUnit::Pixels, "Pixels");
                        ui.selectable_value(&mut settings.unit, CanvasUnit::Inches, "Inches");
                        ui.selectable_value(
                            &mut settings.unit,
                            CanvasUnit::Millimeters,
                            "Millimeters",
                        );
                        ui.selectable_value(
                            &mut settings.unit,
                            CanvasUnit::Centimeters,
                            "Centimeters",
                        );
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Resolution (DPI)");
                ui.add(
                    egui::DragValue::new(&mut settings.resolution)
                        .speed(1.0)
                        .range(1.0..=MAX_CANVAS_DPI),
                );
                let mut orientation_changed = false;
                orientation_changed |= ui
                    .selectable_value(&mut settings.orientation, Orientation::Portrait, "Portrait")
                    .changed();
                orientation_changed |= ui
                    .selectable_value(
                        &mut settings.orientation,
                        Orientation::Landscape,
                        "Landscape",
                    )
                    .changed();
                if orientation_changed {
                    std::mem::swap(&mut settings.width, &mut settings.height);
                }
            });

            ui.separator();
            ui.heading("Color");
            ui.horizontal(|ui| {
                ui.label("Background");
                ui.radio_value(&mut settings.background, BackgroundChoice::White, "White");
                ui.radio_value(&mut settings.background, BackgroundChoice::Black, "Black");
                ui.radio_value(
                    &mut settings.background,
                    BackgroundChoice::Transparent,
                    "Transparent",
                );
                ui.radio_value(&mut settings.background, BackgroundChoice::Custom, "Custom");
                if settings.background == BackgroundChoice::Custom {
                    ui.color_edit_button_srgba(&mut settings.custom_bg);
                }
            });

            ui.horizontal(|ui| {
                ui.label("Color Model");
                egui::ComboBox::from_id_salt("color_model")
                    .selected_text(match settings.color_model {
                        ColorModel::Rgba => "RGBA",
                        ColorModel::Grayscale => "Grayscale",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.color_model, ColorModel::Rgba, "RGBA");
                        ui.selectable_value(
                            &mut settings.color_model,
                            ColorModel::Grayscale,
                            "Grayscale",
                        );
                    });
            });
            ui.weak("Grayscale paints in a single channel.");

            ui.horizontal(|ui| {
                ui.label("Color blending");
                blend_space_picker(ui, &mut settings.blend_space);
            });
            ui.weak(blend_space_hint(settings.blend_space));

            let validation = settings.validated_dimensions();
            match validation {
                Ok((px_w, px_h)) => {
                    ui.label(format!(
                        "Result: {} × {} px @ {:.0} dpi",
                        px_w, px_h, settings.resolution
                    ));
                }
                Err(ref msg) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, msg);
                }
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(validation.is_ok(), egui::Button::new("Create"))
                    .clicked()
                {
                    app.apply_new_canvas();
                    app.modal_state.show_new_canvas_modal = false;
                }
                if ui.button("Cancel").clicked() {
                    app.modal_state.show_new_canvas_modal = false;
                }
            });
        });

    app.modal_state.show_new_canvas_modal = open;
}

/// Linear / Gamma choice for how a document blends colours.
pub(crate) fn blend_space_picker(ui: &mut egui::Ui, space: &mut BlendSpace) -> bool {
    let mut changed = false;
    changed |= ui
        .selectable_value(space, BlendSpace::Linear, "Linear light")
        .changed();
    changed |= ui
        .selectable_value(space, BlendSpace::Gamma, "Gamma (Krita / Photoshop)")
        .changed();
    changed
}

pub(crate) fn blend_space_hint(space: BlendSpace) -> &'static str {
    match space {
        BlendSpace::Linear => {
            "Physically correct mixing: bright, clean colour blends and lighter soft edges."
        }
        BlendSpace::Gamma => {
            "Mixes stored sRGB values like Krita and Photoshop: familiar soft-brush falloff and blend modes."
        }
    }
}
