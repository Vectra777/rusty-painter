//! The timeline panel (under the canvas): the frame showing, playback, the
//! frame rate and range, onion skins, and a row of frames for each animated
//! layer, where its drawings are added, moved (dragged) and taken away.

use crate::PainterApp;
use crate::canvas::storage::Anim;
use crate::ui::style::*;
use eframe::egui::{self, Color32, Sense, Stroke};

/// A frame cell's width and a row's height.
const CELL: f32 = 14.0;
const ROW: f32 = 20.0;
/// Room for the layer names.
const NAMES: f32 = 110.0;

pub fn timeline_panel(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.workspace.animation.show_timeline {
        return;
    }
    egui::TopBottomPanel::bottom("timeline")
        .resizable(true)
        .default_height(110.0)
        .show(ctx, |ui| {
            controls(app, ui);
            ui.separator();
            egui::ScrollArea::both()
                .id_salt("timeline_rows")
                .show(ui, |ui| rows(app, ui));
        });
}

fn controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let timeline = app.canvas.timeline;
    let time = app.canvas.time;
    ui.horizontal_wrapped(|ui| {
        if ui.button("⏮").on_hover_text("First frame").clicked() {
            app.go_to_frame(timeline.start);
        }
        if ui.button("◀").on_hover_text("Previous frame (,)").clicked() {
            app.go_to_frame(timeline.previous(time));
        }
        let playing = app.workspace.animation.playing;
        if ui
            .button(if playing { "⏸" } else { "▶" })
            .on_hover_text("Play / pause")
            .clicked()
        {
            app.workspace.animation.playing = !playing;
        }
        if ui.button("▶").on_hover_text("Next frame (.)").clicked() {
            app.go_to_frame(timeline.next(time));
        }
        if ui.button("⏭").on_hover_text("Last frame").clicked() {
            app.go_to_frame(timeline.end);
        }
        let mut frame = time;
        if ui
            .add(egui::DragValue::new(&mut frame).prefix("Frame "))
            .changed()
        {
            app.go_to_frame(frame);
        }
        ui.separator();
        let mut t = timeline;
        let mut changed = false;
        changed |= ui
            .add(
                egui::DragValue::new(&mut t.fps)
                    .range(1..=120)
                    .suffix(" fps"),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut t.start)
                    .range(0..=t.end)
                    .prefix("from "),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut t.end)
                    .range(t.start..=9999)
                    .prefix("to "),
            )
            .changed();
        if changed {
            app.canvas_mut().timeline = t;
            app.mark_unsaved();
        }
        ui.separator();
        let mut onion = app.canvas.onion;
        let mut onion_changed = ui.checkbox(&mut onion.enabled, "Onion skin").changed();
        if onion.enabled {
            onion_changed |= ui
                .add(
                    egui::DragValue::new(&mut onion.before)
                        .range(0..=5)
                        .prefix("before "),
                )
                .changed();
            onion_changed |= ui
                .add(
                    egui::DragValue::new(&mut onion.after)
                        .range(0..=5)
                        .prefix("after "),
                )
                .changed();
            onion_changed |= ui
                .add(egui::Slider::new(&mut onion.opacity, 0.05..=1.0).text("opacity"))
                .changed();
        }
        if onion_changed {
            app.canvas_mut().onion = onion;
            app.mark_all_tiles_dirty();
        }
        ui.separator();
        let track = app.active_track();
        let paint_layer = app
            .canvas
            .layers
            .get(app.canvas.active_layer_idx)
            .is_some_and(|l| l.anim.is_none() && app.canvas.active_layer_idx != 0);
        if track.is_none()
            && ui
                .add_enabled(paint_layer, egui::Button::new("Animate layer"))
                .on_hover_text(
                    "Make the selected layer animated: its picture is the first drawing.",
                )
                .clicked()
        {
            app.animate_active_layer();
        }
        if track.is_some() {
            let starts_here =
                track.is_some_and(|t| app.canvas.frames_of(t).iter().any(|(at, _)| *at == time));
            if ui
                .add_enabled(!starts_here, egui::Button::new("New drawing"))
                .clicked()
            {
                app.add_drawing(false);
            }
            if ui
                .add_enabled(!starts_here, egui::Button::new("Copy drawing"))
                .on_hover_text("A new drawing here, starting as a copy of the one showing.")
                .clicked()
            {
                app.add_drawing(true);
            }
            if ui
                .add_enabled(starts_here, egui::Button::new("Remove drawing"))
                .clicked()
            {
                app.remove_drawing();
            }
        }
        ui.separator();
        if ui.button("Export…").clicked() {
            crate::ui::timeline::pick_export(app);
        }
    });
}

