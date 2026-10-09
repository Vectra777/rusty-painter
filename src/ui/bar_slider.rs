//! Blender's number slider: one rounded field, filled with the accent up
//! to its value, its label and value written inside. Dragged anywhere on it
//! the value moves by how far the pointer went (Shift: a tenth as fast), not
//! to where the pointer is; a click without a drag types a value in.
//!
//! Built like `egui::Slider` (the same builder methods), so it takes its
//! place wherever one was used.

use crate::ui::style::*;
use eframe::egui::{self, Response, Sense, Ui, Widget, WidgetInfo, emath};
use std::ops::RangeInclusive;

type GetSet<'a> = Box<dyn 'a + FnMut(Option<f64>) -> f64>;
type Formatter<'a> = Box<dyn 'a + Fn(f64, RangeInclusive<usize>) -> String>;
type Parser<'a> = Box<dyn 'a + Fn(&str) -> Option<f64>>;

#[cfg(test)]
thread_local! {
    /// Each bar drawn: its label, rect and clip (tests look for overflow).
    /// The label of the `slider_row` being drawn.
    pub(crate) static ROW: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    pub(crate) static DRAWN: std::cell::RefCell<Vec<(String, egui::Rect, egui::Rect)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// How long a typed or reset value takes to slide into place.
const SETTLE_TIME: f32 = 0.1;
/// Past this many points the press is a drag, not a click.
const DRAG_THRESHOLD: f32 = 3.0;
/// egui's longest gap between the clicks of a double-click (not exposed).
const DOUBLE_CLICK_WINDOW: f64 = 0.3;
/// Shift: how much slower the value follows the pointer.
const FINE_SPEED: f64 = 0.1;

#[must_use = "add it with `ui.add(...)`"]
pub(crate) struct BarSlider<'a> {
    get_set: GetSet<'a>,
    range: RangeInclusive<f64>,
    spec: Spec,
    integer: bool,
    show_value: bool,
    suffix: String,
    text: String,
    step: Option<f64>,
    max_decimals: Option<usize>,
    formatter: Option<Formatter<'a>>,
    parser: Option<Parser<'a>>,
}

/// How the bar's width maps to the range (as `egui::Slider` does it).
#[derive(Clone, Copy)]
struct Spec {
    logarithmic: bool,
    smallest_positive: f64,
}

/// The text being typed in, while a bar is being edited.
#[derive(Clone, Default)]
struct Editing {
    text: String,
    /// Focus was asked for (the first frame).
    focused: bool,
}

impl<'a> BarSlider<'a> {
    pub(crate) fn new<Num: emath::Numeric>(value: &'a mut Num, range: RangeInclusive<Num>) -> Self {
        let range = range.start().to_f64()..=range.end().to_f64();
        let slider = Self {
            get_set: Box::new(move |v: Option<f64>| {
                if let Some(v) = v {
                    *value = Num::from_f64(v);
                }
                value.to_f64()
            }),
            range,
            spec: Spec {
                logarithmic: false,
                smallest_positive: 1e-6,
            },
            integer: false,
            show_value: true,
            suffix: String::new(),
            text: String::new(),
            step: None,
            max_decimals: None,
            formatter: None,
            parser: None,
        };
        if Num::INTEGRAL {
            Self {
                integer: true,
                ..slider.max_decimals(0).smallest_positive(1.0).step_by(1.0)
            }
        } else {
            slider
        }
    }

    /// The label, written at the bar's left.
    pub(crate) fn text(mut self, text: impl ToString) -> Self {
        self.text = text.to_string();
        self
    }

    pub(crate) fn suffix(mut self, suffix: impl ToString) -> Self {
        self.suffix = suffix.to_string();
        self
    }

    pub(crate) fn show_value(mut self, show: bool) -> Self {
        self.show_value = show;
        self
    }

    pub(crate) fn logarithmic(mut self, logarithmic: bool) -> Self {
        self.spec.logarithmic = logarithmic;
        self
    }

    /// For a logarithmic bar starting at 0: the smallest value above it.
    pub(crate) fn smallest_positive(mut self, smallest_positive: f64) -> Self {
        self.spec.smallest_positive = smallest_positive;
        self
    }

    pub(crate) fn step_by(mut self, step: f64) -> Self {
        self.step = (step != 0.0).then_some(step);
        self
    }

