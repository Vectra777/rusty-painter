//! The options bar under the menus: one row of controls per tool. Some
//! rows (fill, liquify, gradient, transform) are reused by the side panels
//! and the touch context bar.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::transform;
use crate::brush_engine::brush_options::PaintingMode;
use crate::selection::SelectionType;
use crate::ui::style::*;
use crate::ui::widgets::{bar_frame, percent_of_unit, segmented, vdivider};
use eframe::egui::{self, RichText};

/// Context bar under the menus showing the active tool's options.
pub fn options_bar(app: &mut PainterApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("tool_options")
        .exact_height(metrics(ctx).bar_height)
        .frame(bar_frame(BG_PANEL))
        .show(ctx, |ui| {
            // Options wider than the window scroll sideways instead of
            // being cut off.
            let height = ui.available_height();
            egui::ScrollArea::horizontal()
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| options_row(app, ui),
                    );
                });
        });
}

fn options_row(app: &mut PainterApp, ui: &mut egui::Ui) {
    // Desktop only (tablets use the canvas faders); hints fill the right.
    let hint = match app.active_tool {
        Tool::Brush => brush_options(app, ui),
        Tool::Select(kind) => select_options(app, ui, kind),
        Tool::Transform(_) => transform_options(app, ui),
        Tool::Eyedropper => eyedropper_options(app, ui),
        Tool::Fill => fill_options(app, ui, true),
        Tool::Liquify => liquify_options(app, ui, true),
        Tool::Smudge | Tool::Blur => blend_options(app, ui),
        Tool::Shape(kind) => shape_options(app, ui, kind),
        Tool::Gradient => gradient_options(app, ui, true),
    };
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Hints are optional: drop them rather than overlap the options.
        let font = egui::TextStyle::Small.resolve(ui.style());
        let width = ui
            .painter()
            .layout_no_wrap(hint.into(), font, TEXT_DIM)
            .size()
            .x;
        if width + 12.0 < ui.available_width() {
            ui.label(RichText::new(hint).small().color(TEXT_DIM));
        }
    });
}

fn tool_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).strong().color(TEXT_STRONG));
    vdivider(ui);
}

fn bar_slider(ui: &mut egui::Ui, label: &str, width: f32, slider: egui::Slider) -> bool {
    ui.label(RichText::new(label).color(TEXT_DIM));
    let scale = if metrics(ui.ctx()).touch { 1.4 } else { 1.0 };
    ui.spacing_mut().slider_width = width * scale;
    ui.add(slider).changed()
}

