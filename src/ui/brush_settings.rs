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

/// Adds a slider built by the caller and marks the preview dirty when it changes.
fn dirty_slider(ui: &mut egui::Ui, slider: egui::Slider, preview: &mut BrushPreviewState) -> bool {
    let changed = ui.add(slider).changed();
    if changed {
        preview.dirty = true;
    }
    changed
}

/// Adds a `selectable_value` and marks the preview dirty when it changes.
fn dirty_selectable<T: PartialEq>(
    ui: &mut egui::Ui,
    current_value: &mut T,
    selected_value: T,
    text: &str,
    preview: &mut BrushPreviewState,
) -> bool {
    let changed = ui
        .selectable_value(current_value, selected_value, text)
        .changed();
    if changed {
        preview.dirty = true;
    }
    changed
}

/// Adds a checkbox and marks the preview dirty when it changes.
fn dirty_checkbox(
    ui: &mut egui::Ui,
    value: &mut bool,
    text: &str,
    preview: &mut BrushPreviewState,
) -> bool {
    let changed = ui.checkbox(value, text).changed();
    if changed {
        preview.dirty = true;
    }
    changed
}

/// Draws a selectable brush-tip swatch (border + custom shape) and returns its response
/// (with hover text already attached) so the caller can check `.clicked()`.
fn tip_swatch(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    is_selected: bool,
    hover_text: &str,
    draw_shape: impl FnOnce(&egui::Painter, egui::Rect),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
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
    draw_shape(ui.painter(), rect);
    response.on_hover_text(hover_text)
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
        dirty_selectable(ui, &mut brush.brush_type, BrushType::Soft, "Soft", preview);
        dirty_selectable(ui, &mut brush.brush_type, BrushType::Pixel, "Pixel", preview);
    });

    ui.horizontal(|ui| {
        ui.label("Mode:");
        dirty_selectable(
            ui,
            &mut brush.brush_options.blend_mode,
            BlendMode::Normal,
            "Normal",
            preview,
        );
        dirty_selectable(
            ui,
            &mut brush.brush_options.blend_mode,
            BlendMode::Eraser,
            "Eraser",
            preview,
        );
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
                let is_selected =
                    matches!(brush.brush_options.pixel_shape, PixelBrushShape::Circle);
                if tip_swatch(ui, size, is_selected, "Circle", |painter, rect| {
                    painter.circle_filled(rect.center(), 12.0, Color32::WHITE);
                })
                .clicked()
                {
                    brush.brush_options.pixel_shape = PixelBrushShape::Circle;
                    preview.dirty = true;
                }

                // Square
                let is_selected =
                    matches!(brush.brush_options.pixel_shape, PixelBrushShape::Square);
                if tip_swatch(ui, size, is_selected, "Square", |painter, rect| {
                    painter.rect_filled(rect.shrink(4.0), 0.0, Color32::WHITE);
                })
                .clicked()
                {
                    brush.brush_options.pixel_shape = PixelBrushShape::Square;
                    preview.dirty = true;
                }

                // Custom tips
                for (name, shape, texture_opt) in loaded_tips {
                    if let Some(texture) = texture_opt {
                        let is_selected = &brush.brush_options.pixel_shape == shape;
                        let texture_id = texture.id();
                        if tip_swatch(ui, size, is_selected, name, |painter, rect| {
                            painter.image(
                                texture_id,
                                rect.shrink(2.0),
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        })
                        .clicked()
                        {
                            brush.brush_options.pixel_shape = shape.clone();
                            preview.dirty = true;
                        }
                    }
                }
            });
        });
    ui.add_space(5.0);

    ui.label("Size:");
    if dirty_slider(
        ui,
        egui::Slider::new(&mut brush.brush_options.diameter, 1.0..=3000.0).logarithmic(true),
        preview,
    ) {
        mask_dirty = true;
    }

    if brush.brush_type == BrushType::Soft {
        ui.horizontal(|ui| {
            ui.label("Softness:");
            if dirty_selectable(
                ui,
                &mut brush.brush_options.softness_selector,
                SoftnessSelector::Gaussian,
                "Gaussian",
                preview,
            ) {
                mask_dirty = true;
            }
            if dirty_selectable(
                ui,
                &mut brush.brush_options.softness_selector,
                SoftnessSelector::Curve,
                "Curve",
                preview,
            ) {
                mask_dirty = true;
            }
        });

        match brush.brush_options.softness_selector {
            SoftnessSelector::Gaussian => {
                ui.label("Hardness:");
                if dirty_slider(
                    ui,
                    egui::Slider::new(&mut brush.brush_options.hardness, 0.0..=100.0),
                    preview,
                ) {
                    mask_dirty = true;
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
    dirty_slider(
        ui,
        egui::Slider::new(&mut brush.brush_options.opacity, 0.0..=1.0),
        preview,
    );

    ui.label("Flow:");
    dirty_slider(
        ui,
        egui::Slider::new(&mut brush.brush_options.flow, 0.0..=100.0),
        preview,
    );

    ui.label("Spacing (%):");
    dirty_slider(
        ui,
        egui::Slider::new(&mut brush.brush_options.spacing, 1.0..=200.0),
        preview,
    );

    ui.label("Jitter (% of size):");
    dirty_slider(ui, egui::Slider::new(&mut brush.jitter, 0.0..=50.0), preview);

    ui.label("Stabilizer:");
    ui.horizontal(|ui| {
        dirty_selectable(
            ui,
            &mut brush.stabilizer_algorithm,
            StabilizerAlgorithm::None,
            "None",
            preview,
        );
        dirty_selectable(
            ui,
            &mut brush.stabilizer_algorithm,
            StabilizerAlgorithm::Simple,
            "Simple",
            preview,
        );
        dirty_selectable(
            ui,
            &mut brush.stabilizer_algorithm,
            StabilizerAlgorithm::Dynamic,
            "Dynamic",
            preview,
        );
    });

    match brush.stabilizer_algorithm {
        StabilizerAlgorithm::None => {}
        StabilizerAlgorithm::Simple => {
            dirty_slider(
                ui,
                egui::Slider::new(&mut brush.stabilizer, 0.0..=1.0).text("Strength"),
                preview,
            );
        }
        StabilizerAlgorithm::Dynamic => {
            dirty_slider(
                ui,
                egui::Slider::new(&mut brush.stabilizer_mass, 0.01..=1.0).text("Mass"),
                preview,
            );
            dirty_slider(
                ui,
                egui::Slider::new(&mut brush.stabilizer_drag, 0.0..=1.0).text("Drag"),
                preview,
            );
        }
    }

    ui.separator();
    dirty_checkbox(ui, &mut brush.pixel_perfect, "Pixel Perfect Mode", preview);
    dirty_checkbox(ui, &mut brush.anti_aliasing, "Anti-aliasing", preview);

    if mask_dirty {
        brush.is_changed = true;
    }
}
