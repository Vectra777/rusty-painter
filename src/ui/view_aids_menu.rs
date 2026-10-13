//! The View menu's viewing aids: the grid and its settings, guides,
//! snapping, the reference image and the navigator; and the Guides window.

use crate::PainterApp;
use crate::ui::bar_slider::BarSlider;
use crate::ui::style::*;
use crate::ui::widgets::FitScreen;
use crate::ui::widgets::{segmented, slider_row};
use eframe::egui::{self, Color32, RichText};

/// The View menu's entries for the viewing aids. `item` adds a menu entry
/// (closing the menu when picked) with a shortcut hint.
pub(crate) fn view_aids_items(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    mut item: impl FnMut(&mut egui::Ui, &str, Option<String>) -> bool,
) {
    use crate::app::input::keymap::Action;
    let ctx = ui.ctx().clone();
    let keymap = &app.workspace.keymap;
    let (grid_keys, guides_keys) = (
        keymap.labels(&ctx, Action::Grid),
        keymap.labels(&ctx, Action::Guides),
    );
    let aids = &mut app.workspace.view_aids;
    ui.checkbox(&mut aids.grid.show, "Grid")
        .on_hover_text(format!(
            "Lines every so many pixels over the canvas ({grid_keys})"
        ));
    ui.menu_button("Grid Settings", |ui| grid_controls(app, ui));
    ui.menu_button("Guides", |ui| {
        if item(ui, "New Guide…", None) {
            open_new_guide(app);
        }
        let guides = &mut app.workspace.view_aids.guides;
        ui.checkbox(&mut guides.show, "Show Guides")
            .on_hover_text(guides_keys);
        ui.checkbox(&mut guides.locked, "Lock Guides")
            .on_hover_text("Guides can't be moved or removed by dragging");
        let any = !guides.lines.is_empty();
        if ui
            .add_enabled_ui(any, |ui| item(ui, "Clear Guides", None))
            .inner
        {
            app.clear_guides();
        }
        ui.label(
            RichText::new(
                "Ctrl+drag a guide to move it, or from beside the canvas to add one; \
                 drag it off the canvas to remove it.",
            )
            .small()
            .color(TEXT_DIM),
        );
    });
    let aids = &mut app.workspace.view_aids;
    ui.checkbox(&mut aids.snap_to_guides, "Snap to Guides")
        .on_hover_text(
            "Selections, shapes, gradients, text and the start of a stroke snap to guides \
             nearby",
        );
    ui.checkbox(&mut aids.snap_to_grid, "Snap to Grid")
        .on_hover_text("Likewise to the grid's lines, while it's shown");
    ui.checkbox(&mut aids.reference.open, "Reference Image")
        .on_hover_text("A picture to paint from, in its own window; click it to pick a colour");
    ui.checkbox(&mut aids.navigator.open, "Navigator")
        .on_hover_text("The whole picture small; click or drag in it to move the view");
}

/// Grid spacing, subdivisions, colour, isometric, and the pixel grid.
pub(crate) fn grid_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let g = &mut app.workspace.view_aids.grid;
    ui.set_min_width(240.0);
    ui.checkbox(&mut g.show, "Show grid");
    slider_row(
        ui,
        "Spacing",
        crate::ui::widgets::reset(&mut g.spacing, |v| {
            BarSlider::new(v, 2.0..=1000.0)
                .logarithmic(true)
                .suffix(" px")
                .max_decimals(0)
        }),
    );
    slider_row(
        ui,
        "Divisions",
        crate::ui::widgets::reset(&mut g.subdivisions, |v| BarSlider::new(v, 1..=16)),
    );
    segmented(
        ui,
        &mut g.isometric,
        &[(false, "Squares"), (true, "Isometric")],
        false,
    );
    ui.horizontal(|ui| {
        ui.label(RichText::new("Colour").color(TEXT_DIM));
        let [r, gr, b] = g.color;
        let mut color = Color32::from_rgb(r, gr, b);
        if ui.color_edit_button_srgba(&mut color).changed() {
            g.color = [color.r(), color.g(), color.b()];
        }
        ui.add(
            BarSlider::new(&mut g.opacity, 0.05..=1.0)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .custom_parser(|s| {
                    s.trim_end_matches('%')
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .map(|v| v / 100.0)
                }),
        );
    });
    ui.checkbox(&mut g.pixel_grid, "Pixel grid when zoomed in (800%+)");
}

fn open_new_guide(app: &mut PainterApp) {
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let new = &mut app.workspace.view_aids.guides.new_guide;
    new.open = true;
    new.pos = if new.vertical { w * 0.5 } else { h * 0.5 }.round();
}

/// The Guides window (View → Guides → New Guide…): add a guide at an exact
/// position, and move or remove the ones there are.
pub fn guides_window(app: &mut PainterApp, ctx: &egui::Context) {
    let mut open = app.workspace.view_aids.guides.new_guide.open;
    if !open {
        return;
    }
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    egui::Window::new("Guides")
        .fit_screen(ctx)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(260.0)
        .show(ctx, |ui| {
            let guides = &mut app.workspace.view_aids.guides;
            let new = &mut guides.new_guide;
            let was = new.vertical;
            segmented(
                ui,
                &mut new.vertical,
                &[(false, "Horizontal"), (true, "Vertical")],
                false,
            );
            if new.vertical != was {
                new.pos = if new.vertical { w * 0.5 } else { h * 0.5 }.round();
            }
            let extent = if new.vertical { w } else { h };
            let mut add = false;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Position").color(TEXT_DIM));
                ui.add(
                    egui::DragValue::new(&mut new.pos)
                        .range(0.0..=extent)
                        .suffix(" px")
                        .max_decimals(1),
                );
                add = ui.button("Add").clicked();
            });
            let (vertical, pos) = (new.vertical, new.pos);
            if add {
                app.add_guide(vertical, pos);
            }
            let guides = &mut app.workspace.view_aids.guides;
            if guides.lines.is_empty() {
                return;
            }
            ui.separator();
            let mut remove = None;
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    for (i, line) in guides.lines.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(if line.vertical {
                                "Vertical"
                            } else {
                                "Horizontal"
                            });
                            let extent = if line.vertical { w } else { h };
                            ui.add_enabled(
                                !guides.locked,
                                egui::DragValue::new(&mut line.pos)
                                    .range(0.0..=extent)
                                    .suffix(" px")
                                    .max_decimals(1),
                            );
                            if ui
                                .add_enabled(!guides.locked, egui::Button::new("Remove").small())
                                .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                    }
                });
            if let Some(i) = remove {
                guides.lines.remove(i);
            }
        });
    app.workspace.view_aids.guides.new_guide.open = open;
}
