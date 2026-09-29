//! The chrome beside the canvas, kept small so the canvas gets the screen:
//! a thin right rail (colour, layers) and the side panels the rails open,
//! plus notices over the canvas. The tool strip on the left is the left
//! rail; its brush-settings button opens the brush panel.
//!
//! Panels sit beside the canvas; on a narrow screen (a phone) they float
//! over it instead, and a tap on the canvas closes them.

use crate::ui::icons::Icon;
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, paint_swatch};
use crate::{PainterApp, ui};
use eframe::egui::{self, Stroke};

/// Below this window width panels float over the canvas.
pub(crate) const NARROW_WIDTH: f32 = 760.0;
const PANEL_WIDTH: f32 = 290.0;
const PANEL_MIN_WIDTH: f32 = 230.0;
/// Canvas a floating panel leaves uncovered on a narrow screen.
const MIN_CANVAS_WIDTH: f32 = 56.0;

fn narrow(ctx: &egui::Context) -> bool {
    ctx.screen_rect().width() < NARROW_WIDTH
}

impl PainterApp {
    /// Whether any side panel is open.
    pub(crate) fn any_panel_open(&self) -> bool {
        let ws = &self.workspace;
        ws.show_left_panel || ws.show_color || ws.show_layers
    }

    /// Tab: hide every panel, or (all hidden) open them all.
    pub(crate) fn toggle_all_panels(&mut self) {
        let show = !self.any_panel_open();
        let ws = &mut self.workspace;
        (ws.show_left_panel, ws.show_color, ws.show_layers) = (show, show, show);
    }
}

/// The right rail: the brush colour (opens the colour panel) and layers.
pub(crate) fn right_rail(app: &mut PainterApp, ctx: &egui::Context) {
    let m = metrics(ctx);
    let size = m.tool_button.min(40.0);
    egui::SidePanel::right("rail_right")
        .exact_width(size + 10.0)
        .resizable(false)
        .frame(
            egui::Frame::none()
                .fill(BG_PANEL)
                .inner_margin(egui::Margin::symmetric(5.0, 6.0)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // The colour button is the colour itself.
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
            let open = app.workspace.show_color;
            paint_swatch(
                ui.painter(),
                rect.shrink(5.0),
                app.brush_state.brush.brush_options.color,
            );
            let ring = if open {
                ACCENT
            } else if response.hovered() {
                TEXT_STRONG
            } else {
                BORDER_LIGHT
            };
            ui.painter()
                .rect_stroke(rect.shrink(3.0), 0.0, Stroke::new(2.0_f32, ring));
            if response.on_hover_text("Colour").clicked() {
                app.workspace.show_color = !open;
            }
            let open = app.workspace.show_layers;
            if icon_button(ui, Icon::Layers, size, open, "Layers").clicked() {
                app.workspace.show_layers = !open;
            }
        });
}

