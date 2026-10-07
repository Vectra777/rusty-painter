//! The selected layer's keyed motion as numbers (position, scale, turn,
//! opacity), each with its key at this frame, the layer it follows, and
//! the easing curve editor for a key: presets, and a curve whose two
//! handles are dragged.

use crate::PainterApp;
use crate::canvas::motion::{Ease, Prop};
use crate::canvas::rig::Curve;
use crate::canvas::storage::{Anim, LayerId, LayerKind};
use crate::ui::style::*;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

/// A key's diamond: filled when on, an outline when not (dimmer still when
/// the property has no keys).
pub(crate) fn paint_diamond(
    p: &egui::Painter,
    c: Pos2,
    r: f32,
    fill: Option<Color32>,
    edge: Color32,
) {
    let points = vec![
        c + vec2(0.0, -r),
        c + vec2(r, 0.0),
        c + vec2(0.0, r),
        c + vec2(-r, 0.0),
    ];
    p.add(Shape::convex_polygon(
        points,
        fill.unwrap_or(Color32::TRANSPARENT),
        Stroke::new(1.2_f32, edge),
    ));
}

/// A toggle showing whether `p` is keyed at this frame. Returns whether
/// it was clicked.
fn key_toggle(ui: &mut egui::Ui, keyed_here: bool, has_keys: bool, p: Prop) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let color = if keyed_here {
        ACCENT
    } else if has_keys {
        TEXT
    } else {
        TEXT_DIM
    };
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, WIDGET_HOVER);
    }
    paint_diamond(
        ui.painter(),
        rect.center(),
        5.0,
        keyed_here.then_some(ACCENT),
        color,
    );
    let tip = if keyed_here {
        format!(
            "{}: keyed at this frame (click to take the key away)",
            p.name()
        )
    } else {
        format!("Key {} at this frame", p.name().to_lowercase())
    };
    response.on_hover_text(tip).clicked()
}

