//! The timeline panel (under the canvas), laid out like an animation
//! package's: a transport bar (playback, the frame showing, the frame rate
//! and range, onion skins, drawings and keys), a ruler to scrub along, a
//! row for every layer, and an inspector for the selected layer's motion
//! and a key's easing.
//!
//! On an animated layer's row each drawing is a block showing its
//! thumbnail and stretching over the frames it's held (an empty drawing is
//! an empty exposure). A plain layer's row is one bar: double-click a frame
//! to start drawing frames on it. Open a row (its triangle) for its keyed
//! properties, each key a diamond to drag.
//!
//! Click a frame to go there; drag a block to move its drawing, or its end
//! to hold it longer or shorter (the later drawings follow); double-click
//! an empty frame for a new drawing; right-click (or long-press) for the
//! rest. The wheel scrolls, Ctrl + wheel (or a pinch) zooms; on a touch
//! screen a drag over empty frames scrolls.

use crate::PainterApp;
use crate::app::animation::FrameSelection;
use crate::app::input::keymap::Action;
use crate::canvas::motion::{Ease, Prop};
use crate::canvas::rig::Curve;
use crate::canvas::storage::{Anim, LayerId, LayerKind};
use crate::ui::bar_slider::BarSlider;
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::motion_panel::paint_diamond;
use crate::ui::style::*;
use crate::ui::widgets::{icon_button, vdivider};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, pos2, vec2};
use std::collections::HashSet;

/// A frame's width: at first, and the zoom's limits.
const MIN_CELL: f32 = 6.0;
const MAX_CELL: f32 = 96.0;
/// How near a block's end (points) grabs it to change its hold.
const EDGE_GRAB: f32 = 5.0;
/// The paper under a drawing's thumbnail.
const PAPER: Color32 = Color32::from_gray(236);
/// A layer row's untagged colour (as in the layers panel).
const UNTAGGED: Color32 = Color32::from_gray(40);
/// The inspector's width.
const INSPECTOR_W: f32 = 260.0;

/// Sizes, larger for fingers.
#[derive(Clone, Copy)]
struct Sizes {
    header: f32,
    ruler: f32,
    row: f32,
    prop: f32,
    scrollbar: f32,
    button: f32,
    cell: f32,
    touch: bool,
}

impl Sizes {
    fn of(ctx: &egui::Context) -> Self {
        if metrics(ctx).touch {
            Sizes {
                header: 200.0,
                ruler: 30.0,
                row: 60.0,
                prop: 38.0,
                scrollbar: 14.0,
                button: 36.0,
                cell: 34.0,
                touch: true,
            }
        } else {
            Sizes {
                header: 176.0,
                ruler: 24.0,
                row: 46.0,
                prop: 24.0,
                scrollbar: 10.0,
                button: 26.0,
                cell: 24.0,
                touch: false,
            }
        }
    }
}

/// The panel's zoom and scroll, the edit under way in it, and what's open
/// and picked.
#[derive(Default)]
pub struct TimelineView {
    /// A frame's width (0 until first shown).
    pub cell: f32,
    pub scroll: egui::Vec2,
    drag: Option<Drag>,
    /// What was right-clicked, for its menu.
    menu: Option<Menu>,
    /// Where the frames were last drawn.
    pub(crate) grid: Option<Rect>,
    /// The frame showing when last drawn: the view follows it when it moves.
    seen_time: Option<u32>,
    /// Rows open on their keyed properties (layer ids).
    pub(crate) open: HashSet<LayerId>,
    /// The key picked: its layer, property (`None`: every key at that
    /// frame) and frame.
    pub(crate) picked: Option<(LayerId, Option<Prop>, u32)>,
    /// The inspector shows.
    pub inspector: bool,
    /// Frames picked (copied, cut, pasted over, deleted or moved together).
    pub(crate) sel: Option<FrameSelection>,
    /// Where a Shift+click picks frames from: a layer's row and a frame.
    anchor: Option<(LayerId, u32)>,
    /// Where the panel's frames were last drawn (its keys work while the
    /// pointer's over them).
    pub(crate) panel: Option<Rect>,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// Along the ruler.
    Scrub,
    /// Picking frames from the `row`th layer's row (top first), frame `t`.
    Select { row: usize, t: u32 },
    /// The picked frames, held at frame `from`, now over `to`.
    Range { from: u32, to: u32 },
    /// The scrollbar's thumb, held `grab` points from its left.
    Scrollbar { grab: f32 },
    /// A finger moving the frames.
    Pan,
    /// `track`'s drawing starting at `from`, held `offset` frames into its
    /// block, to start at `to`.
    Move {
        track: LayerId,
        from: u32,
        offset: i64,
        to: u32,
    },
    /// The end of the hold of `track`'s drawing starting at `start`: the
    /// next one (starting at `next`) and the rest to start at `to`.
    Hold {
        track: LayerId,
        start: u32,
        next: u32,
        to: u32,
    },
    /// `layer`'s key of `prop` (every key at the frame, for `None`) from
    /// frame `from` to `to`.
    Key {
        layer: LayerId,
        prop: Option<Prop>,
        from: u32,
        to: u32,
    },
}

#[derive(Clone, Copy, Debug)]
enum Menu {
    Ruler(u32),
    Frame(LayerId, u32),
    Key(LayerId, Option<Prop>, u32),
}

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// An animated layer: its drawings.
    Track,
    /// A picture (paint, text, lines, fill...).
    Plain,
    Folder,
    Rig,
}

/// A row: a layer, or one of its keyed properties.
#[derive(Clone, Copy, Debug)]
struct Row {
    idx: usize,
    id: LayerId,
    depth: usize,
    kind: Kind,
    prop: Option<Prop>,
    top: f32,
    height: f32,
}

/// The layers as the timeline lists them, top first: folders' layers
/// under them (not an animated layer's drawings, which are its blocks),
/// each followed by its open properties.
fn layer_rows(app: &PainterApp, view: &TimelineView, s: Sizes) -> Vec<Row> {
    let canvas = &app.canvas;
    let animate_target = matches!(app.active_tool, crate::app::tools::Tool::Animate)
        .then(|| app.motion_target(canvas.active_layer_idx))
        .flatten();
    let mut rows = Vec::new();
    let mut top = 0.0;
    #[allow(clippy::too_many_arguments)]
    fn walk(
        app: &PainterApp,
        view: &TimelineView,
        s: Sizes,
        parent: Option<LayerId>,
        depth: usize,
        animate_target: Option<usize>,
        rows: &mut Vec<Row>,
        top: &mut f32,
    ) {
        let canvas = &app.canvas;
        for idx in (1..canvas.layers.len()).rev() {
            let layer = &canvas.layers[idx];
            // (Not a transform's floating pixels: they go back on apply.)
            if layer.parent != parent
                || app.layer_state.floating_layer_idx == Some(idx)
                || matches!(layer.kind, LayerKind::Mask { .. })
                || matches!(layer.anim, Some(Anim::Frame(_)))
            {
                continue;
            }
            let kind = if layer.anim == Some(Anim::Track) {
                Kind::Track
            } else if layer.rig.is_some() {
                Kind::Rig
            } else if layer.kind == LayerKind::Group {
                Kind::Folder
            } else {
                Kind::Plain
            };
            let row = |prop, height: f32, top: &mut f32| {
                let r = Row {
                    idx,
                    id: layer.id,
                    depth,
                    kind,
                    prop,
                    top: *top,
                    height,
                };
                *top += height;
                r
            };
            rows.push(row(None, s.row, top));
            if view.open.contains(&layer.id) || animate_target == Some(idx) {
                // Where it is, and the effects it has keys for.
                let effects = layer
                    .motion
                    .as_ref()
                    .map_or_else(Vec::new, |m| m.keyed(&Prop::EFFECTS));
                for p in Prop::TRANSFORM.into_iter().chain(effects) {
                    rows.push(row(Some(p), s.prop, top));
                }
            }
            if kind == Kind::Folder && layer.expanded {
                walk(
                    app,
                    view,
                    s,
                    Some(layer.id),
                    depth + 1,
                    animate_target,
                    rows,
                    top,
                );
            }
        }
    }
    walk(app, view, s, None, 0, animate_target, &mut rows, &mut top);
    rows
}

pub fn timeline_panel(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.workspace.animation.show_timeline {
        return;
    }
    let s = Sizes::of(ctx);
    egui::TopBottomPanel::bottom("timeline")
        .resizable(true)
        .default_height(if s.touch { 300.0 } else { 220.0 })
        .min_height(120.0)
        .frame(egui::Frame::none().fill(BG_PANEL))
        .show(ctx, |ui| {
            let mut view = std::mem::take(&mut app.workspace.animation.view);
            egui::Frame::none()
                .fill(BG_RAISED)
                .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    transport(app, ui, &mut view, s);
                });
            if app.active_rig().is_some() {
                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| rig_controls(app, ui));
            }
            let rect = ui.available_rect_before_wrap();
            let (body_rect, side) = if view.inspector && rect.width() > INSPECTOR_W + 360.0 {
                let split = rect.right() - INSPECTOR_W;
                (
                    Rect::from_min_max(rect.min, pos2(split, rect.bottom())),
                    Some(Rect::from_min_max(pos2(split, rect.top()), rect.max)),
                )
            } else {
                (rect, None)
            };
            body(app, ui, &mut view, body_rect, s);
            if let Some(side) = side {
                inspector(app, ui, &mut view, side);
            }
            ui.allocate_rect(rect, Sense::hover());
            app.workspace.animation.view = view;
        });
}

/// `text`, with the keys of `action` if it has any.
fn tip(app: &PainterApp, ctx: &egui::Context, text: &str, action: Action) -> String {
    match app.workspace.keymap.label(ctx, action) {
        Some(keys) => format!("{text} ({keys})"),
        None => text.to_string(),
    }
}

