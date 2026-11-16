//! The QuickMenu: six actions in a ring round where it opened (three
//! fingers held still, or its key), each one of the keymap's actions,
//! chosen by a right-click or long press on its slot or in Settings. And
//! touch mode's on-screen modifier keys: Shift, Alt and Ctrl for hands
//! without a keyboard, fed to the tools as if held on one.

use crate::PainterApp;
use crate::app::input::keymap::{ACTIONS, Action, ActionInfo};
use crate::ui::style::*;
use crate::ui::widgets::context_menu_or_hold;
use eframe::egui::{self, Stroke};

fn open_id() -> egui::Id {
    egui::Id::new("quick_menu_at")
}

/// Open the QuickMenu round `at` (screen).
pub(crate) fn open(ctx: &egui::Context, at: egui::Pos2) {
    ctx.data_mut(|d| d.insert_temp(open_id(), at));
}

/// Open it round `at`, or close it if it's open.
pub(crate) fn toggle(ctx: &egui::Context, at: egui::Pos2) {
    if is_open(ctx) {
        close(ctx);
    } else {
        open(ctx, at);
    }
}

fn close(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<egui::Pos2>(open_id()));
}

pub(crate) fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<egui::Pos2>(open_id())).is_some()
}

/// What a slot can run: every keymap action but the QuickMenu itself.
pub(crate) fn choices() -> impl Iterator<Item = &'static ActionInfo> {
    ACTIONS.iter().filter(|i| i.action != Action::QuickMenu)
}

/// The action kept under `id` in the settings.
pub(crate) fn action_for(id: &str) -> Option<Action> {
    choices().find(|i| i.id == id).map(|i| i.action)
}

/// An action's name, short enough for a slot: its label without the
/// part in brackets.
pub(crate) fn short_label(info: &ActionInfo) -> &'static str {
    info.label.split(" (").next().unwrap_or(info.label)
}

/// The menu of actions to put in a slot; true once one was chosen.
pub(crate) fn slot_choices(ui: &mut egui::Ui, slot: &mut Option<String>) -> bool {
    let mut chosen = false;
    if ui.button("Empty").clicked() {
        *slot = None;
        chosen = true;
    }
    let mut group = "";
    for info in choices() {
        if info.group != group {
            group = info.group;
            ui.separator();
            ui.label(egui::RichText::new(group).small().color(TEXT_DIM));
        }
        let on = slot.as_deref() == Some(info.id);
        if ui.selectable_label(on, short_label(info)).clicked() {
            *slot = Some(info.id.to_string());
            chosen = true;
        }
    }
    chosen
}

/// Where each of the six slots sits round the centre, clockwise from the
/// top, `radius` out.
pub(crate) fn slot_centers(center: egui::Pos2, radius: f32) -> [egui::Pos2; 6] {
    std::array::from_fn(|i| {
        let a = (i as f32 * 60.0 - 90.0).to_radians();
        center + radius * egui::vec2(a.cos(), a.sin())
    })
}

/// The QuickMenu, while open: a tap on a slot runs its action (as its
/// shortcut would) and closes it; a tap elsewhere or Esc closes it.
pub(crate) fn show(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(at) = ctx.data(|d| d.get_temp::<egui::Pos2>(open_id())) else {
        return;
    };
    let touch = metrics(ctx).touch;
    let button = if touch { 32.0 } else { 26.0 };
    let radius = button * 2.4;
    let reach = radius + button + 4.0;
    // Kept whole on screen.
    let screen = ctx.screen_rect();
    let center = at.clamp(
        screen.min + egui::Vec2::splat(reach),
        (screen.max - egui::Vec2::splat(reach)).max(screen.min + egui::Vec2::splat(reach)),
    );
    let mut run = None;
    let area = egui::Area::new(egui::Id::new("quick_menu"))
        .fixed_pos(center - egui::Vec2::splat(reach))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::Vec2::splat(2.0 * reach), egui::Sense::hover());
            let painter = ui.painter();
            painter.circle_filled(rect.center(), radius, BG_PANEL.gamma_multiply(0.6));
            for (i, c) in slot_centers(rect.center(), radius).into_iter().enumerate() {
                let hit = egui::Rect::from_center_size(c, egui::Vec2::splat(2.0 * button));
                let response = ui.interact(hit, ui.id().with(("slot", i)), egui::Sense::click());
                let info = app.workspace.ui.quick_menu[i]
                    .as_deref()
                    .and_then(action_for)
                    .map(Action::info);
                let hot = response.hovered() || response.is_pointer_button_down_on();
                ui.painter()
                    .circle_filled(c, button, if hot { BG_RAISED } else { BG_PANEL });
                ui.painter()
                    .circle_stroke(c, button, Stroke::new(1.0_f32, BORDER_LIGHT));
                let (text, color) = match info {
                    Some(info) => (short_label(info), TEXT),
                    None => ("Empty", TEXT_DIM),
                };
                let galley = ui.painter().layout(
                    text.to_string(),
                    egui::FontId::proportional(11.0),
                    color,
                    2.0 * button - 8.0,
                );
                ui.painter().galley(c - galley.size() * 0.5, galley, color);
                let response = response
                    .on_hover_text("Tap to run · right-click or long-press to choose what it runs");
                let hold = context_menu_or_hold(ui, &response, |ui| {
                    ui.set_max_height(360.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if slot_choices(ui, &mut app.workspace.ui.quick_menu[i]) {
                            ui.close_menu();
                        }
                    });
                });
                if response.clicked() && !hold.open {
                    run = Some(info.map(|i| i.action));
                }
            }
        });
    match run {
        Some(action) => {
            // Run as its shortcut would, next time shortcuts are read.
            app.workspace.jobs.deferred_actions.extend(action);
            close(ctx);
            ctx.request_repaint();
        }
        None => {
            let pressed_outside = ctx.input(|i| i.pointer.any_pressed())
                && ctx
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| p.distance(center) > reach)
                && !ctx.is_context_menu_open();
            let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if pressed_outside || escape {
                close(ctx);
            }
        }
    }
    let _ = area;
}

