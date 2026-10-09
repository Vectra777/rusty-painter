//! The pop-up palette (`K`, a right click on the canvas, or the touch
//! button): the favourite brushes, or the recent ones, in a ring around the
//! pointer. Pointing toward one and clicking (or tapping it, or letting go
//! of `K`) picks it; Esc or the middle closes it.

use crate::PainterApp;
use crate::app::brush_library::{RadialHit, radial_hit, slice_direction};
use crate::ui::style::*;
use eframe::egui::{self, Color32, Key, Stroke};

/// Radii of the middle (closes it) and of the ring.
const INNER: f32 = 26.0;
const OUTER: f32 = 124.0;
const TOUCH_INNER: f32 = 36.0;
const TOUCH_OUTER: f32 = 164.0;
/// Room for the names outside the ring, kept on screen.
const LABEL_ROOM: f32 = 40.0;
/// Segments per slice for its highlight.
const ARC_STEPS: usize = 12;

fn radii(ctx: &egui::Context) -> (f32, f32) {
    if metrics(ctx).touch {
        (TOUCH_INNER, TOUCH_OUTER)
    } else {
        (INNER, OUTER)
    }
}

/// Keys while the palette is open: Esc or its key (`K`) again closes it;
/// letting go of the key that opened it over a slice picks that brush.
/// Returns whether to repaint.
pub(crate) fn palette_keys(app: &mut PainterApp, ctx: &egui::Context) -> bool {
    let (inner, _) = radii(ctx);
    let binding = app
        .workspace
        .keymap
        .bindings(crate::app::input::keymap::Action::Palette)
        .first()
        .copied();
    let (escape, again, released, pointer) = ctx.input_mut(|i| {
        let (again, released) = match binding {
            Some(b) => {
                let again = i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key {
                            key,
                            pressed: true,
                            repeat: false,
                            ..
                        } if *key == b.key
                    )
                });
                // Held keys repeat: those presses are the palette's too.
                i.consume_key(b.modifiers(), b.key);
                (again, i.key_released(b.key))
            }
            None => (false, false),
        };
        (
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
            again,
            released,
            i.pointer.latest_pos(),
        )
    });
    let library = &mut app.brush_state.library;
    let Some(radial) = &mut library.radial else {
        return false;
    };
    if escape || again {
        library.radial = None;
        return true;
    }
    if released && radial.key_held {
        radial.key_held = false;
        // Let go over the middle: it stays open for a click.
        if let Some(pos) = pointer
            && let RadialHit::Slice(n) = radial_hit(radial.centre, pos, radial.names.len(), inner)
        {
            app.pick_radial_slice(n);
        }
        return true;
    }
    false
}

