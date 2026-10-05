//! Small reusable widgets that give every panel the same flat, square look:
//! segmented controls, labeled property rows, section headers, icon buttons
//! and color swatches.

use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use eframe::egui::{self, Color32, Rect, Response, RichText, Sense, Stroke};

/// Room kept between a floating window and the app's edges.
const WINDOW_MARGIN: f32 = 8.0;

/// Floating windows kept whole inside the app, whatever its size: never
/// wider or taller than the window (or, after a resize, left past its
/// edges).
pub(crate) trait FitScreen: Sized {
    /// Sized by its contents: what doesn't fit scrolls.
    fn fit_screen(self, ctx: &egui::Context) -> Self;
    /// Resizable (it scrolls its own contents): only capped.
    fn fit_screen_size(self, ctx: &egui::Context) -> Self;
}

impl FitScreen for egui::Window<'_> {
    fn fit_screen(self, ctx: &egui::Context) -> Self {
        self.fit_screen_size(ctx).vscroll(true)
    }

    fn fit_screen_size(self, ctx: &egui::Context) -> Self {
        let room = ctx.screen_rect().shrink(WINDOW_MARGIN);
        // The cap is on the contents: without the title bar and the
        // frame's margins and border, those stuck out past the app's edge.
        let style = ctx.style();
        let frame = egui::Frame::window(&style);
        let title = ctx.fonts(|f| f.row_height(&egui::TextStyle::Body.resolve(&style)))
            + frame.inner_margin.sum().y;
        let chrome = frame.inner_margin.sum()
            + frame.outer_margin.sum()
            + egui::vec2(0.0, title)
            + egui::Vec2::splat(2.0 * frame.stroke.width);
        self.constrain_to(room)
            .max_size((room.size() - chrome).max(egui::Vec2::splat(40.0)))
    }
}

/// A bar's frame: flat `fill`, 8 px side padding.
pub(crate) fn bar_frame(fill: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(8.0, 0.0))
}

/// Paints a transparency checkerboard into `rect`.
pub(crate) fn draw_checkerboard(painter: &egui::Painter, rect: Rect, cell: f32) {
    painter.rect_filled(rect, 0.0, CHECKERBOARD_LIGHT);
    let rows = ((rect.height() / cell).ceil() as i32).max(1);
    let cols = ((rect.width() / cell).ceil() as i32).max(1);
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                let min = egui::pos2(rect.left() + x as f32 * cell, rect.top() + y as f32 * cell);
                let max = (min + egui::vec2(cell, cell)).min(rect.max);
                painter.rect_filled(Rect::from_min_max(min, max), 0.0, CHECKERBOARD_DARK);
            }
        }
    }
}

/// Paints a color over a checkerboard with a hairline border.
pub(crate) fn paint_swatch(painter: &egui::Painter, rect: Rect, color: Color32) {
    if color.a() < 255 {
        draw_checkerboard(painter, rect, (rect.height() / 3.0).clamp(3.0, 8.0));
    }
    painter.rect_filled(rect, 0.0, color);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, BORDER));
}

/// A gradient's colours left to right across `rect`, over a checkerboard
/// where they're see-through.
pub(crate) fn paint_gradient_strip(
    painter: &egui::Painter,
    rect: Rect,
    stops: &[crate::canvas::gradient::Stop],
) {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return;
    };
    if stops.iter().any(|s| s.color.a() < 255) {
        draw_checkerboard(painter, rect, (rect.height() / 3.0).clamp(3.0, 8.0));
    }
    // The end colours carry on to the edges.
    let points: Vec<(f32, Color32)> = std::iter::once((0.0, first.color))
        .chain(stops.iter().map(|s| (s.pos.clamp(0.0, 1.0), s.color)))
        .chain(std::iter::once((1.0, last.color)))
        .collect();
    let mut mesh = egui::Mesh::default();
    for (pos, color) in points {
        let x = egui::lerp(rect.left()..=rect.right(), pos);
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(x, rect.top()), color);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), color);
        if i > 0 {
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i, i + 1);
        }
    }
    painter.add(mesh);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, BORDER));
}

