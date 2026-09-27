//! The Brush Settings panel: tip, size, hardness, spacing, dynamics and
//! stabiliser.

use crate::brush_engine::brush::{Brush, BrushType, StabilizerAlgorithm};
use crate::brush_engine::brush_options::{PaintingMode, PixelBrushShape};
use crate::brush_engine::hardness::SoftnessSelector;
use crate::ui::style::*;
use crate::ui::widgets::{percent_of_unit, property_row, section, segmented, slider_row};
use crate::ui::{brush_preview::render_preview, curve_editor::curve_editor};
use eframe::egui::{self, Color32};
use rayon::ThreadPool;

pub struct BrushPreviewState {
    /// Strip size in points.
    pub size: [usize; 2],
    pub texture: Option<egui::TextureHandle>,
    pub dirty: bool,
    /// Display scale the texture was rendered at.
    pub pixels_per_point: f32,
}

impl Default for BrushPreviewState {
    fn default() -> Self {
        Self {
            size: [200, 80],
            texture: None,
            dirty: true,
            pixels_per_point: 1.0,
        }
    }
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
    ui.painter().rect_filled(rect, 0.0, BG_INSET);
    draw_shape(ui.painter(), rect);
    let stroke = if is_selected {
        egui::Stroke::new(2.0_f32, ACCENT)
    } else if response.hovered() {
        egui::Stroke::new(1.0_f32, TEXT_DIM)
    } else {
        egui::Stroke::new(1.0_f32, BORDER_LIGHT)
    };
    ui.painter().rect_stroke(rect, 0.0, stroke);
    response.on_hover_text(hover_text)
}

/// Height of the stroke preview strip.
const PREVIEW_HEIGHT: usize = 72;

/// Panel for tweaking the currently selected brush properties.
pub fn brush_settings_panel(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    preview: &mut BrushPreviewState,
    pool: &ThreadPool,
    loaded_tips: &[(String, PixelBrushShape, Option<egui::TextureHandle>)],
    textures: &[std::sync::Arc<crate::brush_engine::texture::Pattern>],
) {
    egui::ScrollArea::vertical()
        .id_salt("brush_settings_scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let (changed, mask_changed) =
                brush_settings_contents(ui, brush, preview, pool, loaded_tips, textures);
            if changed || mask_changed {
                preview.dirty = true;
            }
            if mask_changed {
                brush.is_changed = true;
            }
        });
}

