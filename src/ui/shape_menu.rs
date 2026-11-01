//! The Shape tool's menu (toolbar button) and options: which shape, outline
//! or fill, and the ruler.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::shape::{ShapeKind, ShapeStyle};
use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::segmented;
use eframe::egui::{self, RichText};

pub(crate) fn icon_for(kind: ShapeKind) -> Icon {
    match kind {
        ShapeKind::Line => Icon::ShapeLine,
        ShapeKind::Rectangle => Icon::ShapeRect,
        ShapeKind::Ellipse => Icon::ShapeEllipse,
        ShapeKind::Polygon => Icon::ShapePolygon,
        ShapeKind::Curve => Icon::ShapeCurve,
    }
}

/// Shape kind, outline / fill, and polygon closing. `compact` for bars.
pub(crate) fn shape_controls(app: &mut PainterApp, ui: &mut egui::Ui, compact: bool) {
    let mut kind = match app.active_tool {
        Tool::Shape(kind) => kind,
        _ => app.workspace.shapes.last_kind,
    };
    if segmented(ui, &mut kind, &ShapeKind::ALL, compact) {
        app.set_shape_tool(kind);
    }
    let settings = &mut app.workspace.shapes.settings;
    segmented(
        ui,
        &mut settings.style,
        &[
            (ShapeStyle::Outline, "Outline"),
            (ShapeStyle::Fill, "Fill"),
            (ShapeStyle::Both, "Both"),
        ],
        compact,
    );
    if kind == ShapeKind::Polygon {
        ui.checkbox(&mut settings.closed, "Closed");
    }
}

/// Apply / Cancel for the shape being edited.
pub(crate) fn shape_actions(app: &mut PainterApp, ui: &mut egui::Ui) {
    let editing = app.workspace.shapes.session.is_some();
    if ui
        .add_enabled(editing, egui::Button::new("Apply"))
        .on_hover_text("Paint the shape (Enter)")
        .clicked()
    {
        app.shape_commit();
    }
    if ui
        .add_enabled(editing, egui::Button::new("Cancel"))
        .on_hover_text("Discard it (Esc)")
        .clicked()
    {
        app.shape_cancel();
    }
}

/// The drawing assistants: add one, show or hide each, remove it.
pub(crate) fn assistant_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::app::tools::assistants::AssistantKind;
    ui.horizontal_wrapped(|ui| {
        for kind in AssistantKind::ALL {
            if ui
                .button(format!("+ {}", kind.label()))
                .on_hover_text(match kind {
                    AssistantKind::VanishingPoint => {
                        "One-point perspective: strokes run towards the point"
                    }
                    AssistantKind::TwoPoint => {
                        "Two-point perspective: two points on a horizon; strokes run towards \
                         either, or upright. The point between them is the centre of vision: \
                         it sets the ground grid's squares"
                    }
                    AssistantKind::ThreePoint => {
                        "Three-point perspective: two points on a horizon and one above or \
                         below for the uprights"
                    }
                    AssistantKind::Perspective => {
                        "Drag the corners onto a rectangle seen at an angle: strokes starting on \
                         it run along its sides' directions, or upright"
                    }
                    AssistantKind::Fisheye => {
                        "Fisheye: two points on an axis; strokes run round the ellipse through \
                         both and where they start. Add two, square to each other, and a 1-point \
                         between for a full fisheye view"
                    }
                    AssistantKind::Spline => {
                        "A curve set by its ends and two control points: strokes started near \
                         it run along it"
                    }
                    AssistantKind::Ellipse => "Strokes started near it run round it",
                    AssistantKind::Concentric => "Strokes run round ellipses of its shape",
                })
                .clicked()
            {
                app.add_assistant(kind);
            }
        }
    });
    let g = &mut app.workspace.guides;
    let mut strength = 1.0 - g.looseness;
    ui.horizontal(|ui| {
        ui.label("Strength");
        if ui
            .add(
                egui::Slider::new(&mut strength, 0.0..=1.0)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            )
            .on_hover_text(
                "How hard the ruler and assistants pull strokes onto their lines (100%: right \
                 on them; lower lets the pen wander)",
            )
            .changed()
        {
            g.looseness = 1.0 - strength;
        }
    });
    let mut remove = None;
    let mut paint = None;
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let mut placing = false;
    for (i, a) in app.workspace.guides.assistants.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut a.enabled, a.kind.label());
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
        });
        if !a.enabled {
            continue;
        }
        ui.indent(("assistant", i), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut a.snap, "Snap")
                    .on_hover_text("Strokes follow it (off: it only shows lines)");
                if a.kind.has_line_count() {
                    let mut n = a.line_count();
                    let label = if a.kind == AssistantKind::Perspective {
                        "Grid"
                    } else {
                        "Lines"
                    };
                    ui.label(label);
                    if ui
                        .add(egui::DragValue::new(&mut n).range(1..=360))
                        .on_hover_text(match a.kind {
                            AssistantKind::Perspective => "How many squares across its grid has",
                            AssistantKind::TwoPoint => "How fine its ground grid is",
                            AssistantKind::Fisheye => "How many curves it shows round each cell",
                            _ => "How many lines it shows round each vanishing point",
                        })
                        .changed()
                    {
                        a.lines = n;
                    }
                }
                if a.kind.has_horizon() {
                    ui.checkbox(&mut a.horizon, "Horizon");
                }
                if a.kind.has_upright() {
                    ui.checkbox(&mut a.upright, "Upright")
                        .on_hover_text("Strokes may also run upright (and level, in one-point)");
                }
                if a.kind.can_be_local() {
                    let mut local = a.area.is_some();
                    if ui
                        .checkbox(&mut local, "Local")
                        .on_hover_text(
                            "Only guide strokes starting inside an area (drag its square \
                             corners), and show its lines only there",
                        )
                        .changed()
                    {
                        a.set_local(local, w, h);
                        placing |= local;
                    }
                }
                if a.kind.has_lines()
                    && ui
                        .small_button("Paint lines")
                        .on_hover_text(
                            "Paint its lines onto the selected layer with the brush, as the \
                             Shape tool does",
                        )
                        .clicked()
                {
                    paint = Some(i);
                }
            });
        });
    }
    if placing {
        // Ready to be put in place.
        app.set_editing_guides(true);
    }
    if let Some(i) = paint {
        app.paint_assistant_lines(i);
    }
    if let Some(i) = remove {
        app.workspace.guides.assistants.remove(i);
    }
}