/// The motion fields of the layer the keys go on for the selected one.
/// `stacked`: one property a row (in the timeline), else in a line.
pub(crate) fn motion_fields(app: &mut PainterApp, ui: &mut egui::Ui, stacked: bool) {
    let Some(target) = app.motion_target(app.canvas.active_layer_idx) else {
        ui.label(egui::RichText::new("Select a layer to animate").color(TEXT_DIM));
        return;
    };
    let t = app.canvas.time;
    let motion = app.canvas.layers[target].motion.as_deref().cloned();
    let keyed = |p: Prop| motion.as_ref().is_some_and(|m| m.key_at(p, t).is_some());
    let has = |p: Prop| motion.as_ref().is_some_and(|m| !m.keys(p).is_empty());
    let mut changed: Option<(Prop, [f32; 2])> = None;
    let mut done = false;
    let mut toggle: Option<Prop> = None;
    let row = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
        if stacked {
            ui.horizontal(|ui| add(ui));
        } else {
            add(ui);
        }
    };
    // The tool's bar has room for where it is; the timeline's inspector
    // for the pivot and the effects too.
    let props: &[Prop] = if stacked {
        &Prop::ALL
    } else {
        &Prop::TRANSFORM[..4]
    };
    for &p in props {
        if stacked && p == Prop::EFFECTS[0] {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Effects").strong().color(TEXT));
        }
        let value = app.motion_value(target, p);
        row(ui, &mut |ui: &mut egui::Ui| {
            if key_toggle(ui, keyed(p), has(p), p) {
                toggle = Some(p);
            }
            let label = egui::RichText::new(p.name()).color(TEXT_DIM);
            if stacked {
                ui.add_sized(vec2(58.0, 18.0), egui::Label::new(label));
            } else {
                ui.label(label);
            }
            let mut v = value;
            let mut responses = Vec::new();
            // A one-number property shown as `shown` times its value.
            let one = |ui: &mut egui::Ui,
                       v: &mut f32,
                       shown: f32,
                       suffix: &str,
                       range: std::ops::RangeInclusive<f32>| {
                let mut x = *v * shown;
                let r = ui.add(
                    egui::DragValue::new(&mut x)
                        .speed(0.5)
                        .range(range)
                        .suffix(suffix)
                        .max_decimals(1),
                );
                *v = x / shown;
                r
            };
            match p {
                Prop::Anchor => {
                    responses.push(
                        ui.add(
                            egui::DragValue::new(&mut v[0])
                                .speed(1.0)
                                .prefix("x ")
                                .max_decimals(1),
                        ),
                    );
                    responses.push(
                        ui.add(
                            egui::DragValue::new(&mut v[1])
                                .speed(1.0)
                                .prefix("y ")
                                .max_decimals(1),
                        ),
                    );
                }
                Prop::Blur => responses.push(one(ui, &mut v[0], 1.0, " px", 0.0..=200.0)),
                Prop::Brightness | Prop::Contrast => {
                    responses.push(one(ui, &mut v[0], 100.0, "%", -100.0..=100.0))
                }
                Prop::Saturation => responses.push(one(ui, &mut v[0], 100.0, "%", 0.0..=200.0)),
                Prop::Hue => responses.push(one(ui, &mut v[0], 1.0, "°", -360.0..=360.0)),
                Prop::Tint => responses.push(one(ui, &mut v[0], 100.0, "%", 0.0..=100.0)),
                Prop::Position => {
                    responses.push(
                        ui.add(
                            egui::DragValue::new(&mut v[0])
                                .speed(1.0)
                                .prefix("x ")
                                .max_decimals(1),
                        ),
                    );
                    responses.push(
                        ui.add(
                            egui::DragValue::new(&mut v[1])
                                .speed(1.0)
                                .prefix("y ")
                                .max_decimals(1),
                        ),
                    );
                }
                Prop::Scale => {
                    let mut pct = [v[0] * 100.0, v[1] * 100.0];
                    let both = (pct[0] - pct[1]).abs() < 1e-3;
                    if both {
                        let r = ui.add(
                            egui::DragValue::new(&mut pct[0])
                                .speed(0.5)
                                .suffix("%")
                                .max_decimals(1),
                        );
                        pct[1] = pct[0];
                        responses.push(r);
                    } else {
                        responses.push(
                            ui.add(
                                egui::DragValue::new(&mut pct[0])
                                    .speed(0.5)
                                    .prefix("x ")
                                    .suffix("%")
                                    .max_decimals(1),
                            ),
                        );
                        responses.push(
                            ui.add(
                                egui::DragValue::new(&mut pct[1])
                                    .speed(0.5)
                                    .prefix("y ")
                                    .suffix("%")
                                    .max_decimals(1),
                            ),
                        );
                    }
                    v = [pct[0] / 100.0, pct[1] / 100.0];
                }
                Prop::Rotation => {
                    responses.push(
                        ui.add(
                            egui::DragValue::new(&mut v[0])
                                .speed(0.5)
                                .suffix("°")
                                .max_decimals(1),
                        ),
                    );
                }
                Prop::Opacity => {
                    let mut pct = v[0] * 100.0;
                    responses.push(
                        ui.add(
                            egui::DragValue::new(&mut pct)
                                .speed(0.5)
                                .range(0.0..=100.0)
                                .suffix("%")
                                .max_decimals(0),
                        ),
                    );
                    v[0] = pct / 100.0;
                }
            }
            for r in &responses {
                done |= r.drag_stopped() || r.lost_focus();
            }
            if responses.iter().any(|r| r.changed()) {
                changed = Some((p, v));
            }
        });
        if !stacked {
            ui.add_space(4.0);
        }
    }
    if let Some((p, v)) = changed {
        app.motion_edit(target, |m| m.set(p, t, v));
    }
    if done {
        app.motion_edit_done();
    }
    if let Some(p) = toggle {
        if keyed(p) {
            app.remove_key(target, p, t);
        } else {
            let v = app.motion_value(target, p);
            app.motion_step(target, |m| m.set(p, t, v));
        }
    }
    let follow = |ui: &mut egui::Ui, app: &mut PainterApp| {
        let current = motion.as_ref().and_then(|m| m.parent).map(LayerId);
        let name_of = |id: LayerId| {
            app.canvas
                .layer_index_of(id)
                .map_or("(gone)".to_string(), |i| app.canvas.layers[i].name.clone())
        };
        let mut chosen = None;
        egui::ComboBox::from_id_salt(("motion_follow", target))
            .selected_text(current.map_or("Nothing".to_string(), name_of))
            .width(110.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "Nothing").clicked() {
                    chosen = Some(None);
                }
                for (i, layer) in app.canvas.layers.iter().enumerate().rev() {
                    let candidate = i != target
                        && i != 0
                        && !matches!(layer.anim, Some(Anim::Frame(_)))
                        && !matches!(layer.kind, LayerKind::Mask { .. });
                    if candidate
                        && ui
                            .selectable_label(current == Some(layer.id), &layer.name)
                            .clicked()
                    {
                        chosen = Some(Some(layer.id));
                    }
                }
            })
            .response
            .on_hover_text("Follow another layer: move, turn and scale with it");
        if let Some(c) = chosen {
            app.set_motion_parent(target, c);
        }
    };
    if stacked {
        // The tint's colour (the same at every frame).
        let mut colour = motion.as_ref().map_or([255, 150, 40], |m| m.tint_color);
        ui.horizontal(|ui| {
            ui.add_sized(
                vec2(78.0, 18.0),
                egui::Label::new(egui::RichText::new("Tint colour").color(TEXT_DIM)),
            );
            if ui.color_edit_button_srgb(&mut colour).changed() {
                app.motion_edit(target, |m| m.tint_color = colour);
            }
        });
        // (A drag on the colour picker is one undo step.)
        if ui.input(|i| i.pointer.any_released()) {
            app.motion_edit_done();
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_sized(
                vec2(78.0, 18.0),
                egui::Label::new(egui::RichText::new("Follows").color(TEXT_DIM)),
            );
            follow(ui, app);
        });
        ui.horizontal(|ui| {
            if ui
                .button("Key all")
                .on_hover_text("Key every property at this frame (Alt+K)")
                .clicked()
            {
                app.key_all_here(target);
            }
            if ui
                .add_enabled(motion.is_some(), egui::Button::new("Clear motion"))
                .on_hover_text("Take every key away: the layer shows where it was painted")
                .clicked()
            {
                app.clear_motion(target);
            }
        });
    } else {
        ui.label(egui::RichText::new("Follows").color(TEXT_DIM));
        follow(ui, app);
        if ui
            .button("Key all")
            .on_hover_text("Key every property at this frame (Alt+K)")
            .clicked()
        {
            app.key_all_here(target);
        }
    }
}