/// Returns `(anything changed, brush mask changed)`.
fn brush_settings_contents(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    preview: &mut BrushPreviewState,
    pool: &ThreadPool,
    loaded_tips: &[(String, PixelBrushShape, Option<egui::TextureHandle>)],
    textures: &[std::sync::Arc<crate::brush_engine::texture::Pattern>],
) -> (bool, bool) {
    let mut changed = false;
    let mut mask_changed = false;
    let mut size_changed = false;

    // Preview strip, re-rendered at the panel's width.
    let width = ((ui.available_width() as usize) / 8 * 8).clamp(120, 480);
    if preview.size != [width, PREVIEW_HEIGHT]
        || preview.pixels_per_point != ui.ctx().pixels_per_point()
    {
        preview.size = [width, PREVIEW_HEIGHT];
        preview.dirty = true;
    }
    if preview.dirty {
        render_preview(preview, brush, pool, ui.ctx());
        preview.dirty = false;
    }
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), PREVIEW_HEIGHT as f32),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, BG_INSET);
    if let Some(texture) = &preview.texture {
        let image_rect = egui::Rect::from_center_size(
            rect.center(),
            texture.size_vec2() / preview.pixels_per_point,
        );
        ui.painter().image(
            texture.id(),
            image_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    ui.painter()
        .rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, BORDER));
    ui.add_space(6.0);

    property_row(ui, "Type", |ui| {
        mask_changed |= segmented(
            ui,
            &mut brush.brush_type,
            &[(BrushType::Soft, "Soft"), (BrushType::Pixel, "Pixel")],
            false,
        );
    });
    ui.label(
        egui::RichText::new(match brush.brush_type {
            BrushType::Soft => {
                "Smooth, anti-aliased dabs placed at sub-pixel positions: painting and inking."
            }
            BrushType::Pixel => {
                "Dabs snap to whole pixels with hard edges: pixel art and crisp 1-px lines."
            }
        })
        .small()
        .color(TEXT_DIM),
    );
    property_row(ui, "Painting", |ui| {
        changed |= segmented(
            ui,
            &mut brush.brush_options.painting_mode,
            &[
                (PaintingMode::BuildUp, "Build-up"),
                (PaintingMode::Wash, "Wash"),
            ],
            false,
        );
    });
    ui.label(
        egui::RichText::new(match brush.brush_options.painting_mode {
            PaintingMode::BuildUp => "Overlapping dabs keep adding paint.",
            PaintingMode::Wash => "A stroke never exceeds the brush opacity.",
        })
        .small()
        .color(TEXT_DIM),
    );

    section(ui, "Tip", true, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("pixel_tip_selector")
            .max_height(112.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                    let side = metrics(ui.ctx()).tip_swatch;
                    let size = egui::vec2(side, side);
                    let shape = &mut brush.brush_options.pixel_shape;

                    let selected = matches!(shape, PixelBrushShape::Circle);
                    if tip_swatch(ui, size, selected, "Circle", |painter, rect| {
                        painter.circle_filled(rect.center(), rect.width() * 0.32, TEXT);
                    })
                    .clicked()
                    {
                        *shape = PixelBrushShape::Circle;
                        mask_changed = true;
                    }

                    let selected = matches!(shape, PixelBrushShape::Square);
                    if tip_swatch(ui, size, selected, "Square", |painter, rect| {
                        painter.rect_filled(rect.shrink(rect.width() * 0.2), 0.0, TEXT);
                    })
                    .clicked()
                    {
                        *shape = PixelBrushShape::Square;
                        mask_changed = true;
                    }

                    for (name, tip_shape, texture_opt) in loaded_tips {
                        let Some(texture) = texture_opt else { continue };
                        let selected = &*shape == tip_shape;
                        let texture_id = texture.id();
                        let (fw, fh) = match tip_shape {
                            PixelBrushShape::Custom(tip) => tip.extent(),
                            _ => (1.0, 1.0),
                        };
                        if tip_swatch(ui, size, selected, name, |painter, rect| {
                            // At the tip's own proportions.
                            let inner = rect.shrink(3.0);
                            let fit = egui::vec2(inner.width() * fw, inner.height() * fh);
                            painter.image(
                                texture_id,
                                egui::Rect::from_center_size(inner.center(), fit),
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        })
                        .clicked()
                        {
                            *shape = tip_shape.clone();
                            mask_changed = true;
                        }
                    }
                });
            });
        if let PixelBrushShape::Custom(tip) = &brush.brush_options.pixel_shape
            && ui
                .button("Invert tip")
                .on_hover_text("Paint with what the picture leaves empty, and the other way round.")
                .clicked()
        {
            brush.brush_options.pixel_shape = PixelBrushShape::Custom(tip.inverted());
            mask_changed = true;
        }
        ui.add_space(4.0);

        // The preview is drawn at a fixed size, so Size only rebuilds the mask.
        size_changed |= slider_row(
            ui,
            "Size",
            egui::Slider::new(&mut brush.brush_options.diameter, 1.0..=3000.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px"),
        )
        .changed();

        if brush.brush_type == BrushType::Soft {
            property_row(ui, "Softness", |ui| {
                mask_changed |= segmented(
                    ui,
                    &mut brush.brush_options.softness_selector,
                    &[
                        (SoftnessSelector::Gaussian, "Gaussian"),
                        (SoftnessSelector::Curve, "Curve"),
                    ],
                    false,
                );
            });
            match brush.brush_options.softness_selector {
                SoftnessSelector::Gaussian => {
                    mask_changed |= slider_row(
                        ui,
                        "Hardness",
                        egui::Slider::new(&mut brush.brush_options.hardness, 0.0..=100.0)
                            .max_decimals(0)
                            .suffix("%"),
                    )
                    .changed();
                }
                SoftnessSelector::Curve => {
                    mask_changed |= curve_editor(ui, &mut brush.brush_options.softness_curve);
                }
            }
        }
    });

    section(ui, "Stroke", true, |ui| {
        changed |= slider_row(
            ui,
            "Opacity",
            percent_of_unit(egui::Slider::new(
                &mut brush.brush_options.opacity,
                0.0..=1.0,
            )),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Flow",
            egui::Slider::new(&mut brush.brush_options.flow, 0.0..=100.0)
                .max_decimals(0)
                .suffix("%"),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Spacing",
            egui::Slider::new(&mut brush.brush_options.spacing, 1.0..=200.0)
                .max_decimals(0)
                .suffix("%"),
        )
        .on_hover_text("Distance between dabs, as a percentage of the brush size.")
        .changed();
        changed |= slider_row(
            ui,
            "Jitter",
            egui::Slider::new(&mut brush.jitter, 0.0..=50.0)
                .max_decimals(0)
                .suffix("%"),
        )
        .on_hover_text("Random dab offset, as a percentage of the brush size.")
        .changed();
        property_row(ui, "Blend", |ui| {
            use crate::canvas::blend_modes::LayerBlend;
            egui::ComboBox::from_id_salt("brush_paint_blend")
                .selected_text(brush.paint_blend.label())
                .show_ui(ui, |ui| {
                    for (i, group) in LayerBlend::GROUPS.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        for &mode in *group {
                            changed |= ui
                                .selectable_value(&mut brush.paint_blend, mode, mode.label())
                                .changed();
                        }
                    }
                })
                .response
                .on_hover_text(
                    "How the paint mixes with what's already on the layer: Multiply darkens, \
                     Screen and Add (Linear Dodge) lighten and glow, Overlay adds contrast.",
                );
        });
    });

    changed |= dynamics_sections(ui, &mut brush.dynamics);
    changed |= texture_section(ui, &mut brush.texture, textures);

    section(ui, "Pen pressure & tilt", true, |ui| {
        let o = &mut brush.brush_options;
        property_row(ui, "Controls", |ui| {
            changed |= ui.toggle_value(&mut o.pressure_size, "Size").changed();
            changed |= ui
                .toggle_value(&mut o.pressure_opacity, "Opacity")
                .changed();
            changed |= ui.toggle_value(&mut o.pressure_flow, "Flow").changed();
        });
        if o.pressure_size {
            mask_changed |= slider_row(
                ui,
                "Min size",
                percent_of_unit(egui::Slider::new(&mut o.pressure_min_size, 0.0..=1.0)),
            )
            .on_hover_text("Brush size at the lightest pressure, as a share of the full size.")
            .changed();
        }
        let tilt = &mut brush.dynamics.tilt;
        changed |= slider_row(
            ui,
            "Tilt → size",
            percent_of_unit(egui::Slider::new(&mut tilt.size, -1.0..=1.0)),
        )
        .on_hover_text("Above 0: the stroke widens as the pen leans, like a pencil on its side.")
        .changed();
        changed |= slider_row(
            ui,
            "Tilt → opacity",
            percent_of_unit(egui::Slider::new(&mut tilt.opacity, -1.0..=1.0)),
        )
        .on_hover_text("Above 0: stronger as the pen leans; below 0: lighter.")
        .changed();
        let o = &mut brush.brush_options;
        let curves = &mut o.pressure_curves;
        for (on, name, curve) in [
            (o.pressure_size, "Size", &mut curves.size),
            (o.pressure_opacity, "Opacity", &mut curves.opacity),
            (o.pressure_flow, "Flow", &mut curves.flow),
        ] {
            if on {
                changed |= pressure_curve_row(ui, name, curve);
            }
        }
    });

    section(ui, "Stabilizer", true, |ui| {
        property_row(ui, "Method", |ui| {
            changed |= segmented(
                ui,
                &mut brush.stabilizer_algorithm,
                &[
                    (StabilizerAlgorithm::None, "None"),
                    (StabilizerAlgorithm::Simple, "Simple"),
                    (StabilizerAlgorithm::Dynamic, "Dynamic"),
                ],
                false,
            );
        });
        match brush.stabilizer_algorithm {
            StabilizerAlgorithm::None => {}
            StabilizerAlgorithm::Simple => {
                changed |= slider_row(
                    ui,
                    "Strength",
                    percent_of_unit(egui::Slider::new(&mut brush.stabilizer, 0.0..=1.0)),
                )
                .changed();
            }
            StabilizerAlgorithm::Dynamic => {
                changed |= slider_row(
                    ui,
                    "Mass",
                    egui::Slider::new(&mut brush.stabilizer_mass, 0.01..=1.0),
                )
                .changed();
                changed |= slider_row(
                    ui,
                    "Drag",
                    egui::Slider::new(&mut brush.stabilizer_drag, 0.0..=1.0),
                )
                .changed();
            }
        }
    });

    section(ui, "Options", false, |ui| {
        changed |= ui
            .checkbox(&mut brush.pixel_perfect, "Pixel-perfect lines")
            .on_hover_text(
                "For thin pixel-art lines: drops the extra pixel at each corner so a \
                 1-px line never doubles up into L-shaped clumps.",
            )
            .changed();
        changed |= ui
            .checkbox(&mut brush.anti_aliasing, "Anti-aliasing")
            .on_hover_text(
                "Smooth, partly transparent edge pixels (on), or hard all-or-nothing \
                 edges (off). Soft brushes want it on; pixel art usually off.",
            )
            .changed();
    });

    if size_changed {
        brush.is_changed = true;
    }
    (changed, mask_changed)
}

