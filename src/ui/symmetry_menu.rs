//! The mirror painting menu (toolbar button): mode, copies and the axes.

use crate::PainterApp;
use crate::brush_engine::symmetry::{MAX_COUNT, SymmetryMode};
use crate::ui::style::*;
use crate::ui::widgets::{segmented, slider_row};
use eframe::egui::{self, RichText};

/// Mirror painting controls, shared by the menu and the View menu.
pub(crate) fn symmetry_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let before = app.workspace.symmetry.mode;
    let s = &mut app.workspace.symmetry;
    segmented(
        ui,
        &mut s.mode,
        &[
            (SymmetryMode::Off, "Off"),
            (SymmetryMode::Vertical, "L | R"),
            (SymmetryMode::Horizontal, "T | B"),
            (SymmetryMode::Both, "4-way"),
            (SymmetryMode::Radial, "Radial"),
        ],
        true,
    );
    let mode_name = match s.mode {
        SymmetryMode::Off => "No mirroring",
        SymmetryMode::Vertical => "Left / right",
        SymmetryMode::Horizontal => "Top / bottom",
        SymmetryMode::Both => "Four ways",
        SymmetryMode::Radial => "Around the centre (mandala)",
    };
    ui.label(RichText::new(mode_name).small().color(TEXT_DIM));
    if s.mode == SymmetryMode::Radial {
        slider_row(
            ui,
            "Copies",
            crate::ui::widgets::reset(&mut s.count, |v| egui::Slider::new(v, 2..=MAX_COUNT)),
        );
        ui.checkbox(&mut s.mirrored, "Mirror each copy (kaleidoscope)");
    }
    if s.mode != SymmetryMode::Off {
        let mut degrees = s.angle.to_degrees();
        if slider_row(
            ui,
            "Angle",
            crate::ui::widgets::reset(&mut degrees, |v| {
                egui::Slider::new(v, -180.0..=180.0)
                    .suffix("°")
                    .max_decimals(1)
            }),
        )
        .changed()
        {
            s.angle = degrees.to_radians();
        }
        let guides = &mut app.workspace.guides;
        let mut show = !guides.hide_symmetry;
        if ui
            .checkbox(&mut show, "Show axes")
            .on_hover_text(
                "Drag the centre to move the axes, the ring to turn them (Shift: 15° steps)",
            )
            .changed()
        {
            guides.hide_symmetry = !show;
        }
        ui.horizontal(|ui| {
            if ui.button("Centre on canvas").clicked() {
                app.centre_symmetry();
            }
            if ui.button("Reset angle").clicked() {
                app.workspace.symmetry.angle = 0.0;
            }
        });
    }
    // Turning mirroring on shows the axes, so their handles can be found.
    if before == SymmetryMode::Off && app.workspace.symmetry.mode != SymmetryMode::Off {
        app.workspace.guides.hide_symmetry = false;
    }
}

/// The menu sliding out from the toolbar's mirror button (`anchor`).
pub fn show(app: &mut PainterApp, ctx: &egui::Context, anchor: egui::Rect) {
    let width = if metrics(ctx).touch { 300.0 } else { 250.0 };
    let mut open = app.modal_state.symmetry_menu_open;
    crate::ui::widgets::flyout(ctx, "symmetry_menu", &mut open, anchor, width, |ui| {
        ui.label(
            RichText::new("MIRROR PAINTING")
                .small()
                .strong()
                .color(TEXT_DIM),
        );
        symmetry_controls(app, ui);
    });
    app.modal_state.symmetry_menu_open = open;
}