/// Each animated layer's frames: a key where a drawing starts, a bar while
/// it's held; the frame showing marked. Click a frame to go there (and
/// select that layer); drag a key to move its drawing.
fn rows(app: &mut PainterApp, ui: &mut egui::Ui) {
    let timeline = app.canvas.timeline;
    let n = timeline.len().min(2000) as usize;
    let tracks = app.canvas.tracks();
    if tracks.is_empty() {
        ui.label(
            egui::RichText::new("No animated layer: select a layer and press Animate layer.")
                .color(TEXT_DIM),
        );
        return;
    }
    let width = NAMES + n as f32 * CELL;
    let drag_id = ui.id().with("timeline_drag");
    let mut drag: Option<(usize, u32)> = ui.data(|d| d.get_temp(drag_id));
    for track in tracks.into_iter().rev() {
        let (id, name) = (
            app.canvas.layers[track].id,
            app.canvas.layers[track].name.clone(),
        );
        let frames = app.canvas.frames_of(id);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, ROW), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let selected = app.active_track() == Some(id);
        painter.text(
            rect.left_center() + egui::vec2(4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &name,
            egui::FontId::proportional(12.0),
            if selected { TEXT_STRONG } else { TEXT },
        );
        let cell = |k: usize| {
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + NAMES + k as f32 * CELL, rect.top() + 2.0),
                egui::vec2(CELL - 1.0, ROW - 4.0),
            )
        };
        for k in 0..n {
            let t = timeline.start + k as u32;
            let held = frames.iter().any(|(at, _)| *at <= t);
            let key = frames.iter().any(|(at, _)| *at == t);
            let fill = if key {
                ACCENT
            } else if held {
                ACCENT.gamma_multiply(0.3)
            } else {
                Color32::from_gray(40)
            };
            painter.rect_filled(cell(k), 2.0, fill);
            if t == app.canvas.time {
                painter.rect_stroke(cell(k).expand(1.0), 2.0, Stroke::new(1.5_f32, TEXT_STRONG));
            }
        }
        let frame_under = |pos: egui::Pos2| {
            let k = ((pos.x - rect.left() - NAMES) / CELL).floor();
            (k >= 0.0 && (k as usize) < n).then(|| timeline.start + k as u32)
        };
        if response.drag_started()
            && let Some(t) = response.interact_pointer_pos().and_then(frame_under)
            && frames.iter().any(|(at, _)| *at == t)
        {
            drag = Some((track, t));
        }
        if response.drag_stopped()
            && let Some((dragged, from)) = drag.take()
            && dragged == track
            && let Some(to) = response.interact_pointer_pos().and_then(frame_under)
            && to != from
        {
            select(app, id, from);
            app.move_drawing(to);
        } else if response.clicked()
            && let Some(t) = response.interact_pointer_pos().and_then(frame_under)
        {
            select(app, id, t);
        }
    }
    ui.data_mut(|d| match drag {
        Some(v) => d.insert_temp(drag_id, v),
        None => d.remove::<(usize, u32)>(drag_id),
    });
}

/// Go to frame `t` on animated layer `track`: its drawing there selected.
fn select(app: &mut PainterApp, track: crate::canvas::storage::LayerId, t: u32) {
    app.go_to_frame(t);
    if let Some(i) = app.canvas.frame_at(track, t) {
        app.canvas_mut().active_layer_idx = i;
    } else if let Some(i) = app.canvas.layer_index_of(track) {
        app.canvas_mut().active_layer_idx = i;
    }
    debug_assert!(
        app.canvas
            .layers
            .iter()
            .all(|l| l.anim != Some(Anim::Frame(u32::MAX)))
    );
}

/// Ask where to export the animation (its format from the name).
pub fn pick_export(app: &mut PainterApp) {
    #[cfg(not(target_os = "android"))]
    {
        use crate::project::video::VideoFormat;
        let dialog = crate::app::settings::file_dialog()
            .set_file_name("animation.gif")
            .add_filter("GIF", &["gif"])
            .add_filter("Animated PNG", &["png", "apng"])
            .add_filter("MP4 (needs ffmpeg)", &["mp4"])
            .add_filter("WebM (needs ffmpeg)", &["webm"]);
        app.file_dialog_job(dialog, crate::app::jobs::Pick::Save, |app, paths| {
            if let Some(path) = paths.into_iter().next() {
                let format = VideoFormat::from_path(&path).unwrap_or(VideoFormat::Gif);
                app.export_animation(path, format);
            }
        });
    }
    #[cfg(target_os = "android")]
    {
        // No file dialog: a GIF in the cache, published to Pictures.
        let path = std::env::temp_dir().join("animation.gif");
        app.export_animation(path, crate::project::video::VideoFormat::Gif);
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use eframe::egui::{self, Color32};

    /// The panel drawn headlessly with an animated layer: frames show, and
    /// a click on a frame cell goes there.
    #[test]
    fn the_panel_shows_frames_and_a_click_goes_to_one() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.animate_active_layer();
        assert!(app.workspace.animation.show_timeline);
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 400.0));
        let frame = |events: Vec<egui::Event>, app: &mut crate::PainterApp| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| super::timeline_panel(app, ctx));
        };
        frame(Vec::new(), &mut app);
        frame(Vec::new(), &mut app);
        // The rows sit at the bottom; frame 5's cell is NAMES + 5.5 cells in.
        let rows_top = ctx.used_rect().bottom() - super::ROW - 8.0;
        let at = egui::pos2(super::NAMES + 5.5 * super::CELL + 8.0, rows_top + super::ROW / 2.0);
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![egui::Event::PointerMoved(at)], &mut app);
        frame(vec![press(true)], &mut app);
        frame(vec![press(false)], &mut app);
        assert_eq!(app.canvas.time, 5, "clicked frame 5");
    }
}