/// The bar over the frames: playback, where and how fast, onion skins,
/// drawings and keys, layers, the inspector, export.
fn transport(app: &mut PainterApp, ui: &mut egui::Ui, view: &mut TimelineView, s: Sizes) {
    let timeline = app.canvas.timeline;
    let time = app.canvas.time;
    let keys = app.navigation_keys();
    let ctx = ui.ctx().clone();
    let b = s.button;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if glyph_button(
            ui,
            b,
            Glyph::First,
            &tip(app, &ctx, "First frame", Action::FirstFrame),
            false,
        )
        .clicked()
        {
            app.go_to_frame(timeline.start);
        }
        let previous = tip(
            app,
            &ctx,
            "Previous drawing or key",
            Action::PreviousDrawing,
        );
        if glyph_button(ui, b, Glyph::PrevKey, &previous, false).clicked()
            && let Some(&k) = keys.iter().rev().find(|&&k| k < time)
        {
            app.go_to_frame(k);
        }
        if glyph_button(
            ui,
            b,
            Glyph::PrevFrame,
            &tip(app, &ctx, "Previous frame", Action::PreviousFrame),
            false,
        )
        .clicked()
        {
            app.go_to_frame(timeline.previous(time));
        }
        let playing = app.workspace.animation.playing;
        let play = if playing { Glyph::Pause } else { Glyph::Play };
        if glyph_button(
            ui,
            b,
            play,
            &tip(app, &ctx, "Play / pause", Action::PlayAnimation),
            playing,
        )
        .clicked()
        {
            app.workspace.animation.playing = !playing;
        }
        if glyph_button(
            ui,
            b,
            Glyph::NextFrame,
            &tip(app, &ctx, "Next frame", Action::NextFrame),
            false,
        )
        .clicked()
        {
            app.go_to_frame(timeline.next(time));
        }
        let next = tip(app, &ctx, "Next drawing or key", Action::NextDrawing);
        if glyph_button(ui, b, Glyph::NextKey, &next, false).clicked()
            && let Some(&k) = keys.iter().find(|&&k| k > time)
        {
            app.go_to_frame(k);
        }
        if glyph_button(
            ui,
            b,
            Glyph::Last,
            &tip(app, &ctx, "Last frame", Action::LastFrame),
            false,
        )
        .clicked()
        {
            app.go_to_frame(timeline.end);
        }
        ui.spacing_mut().item_spacing.x = 6.0;
        vdivider(ui);
        let mut frame = time;
        if ui
            .add(egui::DragValue::new(&mut frame).range(0..=99_999))
            .on_hover_text("The frame showing")
            .changed()
        {
            app.go_to_frame(frame);
        }
        ui.label(
            RichText::new(timecode(time, timeline.fps))
                .monospace()
                .color(TEXT_DIM),
        )
        .on_hover_text("Minutes : seconds : frames");
        vdivider(ui);
        let mut t = timeline;
        let mut changed = false;
        changed |= ui
            .add(
                egui::DragValue::new(&mut t.fps)
                    .range(1..=120)
                    .suffix(" fps"),
            )
            .on_hover_text("Frames a second")
            .changed();
        ui.label(RichText::new("In").color(TEXT_DIM));
        changed |= ui
            .add(egui::DragValue::new(&mut t.start).range(0..=t.end))
            .on_hover_text("The first frame played (right-click the ruler to set it)")
            .changed();
        ui.label(RichText::new("Out").color(TEXT_DIM));
        changed |= ui
            .add(egui::DragValue::new(&mut t.end).range(t.start..=99_999))
            .on_hover_text("The last frame played (right-click the ruler to set it)")
            .changed();
        if changed {
            app.canvas_mut().timeline = t;
            app.mark_unsaved();
        }
        vdivider(ui);
        let mut onion = app.canvas.onion;
        let mut onion_changed = false;
        if ui
            .selectable_label(onion.enabled, "Onion skin")
            .on_hover_text(tip(
                app,
                &ctx,
                "Show the drawings before (red) and after (green) faintly",
                Action::ToggleOnion,
            ))
            .clicked()
        {
            onion.enabled = !onion.enabled;
            onion_changed = true;
        }
        ui.menu_button("⚙", |ui| {
            ui.set_min_width(180.0);
            onion_changed |= ui.checkbox(&mut onion.enabled, "Onion skin").changed();
            onion_changed |= ui
                .add(BarSlider::new(&mut onion.before, 0..=5).text("before"))
                .changed();
            onion_changed |= ui
                .add(BarSlider::new(&mut onion.after, 0..=5).text("after"))
                .changed();
            onion_changed |= ui
                .add(BarSlider::new(&mut onion.opacity, 0.05..=1.0).text("opacity"))
                .changed();
        })
        .response
        .on_hover_text("Onion skin settings");
        if onion_changed {
            app.canvas_mut().onion = onion;
            app.canvas.pose_motions();
            app.mark_all_tiles_dirty();
        }
        vdivider(ui);
        ui.spacing_mut().item_spacing.x = 2.0;
        let active = app.canvas.active_layer_idx;
        let track = app.active_track();
        let starts_here =
            track.is_some_and(|t| app.canvas.frames_of(t).iter().any(|(at, _)| *at == time));
        let drawable = app.canvas.layers.get(active).is_some_and(|l| {
            active != 0 && (l.anim.is_some() || (l.kind == LayerKind::Paint && l.rig.is_none()))
        });
        let new_tip = if track.is_some() {
            tip(app, &ctx, "New blank drawing here", Action::NewDrawing)
        } else {
            tip(
                app,
                &ctx,
                "Start drawing frames on this layer: a new drawing here",
                Action::NewDrawing,
            )
        };
        if ui
            .add_enabled_ui(drawable && !starts_here, |ui| {
                icon_button(ui, Icon::Plus, b, false, &new_tip)
            })
            .inner
            .clicked()
        {
            app.drawing_at(active, time, false);
        }
        let showing = track.is_some_and(|t| app.canvas.frame_at(t, time).is_some());
        let copy_tip = tip(
            app,
            &ctx,
            "New drawing here, a copy of the one showing",
            Action::CopyDrawing,
        );
        if ui
            .add_enabled_ui(!starts_here && showing, |ui| {
                glyph_button(ui, b, Glyph::Duplicate, &copy_tip, false)
            })
            .inner
            .clicked()
        {
            app.drawing_at(active, time, true);
        }
        let remove_tip = tip(
            app,
            &ctx,
            "Remove the drawing starting here",
            Action::RemoveDrawing,
        );
        if ui
            .add_enabled_ui(starts_here, |ui| {
                icon_button(ui, Icon::Trash, b, false, &remove_tip)
            })
            .inner
            .clicked()
        {
            app.remove_drawing();
        }
        let target = app.motion_target(active);
        let key_tip = tip(
            app,
            &ctx,
            "Key the layer's position, scale, turn and opacity here",
            Action::KeyMotion,
        );
        if ui
            .add_enabled_ui(target.is_some(), |ui| {
                glyph_button(ui, b, Glyph::Key, &key_tip, false)
            })
            .inner
            .clicked()
            && let Some(target) = target
        {
            app.key_all_here(target);
            view.open.insert(app.canvas.layers[target].id);
        }
        ui.spacing_mut().item_spacing.x = 6.0;
        vdivider(ui);
        let layer_tip = tip(
            app,
            &ctx,
            "A new animated layer, to draw frames on",
            Action::NewAnimationLayer,
        );
        if ui
            .button("+ Animated layer")
            .on_hover_text(layer_tip)
            .clicked()
        {
            app.new_animation_layer();
        }
        let animate_on = matches!(app.active_tool, crate::app::tools::Tool::Animate);
        let animate_tip = tip(
            app,
            &ctx,
            "Animate tool: move, scale and turn the layer on the canvas, keyed here",
            Action::Animate,
        );
        if icon_button(ui, Icon::Motion, b, animate_on, &animate_tip).clicked() {
            app.active_tool = if animate_on {
                crate::app::tools::Tool::Brush
            } else {
                crate::app::tools::Tool::Animate
            };
        }
        if icon_button(
            ui,
            Icon::Sliders,
            b,
            view.inspector,
            "Inspector: the layer's motion and a key's easing",
        )
        .clicked()
        {
            view.inspector = !view.inspector;
        }
        vdivider(ui);
        if ui.button("Export…").clicked() {
            pick_export(app);
        }
    });
}

/// Frame `t` as minutes, seconds and frames.
fn timecode(t: u32, fps: u32) -> String {
    let fps = fps.max(1);
    let seconds = t / fps;
    format!("{:02}:{:02}:{:02}", seconds / 60, seconds % 60, t % fps)
}

#[derive(Clone, Copy)]
enum Glyph {
    First,
    PrevKey,
    PrevFrame,
    Play,
    Pause,
    NextFrame,
    NextKey,
    Last,
    Duplicate,
    Key,
}

/// A flat transport button, filled with the accent while `on`.
fn glyph_button(
    ui: &mut egui::Ui,
    size: f32,
    glyph: Glyph,
    tooltip: &str,
    on: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(size, size.min(30.0).max(size * 0.9)), Sense::click());
    let enabled = ui.is_enabled();
    let bg = if on {
        accent()
    } else if !enabled {
        Color32::TRANSPARENT
    } else if response.is_pointer_button_down_on() {
        WIDGET_ACTIVE
    } else if response.hovered() {
        WIDGET_HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    let fg = if !enabled {
        TEXT_DIM.gamma_multiply(0.6)
    } else if on || response.hovered() {
        TEXT_STRONG
    } else {
        TEXT
    };
    let inset = vec2(size * 0.27, size * 0.27);
    paint_glyph(
        ui.painter(),
        Rect::from_center_size(rect.center(), rect.size() - inset * 2.0),
        glyph,
        fg,
    );
    response.on_hover_text(tooltip)
}

fn paint_glyph(p: &egui::Painter, r: Rect, glyph: Glyph, c: Color32) {
    let (top, bottom, cy) = (r.top(), r.bottom(), r.center().y);
    // A triangle pointing from `base` (its vertical side) to `tip`.
    let tri = |tip: f32, base: f32| {
        let points = if tip > base {
            vec![pos2(base, top), pos2(tip, cy), pos2(base, bottom)]
        } else {
            vec![pos2(base, top), pos2(base, bottom), pos2(tip, cy)]
        };
        p.add(Shape::convex_polygon(points, c, Stroke::NONE));
    };
    let bar = |x: f32, w: f32| {
        p.rect_filled(Rect::from_x_y_ranges(x..=x + w, r.y_range()), 0.5, c);
    };
    let diamond = |x: f32| {
        let h = r.height() * 0.4;
        let points = vec![
            pos2(x, cy - h),
            pos2(x + h, cy),
            pos2(x, cy + h),
            pos2(x - h, cy),
        ];
        p.add(Shape::convex_polygon(points, c, Stroke::NONE));
    };
    let (l, rt) = (r.left(), r.right());
    match glyph {
        Glyph::Play => tri(rt, l + 1.0),
        Glyph::Pause => {
            bar(l + 1.5, 3.0);
            bar(rt - 4.5, 3.0);
        }
        Glyph::PrevFrame => {
            bar(l, 2.0);
            tri(l + 3.0, rt);
        }
        Glyph::NextFrame => {
            tri(rt - 3.0, l);
            bar(rt - 2.0, 2.0);
        }
        Glyph::First => {
            bar(l, 2.0);
            let mid = (l + 2.5 + rt) / 2.0;
            tri(l + 2.5, mid);
            tri(mid, rt);
        }
        Glyph::Last => {
            bar(rt - 2.0, 2.0);
            let mid = (l + rt - 2.5) / 2.0;
            tri(mid, l);
            tri(rt - 2.5, mid);
        }
        Glyph::PrevKey => {
            tri(l, l + 5.0);
            diamond(rt - r.height() * 0.4);
        }
        Glyph::NextKey => {
            diamond(l + r.height() * 0.4);
            tri(rt, rt - 5.0);
        }
        Glyph::Key => diamond(r.center().x),
        Glyph::Duplicate => {
            let stroke = Stroke::new(1.3_f32, c);
            let back = Rect::from_min_size(r.min, r.size() * 0.72);
            let front = Rect::from_min_size(r.max - r.size() * 0.72, r.size() * 0.72);
            p.rect_stroke(back, 1.0, stroke);
            p.rect_filled(front.expand(1.0), 1.0, BG_RAISED);
            p.rect_stroke(front, 1.0, stroke);
        }
    }
}

