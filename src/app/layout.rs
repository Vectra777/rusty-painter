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
/// Touch colour dropdown: below the wheel, the colour pair and H, S, V, A.
const COLOUR_BELOW_WHEEL: f32 = 240.0;
/// Touch dropdowns: the layers header and its option rows fit at this width.
const DROPDOWN_WIDTH: f32 = 340.0;
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

/// Touch mode's dropdowns on the top bar's right: brushes, colour, layers.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Dropdown {
    Brushes,
    Colour,
    Layers,
}

impl Dropdown {
    pub(crate) const ALL: [(Dropdown, &'static str); 3] = [
        (Dropdown::Brushes, "Brush presets"),
        (Dropdown::Colour, "Colour"),
        (Dropdown::Layers, "Layers"),
    ];

    fn anchor_id(self) -> egui::Id {
        egui::Id::new(("dropdown_anchor", self as u8))
    }

    fn flag(self, app: &mut PainterApp) -> &mut bool {
        match self {
            Dropdown::Brushes => &mut app.brush_state.show_presets,
            Dropdown::Colour => &mut app.workspace.show_color,
            Dropdown::Layers => &mut app.workspace.show_layers,
        }
    }

    pub(crate) fn is_open(self, app: &mut PainterApp) -> bool {
        *self.flag(app)
    }

    /// Open this one (closing the others), or close it.
    pub(crate) fn toggle(self, app: &mut PainterApp) {
        let open = !self.is_open(app);
        for (d, _) in Dropdown::ALL {
            *d.flag(app) = false;
        }
        *self.flag(app) = open;
    }
}

/// Touch mode: one dropdown at a time, however it was opened (a toolbar
/// swatch, a shortcut): the one opened last wins.
fn one_dropdown(app: &mut PainterApp, ctx: &egui::Context) {
    let id = egui::Id::new("dropdowns_last_frame");
    let before: [bool; 3] = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let now = Dropdown::ALL.map(|(d, _)| d.is_open(app));
    if now.iter().filter(|o| **o).count() > 1 {
        let keep = (0..3)
            .find(|&i| now[i] && !before[i])
            .or((0..3).find(|&i| now[i]));
        for (i, (d, _)) in Dropdown::ALL.into_iter().enumerate() {
            *d.flag(app) = Some(i) == keep;
        }
    }
    let now = Dropdown::ALL.map(|(d, _)| d.is_open(app));
    ctx.data_mut(|d| d.insert_temp(id, now));
}

/// Touch mode: the dropdown buttons, right to left (layers, colour,
/// brushes), in a right-to-left layout on the top bar.
pub(crate) fn dropdown_buttons(app: &mut PainterApp, ui: &mut egui::Ui, size: f32) {
    for (dropdown, tip) in Dropdown::ALL.into_iter().rev() {
        let open = dropdown.is_open(app);
        let response = match dropdown {
            Dropdown::Brushes => icon_button(ui, Icon::Presets, size, open, tip),
            Dropdown::Colour => colour_button(app, ui, size, open),
            Dropdown::Layers => icon_button(ui, Icon::Layers, size, open, tip),
        };
        ui.data_mut(|d| d.insert_temp(dropdown.anchor_id(), response.rect));
        if response.clicked() {
            dropdown.toggle(app);
        }
    }
}

/// Touch mode: the open dropdown, under its button.
pub(crate) fn show_dropdowns(app: &mut PainterApp, ctx: &egui::Context) {
    one_dropdown(app, ctx);
    let screen = ctx.screen_rect();
    let tall = (screen.height() * 0.6).clamp(200.0, 520.0);
    for (dropdown, _) in Dropdown::ALL {
        let Some(anchor) = ctx.data(|d| d.get_temp::<egui::Rect>(dropdown.anchor_id())) else {
            continue;
        };
        let width = DROPDOWN_WIDTH.min(screen.width() - 40.0);
        let height = match dropdown {
            // The wheel, the colour pair and the four sliders, unscrolled.
            Dropdown::Colour => {
                let wheel = width.min(metrics(ctx).wheel_max);
                (wheel + COLOUR_BELOW_WHEEL).min(screen.height() - 80.0)
            }
            _ => tall,
        };
        let size = egui::vec2(width, height);
        let mut open = dropdown.is_open(app);
        let id = format!("dropdown_{}", dropdown as u8);
        ui::widgets::dropdown(ctx, &id, &mut open, anchor, size, |ui| match dropdown {
            Dropdown::Brushes => ui::brush_list::presets_contents(app, ui),
            Dropdown::Colour => ui::color_picker::color_picker_panel(
                ui,
                &mut app.brush_state,
                app.workspace.color_model,
            ),
            Dropdown::Layers => {
                let ctx = ui.ctx().clone();
                ui::layers::layers_panel(&ctx, ui, app);
            }
        });
        *dropdown.flag(app) = open;
    }
}

/// The brush colour as a button: the colour itself in a ring.
fn colour_button(app: &PainterApp, ui: &mut egui::Ui, size: f32, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
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
    response.on_hover_text("Colour")
}

/// Past the left side's edge, how near the pen or mouse brings it back.
const REVEAL_MARGIN: f32 = 32.0;

/// Touch mode with auto-hide: how much the left side (tool strip, from the
/// window's edge to `strip_right`, and the brush panel beside it) shows,
/// 0 to 1. It fades out slowly while painting or with the pen away, and
/// back quickly when the pen or mouse comes near.
pub(crate) fn left_reveal(app: &PainterApp, ctx: &egui::Context, strip_right: f32) -> f32 {
    let id = egui::Id::new("left_reveal");
    let (shown, wanted): (f32, bool) = ctx.data(|d| d.get_temp(id)).unwrap_or((1.0, true));
    // Near the strip's edge, or on the strip or the brush panel themselves
    // (whatever their size).
    let near = |p: egui::Pos2| {
        p.x <= strip_right + REVEAL_MARGIN
            || ctx.layer_id_at(p).is_some_and(|l| {
                l.id == egui::Id::new("toolbar_overlay") || l.id == egui::Id::new("float_brush")
            })
    };
    let (hover, down, origin, dt) = ctx.input(|i| {
        let p = &i.pointer;
        (p.hover_pos(), p.any_down(), p.press_origin(), i.stable_dt)
    });
    let ms = &app.modal_state;
    // A menu of the strip's, or a list dropped from the brush panel, is open.
    // The brush panel stays too, like those menus, until closed.
    let menu = ms.select_menu_open
        || ms.symmetry_menu_open
        || ms.shape_menu_open
        || app.workspace.show_left_panel;
    let wanted = if menu || ctx.memory(|m| m.any_popup_open()) {
        true
    } else if down {
        // Painting: hide, unless the press was on the side itself.
        origin.is_some_and(near)
    } else {
        // A finger lifted leaves no position: keep what it was.
        hover.map_or(wanted, near)
    };
    let step = if wanted { dt / 0.15 } else { -dt / 0.8 };
    let shown = (shown + step).clamp(0.0, 1.0);
    ctx.data_mut(|d| d.insert_temp(id, (shown, wanted)));
    if shown > 0.0 && shown < 1.0 {
        ctx.request_repaint();
    }
    shown
}

/// A brush panel floating over the canvas: its width.
fn float_width(ctx: &egui::Context) -> f32 {
    PANEL_WIDTH
        .min(ctx.available_rect().width() - MIN_CANVAS_WIDTH)
        .max(160.0)
}

/// Touch auto-hide: the left side (the `strip` and the brush panel beside
/// it) slides in from the window's edge as it shows. Returns its offset
/// (0 when fully in, negative while sliding out) and how far into the
/// canvas it reaches, for the canvas overlays to make way.
pub(crate) fn left_slide(app: &PainterApp, ctx: &egui::Context, strip: f32) -> (f32, f32) {
    let panel = if app.workspace.show_left_panel {
        float_width(ctx)
    } else {
        0.0
    };
    let shown = left_shown(app, ctx);
    let total = strip + panel;
    (-(1.0 - shown) * total, shown * total)
}

/// How far the left side reaches into the canvas (see [`left_slide`]).
pub(crate) fn left_cover(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp(egui::Id::new("left_cover")))
        .unwrap_or(0.0)
}