/// How a key eases to the next: preset buttons and the curve, its two
/// handles dragged (up past the top overshoots). Returns the new curve
/// when it changes, and whether a drag of it ended.
pub(crate) fn ease_editor(ui: &mut egui::Ui, curve: Curve, id: egui::Id) -> (Option<Curve>, bool) {
    let mut out = None;
    let mut ended = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for ease in Ease::ALL {
            let on = Ease::of(curve) == Some(ease);
            if ui.selectable_label(on, ease.name()).clicked() {
                out = Some(ease.curve());
                ended = true;
            }
        }
    });
    let side = ui.available_width().clamp(120.0, 220.0);
    let (rect, _) = ui.allocate_exact_size(vec2(side, side * 0.8), Sense::hover());
    let p = ui.painter_at(rect.expand(6.0));
    p.rect_filled(rect, 4.0, BG_INSET);
    // The unit square sits in the middle; room above and below for
    // overshoot.
    let box_ = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.top() + rect.height() * 0.2),
        pos2(rect.right() - 10.0, rect.bottom() - rect.height() * 0.2),
    );
    let to_screen = |x: f32, y: f32| {
        pos2(
            box_.left() + x * box_.width(),
            box_.bottom() - y * box_.height(),
        )
    };
    let from_screen = |s: Pos2| {
        [
            ((s.x - box_.left()) / box_.width()).clamp(0.0, 1.0),
            ((box_.bottom() - s.y) / box_.height()).clamp(-0.6, 1.6),
        ]
    };
    p.rect_stroke(box_, 0.0, Stroke::new(1.0_f32, BORDER_LIGHT));
    for k in 1..4 {
        let x = box_.left() + box_.width() * k as f32 / 4.0;
        p.vline(
            x,
            box_.y_range(),
            Stroke::new(1.0_f32, Color32::from_gray(40)),
        );
        let y = box_.top() + box_.height() * k as f32 / 4.0;
        p.hline(
            box_.x_range(),
            y,
            Stroke::new(1.0_f32, Color32::from_gray(40)),
        );
    }
    let ys: Vec<Pos2> = (0..=48)
        .map(|k| {
            let u = k as f32 / 48.0;
            to_screen(u, crate::canvas::rig::eval::ease(curve, u))
        })
        .collect();
    if curve == Curve::Stepped {
        p.add(Shape::line(
            vec![
                to_screen(0.0, 0.0),
                to_screen(1.0, 0.0),
                to_screen(1.0, 1.0),
            ],
            Stroke::new(2.0_f32, ACCENT),
        ));
    } else {
        p.add(Shape::line(ys, Stroke::new(2.0_f32, ACCENT)));
    }
    let [x1, y1, x2, y2] = match curve {
        Curve::Bezier(c) => c,
        Curve::Linear => [1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0],
        Curve::Stepped => [0.5, 0.0, 1.0, 0.0],
    };
    let handles = [
        (to_screen(0.0, 0.0), to_screen(x1, y1)),
        (to_screen(1.0, 1.0), to_screen(x2, y2)),
    ];
    let mut control = [x1, y1, x2, y2];
    for (k, (from, at)) in handles.into_iter().enumerate() {
        p.line_segment([from, at], Stroke::new(1.0_f32, TEXT_DIM));
        let r = ui.interact(
            Rect::from_center_size(at, vec2(16.0, 16.0)),
            id.with(("ease_handle", k)),
            Sense::drag(),
        );
        let fill = if r.hovered() || r.dragged() {
            TEXT_STRONG
        } else {
            TEXT
        };
        p.circle_filled(at, 5.0, fill);
        p.circle_stroke(at, 5.0, Stroke::new(1.5_f32, ACCENT));
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos()
        {
            let [x, y] = from_screen(pos);
            control[k * 2] = x;
            control[k * 2 + 1] = y;
            out = Some(Curve::Bezier(control));
        }
        if r.drag_stopped() {
            ended = true;
        }
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    (out, ended)
}