/// The open panels: beside the canvas, or over it on a narrow screen.
pub(crate) fn show_panels(app: &mut PainterApp, ctx: &egui::Context) {
    let frame = egui::Frame::none()
        .fill(BG_PANEL)
        .inner_margin(egui::Margin::symmetric(6.0, 4.0));
    let right_open = app.workspace.show_color || app.workspace.show_layers;
    if !narrow(ctx) {
        // A fixed width (dragged at the edge to change): egui's resizable
        // panels grow to fit their content, and content a pixel too wide
        // would then widen the panel every frame.
        let max = (ctx.available_rect().width() * 0.4).max(PANEL_MIN_WIDTH);
        let width = |id: &str| -> f32 {
            ctx.data(|d| d.get_temp(egui::Id::new((id, "width"))))
                .unwrap_or(PANEL_WIDTH)
                .clamp(PANEL_MIN_WIDTH, max)
        };
        let (left_w, right_w) = (width("panel_brush"), width("panel_right"));
        let left = egui::SidePanel::left("panel_brush")
            .exact_width(left_w)
            .resizable(false)
            .frame(frame)
            .show_animated(ctx, app.workspace.show_left_panel, |ui| {
                ui.set_clip_rect(ui.max_rect());
                brush_panel(app, ui);
            });
        let right = egui::SidePanel::right("panel_right")
            .exact_width(right_w)
            .resizable(false)
            .frame(frame)
            .show_animated(ctx, right_open, |ui| {
                ui.set_clip_rect(ui.max_rect());
                right_panel(app, ui);
            });
        let mut resized = false;
        if let Some(r) = left {
            let rect = r.response.rect;
            resized |= resize_edge(ctx, "panel_brush", rect.right(), rect, left_w, 1.0);
        }
        if let Some(r) = right {
            let rect = r.response.rect;
            resized |= resize_edge(ctx, "panel_right", rect.left(), rect, right_w, -1.0);
        }
        if resized {
            app.save_panel_widths(ctx);
        }
        return;
    }
    // Narrow: floating over the canvas, beside the rail it came from, one
    // side at a time (the one just opened wins).
    let id = egui::Id::new("panels_last_frame");
    let (left_before, right_before): (bool, bool) =
        ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let ws = &mut app.workspace;
    if ws.show_left_panel && right_open {
        if left_before && !right_before {
            ws.show_left_panel = false;
        } else {
            (ws.show_color, ws.show_layers) = (false, false);
        }
    }
    let right_open = ws.show_color || ws.show_layers;
    ctx.data_mut(|d| d.insert_temp(id, (ws.show_left_panel, right_open)));
    let area = ctx.available_rect();
    let width = PANEL_WIDTH.min(area.width() - MIN_CANVAS_WIDTH).max(160.0);
    let floating = |id: &str, left: f32, contents: &mut dyn FnMut(&mut egui::Ui)| {
        egui::Area::new(egui::Id::new(id))
            .fixed_pos(egui::pos2(left, area.top()))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.set_max_width(width);
                frame.stroke(Stroke::new(1.0_f32, BORDER)).show(ui, |ui| {
                    ui.set_width(width - 14.0);
                    ui.set_height(area.height() - 10.0);
                    ui.set_clip_rect(ui.max_rect());
                    contents(ui);
                });
            });
    };
    if app.workspace.show_left_panel {
        floating("float_brush", area.left(), &mut |ui| brush_panel(app, ui));
    }
    if right_open {
        floating("float_right", area.right() - width, &mut |ui| {
            right_panel(app, ui)
        });
    }
}

/// A handle on a panel's inner edge at `x`: dragging it changes the
/// panel's width (`sign`: +1 grows to the right, -1 to the left). Returns
/// whether a drag ended this frame.
fn resize_edge(
    ctx: &egui::Context,
    id: &str,
    x: f32,
    panel: egui::Rect,
    width: f32,
    sign: f32,
) -> bool {
    let rect = egui::Rect::from_x_y_ranges(x - 4.0..=x + 4.0, panel.y_range());
    let key = egui::Id::new((id, "width"));
    let mut released = false;
    egui::Area::new(egui::Id::new((id, "edge")))
        .fixed_pos(rect.min)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(rect.size(), egui::Sense::drag());
            if response.hovered() || response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                ui.painter().vline(
                    rect.center().x,
                    rect.y_range(),
                    Stroke::new(2.0_f32, ACCENT),
                );
            }
            if response.dragged() {
                let w = width + sign * response.drag_delta().x;
                ctx.data_mut(|d| d.insert_temp(key, w.max(PANEL_MIN_WIDTH)));
            }
            released = response.drag_stopped();
        });
    released
}

/// The side panels' widths as the user left them, saved beside the brushes
/// folder. A panel never resized is left out (it opens at the default).
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PanelWidths {
    #[serde(default)]
    brush: Option<f32>,
    #[serde(default)]
    right: Option<f32>,
}