    pub(crate) fn max_decimals(mut self, max_decimals: usize) -> Self {
        self.max_decimals = Some(max_decimals);
        self
    }

    /// The value as shown: `(value, decimals allowed) -> text`.
    pub(crate) fn custom_formatter(
        mut self,
        formatter: impl 'a + Fn(f64, RangeInclusive<usize>) -> String,
    ) -> Self {
        self.formatter = Some(Box::new(formatter));
        self
    }

    /// What's typed in, read back (`None`: not a value, nothing changes).
    pub(crate) fn custom_parser(mut self, parser: impl 'a + Fn(&str) -> Option<f64>) -> Self {
        self.parser = Some(Box::new(parser));
        self
    }

    fn get(&mut self) -> f64 {
        clamp((self.get_set)(None), &self.range)
    }

    fn set(&mut self, value: f64) {
        let mut value = clamp(value, &self.range);
        if let Some(step) = self.step {
            let start = *self.range.start();
            value = clamp(start + ((value - start) / step).round() * step, &self.range);
        }
        if let Some(decimals) = self.max_decimals {
            value = emath::round_to_decimals(value, decimals);
        }
        (self.get_set)(Some(value));
    }

    /// The value as written in the bar, suffix included.
    fn format(&self, value: f64, width: f32) -> String {
        let max = self
            .max_decimals
            .unwrap_or_else(|| self.auto_decimals(width));
        let number = match &self.formatter {
            Some(f) => f(value, 0..=max),
            None => emath::format_with_decimals_in_range(value, 0..=max),
        };
        format!("{number}{}", self.suffix)
    }

    /// Enough decimals to tell apart the values a point of the bar spans.
    fn auto_decimals(&self, width: f32) -> usize {
        if self.integer {
            return 0;
        }
        let span = (self.range.end() - self.range.start()).abs();
        let per_point = span / width.max(1.0) as f64;
        if !per_point.is_finite() || per_point <= 0.0 {
            return 2;
        }
        (-per_point.log10()).ceil().clamp(0.0, 6.0) as usize
    }