/// A clickable color swatch of the given size.
pub(crate) fn color_swatch(ui: &mut egui::Ui, color: Color32, size: egui::Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    paint_swatch(ui.painter(), rect, color);
    if response.hovered() {
        ui.painter()
            .rect_stroke(rect.expand(1.0), 0.0, Stroke::new(1.0_f32, TEXT_STRONG));
    }
    response
}

/// Square button showing an icon. `selected` fills it with the accent.
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    selected: bool,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    let (bg, fg) = if selected {
        (ACCENT, TEXT_STRONG)
    } else if response.is_pointer_button_down_on() {
        (WIDGET_ACTIVE, TEXT_STRONG)
    } else if response.hovered() {
        (WIDGET_HOVER, TEXT_STRONG)
    } else {
        (Color32::TRANSPARENT, TEXT)
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    let inset = (size * 0.24).round();
    paint_icon(ui.painter(), rect.shrink(inset), icon, fg);
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Small borderless icon toggle used in list rows (visibility, lock).
pub(crate) fn icon_toggle(
    ui: &mut egui::Ui,
    on: &mut bool,
    icon_on: Icon,
    icon_off: Icon,
    tooltip: &str,
) -> bool {
    let size = metrics(ui.ctx()).row_toggle;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, WIDGET_HOVER);
    }
    let (icon, color) = if *on {
        (icon_on, TEXT)
    } else {
        (icon_off, TEXT_DIM)
    };
    paint_icon(ui.painter(), rect.shrink(size * 0.16), icon, color);
    let clicked = response.on_hover_text(tooltip).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// Joined row of mutually exclusive options. Fills the available width
/// with equal segments unless `compact`, where each segment fits its label.
pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
    compact: bool,
) -> bool {
    let mut changed = false;
    let height = ui.spacing().interact_size.y;
    let font = egui::TextStyle::Button.resolve(ui.style());
    let pad = ui.spacing().button_padding.x * 1.5;
    let equal_width = (ui.available_width() / options.len().max(1) as f32).floor();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        for (option, label) in options {
            let galley = ui
                .painter()
                .layout_no_wrap(label.to_string(), font.clone(), TEXT);
            let width = if compact {
                galley.size().x + pad * 2.0
            } else {
                (equal_width - 1.0).max(galley.size().x + 8.0)
            };
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
            let selected = *value == *option;
            let (bg, fg) = if selected {
                (ACCENT, TEXT_STRONG)
            } else if response.hovered() {
                (WIDGET_HOVER, TEXT_STRONG)
            } else {
                (WIDGET, TEXT)
            };
            ui.painter().rect_filled(rect, 0.0, bg);
            ui.painter().galley(
                rect.center() - galley.size() * 0.5,
                ui.painter()
                    .layout_no_wrap(label.to_string(), font.clone(), fg),
                fg,
            );
            if response.clicked() && !selected {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

/// Dim fixed-width label that starts a property row.
fn row_label(ui: &mut egui::Ui, label: &str) {
    let height = ui.spacing().interact_size.y;
    let width = metrics(ui.ctx()).label_width;
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.label(RichText::new(label).color(TEXT_DIM));
        },
    );
}

/// `label  [slider ----------] [value]` on one line, the slider filling
/// the space left by the label and value box.
pub(crate) fn slider_row(ui: &mut egui::Ui, label: &str, slider: impl egui::Widget) -> Response {
    // egui sizes the number box to its text, so a long value ("1000 px")
    // comes out wider than `value_box_width`. The row must still end at the
    // edge: a row a few pixels too wide widens an auto-sized window, which
    // gives the next frame's row more room to overflow again, and the window
    // grows every time the pointer moves. So the overflow is measured and
    // taken off the slider next frame.
    let overflow_id = ui.next_auto_id().with(("slider_row_overflow", label));
    let extra = ui.data(|d| d.get_temp::<f32>(overflow_id)).unwrap_or(0.0);
    ui.horizontal(|ui| {
        row_label(ui, label);
        let spacing = ui.spacing().item_spacing.x;
        let value_box = metrics(ui.ctx()).value_box_width;
        let room = ui.available_width();
        let right_edge = ui.cursor().left() + room;
        let slider_width = (room - value_box - extra - spacing).max(40.0);
        ui.spacing_mut().slider_width = slider_width;
        let response = ui.add(slider);
        let over = ui.min_rect().right() - right_edge;
        let next = (extra + over).clamp(0.0, (room - value_box - spacing - 40.0).max(0.0));
        if (next - extra).abs() > 0.25 {
            ui.data_mut(|d| d.insert_temp(overflow_id, next));
            ui.ctx().request_repaint();
        }
        response
    })
    .inner
}

/// `label  [contents]` on one line.
pub(crate) fn property_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        row_label(ui, label);
        add_contents(ui)
    })
    .inner
}