fn brush_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    let eraser = app.is_eraser_active();
    tool_title(ui, if eraser { "Eraser" } else { "Brush" });

    let brush = &mut app.brush_state.brush;
    let mut changed = false;
    let size_changed = bar_slider(
        ui,
        "Size",
        140.0,
        egui::Slider::new(&mut brush.brush_options.diameter, 1.0..=3000.0)
            .logarithmic(true)
            .max_decimals(0)
            .suffix(" px"),
    );
    ui.add_space(6.0);
    changed |= bar_slider(
        ui,
        "Opacity",
        100.0,
        percent_of_unit(egui::Slider::new(
            &mut brush.brush_options.opacity,
            0.0..=1.0,
        )),
    );
    ui.add_space(6.0);
    changed |= bar_slider(
        ui,
        "Flow",
        100.0,
        egui::Slider::new(&mut brush.brush_options.flow, 0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    vdivider(ui);
    changed |= segmented(
        ui,
        &mut brush.brush_options.painting_mode,
        &[
            (PaintingMode::BuildUp, "Build-up"),
            (PaintingMode::Wash, "Wash"),
        ],
        true,
    );

    if size_changed {
        brush.is_changed = true;
    }
    // Size doesn't change the (fixed-size) preview.
    if changed {
        app.brush_state.brush_preview.dirty = true;
    }
    vdivider(ui);
    crate::ui::shape_menu::ruler_controls(app, ui);

    "[ ] size  ·  Alt+click pick color  ·  Space drag to pan"
}

/// Smudge / Blur: the brush's size, strength (flow) and softness, plus the
/// tool's own setting. The rest (spacing, pressure...) is in the brush panel.
fn blend_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    let smudge = matches!(app.active_tool, Tool::Smudge);
    tool_title(ui, if smudge { "Smudge" } else { "Blur" });
    let o = &mut app.brush_state.brush.brush_options;
    let size_changed = bar_slider(
        ui,
        "Size",
        140.0,
        egui::Slider::new(&mut o.diameter, 1.0..=3000.0)
            .logarithmic(true)
            .max_decimals(0)
            .suffix(" px"),
    );
    ui.add_space(6.0);
    bar_slider(
        ui,
        "Strength",
        100.0,
        egui::Slider::new(&mut o.flow, 0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    ui.add_space(6.0);
    bar_slider(
        ui,
        "Hardness",
        100.0,
        egui::Slider::new(&mut o.hardness, 0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    vdivider(ui);
    let b = &mut app.workspace.blend;
    if smudge {
        bar_slider(
            ui,
            "Length",
            100.0,
            percent_of_unit(egui::Slider::new(&mut b.smudge_length, 0.0..=1.0)),
        );
    } else {
        bar_slider(
            ui,
            "Blur size",
            100.0,
            percent_of_unit(egui::Slider::new(&mut b.blur_size, 0.05..=1.0)),
        );
    }
    if size_changed {
        app.brush_state.brush.is_changed = true;
    }
    if smudge {
        "Drag to smear the paint  ·  Length: how far colour is carried  ·  uses the brush's spacing & pressure"
    } else {
        "Paint over edges to soften them  ·  uses the brush's spacing & pressure"
    }
}

/// Gradient shape, colours, repetition, opacity, apply/cancel. `compact`
/// lays it out in a row (bars).
pub(crate) fn gradient_options(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    compact: bool,
) -> &'static str {
    use crate::app::tools::gradient::GradientColors;
    use crate::canvas::gradient::{GradientRepeat, GradientShape};
    if compact {
        tool_title(ui, "Gradient");
    }
    let s = &mut app.workspace.gradient.settings;
    let mut changed = segmented(
        ui,
        &mut s.shape,
        &[
            (GradientShape::Linear, "Linear"),
            (GradientShape::Radial, "Radial"),
            (GradientShape::Reflected, "Reflected"),
            (GradientShape::Angle, "Angle"),
        ],
        compact,
    );
    if compact {
        vdivider(ui);
    }
    changed |= segmented(
        ui,
        &mut s.colors,
        &[
            (GradientColors::ForegroundToBackground, "To secondary"),
            (GradientColors::ForegroundToTransparent, "To clear"),
        ],
        compact,
    );
    if compact {
        vdivider(ui);
    }
    changed |= segmented(
        ui,
        &mut s.repeat,
        &[
            (GradientRepeat::None, "Once"),
            (GradientRepeat::Repeat, "Repeat"),
            (GradientRepeat::Mirror, "Mirror"),
        ],
        compact,
    );
    changed |= ui.checkbox(&mut s.reverse, "Reverse").changed();
    if compact {
        vdivider(ui);
    }
    changed |= bar_slider(
        ui,
        "Opacity",
        90.0,
        percent_of_unit(egui::Slider::new(&mut s.opacity, 0.0..=1.0)),
    );
    changed |= ui
        .checkbox(&mut s.dither, "Dither")
        .on_hover_text("Breaks up the bands of a smooth gradient")
        .changed();
    if changed {
        app.gradient_settings_changed();
    }
    if compact {
        vdivider(ui);
    }
    let placing = app.workspace.gradient.session.is_some();
    if ui
        .add_enabled(placing, egui::Button::new("Apply"))
        .on_hover_text("Keep the gradient (Enter)")
        .clicked()
    {
        app.gradient_commit();
    }
    if ui
        .add_enabled(placing, egui::Button::new("Cancel"))
        .on_hover_text("Take it back off (Esc)")
        .clicked()
    {
        app.gradient_cancel();
    }
    "Drag from the first colour to the second  ·  Shift: 15° steps  ·  drag the ends to adjust  ·  Enter apply"
}

fn shape_options(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    kind: crate::app::tools::shape::ShapeKind,
) -> &'static str {
    tool_title(ui, "Shape");
    crate::ui::shape_menu::shape_controls(app, ui, true);
    vdivider(ui);
    crate::ui::shape_menu::shape_actions(app, ui);
    use crate::app::tools::shape::ShapeKind;
    match kind {
        ShapeKind::Polygon => {
            "Click to add points  ·  click the first point or double-click to finish  ·  Backspace removes a point  ·  Enter apply"
        }
        ShapeKind::Line => {
            "Drag to draw  ·  Shift: 15° steps  ·  drag the ends to adjust  ·  Enter apply  ·  Esc cancel"
        }
        _ => {
            "Drag to draw  ·  Shift: square / circle  ·  Alt: from the centre  ·  Enter apply  ·  Esc cancel"
        }
    }
}

fn select_options(app: &mut PainterApp, ui: &mut egui::Ui, kind: SelectionType) -> &'static str {
    tool_title(ui, "Selection");
    let types = &crate::ui::select_menu::TYPES;
    let label = types.iter().find(|t| t.0 == kind).map_or("", |t| t.2);
    let mut picked = None;
    egui::ComboBox::from_id_salt("selection_type")
        .selected_text(label)
        .show_ui(ui, |ui| {
            for &(t, _, name, _) in types {
                if ui.selectable_label(t == kind, name).clicked() {
                    picked = Some(t);
                }
            }
        });
    if let Some(t) = picked {
        app.set_select_tool(t);
    }
    vdivider(ui);
    crate::ui::select_menu::mode_and_brush_controls(app, ui, true);
    vdivider(ui);
    crate::ui::select_menu::selection_actions(app, ui);
    match kind {
        SelectionType::Wand => {
            "Click an area to select it  ·  Shift add  ·  Alt erase  ·  Q toggles colour range"
        }
        SelectionType::ColorRange => {
            "Click a colour to select it everywhere  ·  Shift add  ·  Alt erase"
        }
        SelectionType::Magnetic => {
            "Click along an edge  ·  click the start or double-click to close  ·  Backspace removes a point  ·  Esc cancels"
        }
        _ => "Shift add  ·  Alt erase  ·  Ctrl+A all  ·  Ctrl+Shift+I invert",
    }
}

fn transform_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    tool_title(ui, "Transform");
    transform_controls(app, ui);
    "Drag inside to move  ·  outside to rotate  ·  Shift keeps proportions  ·  Enter apply  ·  Esc cancel"
}