/// The panels whose widths are saved, by egui id, with their field.
fn panel_width_slots(widths: &mut PanelWidths) -> [(&'static str, &mut Option<f32>); 2] {
    [
        ("panel_brush", &mut widths.brush),
        ("panel_right", &mut widths.right),
    ]
}

impl PainterApp {
    fn panel_widths_path(&self) -> std::path::PathBuf {
        self.brush_state.brushes_path.with_file_name("panels.json")
    }

    /// Restore the panel widths saved last time. They're fitted to the
    /// screen each frame (at most 40% of it), so a width saved on a bigger
    /// screen comes back narrower rather than squeezing the canvas.
    pub(crate) fn load_panel_widths(&self, ctx: &egui::Context) {
        let Ok(bytes) = std::fs::read(self.panel_widths_path()) else {
            return;
        };
        let Ok(mut widths) = serde_json::from_slice::<PanelWidths>(&bytes) else {
            log::warn!("Ignoring unreadable panel widths");
            return;
        };
        for (id, width) in panel_width_slots(&mut widths) {
            if let Some(w) = width.filter(|w| w.is_finite()) {
                let key = egui::Id::new((id, "width"));
                ctx.data_mut(|d| d.insert_temp(key, w.max(PANEL_MIN_WIDTH)));
            }
        }
    }

    /// Save the panel widths (after one was dragged).
    fn save_panel_widths(&self, ctx: &egui::Context) {
        let mut widths = PanelWidths::default();
        for (id, width) in panel_width_slots(&mut widths) {
            *width = ctx.data(|d| d.get_temp(egui::Id::new((id, "width"))));
        }
        let result = serde_json::to_vec_pretty(&widths)
            .map_err(|e| e.to_string())
            .and_then(|bytes| crate::project::write_atomically(&self.panel_widths_path(), &bytes));
        if let Err(err) = result {
            log::warn!("Couldn't save the panel widths: {err}");
        }
    }
}

/// On a narrow screen, a press on the canvas closes floating panels (and
/// does nothing else). Returns whether it did.
pub(crate) fn close_floating_panels(
    app: &mut PainterApp,
    ctx: &egui::Context,
    canvas: &egui::Response,
) -> bool {
    if !narrow(ctx) || !app.any_panel_open() {
        return false;
    }
    let pressed = ctx.input(|i| i.pointer.any_pressed()) && canvas.hovered();
    if pressed {
        let ws = &mut app.workspace;
        (ws.show_left_panel, ws.show_color, ws.show_layers) = (false, false, false);
    }
    pressed
}

fn panel_title(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).small().strong().color(TEXT_DIM));
}

/// The brush settings, or the active tool's own (fill, liquify).
fn brush_panel(app: &mut PainterApp, ui: &mut egui::Ui) {
    use crate::app::tools::Tool;
    match app.active_tool {
        Tool::Fill => {
            panel_title(ui, "FILL");
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui::tool_options::fill_options(app, ui, false);
            });
        }
        Tool::Liquify => {
            panel_title(ui, "LIQUIFY");
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui::tool_options::liquify_options(app, ui, false);
            });
        }
        _ => {
            panel_title(ui, "BRUSH");
            ui::brush_settings::brush_settings_panel(
                ui,
                &mut app.brush_state.brush,
                &mut app.brush_state.brush_preview,
                &app.workspace.pool,
                &app.brush_state.loaded_brush_tips,
                &app.brush_state.loaded_textures,
            );
        }
    }
}

/// Colour above layers, whichever of them is open.
fn right_panel(app: &mut PainterApp, ui: &mut egui::Ui) {
    if app.workspace.show_color {
        panel_title(ui, "COLOUR");
        // Sharing with the layers, the colours take at most about half.
        let height = if app.workspace.show_layers {
            (ui.available_height() * 0.5).clamp(160.0, 520.0)
        } else {
            ui.available_height()
        };
        let width = ui.available_width();
        ui.allocate_ui(egui::vec2(width, height), |ui| {
            ui::color_picker::color_picker_panel(
                ui,
                &mut app.brush_state,
                app.workspace.color_model,
            );
        });
        ui.add_space(4.0);
    }
    if app.workspace.show_layers {
        if app.workspace.show_color {
            ui.separator();
        }
        panel_title(ui, "LAYERS");
        let ctx = ui.ctx().clone();
        ui::layers::layers_panel(&ctx, ui, app);
    }
}

