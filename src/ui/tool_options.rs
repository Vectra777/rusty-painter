//! The active tool's options, one row of controls per tool, shown in the
//! top bar beside the menus. Some rows (fill, liquify, gradient,
//! transform) are reused by the side panels and the touch context bar.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::transform;
use crate::selection::SelectionType;
use crate::ui::style::*;
use crate::ui::widgets::{percent_of_unit, segmented, vdivider};
use eframe::egui::{self, RichText};

/// The active tool's options in the space `ui` has; wider than that, they
/// scroll sideways instead of being cut off.
pub fn options_inline(app: &mut PainterApp, ui: &mut egui::Ui) {
    let height = ui.available_height();
    egui::ScrollArea::horizontal()
        .id_salt("tool_options")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| options_row(app, ui),
            );
        });
}

fn options_row(app: &mut PainterApp, ui: &mut egui::Ui) {
    // Desktop only (tablets have size and opacity sliders); hints fill the right.
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
        Tool::VectorEdit => {
            tool_title(ui, "Edit Lines");
            if ui.button("Back to brush").clicked() {
                app.active_tool = Tool::Brush;
            }
            "Click a line; drag a handle to bend it, Shift+drag to widen, Alt+drag to move it, Delete removes a handle"
        }
        Tool::Animate => {
            tool_title(ui, "Animate");
            crate::ui::motion_panel::motion_fields(app, ui, false);
            "Drag to move, a corner to scale, the round handle to turn, Alt+drag to move the pivot (Shift snaps); keyed at this frame"
        }
        Tool::Text => {
            tool_title(ui, "Text");
            crate::ui::text_dialog::text_controls(app, ui);
            "Click to type; drag the text to move it; Ctrl+Enter keeps it"
        }
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
            let hint = crate::app::input::keyboard::with_keycaps(ui.ctx(), hint);
            ui.label(RichText::new(hint).small().color(TEXT_DIM));
        }
    });
}

fn tool_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).strong().color(TEXT_STRONG));
    vdivider(ui);
}

fn bar_slider(ui: &mut egui::Ui, label: &str, width: f32, slider: impl egui::Widget) -> bool {
    ui.label(RichText::new(label).color(TEXT_DIM));
    let scale = if metrics(ui.ctx()).touch { 1.4 } else { 1.0 };
    ui.spacing_mut().slider_width = width * scale;
    ui.add(slider).changed()
}

fn brush_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    let eraser = app.is_eraser_active();
    tool_title(ui, if eraser { "Eraser" } else { "Brush" });
    // Vector lines have no flow, and are erased whole or in part.
    let vector = app.is_vector_layer(app.canvas.active_layer_idx);

    let brush = &mut app.brush_state.brush;
    let mut changed = false;
    let size_changed = bar_slider(
        ui,
        "Size",
        140.0,
        crate::ui::widgets::reset(&mut brush.brush_options.diameter, |v| {
            egui::Slider::new(v, 1.0..=3000.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px")
        }),
    );
    if !(vector && eraser) {
        ui.add_space(6.0);
        changed |= bar_slider(
            ui,
            "Opacity",
            100.0,
            crate::ui::widgets::reset(&mut brush.brush_options.opacity, |v| {
                percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
            }),
        );
    }
    if !vector {
        ui.add_space(6.0);
        changed |= bar_slider(
            ui,
            "Flow",
            100.0,
            crate::ui::widgets::reset(&mut brush.brush_options.flow, |v| {
                egui::Slider::new(v, 0.0..=100.0)
                    .max_decimals(0)
                    .suffix("%")
            }),
        );
    }

    if size_changed {
        brush.is_changed = true;
    }
    // Size doesn't change the (fixed-size) preview.
    if changed {
        app.brush_state.brush_preview.dirty = true;
    }
    // Kept to the essentials: the rest is in the brush panel, the ruler in
    // the View menu (R).

    // On a vector layer the eraser takes out lines, whole or in part.
    if vector {
        use crate::app::tools::vector::VectorErase;
        ui.add_space(6.0);
        if eraser {
            ui.label(RichText::new("Erase").small().color(TEXT_DIM));
            crate::ui::widgets::segmented(
                ui,
                &mut app.workspace.vector.erase,
                &[
                    (VectorErase::WholeLine, "Whole line"),
                    (VectorErase::Touched, "Touched part"),
                    (VectorErase::ToCrossing, "To crossings"),
                ],
                true,
            );
        } else {
            ui.label(RichText::new("Vector layer").small().color(ACCENT))
                .on_hover_text("Lines stay editable: Layer → Vector");
            if ui.small_button("Edit lines").clicked() {
                app.active_tool = Tool::VectorEdit;
            }
        }
    }

    "{[} {]} size  ·  Alt+click pick color  ·  Space drag to pan"
}

