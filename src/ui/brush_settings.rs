use crate::brush_engine::brush::{Brush, BrushType, StabilizerAlgorithm};
use crate::brush_engine::brush_options::{BlendMode, PixelBrushShape};
use crate::brush_engine::hardness::SoftnessSelector;
use crate::ui::{brush_preview::render_preview, curve_editor::curve_editor};
use eframe::egui::{self, Color32};
use rayon::ThreadPool;

pub struct BrushPreviewState {
    pub size: [usize; 2],
    pub texture: Option<egui::TextureHandle>,
    pub dirty: bool,
}

impl Default for BrushPreviewState {
    fn default() -> Self {
        Self {
            size: [200, 80],
            texture: None,
            dirty: true,
        }
    }
}

/// Panel for tweaking the currently selected brush properties.
pub fn brush_settings_panel(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    preview: &mut BrushPreviewState,
    pool: &ThreadPool,
    loaded_tips: &[(String, PixelBrushShape, Option<egui::TextureHandle>)],
) {
    let mut mask_dirty = false;

    ui.heading("Brush Properties");
    ui.separator();

    // --- Preview Area ---
    ui.collapsing("Preview", |ui| {
        if preview.dirty {
            render_preview(preview, brush, pool, ui.ctx());
            preview.dirty = false;
        }

        if let Some(texture) = &preview.texture {
            ui.image((texture.id(), texture.size_vec2()));
        }
    });
    ui.separator();
    // --------------------

    ui.horizontal(|ui| {
        ui.label("Type:");
        if ui
            .selectable_value(&mut brush.brush_type, BrushType::Soft, "Soft")
            .changed()
        {
            preview.dirty = true;
        }
        if ui
            .selectable_value(&mut brush.brush_type, BrushType::Pixel, "Pixel")
            .changed()
        {
            preview.dirty = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Mode:");
        if ui
            .selectable_value(
                &mut brush.brush_options.blend_mode,
                BlendMode::Normal,
                "Normal",
            )
            .changed()
        {
            preview.dirty = true;
        }
        if ui
            .selectable_value(
                &mut brush.brush_options.blend_mode,
                BlendMode::Eraser,
                "Eraser",
            )
            .changed()
        {
            preview.dirty = true;
        }
    });

    ui.add_space(5.0);

    ui.label("Brush Tip:");
    egui::ScrollArea::vertical()
        .id_salt("pixel_tip_selector")
        .max_height(120.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let size = egui::vec2(32.0, 32.0);

                // Circle
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                let is_selected =
                    matches!(brush.brush_options.pixel_shape, PixelBrushShape::Circle);
                ui.painter().rect_stroke(
                    rect,
                    1.0,
                    (
                        1.0,
                        if is_selected {
                            Color32::WHITE
                        } else {
                            Color32::GRAY
                        },
                    ),
                );
                ui.painter()
                    .circle_filled(rect.center(), 12.0, Color32::WHITE);
                if response.on_hover_text("Circle").clicked() {
                    brush.brush_options.pixel_shape = PixelBrushShape::Circle;
                    preview.dirty = true;
                }

                // Square
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                let is_selected =
                    matches!(brush.brush_options.pixel_shape, PixelBrushShape::Square);
                ui.painter().rect_stroke(
                    rect,
                    1.0,
                    (
                        1.0,
                        if is_selected {
                            Color32::WHITE
                        } else {
                            Color32::GRAY
                        },
                    ),
                );
                ui.painter()
                    .rect_filled(rect.shrink(4.0), 0.0, Color32::WHITE);
                if response.on_hover_text("Square").clicked() {
                    brush.brush_options.pixel_shape = PixelBrushShape::Square;
                    preview.dirty = true;
                }

                // Custom tips
                for (name, shape, texture_opt) in loaded_tips {
                    if let Some(texture) = texture_opt {
                        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                        let is_selected = &brush.brush_options.pixel_shape == shape;

                        ui.painter().rect_stroke(
                            rect,
                            1.0,
                            (
                                1.0,
                                if is_selected {
                                    Color32::WHITE
                                } else {
                                    Color32::GRAY
                                },
                            ),
                        );
                        ui.painter().image(
                            texture.id(),
                            rect.shrink(2.0),
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );

                        if response.on_hover_text(name).clicked() {
                            brush.brush_options.pixel_shape = shape.clone();
                            preview.dirty = true;
                        }
                    }
                }
            });
        });
    ui.add_space(5.0);

    ui.label("Size:");
    if ui
        .add(egui::Slider::new(&mut brush.brush_options.diameter, 1.0..=3000.0).logarithmic(true))
        .changed()
    {
        mask_dirty = true;
        preview.dirty = true;
    }

    if brush.brush_type == BrushType::Soft {
        ui.horizontal(|ui| {
            ui.label("Softness:");
            if ui
                .selectable_value(
                    &mut brush.brush_options.softness_selector,
                    SoftnessSelector::Gaussian,
                    "Gaussian",
                )
                .changed()
            {
                mask_dirty = true;
                preview.dirty = true;
            }
            if ui
                .selectable_value(
                    &mut brush.brush_options.softness_selector,
                    SoftnessSelector::Curve,
                    "Curve",
                )
                .changed()
            {
                mask_dirty = true;
                preview.dirty = true;
            }
        });

        match brush.brush_options.softness_selector {
            SoftnessSelector::Gaussian => {
                ui.label("Hardness:");
                if ui
                    .add(egui::Slider::new(
                        &mut brush.brush_options.hardness,
                        0.0..=100.0,
                    ))
                    .changed()
                {
                    mask_dirty = true;
                    preview.dirty = true;
                }
            }
            SoftnessSelector::Curve => {
                ui.label("Softness Curve:");
                if curve_editor(ui, &mut brush.brush_options.softness_curve) {
                    mask_dirty = true;
                    preview.dirty = true;
                }
                ui.small("Double-click to add/remove points.");
            }
        }
    }

    ui.label("Opacity:");
    if ui
        .add(egui::Slider::new(
            &mut brush.brush_options.opacity,
            0.0..=1.0,
        ))
        .changed()
    {
        preview.dirty = true;
    }

    ui.label("Flow:");
    if ui
        .add(egui::Slider::new(
            &mut brush.brush_options.flow,
            0.0..=100.0,
        ))
        .changed()
    {
        preview.dirty = true;
    }

    ui.label("Spacing (%):");
    if ui
        .add(egui::Slider::new(
            &mut brush.brush_options.spacing,
            1.0..=200.0,
        ))
        .changed()
    {
        preview.dirty = true;
    }

    ui.label("Jitter (% of size):");
    if ui
        .add(egui::Slider::new(&mut brush.jitter, 0.0..=50.0))
        .changed()
    {
        preview.dirty = true;
    }

    ui.label("Stabilizer:");
    ui.horizontal(|ui| {
        if ui
            .selectable_value(
                &mut brush.stabilizer_algorithm,
                StabilizerAlgorithm::None,
                "None",
            )
            .changed()
        {
            preview.dirty = true;
        }
        if ui
            .selectable_value(
                &mut brush.stabilizer_algorithm,
                StabilizerAlgorithm::Simple,
                "Simple",
            )
            .changed()
        {
            preview.dirty = true;
        }
        if ui
            .selectable_value(
                &mut brush.stabilizer_algorithm,
                StabilizerAlgorithm::Dynamic,
                "Dynamic",
            )
            .changed()
        {
            preview.dirty = true;
        }
    });

    match brush.stabilizer_algorithm {
        StabilizerAlgorithm::None => {}
        StabilizerAlgorithm::Simple => {
            if ui
                .add(egui::Slider::new(&mut brush.stabilizer, 0.0..=1.0).text("Strength"))
                .changed()
            {
                preview.dirty = true;
            }
        }
        StabilizerAlgorithm::Dynamic => {
            if ui
                .add(egui::Slider::new(&mut brush.stabilizer_mass, 0.01..=1.0).text("Mass"))
                .changed()
            {
                preview.dirty = true;
            }
            if ui
                .add(egui::Slider::new(&mut brush.stabilizer_drag, 0.0..=1.0).text("Drag"))
                .changed()
            {
                preview.dirty = true;
            }
        }
    }

    ui.separator();
    if ui
        .checkbox(&mut brush.pixel_perfect, "Pixel Perfect Mode")
        .changed()
    {
        preview.dirty = true;
    }
    if ui
        .checkbox(&mut brush.anti_aliasing, "Anti-aliasing")
        .changed()
    {
        preview.dirty = true;
    }

    if mask_dirty {
        brush.is_changed = true;
    }
}