/// Tip shape, tapers and speed, and randomness. Returns whether anything
/// changed.
fn dynamics_sections(
    ui: &mut egui::Ui,
    d: &mut crate::brush_engine::dynamics::BrushDynamics,
) -> bool {
    let mut changed = false;
    section(ui, "Tip shape", false, |ui| {
        let t = &mut d.tip;
        changed |= slider_row(
            ui,
            "Angle",
            egui::Slider::new(&mut t.angle, -180.0..=180.0)
                .max_decimals(0)
                .suffix("°"),
        )
        .on_hover_text("Turn of the tip; with Follow stroke, relative to the stroke's direction.")
        .changed();
        changed |= slider_row(
            ui,
            "Squash",
            percent_of_unit(egui::Slider::new(&mut t.ratio, 0.05..=1.0)),
        )
        .on_hover_text("Tip height as a share of its width: low values make a flat nib.")
        .changed();
        changed |= slider_row(
            ui,
            "Random angle",
            egui::Slider::new(&mut t.random_angle, 0.0..=180.0)
                .max_decimals(0)
                .suffix("°"),
        )
        .on_hover_text("Each dab turns randomly by up to this much either way.")
        .changed();
        changed |= ui
            .checkbox(&mut t.follow_stroke, "Follow stroke")
            .on_hover_text("Turn the tip with the direction the stroke is going (calligraphy).")
            .changed();
        changed |= ui
            .checkbox(&mut t.follow_tilt, "Follow pen tilt")
            .on_hover_text("Turn the tip the way the pen leans (tablets that report tilt).")
            .changed();
    });
    section(ui, "Taper & speed", false, |ui| {
        let t = &mut d.taper;
        changed |= slider_row(
            ui,
            "Taper in",
            egui::Slider::new(&mut t.start, 0.0..=400.0)
                .max_decimals(0)
                .suffix(" px"),
        )
        .on_hover_text("Length over which the stroke's start grows to full.")
        .changed();
        changed |= slider_row(
            ui,
            "Taper out",
            egui::Slider::new(&mut t.end, 0.0..=400.0)
                .max_decimals(0)
                .suffix(" px"),
        )
        .on_hover_text(
            "Length over which the stroke's end thins out when the pen lifts. The line \
             follows the pen while drawing and thins as you let go.",
        )
        .changed();
        if t.start > 0.0 || t.end > 0.0 {
            property_row(ui, "Tapers", |ui| {
                changed |= ui.toggle_value(&mut t.size, "Size").changed();
                changed |= ui.toggle_value(&mut t.opacity, "Opacity").changed();
            });
            changed |= slider_row(
                ui,
                "Tip",
                percent_of_unit(egui::Slider::new(&mut t.min, 0.0..=1.0)),
            )
            .on_hover_text("Size / opacity at the very end of a taper.")
            .changed();
        }
        let s = &mut d.speed;
        changed |= slider_row(
            ui,
            "Speed → size",
            percent_of_unit(egui::Slider::new(&mut s.size, -1.0..=1.0)),
        )
        .on_hover_text("Below 0: fast strokes get thinner (ink). Above 0: thicker.")
        .changed();
        changed |= slider_row(
            ui,
            "Speed → opacity",
            percent_of_unit(egui::Slider::new(&mut s.opacity, -1.0..=1.0)),
        )
        .on_hover_text("Below 0: fast strokes get lighter (dry brush). Above 0: stronger.")
        .changed();
    });
    section(ui, "Randomness", false, |ui| {
        let r = &mut d.random;
        changed |= slider_row(
            ui,
            "Size",
            percent_of_unit(egui::Slider::new(&mut r.size, 0.0..=1.0)),
        )
        .on_hover_text("Each dab is randomly smaller, by up to this much.")
        .changed();
        changed |= slider_row(
            ui,
            "Opacity",
            percent_of_unit(egui::Slider::new(&mut r.opacity, 0.0..=1.0)),
        )
        .on_hover_text("Each dab is randomly lighter, by up to this much.")
        .changed();
        changed |= slider_row(
            ui,
            "Hue",
            egui::Slider::new(&mut r.hue, 0.0..=180.0)
                .max_decimals(0)
                .suffix("°"),
        )
        .on_hover_text("Each dab's hue turns randomly by up to this much either way.")
        .changed();
        changed |= slider_row(
            ui,
            "Saturation",
            percent_of_unit(egui::Slider::new(&mut r.saturation, 0.0..=1.0)),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Value",
            percent_of_unit(egui::Slider::new(&mut r.value, 0.0..=1.0)),
        )
        .on_hover_text("Each dab is randomly lighter or darker by up to this much.")
        .changed();
        let mut count = r.count.max(1);
        if slider_row(ui, "Dabs per step", egui::Slider::new(&mut count, 1..=16))
            .on_hover_text("Several dabs at each step, spread by Jitter: spray, foliage, grain.")
            .changed()
        {
            r.count = count;
            changed = true;
        }
    });
    changed
}

