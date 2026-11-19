//! The Figure tool's options (3D figure layers): the selected joint (reset
//! it or its part, its limits, its pin), the scene's figures and props
//! (add, import, delete, snap to the ground), poses (built in, saved,
//! mirrored, copied), the hand pad and hand poses, body shape, pose keys
//! at the frame showing, the camera's field of view (or matched to a
//! perspective ruler), how the figures are drawn and how opaque the layer
//! is.

use crate::PainterApp;
use crate::canvas::figure::RenderStyle;
use crate::canvas::figure::presets::{BODY_PRESETS, HAND_PRESETS, PoseKind};
use crate::canvas::figure::skeleton::Side;
use crate::ui::bar_slider::BarSlider;
use crate::ui::style::*;
use crate::ui::widgets::{percent_of_unit, vdivider};
use eframe::egui::{self, RichText};

/// The hand pad's side, in points.
const PAD_SIZE: f32 = 132.0;

/// The Figure tool's row in the options bar; returns its hint.
pub(crate) fn figure_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    ui.label(RichText::new("3D Figure").strong().color(TEXT_STRONG));
    vdivider(ui);
    let Some(figure) = app.active_figure() else {
        if ui.button("New 3D Figure").clicked() {
            app.add_figure_layer();
        }
        return "Pick a 3D figure layer (Layer → New 3D Figure)";
    };
    let (camera, style, outline, line_fill) = (
        figure.camera,
        figure.style,
        figure.outline,
        figure.line_fill,
    );
    let idx = app.canvas.active_layer_idx;

    joint_controls(app, ui);
    vdivider(ui);
    ui.menu_button("Scene ▾", |ui| scene_menu(app, ui))
        .response
        .on_hover_text("The figures and props in this layer");
    ui.menu_button("Pose ▾", |ui| pose_menu(app, ui))
        .response
        .on_hover_text("Body poses, saved poses, mirror, copy and paste");
    ui.menu_button("Hand ▾", |ui| hand_menu(app, ui))
        .response
        .on_hover_text("The hand pad, hand poses and the Hand Scanner");
    ui.menu_button("Body ▾", |ui| body_menu(app, ui))
        .response
        .on_hover_text("The selected figure's proportions");
    vdivider(ui);

    // Pose keys at the frame showing.
    let frame = app.canvas.time;
    let keyed = app.figure_keyed_here();
    let key_label = if keyed { "Key pose ◆" } else { "Key pose" };
    if ui
        .button(key_label)
        .on_hover_text(format!(
            "Keep the figures' poses at frame {frame}: between keys they ease from one to the next (the timeline shows the keys)"
        ))
        .clicked()
    {
        app.figure_key_pose();
    }
    if ui
        .add_enabled(keyed, egui::Button::new("Remove key"))
        .on_hover_text(format!("Take away the pose key at frame {frame}"))
        .on_disabled_hover_text(format!("No pose key at frame {frame}"))
        .clicked()
    {
        app.figure_remove_key();
    }
    vdivider(ui);

    // The camera.
    ui.label(RichText::new("Lens").color(TEXT_DIM));
    let mut fov = camera.fov;
    let response = ui
        .add(
            BarSlider::new(&mut fov, 10.0..=100.0)
                .max_decimals(0)
                .suffix("°"),
        )
        .on_hover_text("Field of view: wide shows more, with stronger perspective");
    edit_from(app, &response, "Lens", |f| f.camera.fov = fov);
    if ui
        .button("Reset camera")
        .on_hover_text("Back to the camera a new figure layer starts with")
        .clicked()
    {
        app.figure_reset_camera();
    }
    let ruler = app.figure_vanishing().is_some();
    if ui
        .add_enabled(ruler, egui::Button::new("Match perspective ruler"))
        .on_hover_text(
            "Turn the camera and set its lens so the scene's ground and uprights run to the perspective ruler's vanishing points (one point: its horizon and heading, the usual lens)",
        )
        .on_disabled_hover_text("Add a 1-, 2- or 3-point assistant first (View → Assistants), and show it")
        .clicked()
    {
        app.figure_match_perspective();
    }
    vdivider(ui);

    // How it's drawn.
    let mut picked = style;
    egui::ComboBox::from_id_salt("figure_style")
        .selected_text(style.label())
        .show_ui(ui, |ui| {
            for s in RenderStyle::ALL {
                ui.selectable_value(&mut picked, s, s.label());
            }
        });
    if picked != style {
        app.figure_step("Figure style", |f| f.style = picked);
    }
    if style == RenderStyle::LineArt {
        let mut width = outline;
        let response = ui
            .add(
                BarSlider::new(&mut width, 0.0..=16.0)
                    .max_decimals(1)
                    .suffix(" px"),
            )
            .on_hover_text("The outline's width, in canvas pixels");
        edit_from(app, &response, "Outline", |f| f.outline = width);
        let mut fill = line_fill;
        if ui
            .checkbox(&mut fill, "White fill")
            .on_hover_text("White inside the lines (else see-through)")
            .changed()
        {
            app.figure_step("Figure style", |f| f.line_fill = fill);
        }
    }
    ui.label(RichText::new("Opacity").color(TEXT_DIM));
    let mut opacity = app.canvas.layers[idx].opacity;
    if ui
        .add(percent_of_unit(BarSlider::new(&mut opacity, 0.0..=1.0)))
        .on_hover_text("The layer's opacity: fade the figure under your drawing")
        .changed()
    {
        app.canvas_mut().layers[idx].opacity = opacity;
        app.mark_all_tiles_dirty();
        app.mark_unsaved();
    }
    "Drag a part to pose it (IK)  ·  Shift+drag: turn just that joint  ·  Alt+drag: move the figure  ·  double-click: pin  ·  drag empty space (or middle-drag) to orbit  ·  Shift+drag empty: pan  ·  wheel: closer / further"
}