/// `track`'s drawings (start frame, layer index) as the edit under way
/// would leave them.
fn shown_frames(
    view: &TimelineView,
    track: LayerId,
    mut frames: Vec<(u32, usize)>,
) -> Vec<(u32, usize)> {
    match view.drag {
        Some(Drag::Move {
            track: t, from, to, ..
        }) if t == track && !frames.iter().any(|f| f.0 == to) => {
            for f in &mut frames {
                if f.0 == from {
                    f.0 = to;
                }
            }
            frames.sort_unstable();
        }
        Some(Drag::Hold {
            track: t, next, to, ..
        }) if t == track => {
            for f in &mut frames {
                if f.0 >= next {
                    f.0 = (f.0 as i64 + to as i64 - next as i64) as u32;
                }
            }
        }
        _ => {}
    }
    frames
}

/// The frames each drawing covers: from its start to (not including) the
/// next one's, the last held to the end of the range.
fn blocks(frames: &[(u32, usize)], end: u32) -> Vec<(u32, u32, usize)> {
    (frames.iter().enumerate())
        .map(|(k, &(at, i))| {
            let until = frames.get(k + 1).map_or((end + 1).max(at + 1), |n| n.0);
            (at, until, i)
        })
        .collect()
}

/// The frames with keys on row `row` (every property's on a layer's own
/// row), as the drag under way would leave them.
fn row_keys(app: &PainterApp, view: &TimelineView, row: &Row) -> Vec<u32> {
    let Some(motion) = app.canvas.layers[row.idx].motion.as_deref() else {
        return Vec::new();
    };
    let mut keys = match row.prop {
        Some(p) => motion.key_frames(p),
        None => motion.all_key_frames(),
    };
    if let Some(Drag::Key {
        layer,
        prop,
        from,
        to,
    }) = view.drag
        && layer == row.id
        && (prop == row.prop || prop.is_none() || row.prop.is_none())
    {
        for k in &mut keys {
            if *k == from {
                *k = to;
            }
        }
        keys.sort_unstable();
        keys.dedup();
    }
    keys
}

/// The curve of `row`'s key at frame `t` (a layer's own row: its first
/// property keyed there).
fn key_curve(app: &PainterApp, row: &Row, t: u32) -> Option<Curve> {
    let motion = app.canvas.layers[row.idx].motion.as_deref()?;
    let props: Vec<Prop> = row.prop.map_or(Prop::ALL.to_vec(), |p| vec![p]);
    props
        .into_iter()
        .find_map(|p| motion.key_at(p, t).map(|k| k.curve))
}

/// Go to frame `t` on layer `id`, selecting it (an animated layer: its
/// drawing there).
fn select(app: &mut PainterApp, id: LayerId, t: u32) {
    app.go_to_frame(t);
    if let Some(i) = app.canvas.frame_at(id, t) {
        app.canvas_mut().active_layer_idx = i;
    } else if let Some(i) = app.canvas.layer_index_of(id) {
        app.canvas_mut().active_layer_idx = i;
    }
}