/// Smudge / Blur: the brush's size, strength (flow) and softness, plus the
/// tool's own setting. The rest (spacing, pressure...) is in the brush panel.
fn blend_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    use crate::app::tools::blend::{DeformMode, FilterMode, SmudgeMode};
    let smudge = matches!(app.active_tool, Tool::Smudge);
    let (smudge_mode, filter_mode) = (
        app.workspace.blend.smudge_mode,
        app.workspace.blend.filter_mode,
    );
    tool_title(
        ui,
        match (smudge, smudge_mode, filter_mode) {
            (true, SmudgeMode::Smudge, _) => "Smudge",
            (true, SmudgeMode::Deform, _) => "Deform",
            (true, SmudgeMode::Clone, _) => "Clone",
            (false, _, FilterMode::Blur) => "Blur",
            (false, _, FilterMode::Sharpen) => "Sharpen",
            (false, _, FilterMode::Adjust) => "Adjust",
            (false, _, FilterMode::Filter) => "Filter",
        },
    );
    {
        let b = &mut app.workspace.blend;
        if smudge {
            segmented(
                ui,
                &mut b.smudge_mode,
                &[
                    (SmudgeMode::Smudge, "Smudge"),
                    (SmudgeMode::Deform, "Deform"),
                    (SmudgeMode::Clone, "Clone"),
                ],
                true,
            );
        } else {
            segmented(
                ui,
                &mut b.filter_mode,
                &[
                    (FilterMode::Blur, "Blur"),
                    (FilterMode::Sharpen, "Sharpen"),
                    (FilterMode::Adjust, "Adjust"),
                    (FilterMode::Filter, "Filter"),
                ],
                true,
            );
        }
    }
    vdivider(ui);
    let o = &mut app.brush_state.brush.brush_options;
    let size_changed = bar_slider(
        ui,
        "Size",
        140.0,
        crate::ui::widgets::reset(&mut o.diameter, |v| {
            egui::Slider::new(v, 1.0..=3000.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px")
        }),
    );
    ui.add_space(6.0);
    bar_slider(
        ui,
        "Strength",
        100.0,
        crate::ui::widgets::reset(&mut o.flow, |v| {
            egui::Slider::new(v, 0.0..=100.0)
                .max_decimals(0)
                .suffix("%")
        }),
    );
    ui.add_space(6.0);
    bar_slider(
        ui,
        "Hardness",
        100.0,
        crate::ui::widgets::reset(&mut o.hardness, |v| {
            egui::Slider::new(v, 0.0..=100.0)
                .max_decimals(0)
                .suffix("%")
        }),
    );
    vdivider(ui);
    let b = &mut app.workspace.blend;
    let hint = match (smudge, b.smudge_mode, b.filter_mode) {
        (true, SmudgeMode::Smudge, _) => {
            bar_slider(
                ui,
                "Length",
                100.0,
                crate::ui::widgets::reset(&mut b.smudge_length, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            );
            ui.add_space(6.0);
            bar_slider(
                ui,
                "Colour",
                80.0,
                crate::ui::widgets::reset(&mut b.color_rate, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            );
            "Drag to smear the paint  ·  Length: how far colour is carried  ·  Colour above 0: a wet brush mixing in the brush colour"
        }
        (true, SmudgeMode::Deform, _) => {
            egui::ComboBox::from_id_salt("deform_mode")
                .selected_text(b.deform_mode.label())
                .show_ui(ui, |ui| {
                    for mode in DeformMode::ALL {
                        ui.selectable_value(&mut b.deform_mode, mode, mode.label());
                    }
                });
            ui.add_space(6.0);
            bar_slider(
                ui,
                "Amount",
                100.0,
                crate::ui::widgets::reset(&mut b.deform_amount, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
                }),
            );
            "Push the paint along, grow or shrink it from the middle, or swirl it round"
        }
        (true, SmudgeMode::Clone, _) => {
            ui.checkbox(&mut b.clone_aligned, "Aligned")
                .on_hover_text("Keep the same offset from stroke to stroke");
            ui.checkbox(&mut b.clone_merged, "All layers")
                .on_hover_text("Copy what's visible, not only this layer");
            if b.clone_source.is_none() {
                "Ctrl+click where to copy from, then paint"
            } else {
                "Paint with the pixels from the source  ·  Ctrl+click to move it"
            }
        }
        (false, _, FilterMode::Blur) => {
            bar_slider(
                ui,
                "Blur size",
                100.0,
                crate::ui::widgets::reset(&mut b.blur_size, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.05..=1.0))
                }),
            );
            "Paint over edges to soften them  ·  uses the brush's spacing & pressure"
        }
        (false, _, FilterMode::Sharpen) => {
            bar_slider(
                ui,
                "Amount",
                100.0,
                crate::ui::widgets::reset(&mut b.sharpen_amount, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.0..=2.0))
                }),
            );
            ui.add_space(6.0);
            bar_slider(
                ui,
                "Radius",
                80.0,
                crate::ui::widgets::reset(&mut b.blur_size, |v| {
                    percent_of_unit(egui::Slider::new(v, 0.05..=1.0))
                }),
            );
            "Paint over details to crisp them up"
        }
        (false, _, FilterMode::Adjust) => {
            bar_slider(
                ui,
                "Hue",
                90.0,
                crate::ui::widgets::reset(&mut b.adjust_hue, |v| {
                    egui::Slider::new(v, -180.0..=180.0)
                        .max_decimals(0)
                        .suffix("°")
                }),
            );
            ui.add_space(6.0);
            bar_slider(
                ui,
                "Saturation",
                90.0,
                crate::ui::widgets::reset(&mut b.adjust_saturation, |v| {
                    egui::Slider::new(v, -1.0..=1.0).max_decimals(2)
                }),
            );
            ui.add_space(6.0);
            bar_slider(
                ui,
                "Brightness",
                90.0,
                crate::ui::widgets::reset(&mut b.adjust_value, |v| {
                    egui::Slider::new(v, -1.0..=1.0).max_decimals(2)
                }),
            );
            "Paint to shift the colours under the brush"
        }
        (false, _, FilterMode::Filter) => {
            use crate::canvas::filters::Filter;
            let current = b.brush_filter;
            egui::ComboBox::from_id_salt("brush_filter")
                .selected_text(current.name())
                .show_ui(ui, |ui| {
                    for (i, group) in Filter::MENU.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        for &f in group.iter() {
                            let same =
                                std::mem::discriminant(&f) == std::mem::discriminant(&current);
                            if ui.selectable_label(same, f.name()).clicked() && !same {
                                b.brush_filter = f;
                            }
                        }
                    }
                });
            if b.brush_filter.has_settings() {
                ui.add_space(6.0);
                ui.menu_button("Settings…", |ui| {
                    ui.set_min_width(300.0);
                    crate::ui::filter_dialog::settings(ui, &mut b.brush_filter);
                });
            }
            "Paint the filter on through the brush  ·  going over a spot again doesn't filter it twice"
        }
    };
    if size_changed {
        app.brush_state.brush.is_changed = true;
    }
    hint
}

