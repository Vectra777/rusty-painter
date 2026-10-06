//! The Brush Settings panel: tip, size, hardness, spacing, dynamics and
//! stabiliser.

use crate::brush_engine::brush::{Brush, BrushType, StabilizerAlgorithm};
use crate::brush_engine::brush_options::{PaintingMode, PixelBrushShape, Placement, TipOrder};
use crate::brush_engine::hardness::SoftnessSelector;
use crate::ui::style::*;
use crate::ui::widgets::{percent_of_unit, property_row, section, segmented, slider_row};
use crate::ui::{
    brush_preview::{collect_preview, render_preview},
    curve_editor::curve_editor,
};
use eframe::egui::{self, Color32};
use rayon::ThreadPool;
use std::sync::Arc;

pub struct BrushPreviewState {
    /// Strip size in points.
    pub size: [usize; 2],
    pub texture: Option<egui::TextureHandle>,
    pub dirty: bool,
    /// Display scale the texture was rendered at.
    pub pixels_per_point: f32,
    /// Draws the strip off the UI thread.
    pub worker: crate::ui::preview_worker::PreviewWorker,
    /// The display scale of the strip being drawn.
    pub pending_pixels_per_point: f32,
}

impl Default for BrushPreviewState {
    fn default() -> Self {
        Self {
            size: [200, 80],
            texture: None,
            dirty: true,
            pixels_per_point: 1.0,
            worker: Default::default(),
            pending_pixels_per_point: 1.0,
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
    pool: &Arc<ThreadPool>,
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
    pool: &Arc<ThreadPool>,
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
    collect_preview(preview, ui.ctx());
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
        let label = |t: BrushType| match t {
            BrushType::Soft => "Soft",
            BrushType::Pixel => "Pixel",
            BrushType::Bristle => "Bristle",
            BrushType::Sketch => "Sketch",
            BrushType::Hatching => "Hatching",
            BrushType::Spray => "Spray",
            BrushType::Chalk => "Chalk",
            BrushType::Curve => "Curve",
            BrushType::Grid => "Grid",
            BrushType::TangentNormal => "Tangent normal",
            BrushType::Particle => "Particle",
        };
        egui::ComboBox::from_id_salt("brush_type")
            .selected_text(label(brush.brush_type))
            .show_ui(ui, |ui| {
                for t in BrushType::ALL {
                    mask_changed |= ui
                        .selectable_value(&mut brush.brush_type, t, label(t))
                        .changed();
                }
            });
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
            BrushType::Sketch => {
                "Its line, and fine lines to earlier points of the stroke nearby: going back and \
                 forth builds up a web of shading."
            }
            BrushType::Hatching => {
                "Parallel lines pinned to the canvas wherever it passes; pressing harder \
                 cross-hatches."
            }
            BrushType::Spray => {
                "Each dab a cloud of small particles of the brush's tip, evenly, thicker in \
                 the middle or in clumps."
            }
            BrushType::Chalk => {
                "The tip broken up by a grain each dab lays differently; pressing harder \
                 fills it in."
            }
            BrushType::Curve => {
                "Curves swinging from where the stroke was a while back to the pen: loose, \
                 looping lines."
            }
            BrushType::Grid => {
                "One shape of the tip in each cell of a grid the brush passes over: mosaics, \
                 halftone-like fills."
            }
            BrushType::TangentNormal => {
                "Paints a normal map: the pen's tilt as the colour (red leaning right, green \
                 up, blue upright). With a mouse, it leans the way the stroke goes."
            }
            BrushType::Particle => {
                "A swarm the pen pulls along, each particle drawing its own path: lines that \
                 swing and overshoot."
            }
        })
        .small()
        .color(TEXT_DIM),
    );
    match brush.brush_type {
        BrushType::Bristle => changed |= bristle_section(ui, &mut brush.bristles),
        BrushType::Sketch => changed |= sketch_section(ui, &mut brush.sketch),
        BrushType::Hatching => changed |= hatching_section(ui, &mut brush.hatching),
        BrushType::Spray => changed |= spray_section(ui, &mut brush.engines.spray),
        BrushType::Chalk => changed |= chalk_section(ui, &mut brush.engines.chalk),
        BrushType::Curve => changed |= curve_section(ui, &mut brush.engines.curve),
        BrushType::Grid => changed |= grid_section(ui, &mut brush.engines.grid),
        BrushType::TangentNormal => changed |= normal_section(ui, &mut brush.engines.normal),
        BrushType::Particle => changed |= particle_section(ui, &mut brush.engines.particles),
        BrushType::Soft | BrushType::Pixel => {}
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
        if matches!(brush.brush_options.pixel_shape, PixelBrushShape::Custom(_)) {
            let o = &mut brush.brush_options;
            property_row(ui, "Lay", |ui| {
                changed |= segmented(
                    ui,
                    &mut o.placement,
                    &[(Placement::Dabs, "Dabs"), (Placement::Ribbon, "Ribbon")],
                    false,
                );
            });
            if o.placement == Placement::Ribbon {
                ui.label(
                    egui::RichText::new(
                        "The picture is laid along the stroke and repeated, its height across \
                         it: ribbons, lace, borders.",
                    )
                    .small()
                    .color(TEXT_DIM),
                );
            }
            let colored = match &o.pixel_shape {
                PixelBrushShape::Custom(t) => {
                    t.has_colors() || o.extra_tips.iter().any(|t| t.has_colors())
                }
                _ => false,
            };
            if colored {
                changed |= ui
                    .checkbox(&mut o.tip_colors, "Paint the picture's colours")
                    .on_hover_text(
                        "Paint with the tip picture's own colours instead of the brush colour \
                         (flowers, stitches, printed ribbons).",
                    )
                    .changed();
                if o.tip_colors {
                    property_row(ui, "Colours", |ui| {
                        use crate::brush_engine::brush_options::TipMapping;
                        let modes: Vec<(TipMapping, &str)> =
                            TipMapping::ALL.iter().map(|&m| (m, m.label())).collect();
                        changed |= segmented(ui, &mut o.tip_mapping, &modes, false);
                    });
                }
            }
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
            if !matches!(brush.brush_options.pixel_shape, PixelBrushShape::Custom(_)) {
                mask_changed |= auto_tip_rows(ui, &mut brush.brush_options.auto_tip);
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
        let o = &mut brush.brush_options;
        match &mut o.auto_spacing {
            Some(coeff) => {
                changed |= slider_row(
                    ui,
                    "Spacing",
                    crate::ui::widgets::reset(coeff, |v| {
                        egui::Slider::new(v, 0.1..=10.0)
                            .logarithmic(true)
                            .max_decimals(2)
                            .suffix("× √size")
                    }),
                )
                .on_hover_text(
                    "Auto spacing: dabs this many times the square root of the brush size \
                     apart, so big brushes keep their dabs closer for their size.",
                )
                .changed();
            }
            None => {
                changed |= slider_row(
                    ui,
                    "Spacing",
                    crate::ui::widgets::reset(&mut o.spacing, |v| {
                        egui::Slider::new(v, 1.0..=200.0)
                            .max_decimals(0)
                            .suffix("%")
                    }),
                )
                .on_hover_text("Distance between dabs, as a percentage of the brush size.")
                .changed();
            }
        }
        let mut auto = o.auto_spacing.is_some();
        if ui
            .checkbox(&mut auto, "Auto spacing")
            .on_hover_text("Space the dabs by the square root of the brush size, as Krita does.")
            .changed()
        {
            o.auto_spacing = auto.then_some(1.0);
            changed = true;
        }
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
        changed |= slider_row(
            ui,
            "Hard edges",
            crate::ui::widgets::reset(&mut brush.sharpness, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text(
            "Cut each dab's faint edge at this share of its strength and paint the rest \
             solid: crisp, aliased edges from any tip (0: off).",
        )
        .changed();
        if brush.sharpness > 0.0 {
            changed |= slider_row(
                ui,
                "Edge softness",
                crate::ui::widgets::reset(&mut brush.sharpness_softness, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            )
            .on_hover_text(
                "Coverage this far below the cut keeps its own strength rather than \
                 vanishing: a softer hard edge.",
            )
            .changed();
        }
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

    changed |= color_source_section(ui, &mut brush.brush_options.color_source, textures);
    changed |= dynamics_sections(ui, &mut brush.dynamics);
    changed |= inputs_section(ui, &mut brush.inputs);
    changed |= texture_section(ui, &mut brush.texture, textures);
    changed |= dual_section(ui, &mut brush.dual, loaded_tips);
    section(ui, "Colour mixing", false, |ui| {
        let mut on = brush.mixing.is_some();
        if ui
            .checkbox(&mut on, "Mix with the paint under the brush")
            .on_hover_text(
                "Like Krita's Colour Smudge: the brush picks up the paint it passes over, \
                 carries it along and mixes in its own colour (the eraser doesn't mix).",
            )
            .changed()
        {
            brush.mixing = on.then(Default::default);
            changed = true;
        }
        let Some(m) = brush.mixing.as_mut() else {
            return;
        };
        changed |= slider_row(
            ui,
            "Smudge length",
            crate::ui::widgets::reset(&mut m.smudge_length, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text("How far the brush drags the paint it picks up.")
        .changed();
        changed |= slider_row(
            ui,
            "Colour rate",
            crate::ui::widgets::reset(&mut m.color_rate, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text(
            "How much brush colour goes in per brush width travelled (0: a blender with no \
             colour of its own).",
        )
        .changed();
        property_row(ui, "Pressure", |ui| {
            changed |= ui.toggle_value(&mut m.pressure_length, "Length").changed();
            changed |= ui.toggle_value(&mut m.pressure_color, "Colour").changed();
        });
        let mut krita = m.krita.is_some();
        if ui
            .checkbox(&mut krita, "Krita's colour smudge")
            .on_hover_text(
                "Mix as Krita's Colour Smudge engine does: each dab reads the layer where the \
                 last dab was and lays it over the paint under it (Smudge length is then \
                 Krita's smudge rate). Brushes imported from Krita's colour smudge use it.",
            )
            .changed()
        {
            m.krita = krita.then(Default::default);
            changed = true;
        }
        if let Some(k) = m.krita.as_mut() {
            property_row(ui, "Mode", |ui| {
                changed |= segmented(
                    ui,
                    &mut k.dulling,
                    &[(false, "Smearing"), (true, "Dulling")],
                    false,
                );
            });
            changed |= ui
                .checkbox(&mut k.smear_alpha, "Smear alpha")
                .on_hover_text("Smudge transparency too, not just the paint over what's there.")
                .changed();
            if k.dulling {
                changed |= slider_row(
                    ui,
                    "Sample radius",
                    crate::ui::widgets::reset(&mut k.radius, |v| {
                        percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                    }),
                )
                .on_hover_text(
                    "Dulling picks up one colour from under the last dab: from this much of \
                     it (0: its centre).",
                )
                .changed();
            }
        }
    });
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
    section(ui, "Impasto", false, |ui| {
        use crate::canvas::impasto::{Impasto, ImpastoMode};
        let mut on = brush.impasto.is_some();
        if ui
            .checkbox(&mut on, "Paint thickness")
            .on_hover_text(
                "The paint has a thickness the layer's light shows (Layer → Impasto Light). \
                 An eraser takes it away.",
            )
            .changed()
        {
            brush.impasto = on.then(Impasto::default);
            changed = true;
        }
        if let Some(imp) = brush.impasto.as_mut() {
            changed |= slider_row(
                ui,
                "Depth",
                crate::ui::widgets::reset(&mut imp.depth, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            )
            .changed();
            changed |= segmented(
                ui,
                &mut imp.mode,
                &[
                    (ImpastoMode::Add, "Build up"),
                    (ImpastoMode::Max, "Level"),
                    (ImpastoMode::Flatten, "Flatten"),
                ],
                true,
            );
        }
    });

    section(ui, "Pen pressure & tilt", true, |ui| {
        let o = &mut brush.brush_options;
        // Wraps: four touch-sized buttons are wider than the panel.
        property_row(ui, "Controls", |ui| {
            ui.horizontal_wrapped(|ui| {
                changed |= ui.toggle_value(&mut o.pressure_size, "Size").changed();
                changed |= ui
                    .toggle_value(&mut o.pressure_opacity, "Opacity")
                    .changed();
                changed |= ui.toggle_value(&mut o.pressure_flow, "Flow").changed();
                changed |= ui
                    .toggle_value(&mut o.pressure_spacing, "Spacing")
                    .on_hover_text("Lighter pressure places the dabs closer together.")
                    .changed();
            });
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
            (o.pressure_spacing, "Spacing", &mut curves.spacing),
        ] {
            if on {
                changed |= pressure_curve_row(ui, name, curve);
            }
        }
    });

    section(ui, "Stabilizer", true, |ui| {
        property_row(ui, "Method", |ui| {
            let label = |a: StabilizerAlgorithm| match a {
                StabilizerAlgorithm::None => "None",
                StabilizerAlgorithm::Simple => "Simple",
                StabilizerAlgorithm::Dynamic => "Dynamic",
                StabilizerAlgorithm::String => "Pulled string",
                StabilizerAlgorithm::PostCorrection => "Post-correction",
                StabilizerAlgorithm::MotionFilter => "Motion filter",
            };
            egui::ComboBox::from_id_salt("stabilizer_algorithm")
                .selected_text(label(brush.stabilizer_algorithm))
                .show_ui(ui, |ui| {
                    for a in [
                        StabilizerAlgorithm::None,
                        StabilizerAlgorithm::Simple,
                        StabilizerAlgorithm::Dynamic,
                        StabilizerAlgorithm::String,
                        StabilizerAlgorithm::PostCorrection,
                        StabilizerAlgorithm::MotionFilter,
                    ] {
                        changed |= ui
                            .selectable_value(&mut brush.stabilizer_algorithm, a, label(a))
                            .changed();
                    }
                });
        });
        let modes = &mut brush.stabilizer_modes;
        match brush.stabilizer_algorithm {
            StabilizerAlgorithm::None => {}
            StabilizerAlgorithm::String => {
                changed |= slider_row(
                    ui,
                    "Length",
                    crate::ui::widgets::reset(&mut modes.string_length, |v| {
                        egui::Slider::new(v, 2.0..=300.0).suffix(" pt")
                    }),
                )
                .on_hover_text(
                    "The brush trails the pen on a string this long: it only moves once \
                     the pen is further away, so the line stays steady.",
                )
                .changed();
                changed |= ui
                    .checkbox(&mut modes.catch_up, "Catch up")
                    .on_hover_text("When the pen lifts, the line goes on to it.")
                    .changed();
            }
            StabilizerAlgorithm::PostCorrection => {
                changed |= slider_row(
                    ui,
                    "Strength",
                    crate::ui::widgets::reset(&mut modes.correction, |v| {
                        percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                    }),
                )
                .on_hover_text(
                    "When the pen lifts, the stroke's path is smoothed and the stroke \
                     painted again along it.",
                )
                .changed();
                changed |= ui
                    .checkbox(&mut modes.correction_live, "While drawing")
                    .on_hover_text(
                        "Smooth the line as it's drawn: the stretch near the pen settles \
                         as you go on. The same line as smoothing at the end.",
                    )
                    .changed();
            }
            StabilizerAlgorithm::MotionFilter => {
                changed |= slider_row(
                    ui,
                    "Strength",
                    crate::ui::widgets::reset(&mut modes.filter_strength, |v| {
                        percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                    }),
                )
                .on_hover_text("How much shake is taken out of slow, careful lines.")
                .changed();
                changed |= slider_row(
                    ui,
                    "Speed",
                    crate::ui::widgets::reset(&mut modes.filter_speed, |v| {
                        percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                    }),
                )
                .on_hover_text(
                    "How quickly the smoothing lets go as the pen speeds up: higher \
                     keeps fast lines direct, with no lag.",
                )
                .changed();
            }
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
        changed |= ui
            .checkbox(&mut t.follow_barrel, "Follow barrel rotation")
            .on_hover_text(
                "Turn the tip as the pen turns about its own axis (pens that report it, \
                 like Wacom's Art Pen).",
            )
            .changed();
        property_row(ui, "Flip", |ui| {
            changed |= ui
                .toggle_value(&mut t.random_flip_x, "↔")
                .on_hover_text(
                    "Mirror about half the dabs left to right, at random (or by an input \
                     mapped to Mirror).",
                )
                .changed();
            changed |= ui
                .toggle_value(&mut t.random_flip_y, "↕")
                .on_hover_text(
                    "Mirror about half the dabs top to bottom, at random (or by an input \
                     mapped to Mirror).",
                )
                .changed();
        });
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

/// Inputs driving settings: any sensor to any dab setting, each with its
/// own amount and curve.
fn inputs_section(
    ui: &mut egui::Ui,
    inputs: &mut Vec<crate::brush_engine::dynamics::InputMapping>,
) -> bool {
    use crate::brush_engine::dynamics::{DabSetting, InputMapping, Sensor};
    let mut changed = false;
    section(ui, "Inputs", false, |ui| {
        ui.label(
            egui::RichText::new(
                "Make any input drive any setting, each through its own curve (like pressure \
                 does size).",
            )
            .small()
            .color(TEXT_DIM),
        );
        let mut remove = None;
        for (i, m) in inputs.iter_mut().enumerate() {
            ui.push_id(("input", i), |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("sensor")
                        .selected_text(m.sensor.label())
                        .show_ui(ui, |ui| {
                            for s in Sensor::ALL {
                                changed |=
                                    ui.selectable_value(&mut m.sensor, s, s.label()).changed();
                            }
                        });
                    ui.label("→");
                    egui::ComboBox::from_id_salt("setting")
                        .selected_text(m.setting.label())
                        .show_ui(ui, |ui| {
                            for s in DabSetting::ALL {
                                changed |=
                                    ui.selectable_value(&mut m.setting, s, s.label()).changed();
                            }
                        });
                    if ui.small_button("✕").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
                changed |= slider_row(
                    ui,
                    "Amount",
                    percent_of_unit(egui::Slider::new(&mut m.amount, -1.0..=1.0)),
                )
                .on_hover_text(
                    "Size, opacity and texture strength: above 0 a high input keeps them full \
                     and a low one reduces them; below 0 the other way. Others: how far a full \
                     input moves them (scatter up to a brush width, colour mix all the way to \
                     the secondary colour).",
                )
                .changed();
                if m.setting.adds() {
                    changed |= ui
                        .checkbox(&mut m.both_ways, "Both ways")
                        .on_hover_text(
                            "The input moves the setting either way: a low input the other \
                             way, the middle not at all.",
                        )
                        .changed();
                }
                if m.sensor.has_length() {
                    let (range, suffix) = match m.sensor {
                        Sensor::Time => (0.01..=10.0, " s"),
                        Sensor::Fade => (1.0..=5000.0, " dabs"),
                        _ => (1.0..=5000.0, " px"),
                    };
                    changed |= slider_row(
                        ui,
                        "Over",
                        egui::Slider::new(&mut m.length, range)
                            .logarithmic(true)
                            .suffix(suffix),
                    )
                    .on_hover_text("How far into the stroke the input reaches its full value.")
                    .changed();
                    changed |= ui
                        .checkbox(&mut m.periodic, "Repeat")
                        .on_hover_text("Start over from nothing each time it reaches full.")
                        .changed();
                }
                changed |= crate::ui::curve_editor::curve_editor_with(
                    ui,
                    &mut m.curve,
                    &crate::ui::curve_editor::INPUT_PRESETS,
                );
            });
        }
        if let Some(i) = remove {
            inputs.remove(i);
            changed = true;
        }
        ui.separator();
        if ui.button("+ Add input").clicked() {
            inputs.push(InputMapping::default());
            changed = true;
        }
    });
    changed
}

/// Paper texture: which pattern, how it combines, its scale and strength.
/// A sketch brush's joining lines.
fn sketch_section(ui: &mut egui::Ui, s: &mut crate::brush_engine::sketch::Sketch) -> bool {
    let mut changed = false;
    section(ui, "Sketch", true, |ui| {
        changed |= slider_row(
            ui,
            "Reach",
            crate::ui::widgets::reset(&mut s.reach, |v| {
                egui::Slider::new(v, 5.0..=300.0)
                    .logarithmic(true)
                    .max_decimals(0)
                    .suffix(" px")
            }),
        )
        .on_hover_text("How far earlier points can be to be joined.")
        .changed();
        changed |= slider_row(
            ui,
            "Density",
            crate::ui::widgets::reset(&mut s.density, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text("How likely each point in reach is joined.")
        .changed();
        changed |= slider_row(
            ui,
            "Line opacity",
            crate::ui::widgets::reset(&mut s.opacity, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Line width",
            crate::ui::widgets::reset(&mut s.thickness, |v| {
                egui::Slider::new(v, 0.5..=100.0)
                    .logarithmic(true)
                    .max_decimals(1)
                    .suffix(" px")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Line offset",
            crate::ui::widgets::reset(&mut s.offset, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=2.0))
            }),
        )
        .on_hover_text(
            "How much of each joining line is left off at both ends: short strokes \
             between the points rather than lines joining them (past half, the ends \
             cross over).",
        )
        .changed();
    });
    changed
}

/// A slider row with a reset (double-click), for the engines' settings.
fn engine_row<T: egui::emath::Numeric + Send + Sync + 'static>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    slider: impl for<'b> FnOnce(&'b mut T) -> egui::Slider<'b>,
) -> bool {
    slider_row(ui, label, crate::ui::widgets::reset(value, slider)).changed()
}

fn px(v: &mut f32, range: std::ops::RangeInclusive<f32>) -> egui::Slider<'_> {
    egui::Slider::new(v, range)
        .logarithmic(true)
        .max_decimals(1)
        .suffix(" px")
}

/// A spray brush's particles.
fn spray_section(ui: &mut egui::Ui, s: &mut crate::brush_engine::engines::Spray) -> bool {
    use crate::brush_engine::engines::Distribution;
    let mut changed = false;
    section(ui, "Spray", true, |ui| {
        changed |= engine_row(ui, "Particles", &mut s.amount, |v| {
            egui::Slider::new(v, 1..=500).logarithmic(true)
        });
        changed |= segmented(
            ui,
            &mut s.distribution,
            &[
                (Distribution::Uniform, "Even"),
                (Distribution::Gaussian, "Middle"),
                (Distribution::Clustered, "Clumps"),
            ],
            true,
        );
        changed |= engine_row(ui, "Particle size", &mut s.particle_size, |v| {
            percent_of_unit(egui::Slider::new(v, 0.01..=1.0).logarithmic(true))
        });
        changed |= engine_row(ui, "Size randomness", &mut s.size_random, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        });
        changed |= ui
            .checkbox(&mut s.random_rotation, "Turned at random")
            .changed();
    });
    changed
}

/// A chalk brush's grain.
fn chalk_section(ui: &mut egui::Ui, c: &mut crate::brush_engine::engines::Chalk) -> bool {
    let mut changed = false;
    section(ui, "Chalk", true, |ui| {
        changed |= engine_row(ui, "Grain", &mut c.grain, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        });
        changed |= engine_row(ui, "Grain size", &mut c.scale, |v| px(v, 0.5..=16.0));
    });
    changed
}

/// A curve brush's lines.
fn curve_section(ui: &mut egui::Ui, c: &mut crate::brush_engine::engines::CurveLines) -> bool {
    let mut changed = false;
    section(ui, "Curves", true, |ui| {
        changed |= engine_row(ui, "Reach back", &mut c.history, |v| {
            egui::Slider::new(v, 3..=200)
                .logarithmic(true)
                .suffix(" points")
        });
        changed |= engine_row(ui, "Line width", &mut c.line_width, |v| px(v, 0.5..=50.0));
        changed |= engine_row(ui, "Line opacity", &mut c.opacity, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        });
        changed |= ui
            .checkbox(&mut c.connection, "Straight lines too")
            .changed();
    });
    changed
}

/// A grid brush's cells.
fn grid_section(ui: &mut egui::Ui, g: &mut crate::brush_engine::engines::Grid) -> bool {
    let mut changed = false;
    section(ui, "Grid", true, |ui| {
        changed |= engine_row(ui, "Cell size", &mut g.cell, |v| px(v, 2.0..=500.0));
        changed |= engine_row(ui, "Shape size", &mut g.scale, |v| {
            percent_of_unit(egui::Slider::new(v, 0.05..=1.5))
        });
        changed |= engine_row(ui, "Offset x", &mut g.offset[0], |v| {
            egui::Slider::new(v, 0.0..=500.0)
                .max_decimals(0)
                .suffix(" px")
        });
        changed |= engine_row(ui, "Offset y", &mut g.offset[1], |v| {
            egui::Slider::new(v, 0.0..=500.0)
                .max_decimals(0)
                .suffix(" px")
        });
        changed |= engine_row(ui, "Hue randomness", &mut g.hue_jitter, |v| {
            egui::Slider::new(v, 0.0..=180.0)
                .max_decimals(0)
                .suffix("°")
        });
    });
    changed
}

/// A tangent normal brush's colours.
fn normal_section(ui: &mut egui::Ui, n: &mut crate::brush_engine::engines::TangentNormal) -> bool {
    let mut changed = false;
    section(ui, "Normal map", true, |ui| {
        changed |= ui.checkbox(&mut n.flip_x, "Flip red").changed();
        changed |= ui.checkbox(&mut n.flip_y, "Flip green (DirectX)").changed();
        changed |= engine_row(ui, "Mouse elevation", &mut n.elevation, |v| {
            egui::Slider::new(v, 0.0..=90.0).max_decimals(0).suffix("°")
        });
    });
    changed
}

/// A particle brush's swarm.
fn particle_section(ui: &mut egui::Ui, p: &mut crate::brush_engine::engines::Particles) -> bool {
    let mut changed = false;
    section(ui, "Particles", true, |ui| {
        changed |= engine_row(ui, "Count", &mut p.count, |v| {
            egui::Slider::new(v, 1..=200).logarithmic(true)
        });
        changed |= engine_row(ui, "Pull", &mut p.weight, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        });
        changed |= engine_row(ui, "Drag", &mut p.drag, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        });
        changed |= engine_row(ui, "Gravity x", &mut p.gravity[0], |v| {
            egui::Slider::new(v, -10.0..=10.0).max_decimals(1)
        });
        changed |= engine_row(ui, "Gravity y", &mut p.gravity[1], |v| {
            egui::Slider::new(v, -10.0..=10.0).max_decimals(1)
        });
        changed |= engine_row(ui, "Line width", &mut p.line_width, |v| px(v, 0.5..=50.0));
        changed |= engine_row(ui, "Spread", &mut p.spread, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=2.0))
        });
    });
    changed
}

/// A hatching brush's lines.
fn hatching_section(ui: &mut egui::Ui, h: &mut crate::brush_engine::hatching::Hatching) -> bool {
    let mut changed = false;
    section(ui, "Hatching", true, |ui| {
        changed |= slider_row(
            ui,
            "Angle",
            crate::ui::widgets::reset(&mut h.angle, |v| {
                egui::Slider::new(v, -90.0..=90.0)
                    .max_decimals(0)
                    .suffix("°")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Spacing",
            crate::ui::widgets::reset(&mut h.separation, |v| {
                egui::Slider::new(v, 2.0..=40.0)
                    .max_decimals(1)
                    .suffix(" px")
            }),
        )
        .changed();
        changed |= slider_row(
            ui,
            "Line width",
            crate::ui::widgets::reset(&mut h.thickness, |v| {
                egui::Slider::new(v, 0.5..=10.0)
                    .max_decimals(1)
                    .suffix(" px")
            }),
        )
        .changed();
        changed |= ui
            .checkbox(&mut h.crosshatch, "Cross-hatch with pressure")
            .on_hover_text("Pressing harder adds a second direction, then a third.")
            .changed();
    });
    changed
}

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

/// Spikes, fades, density and randomness of a round or square tip.
fn auto_tip_rows(ui: &mut egui::Ui, t: &mut crate::brush_engine::brush_options::AutoTip) -> bool {
    let mut changed = slider_row(
        ui,
        "Spikes",
        crate::ui::widgets::reset(&mut t.spikes, |v| egui::Slider::new(v, 2..=50)),
    )
    .on_hover_text(
        "The tip's shape repeated round its centre: squash the tip (Ratio) for a star with \
         this many points (2: the plain shape).",
    )
    .changed();
    for (i, label) in ["Fade across", "Fade down"].into_iter().enumerate() {
        changed |= slider_row(
            ui,
            label,
            crate::ui::widgets::reset(&mut t.fade[i], |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        )
        .on_hover_text(
            "Solid out to this share of the tip's width (across) or height (down), then \
             fading to its edge (100%: no fade).",
        )
        .changed();
    }
    changed |= slider_row(
        ui,
        "Density",
        crate::ui::widgets::reset(&mut t.density, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        }),
    )
    .on_hover_text("Share of the tip's pixels painted, the rest left out at random.")
    .changed();
    changed |= slider_row(
        ui,
        "Randomness",
        crate::ui::widgets::reset(&mut t.randomness, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        }),
    )
    .on_hover_text("How much each pixel's strength varies at random: a grainy, rough tip.")
    .changed();
    changed
}

/// Where the brush's colour comes from.
fn color_source_section(
    ui: &mut egui::Ui,
    source: &mut crate::brush_engine::brush_options::ColorSource,
    loaded: &[std::sync::Arc<crate::brush_engine::texture::Pattern>],
) -> bool {
    use crate::brush_engine::brush_options::ColorSource;
    use crate::brush_engine::texture::builtin;
    let mut changed = false;
    section(ui, "Colour source", false, |ui| {
        property_row(ui, "Colour", |ui| {
            egui::ComboBox::from_id_salt("brush_color_source")
                .selected_text(source.label())
                .show_ui(ui, |ui| {
                    let first = builtin().first().or(loaded.first()).cloned();
                    let options = [
                        Some(ColorSource::Plain),
                        Some(ColorSource::UniformRandom),
                        Some(ColorSource::TotalRandom),
                        first.map(|pattern| ColorSource::Pattern {
                            pattern,
                            scale: 1.0,
                        }),
                    ];
                    for option in options.into_iter().flatten() {
                        let selected =
                            std::mem::discriminant(&option) == std::mem::discriminant(source);
                        if ui.selectable_label(selected, option.label()).clicked() && !selected {
                            *source = option;
                            changed = true;
                        }
                    }
                })
                .response
                .on_hover_text(
                    "The brush colour; a random colour for each dab or each pixel; or a \
                     pattern pinned to the canvas, from the brush colour where it's dark to \
                     the secondary colour where it's light.",
                );
        });
        if let ColorSource::Pattern { pattern, scale } = source {
            property_row(ui, "Pattern", |ui| {
                egui::ComboBox::from_id_salt("brush_color_pattern")
                    .selected_text(pattern.name.as_str())
                    .show_ui(ui, |ui| {
                        for p in builtin().iter().chain(loaded) {
                            let selected = std::sync::Arc::ptr_eq(p, pattern);
                            if ui.selectable_label(selected, &p.name).clicked() && !selected {
                                *pattern = p.clone();
                                changed = true;
                            }
                        }
                    });
            });
            changed |= slider_row(
                ui,
                "Scale",
                crate::ui::widgets::reset(scale, |v| {
                    egui::Slider::new(v, 0.25..=4.0)
                        .logarithmic(true)
                        .max_decimals(2)
                        .suffix("×")
                }),
            )
            .changed();
        }
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
            egui::ComboBox::from_id_salt("brush_texture_mode")
                .selected_text(t.mode.label())
                .show_ui(ui, |ui| {
                    for m in TextureMode::ALL {
                        if ui.selectable_value(&mut t.mode, m, m.label()).clicked() {
                            // Picking a mode uses this app's formula for it.
                            t.krita = None;
                            changed = true;
                        }
                    }
                });
        });
        if let Some(k) = t.krita {
            property_row(ui, "Krita", |ui| {
                ui.label(format!(
                    "{} ({})",
                    k.label(),
                    if k.soft { "soft texturing" } else { "classic" }
                ))
                .on_hover_text(
                    "This brush came from Krita: its texture combines by Krita's own formula \
                     for this mode. Pick a mode above to use this app's instead.",
                );
            });
        }
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
        let place = &mut t.placement;
        changed |= slider_row(
            ui,
            "Angle",
            crate::ui::widgets::reset(&mut place.angle, |v| {
                egui::Slider::new(v, -180.0..=180.0)
                    .max_decimals(0)
                    .suffix("°")
            }),
        )
        .on_hover_text("Turn the grain.")
        .changed();
        changed |= ui
            .checkbox(&mut place.follow_stroke, "Moves with the stroke")
            .on_hover_text(
                "The grain starts where each stroke starts, rather than being pinned to the \
                 canvas like paper.",
            )
            .changed();
        changed |= ui
            .checkbox(&mut place.random_offset, "Random offset each stroke")
            .on_hover_text(
                "Shift the grain by a random amount for every stroke (every dab, with Each dab).",
            )
            .changed();
        changed |= ui
            .checkbox(&mut place.per_dab, "Each dab")
            .on_hover_text(
                "Every dab gets the grain afresh, centred on itself, rather than the stroke \
                 sharing one sheet of it: a stamped look.",
            )
            .changed();
        ui.label(
            egui::RichText::new(match t.mode {
                _ if t.krita.is_some() => "Krita's texturing, as Krita paints it.",
                TextureMode::Multiply => "The grain darkens the stroke evenly.",
                TextureMode::Subtract => "Low spots lose paint first; heavy strokes fill in.",
                TextureMode::Height => {
                    "Light pressure only catches the peaks; press harder to fill the valleys."
                }
                TextureMode::ColorDodge => "The peaks strengthen the paint: grainy, bright edges.",
                TextureMode::HardMix => {
                    "Paint snaps to full or nothing along the grain: a crisp, broken edge."
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