pub fn radial_palette(app: &mut PainterApp, ctx: &egui::Context) {
    let (inner, outer) = radii(ctx);
    let screen = ctx.screen_rect();
    let Some(radial) = &mut app.brush_state.library.radial else {
        return;
    };
    // The whole ring and its names on screen.
    let room = outer + LABEL_ROOM;
    let clamp = |v: f32, lo: f32, hi: f32, mid: f32| if lo < hi { v.clamp(lo, hi) } else { mid };
    radial.centre = egui::pos2(
        clamp(
            radial.centre.x,
            screen.left() + room,
            screen.right() - room,
            screen.center().x,
        ),
        clamp(
            radial.centre.y,
            screen.top() + room,
            screen.bottom() - room,
            screen.center().y,
        ),
    );
    // Only a press made on it counts: not the release of the click (or
    // tap) that opened it.
    let pressed = ctx.input(|i| i.pointer.any_pressed());
    let fresh = std::mem::take(&mut radial.fresh);
    radial.armed |= pressed && !fresh;
    let armed = radial.armed;
    let centre = radial.centre;
    let names = radial.names.clone();
    let count = names.len();

    let mut picked = None;
    let mut close = false;
    egui::Area::new(egui::Id::new("radial_brush_palette"))
        .fixed_pos(screen.min)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // The whole screen: a click anywhere is the palette's.
            let (_, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            let pointer = ui.input(|i| i.pointer.latest_pos());
            let hit = pointer.map(|p| radial_hit(centre, p, count, inner));
            if armed && response.clicked() {
                match hit {
                    Some(RadialHit::Slice(n)) => picked = Some(n),
                    _ => close = true,
                }
            }
            if armed && response.secondary_clicked() {
                close = true;
            }

            let painter = ui.painter();
            painter.circle_filled(centre, outer, BG_PANEL.gamma_multiply(0.94));
            if let Some(RadialHit::Slice(n)) = hit {
                slice_highlight(painter, centre, inner, outer, n, count);
            }
            painter.circle_stroke(centre, outer, Stroke::new(1.0_f32, BORDER_LIGHT));
            if count > 1 {
                for i in 0..count {
                    // Between slice i - 1 and slice i.
                    let a = slice_angle(i, count) - std::f32::consts::PI / count as f32;
                    let d = egui::vec2(a.cos(), a.sin());
                    painter.line_segment(
                        [centre + d * inner, centre + d * outer],
                        Stroke::new(1.0_f32, BORDER),
                    );
                }
            }
            let over_centre = hit == Some(RadialHit::Centre);
            painter.circle_filled(
                centre,
                inner,
                if over_centre { BG_RAISED } else { BG_INSET },
            );
            painter.circle_stroke(centre, inner, Stroke::new(1.0_f32, BORDER_LIGHT));
            // A cross: the middle closes it.
            let x = inner * 0.3;
            let cross = Stroke::new(1.5_f32, if over_centre { TEXT_STRONG } else { TEXT_DIM });
            painter.line_segment(
                [centre + egui::vec2(-x, -x), centre + egui::vec2(x, x)],
                cross,
            );
            painter.line_segment(
                [centre + egui::vec2(-x, x), centre + egui::vec2(x, -x)],
                cross,
            );

            let mid = (inner + outer) * 0.5;
            let chord = std::f32::consts::TAU * mid / count.max(1) as f32;
            let side = ((outer - inner) * 0.6).min(chord * 0.75).max(16.0);
            let pool = app.workspace.pool.clone();
            crate::ui::brush_list::collect_preset_previews(app, ctx);
            let bs = &mut app.brush_state;
            for (i, name) in names.iter().enumerate() {
                let Some(index) = bs.presets.iter().position(|p| &p.name == name) else {
                    continue;
                };
                let dir = slice_direction(i, count);
                let hovered = hit == Some(RadialHit::Slice(i));
                // The middle of the stroke preview, square.
                let texture = crate::ui::brush_list::preset_preview(bs, index, &pool, ctx);
                let thumb =
                    egui::Rect::from_center_size(centre + dir * mid, egui::vec2(side, side));
                painter.rect_filled(thumb, RADIUS_CARD, BG_INSET);
                let uv = egui::Rect::from_min_max(egui::pos2(0.4, 0.0), egui::pos2(0.6, 1.0));
                if let Some(texture) = texture {
                    painter.image(texture, thumb, uv, Color32::WHITE);
                }
                let active = bs.active_preset.as_deref() == Some(name.as_str());
                let outline = if active {
                    Stroke::new(2.0_f32, accent())
                } else {
                    Stroke::new(1.0_f32, if hovered { TEXT_DIM } else { BORDER })
                };
                painter.rect_stroke(thumb, RADIUS_CARD, outline);
                slice_label(ui, centre + dir * (outer + 6.0), dir, name, hovered);
            }
        });
    if let Some(n) = picked {
        app.pick_radial_slice(n);
    } else if close {
        app.brush_state.library.radial = None;
    }
}

/// The angle (radians, y down) of the middle of slice `n` of `count`.
fn slice_angle(n: usize, count: usize) -> f32 {
    let d = slice_direction(n, count);
    d.y.atan2(d.x)
}

/// Fill slice `n`'s part of the ring, as convex pieces.
fn slice_highlight(
    painter: &egui::Painter,
    centre: egui::Pos2,
    inner: f32,
    outer: f32,
    n: usize,
    count: usize,
) {
    let half = std::f32::consts::PI / count as f32;
    let middle = slice_angle(n, count);
    let at = |a: f32, r: f32| centre + egui::vec2(a.cos(), a.sin()) * r;
    for k in 0..ARC_STEPS {
        let a0 = middle - half + 2.0 * half * k as f32 / ARC_STEPS as f32;
        let a1 = middle - half + 2.0 * half * (k + 1) as f32 / ARC_STEPS as f32;
        painter.add(egui::Shape::convex_polygon(
            vec![at(a0, inner), at(a0, outer), at(a1, outer), at(a1, inner)],
            accent_dim(),
            Stroke::NONE,
        ));
    }
}

/// A preset's name just outside the ring, reading outward from it.
fn slice_label(ui: &egui::Ui, pos: egui::Pos2, dir: egui::Vec2, name: &str, hovered: bool) {
    let side = |v: f32| {
        if v > 0.3 {
            egui::Align::Min
        } else if v < -0.3 {
            egui::Align::Max
        } else {
            egui::Align::Center
        }
    };
    let align = egui::Align2([side(dir.x), side(dir.y)]);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let colour = if hovered { TEXT_STRONG } else { TEXT };
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(name.to_string(), font, colour);
    let chip = align.anchor_size(pos, galley.size() + egui::vec2(8.0, 4.0));
    let fill = if hovered {
        accent_dim()
    } else {
        Color32::from_black_alpha(170)
    };
    painter.rect_filled(chip, RADIUS_SMALL, fill);
    painter.galley(chip.min + egui::vec2(4.0, 2.0), galley, colour);
}