/// Over the canvas's bottom-left corner: the last message (export done,
/// errors) until dismissed or a few seconds pass, and a content-aware
/// fill's progress.
pub(crate) fn notices(app: &mut PainterApp, ctx: &egui::Context, area: egui::Rect) {
    const SHOWN_FOR: f64 = 6.0;
    let now = ctx.input(|i| i.time);
    let id = egui::Id::new("notice_since");
    let message = app.export_state.message.clone();
    // Errors and results stay a moment, then go (unless hovered).
    let since: Option<(String, f64)> = ctx.data(|d| d.get_temp(id));
    let since = match (&message, since) {
        (Some(m), Some((seen, t))) if *m == seen => Some(t),
        (Some(m), _) => {
            ctx.data_mut(|d| d.insert_temp(id, (m.clone(), now)));
            Some(now)
        }
        (None, _) => None,
    };
    let progress = app.patch_progress();
    if message.is_none() && progress.is_none() {
        return;
    }
    let response =
        egui::Area::new(egui::Id::new("canvas_notice"))
            .fixed_pos(area.left_bottom() + egui::vec2(10.0, -10.0))
            .pivot(egui::Align2::LEFT_BOTTOM)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                    .inner_margin(egui::Margin::symmetric(8.0, 5.0))
                    .show(ui, |ui| {
                        ui.set_max_width((area.width() - 40.0).max(120.0));
                        ui.horizontal(|ui| {
                            if let Some(p) = progress {
                                if ui
                                    .small_button("✕")
                                    .on_hover_text("Cancel the fill")
                                    .clicked()
                                {
                                    app.patch_cancel();
                                }
                                ui.add(egui::ProgressBar::new(p).desired_width(140.0).text(
                                    egui::RichText::new("Filling from surroundings").small(),
                                ));
                            } else if let Some(m) = &message {
                                if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                                    app.export_state.message = None;
                                }
                                ui.add(egui::Label::new(egui::RichText::new(m).small()).wrap());
                            }
                        });
                    });
            })
            .response;
    if progress.is_none()
        && let Some(t) = since
    {
        if response.hovered() {
            ctx.data_mut(|d| d.insert_temp(id, (message.clone().unwrap_or_default(), now)));
        } else if now - t > SHOWN_FOR {
            app.export_state.message = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(SHOWN_FOR - (now - t)));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use eframe::egui::{self, Color32};

    /// The canvas width left after 40 frames of the panels, one per frame.
    fn canvas_widths(touch: bool) -> Vec<f32> {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.workspace.touch_mode = touch;
        app.workspace.show_left_panel = true;
        app.workspace.show_color = true;
        app.workspace.show_layers = true;
        let ctx = egui::Context::default();
        crate::ui::theme::apply_style(&ctx, touch);
        crate::ui::style::set_touch_metrics(&ctx, touch);
        (0..40)
            .map(|_| {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 900.0),
                    )),
                    ..Default::default()
                };
                let mut left = 0.0;
                let _ = ctx.run(input, |ctx| {
                    super::show_panels(&mut app, ctx);
                    left = ctx.available_rect().width();
                });
                left
            })
            .collect()
    }

    /// The canvas width left beside the open panels on a `screen_w` wide
    /// window, once they've slid in.
    fn canvas_width_on(app: &mut crate::PainterApp, ctx: &egui::Context, screen_w: f32) -> f32 {
        let mut left = 0.0;
        for _ in 0..40 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(screen_w, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                super::show_panels(app, ctx);
                left = ctx.available_rect().width();
            });
        }
        left
    }

    #[test]
    fn panel_widths_come_back_next_launch_fitted_to_the_screen() {
        let dir = std::env::temp_dir().join(format!("rp-panels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let panels_open = |app: &mut crate::PainterApp| {
            app.brush_state.brushes_path = dir.join("brushes");
            app.workspace.show_left_panel = true;
            app.workspace.show_layers = true;
        };
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        panels_open(&mut app);
        let ctx = egui::Context::default();
        let default_small = canvas_width_on(&mut app, &ctx, 900.0);
        let default_canvas = canvas_width_on(&mut app, &ctx, 1400.0);
        for (id, w) in [("panel_brush", 400.0_f32), ("panel_right", 350.0)] {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new((id, "width")), w));
        }
        app.save_panel_widths(&ctx);

        // Next launch: a new app and window.
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        panels_open(&mut app);
        let ctx = egui::Context::default();
        app.load_panel_widths(&ctx);
        let canvas = canvas_width_on(&mut app, &ctx, 1400.0);
        let grown = (400.0 - super::PANEL_WIDTH) + (350.0 - super::PANEL_WIDTH);
        assert!(
            (default_canvas - canvas - grown).abs() < 1.0,
            "{default_canvas} - {canvas} != {grown}"
        );
        // On a smaller screen each panel takes at most 40% of it: the
        // 400 px one comes back 360 px wide.
        let small = canvas_width_on(&mut app, &ctx, 900.0);
        let fitted = (360.0 - super::PANEL_WIDTH) + (350.0 - super::PANEL_WIDTH);
        assert!(
            (default_small - small - fitted).abs() < 1.0,
            "{default_small} - {small} != {fitted}"
        );
        // A missing or broken file leaves the defaults.
        std::fs::write(dir.join("panels.json"), b"not json").unwrap();
        let ctx = egui::Context::default();
        app.load_panel_widths(&ctx);
        assert_eq!(canvas_width_on(&mut app, &ctx, 1400.0), default_canvas);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_panels_keep_their_width() {
        for touch in [false, true] {
            let w = canvas_widths(touch);
            // Past the slide-in, not a pixel of drift.
            assert_eq!(w[20], w[39], "touch {touch}: {:?}", &w[15..]);
            assert!(
                w[39] > 600.0,
                "touch {touch}: the canvas keeps its room ({})",
                w[39]
            );
        }
    }
}