/// The ruler, the layers' names and their rows.
fn body(app: &mut PainterApp, ui: &mut egui::Ui, view: &mut TimelineView, rect: Rect, s: Sizes) {
    if rect.width() < s.header + 40.0 || rect.height() < s.ruler + 16.0 {
        return;
    }
    let rows = layer_rows(app, view, s);
    if rows.is_empty() {
        empty_state(app, ui, rect);
        return;
    }
    let timeline = app.canvas.timeline;
    let fps = timeline.fps.max(1);
    let header = Rect::from_min_max(rect.min, pos2(rect.left() + s.header, rect.bottom()));
    let corner = Rect::from_min_max(header.min, pos2(header.right(), rect.top() + s.ruler));
    let ruler = Rect::from_min_max(
        pos2(header.right(), rect.top()),
        pos2(rect.right(), rect.top() + s.ruler),
    );
    let grid = Rect::from_min_max(
        pos2(header.right(), ruler.bottom()),
        pos2(rect.right(), rect.bottom() - s.scrollbar),
    );
    let bar = Rect::from_min_max(
        pos2(grid.left(), grid.bottom()),
        pos2(rect.right(), rect.bottom()),
    );
    let names = Rect::from_min_max(
        pos2(header.left(), grid.top()),
        pos2(header.right(), grid.bottom()),
    );
    view.grid = Some(grid);
    view.panel = Some(rect);
    if view.cell <= 0.0 {
        view.cell = s.cell;
    }
    // The layers' own rows, top first, and frames picked between two of
    // them.
    let mains: Vec<LayerId> = rows
        .iter()
        .filter(|r| r.prop.is_none())
        .map(|r| r.id)
        .collect();
    let main_of = |id: LayerId| mains.iter().position(|&m| m == id).unwrap_or(0);
    let pick = |(a, at): (usize, u32), (b, bt): (usize, u32)| FrameSelection {
        rows: mains[a.min(b)..=a.max(b).min(mains.len() - 1)].to_vec(),
        from: at.min(bt),
        to: at.max(bt),
    };
    let content_h = rows.last().map_or(0.0, |r| r.top + r.height);

    // Zoom (about the pointer) and scroll with the wheel or a pinch.
    if ui.rect_contains_pointer(rect) {
        let (zoom, delta, shift, pointer) = ui.input(|i| {
            (
                i.zoom_delta(),
                i.smooth_scroll_delta,
                i.modifiers.shift,
                i.pointer.hover_pos(),
            )
        });
        if zoom != 1.0 {
            let x = pointer.map_or(grid.center().x, |p| p.x).max(grid.left()) - grid.left();
            let at = (x + view.scroll.x) / view.cell;
            view.cell = (view.cell * zoom).clamp(MIN_CELL, MAX_CELL);
            view.scroll.x = at * view.cell - x;
        } else if delta != egui::Vec2::ZERO {
            let rows_overflow = content_h > grid.height();
            let delta = if delta.x == 0.0 && (shift || !rows_overflow) {
                vec2(delta.y, 0.0)
            } else {
                delta
            };
            view.scroll -= delta;
        }
    }
    let cell = view.cell;
    let time = app.canvas.time;
    let last_key = (rows.iter())
        .filter_map(|r| {
            let track_last = app.canvas.frames_of(r.id).last().map(|f| f.0);
            let key_last = app.canvas.layers[r.idx]
                .motion
                .as_ref()
                .and_then(|m| m.all_key_frames().last().copied());
            track_last.max(key_last)
        })
        .max()
        .unwrap_or(0);
    let content_w = (timeline.end.max(last_key).max(time) + 1 + 2 * fps) as f32 * cell;
    // The frame showing kept in view when it moves (not while scrubbing).
    if view.seen_time != Some(time) && !matches!(view.drag, Some(Drag::Scrub)) {
        let x = time as f32 * cell;
        if x < view.scroll.x || x + cell > view.scroll.x + grid.width() {
            view.scroll.x = x - grid.width() * 0.2;
        }
    }
    view.seen_time = Some(time);
    view.scroll.x = view
        .scroll
        .x
        .clamp(0.0, (content_w - grid.width()).max(0.0));
    view.scroll.y = view
        .scroll
        .y
        .clamp(0.0, (content_h - grid.height()).max(0.0));
    let (sx, sy) = (view.scroll.x, view.scroll.y);
    let x_of = |t: u32| grid.left() + t as f32 * cell - sx;
    let frame_at = |x: f32| ((x - grid.left() + sx) / cell).floor().max(0.0) as u32;
    let row_rect = |r: &Row| {
        Rect::from_min_size(
            pos2(grid.left(), grid.top() + r.top - sy),
            vec2(grid.width(), r.height),
        )
    };
    let head_rect = |r: &Row| {
        Rect::from_min_size(
            pos2(header.left(), grid.top() + r.top - sy),
            vec2(s.header, r.height),
        )
    };
    // The layer whose rows are at height `y`.
    let main_at_y = |y: f32| {
        rows.iter()
            .find(|r| row_rect(r).y_range().contains(y))
            .map(|r| main_of(r.id))
    };
    // Where a key's diamond sits on a row.
    let diamond_at = |r: &Row, t: u32| {
        let rr = row_rect(r);
        let y = if r.prop.is_some() {
            rr.center().y
        } else {
            rr.bottom() - 7.0
        };
        pos2(x_of(t) + cell / 2.0, y)
    };
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let released = ui.input(|i| i.pointer.any_released());
    // Pulled along while a drag nears either side.
    let edge_scroll = |x: f32, view: &mut TimelineView| {
        if x > grid.right() - 12.0 {
            view.scroll.x += cell * 0.5;
        } else if x < grid.left() + 12.0 {
            view.scroll.x -= cell * 0.5;
        }
        ui.ctx().request_repaint();
    };

    // The ruler: press or drag to scrub; right-click to set the range.
    let ruler_response = ui.interact(ruler, ui.id().with("tl_ruler"), Sense::click_and_drag());
    if ruler_response.is_pointer_button_down_on()
        && ui.input(|i| i.pointer.primary_down())
        && let Some(p) = pointer
    {
        view.drag = Some(Drag::Scrub);
        app.go_to_frame(frame_at(p.x));
        edge_scroll(p.x, view);
    } else if matches!(view.drag, Some(Drag::Scrub)) {
        view.drag = None;
    }
    if ruler_response.secondary_clicked()
        && let Some(p) = pointer
    {
        view.menu = Some(Menu::Ruler(frame_at(p.x)));
    }
    ruler_response.context_menu(|ui| {
        let Some(Menu::Ruler(t)) = view.menu else {
            ui.close_menu();
            return;
        };
        ui.label(RichText::new(format!("Frame {t}")).color(TEXT_DIM));
        let mut range = app.canvas.timeline;
        if ui
            .add_enabled(t <= range.end, egui::Button::new("Play from here"))
            .clicked()
        {
            range.start = t;
        }
        if ui
            .add_enabled(t >= range.start, egui::Button::new("Play up to here"))
            .clicked()
        {
            range.end = t;
        }
        if ui.button("Play everything").clicked() {
            range.start = 0;
            range.end = last_key.max(1);
        }
        if range != app.canvas.timeline {
            app.canvas_mut().timeline = range;
            app.mark_unsaved();
            ui.close_menu();
        }
    });

    // The scrollbar.
    let thumb_w = (grid.width() / content_w.max(1.0) * bar.width()).clamp(24.0, bar.width());
    let thumb_x = bar.left() + sx / content_w.max(1.0) * bar.width();
    let thumb = Rect::from_min_size(
        pos2(thumb_x, bar.top() + 2.0),
        vec2(thumb_w, bar.height() - 4.0),
    );
    let bar_response = ui.interact(bar, ui.id().with("tl_scrollbar"), Sense::drag());
    if bar_response.drag_started()
        && let Some(p) = ui.input(|i| i.pointer.press_origin())
    {
        let grab = if thumb.x_range().contains(p.x) {
            p.x - thumb.left()
        } else {
            thumb_w / 2.0
        };
        view.drag = Some(Drag::Scrollbar { grab });
    }
    if let Some(Drag::Scrollbar { grab }) = view.drag {
        if let Some(p) = pointer {
            view.scroll.x = (p.x - grab - bar.left()) / bar.width() * content_w;
        }
        if released {
            view.drag = None;
        }
    }

    // The names: the triangle opens a row's keys, the eye hides it, a
    // click selects it; on a property's row the diamond keys it here.
    for r in &rows {
        let row = head_rect(r);
        if !row.intersects(names) {
            continue;
        }
        let indent = r.depth as f32 * 12.0;
        if let Some(p) = r.prop {
            let toggle = Rect::from_center_size(
                pos2(row.left() + 30.0 + indent, row.center().y),
                vec2(18.0, 18.0),
            );
            let response = ui.interact(
                toggle.intersect(names),
                ui.id().with(("tl_key_toggle", r.id, p as u8)),
                Sense::click(),
            );
            if response
                .on_hover_text(format!(
                    "Key {} here, or take its key away",
                    p.name().to_lowercase()
                ))
                .clicked()
            {
                let keyed = (app.canvas.layers[r.idx].motion.as_ref())
                    .is_some_and(|m| m.key_at(p, time).is_some());
                if keyed {
                    app.remove_key(r.idx, p, time);
                } else {
                    let v = app.motion_value(r.idx, p);
                    app.motion_step(r.idx, |m| m.set(p, time, v));
                }
            }
            continue;
        }
        let arrow = Rect::from_center_size(
            pos2(row.left() + 12.0 + indent, row.center().y),
            vec2(18.0, 22.0),
        );
        let eye = Rect::from_center_size(
            pos2(row.left() + 32.0 + indent, row.center().y),
            vec2(20.0, 20.0),
        );
        let head = ui.interact(
            row.intersect(names),
            ui.id().with(("tl_head", r.id)),
            Sense::click(),
        );
        let arrow_response = ui.interact(
            arrow.intersect(names),
            ui.id().with(("tl_open", r.id)),
            Sense::click(),
        );
        let eye_response = ui.interact(
            eye.intersect(names),
            ui.id().with(("tl_eye", r.id)),
            Sense::click(),
        );
        if arrow_response
            .on_hover_text("Show the keyed properties")
            .clicked()
        {
            if !view.open.remove(&r.id) {
                view.open.insert(r.id);
            }
        } else if eye_response.on_hover_text("Show / hide").clicked() {
            let layer = &mut app.canvas_mut().layers[r.idx];
            layer.visible = !layer.visible;
            app.mark_all_tiles_dirty();
        } else if head.clicked() {
            let t = app.canvas.time;
            select(app, r.id, t);
        } else if head.double_clicked() && r.kind == Kind::Folder {
            let layer = &mut app.canvas_mut().layers[r.idx];
            layer.expanded = !layer.expanded;
        }
    }

    // The rows: click, drag, double-click and right-click on frames and keys.
    for r in &rows {
        let rr = row_rect(r);
        let area = rr.intersect(grid);
        if area.height() <= 0.0 {
            continue;
        }
        let salt = (r.id, r.prop.map_or(9, |p| p as u8));
        let response = ui.interact(
            area,
            ui.id().with(("tl_row", salt)),
            Sense::click_and_drag(),
        );
        let keys = row_keys(app, &TimelineView::default(), r);
        let key_near = |p: Pos2| {
            keys.iter()
                .copied()
                .find(|&k| (diamond_at(r, k) - p).length() <= if s.touch { 14.0 } else { 7.0 })
        };
        let frames = if r.kind == Kind::Track && r.prop.is_none() {
            app.canvas.frames_of(r.id)
        } else {
            Vec::new()
        };
        let spans = blocks(&frames, timeline.end);
        // The hold whose end is at `x` (its start and the next drawing's).
        let edge_at = |x: f32| {
            (spans.iter())
                .zip(frames.iter().skip(1))
                .find(|((at, until, _), _)| {
                    (x - x_of(*until)).abs() <= EDGE_GRAB && x > x_of(*at) + 4.0
                })
                .map(|((at, _, _), next)| (*at, next.0))
        };
        let block_at = |t: u32| {
            spans
                .iter()
                .find(|(at, until, _)| (*at..*until).contains(&t))
                .map(|b| b.0)
        };
        if let Some(p) = response.hover_pos() {
            let icon = if key_near(p).is_some() || matches!(view.drag, Some(Drag::Key { .. })) {
                Some(egui::CursorIcon::Grab)
            } else if edge_at(p.x).is_some() || matches!(view.drag, Some(Drag::Hold { .. })) {
                Some(egui::CursorIcon::ResizeHorizontal)
            } else if matches!(view.drag, Some(Drag::Move { .. })) {
                Some(egui::CursorIcon::Grabbing)
            } else {
                block_at(frame_at(p.x)).map(|_| egui::CursorIcon::Grab)
            };
            if let Some(icon) = icon {
                ui.ctx().set_cursor_icon(icon);
            }
        }
        if response.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            let here = (main_of(r.id), frame_at(p.x));
            let in_selection = view
                .sel
                .as_ref()
                .is_some_and(|sel| sel.contains(r.id, here.1));
            if ui.input(|i| i.modifiers.shift) {
                view.drag = Some(Drag::Select {
                    row: here.0,
                    t: here.1,
                });
                view.sel = Some(pick(here, here));
            } else if let Some(k) = key_near(p) {
                view.picked = Some((r.id, r.prop, k));
                view.drag = Some(Drag::Key {
                    layer: r.id,
                    prop: r.prop,
                    from: k,
                    to: k,
                });
            } else if let Some((start, next)) = edge_at(p.x) {
                view.drag = Some(Drag::Hold {
                    track: r.id,
                    start,
                    next,
                    to: next,
                });
            } else if in_selection {
                view.drag = Some(Drag::Range {
                    from: here.1,
                    to: here.1,
                });
            } else if let Some(at) = block_at(frame_at(p.x)) {
                select(app, r.id, at);
                let offset = frame_at(p.x) as i64 - at as i64;
                view.drag = Some(Drag::Move {
                    track: r.id,
                    from: at,
                    offset,
                    to: at,
                });
            } else if s.touch {
                view.drag = Some(Drag::Pan);
            } else {
                // Over empty frames: a box picking frames.
                view.drag = Some(Drag::Select {
                    row: here.0,
                    t: here.1,
                });
                view.sel = Some(pick(here, here));
            }
        }
        if response.dragged()
            && let Some(p) = pointer
        {
            match view.drag {
                Some(Drag::Move {
                    track,
                    from,
                    offset,
                    ..
                }) if track == r.id => {
                    let to = (frame_at(p.x) as i64 - offset).max(0) as u32;
                    view.drag = Some(Drag::Move {
                        track,
                        from,
                        offset,
                        to,
                    });
                    edge_scroll(p.x, view);
                }
                Some(Drag::Hold {
                    track, start, next, ..
                }) if track == r.id => {
                    let nearest = ((p.x - grid.left() + sx) / cell).round().max(0.0) as u32;
                    let to = nearest.max(start + 1);
                    view.drag = Some(Drag::Hold {
                        track,
                        start,
                        next,
                        to,
                    });
                    edge_scroll(p.x, view);
                }
                Some(Drag::Key {
                    layer, prop, from, ..
                }) if layer == r.id && prop == r.prop => {
                    let to = frame_at(p.x);
                    view.drag = Some(Drag::Key {
                        layer,
                        prop,
                        from,
                        to,
                    });
                    edge_scroll(p.x, view);
                }
                Some(Drag::Pan) => {
                    view.scroll -= response.drag_delta();
                }
                Some(Drag::Select { row, t }) => {
                    if let Some(b) = main_at_y(p.y.clamp(grid.top() + 1.0, grid.bottom() - 1.0)) {
                        view.sel = Some(pick((row, t), (b, frame_at(p.x))));
                    }
                    edge_scroll(p.x, view);
                }
                Some(Drag::Range { from, .. }) => {
                    view.drag = Some(Drag::Range {
                        from,
                        to: frame_at(p.x),
                    });
                    edge_scroll(p.x, view);
                }
                _ => {}
            }
        }
        if response.drag_stopped() {
            match view.drag.take() {
                Some(Drag::Move {
                    track, from, to, ..
                }) if track == r.id && to != from => {
                    select(app, r.id, from);
                    app.move_drawing(to);
                }
                Some(Drag::Hold {
                    track, next, to, ..
                }) if track == r.id && to != next => {
                    app.shift_drawings(r.id, next, to as i32 - next as i32);
                }
                Some(Drag::Key {
                    layer,
                    prop,
                    from,
                    to,
                }) if layer == r.id && to != from => {
                    let idx = r.idx;
                    app.motion_step(idx, |m| {
                        let props: Vec<Prop> = prop.map_or(Prop::ALL.to_vec(), |p| vec![p]);
                        // All of them or none: no key lands on another.
                        if props
                            .iter()
                            .all(|&p| m.key_at(p, from).is_none() || m.key_at(p, to).is_none())
                        {
                            for p in props {
                                m.move_key(p, from, to);
                            }
                        }
                    });
                    view.picked = Some((layer, prop, to));
                    app.go_to_frame(to);
                }
                Some(Drag::Range { from, to }) if to != from => {
                    if let Some(sel) = view.sel.clone() {
                        view.sel = app.move_frames(&sel, to as i64 - from as i64).or(Some(sel));
                    }
                }
                _ => {}
            }
        }
        let t_here = response.interact_pointer_pos().map(|p| frame_at(p.x));
        let key_here = response.interact_pointer_pos().and_then(key_near);
        if response.double_clicked()
            && let Some(t) = t_here
        {
            match (r.prop, r.kind) {
                (Some(p), _) => {
                    let v = (app.canvas.layers[r.idx].motion.as_ref())
                        .map_or(p.rest(), |m| m.value(p, t as f32));
                    app.motion_step(r.idx, |m| m.set(p, t, v));
                    view.picked = Some((r.id, Some(p), t));
                    app.go_to_frame(t);
                }
                (None, Kind::Track | Kind::Plain) if !frames.iter().any(|f| f.0 == t) => {
                    app.drawing_at(r.idx, t, false);
                    app.go_to_frame(t);
                }
                _ => {}
            }
        } else if response.clicked()
            && let Some(t) = t_here
        {
            // Shift+click: the frames from the last click to here.
            let anchor = view.anchor.filter(|_| ui.input(|i| i.modifiers.shift));
            if let Some((row, at)) = anchor {
                view.sel = Some(pick((main_of(row), at), (main_of(r.id), t)));
            } else {
                view.sel = None;
                view.anchor = Some((r.id, t));
                select(app, r.id, key_here.unwrap_or(t));
                view.picked = key_here.map(|k| (r.id, r.prop, k));
                if key_here.is_some() {
                    view.inspector = true;
                }
            }
        }
        if response.secondary_clicked()
            && let Some(t) = t_here
        {
            view.menu = Some(match key_here {
                Some(k) => Menu::Key(r.id, r.prop, k),
                None => Menu::Frame(r.id, t),
            });
        }
        response.context_menu(|ui| match view.menu {
            Some(Menu::Key(id, prop, k)) if id == r.id && prop == r.prop => {
                key_menu(app, ui, r, prop, k);
            }
            Some(Menu::Frame(id, t)) if id == r.id => frame_menu(app, ui, view, r, t, &mains),
            _ => ui.close_menu(),
        });
    }

    // Painting: the names' column...
    let thumbs = &app.layer_state.thumbnails;
    let active = app.canvas.active_layer_idx;
    let active_row = app
        .motion_target(active)
        .or(Some(active))
        .map(|i| app.canvas.layers[i].id);
    let active_track = app.active_track();
    let painter = ui.painter_at(rect);
    painter.rect_filled(header, 0.0, BG_PANEL);
    painter.rect_filled(ruler, 0.0, Color32::from_gray(40));
    painter.rect_filled(grid, 0.0, BG_INSET);
    painter.rect_filled(bar, 0.0, BG_PANEL);
    let head_painter = ui.painter_at(names);
    for r in &rows {
        let row = head_rect(r);
        if !row.intersects(names) {
            continue;
        }
        let layer = &app.canvas.layers[r.idx];
        let selected = active_track == Some(r.id) || active_row == Some(r.id);
        let hovered = pointer.is_some_and(|p| row.contains(p) && names.contains(p));
        let indent = r.depth as f32 * 12.0;
        if let Some(p) = r.prop {
            head_painter.rect_filled(
                row,
                0.0,
                if hovered {
                    BG_RAISED
                } else {
                    Color32::from_gray(32)
                },
            );
            let motion = layer.motion.as_deref();
            let keyed = motion.is_some_and(|m| m.key_at(p, time).is_some());
            let has = motion.is_some_and(|m| !m.keys(p).is_empty());
            let c = pos2(row.left() + 30.0 + indent, row.center().y);
            let edge = if keyed {
                accent()
            } else if has {
                TEXT
            } else {
                TEXT_DIM
            };
            paint_diamond(&head_painter, c, 5.0, keyed.then_some(accent()), edge);
            head_painter.text(
                pos2(row.left() + 44.0 + indent, row.center().y),
                egui::Align2::LEFT_CENTER,
                p.name(),
                egui::FontId::proportional(12.0),
                if has { TEXT } else { TEXT_DIM },
            );
            let v = motion.map_or(p.rest(), |m| m.value(p, time as f32));
            let value = match p {
                Prop::Position => format!("{:.0}, {:.0}", v[0], v[1]),
                Prop::Scale if (v[0] - v[1]).abs() < 1e-3 => format!("{:.0}%", v[0] * 100.0),
                Prop::Scale => format!("{:.0}×{:.0}%", v[0] * 100.0, v[1] * 100.0),
                Prop::Rotation => format!("{:.1}°", v[0]),
                Prop::Opacity
                | Prop::Brightness
                | Prop::Contrast
                | Prop::Saturation
                | Prop::Tint => {
                    format!("{:.0}%", v[0] * 100.0)
                }
                Prop::Anchor => format!("{:.0}, {:.0}", v[0], v[1]),
                Prop::Blur => format!("{:.0} px", v[0]),
                Prop::Hue => format!("{:.0}°", v[0]),
            };
            head_painter.text(
                pos2(row.right() - 8.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                value,
                egui::FontId::monospace(11.0),
                TEXT_DIM,
            );
            head_painter.hline(
                row.x_range(),
                row.bottom() - 0.5,
                Stroke::new(1.0_f32, Color32::from_gray(30)),
            );
            continue;
        }
        let fill = if selected {
            accent_dim()
        } else if hovered {
            BG_RAISED
        } else {
            BG_PANEL
        };
        head_painter.rect_filled(row, 0.0, fill);
        if let Some(&tag) = app.layer_state.layer_ui_colors.get(r.idx)
            && tag != UNTAGGED
        {
            head_painter.rect_filled(
                Rect::from_min_size(row.min, vec2(3.0, row.height())),
                0.0,
                tag,
            );
        }
        // The triangle: open (pointing down) or closed.
        let open =
            view.open.contains(&r.id) || rows.iter().any(|o| o.id == r.id && o.prop.is_some());
        let a = pos2(row.left() + 12.0 + indent, row.center().y);
        let has_keys = layer.motion.as_ref().is_some_and(|m| !m.is_still());
        let arrow_color = if has_keys { TEXT } else { TEXT_DIM };
        let tri = if open {
            vec![
                a + vec2(-4.0, -2.5),
                a + vec2(4.0, -2.5),
                a + vec2(0.0, 3.5),
            ]
        } else {
            vec![
                a + vec2(-2.5, -4.0),
                a + vec2(3.5, 0.0),
                a + vec2(-2.5, 4.0),
            ]
        };
        head_painter.add(Shape::convex_polygon(tri, arrow_color, Stroke::NONE));
        let eye = Rect::from_center_size(
            pos2(row.left() + 32.0 + indent, row.center().y),
            vec2(16.0, 16.0),
        );
        let (icon, colour) = if layer.visible {
            (Icon::Eye, TEXT)
        } else {
            (Icon::EyeOff, TEXT_DIM)
        };
        paint_icon(&head_painter, eye, icon, colour);
        let text_left = row.left() + 48.0 + indent;
        let name_painter = head_painter.with_clip_rect(names.intersect(Rect::from_x_y_ranges(
            text_left..=row.right() - 6.0,
            row.y_range(),
        )));
        name_painter.text(
            pos2(text_left, row.center().y - 7.0),
            egui::Align2::LEFT_CENTER,
            &layer.name,
            egui::FontId::proportional(13.0),
            if selected { TEXT_STRONG } else { TEXT },
        );
        let detail = match r.kind {
            Kind::Track => {
                let count = app.canvas.frames_of(r.id).len();
                format!("{count} drawing{}", if count == 1 { "" } else { "s" })
            }
            Kind::Plain => "Picture".to_string(),
            Kind::Folder => "Folder".to_string(),
            Kind::Rig => "Rig".to_string(),
        };
        let detail = if has_keys {
            format!("{detail} · keyed")
        } else {
            detail
        };
        name_painter.text(
            pos2(text_left, row.center().y + 9.0),
            egui::Align2::LEFT_CENTER,
            detail,
            egui::FontId::proportional(11.0),
            TEXT_DIM,
        );
        head_painter.hline(
            row.x_range(),
            row.bottom() - 0.5,
            Stroke::new(1.0_f32, BORDER),
        );
    }

    // ...the frames: a stronger line each second, the frame showing lit,
    // each drawing a block, each key a diamond...
    let gp = ui.painter_at(grid);
    let (t0, t1) = (frame_at(grid.left()), frame_at(grid.right()) + 1);
    for r in &rows {
        let rr = row_rect(r);
        if !rr.intersects(grid) {
            continue;
        }
        if r.prop.is_some() {
            gp.rect_filled(rr, 0.0, Color32::from_gray(25));
        } else if active_track == Some(r.id) || active_row == Some(r.id) {
            gp.rect_filled(rr, 0.0, Color32::from_gray(33));
        }
    }
    for t in t0..=t1 {
        let x = x_of(t);
        if t % fps == 0 {
            gp.vline(
                x,
                grid.y_range(),
                Stroke::new(1.0_f32, Color32::from_gray(52)),
            );
        } else if cell >= 8.0 {
            gp.vline(
                x,
                grid.y_range(),
                Stroke::new(1.0_f32, Color32::from_gray(34)),
            );
        }
    }
    let now = Rect::from_x_y_ranges(x_of(time)..=x_of(time + 1), grid.y_range());
    gp.rect_filled(now, 0.0, accent().gamma_multiply(0.14));
    for r in &rows {
        let rr = row_rect(r);
        if !rr.intersects(grid) {
            continue;
        }
        if r.prop.is_none() {
            let inner = Rect::from_min_max(
                pos2(rr.left(), rr.top() + 4.0),
                pos2(rr.right(), rr.bottom() - 4.0),
            );
            match r.kind {
                Kind::Track => {
                    let frames = shown_frames(view, r.id, app.canvas.frames_of(r.id));
                    for (at, until, i) in blocks(&frames, timeline.end) {
                        if until <= t0 || at > t1 {
                            continue;
                        }
                        let block = Rect::from_min_max(
                            pos2(x_of(at) + 1.0, inner.top()),
                            pos2(x_of(until) - 1.0, inner.bottom()),
                        );
                        let hovered =
                            view.drag.is_none() && pointer.is_some_and(|p| block.contains(p));
                        let thumb = thumbs.get(i).and_then(|t| t.as_ref());
                        let clip = gp.with_clip_rect(block.intersect(grid));
                        if app.canvas.is_blank_drawing(i) && i != active {
                            paint_empty(&clip, block);
                        } else {
                            paint_block(&clip, block, thumb, i == active, hovered);
                        }
                    }
                }
                Kind::Plain | Kind::Folder | Kind::Rig => {
                    let span = Rect::from_min_max(
                        pos2(x_of(0) + 1.0, inner.top()),
                        pos2(x_of(timeline.end.max(last_key) + 1) - 1.0, inner.bottom()),
                    );
                    let thumb = (r.kind != Kind::Folder)
                        .then(|| thumbs.get(r.idx).and_then(|t| t.as_ref()))
                        .flatten();
                    paint_still(
                        &gp.with_clip_rect(span.intersect(grid)),
                        span,
                        thumb,
                        active == r.idx,
                        r.kind,
                    );
                }
            }
        }
        // Keys: on a property's row, a line between them.
        let keys = row_keys(app, view, r);
        if r.prop.is_some() && keys.len() > 1 {
            let y = rr.center().y;
            gp.line_segment(
                [
                    pos2(x_of(keys[0]) + cell / 2.0, y),
                    pos2(x_of(keys[keys.len() - 1]) + cell / 2.0, y),
                ],
                Stroke::new(1.0_f32, Color32::from_gray(70)),
            );
        }
        for &k in &keys {
            if k + 1 < t0 || k > t1 {
                continue;
            }
            let c = diamond_at(r, k);
            let picked = view.picked == Some((r.id, r.prop, k));
            let size = if r.prop.is_some() { 5.5 } else { 4.5 };
            let curve = key_curve(app, r, k).unwrap_or(Curve::Linear);
            let (fill, edge) = if picked {
                (accent(), TEXT_STRONG)
            } else {
                (Color32::from_gray(200), Color32::BLACK)
            };
            paint_key(&gp, c, size, curve, fill, edge);
        }
        gp.hline(
            grid.x_range(),
            rr.bottom() - 0.5,
            Stroke::new(
                1.0_f32,
                if r.prop.is_some() {
                    Color32::from_gray(30)
                } else {
                    BORDER
                },
            ),
        );
    }
    // ...the frames picked (where they'd go, while they're dragged)...
    if let Some(sel) = &view.sel {
        let shift = match view.drag {
            Some(Drag::Range { from, to }) => to as i64 - from as i64,
            _ => 0,
        };
        let x0 = x_of((sel.from as i64 + shift).max(0) as u32);
        let x1 = x_of((sel.to as i64 + shift).max(0) as u32 + 1);
        for r in rows.iter().filter(|r| sel.rows.contains(&r.id)) {
            let area =
                Rect::from_x_y_ranges(x0..=x1, row_rect(r).y_range()).shrink2(vec2(0.0, 1.0));
            gp.rect_filled(area, 2.0, accent().gamma_multiply(0.18));
            gp.rect_stroke(
                area,
                2.0,
                Stroke::new(1.0_f32, accent().gamma_multiply(0.8)),
            );
        }
    }
    // ...the frames outside the range played dimmed...
    let dim = Color32::from_black_alpha(90);
    let before = Rect::from_x_y_ranges(grid.left()..=x_of(timeline.start), grid.y_range());
    let after = Rect::from_x_y_ranges(x_of(timeline.end + 1)..=grid.right(), grid.y_range());
    for r in [before, after] {
        if r.width() > 0.0 {
            gp.rect_filled(r, 0.0, dim);
        }
    }

    // ...the ruler: frame numbers, the range, the playhead...
    let rp = ui.painter_at(ruler);
    let range = Rect::from_x_y_ranges(
        x_of(timeline.start)..=x_of(timeline.end + 1),
        ruler.y_range(),
    );
    rp.rect_filled(range, 0.0, Color32::from_gray(52));
    let step = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000]
        .into_iter()
        .find(|st| *st as f32 * cell >= 30.0)
        .unwrap_or(1000);
    for t in t0..=t1 {
        let x = x_of(t);
        if cell >= 5.0 || t % fps == 0 {
            let h = if t % fps == 0 { 8.0 } else { 4.0 };
            rp.vline(
                x,
                (ruler.bottom() - h)..=ruler.bottom(),
                Stroke::new(1.0_f32, BORDER_LIGHT),
            );
        }
        if t % step == 0 {
            let at = if cell >= 18.0 {
                x + cell / 2.0
            } else {
                x + 3.0
            };
            let align = if cell >= 18.0 {
                egui::Align2::CENTER_CENTER
            } else {
                egui::Align2::LEFT_CENTER
            };
            rp.text(
                pos2(at, ruler.center().y - 2.0),
                align,
                t.to_string(),
                egui::FontId::proportional(10.5),
                TEXT_DIM,
            );
        }
    }
    // Frames ready to play from memory: a green line under them.
    let cache = &app.workspace.animation.cache;
    for t in t0.max(timeline.start)..=t1.min(timeline.end) {
        if cache.has(t) {
            let y = ruler.bottom() - 1.5;
            rp.hline(
                x_of(t)..=x_of(t + 1),
                y,
                Stroke::new(3.0_f32, Color32::from_rgb(70, 190, 110)),
            );
        }
    }
    for x in [x_of(timeline.start), x_of(timeline.end + 1)] {
        rp.vline(x, ruler.y_range(), Stroke::new(2.0_f32, accent_dim()));
    }
    rp.hline(
        ruler.x_range(),
        ruler.bottom() - 0.5,
        Stroke::new(1.0_f32, BORDER),
    );
    let head_x = x_of(time) + cell / 2.0;
    let label = time.to_string();
    let tag_w = (label.len() as f32 * 7.0 + 10.0).max(cell.min(28.0));
    let tag = Rect::from_center_size(
        pos2(head_x, ruler.center().y - 1.0),
        vec2(tag_w, s.ruler - 8.0),
    );
    rp.rect_filled(tag, 3.0, accent());
    rp.add(Shape::convex_polygon(
        vec![
            pos2(head_x - 4.0, tag.bottom()),
            pos2(head_x + 4.0, tag.bottom()),
            pos2(head_x, tag.bottom() + 4.0),
        ],
        accent(),
        Stroke::NONE,
    ));
    rp.text(
        tag.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        TEXT_STRONG,
    );
    gp.vline(head_x, grid.y_range(), Stroke::new(1.5_f32, accent()));

    // ...the scrollbar, and the zoom over the names.
    let bp = ui.painter_at(bar);
    let thumb_fill = if matches!(view.drag, Some(Drag::Scrollbar { .. })) || bar_response.hovered()
    {
        WIDGET_ACTIVE
    } else {
        WIDGET
    };
    if thumb_w < bar.width() {
        bp.rect_filled(thumb, 3.0, thumb_fill);
    }
    painter.rect_filled(corner, 0.0, BG_PANEL);
    painter.hline(
        corner.x_range(),
        corner.bottom() - 0.5,
        Stroke::new(1.0_f32, BORDER),
    );
    painter.vline(
        header.right() - 0.5,
        rect.y_range(),
        Stroke::new(1.0_f32, BORDER),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(corner.shrink2(vec2(8.0, 2.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.label(RichText::new("Zoom").small().color(TEXT_DIM));
            ui.spacing_mut().slider_width = ui.available_width() - 4.0;
            ui.add(
                BarSlider::new(&mut view.cell, MIN_CELL..=MAX_CELL)
                    .logarithmic(true)
                    .show_value(false),
            )
            .on_hover_text("Zoom (Ctrl + wheel, or pinch)");
        },
    );
}

/// A key: a diamond (linear), a circle (eased) or a square (held).
fn paint_key(p: &egui::Painter, c: Pos2, r: f32, curve: Curve, fill: Color32, edge: Color32) {
    let stroke = Stroke::new(1.0_f32, edge);
    match curve {
        Curve::Linear => paint_diamond(p, c, r + 0.5, Some(fill), edge),
        Curve::Bezier(_) => {
            p.circle_filled(c, r, fill);
            p.circle_stroke(c, r, stroke);
        }
        Curve::Stepped => {
            let rect = Rect::from_center_size(c, vec2(r * 1.6, r * 1.6));
            p.rect_filled(rect, 1.0, fill);
            p.rect_stroke(rect, 1.0, stroke);
        }
    }
}

/// A drawing's block: its thumbnail on paper, then a line along the frames
/// it's held.
fn paint_block(
    p: &egui::Painter,
    block: Rect,
    thumb: Option<&egui::TextureHandle>,
    active: bool,
    hovered: bool,
) {
    let (fill, edge) = if active {
        (accent_dim(), accent())
    } else if hovered {
        (Color32::from_gray(72), Color32::from_gray(96))
    } else {
        (Color32::from_gray(58), Color32::from_gray(74))
    };
    p.rect_filled(block, 3.0, fill);
    p.rect_stroke(
        block,
        3.0,
        Stroke::new(if active { 1.5_f32 } else { 1.0 }, edge),
    );
    let picture_end = paint_thumb(p, block, thumb, active);
    let inner = block.shrink(3.0);
    let (a, b) = (picture_end + 5.0, inner.right() - 2.0);
    if b - a > 4.0 {
        let y = block.center().y;
        let stroke = Stroke::new(
            1.5_f32,
            if active {
                accent()
            } else {
                Color32::from_gray(150)
            },
        );
        p.line_segment([pos2(a, y), pos2(b, y)], stroke);
        p.line_segment([pos2(b, y - 4.0), pos2(b, y + 4.0)], stroke);
    }
}

/// A thumbnail on paper at the left of `block`, fitted to its height.
/// Returns where it ends.
fn paint_thumb(
    p: &egui::Painter,
    block: Rect,
    thumb: Option<&egui::TextureHandle>,
    active: bool,
) -> f32 {
    let inner = block.shrink(3.0);
    if inner.width() < 6.0 {
        // Too narrow for a picture: a dot where it starts.
        p.circle_filled(block.center(), 2.5, if active { accent() } else { TEXT });
        return block.right();
    }
    let aspect = thumb.map_or(4.0 / 3.0, |t| {
        let s = t.size_vec2();
        s.x / s.y.max(1.0)
    });
    let mut size = vec2(inner.height() * aspect, inner.height());
    if size.x > inner.width() {
        size = vec2(inner.width(), inner.width() / aspect);
    }
    let picture = Rect::from_min_size(pos2(inner.left(), inner.center().y - size.y / 2.0), size);
    p.rect_filled(picture, 1.5, PAPER);
    if let Some(t) = thumb {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        p.image(t.id(), picture, uv, Color32::WHITE);
    }
    picture.right()
}

/// An empty exposure: the layer shows nothing for these frames.
fn paint_empty(p: &egui::Painter, block: Rect) {
    p.rect_stroke(block, 3.0, Stroke::new(1.0_f32, Color32::from_gray(64)));
    let mut x = block.left() - block.height();
    while x < block.right() {
        p.line_segment(
            [
                pos2(x, block.bottom()),
                pos2(x + block.height(), block.top()),
            ],
            Stroke::new(1.0_f32, Color32::from_gray(44)),
        );
        x += 8.0;
    }
    if block.width() > 50.0 {
        p.text(
            pos2(block.left() + 6.0, block.center().y),
            egui::Align2::LEFT_CENTER,
            "empty",
            egui::FontId::proportional(10.5),
            TEXT_DIM,
        );
    }
}

/// A layer that's the same every frame: one bar, its thumbnail at the
/// start.
fn paint_still(
    p: &egui::Painter,
    span: Rect,
    thumb: Option<&egui::TextureHandle>,
    active: bool,
    kind: Kind,
) {
    let fill = if active {
        Color32::from_gray(52)
    } else {
        Color32::from_gray(44)
    };
    p.rect_filled(span, 3.0, fill);
    p.rect_stroke(
        span,
        3.0,
        Stroke::new(
            1.0_f32,
            if active {
                accent_dim()
            } else {
                Color32::from_gray(58)
            },
        ),
    );
    let start = match kind {
        Kind::Folder => span.left() + 4.0,
        _ => paint_thumb(p, span, thumb, active),
    };
    let label = match kind {
        Kind::Folder => "Folder",
        Kind::Rig => "Rig",
        _ => "Same every frame: double-click a frame to draw frames here",
    };
    p.text(
        pos2(start + 8.0, span.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(11.0),
        TEXT_DIM,
    );
}

/// The right-click menu on frame `t` of row `r`.
fn frame_menu(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    view: &mut TimelineView,
    r: &Row,
    t: u32,
    mains: &[LayerId],
) {
    ui.label(
        RichText::new(format!("{} · frame {t}", app.canvas.layers[r.idx].name)).color(TEXT_DIM),
    );
    // The frames picked, and the frames copied.
    if let Some(sel) = view.sel.clone().filter(|sel| sel.contains(r.id, t)) {
        let what = format!(
            "{} frame{} × {} layer{}",
            sel.len(),
            if sel.len() == 1 { "" } else { "s" },
            sel.rows.len(),
            if sel.rows.len() == 1 { "" } else { "s" }
        );
        ui.label(RichText::new(what).small().color(TEXT_DIM));
        if ui.button("Copy frames  (Ctrl+C)").clicked() {
            app.copy_frames(&sel);
            ui.close_menu();
        }
        if ui.button("Cut frames  (Ctrl+X)").clicked() {
            app.cut_frames(&sel);
            ui.close_menu();
        }
        if ui.button("Delete frames  (Delete)").clicked() {
            app.delete_frames(&sel);
            ui.close_menu();
        }
        ui.separator();
    }
    if let Some(clip) = app.workspace.animation.clipboard.as_ref() {
        let (n, len) = (clip.rows.len(), clip.len);
        if ui
            .button("Paste frames here  (Ctrl+V)")
            .on_hover_text(
                "Over what's here, from this frame on this layer (and the ones under it)",
            )
            .clicked()
        {
            let k = mains.iter().position(|&m| m == r.id).unwrap_or(0);
            let rows: Vec<LayerId> = mains.iter().skip(k).take(n).copied().collect();
            app.paste_frames(&rows, t);
            view.sel = Some(FrameSelection {
                rows,
                from: t,
                to: t + len - 1,
            });
            ui.close_menu();
        }
        ui.separator();
    }
    if let Some(p) = r.prop {
        if ui
            .button(format!("Key {} here", p.name().to_lowercase()))
            .clicked()
        {
            let v = (app.canvas.layers[r.idx].motion.as_ref())
                .map_or(p.rest(), |m| m.value(p, t as f32));
            app.motion_step(r.idx, |m| m.set(p, t, v));
            ui.close_menu();
        }
        return;
    }
    let frames = app.canvas.frames_of(r.id);
    let starts_here = frames.iter().any(|f| f.0 == t);
    let start = frames.iter().rev().find(|f| f.0 <= t).map(|f| f.0);
    let next = frames.iter().find(|f| f.0 > t).map(|f| f.0);
    match r.kind {
        Kind::Track => {
            if ui
                .add_enabled(!starts_here, egui::Button::new("New drawing"))
                .clicked()
            {
                app.drawing_at(r.idx, t, false);
                app.go_to_frame(t);
                ui.close_menu();
            }
            if ui
                .add_enabled(
                    !starts_here && start.is_some(),
                    egui::Button::new("Copy the drawing showing"),
                )
                .clicked()
            {
                app.drawing_at(r.idx, t, true);
                app.go_to_frame(t);
                ui.close_menu();
            }
            if ui
                .add_enabled(
                    !starts_here && start.is_some(),
                    egui::Button::new("Empty from here"),
                )
                .on_hover_text("The layer shows nothing from this frame until its next drawing")
                .clicked()
            {
                app.empty_from(r.id, t);
                ui.close_menu();
            }
            if ui
                .add_enabled(starts_here, egui::Button::new("Remove drawing"))
                .clicked()
            {
                select(app, r.id, t);
                app.remove_drawing();
                ui.close_menu();
            }
            ui.separator();
            let can_hold = start.is_some() && next.is_some();
            if ui
                .add_enabled(can_hold, egui::Button::new("Hold one frame longer"))
                .clicked()
                && let Some(next) = next
            {
                app.shift_drawings(r.id, next, 1);
                ui.close_menu();
            }
            let can_shorten = start.zip(next).is_some_and(|(s, n)| n - s > 1);
            if ui
                .add_enabled(can_shorten, egui::Button::new("Hold one frame shorter"))
                .clicked()
                && let Some(next) = next
            {
                app.shift_drawings(r.id, next, -1);
                ui.close_menu();
            }
        }
        Kind::Plain => {
            if ui
                .button("Draw frames on this layer from here")
                .on_hover_text("Its picture becomes the first drawing; a new blank one starts here")
                .clicked()
            {
                app.drawing_at(r.idx, t, false);
                app.go_to_frame(t);
                ui.close_menu();
            }
        }
        Kind::Folder | Kind::Rig => {}
    }
    ui.separator();
    if ui
        .button("Key motion here")
        .on_hover_text("Key position, scale, turn and opacity")
        .clicked()
    {
        app.go_to_frame(t);
        app.key_all_here(r.idx);
        view.open.insert(r.id);
        ui.close_menu();
    }
    let keyed_here =
        (app.canvas.layers[r.idx].motion.as_ref()).is_some_and(|m| m.all_key_frames().contains(&t));
    if ui
        .add_enabled(keyed_here, egui::Button::new("Remove the keys here"))
        .clicked()
    {
        app.remove_keys_at(r.idx, t);
        ui.close_menu();
    }
}

/// The right-click menu on a key: its easing, or taking it away.
fn key_menu(app: &mut PainterApp, ui: &mut egui::Ui, r: &Row, prop: Option<Prop>, k: u32) {
    let what = prop.map_or("Keys".to_string(), |p| format!("{} key", p.name()));
    ui.label(RichText::new(format!("{what} · frame {k}")).color(TEXT_DIM));
    let current = key_curve(app, r, k);
    for ease in Ease::ALL {
        if ui
            .selectable_label(current.and_then(Ease::of) == Some(ease), ease.name())
            .clicked()
        {
            let props: Vec<Prop> = prop.map_or(Prop::ALL.to_vec(), |p| vec![p]);
            app.motion_step(r.idx, |m| {
                for p in props {
                    m.set_curve(p, k, ease.curve());
                }
            });
            ui.close_menu();
        }
    }
    ui.separator();
    if ui.button("Remove").clicked() {
        match prop {
            Some(p) => app.remove_key(r.idx, p, k),
            None => app.remove_keys_at(r.idx, k),
        }
        ui.close_menu();
    }
}

/// The selected layer's motion, and the picked key's easing.
fn inspector(app: &mut PainterApp, ui: &mut egui::Ui, view: &mut TimelineView, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, BG_PANEL);
    ui.painter().vline(
        rect.left() + 0.5,
        rect.y_range(),
        Stroke::new(1.0_f32, BORDER),
    );
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))), |ui| {
        egui::ScrollArea::vertical().id_salt("tl_inspector").show(ui, |ui| {
            let target = app.motion_target(app.canvas.active_layer_idx);
            let name = target.map_or("No layer".to_string(), |t| app.canvas.layers[t].name.clone());
            ui.label(RichText::new(format!("Motion · {name}")).strong().color(TEXT_STRONG));
            ui.label(RichText::new(format!("At frame {}", app.canvas.time)).small().color(TEXT_DIM));
            ui.add_space(4.0);
            crate::ui::motion_panel::motion_fields(app, ui, true);
            ui.add_space(6.0);
            ui.separator();
            // The picked key, if it's still there.
            let picked = view.picked.and_then(|(id, prop, k)| {
                let i = app.canvas.layer_index_of(id).filter(|&i| Some(i) == target)?;
                let motion = app.canvas.layers[i].motion.as_deref()?;
                let props: Vec<Prop> = prop.map_or(Prop::ALL.to_vec(), |p| vec![p]);
                let p = props.into_iter().find(|&p| motion.key_at(p, k).is_some())?;
                let key = motion.key_at(p, k)?.clone();
                let last = motion.keys(p).last().is_some_and(|l| l.time.round() as u32 == k);
                Some((i, prop, p, k, key, last))
            });
            let Some((i, prop, p, k, key, last)) = picked else {
                ui.label(
                    RichText::new("Pick a key (a diamond on an open row) to set how it eases to the next.")
                        .small()
                        .color(TEXT_DIM),
                );
                return;
            };
            let what = prop.map_or("Keys".to_string(), |p| format!("{} key", p.name()));
            ui.label(RichText::new(format!("{what} at frame {k}")).strong());
            if last {
                ui.label(RichText::new("The last key: nothing after it to ease into.").small().color(TEXT_DIM));
            }
            let (curve, ended) = crate::ui::motion_panel::ease_editor(ui, key.curve, ui.id().with(("ease", i, p as u8, k)));
            if let Some(curve) = curve {
                let props: Vec<Prop> = prop.map_or(Prop::ALL.to_vec(), |p| vec![p]);
                app.motion_edit(i, |m| {
                    for p in props {
                        m.set_curve(p, k, curve);
                    }
                });
            }
            if ended {
                app.motion_edit_done();
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Go to it").clicked() {
                    app.go_to_frame(k);
                }
                if ui.button("Remove").clicked() {
                    match prop {
                        Some(p) => app.remove_key(i, p, k),
                        None => app.remove_keys_at(i, k),
                    }
                    view.picked = None;
                }
            });
        });
    });
}