/// The on-screen modifier keys: each latched (a tap) or held down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ModifierKeys {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

fn keys_id() -> egui::Id {
    egui::Id::new("modifier_keys")
}

/// Touch mode's modifier keys this frame: held as if on a keyboard, for
/// the tools and panels reading the modifiers after (call it before them).
pub(crate) fn apply_modifiers(ctx: &egui::Context) {
    let keys: ModifierKeys = ctx.data(|d| d.get_temp(keys_id())).unwrap_or_default();
    if keys == ModifierKeys::default() {
        return;
    }
    ctx.input_mut(|i| {
        let m = &mut i.modifiers;
        m.shift |= keys.shift;
        m.alt |= keys.alt;
        if keys.ctrl {
            m.ctrl = true;
            m.command = true;
        }
    });
}

/// Touch mode: Shift, Alt and Ctrl as a column of keys at `bottom_left`
/// (screen). A tap latches a key (tap again to let go); pressed and held,
/// it's down until it lifts (for a pen in the other hand).
pub(crate) fn modifier_keys(ctx: &egui::Context, bottom_left: egui::Pos2, side: f32) {
    let latched_id = keys_id().with("latched");
    let mut latched: ModifierKeys = ctx.data(|d| d.get_temp(latched_id)).unwrap_or_default();
    let mut held = ModifierKeys::default();
    egui::Area::new(egui::Id::new("canvas_modifier_keys"))
        .fixed_pos(bottom_left)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (name, latch, hold) in [
                ("Ctrl", &mut latched.ctrl, &mut held.ctrl),
                ("Alt", &mut latched.alt, &mut held.alt),
                ("Shift", &mut latched.shift, &mut held.shift),
            ] {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(side, side * 0.8), egui::Sense::click());
                let down = response.is_pointer_button_down_on();
                if response.clicked() {
                    // A tap latches; a long press was only held.
                    let quick = ui.input(|i| {
                        i.pointer
                            .press_start_time()
                            .is_none_or(|t| i.time - t < 0.35)
                    });
                    if quick {
                        *latch = !*latch;
                    }
                }
                *hold = down;
                let on = *latch || down;
                ui.painter()
                    .rect_filled(rect, RADIUS_SMALL, if on { accent() } else { BG_PANEL });
                ui.painter()
                    .rect_stroke(rect, RADIUS_SMALL, Stroke::new(1.0_f32, BORDER_LIGHT));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    name,
                    egui::FontId::proportional(12.0),
                    if on { TEXT_STRONG } else { TEXT },
                );
                response.on_hover_text(format!(
                    "{name}: tap to keep it down, tap again to let go; or hold it while you paint"
                ));
            }
        });
    ctx.data_mut(|d| d.insert_temp(latched_id, latched));
    let keys = ModifierKeys {
        shift: latched.shift || held.shift,
        alt: latched.alt || held.alt,
        ctrl: latched.ctrl || held.ctrl,
    };
    ctx.data_mut(|d| d.insert_temp(keys_id(), keys));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_slot_names_an_action() {
        let prefs = crate::ui::prefs::UiPrefs::default();
        for slot in prefs.quick_menu.iter().flatten() {
            assert!(action_for(slot).is_some(), "{slot}");
        }
        assert!(action_for("quick_menu").is_none(), "not itself");
        let c = slot_centers(egui::pos2(100.0, 100.0), 50.0);
        assert!(
            (c[0] - egui::pos2(100.0, 50.0)).length() < 1e-3,
            "the first on top"
        );
        assert!(c[1].x > 100.0 && c[1].y < 100.0, "then clockwise");
    }

    /// A tap on a slot runs its action, as its shortcut would.
    #[test]
    fn a_tap_on_a_slot_runs_its_action() {
        let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
            64,
            64,
            egui::Color32::WHITE,
            64,
        ));
        app.active_tool = crate::app::tools::Tool::Fill;
        // Slot 3 (straight down) picks the brush.
        app.workspace.ui.quick_menu[3] = Some("brush".into());
        let ctx = egui::Context::default();
        let center = egui::pos2(400.0, 300.0);
        open(&ctx, center);
        let below = slot_centers(center, 26.0 * 2.4)[3];
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                show(&mut app, ctx);
                crate::app::input::shortcuts::handle_shortcuts(&mut app, ctx);
            });
        };
        let press = |pressed| egui::Event::PointerButton {
            pos: below,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        // (Its first frame only sizes it.)
        frame(vec![]);
        frame(vec![egui::Event::PointerMoved(below)]);
        frame(vec![press(true)]);
        frame(vec![press(false)]);
        frame(vec![]);
        assert!(matches!(app.active_tool, crate::app::tools::Tool::Brush));
        assert!(!is_open(&ctx), "it closes");
    }

    #[test]
    fn a_latched_modifier_key_reaches_the_input() {
        let ctx = egui::Context::default();
        let mut shift = false;
        for _ in 0..2 {
            ctx.data_mut(|d| {
                d.insert_temp(
                    keys_id(),
                    ModifierKeys {
                        shift: true,
                        ..Default::default()
                    },
                )
            });
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                apply_modifiers(ctx);
                shift = ctx.input(|i| i.modifiers.shift);
            });
        }
        assert!(shift);
    }
}