    /// What's typed in as a value: the parser, or a number with the suffix
    /// (if typed too) left off.
    fn parse(&self, text: &str) -> Option<f64> {
        if let Some(parser) = &self.parser {
            return parser(text);
        }
        let text = text.trim();
        let text = text.strip_suffix(self.suffix.trim()).unwrap_or(text).trim();
        text.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    fn normalized(&self, value: f64) -> f64 {
        normalized_from_value(value, self.range.clone(), &self.spec)
    }

    fn value_at(&self, normalized: f64) -> f64 {
        value_from_normalized(normalized, self.range.clone(), &self.spec)
    }

    /// Its size: the row's width in a column; in a row, `slider_width` and
    /// room for the value (and the label, if any).
    fn desired_size(&self, ui: &Ui) -> egui::Vec2 {
        let height = ui
            .spacing()
            .interact_size
            .y
            .max(ui.text_style_height(&egui::TextStyle::Body) + 4.0);
        let width = if ui.layout().main_dir().is_horizontal() {
            let label = if self.text.is_empty() {
                0.0
            } else {
                let font = egui::TextStyle::Button.resolve(ui.style());
                ui.fonts(|f| f.layout_no_wrap(self.text.clone(), font, TEXT).size().x) + 12.0
            };
            let value = if self.show_value {
                metrics(ui.ctx()).value_box_width
            } else {
                0.0
            };
            let natural = ui.spacing().slider_width + value;
            // In a squeezed row (the top bar, short of room), narrower.
            let squeeze = ui.ctx().data(|d| d.get_temp::<f32>(squeeze_id()));
            match squeeze {
                Some(squeeze) => {
                    let width = natural * squeeze;
                    ui.ctx().data_mut(|d| {
                        *d.get_temp_mut_or_default::<f32>(squeezed_width_id()) += width
                    });
                    width + label
                }
                // Elsewhere never past the row's end.
                None => (natural + label).min(ui.available_width()),
            }
        } else {
            ui.available_width()
        };
        egui::vec2(width.max(40.0), height)
    }
}

fn squeeze_id() -> egui::Id {
    egui::Id::new("bar_slider_squeeze")
}

fn squeezed_width_id() -> egui::Id {
    egui::Id::new("bar_slider_squeezed_width")
}

/// Show `add_contents` (a row) in `width`, the bars in it narrowed as much
/// as it takes to fit, to a point. Past it the row wraps: returns how many
/// rows' height it wants (it lays itself out in what it's given; `id`
/// keeps the narrowing and the width it needs from frame to frame).
pub(crate) fn fitted_row(
    ui: &mut Ui,
    id: egui::Id,
    width: f32,
    row_height: f32,
    add_contents: impl FnOnce(&mut Ui),
) -> u8 {
    const NARROWEST: f32 = 0.45;
    let needed_id = id.with("needed");
    let height = ui.available_height();
    // Known not to fit on one row at this width: wrapped onto two.
    if let Some(needed) = ui.data(|d| d.get_temp::<f32>(needed_id))
        && width + 0.5 < needed
    {
        let used = ui
            .allocate_ui_with_layout(
                egui::vec2(width, height),
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                add_contents,
            )
            .response
            .rect
            .height();
        // As many rows as the wrapped contents took (a row: `row_height`).
        return (used / row_height.max(1.0) - 0.05).ceil().clamp(2.0, 6.0) as u8;
    }
    let squeeze: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(1.0);
    ui.ctx().data_mut(|d| {
        d.insert_temp(squeeze_id(), squeeze);
        d.insert_temp(squeezed_width_id(), 0.0_f32);
    });
    // (Only for the frame it takes to find it doesn't fit: no bar to see.)
    let used = egui::ScrollArea::horizontal()
        .id_salt(id)
        .max_width(width)
        .auto_shrink([false, true])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            // As wide as the row at least (what's right-aligned in it goes
            // to its right end), wider if the contents are.
            ui.allocate_ui_with_layout(
                egui::vec2(width, height),
                egui::Layout::left_to_right(egui::Align::Center),
                add_contents,
            )
            .response
            .rect
            .width()
        })
        .inner;
    let bars = ui.ctx().data_mut(|d| {
        d.remove::<f32>(squeeze_id());
        d.remove_temp::<f32>(squeezed_width_id()).unwrap_or(0.0)
    });
    // The bars at full width, and everything else as it is.
    let (natural, rest) = (bars / squeeze, used - bars);
    let wanted = if bars > 0.0 {
        ((width - rest) / natural).clamp(NARROWEST, 1.0)
    } else {
        1.0
    };
    let narrowest = rest + natural * NARROWEST;
    if narrowest > width + 0.5 {
        ui.data_mut(|d| d.insert_temp(needed_id, narrowest));
        ui.ctx().request_repaint();
    } else {
        ui.data_mut(|d| d.remove::<f32>(needed_id));
    }
    if (wanted - squeeze).abs() > 0.005 {
        ui.data_mut(|d| d.insert_temp(id, wanted));
        ui.ctx().request_repaint();
    }
    1
}