/// No layer to animate yet: how to start.
fn empty_state(app: &mut PainterApp, ui: &mut egui::Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0.0, BG_INSET);
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(16.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
        |ui| {
            ui.add_space((rect.height() / 2.0 - 40.0).max(0.0));
            ui.label(
                RichText::new("Nothing to animate yet")
                    .color(TEXT)
                    .size(14.0),
            );
            ui.label(
                RichText::new("Add an animated layer and draw its first frame.").color(TEXT_DIM),
            );
            ui.add_space(4.0);
            if ui.button("+ Animated layer").clicked() {
                app.new_animation_layer();
            }
        },
    );
}

/// Ctrl+C, Ctrl+X, Ctrl+V, Delete and Esc on the timeline's frames while
/// the pointer is over them: picked frames copied, cut, deleted or let go,
/// copied frames pasted at the frame showing (onto the picked rows, or the
/// selected layer's). Returns whether a key was used.
pub fn timeline_keys(app: &mut PainterApp, ctx: &egui::Context) -> bool {
    use egui::{Key, Modifiers};
    let animation = &app.workspace.animation;
    let over = animation.view.panel.is_some_and(|panel| {
        ctx.input(|i| i.pointer.hover_pos())
            .is_some_and(|p| panel.contains(p))
    });
    if !animation.show_timeline || !over {
        return false;
    }
    let sel = animation.view.sel.clone();
    let has_clip = animation.clipboard.is_some();
    // Ctrl+C, X and V come as copy, cut and paste events: taken here (not
    // by the pixels' clipboard) when there's something for them to do.
    let (mut copy, mut cut, mut paste) = (false, false, false);
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Copy if sel.is_some() => {
                copy = true;
                false
            }
            egui::Event::Cut if sel.is_some() => {
                cut = true;
                false
            }
            egui::Event::Paste(_) if has_clip => {
                paste = true;
                false
            }
            egui::Event::Key {
                key: Key::C | Key::X | Key::V,
                modifiers,
                ..
            } if modifiers.command && (sel.is_some() || has_clip) => false,
            _ => true,
        })
    });
    let pressed =
        |ctx: &egui::Context, m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    if let Some(sel) = &sel {
        if copy {
            app.copy_frames(sel);
            return true;
        }
        if cut {
            app.cut_frames(sel);
            return true;
        }
        if pressed(ctx, Modifiers::NONE, Key::Delete)
            || pressed(ctx, Modifiers::NONE, Key::Backspace)
        {
            app.delete_frames(sel);
            return true;
        }
        if !ctx.is_context_menu_open() && pressed(ctx, Modifiers::NONE, Key::Escape) {
            app.workspace.animation.view.sel = None;
            return true;
        }
    }
    if paste {
        let s = Sizes::of(ctx);
        let mains: Vec<LayerId> = (layer_rows(app, &app.workspace.animation.view, s).into_iter())
            .filter(|r| r.prop.is_none())
            .map(|r| r.id)
            .collect();
        let active = app.canvas.active_layer_idx;
        let first = sel.as_ref().map(|s| s.rows[0]).or_else(|| {
            app.active_track()
                .or_else(|| app.motion_target(active).map(|i| app.canvas.layers[i].id))
        });
        let Some(first) = first else {
            return true;
        };
        let n = app
            .workspace
            .animation
            .clipboard
            .as_ref()
            .map_or(1, |c| c.rows.len());
        let len = app
            .workspace
            .animation
            .clipboard
            .as_ref()
            .map_or(1, |c| c.len);
        let k = mains.iter().position(|&m| m == first).unwrap_or(0);
        let rows: Vec<LayerId> = mains.iter().skip(k).take(n).copied().collect();
        let t = app.canvas.time;
        app.paste_frames(&rows, t);
        app.workspace.animation.view.sel = Some(FrameSelection {
            rows,
            from: t,
            to: t + len - 1,
        });
        return true;
    }
    false
}

