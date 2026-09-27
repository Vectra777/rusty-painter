//! The Shape tool's menu (toolbar button) and options: which shape, outline
//! or fill, and the ruler.

use crate::PainterApp;
use crate::app::shape_tool::{ShapeKind, ShapeStyle};
use crate::app::tools::Tool;
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
    });
    app.modal_state.shape_menu_open = open;
}