/// The selected joint: its name, resets, limits and pin.
fn joint_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let joint = app.figure_selected_name();
    ui.label(
        RichText::new(joint.as_deref().unwrap_or("No joint picked")).color(if joint.is_some() {
            TEXT
        } else {
            TEXT_DIM
        }),
    );
    let selected = app.workspace.figure.tool.selected;
    let instance = selected.and_then(|(f, _)| app.active_figure()?.scene.get(f));
    let (pinned, limited) = match (selected, instance) {
        (Some((_, j)), Some(i)) => (i.is_pinned(j), !i.is_unlimited(j)),
        _ => (false, true),
    };
    let some = joint.is_some();
    if ui
        .add_enabled(some, egui::Button::new("Reset joint"))
        .on_hover_text("The picked joint back as the model has it at rest")
        .clicked()
    {
        app.figure_reset_joint();
    }
    if ui
        .add_enabled(some, egui::Button::new("Reset part"))
        .on_hover_text("The picked joint and everything below it back at rest")
        .clicked()
    {
        app.figure_reset_part();
    }
    let mut on = limited;
    if ui
        .add_enabled(some, egui::Checkbox::new(&mut on, "Limits"))
        .on_hover_text("Keep the joint within how far a human joint turns (off: any way)")
        .changed()
    {
        app.figure_toggle_limits();
    }
    let mut pin = pinned;
    if ui
        .add_enabled(some, egui::Checkbox::new(&mut pin, "Pin"))
        .on_hover_text(
            "A pinned joint keeps its place while the rest is posed (double-click a joint)",
        )
        .changed()
        && let Some((f, j)) = selected
    {
        app.figure_toggle_pin(f, j);
    }
}

/// The figures and props: pick one, add, import, delete, put on the
/// ground.
fn scene_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let names: Vec<String> = (app.active_figure())
        .map(|l| l.scene.iter().map(|f| f.model.name().to_string()).collect())
        .unwrap_or_default();
    let current = app.figure_selected_instance();
    for (i, name) in names.iter().enumerate() {
        if ui
            .selectable_label(current == Some(i), format!("{}. {name}", i + 1))
            .clicked()
        {
            let tool = &mut app.workspace.figure.tool;
            tool.figure = Some(i);
            tool.selected = None;
        }
    }
    if names.is_empty() {
        ui.label(RichText::new("No figures").color(TEXT_DIM));
    }
    ui.separator();
    if ui.button("Add mannequin").clicked() {
        app.figure_add_mannequin();
    }
    let realistic = crate::canvas::figure::pack::models();
    if realistic.is_empty() {
        ui.label(
            RichText::new(crate::canvas::figure::pack::NOT_INSTALLED)
                .small()
                .color(TEXT_DIM),
        )
        .on_hover_text("The figure pack ships beside the app (a figures folder); the mannequin works without it");
    }
    for model in realistic {
        if ui
            .button(format!("Add {}", model.name.to_lowercase()))
            .on_hover_text(format!("A realistic figure ({} licence)", model.licence))
            .clicked()
        {
            app.figure_add_pack_model(&model.id);
        }
    }
    if ui
        .button("Import model…")
        .on_hover_text("A .glb or .gltf file: a figure if it has a human skeleton, else a prop")
        .clicked()
    {
        app.pick_open(crate::app::files::OpenFor::FigureModel);
        ui.close_menu();
    }
    if ui
        .add_enabled(current.is_some(), egui::Button::new("Delete selected"))
        .on_hover_text("Delete the selected figure or prop (Delete)")
        .clicked()
    {
        app.figure_delete_selected();
    }
    if ui
        .add_enabled(!names.is_empty(), egui::Button::new("Snap to ground"))
        .on_hover_text("Put the selected figure's lowest point on the ground")
        .clicked()
    {
        app.figure_snap_to_ground();
    }
}