#[cfg(test)]
mod tests {
    use crate::app::brush_library::slice_direction;
    use crate::canvas::Canvas;
    use eframe::egui::{self, Color32};

    /// One frame of the app's order: shortcuts, the canvas and its input,
    /// then the windows (the presets window and the palette).
    fn frame(app: &mut crate::PainterApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            crate::app::input::shortcuts::handle_shortcuts(app, ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                let response =
                    ui.allocate_response(ui.available_size(), egui::Sense::click_and_drag());
                let rect = response.rect;
                crate::app::input::handle_input(app, ctx, &response, rect.min, rect.center(), &[]);
            });
            crate::ui::brush_list::presets_window(app, ctx);
            super::radial_palette(app, ctx);
        });
    }

    fn button(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn key(key: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn app() -> crate::PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(400, 300, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.presets = crate::PainterApp::default_brush_presets();
        app.brush_state.show_presets = true;
        app.edit_library(|lib| {
            lib.toggle_favourite("Chalk");
            lib.toggle_favourite("Ink Pen");
            lib.toggle_favourite("Glow");
        });
        app
    }

    #[test]
    fn a_right_click_opens_the_palette_and_a_click_toward_a_slice_picks_it() {
        use egui::PointerButton::{Primary, Secondary};
        let (mut app, ctx) = (app(), egui::Context::default());
        let at = egui::pos2(700.0, 300.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
        frame(&mut app, &ctx, vec![button(at, Secondary, true)]);
        frame(&mut app, &ctx, vec![button(at, Secondary, false)]);
        let radial = app.brush_state.library.radial.as_ref().expect("open");
        assert_eq!(radial.names, ["Chalk", "Ink Pen", "Glow"]);
        let centre = radial.centre;
        // Toward the second slice, past the ring.
        let toward = centre + slice_direction(1, 3) * 150.0;
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(toward)]);
        frame(&mut app, &ctx, vec![button(toward, Primary, true)]);
        assert!(!app.brush_state.is_drawing, "the click isn't a stroke");
        frame(&mut app, &ctx, vec![button(toward, Primary, false)]);
        assert!(app.brush_state.library.radial.is_none());
        assert_eq!(app.brush_state.active_preset.as_deref(), Some("Ink Pen"));
        assert_eq!(app.brush_state.library.file.recent, ["Ink Pen"]);
        app.release_canvas();
        assert_eq!(
            app.layer_state.history.stacks().0.len(),
            0,
            "nothing painted"
        );

        // A right drag still pans, and opens nothing.
        let offset = app.viewport.offset;
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
        frame(&mut app, &ctx, vec![button(at, Secondary, true)]);
        let away = at + egui::vec2(60.0, 0.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(away)]);
        frame(&mut app, &ctx, vec![button(away, Secondary, false)]);
        assert!(app.brush_state.library.radial.is_none());
        assert_ne!(app.viewport.offset, offset, "panned");
    }

    #[test]
    fn the_key_opens_it_and_letting_go_over_a_slice_picks_it() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let at = egui::pos2(400.0, 300.0);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
        frame(&mut app, &ctx, vec![key(egui::Key::K, true, false)]);
        assert!(app.brush_state.library.radial.is_some());
        let centre = app.brush_state.library.radial.as_ref().unwrap().centre;
        let toward = centre + slice_direction(2, 3) * 80.0;
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(toward),
                key(egui::Key::K, true, true),
            ],
        );
        assert!(app.brush_state.library.radial.is_some(), "held, not closed");
        frame(&mut app, &ctx, vec![key(egui::Key::K, false, false)]);
        assert!(app.brush_state.library.radial.is_none());
        assert_eq!(app.brush_state.active_preset.as_deref(), Some("Glow"));

        // A tap leaves it open; Esc closes it and deselects nothing else.
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
        frame(&mut app, &ctx, vec![key(egui::Key::K, true, false)]);
        frame(&mut app, &ctx, vec![key(egui::Key::K, false, false)]);
        assert!(app.brush_state.library.radial.is_some(), "stays open");
        frame(&mut app, &ctx, vec![key(egui::Key::Escape, true, false)]);
        assert!(app.brush_state.library.radial.is_none());
        assert_eq!(app.brush_state.active_preset.as_deref(), Some("Glow"));
    }

    #[test]
    fn a_click_on_the_middle_closes_it() {
        let (mut app, ctx) = (app(), egui::Context::default());
        app.open_radial_palette(egui::pos2(400.0, 300.0), false);
        frame(&mut app, &ctx, vec![]);
        let centre = app.brush_state.library.radial.as_ref().unwrap().centre;
        let primary = egui::PointerButton::Primary;
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(centre)]);
        frame(&mut app, &ctx, vec![button(centre, primary, true)]);
        frame(&mut app, &ctx, vec![button(centre, primary, false)]);
        assert!(app.brush_state.library.radial.is_none());
        assert_eq!(app.brush_state.active_preset, None);
    }
}