/// Formats a 0..=1 value as a whole percentage and parses it back.
pub(crate) fn percent_of_unit(slider: egui::Slider) -> egui::Slider {
    slider
        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
        .custom_parser(|s| {
            s.trim()
                .trim_end_matches('%')
                .trim()
                .parse::<f64>()
                .ok()
                .map(|v| v / 100.0)
        })
}

/// Collapsible section: a hairline, a small uppercase title with a
/// disclosure triangle, and an unindented body.
pub(crate) fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let id = ui.id().with(("section", title));
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(default_open);

    ui.add_space(4.0);
    let header_height = if metrics(ui.ctx()).touch { 34.0 } else { 22.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), header_height),
        Sense::click(),
    );
    let painter = ui.painter();
    painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0_f32, BORDER));
    let color = if response.hovered() { TEXT } else { TEXT_DIM };
    let c = egui::pos2(rect.left() + 4.0, rect.center().y + 1.0);
    let tri = if open {
        vec![
            c + egui::vec2(-3.5, -2.0),
            c + egui::vec2(3.5, -2.0),
            c + egui::vec2(0.0, 2.5),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -3.5),
            c + egui::vec2(2.5, 0.0),
            c + egui::vec2(-2.0, 3.5),
        ]
    };
    painter.add(egui::Shape::convex_polygon(tri, color, Stroke::NONE));
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y + 1.0),
        egui::Align2::LEFT_CENTER,
        title.to_uppercase(),
        egui::FontId::proportional(11.0),
        color,
    );
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }

    open.then(|| add_contents(ui))
}

/// Thin vertical divider for horizontal bars.
pub(crate) fn vdivider(ui: &mut egui::Ui) {
    let height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, height), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range().shrink(2.0),
        Stroke::new(1.0_f32, BORDER_LIGHT),
    );
}

/// A menu sliding out from a toolbar button (`anchor`) while `*open`;
/// scrolls when taller than the screen, and closes on a click elsewhere.
pub(crate) fn flyout(
    ctx: &egui::Context,
    id: &str,
    open: &mut bool,
    anchor: Rect,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let t = ctx.animate_bool_with_time(egui::Id::new((id, "anim")), *open, 0.12);
    if t <= 0.0 {
        return;
    }
    let x = anchor.right() + 4.0 - (1.0 - t) * width;
    let screen = ctx.screen_rect();
    // Low buttons open upward enough to fit.
    let top = anchor
        .top()
        .min((screen.bottom() - 260.0).max(screen.top()));
    let response = egui::Area::new(egui::Id::new(id))
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
                        .max_height((screen.bottom() - top - 24.0).max(120.0))
                        .show(ui, add_contents);
                });
        })
        .response;
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !response.rect.contains(p) && !anchor.contains(p));
    if *open && clicked_outside {
        *open = false;
    }
}