/// How much the left side shows (1 unless touch mode hides it).
fn left_shown(app: &PainterApp, ctx: &egui::Context) -> f32 {
    if !(app.workspace.touch_mode && app.workspace.autohide_panels) {
        return 1.0;
    }
    let state: Option<(f32, bool)> = ctx.data(|d| d.get_temp(egui::Id::new("left_reveal")));
    state.map_or(1.0, |(shown, _)| shown)
}

/// The right rail: the brush colour (opens the colour panel) and layers.
/// Touch mode has them on the top bar instead.
pub(crate) fn right_rail(app: &mut PainterApp, ctx: &egui::Context) {
    if app.workspace.touch_mode {
        return;
    }
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
            let open = app.workspace.show_color;
            if colour_button(app, ui, size, open).clicked() {
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
    let touch = app.workspace.touch_mode;
    // Touch mode has colour and layers in dropdowns (show_dropdowns).
    let right_open = !touch && (app.workspace.show_color || app.workspace.show_layers);
    // Touch mode: over the canvas, so opening one never resizes it.
    if !touch && !narrow(ctx) {
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
    let right_open = !touch && (ws.show_color || ws.show_layers);
    ctx.data_mut(|d| d.insert_temp(id, (ws.show_left_panel, right_open)));
    let area = ctx.available_rect();
    let width = float_width(ctx);
    // Beside the tool strip, which floats too when it auto-hides.
    let shown = left_shown(app, ctx);
    let strip: f32 = if touch && app.workspace.autohide_panels {
        ctx.data(|d| d.get_temp(egui::Id::new("toolbar_overlay_width")))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let cover = if strip > 0.0 {
        left_slide(app, ctx, strip).1
    } else {
        0.0
    };
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("left_cover"), cover));
    let floating = |id: &str, left: f32, opacity: f32, contents: &mut dyn FnMut(&mut egui::Ui)| {
        egui::Area::new(egui::Id::new(id))
            .fixed_pos(egui::pos2(left, area.top()))
            .order(egui::Order::Middle)
            .interactable(opacity > 0.3)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
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
        let left = area.left() + strip + left_slide(app, ctx, strip).0;
        floating("float_brush", left, shown, &mut |ui| brush_panel(app, ui));
    }
    if right_open {
        floating("float_right", area.right() - width, 1.0, &mut |ui| {
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
        match serde_json::to_vec_pretty(&widths) {
            Ok(bytes) => {
                crate::app::jobs::write_later(self.panel_widths_path(), "panel widths", move || {
                    Ok(bytes)
                })
            }
            Err(err) => log::warn!("Couldn't save the panel widths: {err}"),
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
    // Touch mode keeps panels over the canvas.
    let floats = narrow(ctx) || app.workspace.touch_mode;
    if !floats || !app.any_panel_open() {
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
        // Waits (greyed) while strokes are still being painted: its edits
        // need the canvas to itself.
        let settling = app.strokes_settling();
        ui.add_enabled_ui(!settling, |ui| ui::layers::layers_panel(&ctx, ui, app));
        if settling {
            ctx.request_repaint();
        }
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

    /// Touch mode's brushes/colour/layers dropdowns: one open at a time.
    #[test]
    fn one_dropdown_open() {
        use super::Dropdown;
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.workspace.touch_mode = true;
        Dropdown::Colour.toggle(&mut app);
        Dropdown::Layers.toggle(&mut app);
        assert!(!app.workspace.show_color && app.workspace.show_layers);
        Dropdown::Layers.toggle(&mut app);
        assert!(!app.workspace.show_layers);
        // Opened another way (the toolbar swatch): the newest one wins.
        let ctx = egui::Context::default();
        app.workspace.show_layers = true;
        super::one_dropdown(&mut app, &ctx);
        app.brush_state.show_presets = true;
        super::one_dropdown(&mut app, &ctx);
        assert!(app.brush_state.show_presets && !app.workspace.show_layers);
    }

    /// Touch mode: the floating brush panel is as wide as it's meant to be
    /// (a row too wide for it used to stretch it, leaving an empty band).
    #[test]
    fn floating_brush_panel_keeps_its_width() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.workspace.touch_mode = true;
        app.workspace.show_left_panel = true;
        let ctx = egui::Context::default();
        crate::ui::theme::apply_style(&ctx, true);
        crate::ui::style::set_touch_metrics(&ctx, true);
        for _ in 0..10 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                crate::ui::toolbar::toolbar(&mut app, ctx);
                super::show_panels(&mut app, ctx);
            });
        }
        let rect = ctx.memory(|m| m.area_rect(egui::Id::new("float_brush")));
        let width = rect.expect("the brush panel floats").width();
        assert!(width <= super::PANEL_WIDTH + 1.0, "{width}");
    }

    /// Auto-hide: the side fades out with the pen away, back when it's near.
    #[test]
    fn side_fades_with_the_pen() {
        let app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        let ctx = egui::Context::default();
        let mut shown = 1.0;
        let mut frames = |x: f32, n: usize| {
            for _ in 0..n {
                let input = egui::RawInput {
                    predicted_dt: 0.05,
                    events: vec![egui::Event::PointerMoved(egui::pos2(x, 100.0))],
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| shown = super::left_reveal(&app, ctx, 50.0));
            }
            shown
        };
        let away = frames(600.0, 3);
        assert!(away > 0.0 && away < 1.0, "fades slowly: {away}");
        assert_eq!(frames(600.0, 30), 0.0);
        assert_eq!(frames(40.0, 5), 1.0);
    }

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
