//! The Brush Settings panel: tip, size, hardness, spacing, dynamics and
//! stabiliser.

use crate::brush_engine::brush::{Brush, BrushType, StabilizerAlgorithm};
use crate::brush_engine::brush_options::{PaintingMode, PixelBrushShape, TipOrder};
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
    loaded_tips: &[crate::app::state::LoadedTip],
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
    loaded_tips: &[crate::app::state::LoadedTip],
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
            &[
                (BrushType::Soft, "Soft"),
                (BrushType::Pixel, "Pixel"),
                (BrushType::Bristle, "Bristle"),
            ],
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
            BrushType::Bristle => {
                "A row of hairs across the stroke, each painting its own line: streaky paint \
                 that fans out with pressure."
            }
        })
        .small()
        .color(TEXT_DIM),
    );
    if brush.brush_type == BrushType::Bristle {
        changed |= bristle_section(ui, &mut brush.bristles);
    }
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
                    let o = &mut brush.brush_options;
                    let (shape, extras) = (&mut o.pixel_shape, &mut o.extra_tips);

                    let selected = matches!(shape, PixelBrushShape::Circle);
                    if tip_swatch(ui, size, selected, "Circle", |painter, rect| {
                        painter.circle_filled(rect.center(), rect.width() * 0.32, TEXT);
                    })
                    .clicked()
                    {
                        *shape = PixelBrushShape::Circle;
                        extras.clear();
                        mask_changed = true;
                    }

                    let selected = matches!(shape, PixelBrushShape::Square);
                    if tip_swatch(ui, size, selected, "Square", |painter, rect| {
                        painter.rect_filled(rect.shrink(rect.width() * 0.2), 0.0, TEXT);
                    })
                    .clicked()
                    {
                        *shape = PixelBrushShape::Square;
                        extras.clear();
                        mask_changed = true;
                    }

                    for loaded in loaded_tips {
                        let Some(texture) = &loaded.texture else {
                            continue;
                        };
                        let tip_shape = &loaded.shape;
                        let selected = &*shape == tip_shape && *extras == loaded.extra;
                        let texture_id = texture.id();
                        let (fw, fh) = match tip_shape {
                            PixelBrushShape::Custom(tip) => tip.extent(),
                            _ => (1.0, 1.0),
                        };
                        let set = loaded.extra.len() + 1;
                        let hover = if set > 1 {
                            format!("{}: {set} tips, taken in turn", loaded.name)
                        } else {
                            format!("{}\nRight-click: add to this brush's tips", loaded.name)
                        };
                        let response = tip_swatch(ui, size, selected, &hover, |painter, rect| {
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
                            if set > 1 {
                                // A set: its count in the corner.
                                painter.text(
                                    rect.right_bottom() - egui::vec2(3.0, 2.0),
                                    egui::Align2::RIGHT_BOTTOM,
                                    format!("×{set}"),
                                    egui::FontId::proportional(10.0),
                                    ACCENT,
                                );
                            }
                        });
                        if response.clicked() {
                            *shape = tip_shape.clone();
                            *extras = loaded.extra.clone();
                            mask_changed = true;
                        }
                        // Right-click adds a tip to an image-tip brush's own.
                        if response.secondary_clicked()
                            && matches!(shape, PixelBrushShape::Custom(_))
                            && let PixelBrushShape::Custom(tip) = tip_shape
                        {
                            extras.push(tip.clone());
                            extras.extend(loaded.extra.iter().cloned());
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
            let inverted = tip.inverted();
            let o = &mut brush.brush_options;
            o.pixel_shape = PixelBrushShape::Custom(inverted);
            for extra in &mut o.extra_tips {
                *extra = extra.inverted();
            }
            mask_changed = true;
        }
        let tips = brush.brush_options.tip_count();
        if tips > 1 {
            let o = &mut brush.brush_options;
            property_row(ui, &format!("{tips} tips"), |ui| {
                egui::ComboBox::from_id_salt("brush_tip_order")
                    .selected_text(o.tip_order.label())
                    .show_ui(ui, |ui| {
                        for order in TipOrder::ALL {
                            changed |= ui
                                .selectable_value(&mut o.tip_order, order, order.label())
                                .changed();
                        }
                    })
                    .response
                    .on_hover_text(
                        "How each dab picks its tip: in turn, at random, by pen pressure (light \
                         the first tip, full the last) or by the stroke's direction.",
                    );
                if ui
                    .button("One tip")
                    .on_hover_text("Keep only the first tip")
                    .clicked()
                {
                    o.extra_tips.clear();
                    mask_changed = true;
                }
            });
        } else if matches!(brush.brush_options.pixel_shape, PixelBrushShape::Custom(_)) {
            ui.label(
                egui::RichText::new(
                    "Right-click more tips to have the dabs alternate between them.",
                )
                .small()
                .color(TEXT_DIM),
            );
        }
        ui.add_space(4.0);

        // The preview is drawn at a fixed size, so Size only rebuilds the mask.
        size_changed |= slider_row(
            ui,
            "Size",
            crate::ui::widgets::reset(&mut brush.brush_options.diameter, |v| {
                egui::Slider::new(v, 1.0..=3000.0)
                    .logarithmic(true)
                    .max_decimals(0)
                    .suffix(" px")
            }),
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
                        crate::ui::widgets::reset(&mut brush.brush_options.hardness, |v| {
                            egui::Slider::new(v, 0.0..=100.0)
                                .max_decimals(0)
                                .suffix("%")
                        }),
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
            crate::ui::widgets::reset(&mut brush.brush_options.opacity, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Flow",
            crate::ui::widgets::reset(&mut brush.brush_options.flow, |v| {
                egui::Slider::new(v, 0.0..=100.0)
                    .max_decimals(0)
                    .suffix("%")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Spacing",
            crate::ui::widgets::reset(&mut brush.brush_options.spacing, |v| {
                egui::Slider::new(v, 1.0..=200.0)
                    .max_decimals(0)
                    .suffix("%")
            }),
        )
        .on_hover_text("Distance between dabs, as a percentage of the brush size.")
        .changed();
        changed |= slider_row(
            ui,
            "Jitter",
            crate::ui::widgets::reset(&mut brush.jitter, |v| {
                egui::Slider::new(v, 0.0..=50.0).max_decimals(0).suffix("%")
            }),
        )
        .on_hover_text("Random dab offset, as a percentage of the brush size.")
        .changed();
        changed |= slider_row(
            ui,
            "Airbrush",
            crate::ui::widgets::reset(&mut brush.airbrush_rate, |v| {
                egui::Slider::new(v, 0.0..=100.0)
                    .max_decimals(0)
                    .suffix("/s")
                    .logarithmic(true)
                    .smallest_positive(1.0)
            }),
        )
        .on_hover_text(
            "Dabs per second added where the pen is, so paint keeps building while it's held \
             still (0: off).",
        )
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
    changed |= dual_section(ui, &mut brush.dual, loaded_tips);
    section(ui, "Watercolour", false, |ui| {
        changed |= slider_row(
            ui,
            "Wet edges",
            crate::ui::widgets::reset(&mut brush.wet_edge, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=0.95))
            }),
        )
        .on_hover_text(
            "When the pen lifts, the middle of the stroke thins and its paint gathers at the \
             rim, as a wash does drying (0: off).",
        )
        .changed();
        if brush.wet_edge > 0.0 {
            changed |= slider_row(
                ui,
                "Edge width",
                crate::ui::widgets::reset(&mut brush.wet_edge_width, |v| {
                    egui::Slider::new(v, 1.0..=32.0)
                        .max_decimals(0)
                        .suffix(" px")
                }),
            )
            .changed();
        }
    });

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
                crate::ui::widgets::reset(&mut o.pressure_min_size, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            )
            .on_hover_text("Brush size at the lightest pressure, as a share of the full size.")
            .changed();
        }
        let tilt = &mut brush.dynamics.tilt;
        changed |= slider_row(
            ui,
            "Tilt → size",
            crate::ui::widgets::reset(&mut tilt.size, |v| {
                percent_of_unit(egui::Slider::new(v, -1.0..=1.0))
            }),
        )
        .on_hover_text("Above 0: the stroke widens as the pen leans, like a pencil on its side.")
        .changed();
        changed |= slider_row(
            ui,
            "Tilt → opacity",
            crate::ui::widgets::reset(&mut tilt.opacity, |v| {
                percent_of_unit(egui::Slider::new(v, -1.0..=1.0))
            }),
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
                    crate::ui::widgets::reset(&mut brush.stabilizer, |v| {
                        percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                    }),
                )
                .changed();
            }
            StabilizerAlgorithm::Dynamic => {
                changed |= slider_row(
                    ui,
                    "Mass",
                    crate::ui::widgets::reset(&mut brush.stabilizer_mass, |v| {
                        egui::Slider::new(v, 0.01..=1.0)
                    }),
                )
                .changed();
                changed |= slider_row(
                    ui,
                    "Drag",
                    crate::ui::widgets::reset(&mut brush.stabilizer_drag, |v| {
                        egui::Slider::new(v, 0.0..=1.0)
                    }),
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
            crate::ui::widgets::reset(&mut t.angle, |v| {
                egui::Slider::new(v, -180.0..=180.0)
                    .max_decimals(0)
                    .suffix("°")
            }),
        )
        .on_hover_text("Turn of the tip; with Follow stroke, relative to the stroke's direction.")
        .changed();
        changed |= slider_row(
            ui,
            "Squash",
            crate::ui::widgets::reset(&mut t.ratio, |v| {
                percent_of_unit(egui::Slider::new(v, 0.05..=1.0))
            }),
        )
        .on_hover_text("Tip height as a share of its width: low values make a flat nib.")
        .changed();
        changed |= slider_row(
            ui,
            "Random angle",
            crate::ui::widgets::reset(&mut t.random_angle, |v| {
                egui::Slider::new(v, 0.0..=180.0)
                    .max_decimals(0)
                    .suffix("°")
            }),
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
            crate::ui::widgets::reset(&mut t.start, |v| {
                egui::Slider::new(v, 0.0..=400.0)
                    .max_decimals(0)
                    .suffix(" px")
            }),
        )
        .on_hover_text("Length over which the stroke's start grows to full.")
        .changed();
        changed |= slider_row(
            ui,
            "Taper out",
            crate::ui::widgets::reset(&mut t.end, |v| {
                egui::Slider::new(v, 0.0..=400.0)
                    .max_decimals(0)
                    .suffix(" px")
            }),
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
                crate::ui::widgets::reset(&mut t.min, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            )
            .on_hover_text("Size / opacity at the very end of a taper.")
            .changed();
        }
        let s = &mut d.speed;
        changed |= slider_row(
            ui,
            "Speed → size",
            crate::ui::widgets::reset(&mut s.size, |v| {
                percent_of_unit(egui::Slider::new(v, -1.0..=1.0))
            }),
        )
        .on_hover_text("Below 0: fast strokes get thinner (ink). Above 0: thicker.")
        .changed();
        changed |= slider_row(
            ui,
            "Speed → opacity",
            crate::ui::widgets::reset(&mut s.opacity, |v| {
                percent_of_unit(egui::Slider::new(v, -1.0..=1.0))
            }),
        )
        .on_hover_text("Below 0: fast strokes get lighter (dry brush). Above 0: stronger.")
        .changed();
    });
    section(ui, "Randomness", false, |ui| {
        let r = &mut d.random;
        changed |= slider_row(
            ui,
            "Size",
            crate::ui::widgets::reset(&mut r.size, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text("Each dab is randomly smaller, by up to this much.")
        .changed();
        changed |= slider_row(
            ui,
            "Opacity",
            crate::ui::widgets::reset(&mut r.opacity, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text("Each dab is randomly lighter, by up to this much.")
        .changed();
        changed |= slider_row(
            ui,
            "Hue",
            crate::ui::widgets::reset(&mut r.hue, |v| {
                egui::Slider::new(v, 0.0..=180.0)
                    .max_decimals(0)
                    .suffix("°")
            }),
        )
        .on_hover_text("Each dab's hue turns randomly by up to this much either way.")
        .changed();
        changed |= slider_row(
            ui,
            "Saturation",
            crate::ui::widgets::reset(&mut r.saturation, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Value",
            crate::ui::widgets::reset(&mut r.value, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text("Each dab is randomly lighter or darker by up to this much.")
        .changed();
        let mut count = r.count.max(1);
        if slider_row(
            ui,
            "Dabs per step",
            crate::ui::widgets::reset(&mut count, |v| egui::Slider::new(v, 1..=16)),
        )
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
/// A bristle brush's hairs.
fn bristle_section(ui: &mut egui::Ui, b: &mut crate::brush_engine::bristle::Bristles) -> bool {
    let mut changed = false;
    section(ui, "Bristles", true, |ui| {
        changed |= slider_row(
            ui,
            "Hairs",
            crate::ui::widgets::reset(&mut b.count, |v| {
                egui::Slider::new(v, 1..=crate::brush_engine::bristle::MAX_HAIRS)
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Thickness",
            crate::ui::widgets::reset(&mut b.thickness, |v| {
                egui::Slider::new(v, 0.5..=20.0)
                    .logarithmic(true)
                    .max_decimals(1)
                    .suffix(" px")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Spread",
            crate::ui::widgets::reset(&mut b.spread, |v| {
                percent_of_unit(egui::Slider::new(v, 0.1..=2.0))
            }),
        )
        .on_hover_text("How far the hairs fan out, as a share of the brush size.")
        .changed();
        changed |= slider_row(
            ui,
            "Paint lasts",
            crate::ui::widgets::reset(&mut b.ink, |v| {
                egui::Slider::new(v, 0.0..=4000.0)
                    .logarithmic(true)
                    .smallest_positive(50.0)
                    .max_decimals(0)
                    .suffix(" px")
            }),
        )
        .on_hover_text(
            "How long a stroke the hairs' paint lasts before they run dry, each at its own \
             pace (0: it never runs out).",
        )
        .changed();
        changed |= slider_row(
            ui,
            "Variation",
            crate::ui::widgets::reset(&mut b.variation, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text("How much the hairs differ in thickness and strength.")
        .changed();
    });
    changed
}

/// The dual brush: a second tip masking the first.
fn dual_section(
    ui: &mut egui::Ui,
    dual: &mut Option<crate::brush_engine::dual::DualTip>,
    loaded_tips: &[crate::app::state::LoadedTip],
) -> bool {
    use crate::brush_engine::dual::{DualMode, DualTip};
    let mut changed = false;
    section(ui, "Dual brush", false, |ui| {
        let mut on = dual.is_some();
        if ui
            .checkbox(&mut on, "Mask with a second tip")
            .on_hover_text(
                "A second tip is stamped along the stroke, and paint shows only where it \
                 reaches: a round brush masked by a spatter tip paints like dry media.",
            )
            .changed()
        {
            *dual = on.then(DualTip::default);
            changed = true;
        }
        let Some(d) = dual else {
            return;
        };
        let name_of = |shape: &PixelBrushShape| match shape {
            PixelBrushShape::Circle => "Circle".to_string(),
            PixelBrushShape::Square => "Square".to_string(),
            PixelBrushShape::Custom(_) => loaded_tips
                .iter()
                .find(|t| &t.shape == shape)
                .map_or_else(|| "Picture".to_string(), |t| t.name.clone()),
        };
        property_row(ui, "Tip", |ui| {
            egui::ComboBox::from_id_salt("dual_tip")
                .selected_text(name_of(&d.shape))
                .show_ui(ui, |ui| {
                    let choices = [PixelBrushShape::Circle, PixelBrushShape::Square]
                        .into_iter()
                        .chain(
                            loaded_tips
                                .iter()
                                .filter(|t| t.extra.is_empty())
                                .map(|t| t.shape.clone()),
                        );
                    for shape in choices {
                        let selected = d.shape == shape;
                        let label = name_of(&shape);
                        if ui.selectable_label(selected, label).clicked() && !selected {
                            d.shape = shape;
                            changed = true;
                        }
                    }
                });
        });
        property_row(ui, "Mode", |ui| {
            egui::ComboBox::from_id_salt("dual_mode")
                .selected_text(d.mode.label())
                .show_ui(ui, |ui| {
                    for mode in DualMode::ALL {
                        changed |= ui
                            .selectable_value(&mut d.mode, mode, mode.label())
                            .changed();
                    }
                })
                .response
                .on_hover_text(
                    "Multiply: soft all over. Darken: the weaker of the two. Subtract: the \
                     second tip's gaps eat into the paint. Height: light paint only on its \
                     peaks.",
                );
        });
        changed |= slider_row(
            ui,
            "Size",
            crate::ui::widgets::reset(&mut d.size, |v| {
                percent_of_unit(egui::Slider::new(v, 0.05..=2.0).logarithmic(true))
            }),
        )
        .on_hover_text("As a share of the brush size.")
        .changed();
        if !matches!(d.shape, PixelBrushShape::Custom(_)) {
            changed |= slider_row(
                ui,
                "Hardness",
                crate::ui::widgets::reset(&mut d.hardness, |v| {
                    egui::Slider::new(v, 0.0..=100.0)
                        .max_decimals(0)
                        .suffix("%")
                }),
            )
            .changed();
        }
        changed |= slider_row(
            ui,
            "Spacing",
            crate::ui::widgets::reset(&mut d.spacing, |v| {
                egui::Slider::new(v, 5.0..=300.0)
                    .max_decimals(0)
                    .suffix("%")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Scatter",
            crate::ui::widgets::reset(&mut d.scatter, |v| {
                egui::Slider::new(v, 0.0..=300.0)
                    .max_decimals(0)
                    .suffix("%")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Count",
            crate::ui::widgets::reset(&mut d.count, |v| egui::Slider::new(v, 1..=16)),
        )
        .changed();
        changed |= ui
            .checkbox(&mut d.random_angle, "Turn each dab at random")
            .changed();
    });
    changed
}

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
            crate::ui::widgets::reset(&mut t.strength, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Scale",
            crate::ui::widgets::reset(&mut t.scale, |v| {
                egui::Slider::new(v, 0.25..=4.0)
                    .logarithmic(true)
                    .max_decimals(2)
                    .suffix("×")
            }),
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
