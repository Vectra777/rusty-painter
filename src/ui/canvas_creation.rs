use crate::{
    BackgroundChoice, CanvasUnit, ColorDepth, ColorModel, NewCanvasSettings, Orientation,
    PainterApp,
};
use eframe::egui;

/// Modal dialog to configure and create a new canvas, inspired by Krita's new file window.
pub fn canvas_creation_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.show_new_canvas_modal {
        return;
    }

    let mut open = app.show_new_canvas_modal;
    egui::Window::new("New Canvas")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let settings: &mut NewCanvasSettings = &mut app.new_canvas;

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
                        .range(1.0..=50000.0)
                        .suffix(settings.unit.label()),
                );
                ui.label("Height");
                ui.add(
                    egui::DragValue::new(&mut settings.height)
                        .speed(1.0)
                        .range(1.0..=50000.0)
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
                        .range(1.0..=1200.0),
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
                        ColorModel::Cmyk => "CMYK",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.color_model, ColorModel::Rgba, "RGBA");
                        ui.selectable_value(
                            &mut settings.color_model,
                            ColorModel::Grayscale,
                            "Grayscale",
                        );
                        ui.selectable_value(&mut settings.color_model, ColorModel::Cmyk, "CMYK");
                    });
                ui.label("Depth");
                egui::ComboBox::from_id_salt("color_depth")
                    .selected_text(match settings.color_depth {
                        ColorDepth::Bit8 => "8-bit integer",
                        ColorDepth::Bit16 => "16-bit integer",
                        ColorDepth::Float32 => "32-bit float",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut settings.color_depth,
                            ColorDepth::Bit8,
                            "8-bit integer",
                        );
                        ui.selectable_value(
                            &mut settings.color_depth,
                            ColorDepth::Bit16,
                            "16-bit integer",
                        );
                        ui.selectable_value(
                            &mut settings.color_depth,
                            ColorDepth::Float32,
                            "32-bit float",
                        );
                    });
            });
            ui.weak(
                "Grayscale paints in a single channel; CMYK converts selections into an on-screen approximation.",
            );

            let (px_w, px_h) = settings.dimensions_in_pixels();
            ui.label(format!(
                "Result: {} × {} px @ {:.0} dpi",
                px_w, px_h, settings.resolution
            ));

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Create").clicked() {
                    app.apply_new_canvas(ctx);
                    app.show_new_canvas_modal = false;
                }
                if ui.button("Cancel").clicked() {
                    app.show_new_canvas_modal = false;
                }
            });
        });

    app.show_new_canvas_modal = open;
}
