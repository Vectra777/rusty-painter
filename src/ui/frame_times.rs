//! The frame-time readout: frames per second and milliseconds per frame in
//! the status bar, and the Frame Times window with where each frame's time
//! goes (see [`crate::app::frame_stats`]).

use crate::PainterApp;
use crate::app::frame_stats::{FrameTimes, Group, Stage};
use crate::ui::style::*;
use eframe::egui::{self, Color32, RichText, Stroke};

/// One colour per group, in [`Group::ALL`] order (checked for colour-blind
/// separation and contrast against the panel background).
const GROUP_COLORS: [Color32; 4] = [
    Color32::from_rgb(0x39, 0x87, 0xe5),
    Color32::from_rgb(0xd9, 0x59, 0x26),
    Color32::from_rgb(0x19, 0x9e, 0x70),
    Color32::from_rgb(0xc9, 0x85, 0x00),
];

fn group_color(group: Group) -> Color32 {
    GROUP_COLORS[Group::ALL.iter().position(|&g| g == group).unwrap_or(0)]
}

/// Refresh rates the worst frame is checked against.
const RATES: [f32; 4] = [60.0, 144.0, 240.0, 260.0];
/// Those whose budget is drawn on the graph (240 and 260 Hz would overlap).
const LINE_RATES: [f32; 3] = [60.0, 144.0, 260.0];

/// `240 fps · 3.1 ms` in the status bar while frame times are on; a click
/// opens the breakdown.
pub fn status_readout(app: &mut PainterApp, ui: &mut egui::Ui) {
    let stats = &mut app.workspace.frame_stats;
    if !stats.enabled {
        return;
    }
    let summary = stats.summary();
    let fps = summary
        .fps
        .map_or_else(|| "– fps".to_string(), |f| format!("{f:.0} fps"));
    let text = format!("{fps} · {:.1} ms", summary.avg.total());
    let button = egui::Button::new(RichText::new(text).small().color(TEXT_DIM)).frame(false);
    if ui
        .add(button)
        .on_hover_text("Frames per second and main-thread time per frame. Click for the breakdown.")
        .clicked()
    {
        stats.window_open = !stats.window_open;
    }
}

pub fn frame_times_window(app: &mut PainterApp, ctx: &egui::Context) {
    let display_hz = app.workspace.refresh.hz();
    let stats = &mut app.workspace.frame_stats;
    if !stats.enabled || !stats.window_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Frame Times")
        .open(&mut open)
        .resizable(false)
        .collapsible(true)
        .default_width(340.0)
        .show(ctx, |ui| {
            let summary = stats.summary();
            let fps = summary
                .fps
                .map_or_else(|| "idle".to_string(), |f| format!("{f:.0} fps"));
            ui.label(
                RichText::new(format!(
                    "{fps}  ·  {:.2} ms average  ·  {:.2} ms worst",
                    summary.avg.total(),
                    summary.max_total
                ))
                .strong()
                .color(TEXT_STRONG),
            );
            let fits: Vec<String> = RATES
                .iter()
                .map(|&hz| {
                    let ok = summary.max_total <= 1000.0 / hz;
                    format!("{hz:.0} Hz {}", if ok { "✔" } else { "✖" })
                })
                .collect();
            ui.label(
                RichText::new(format!("Worst frame fits: {}", fits.join("  ")))
                    .small()
                    .color(TEXT_DIM),
            );
            let display = match display_hz {
                Some(hz) => {
                    let ok = summary.avg.total() <= 1000.0 / hz;
                    format!(
                        "Display: {hz:.0} Hz (measured), {:.1} ms a frame  ·  average {}",
                        1000.0 / hz,
                        if ok { "keeps up ✔" } else { "too slow ✖" }
                    )
                }
                None => "Display: measuring… (draw or pan for a moment)".to_string(),
            };
            ui.label(RichText::new(display).small().color(TEXT_DIM))
                .on_hover_text(
                    "Measured from how frames are paced while they run back to back \
                     (vsync); follows the window to another monitor.",
                );
            ui.add_space(6.0);
            graph(ui, stats.frames().collect::<Vec<_>>().as_slice());
            legend(ui);
            ui.add_space(6.0);
            stage_table(ui, &summary.avg, &summary.max, summary.max_total);
            ui.add_space(6.0);
            ui.checkbox(&mut stats.continuous, "Redraw continuously")
                .on_hover_text(
                    "Draw frames back to back (as fast as vsync allows) to see the steady cost \
                     and the frame rate the screen can reach. Uses more power.",
                );
            ui.label(
                RichText::new(
                    "Main-thread CPU time over about the last second. GPU work and waiting \
                     for vsync are not included; idle gaps are left out.",
                )
                .small()
                .color(TEXT_DIM),
            );
        });
    if stats.continuous {
        ctx.request_repaint();
    }
    if !open {
        stats.window_open = false;
    }
}

