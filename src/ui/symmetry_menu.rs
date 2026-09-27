//! The mirror painting menu (toolbar button): mode, copies and the axes.

use crate::PainterApp;
use crate::brush_engine::symmetry::{MAX_COUNT, SymmetryMode};
use crate::ui::style::*;
use crate::ui::widgets::{segmented, slider_row};
use eframe::egui::{self, RichText, Stroke};

/// Mirror painting controls, shared by the menu and the View menu.
pub(crate) fn symmetry_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let before = app.workspace.symmetry.mode;
    let s = &mut app.workspace.symmetry;
    segmented(
        ui,
        &mut s.mode,
        &[
            (SymmetryMode::Off, "Off"),
            (SymmetryMode::Vertical, "│"),
            (SymmetryMode::Horizontal, "─"),
            (SymmetryMode::Both, "┼"),
            (SymmetryMode::Radial, "✳"),
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
        slider_row(ui, "Copies", egui::Slider::new(&mut s.count, 2..=MAX_COUNT));
        ui.checkbox(&mut s.mirrored, "Mirror each copy (kaleidoscope)");
    }
    if s.mode != SymmetryMode::Off {
        let mut degrees = s.angle.to_degrees();
        if slider_row(
            ui,
            "Angle",
            egui::Slider::new(&mut degrees, -180.0..=180.0)
                .suffix("°")
                .max_decimals(1),
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
    let open = app.modal_state.symmetry_menu_open;
    let t = ctx.animate_bool_with_time(egui::Id::new("symmetry_menu_anim"), open, 0.12);
    if t <= 0.0 {
        return;
    }
    let width = if metrics(ctx).touch { 300.0 } else { 250.0 };
    let x = anchor.right() + 4.0 - (1.0 - t) * width;
    let max_height = ctx.screen_rect().bottom() - anchor.top() - 24.0;
    let top = if max_height < 200.0 {
        (ctx.screen_rect().bottom() - 240.0).max(0.0)
    } else {
        anchor.top()
    };
    let response = egui::Area::new(egui::Id::new("symmetry_menu"))
        .fixed_pos(egui::pos2(x, top))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    egui::ScrollArea::vertical()
                        .max_height((ctx.screen_rect().bottom() - top - 24.0).max(120.0))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("MIRROR PAINTING")
                                    .small()
                                    .strong()
                                    .color(TEXT_DIM),
                            );
                            symmetry_controls(app, ui);
                        });
                });
        })
        .response;
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !response.rect.contains(p) && !anchor.contains(p));
    if open && clicked_outside {
        app.modal_state.symmetry_menu_open = false;
    }
}
