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
                    AssistantKind::VanishingPoint => "Strokes run towards the point",
                    AssistantKind::Perspective => {
                        "Drag the corners onto a rectangle seen at an angle: strokes run towards \
                         its two vanishing points, or upright"
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
    let mut remove = None;
    for (i, a) in app.workspace.guides.assistants.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut a.enabled, a.kind.label());
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        app.workspace.guides.assistants.remove(i);
    }
}

/// The ruler's switches.
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
        ui.separator();
        ui.label(RichText::new("ASSISTANTS").small().strong().color(TEXT_DIM));
        assistant_controls(app, ui);
    });
    app.modal_state.shape_menu_open = open;
}