/// The selected rig layer: the animation it plays, and its bones turned and
/// moved (keyed at the frame showing, or its setup pose without an
/// animation).
fn rig_controls(app: &mut PainterApp, ui: &mut egui::Ui) {
    let Some(rig) = app.active_rig().cloned() else {
        return;
    };
    let seconds = app.canvas.time as f32 / app.canvas.timeline.fps.max(1) as f32;
    ui.horizontal_wrapped(|ui| {
        ui.label("Rig");
        let current = (rig.animation.and_then(|a| rig.animations.get(a)))
            .map_or("Setup pose", |a| a.name.as_str());
        let mut chosen = None;
        egui::ComboBox::from_id_salt("rig_animation")
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(rig.animation.is_none(), "Setup pose")
                    .clicked()
                {
                    chosen = Some(None);
                }
                for (i, a) in rig.animations.iter().enumerate() {
                    if ui
                        .selectable_label(rig.animation == Some(i), &a.name)
                        .clicked()
                    {
                        chosen = Some(Some(i));
                    }
                }
            });
        if let Some(c) = chosen {
            app.rig_edit(|r| r.animation = c);
            app.rig_edit_done();
        }
        ui.label(
            egui::RichText::new(if rig.animation.is_some() {
                "Bone changes are keyed at this frame."
            } else {
                "Bone changes move the setup pose."
            })
            .color(TEXT_DIM),
        );
    });
    egui::CollapsingHeader::new(format!("Bones ({})", rig.bones.len()))
        .id_salt("rig_bones")
        .show(ui, |ui| {
            egui::Grid::new("rig_bone_grid")
                .striped(true)
                .show(ui, |ui| {
                    for (b, bone) in rig.bones.iter().enumerate() {
                        ui.label(&bone.name);
                        let (mut rot, mut x, mut y) = crate::app::rig::bone_now(&rig, b, seconds);
                        let mut changed = false;
                        let mut done = false;
                        for (value, suffix) in [(&mut rot, "°"), (&mut x, " x"), (&mut y, " y")] {
                            let r = ui.add(egui::DragValue::new(value).speed(0.5).suffix(suffix));
                            changed |= r.changed();
                            done |= r.drag_stopped() || r.lost_focus();
                        }
                        if changed {
                            app.rig_edit(|r| crate::app::rig::set_bone(r, b, seconds, rot, [x, y]));
                        }
                        if done {
                            app.rig_edit_done();
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Ask for a Spine, DragonBones or Lottie JSON to bring in as a rig layer.
#[cfg(not(target_os = "android"))]
pub fn pick_animation(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog()
        .add_filter("Spine, DragonBones or Lottie", &["json", "JSON"]);
    app.file_dialog_job(dialog, crate::app::jobs::Pick::File, |app, paths| {
        if let Some(path) = paths.into_iter().next() {
            app.import_animation_in_background(path);
        }
    });
}

/// Ask for a video, GIF or animated picture to bring in as an animated
/// layer.
#[cfg(not(target_os = "android"))]
pub fn pick_video(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog().add_filter(
        "Videos and animated pictures",
        &[
            "mp4", "webm", "mov", "mkv", "avi", "gif", "png", "apng", "webp", "MP4", "MOV", "GIF",
        ],
    );
    app.file_dialog_job(dialog, crate::app::jobs::Pick::File, |app, paths| {
        if let Some(path) = paths.into_iter().next() {
            app.import_frames_in_background(path);
        }
    });
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

    /// The panel drawn headlessly with an animated layer at frame 0 and a
    /// second drawing at frame 3: a click on a frame goes there, and the
    /// first drawing's end dragged from frame 3 to 6 holds it longer.
    #[test]
    fn a_click_goes_to_a_frame_and_a_block_end_drags_its_hold() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.animate_active_layer();
        let track = app.active_track().unwrap();
        app.go_to_frame(3);
        app.add_drawing(false);
        app.go_to_frame(0);
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
        let grid = app.workspace.animation.view.grid.expect("the frames drawn");
        let cell = app.workspace.animation.view.cell;
        let y = grid.top() + super::Sizes::of(&ctx).row / 2.0;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let at = egui::pos2(grid.left() + 5.5 * cell, y);
        frame(vec![egui::Event::PointerMoved(at)], &mut app);
        frame(vec![button(at, true)], &mut app);
        frame(vec![button(at, false)], &mut app);
        assert_eq!(app.canvas.time, 5, "clicked frame 5");

        // The first block's end, at frame 3's left edge, dragged to frame 6's.
        let from = egui::pos2(grid.left() + 3.0 * cell, y);
        let to = egui::pos2(grid.left() + 6.0 * cell, y);
        frame(vec![egui::Event::PointerMoved(from)], &mut app);
        frame(vec![button(from, true)], &mut app);
        for k in 1..=6 {
            let p = from + (to - from) * (k as f32 / 6.0);
            frame(vec![egui::Event::PointerMoved(p)], &mut app);
        }
        frame(vec![button(to, false)], &mut app);
        let starts: Vec<u32> = app.canvas.frames_of(track).iter().map(|f| f.0).collect();
        assert_eq!(starts, [0, 6], "the second drawing now starts at frame 6");
    }

    /// Frames picked by dragging over empty frames, then dragged along:
    /// the drawing in them moves.
    #[test]
    fn frames_are_picked_by_a_box_and_moved_by_a_drag() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.animate_active_layer();
        let track = app.active_track().unwrap();
        app.go_to_frame(4);
        app.add_drawing(false);
        app.go_to_frame(0);
        // Empty from 6: frames after it are empty, to drag over.
        app.empty_from(track, 6);
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
        let grid = app.workspace.animation.view.grid.unwrap();
        let cell = app.workspace.animation.view.cell;
        let y = grid.top() + super::Sizes::of(&ctx).row / 2.0;
        let at = |t: f32| egui::pos2(grid.left() + (t + 0.5) * cell, y);
        let button = |pos, pressed, shift: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers {
                shift,
                ..Default::default()
            },
        };
        let drag = |from: egui::Pos2, to: egui::Pos2, shift: bool, app: &mut crate::PainterApp| {
            let held = egui::Modifiers {
                shift,
                ..Default::default()
            };
            let input = |events: Vec<egui::Event>| egui::RawInput {
                screen_rect: Some(screen),
                events,
                modifiers: held,
                ..Default::default()
            };
            for events in [
                vec![egui::Event::PointerMoved(from)],
                vec![button(from, true, shift)],
            ] {
                let _ = ctx.run(input(events), |ctx| super::timeline_panel(app, ctx));
            }
            for k in 1..=8 {
                let p = from + (to - from) * (k as f32 / 8.0);
                let _ = ctx.run(input(vec![egui::Event::PointerMoved(p)]), |ctx| {
                    super::timeline_panel(app, ctx)
                });
            }
            let _ = ctx.run(input(vec![button(to, false, shift)]), |ctx| {
                super::timeline_panel(app, ctx)
            });
        };
        // Shift+drag from frame 4 to 5 picks them.
        drag(at(4.0), at(5.0), true, &mut app);
        let sel = app.workspace.animation.view.sel.clone().expect("picked");
        assert_eq!((sel.from, sel.to, sel.rows.clone()), (4, 5, vec![track]));
        // Dragged 4 along: the drawing at 4 now starts at 8.
        drag(at(4.0), at(8.0), false, &mut app);
        let starts: Vec<u32> = app.canvas.frames_of(track).iter().map(|f| f.0).collect();
        assert!(starts.contains(&8), "{starts:?}");
        let sel = app.workspace.animation.view.sel.clone().unwrap();
        assert_eq!((sel.from, sel.to), (8, 9));
    }

    #[test]
    fn timecodes_count_minutes_seconds_and_frames() {
        assert_eq!(super::timecode(0, 24), "00:00:00");
        assert_eq!(super::timecode(24 * 61 + 5, 24), "01:01:05");
    }
}