/// Mode, flips, quarter turns, apply and cancel. Shared with the touch bar.
pub(crate) fn transform_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let distort = matches!(app.active_tool, Tool::Transform(info) if info.corners.is_some());
    let mut mode = distort;
    segmented(ui, &mut mode, &[(false, "Free"), (true, "Distort")], true);
    if mode != distort {
        transform::set_distort(app, mode);
    }
    vdivider(ui);
    if ui
        .button("Flip H")
        .on_hover_text("Mirror horizontally")
        .clicked()
    {
        transform::flip(app, true);
    }
    if ui
        .button("Flip V")
        .on_hover_text("Mirror vertically")
        .clicked()
    {
        transform::flip(app, false);
    }
    if ui
        .button("−90°")
        .on_hover_text("Rotate 90° counter-clockwise")
        .clicked()
    {
        transform::rotate_quarter(app, false);
    }
    if ui
        .button("+90°")
        .on_hover_text("Rotate 90° clockwise")
        .clicked()
    {
        transform::rotate_quarter(app, true);
    }
    vdivider(ui);
    ui.checkbox(&mut app.workspace.transform_pick_layer, "Pick layer")
        .on_hover_text(
            "Click an image or drawing on another (unlocked) layer to select and transform it",
        );
    vdivider(ui);
    let floating = app.layer_state.floating_layer_idx.is_some();
    if ui
        .add_enabled(floating, egui::Button::new("Apply"))
        .on_hover_text("Apply the transform (Enter)")
        .clicked()
    {
        transform::commit_floating_layer(app);
    }
    if ui
        .add_enabled(floating, egui::Button::new("Cancel"))
        .on_hover_text("Put everything back (Esc)")
        .clicked()
    {
        transform::cancel_floating_layer(app);
    }
}