/// Body poses, the user's, mirror, copy and paste.
fn pose_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    if ui
        .button("Pose from camera…")
        .on_hover_text("Pose the figure from yourself, through the camera")
        .clicked()
    {
        app.open_pose_from_camera();
        ui.close_menu();
    }
    if ui
        .add_enabled(
            cfg!(feature = "pose-capture"),
            egui::Button::new("Pose from photo…"),
        )
        .on_hover_text("Pose the figure as a person in a photo stands")
        .on_disabled_hover_text("This build can't read poses (it was built without `pose-capture`)")
        .clicked()
    {
        app.pose_from_photo();
        ui.close_menu();
    }
    ui.separator();
    ui.label(RichText::new("Body").color(TEXT_DIM));
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(260.0);
        for (i, preset) in BODY_PRESETS.iter().enumerate() {
            if ui.button(preset.name).clicked() {
                app.figure_apply_body_preset(i);
            }
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button("Mirror")
            .on_hover_text("Swap left and right")
            .clicked()
        {
            app.figure_mirror_pose();
        }
        if ui.button("Copy").clicked() {
            app.figure_copy_pose();
        }
        let can_paste = app.workspace.figure.clipboard.is_some();
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste"))
            .on_hover_text("The copied pose, on any figure (by joint name)")
            .clicked()
        {
            app.figure_paste_pose();
        }
        if ui
            .button("Reset")
            .on_hover_text("Every figure back in its standing pose")
            .clicked()
        {
            app.figure_reset_pose();
        }
    });
    ui.separator();
    ui.label(RichText::new("My poses").color(TEXT_DIM));
    let saved: Vec<(String, PoseKind)> = (app.workspace.figure.saved_poses().iter())
        .map(|p| (p.name.clone(), p.kind))
        .collect();
    if saved.is_empty() {
        ui.label(RichText::new("None saved yet").color(TEXT_DIM));
    }
    let mut delete = None;
    for (i, (name, kind)) in saved.iter().enumerate() {
        ui.horizontal(|ui| {
            let tag = match kind {
                PoseKind::Body => "",
                PoseKind::Hand => " (hand)",
            };
            if ui.button(format!("{name}{tag}")).clicked() {
                app.figure_apply_saved(i);
            }
            if ui.small_button("✕").on_hover_text("Forget it").clicked() {
                delete = Some(i);
            }
        });
    }
    if let Some(i) = delete {
        app.figure_delete_saved(i);
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.workspace.figure.pose_name)
                .hint_text("Name")
                .desired_width(110.0),
        );
        let name = app.workspace.figure.pose_name.clone();
        if ui
            .button("Save pose")
            .on_hover_text("Save the selected figure's whole pose")
            .clicked()
        {
            app.figure_save_pose(&name, PoseKind::Body);
            app.workspace.figure.pose_name.clear();
        }
        if ui
            .button("Save hand")
            .on_hover_text("Save the chosen hand's pose (Hand menu)")
            .clicked()
        {
            app.figure_save_pose(&name, PoseKind::Hand);
            app.workspace.figure.pose_name.clear();
        }
    });
}

/// Which hand, the hand pad with its finger locks, and hand poses.
fn hand_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    if ui
        .button("Hand Scanner…")
        .on_hover_text("Pose the hand from your own, through the camera")
        .clicked()
    {
        app.open_hand_scanner();
        ui.close_menu();
    }
    ui.separator();
    ui.horizontal(|ui| {
        let hand = &mut app.workspace.figure.hand;
        ui.selectable_value(hand, Side::Left, "Left hand");
        ui.selectable_value(hand, Side::Right, "Right hand");
    });
    ui.separator();
    hand_pad(app, ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Lock").color(TEXT_DIM));
        let names = ["Thumb", "Index", "Middle", "Ring", "Little"];
        for (k, name) in names.into_iter().enumerate() {
            ui.checkbox(&mut app.workspace.figure.pad.locked[k], name)
                .on_hover_text("A locked finger keeps its pose on the pad");
        }
    });
    ui.separator();
    ui.label(RichText::new("Hand poses").color(TEXT_DIM));
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(300.0);
        for (i, preset) in HAND_PRESETS.iter().enumerate() {
            if ui.button(preset.name).clicked() {
                app.figure_edit_done();
                app.figure_apply_hand_preset(i);
                app.figure_edit_done();
            }
        }
    });
}