/// The recent frames as stacked bars by group, newest on the right, with
/// the budgets of common refresh rates as lines. Hover a bar for its values.
fn graph(ui: &mut egui::Ui, frames: &[&FrameTimes]) {
    let size = egui::vec2(ui.available_width().max(300.0), 90.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, BG_INSET);
    // Room for the tallest frame, and at least the 240 Hz budget.
    let top = frames
        .iter()
        .map(|f| f.total())
        .fold(1000.0 / 240.0, f32::max)
        * 1.15;
    let y = |ms: f32| rect.bottom() - (ms / top).min(1.0) * rect.height();
    let bar = 2.0;
    let count = ((rect.width() / bar) as usize).min(frames.len());
    let shown = &frames[frames.len() - count..];
    let x0 = rect.right() - count as f32 * bar;
    for (i, f) in shown.iter().enumerate() {
        let left = x0 + i as f32 * bar;
        let mut base = 0.0;
        for group in Group::ALL {
            let v = f.group(group);
            if v <= 0.0 {
                continue;
            }
            let r = egui::Rect::from_min_max(
                egui::pos2(left, y(base + v)),
                egui::pos2(left + bar - 0.5, y(base)),
            );
            painter.rect_filled(r, 0.0, group_color(group));
            base += v;
        }
    }
    // Budget lines, labelled.
    let font = egui::TextStyle::Small.resolve(ui.style());
    for hz in LINE_RATES {
        let budget = 1000.0 / hz;
        if budget > top {
            continue;
        }
        let ly = y(budget);
        painter.hline(
            rect.x_range(),
            ly,
            Stroke::new(1.0_f32, BORDER_LIGHT.gamma_multiply(1.4)),
        );
        painter.text(
            egui::pos2(rect.left() + 4.0, ly - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{hz:.0} Hz  {budget:.1} ms"),
            font.clone(),
            TEXT_DIM,
        );
    }
    if let Some(pos) = response.hover_pos() {
        let i = ((pos.x - x0) / bar).floor();
        if i >= 0.0
            && let Some(f) = shown.get(i as usize)
        {
            let mut lines = vec![format!("{:.2} ms", f.total())];
            if let Some(interval) = f.interval {
                lines[0] += &format!("  ·  {:.0} fps", 1000.0 / interval.max(0.01));
            }
            for group in Group::ALL {
                lines.push(format!("{}: {:.2} ms", group.label(), f.group(group)));
            }
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                ui.id().with("frame_tip"),
                |ui| {
                    ui.label(lines.join("\n"));
                },
            );
            painter.vline(
                x0 + i * bar + bar / 2.0,
                rect.y_range(),
                Stroke::new(1.0_f32, TEXT_DIM),
            );
        }
    }
}

fn legend(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        for group in Group::ALL {
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, group_color(group));
            ui.label(RichText::new(group.label()).small().color(TEXT));
            ui.add_space(6.0);
        }
    });
}

/// Per stage, the average and the worst over the recent frames; the total's
/// worst is the slowest whole frame.
fn stage_table(ui: &mut egui::Ui, avg: &FrameTimes, max: &FrameTimes, max_total: f32) {
    egui::Grid::new("frame_stage_table")
        .num_columns(3)
        .spacing(egui::vec2(12.0, 2.0))
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Stage").small().color(TEXT_DIM));
            ui.label(RichText::new("avg ms").small().color(TEXT_DIM));
            ui.label(RichText::new("worst ms").small().color(TEXT_DIM));
            ui.end_row();
            for stage in Stage::ALL {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 2.0, group_color(stage.group()));
                    ui.label(RichText::new(stage.label()).color(TEXT))
                        .on_hover_text(stage.hint());
                });
                ui.label(RichText::new(format!("{:.2}", avg.stage(stage))).monospace());
                ui.label(RichText::new(format!("{:.2}", max.stage(stage))).monospace());
                ui.end_row();
            }
            ui.label(RichText::new("Total").strong().color(TEXT_STRONG));
            ui.label(
                RichText::new(format!("{:.2}", avg.total()))
                    .monospace()
                    .strong(),
            );
            ui.label(RichText::new(format!("{max_total:.2}")).monospace());
            ui.end_row();
        });
}
