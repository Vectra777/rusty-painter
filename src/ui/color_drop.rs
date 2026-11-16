//! ColorDrop: the brush colour's swatch dragged onto the canvas fills
//! there (the Fill tool's bucket, its worker and settings). Held still
//! after the drop, a threshold bar shows: sliding left or right refills
//! with less or more of the colours around; lifting keeps it.

use crate::PainterApp;
use crate::ui::style::*;
use eframe::egui::{self, Color32, Stroke};

/// Held this still (points) this long (seconds) over the canvas, the drop
/// fills and the threshold bar shows.
const HOLD_SLOP: f32 = 8.0;
const HOLD_SECONDS: f64 = 0.4;
/// Points of sliding per step of threshold (0 to 255).
const POINTS_PER_STEP: f32 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drop {
    /// The swatch being dragged; where the pointer is, and where and when
    /// it last settled.
    Dragging {
        at: egui::Pos2,
        still_at: egui::Pos2,
        still_since: f64,
    },
    /// Filled at `canvas` (canvas pixels) and held: sliding sets the
    /// threshold. `mark` is the history before the fill, to take it back
    /// for a refill; `start_x` where the slide is measured from.
    Scrubbing {
        canvas: eframe::egui::Vec2,
        start_x: f32,
        from: u8,
        tolerance: u8,
        filled: u8,
        mark: u64,
    },
}

fn state_id() -> egui::Id {
    egui::Id::new("color_drop")
}

/// The threshold for a slide of `dx` points from where it began at `from`.
pub(crate) fn scrub_tolerance(from: u8, dx: f32) -> u8 {
    (from as f32 + dx / POINTS_PER_STEP)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Make the brush colour's swatch (`response`) draggable onto the canvas.
pub(crate) fn drag_source(ui: &egui::Ui, response: &egui::Response, _color: Color32) {
    let response = response.interact(egui::Sense::click_and_drag());
    if response.drag_started()
        && let Some(at) = response.interact_pointer_pos()
    {
        let now = ui.input(|i| i.time);
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                state_id(),
                Drop::Dragging {
                    at,
                    still_at: at,
                    still_since: now,
                },
            )
        });
    }
}

/// Where `screen` falls on the picture, if it's over the canvas itself (not
/// a panel or window over it).
fn on_canvas(app: &PainterApp, ctx: &egui::Context, screen: egui::Pos2) -> Option<egui::Vec2> {
    let area = app.viewport.canvas_area?;
    let over = area.contains(screen)
        && ctx
            .layer_id_at(screen)
            .is_none_or(|l| l.order == egui::Order::Background);
    if !over {
        return None;
    }
    let (origin, center) = crate::app::view::render::canvas_placement(app, area);
    let (pos, inside) = app.screen_to_canvas(screen, origin, center);
    inside.then_some(pos)
}

/// Once a frame: follow a ColorDrop in progress.
pub(crate) fn tick(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(state) = ctx.data(|d| d.get_temp::<Drop>(state_id())) else {
        return;
    };
    let (down, pos, now) =
        ctx.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos(), i.time));
    let color = app.brush_state.brush.brush_options.color;
    let next = match state {
        Drop::Dragging {
            at,
            still_at,
            still_since,
        } => {
            let at = pos.unwrap_or(at);
            if !down {
                // Let go: over the canvas, it fills there.
                if let Some(c) = on_canvas(app, ctx, at) {
                    app.color_drop_fill(c, app.workspace.fill.settings.tolerance);
                }
                None
            } else {
                ghost(ctx, at, color);
                let (still_at, still_since) = if at.distance(still_at) > HOLD_SLOP {
                    (at, now)
                } else {
                    (still_at, still_since)
                };
                match on_canvas(app, ctx, at) {
                    Some(c) if now - still_since >= HOLD_SECONDS => {
                        let from = app.workspace.fill.settings.tolerance;
                        let mark = app.layer_state.history.push_count();
                        app.color_drop_fill(c, from);
                        Some(Drop::Scrubbing {
                            canvas: c,
                            start_x: at.x,
                            from,
                            tolerance: from,
                            filled: from,
                            mark,
                        })
                    }
                    over => {
                        if over.is_some() {
                            let wait = HOLD_SECONDS - (now - still_since);
                            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                                wait.max(0.0),
                            ));
                        }
                        Some(Drop::Dragging {
                            at,
                            still_at,
                            still_since,
                        })
                    }
                }
            }
        }
        Drop::Scrubbing {
            canvas,
            start_x,
            from,
            filled,
            mark,
            ..
        } => {
            if down {
                let x = pos.map_or(start_x, |p| p.x);
                let tolerance = scrub_tolerance(from, x - start_x);
                let mut filled = filled;
                // Refilled once the last fill has landed (it paints on the
                // worker): that one taken back first.
                if tolerance != filled && app.layer_state.history.push_count() > mark {
                    app.apply_history_now(false);
                    app.layer_state.history.discard_redo();
                    app.color_drop_fill(canvas, tolerance);
                    filled = tolerance;
                }
                threshold_bar(app, ctx, tolerance);
                ctx.request_repaint();
                Some(Drop::Scrubbing {
                    canvas,
                    start_x,
                    from,
                    tolerance,
                    filled,
                    mark,
                })
            } else {
                // Lifted: the fill stays, and so does the threshold for next time.
                app.workspace.fill.settings.tolerance = filled;
                None
            }
        }
    };
    ctx.data_mut(|d| match next {
        Some(s) => d.insert_temp(state_id(), s),
        None => d.remove::<Drop>(state_id()),
    });
}