/// A small menu dropping from a top-bar button (`anchor`) while `*open`,
/// its right edge under the button's; `size` is its content's. Closes on a
/// tap on the canvas or a panel (not on its own popups, like a combo box).
pub(crate) fn dropdown(
    ctx: &egui::Context,
    id: &str,
    open: &mut bool,
    anchor: Rect,
    size: egui::Vec2,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let t = ctx.animate_bool_with_time(egui::Id::new((id, "anim")), *open, 0.12);
    if t <= 0.0 {
        return;
    }
    let screen = ctx.screen_rect();
    let x = (anchor.right() - size.x - 16.0).max(screen.left());
    let y = anchor.bottom() + 2.0 - (1.0 - t) * 12.0;
    egui::Area::new(egui::Id::new(id))
        .fixed_pos(egui::pos2(x, y))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::none()
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_LIGHT))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(size.x);
                    ui.set_height(size.y);
                    ui.set_clip_rect(ui.max_rect().expand(1.0));
                    add_contents(ui);
                });
        });
    let tapped_behind = ctx.input(|i| i.pointer.any_pressed())
        && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| {
            !anchor.contains(p)
                && ctx
                    .layer_id_at(p)
                    .is_none_or(|l| l.order == egui::Order::Background)
        });
    if *open && tapped_behind {
        *open = false;
    }
}

/// Sliders reset to this "default" epoch's first value; bumped when a preset
/// is chosen, so their defaults become the preset's.
static DEFAULTS_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The brush changed wholesale (a preset): sliders take their current
/// values as the ones a double-click returns to.
pub(crate) fn new_slider_defaults() {
    DEFAULTS_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A slider that a double-click (or double-tap) resets: to its value when
/// first shown since the last [`new_slider_defaults`] (the preset's, for
/// brush settings; the default, for tools).
pub(crate) fn reset<'a, T, F>(value: &'a mut T, build: F) -> Reset<'a, T, F>
where
    T: egui::emath::Numeric + Send + Sync + 'static,
    F: for<'b> FnOnce(&'b mut T) -> egui::Slider<'b>,
{
    Reset { value, build }
}

pub(crate) struct Reset<'a, T, F> {
    value: &'a mut T,
    build: F,
}

impl<T, F> egui::Widget for Reset<'_, T, F>
where
    T: egui::emath::Numeric + Send + Sync + 'static,
    F: for<'b> FnOnce(&'b mut T) -> egui::Slider<'b>,
{
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let before = *self.value;
        let mut response = ui.add((self.build)(self.value));
        let epoch = DEFAULTS_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
        let key = response.id.with(("slider_default", epoch));
        let default = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(key, || before));
        // The second click of a double-click is also a press the slider acts
        // on (it jumps to the pointer): the reset holds until the button is
        // let go.
        let holding = response.id.with("slider_resetting");
        // A slider senses drags, not clicks, so its response never reports
        // a double-click: read it off the pointer (a finger's double-tap
        // comes the same way).
        let double = ui.input(|i| {
            i.pointer
                .button_double_clicked(egui::PointerButton::Primary)
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| response.rect.contains(p))
        });
        if double {
            ui.data_mut(|d| d.insert_temp(holding, true));
        }
        if ui.data(|d| d.get_temp::<bool>(holding)).unwrap_or(false) {
            if *self.value != default {
                *self.value = default;
                response.mark_changed();
            }
            if !ui.input(|i| i.pointer.any_down()) {
                ui.data_mut(|d| d.remove::<bool>(holding));
            }
        }
        response
    }
}

#[cfg(test)]
mod reset_tests {
    use super::*;