/// Paper texture: which pattern, how it combines, its scale and strength.
fn texture_section(
    ui: &mut egui::Ui,
    texture: &mut Option<crate::brush_engine::texture::BrushTexture>,
    loaded: &[std::sync::Arc<crate::brush_engine::texture::Pattern>],
) -> bool {
    use crate::brush_engine::texture::{BrushTexture, TextureMode, builtin};
    let mut changed = false;
    section(ui, "Texture", false, |ui| {
        let current = texture
            .as_ref()
            .map_or("None", |t| t.pattern.name.as_str())
            .to_string();
        property_row(ui, "Paper", |ui| {
            egui::ComboBox::from_id_salt("brush_texture")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(texture.is_none(), "None").clicked() {
                        *texture = None;
                        changed = true;
                    }
                    for pattern in builtin().iter().chain(loaded) {
                        let selected = texture
                            .as_ref()
                            .is_some_and(|t| std::sync::Arc::ptr_eq(&t.pattern, pattern));
                        if ui.selectable_label(selected, &pattern.name).clicked() && !selected {
                            match texture {
                                Some(t) => t.pattern = pattern.clone(),
                                None => *texture = Some(BrushTexture::new(pattern.clone())),
                            }
                            changed = true;
                        }
                    }
                })
                .response
                .on_hover_text("Add your own: greyscale pictures in brushes/textures/.");
        });
        let Some(t) = texture else {
            return;
        };
        property_row(ui, "Mode", |ui| {
            let modes: Vec<(TextureMode, &str)> =
                TextureMode::ALL.iter().map(|&m| (m, m.label())).collect();
            changed |= segmented(ui, &mut t.mode, &modes, false);
        });
        changed |= slider_row(
            ui,
            "Strength",
            percent_of_unit(egui::Slider::new(&mut t.strength, 0.0..=1.0)),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Scale",
            egui::Slider::new(&mut t.scale, 0.25..=4.0)
                .logarithmic(true)
                .max_decimals(2)
                .suffix("×"),
        )
        .changed();
        changed |= ui.checkbox(&mut t.invert, "Invert").changed();
        ui.label(
            egui::RichText::new(match t.mode {
                TextureMode::Multiply => "The grain darkens the stroke evenly.",
                TextureMode::Subtract => "Low spots lose paint first; heavy strokes fill in.",
                TextureMode::Height => {
                    "Light pressure only catches the peaks; press harder to fill the valleys."
                }
            })
            .small()
            .color(TEXT_DIM),
        );
    });
    changed
}

/// A setting's pressure curve: off (pressure straight through) or an
/// editable response.
fn pressure_curve_row(
    ui: &mut egui::Ui,
    name: &str,
    curve: &mut Option<crate::brush_engine::hardness::SoftnessCurve>,
) -> bool {
    use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve};
    let mut changed = false;
    let mut custom = curve.is_some();
    property_row(ui, &format!("{name} curve"), |ui| {
        if ui
            .checkbox(&mut custom, "Custom")
            .on_hover_text("Shape how pen pressure drives this setting (off: straight through).")
            .changed()
        {
            *curve = custom.then(|| SoftnessCurve {
                points: vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)],
            });
            changed = true;
        }
    });
    if let Some(c) = curve {
        changed |= crate::ui::curve_editor::curve_editor_with(
            ui,
            c,
            &crate::ui::curve_editor::PRESSURE_PRESETS,
        );
    }
    changed
}