impl Widget for BarSlider<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let size = self.desired_size(ui);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        #[cfg(test)]
        DRAWN.with(|d| {
            let row = ROW.with(|r| std::mem::take(&mut *r.borrow_mut()));
            let label = if self.text.is_empty() {
                row
            } else {
                self.text.clone()
            };
            d.borrow_mut().push((label, rect, ui.clip_rect()))
        });
        let id = response.id;
        let edit_id = id.with("bar_slider_edit");
        let drag_id = id.with("bar_slider_drag");
        let anim_id = id.with("bar_slider_fill");
        let before = self.get();

        // Typing a value in.
        if let Some(mut edit) = ui.data(|d| d.get_temp::<Editing>(edit_id)) {
            let text_id = id.with("bar_slider_text");
            let output = egui::TextEdit::singleline(&mut edit.text)
                .id(text_id)
                .horizontal_align(egui::Align::Center)
                .vertical_align(egui::Align::Center)
                .margin(egui::Margin::symmetric(6.0, 0.0))
                .frame(true)
                .show(&mut ui.new_child(egui::UiBuilder::new().max_rect(rect)));
            if !edit.focused {
                output.response.request_focus();
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(edit.text.chars().count()),
                );
                let mut state = output.state;
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), text_id);
                edit.focused = true;
            }
            let (enter, escape) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if escape {
                ui.data_mut(|d| d.remove::<Editing>(edit_id));
            } else if enter || output.response.lost_focus() {
                // Committed on Enter or clicking away, Blender's way.
                if let Some(v) = self.parse(&edit.text) {
                    self.set(v);
                }
                ui.data_mut(|d| d.remove::<Editing>(edit_id));
            } else {
                ui.data_mut(|d| d.insert_temp(edit_id, edit));
            }
            let after = self.get();
            if after != before {
                response.mark_changed();
            }
            response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), after, &self.text));
            return response;
        }

        // Dragging: relative to where the drag started.
        let dragging = response.dragged_by(egui::PointerButton::Primary);
        if response.drag_started_by(egui::PointerButton::Primary) {
            ui.data_mut(|d| d.insert_temp(drag_id, (self.normalized(before), 0.0_f32)));
        }
        if dragging
            && let Some((mut norm, mut travel)) = ui.data(|d| d.get_temp::<(f64, f32)>(drag_id))
        {
            let (delta, fine) = ui.input(|i| (i.pointer.delta().x, i.modifiers.shift));
            travel += delta.abs();
            let speed = if fine { FINE_SPEED } else { 1.0 };
            norm = (norm + delta as f64 / rect.width().max(1.0) as f64 * speed).clamp(0.0, 1.0);
            // A press that barely moves stays a click.
            if travel > DRAG_THRESHOLD {
                let value = self.value_at(norm);
                self.set(value);
            }
            ui.data_mut(|d| d.insert_temp(drag_id, (norm, travel)));
        }
        let travelled = ui
            .data(|d| d.get_temp::<(f64, f32)>(drag_id))
            .is_some_and(|(_, t)| t > DRAG_THRESHOLD);
        if response.drag_stopped() {
            ui.data_mut(|d| d.remove::<(f64, f32)>(drag_id));
        }

        // A click (not the end of a drag, nor half of a double-click, which
        // resets: see `widgets::reset`) types a value in, once the double
        // click's window has passed.
        let pending_id = id.with("bar_slider_pending_edit");
        if response.double_clicked() {
            ui.data_mut(|d| d.remove::<f64>(pending_id));
        } else if response.clicked() && !travelled {
            let now = ui.input(|i| i.time);
            ui.data_mut(|d| d.insert_temp(pending_id, now));
        }
        if let Some(at) = ui.data(|d| d.get_temp::<f64>(pending_id)) {
            let now = ui.input(|i| i.time);
            if now - at >= DOUBLE_CLICK_WINDOW {
                ui.data_mut(|d| d.remove::<f64>(pending_id));
                let value = self.get();
                let text = if self.formatter.is_some() || self.parser.is_some() {
                    self.format(value, rect.width())
                } else {
                    emath::format_with_decimals_in_range(value, 0..=self.max_decimals.unwrap_or(6))
                };
                ui.data_mut(|d| {
                    d.insert_temp(
                        edit_id,
                        Editing {
                            text,
                            focused: false,
                        },
                    )
                });
            }
            ui.ctx().request_repaint();
        }

        let value = self.get();
        if value != before {
            response.mark_changed();
        }
        response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), value, &self.text));

        if ui.is_rect_visible(rect) {
            let target = self.normalized(value) as f32;
            // A drag follows the pointer; a jump (typed, reset, a preset)
            // slides there.
            let settle = if dragging { 0.0 } else { SETTLE_TIME };
            let shown = ui.ctx().animate_value_with_time(anim_id, target, settle);
            self.paint(ui, rect, &response, dragging, shown, value);
        }
        response
    }
}