/// Fill mode, reference and line handling. `compact` lays it out in a row
/// (options bar); otherwise stacked (tool panel).
pub(crate) fn fill_options(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) -> &'static str {
    use crate::app::tools::fill::{FillMode, FillSource};
    if compact {
        tool_title(ui, "Fill");
    }
    let f = &mut app.workspace.fill;
    segmented(
        ui,
        &mut f.mode,
        &[(FillMode::Bucket, "Bucket"), (FillMode::Enclose, "Enclose")],
        compact,
    );
    if compact {
        vdivider(ui);
        ui.label(RichText::new("Look at").color(TEXT_DIM));
    }
    segmented(
        ui,
        &mut f.source,
        &[
            (FillSource::CurrentLayer, "Layer"),
            (FillSource::LayerBelow, "Below"),
            (FillSource::AllVisible, "All"),
        ],
        compact,
    );
    let s = &mut f.settings;
    let rows: [(&str, &mut u8, std::ops::RangeInclusive<u8>, &str); 3] = [
        (
            "Tolerance",
            &mut s.tolerance,
            0..=255,
            "How different a colour can be and still count as the same area",
        ),
        (
            "Close gaps",
            &mut s.gap,
            0..=40,
            "Don't leak through openings in the lines up to this wide (px)",
        ),
        (
            "Under lines",
            &mut s.expand,
            0..=12,
            "Grow the fill under the line art (px), so no halo is left",
        ),
    ];
    for (label, value, range, tip) in rows {
        if compact {
            vdivider(ui);
            ui.label(RichText::new(label).color(TEXT_DIM))
                .on_hover_text(tip);
            ui.add(egui::Slider::new(value, range).show_value(true))
                .on_hover_text(tip);
        } else {
            crate::ui::widgets::slider_row(ui, label, egui::Slider::new(value, range))
                .on_hover_text(tip);
        }
    }
    if compact {
        vdivider(ui);
    }
    ui.checkbox(&mut s.antialias, "Smooth edge");
    match f.mode {
        FillMode::Bucket => "Click an area to fill it  ·  G toggles Enclose",
        FillMode::Enclose => {
            "Draw around the areas to fill, even across lines  ·  G toggles Bucket"
        }
    }
}

/// Liquify mode, size, strength, apply and cancel.
pub(crate) fn liquify_options(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    compact: bool,
) -> &'static str {
    use crate::canvas::liquify::LiquifyMode;
    if compact {
        tool_title(ui, "Liquify");
    }
    let s = &mut app.workspace.liquify;
    if compact {
        let label = LiquifyMode::ALL
            .iter()
            .find(|m| m.0 == s.mode)
            .map_or("", |m| m.1);
        egui::ComboBox::from_id_salt("liquify_mode")
            .selected_text(label)
            .show_ui(ui, |ui| {
                for (mode, name) in LiquifyMode::ALL {
                    ui.selectable_value(&mut s.mode, mode, name);
                }
            });
        vdivider(ui);
        ui.label(RichText::new("Size").color(TEXT_DIM));
        ui.add(
            egui::Slider::new(&mut s.radius, 4.0..=600.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px"),
        );
        ui.label(RichText::new("Strength").color(TEXT_DIM));
        ui.add(percent_of_unit(egui::Slider::new(
            &mut s.strength,
            0.02..=1.0,
        )));
        vdivider(ui);
    } else {
        ui.horizontal_wrapped(|ui| {
            for (mode, name) in LiquifyMode::ALL {
                ui.selectable_value(&mut s.mode, mode, name);
            }
        });
        crate::ui::widgets::slider_row(
            ui,
            "Size",
            egui::Slider::new(&mut s.radius, 4.0..=600.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px"),
        );
        crate::ui::widgets::slider_row(
            ui,
            "Strength",
            percent_of_unit(egui::Slider::new(&mut s.strength, 0.02..=1.0)),
        );
    }
    let active = app.layer_state.liquify.is_some();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(active, egui::Button::new("Apply"))
            .on_hover_text("Keep the result (Enter)")
            .clicked()
        {
            app.liquify_commit();
        }
        if ui
            .add_enabled(active, egui::Button::new("Cancel"))
            .on_hover_text("Put the layer back (Esc)")
            .clicked()
        {
            app.liquify_cancel();
        }
    });
    "Drag to push  ·  hold still to twirl / pinch / bloat  ·  [ ] size  ·  Enter apply  ·  Esc cancel"
}

fn eyedropper_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    tool_title(ui, "Eyedropper");
    let color = app.brush_state.brush.brush_options.color;
    crate::ui::widgets::color_swatch(ui, color, egui::vec2(36.0, 18.0));
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    ui.label(
        RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
            .monospace()
            .color(TEXT),
    );
    "Click or drag on the canvas to sample the visible color"
}