/// The hand pad: across, how far the fingers spread; down, how far they
/// curl. Dragging on it poses the chosen hand (one undo step a drag).
fn hand_pad(app: &mut PainterApp, ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(PAD_SIZE, PAD_SIZE),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let grid = egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color);
    for k in 1..4 {
        let t = k as f32 / 4.0;
        painter.hline(rect.x_range(), rect.top() + rect.height() * t, grid);
        painter.vline(rect.left() + rect.width() * t, rect.y_range(), grid);
    }
    let small = egui::FontId::proportional(10.0);
    let dim = ui.visuals().weak_text_color();
    painter.text(
        rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        "open",
        small.clone(),
        dim,
    );
    painter.text(
        rect.left_bottom() + egui::vec2(4.0, -2.0),
        egui::Align2::LEFT_BOTTOM,
        "fist",
        small.clone(),
        dim,
    );
    painter.text(
        rect.right_top() + egui::vec2(-4.0, 2.0),
        egui::Align2::RIGHT_TOP,
        "spread",
        small,
        dim,
    );
    let pad = app.workspace.figure.pad;
    let at = rect.min + egui::vec2(pad.spread * rect.width(), pad.curl * rect.height());
    painter.circle(
        at,
        6.0,
        accent(),
        egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
    );
    if response.drag_started() || response.clicked() {
        app.figure_edit_done();
    }
    if let Some(p) = response.interact_pointer_pos()
        && (response.dragged() || response.clicked())
    {
        let pad = &mut app.workspace.figure.pad;
        pad.spread = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        pad.curl = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        app.figure_apply_pad();
    }
    if response.drag_stopped() || response.clicked() {
        app.figure_edit_done();
    }
    response.on_hover_text("Drag: across spreads the fingers, down curls them");
}

/// The selected figure's height, proportions and morph targets.
fn body_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let Some(f) = app
        .figure_selected_instance()
        .or_else(|| (app.active_figure().is_some_and(|l| l.scene.len() == 1)).then_some(0))
    else {
        ui.label(RichText::new("Pick a figure first").color(TEXT_DIM));
        return;
    };
    let Some(instance) = app.active_figure().and_then(|l| l.scene.get(f)).cloned() else {
        return;
    };
    let model = instance.model.resolve().ok();
    let human = model
        .as_ref()
        .is_some_and(|m| m.skeleton.joints.iter().any(|j| j.human.is_some()));
    egui::Grid::new("figure_body")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Height");
            let mut scale = instance.scale;
            let response = ui.add(percent_of_unit(BarSlider::new(&mut scale, 0.2..=2.0)));
            shape_from(app, &response, f, move |i| i.scale = scale);
            ui.end_row();
            if human {
                let mut shape = instance.shape;
                for (k, (label, value)) in shape.fields().into_iter().enumerate() {
                    ui.label(label);
                    let mut v = *value;
                    let range = if label == "Head" || label == "Hands" {
                        0.6..=1.6
                    } else {
                        0.6..=1.5
                    };
                    let response = ui.add(percent_of_unit(BarSlider::new(&mut v, range)));
                    shape_from(app, &response, f, move |i| {
                        *i.shape.fields()[k].1 = v;
                    });
                    ui.end_row();
                }
            }
            if let Some(model) = &model {
                for (k, morph) in model.morphs.iter().enumerate() {
                    let name = if morph.name.is_empty() {
                        format!("Morph {}", k + 1)
                    } else {
                        morph.name.clone()
                    };
                    ui.label(name);
                    let mut w = instance.morphs.get(k).copied().unwrap_or(0.0);
                    let response = ui.add(percent_of_unit(BarSlider::new(&mut w, 0.0..=1.0)));
                    shape_from(app, &response, f, move |i| {
                        if i.morphs.len() <= k {
                            i.morphs.resize(k + 1, 0.0);
                        }
                        i.morphs[k] = w;
                    });
                    ui.end_row();
                }
            }
        });
    if ui.button("Reset shape").clicked() {
        app.figure_edit_done();
        app.figure_edit_shape(f, |i| {
            i.shape = Default::default();
            i.morphs.clear();
            i.scale = 1.0;
        });
        app.figure_edit_done();
    }
}

/// A body slider's change: one undo step a drag.
fn shape_from(
    app: &mut PainterApp,
    response: &egui::Response,
    f: usize,
    edit: impl FnOnce(&mut crate::canvas::figure::FigureInstance),
) {
    if response.changed() {
        if response.drag_started() || !response.dragged() {
            app.figure_edit_done();
        }
        app.figure_edit_shape(f, edit);
    }
    if !response.dragged() && (response.changed() || response.drag_stopped()) {
        app.figure_edit_done();
    }
}

/// Apply a slider's change to the scene: one undo step a drag.
fn edit_from(
    app: &mut PainterApp,
    response: &egui::Response,
    label: &str,
    edit: impl FnOnce(&mut crate::canvas::figure::FigureLayer),
) {
    if response.changed() {
        if response.drag_started() || !response.dragged() {
            app.figure_edit_done();
        }
        app.figure_edit(label, edit);
    }
    if !response.dragged() && (response.changed() || response.drag_stopped()) {
        app.figure_edit_done();
    }
}