    /// Runs one frame showing a slider on `value`; `events` are this frame's
    /// input. Returns the slider's rect.
    fn frame(ctx: &egui::Context, value: &mut f32, events: Vec<egui::Event>, t: f64) -> Rect {
        let mut rect = Rect::NOTHING;
        let _ = ctx.run(
            egui::RawInput {
                events,
                time: Some(t),
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    rect = ui
                        .add(reset(value, |v| egui::Slider::new(v, 0.0..=1.0)))
                        .rect;
                });
            },
        );
        rect
    }

    fn click(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    /// Two clicks in quick succession, each press and release in its own
    /// frame as real input comes.
    fn double_click(ctx: &egui::Context, value: &mut f32, at: egui::Pos2, t: f64) {
        frame(ctx, value, vec![egui::Event::PointerMoved(at)], t);
        for (i, pressed) in [true, false, true, false].into_iter().enumerate() {
            frame(
                ctx,
                value,
                vec![click(at, pressed)],
                t + 0.05 * (i + 1) as f64,
            );
        }
        frame(ctx, value, vec![], t + 0.3);
    }

    #[test]
    fn a_double_click_returns_a_slider_to_its_default() {
        let ctx = egui::Context::default();
        let mut value = 0.5;
        let rect = frame(&ctx, &mut value, vec![], 0.0);
        value = 0.8;
        frame(&ctx, &mut value, vec![], 0.1);
        // On the track, away from where a click would move it much.
        let at = rect.left_center() + egui::vec2(20.0, 0.0);
        double_click(&ctx, &mut value, at, 1.0);
        assert!((value - 0.5).abs() < 1e-6, "reset to 0.5, got {value}");

        // New defaults (a preset chosen): the current value becomes it.
        value = 0.3;
        new_slider_defaults();
        frame(&ctx, &mut value, vec![], 2.0);
        value = 0.9;
        double_click(&ctx, &mut value, at, 3.0);
        assert!(
            (value - 0.3).abs() < 1e-6,
            "reset to the new default, got {value}"
        );
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;

    /// Width of an auto-sized window over `frames` frames with the pointer moving.
    fn window_widths(frames: usize, contents: impl Fn(&mut egui::Ui)) -> Vec<f32> {
        let ctx = egui::Context::default();
        crate::ui::theme::apply_global_style(&ctx);
        let mut widths = Vec::new();
        for i in 0..frames {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(egui::pos2(
                800.0 + i as f32,
                700.0,
            )));
            let mut w = 0.0;
            let _ = ctx.run(input, |ctx| {
                let r = egui::Window::new("t")
                    .resizable(false)
                    .default_width(340.0)
                    .show(ctx, |ui| contents(ui));
                w = r.unwrap().response.rect.width();
            });
            widths.push(w);
        }
        widths
    }

    #[test]
    fn slider_rows_do_not_grow_their_window() {
        let widths = window_widths(30, |ui| {
            let mut a = 1000.0_f32;
            slider_row(
                ui,
                "Radius",
                egui::Slider::new(&mut a, 0.0..=1000.0).suffix(" px"),
            );
            let mut b = 0.5_f32;
            slider_row(
                ui,
                "A much longer label",
                percent_of_unit(egui::Slider::new(&mut b, 0.0..=1.0)),
            );
        });
        let last = widths[widths.len() - 1];
        assert!(
            (last - widths[widths.len() - 10]).abs() < 0.5 && last < 400.0,
            "window kept growing: {widths:?}"
        );
    }

    #[test]
    fn filter_dialogs_do_not_grow() {
        use crate::canvas::filters::Filter;
        for group in Filter::MENU {
            for f in group.iter() {
                let widths = window_widths(40, |ui| {
                    let mut f = *f;
                    crate::ui::filter_dialog::settings(ui, &mut f);
                });
                let last = widths[widths.len() - 1];
                assert!(
                    (last - widths[widths.len() - 20]).abs() < 0.5,
                    "{} keeps resizing: {widths:?}",
                    f.name()
                );
            }
        }
    }
}