/// The ruler's switches.
/// The guides locked (strokes paint on their handles) or being edited.
pub(crate) fn edit_helpers_toggle(app: &mut PainterApp, ui: &mut egui::Ui) {
    let mut editing = app.workspace.guides.editing;
    if ui
        .checkbox(&mut editing, "Edit Helpers")
        .on_hover_text(
            "Drag the ruler, assistants and mirror: a point moves that point, anywhere else on \
             one moves all of it. Off, they're locked and strokes paint over them (Shift+R)",
        )
        .changed()
    {
        app.set_editing_guides(editing);
    }
}

pub(crate) fn ruler_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let mut enabled = app.workspace.guides.ruler.enabled;
    if ui
        .checkbox(&mut enabled, "Ruler")
        .on_hover_text("Strokes run along the ruler, or parallel to it (R)")
        .changed()
    {
        app.set_ruler(enabled);
    }
    if enabled {
        ui.checkbox(&mut app.workspace.guides.ruler.parallel, "Parallel lines")
            .on_hover_text("Strokes started away from the ruler run parallel to it");
    }
    let guides = &mut app.workspace.guides;
    if guides.curve_ruler.is_some() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut guides.curve_enabled, "Curve ruler")
                .on_hover_text("Strokes started near it run along it (made from a vector line: Edit Vectors → Make ruler)");
            if ui.small_button("Remove").clicked() {
                guides.curve_ruler = None;
                guides.curve_enabled = false;
            }
        });
    }
}

/// The menu sliding out from the toolbar's Shape button (`anchor`).
pub fn show(app: &mut PainterApp, ctx: &egui::Context, anchor: egui::Rect) {
    let width = if metrics(ctx).touch { 300.0 } else { 250.0 };
    let mut open = app.modal_state.shape_menu_open;
    crate::ui::widgets::flyout(ctx, "shape_menu", &mut open, anchor, width, |ui| {
        ui.label(RichText::new("SHAPES").small().strong().color(TEXT_DIM));
        ui.horizontal_wrapped(|ui| shape_controls(app, ui, true));
        ui.label(
            RichText::new(
                "Drag to draw; drag the handles to adjust. Shift: square / circle / 15°, \
                 Alt: from the centre. Enter applies.",
            )
            .small()
            .color(TEXT_DIM),
        );
        ui.horizontal(|ui| shape_actions(app, ui));
        ui.separator();
        ui.label(RichText::new("RULER").small().strong().color(TEXT_DIM));
        ruler_controls(app, ui);
        edit_helpers_toggle(app, ui);
        ui.separator();
        ui.label(RichText::new("ASSISTANTS").small().strong().color(TEXT_DIM));
        assistant_controls(app, ui);
    });
    app.modal_state.shape_menu_open = open;
}