/// The colour being dragged, under the pointer.
fn ghost(ctx: &egui::Context, at: egui::Pos2, color: Color32) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("color_drop_ghost"),
    ));
    let r = 18.0;
    painter.circle_filled(at, r + 2.0, Color32::from_black_alpha(140));
    painter.circle_filled(at, r, Color32::from_rgb(color.r(), color.g(), color.b()));
    painter.circle_stroke(at, r + 1.0, Stroke::new(1.0_f32, TEXT_STRONG));
    ctx.request_repaint();
}

/// While held after a drop: the threshold, across the top of the canvas.
fn threshold_bar(app: &PainterApp, ctx: &egui::Context, tolerance: u8) {
    let Some(area) = app.viewport.canvas_area else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("color_drop_threshold"),
    ));
    let width = (area.width() - 40.0).clamp(120.0, 360.0);
    let bar = egui::Rect::from_center_size(
        egui::pos2(area.center().x, area.top() + 36.0),
        egui::vec2(width, 28.0),
    );
    painter.rect_filled(bar.expand(4.0), RADIUS_CARD, BG_PANEL);
    painter.rect_filled(bar, RADIUS_WIDGET, BG_INSET);
    let filled = egui::Rect::from_min_size(
        bar.min,
        egui::vec2(bar.width() * tolerance as f32 / 255.0, bar.height()),
    );
    painter.rect_filled(filled, RADIUS_WIDGET, accent_dim());
    painter.text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        format!(
            "Threshold {:.0}%  ·  slide left or right, lift to keep",
            tolerance as f32 / 2.55
        ),
        egui::FontId::proportional(12.0),
        TEXT_STRONG,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    #[test]
    fn sliding_right_takes_in_more_left_less() {
        assert_eq!(scrub_tolerance(32, 0.0), 32);
        assert_eq!(scrub_tolerance(32, 3.0 * POINTS_PER_STEP), 35);
        assert_eq!(scrub_tolerance(32, -1000.0), 0);
        assert_eq!(scrub_tolerance(32, 1000.0), 255);
    }

    /// The canvas filling a 400×300 panel at 0,0; frames at `t` seconds
    /// with the pointer at `at` (`down`: pressed).
    struct Harness {
        app: PainterApp,
        ctx: egui::Context,
    }

    impl Harness {
        fn new() -> Self {
            let mut app =
                crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
            app.canvas_mut().active_layer_idx = 1;
            app.brush_state.brush.brush_options.color = Color32::RED;
            app.viewport.canvas_area = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            ));
            Self {
                app,
                ctx: egui::Context::default(),
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>, t: f64) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                time: Some(t),
                events,
                ..Default::default()
            };
            let app = &mut self.app;
            let _ = self.ctx.run(input, |ctx| tick(app, ctx));
        }

        /// The screen point over canvas pixel (32, 32).
        fn middle(&self) -> egui::Pos2 {
            let area = self.app.viewport.canvas_area.unwrap();
            let (origin, _) = crate::app::view::render::canvas_placement(&self.app, area);
            origin + egui::vec2(32.0, 32.0) * self.app.viewport.zoom
        }

        fn painted(&self) -> bool {
            let tile = self.app.canvas.get_layer_tile_data(1, 0, 0);
            tile.is_some_and(|t| t[32 * 64 + 32].r() > 200 && t[32 * 64 + 32].a() > 200)
        }

        fn start(&mut self, at: egui::Pos2, t: f64) {
            self.frame(vec![egui::Event::PointerMoved(at)], t);
            self.frame(
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                }],
                t,
            );
            self.ctx.data_mut(|d| {
                d.insert_temp(
                    state_id(),
                    Drop::Dragging {
                        at,
                        still_at: at,
                        still_since: t,
                    },
                )
            });
        }

        fn lift(&mut self, at: egui::Pos2, t: f64) {
            self.frame(
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
                t,
            );
        }
    }

    #[test]
    fn dropping_the_colour_on_the_canvas_fills_there() {
        let mut h = Harness::new();
        let at = h.middle();
        h.start(at - egui::vec2(20.0, 0.0), 0.0);
        h.frame(vec![egui::Event::PointerMoved(at)], 0.1);
        assert!(!h.painted());
        h.lift(at, 0.2);
        h.app.finish_stroke();
        assert!(h.painted(), "filled where it was dropped");
        assert_eq!(
            h.app.layer_state.history.stacks().0.len(),
            1,
            "one undo step"
        );
    }

    #[test]
    fn held_after_the_drop_a_slide_refills_and_lifting_keeps_the_threshold() {
        let mut h = Harness::new();
        let from = h.app.workspace.fill.settings.tolerance;
        let at = h.middle();
        h.start(at, 0.0);
        h.frame(vec![], HOLD_SECONDS + 0.05);
        assert!(h.painted(), "filled on the hold");
        // Slide right: a wider threshold, refilled as one step.
        let right = at + egui::vec2(30.0, 0.0);
        h.frame(vec![egui::Event::PointerMoved(right)], 0.6);
        h.frame(vec![], 0.7);
        h.lift(right, 0.8);
        assert_eq!(
            h.app.workspace.fill.settings.tolerance,
            scrub_tolerance(from, 30.0)
        );
        assert!(h.painted());
        assert_eq!(
            h.app.layer_state.history.stacks().0.len(),
            1,
            "still one undo step"
        );
        assert_eq!(
            h.app.layer_state.history.stacks().1.len(),
            0,
            "no redo left over"
        );
    }
}