impl BarSlider<'_> {
    fn paint(
        &self,
        ui: &Ui,
        rect: egui::Rect,
        response: &Response,
        dragging: bool,
        fill: f32,
        value: f64,
    ) {
        let painter = ui.painter();
        let enabled = ui.is_enabled();
        let hovered = response.hovered() || dragging;
        let bg = if hovered { WIDGET } else { BG_INSET };
        painter.rect_filled(rect, RADIUS_WIDGET, bg);
        if fill > 0.0 {
            let width = rect.width() * fill.clamp(0.0, 1.0);
            let filled = egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()));
            let color = if dragging { accent() } else { accent_dim() };
            let color = if enabled {
                color
            } else {
                color.gamma_multiply(0.4)
            };
            // Rounded at the bar's left; square where it stops, until it
            // reaches the right end.
            let right = (RADIUS_WIDGET - (rect.width() - width)).clamp(0.0, RADIUS_WIDGET);
            let rounding = egui::Rounding {
                nw: RADIUS_WIDGET,
                sw: RADIUS_WIDGET,
                ne: right,
                se: right,
            };
            painter.rect_filled(filled, rounding, color);
        }
        if hovered {
            painter.rect_stroke(
                rect,
                RADIUS_WIDGET,
                egui::Stroke::new(1.0_f32, BORDER_LIGHT),
            );
        }

        let font = egui::TextStyle::Button.resolve(ui.style());
        let ink = if enabled { TEXT_STRONG } else { TEXT_DIM };
        let pad = 8.0;
        let value_text = self.show_value.then(|| self.format(value, rect.width()));
        let clip = painter.with_clip_rect(rect.shrink(1.0).intersect(painter.clip_rect()));
        match (self.text.is_empty(), value_text) {
            (true, Some(v)) => {
                clip.text(rect.center(), egui::Align2::CENTER_CENTER, v, font, ink);
            }
            (false, v) => {
                let label = if enabled { TEXT } else { TEXT_DIM };
                clip.text(
                    rect.left_center() + egui::vec2(pad, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &self.text,
                    font.clone(),
                    label,
                );
                if let Some(v) = v {
                    clip.text(
                        rect.right_center() - egui::vec2(pad, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        v,
                        font,
                        ink,
                    );
                }
            }
            (true, None) => {}
        }
        if hovered && enabled {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
}

fn clamp(value: f64, range: &RangeInclusive<f64>) -> f64 {
    let (a, b) = (*range.start(), *range.end());
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    if value.is_nan() {
        lo
    } else {
        value.clamp(lo, hi)
    }
}

// The range ↔ 0..=1 mapping, as `egui::Slider` has it (private there).
// Logarithmic ranges may include zero and infinity.

/// A range reaching infinity spans this many orders of magnitude.
const INF_RANGE_MAGNITUDE: f64 = 10.0;

fn value_from_normalized(normalized: f64, range: RangeInclusive<f64>, spec: &Spec) -> f64 {
    let (min, max) = (*range.start(), *range.end());
    if min.is_nan() || max.is_nan() {
        f64::NAN
    } else if min == max {
        min
    } else if min > max {
        value_from_normalized(1.0 - normalized, max..=min, spec)
    } else if normalized <= 0.0 {
        min
    } else if normalized >= 1.0 {
        max
    } else if spec.logarithmic {
        if max <= 0.0 {
            -value_from_normalized(normalized, -min..=-max, spec)
        } else if 0.0 <= min {
            let (min_log, max_log) = range_log10(min, max, spec);
            10.0_f64.powf(emath::lerp(min_log..=max_log, normalized))
        } else {
            let zero_cutoff = logarithmic_zero_cutoff(min, max);
            if normalized < zero_cutoff {
                value_from_normalized(
                    emath::remap(normalized, 0.0..=zero_cutoff, 0.0..=1.0),
                    min..=0.0,
                    spec,
                )
            } else {
                value_from_normalized(
                    emath::remap(normalized, zero_cutoff..=1.0, 0.0..=1.0),
                    0.0..=max,
                    spec,
                )
            }
        }
    } else {
        emath::lerp(range, normalized.clamp(0.0, 1.0))
    }
}

fn normalized_from_value(value: f64, range: RangeInclusive<f64>, spec: &Spec) -> f64 {
    let (min, max) = (*range.start(), *range.end());
    if min.is_nan() || max.is_nan() {
        f64::NAN
    } else if min == max {
        0.5
    } else if min > max {
        1.0 - normalized_from_value(value, max..=min, spec)
    } else if value <= min {
        0.0
    } else if value >= max {
        1.0
    } else if spec.logarithmic {
        if max <= 0.0 {
            normalized_from_value(-value, -min..=-max, spec)
        } else if 0.0 <= min {
            let (min_log, max_log) = range_log10(min, max, spec);
            emath::remap_clamp(value.log10(), min_log..=max_log, 0.0..=1.0)
        } else {
            let zero_cutoff = logarithmic_zero_cutoff(min, max);
            if value < 0.0 {
                emath::remap(
                    normalized_from_value(value, min..=0.0, spec),
                    0.0..=1.0,
                    0.0..=zero_cutoff,
                )
            } else {
                emath::remap(
                    normalized_from_value(value, 0.0..=max, spec),
                    0.0..=1.0,
                    zero_cutoff..=1.0,
                )
            }
        }
    } else {
        emath::remap_clamp(value, range, 0.0..=1.0)
    }
}

fn range_log10(min: f64, max: f64, spec: &Spec) -> (f64, f64) {
    if min == 0.0 && max == f64::INFINITY {
        (spec.smallest_positive.log10(), INF_RANGE_MAGNITUDE)
    } else if min == 0.0 {
        if spec.smallest_positive < max {
            (spec.smallest_positive.log10(), max.log10())
        } else {
            (max.log10() - INF_RANGE_MAGNITUDE, max.log10())
        }
    } else if max == f64::INFINITY {
        (min.log10(), min.log10() + INF_RANGE_MAGNITUDE)
    } else {
        (min.log10(), max.log10())
    }
}

/// Where zero sits on a logarithmic bar spanning it.
fn logarithmic_zero_cutoff(min: f64, max: f64) -> f64 {
    let min_magnitude = if min == f64::NEG_INFINITY {
        INF_RANGE_MAGNITUDE
    } else {
        min.abs().log10().abs()
    };
    let max_magnitude = if max == f64::INFINITY {
        INF_RANGE_MAGNITUDE
    } else {
        max.log10().abs()
    };
    min_magnitude / (min_magnitude + max_magnitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(logarithmic: bool) -> Spec {
        Spec {
            logarithmic,
            smallest_positive: 1e-6,
        }
    }

    #[test]
    fn the_mapping_round_trips() {
        for (range, log) in [
            (1.0..=3000.0, true),
            (0.0..=100.0, true),
            (-50.0..=50.0, true),
            (0.0..=1.0, false),
            (10.0..=-10.0, false),
        ] {
            let spec = spec(log);
            for i in 1..20 {
                let n = i as f64 / 20.0;
                let v = value_from_normalized(n, range.clone(), &spec);
                let back = normalized_from_value(v, range.clone(), &spec);
                assert!(
                    (back - n).abs() < 1e-9,
                    "{range:?} log {log}: {n} -> {v} -> {back}"
                );
            }
        }
    }

    /// One frame showing a bar on `value`, 300 points wide; returns its rect.
    fn frame(ctx: &egui::Context, value: &mut f32, events: Vec<egui::Event>, t: f64) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        let _ = ctx.run(
            egui::RawInput {
                events,
                time: Some(t),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        rect = ui.add(BarSlider::new(value, 0.0..=1.0)).rect;
                    });
            },
        );
        rect
    }

    fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_drag_moves_the_value_by_how_far_the_pointer_went() {
        let ctx = egui::Context::default();
        let mut value = 0.5_f32;
        let rect = frame(&ctx, &mut value, vec![], 0.0);
        // Pressed near the left end: the value doesn't jump there.
        let start = rect.left_center() + egui::vec2(10.0, 0.0);
        frame(
            &ctx,
            &mut value,
            vec![egui::Event::PointerMoved(start)],
            0.1,
        );
        frame(&ctx, &mut value, vec![button(start, true)], 0.2);
        let quarter = rect.width() * 0.25;
        let mut t = 0.3;
        for k in 1..=10 {
            let at = start + egui::vec2(quarter * k as f32 / 10.0, 0.0);
            frame(&ctx, &mut value, vec![egui::Event::PointerMoved(at)], t);
            t += 0.02;
        }
        frame(
            &ctx,
            &mut value,
            vec![button(start + egui::vec2(quarter, 0.0), false)],
            t,
        );
        assert!(
            (value - 0.75).abs() < 0.02,
            "moved a quarter from 0.5, got {value}"
        );
    }

    #[test]
    fn a_click_types_a_value_in() {
        let ctx = egui::Context::default();
        let mut value = 0.5_f32;
        let rect = frame(&ctx, &mut value, vec![], 0.0);
        let at = rect.center();
        frame(&ctx, &mut value, vec![egui::Event::PointerMoved(at)], 0.1);
        frame(&ctx, &mut value, vec![button(at, true)], 0.2);
        frame(&ctx, &mut value, vec![button(at, false)], 0.25);
        assert_eq!(value, 0.5, "a click doesn't move it");
        // Past the double-click window: editing, the text all selected.
        frame(&ctx, &mut value, vec![], 1.0);
        frame(&ctx, &mut value, vec![], 1.1);
        frame(
            &ctx,
            &mut value,
            vec![egui::Event::Text("0.25".into())],
            1.2,
        );
        frame(
            &ctx,
            &mut value,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            1.3,
        );
        assert!((value - 0.25).abs() < 1e-6, "typed 0.25, got {value}");
    }
}