/// Gradient shape, colours, repetition, opacity, apply/cancel. `compact`
/// lays it out in a row (bars).
pub(crate) fn gradient_options(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    compact: bool,
) -> &'static str {
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
    let brush_colors = (
        app.brush_state.brush.brush_options.color,
        app.brush_state.secondary_color,
    );
    let (picked, edit) = gradient_picker(ui, &mut app.workspace.gradient, brush_colors);
    changed |= picked;
    if let Some(copy) = edit {
        app.gradient_edit(copy);
    }
    let s = &mut app.workspace.gradient.settings;
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
        crate::ui::widgets::reset(&mut s.opacity, |v| {
            percent_of_unit(egui::Slider::new(v, 0.0..=1.0))
        }),
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

/// The gradient's colours as a strip; a click lists every choice (the brush
/// colours, the presets and the user's own) with a preview of each. Returns
/// whether the choice changed, and whether the editor was asked for (on a
/// new copy: `Some(true)`).
fn gradient_picker(
    ui: &mut egui::Ui,
    state: &mut crate::app::tools::gradient::GradientToolState,
    (primary, secondary): (egui::Color32, egui::Color32),
) -> (bool, Option<bool>) {
    use crate::app::tools::gradient::GradientColors;
    use crate::ui::widgets::paint_gradient_strip;
    let id = ui.make_persistent_id("gradient_colors");
    let height = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(96.0, height), egui::Sense::click());
    let library = &state.library;
    let colors = &mut state.settings.colors;
    paint_gradient_strip(
        ui.painter(),
        rect.shrink(3.0),
        &library.stops(*colors, primary, secondary),
    );
    if response.hovered() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            0.0,
            egui::Stroke::new(1.0_f32, TEXT_STRONG),
        );
    }
    let response =
        response.on_hover_text(format!("{}  ·  click for presets", library.name(*colors)));
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    let mut edit = ui
        .button("Edit…")
        .on_hover_text("Change the colours in the Gradient Editor (a preset is copied first)")
        .clicked()
        .then_some(false);
    let mut changed = false;
    let row_height = if metrics(ui.ctx()).touch { 36.0 } else { 24.0 };
    egui::popup_below_widget(
        ui,
        id,
        &response,
        egui::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(230.0);
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for choice in library.choices() {
                        if choice == GradientColors::Custom(0) {
                            ui.separator();
                            ui.label(
                                RichText::new("YOUR GRADIENTS")
                                    .small()
                                    .strong()
                                    .color(TEXT_DIM),
                            );
                        }
                        let (row, item) = ui.allocate_exact_size(
                            egui::vec2(230.0, row_height),
                            egui::Sense::click(),
                        );
                        if choice == *colors {
                            ui.painter().rect_filled(row, 2.0, ACCENT_DIM);
                        } else if item.hovered() {
                            ui.painter().rect_filled(row, 2.0, WIDGET_HOVER);
                        }
                        let strip = egui::Rect::from_min_size(
                            row.min + egui::vec2(4.0, 4.0),
                            egui::vec2(88.0, row.height() - 8.0),
                        );
                        paint_gradient_strip(
                            ui.painter(),
                            strip,
                            &library.stops(choice, primary, secondary),
                        );
                        ui.painter().text(
                            egui::pos2(strip.right() + 8.0, row.center().y),
                            egui::Align2::LEFT_CENTER,
                            library.name(choice),
                            egui::TextStyle::Body.resolve(ui.style()),
                            TEXT,
                        );
                        if item.clicked() && choice != *colors {
                            *colors = choice;
                            changed = true;
                        }
                    }
                });
            ui.separator();
            if ui
                .button("New gradient…")
                .on_hover_text("A copy of the chosen gradient, to change in the editor")
                .clicked()
            {
                edit = Some(true);
            }
        },
    );
    (changed, edit)
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
        ShapeKind::Curve => {
            "Click for a corner, drag for a smooth point  ·  click the first point or double-click to finish  ·  Alt+drag a handle: sharp turn  ·  Enter apply"
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
        SelectionType::Polygon => {
            "Click each corner  ·  click the start, double-click or Enter to close  ·  Backspace removes a point  ·  Esc cancels"
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
    use crate::canvas::storage::DistortKind;
    let current = match app.active_tool {
        Tool::Transform(info) => info.point_mode(),
        _ => None,
    };
    let mut mode = current;
    segmented(
        ui,
        &mut mode,
        &[
            (None, "Free"),
            (Some(DistortKind::Perspective), "Perspective"),
            (Some(DistortKind::Warp), "Distort"),
        ],
        true,
    );
    if mode != current {
        transform::set_corner_mode(app, mode);
    }
    if let Tool::Transform(info) = app.active_tool
        && info.warp.is_some()
    {
        // Grid cells a side: more points, finer control.
        let mut n = info.warp.map_or(info.warp_size, |w| w.n);
        let before = n;
        segmented(
            ui,
            &mut n,
            &[(2, "1×1"), (3, "2×2"), (4, "3×3"), (5, "4×4"), (6, "5×5")],
            true,
        );
        if n != before {
            transform::set_warp_size(app, n);
        }
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
        &[
            (FillMode::Bucket, "Bucket"),
            (FillMode::Enclose, "Enclose"),
            (FillMode::LassoDelete, "Lasso delete"),
        ],
        compact,
    );
    // Lasso delete erases exactly what it encloses: none of the rest apply.
    if f.mode == FillMode::LassoDelete {
        return "Draw around what to erase from the layer (inside the selection, if any)  ·  \
                G: next mode  ·  Delete key: erase the selection";
    }
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
            (FillSource::Reference, "Reference"),
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
            ui.add(crate::ui::widgets::reset(&mut *value, |v| {
                egui::Slider::new(v, range).show_value(true)
            }))
            .on_hover_text(tip);
        } else {
            crate::ui::widgets::slider_row(
                ui,
                label,
                crate::ui::widgets::reset(&mut *value, |v| egui::Slider::new(v, range)),
            )
            .on_hover_text(tip);
        }
    }
    if compact {
        vdivider(ui);
    }
    ui.checkbox(&mut s.antialias, "Smooth edge");
    match f.mode {
        FillMode::Bucket => "Click an area to fill it  ·  G: next mode (Enclose)",
        FillMode::Enclose | FillMode::LassoDelete => {
            "Draw around the areas to fill, even across lines  ·  G: next mode (Lasso delete)"
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
        ui.add(crate::ui::widgets::reset(&mut s.radius, |v| {
            egui::Slider::new(v, 4.0..=600.0)
                .logarithmic(true)
                .max_decimals(0)
                .suffix(" px")
        }));
        ui.label(RichText::new("Strength").color(TEXT_DIM));
        ui.add(crate::ui::widgets::reset(&mut s.strength, |v| {
            percent_of_unit(egui::Slider::new(v, 0.02..=1.0))
        }));
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
            crate::ui::widgets::reset(&mut s.radius, |v| {
                egui::Slider::new(v, 4.0..=600.0)
                    .logarithmic(true)
                    .max_decimals(0)
                    .suffix(" px")
            }),
        );
        crate::ui::widgets::slider_row(
            ui,
            "Strength",
            crate::ui::widgets::reset(&mut s.strength, |v| {
                percent_of_unit(egui::Slider::new(v, 0.02..=1.0))
            }),
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
