//! Transform sessions. Starting one lifts the selection (or the whole layer)
//! onto a floating layer that is redrawn live as the box is dragged; commit
//! composites it back as a single undo step, cancel restores the original.

use crate::PainterApp;
use crate::app::state::FloatSession;
use crate::app::stroke_ops::exclusive;
use crate::app::tools::Tool;
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::canvas::storage::{DistortKind, LayerKind, TransformParams};
use crate::selection::transform::{TransformInfo, TransformState};
use eframe::egui::{self, Color32, Vec2};
use rayon::prelude::*;
use std::collections::HashMap;

/// Whether a transform session is running (pixels or a selection).
pub(crate) fn transform_running(app: &PainterApp) -> bool {
    app.layer_state.floating_layer_idx.is_some() || app.layer_state.selection_transform.is_some()
}

/// Keep a copy of the session's transform (see
/// [`crate::app::state::LayerState::transform_info`]).
fn remember_info(app: &mut PainterApp) {
    if let Tool::Transform(info) = app.active_tool
        && transform_running(app)
    {
        app.layer_state.transform_info = Some(info);
    }
}

/// The running session's transform: the tool's, or the copy kept when
/// the tool was left.
fn session_info(app: &PainterApp) -> Option<TransformInfo> {
    match app.active_tool {
        Tool::Transform(info) => Some(info),
        _ => app.layer_state.transform_info,
    }
}

/// Lift the selection, or the whole active layer without one, onto a
/// floating layer. A selection with nothing under it to lift (or on a layer
/// that can't be changed) is transformed itself, as an outline. Does
/// nothing if a session is already running, or there's nothing to lift.
pub(crate) fn create_floating_layer(app: &mut PainterApp) {
    if transform_running(app) {
        return;
    }
    if !lift_pixels(app) && app.selection_manager.has_selection() {
        app.layer_state.selection_transform = app.selection_manager.current_shape.clone();
        let bounds = app.selection_manager.get_bounds();
        if let Tool::Transform(ref mut info) = app.active_tool {
            info.reset_to(bounds);
        }
    }
    remember_info(app);
}

/// The floating-layer part of [`create_floating_layer`]: whether it lifted
/// anything.
fn lift_pixels(app: &mut PainterApp) -> bool {
    let active = app.canvas.active_layer_idx;
    let Some(layer) = app.canvas.layers.get(active) else {
        return false;
    };
    if layer.locked || !matches!(layer.kind, LayerKind::Paint) {
        return false;
    }
    if layer.position_locked {
        app.export_state.message = Some("The layer's position is locked".into());
        return false;
    }
    let Some(source_id) = app.canvas.layer_id_at(active) else {
        return false;
    };
    // Its paint's thickness wouldn't move with it: lit as it shows first.
    app.bake_impasto(active);
    app.release_canvas();

    let selection = app
        .selection_manager
        .has_selection()
        .then_some(&app.selection_manager);
    let Some(idx) = exclusive(&mut app.canvas).float_pixels(selection) else {
        return false;
    };
    let buffer = app.canvas.capture_layer_pixels(idx);

    // The source as it was: the float lifted disjoint pixels, so putting
    // them back over what's left is exact.
    let tile_len = app.canvas.tile_size() * app.canvas.tile_size();
    let source_tiles: HashMap<(i32, i32), Vec<Color32>> = buffer
        .par_iter()
        .map(|(&(tx, ty), lifted)| {
            let mut tile = app
                .canvas
                .get_layer_tile_data(active, tx, ty)
                .unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_len]);
            for (dst, &src) in tile.iter_mut().zip(lifted) {
                if src != Color32::TRANSPARENT {
                    *dst = src;
                }
            }
            ((tx, ty), tile)
        })
        .collect();

    app.layer_state.float_session = Some(FloatSession {
        source_id,
        source_tiles,
        selection: app.selection_manager.current_shape.clone(),
        last_rect: None,
        src_bounds: None,
        draft_shown: false,
    });
    app.layer_state.floating_layer_idx = Some(idx);
    app.layer_state.floating_buffer = Some(buffer);
    app.insert_layer_state(idx);

    let bounds = app.canvas.get_content_bounds(idx, None);
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.reset_to(bounds);
    }
    match bounds {
        Some(b) => app.mark_tiles_in_bounds_dirty(b),
        None => app.mark_all_tiles_dirty(),
    }
    true
}

/// Apply a selection-only transform: the selection moves, as one undo step.
fn commit_selection_transform(app: &mut PainterApp) {
    let Some(before) = app.layer_state.selection_transform.take() else {
        return;
    };
    if let Some(info) = session_info(app).filter(|i| !i.is_identity()) {
        app.selection_manager.transform_by(&info.params());
        app.record_selection_change(Some(before));
    }
    reset_tool_info(app);
}

/// Composite the floating layer back into its source as one undo step.
pub(crate) fn commit_floating_layer(app: &mut PainterApp) {
    if app.layer_state.selection_transform.is_some() {
        commit_selection_transform(app);
        return;
    }
    let Some(idx) = app.layer_state.floating_layer_idx else {
        return;
    };
    // Commit full quality, never the quick drag preview.
    if app
        .layer_state
        .float_session
        .as_ref()
        .is_some_and(|s| s.draft_shown)
        || overlay_showing(app)
    {
        if let Tool::Transform(ref mut info) = app.active_tool {
            info.start_pos = None;
        }
        if let Some(info) = app.layer_state.transform_info.as_mut() {
            info.start_pos = None;
        }
        app.layer_state.transform_preview_pending = true;
    }
    flush_transform_preview(app);
    app.release_canvas();
    let session = app.layer_state.float_session.take();
    let info = session_info(app);
    // Where the floating pixels are now: the transformed box (no rescan).
    let float_bounds = info
        .and_then(|i| i.bounds.map(|b| calc_transformed_bounds(b, &i.params())))
        .or_else(|| app.canvas.get_content_bounds(idx, None));

    let target = session
        .as_ref()
        .and_then(|s| app.canvas.layer_index_of(s.source_id))
        .unwrap_or(idx.saturating_sub(1));
    // What the tiles the pixels land on hold now (besides those they came
    // from, saved when lifted): undo puts them back as they were, not empty.
    let landing: HashMap<(i32, i32), Vec<Color32>> = match &session {
        Some(session) => app
            .canvas
            .layer_tile_keys(idx)
            .into_iter()
            .filter(|key| !session.source_tiles.contains_key(key))
            .filter_map(|(tx, ty)| {
                let data = app.canvas.get_layer_tile_data(target, tx, ty)?;
                Some(((tx, ty), data))
            })
            .collect(),
        None => HashMap::new(),
    };
    let touched = exclusive(&mut app.canvas).merge_floating(idx, target);
    app.remove_layer_state(idx);
    app.layer_state.floating_layer_idx = None;
    app.layer_state.floating_buffer = None;
    app.layer_state.transform_preview_pending = false;
    app.layer_state.float_overlay = None;

    // One undo step: every tile the float came from or went to, as it was.
    if let Some(mut session) = session {
        let ts = app.canvas.tile_size();
        // Moved into the undo step, not copied.
        let mut source_tiles = std::mem::take(&mut session.source_tiles);
        source_tiles.extend(landing);
        let mut keys: Vec<(i32, i32)> = source_tiles.keys().copied().collect();
        keys.extend(touched.iter().filter(|k| !source_tiles.contains_key(k)));
        let tiles: Vec<TileSnapshot> = keys
            .into_iter()
            .map(|(tx, ty)| TileSnapshot {
                tx,
                ty,
                layer_id: session.source_id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: source_tiles
                    .remove(&(tx, ty))
                    .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts])
                    .into(),
            })
            .collect();
        let moved = info.is_some_and(|i| !i.is_identity());
        if moved && app.canvas.layer_index_of(session.source_id).is_some() {
            // A whole text or vector layer only moved stays text or lines
            // (at its new place).
            let layer_action = info.filter(|_| session.selection.is_none()).and_then(|i| {
                app.text_layer_moved(session.source_id, &i)
                    .or_else(|| app.vector_layer_moved(session.source_id, &i))
            });
            let action = UndoAction {
                tiles,
                selection: Some(session.selection.clone()),
                transform: None,
                layer_action,
            };
            app.push_undo(action);
        }
    }

    // The selection follows the pixels (a distort too).
    if let Some(info) = info
        && !info.is_identity()
    {
        app.selection_manager.transform_by(&info.params());
    }
    reset_tool_info(app);

    for rect in [float_bounds, info.and_then(|i| i.bounds)]
        .into_iter()
        .flatten()
    {
        app.mark_tiles_in_bounds_dirty(rect);
    }
    if float_bounds.is_none() {
        app.mark_all_tiles_dirty();
    }
}

/// Drop the floating layer and put the source back as it was.
pub(crate) fn cancel_floating_layer(app: &mut PainterApp) {
    // A selection-only transform changed nothing yet.
    if app.layer_state.selection_transform.take().is_some() {
        reset_tool_info(app);
        return;
    }
    let Some(idx) = app.layer_state.floating_layer_idx else {
        return;
    };
    app.release_canvas();
    let float_bounds = app.canvas.get_content_bounds(idx, None);
    let start_bounds = match app.active_tool {
        Tool::Transform(info) => info.bounds,
        _ => None,
    };
    let session = app.layer_state.float_session.take();
    {
        let canvas = exclusive(&mut app.canvas);
        canvas.layers.remove(idx);
        if let Some(session) = &session
            && let Some(source) = canvas.layer_index_of(session.source_id)
        {
            for (&(tx, ty), data) in &session.source_tiles {
                canvas.set_layer_tile_data(source, tx, ty, data.clone());
            }
            canvas.active_layer_idx = source;
        } else {
            canvas.active_layer_idx = idx
                .saturating_sub(1)
                .min(canvas.layers.len().saturating_sub(1));
        }
    }
    app.remove_layer_state(idx);
    if let Some(session) = session {
        app.selection_manager.current_shape = session.selection;
    }
    app.layer_state.floating_layer_idx = None;
    app.layer_state.floating_buffer = None;
    app.layer_state.transform_preview_pending = false;
    app.layer_state.float_overlay = None;
    reset_tool_info(app);
    for rect in [float_bounds, start_bounds].into_iter().flatten() {
        app.mark_tiles_in_bounds_dirty(rect);
    }
    app.mark_all_tiles_dirty();
}

fn reset_tool_info(app: &mut PainterApp) {
    app.layer_state.transform_info = None;
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.reset_to(None);
    }
}

/// The topmost unlocked, visible paint layer with paint at canvas pixel
/// `pos`.
fn layer_under(app: &PainterApp, pos: Vec2) -> Option<usize> {
    let (x, y) = (pos.x.floor() as i32, pos.y.floor() as i32);
    if x < 0 || y < 0 || x >= app.canvas.width() as i32 || y >= app.canvas.height() as i32 {
        return None;
    }
    let ts = app.canvas.tile_size() as i32;
    (1..app.canvas.layers.len()).rev().find(|&i| {
        let l = &app.canvas.layers[i];
        l.visible
            && !l.locked
            && !l.position_locked
            && l.kind == LayerKind::Paint
            && app
                .canvas
                .get_layer_tile_data(i, x / ts, y / ts)
                .is_some_and(|t| t[((y % ts) * ts + x % ts) as usize].a() > 24)
    })
}

/// Pointer down with the Transform tool.
pub(crate) fn transform_press(app: &mut PainterApp, pos: Vec2) {
    // Clicking another image selects its layer (unless the click is on the
    // current box, which moves / scales it as usual).
    if app.workspace.transform_pick_layer
        && app.layer_state.floating_layer_idx.is_none()
        && app.workspace.select.quick_mask.is_none()
        && !app.selection_manager.has_selection()
        && let Tool::Transform(info) = app.active_tool
    {
        let zoom = app.viewport.zoom;
        let on_box = info.bounds.is_some()
            && matches!(
                info.hit_test(pos, zoom),
                TransformState::Moving | TransformState::Scaling(_) | TransformState::Corner(_)
            );
        if !on_box
            && let Some(layer) = layer_under(app, pos)
            && layer != app.canvas.active_layer_idx
        {
            app.canvas_mut().active_layer_idx = layer;
            let bounds = app.canvas.get_content_bounds(layer, None);
            if let Tool::Transform(ref mut info) = app.active_tool {
                info.reset_to(bounds);
            }
        }
    }
    create_floating_layer(app);
    let zoom = app.viewport.zoom;
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.start_pos = Some(pos);
        info.state = info.hit_test(pos, zoom);
    }
}

/// Pointer drag with the Transform tool. `keep_aspect` (Shift) scales
/// proportionally from a corner.
pub(crate) fn transform_drag(app: &mut PainterApp, pos: Vec2, keep_aspect: bool) {
    let Tool::Transform(ref mut info) = app.active_tool else {
        return;
    };
    let Some(start) = info.start_pos else {
        return;
    };
    let delta = pos - start;
    match info.state {
        TransformState::Moving => match info.points_mut() {
            Some(points) => points.iter_mut().for_each(|c| *c += delta),
            None => info.offset += delta,
        },
        TransformState::Rotating => update_rotation(info, start, pos),
        TransformState::Scaling(idx) => update_scaling(info, delta, idx, keep_aspect),
        TransformState::Corner(i) => {
            if let Some(grid) = info.warp.as_mut() {
                // A warp point goes anywhere; the picture bends with it.
                if let Some(p) = grid.used_mut().get_mut(i) {
                    *p += delta;
                }
            } else if let Some(corners) = info.corners.as_mut()
                && i < 4
            {
                let mut moved = *corners;
                moved[i] += delta;
                // A folded or inside-out quad has no sensible picture:
                // the corner waits where it was (and catches up with the
                // pointer once it's somewhere valid again).
                if !crate::canvas::storage::is_convex_quad(&moved) {
                    return;
                }
                *corners = moved;
            }
        }
        TransformState::None => return,
    }
    info.start_pos = Some(pos);
    app.layer_state.transform_preview_pending = true;
    remember_info(app);
}

pub(crate) fn transform_release(app: &mut PainterApp) {
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.start_pos = None;
        info.state = TransformState::None;
    }
    remember_info(app);
    // Replace the quick drag preview (or the GPU overlay) with the
    // full-quality render.
    if overlay_showing(app)
        || app
            .layer_state
            .float_session
            .as_ref()
            .is_some_and(|s| s.draft_shown)
    {
        app.layer_state.transform_preview_pending = true;
    }
}

/// Redraw the floating layer for the current transform, at most once a
/// frame however many pointer events arrived.
pub(crate) fn flush_transform_preview(app: &mut PainterApp) {
    // While the GPU overlay shows the drag, the layer is rendered once on
    // release instead (the request stays queued until then).
    if overlay_showing(app) && dragging(app) {
        return;
    }
    // Once a frame, after input: covers every change to the box.
    remember_info(app);
    if !std::mem::take(&mut app.layer_state.transform_preview_pending) {
        return;
    }
    if let Some(info) = session_info(app) {
        apply_live_transform_preview(app, &info);
    }
}

/// Mirror the floating content inside its box.
pub(crate) fn flip(app: &mut PainterApp, horizontal: bool) {
    create_floating_layer(app);
    if let Tool::Transform(ref mut info) = app.active_tool {
        if let Some(w) = info.warp.as_mut() {
            *w = w.flipped(horizontal);
        } else if let Some(c) = info.corners.as_mut() {
            *c = if horizontal {
                [c[1], c[0], c[3], c[2]]
            } else {
                [c[3], c[2], c[1], c[0]]
            };
        } else if horizontal {
            info.scale.x = -info.scale.x;
        } else {
            info.scale.y = -info.scale.y;
        }
        app.layer_state.transform_preview_pending = true;
    }
}

/// Turn the floating content a quarter turn.
pub(crate) fn rotate_quarter(app: &mut PainterApp, clockwise: bool) {
    create_floating_layer(app);
    if let Tool::Transform(ref mut info) = app.active_tool {
        if let Some(w) = info.warp.as_mut() {
            *w = w.rotated(clockwise);
        } else if let Some(c) = info.corners.as_mut() {
            *c = if clockwise {
                [c[3], c[0], c[1], c[2]]
            } else {
                [c[1], c[2], c[3], c[0]]
            };
        } else {
            let quarter = std::f32::consts::FRAC_PI_2;
            info.rotation += if clockwise { quarter } else { -quarter };
        }
        app.layer_state.transform_preview_pending = true;
    }
}

/// Switch between free transform (`None`), perspective (four corners)
/// and distort (a warp grid). The new mode starts from the picture as it
/// is now.
pub(crate) fn set_corner_mode(app: &mut PainterApp, mode: Option<DistortKind>) {
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.set_point_mode(mode);
        app.layer_state.transform_preview_pending = true;
    }
    remember_info(app);
}

/// Points along a side of the distort grid (keeps the current shape).
pub(crate) fn set_warp_size(app: &mut PainterApp, n: usize) {
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.set_warp_size(n);
        app.layer_state.transform_preview_pending = true;
    }
    remember_info(app);
}

pub(crate) fn mark_transform_dirty(
    app: &mut PainterApp,
    src_bounds: Option<egui::Rect>,
    params: &TransformParams,
) {
    if let Some(bounds) = src_bounds {
        app.mark_tiles_in_bounds_dirty(bounds);
        app.mark_tiles_in_bounds_dirty(calc_transformed_bounds(bounds, params));
    } else {
        app.mark_all_tiles_dirty();
    }
}

fn calc_transformed_bounds(src_bounds: egui::Rect, params: &TransformParams) -> egui::Rect {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    for corner in crate::canvas::storage::rect_corners(src_bounds) {
        let p = params.forward(corner);
        min = min.min(p);
        max = max.max(p);
    }
    let padding = 2.0;
    egui::Rect::from_min_max(
        (min - Vec2::splat(padding)).to_pos2(),
        (max + Vec2::splat(padding)).to_pos2(),
    )
}

pub(crate) fn update_rotation(info: &mut TransformInfo, start: Vec2, current: Vec2) {
    if let Some(bounds) = info.bounds {
        let center = bounds.center().to_vec2() + info.offset;
        let start_vec = start - center;
        let current_vec = current - center;
        let angle = current_vec.y.atan2(current_vec.x) - start_vec.y.atan2(start_vec.x);
        info.rotation += angle;
    }
}

pub(crate) fn update_scaling(
    info: &mut TransformInfo,
    delta: Vec2,
    handle_idx: usize,
    keep_aspect: bool,
) {
    let Some(bounds) = info.bounds else {
        return;
    };
    // The drag in the box's own (rotated, possibly mirrored) frame.
    let (sin_r, cos_r) = info.rotation.sin_cos();
    let dx = (delta.x * cos_r + delta.y * sin_r) * info.scale.x.signum();
    let dy = (-delta.x * sin_r + delta.y * cos_r) * info.scale.y.signum();

    let scale_delta = match handle_idx {
        0 => Vec2::new(-dx, -dy),
        1 => Vec2::new(0.0, -dy),
        2 => Vec2::new(dx, -dy),
        3 => Vec2::new(dx, 0.0),
        4 => Vec2::new(dx, dy),
        5 => Vec2::new(0.0, dy),
        6 => Vec2::new(-dx, dy),
        7 => Vec2::new(-dx, 0.0),
        _ => Vec2::ZERO,
    };

    let before = info.scale;
    let (w, h) = (bounds.width(), bounds.height());
    let mut grow = Vec2::new(
        if w > 0.0 {
            scale_delta.x / (w * 0.5)
        } else {
            0.0
        },
        if h > 0.0 {
            scale_delta.y / (h * 0.5)
        } else {
            0.0
        },
    );
    if keep_aspect && handle_idx.is_multiple_of(2) {
        let g = (grow.x + grow.y) * 0.5;
        grow = Vec2::splat(g);
        let ratio = info.scale.y.abs() / info.scale.x.abs().max(1e-6);
        grow.y *= ratio;
    }
    // Grow away from the centre whichever way the box is mirrored.
    info.scale.x += grow.x * before.x.signum();
    info.scale.y += grow.y * before.y.signum();
    // Never collapse or flip through zero by dragging; flips are explicit.
    let keep = |s: f32, was: f32| {
        if s.abs() < 0.01 || s.signum() != was.signum() {
            0.01 * was.signum()
        } else {
            s
        }
    };
    info.scale.x = keep(info.scale.x, before.x);
    info.scale.y = keep(info.scale.y, before.y);
}

pub(crate) fn apply_live_transform_preview(app: &mut PainterApp, info: &TransformInfo) {
    if app.layer_state.floating_buffer.is_none() {
        return;
    }
    app.release_canvas();
    // The source never changes during a session: find its bounds once.
    let src_bounds = match app
        .layer_state
        .float_session
        .as_ref()
        .and_then(|s| s.src_bounds)
    {
        Some(b) => Some(b),
        None => {
            let b = app
                .layer_state
                .floating_buffer
                .as_ref()
                .and_then(|buffer| app.canvas.tiles_content_bounds(buffer));
            if let Some(s) = app.layer_state.float_session.as_mut() {
                s.src_bounds = b;
            }
            b
        }
    };
    if let Some(buffer) = &app.layer_state.floating_buffer
        && let Some(idx) = app.layer_state.floating_layer_idx
        && let Some(src_bounds) = src_bounds
    {
        let mut params = info.params();
        // Quick nearest-pixel preview while dragging; full quality after.
        params.draft = info.start_pos.is_some();
        exclusive(&mut app.canvas).preview_transform(idx, buffer, src_bounds, params);
        if let Some(s) = app.layer_state.float_session.as_mut() {
            s.draft_shown = params.draft;
        }
        // Repaint where the layer was and where it is now.
        let now = info.bounds.map(|b| calc_transformed_bounds(b, &params));
        let before = app
            .layer_state
            .float_session
            .as_mut()
            .and_then(|s| std::mem::replace(&mut s.last_rect, now));
        if let Some(b) = before {
            app.mark_tiles_in_bounds_dirty(b);
        }
        mark_transform_dirty(app, info.bounds, &params);
    }
}

fn overlay_showing(app: &PainterApp) -> bool {
    app.layer_state
        .float_overlay
        .as_ref()
        .is_some_and(|o| o.showing)
}

/// Whether the transform box is being dragged right now.
fn dragging(app: &PainterApp) -> bool {
    matches!(app.active_tool, Tool::Transform(info) if info.start_pos.is_some() && info.state != TransformState::None)
}

/// Upload the floating pixels as a texture (shrunk by a whole factor if
/// larger than the GPU allows).
fn build_overlay(app: &PainterApp, ctx: &egui::Context) -> Option<crate::app::state::FloatOverlay> {
    let buffer = app.layer_state.floating_buffer.as_ref()?;
    let b = app.layer_state.float_session.as_ref()?.src_bounds?;
    let area = egui::Rect::from_min_max(b.min, b.max + egui::vec2(1.0, 1.0));
    let (w, h) = (area.width() as usize, area.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let max_side = ctx.input(|i| i.max_texture_side).max(256);
    let step = w.max(h).div_ceil(max_side).max(1);
    let (tw, th) = (w.div_ceil(step), h.div_ceil(step));
    let ts = app.canvas.tile_size() as i32;
    let (x0, y0) = (area.min.x as i32, area.min.y as i32);
    let mut pixels = vec![Color32::TRANSPARENT; tw * th];
    pixels
        .par_chunks_mut(tw)
        .enumerate()
        .for_each(|(row, line)| {
            let y = y0 + (row * step) as i32;
            for (col, px) in line.iter_mut().enumerate() {
                let x = x0 + (col * step) as i32;
                let key = (x.div_euclid(ts), y.div_euclid(ts));
                if let Some(tile) = buffer.get(&key) {
                    *px = tile[((y - key.1 * ts) * ts + (x - key.0 * ts)) as usize];
                }
            }
        });
    let image = egui::ColorImage {
        size: [tw, th],
        pixels,
    };
    Some(crate::app::state::FloatOverlay {
        texture: ctx.load_texture("floating-transform", image, egui::TextureOptions::LINEAR),
        area,
        showing: false,
        revealing: false,
    })
}

/// Hide or show the floating layer in the CPU composite, redrawing where
/// it is.
fn set_float_layer_visible(app: &mut PainterApp, visible: bool) {
    let Some(idx) = app.layer_state.floating_layer_idx else {
        return;
    };
    if app
        .canvas
        .layers
        .get(idx)
        .is_some_and(|l| l.visible != visible)
    {
        app.canvas_mut().layers[idx].visible = visible;
        let rects = [
            app.layer_state
                .float_session
                .as_ref()
                .and_then(|s| s.last_rect),
            app.layer_state
                .float_session
                .as_ref()
                .and_then(|s| s.src_bounds),
        ];
        for r in rects.into_iter().flatten() {
            app.mark_tiles_in_bounds_dirty(r);
        }
        if let Tool::Transform(info) = app.active_tool
            && let Some(b) = info.bounds
        {
            app.mark_tiles_in_bounds_dirty(calc_transformed_bounds(b, &info.params()));
        }
    }
}

/// Per frame, after input: while the box is dragged, show the floating
/// pixels as a GPU overlay (cheap at any size, never drawn in pieces)
/// instead of re-rendering the layer; after release, render it once.
pub(crate) fn update_float_overlay(app: &mut PainterApp, ctx: &egui::Context) {
    if app.layer_state.floating_layer_idx.is_none() {
        app.layer_state.float_overlay = None;
        return;
    }
    if dragging(app) {
        if app.layer_state.float_overlay.is_none() {
            // Source bounds are found once per session.
            if app
                .layer_state
                .float_session
                .as_ref()
                .is_some_and(|s| s.src_bounds.is_none())
            {
                let b = app
                    .layer_state
                    .floating_buffer
                    .as_ref()
                    .and_then(|buffer| app.canvas.tiles_content_bounds(buffer));
                if let Some(s) = app.layer_state.float_session.as_mut() {
                    s.src_bounds = b;
                }
            }
            app.layer_state.float_overlay = build_overlay(app, ctx);
        }
        let Some(overlay) = app.layer_state.float_overlay.as_mut() else {
            return; // No overlay possible: the CPU preview carries on.
        };
        overlay.revealing = false;
        if !overlay.showing {
            overlay.showing = true;
            set_float_layer_visible(app, false);
        }
    } else if let Some(overlay) = app.layer_state.float_overlay.as_mut()
        && overlay.showing
        && !overlay.revealing
    {
        // Released: the full-quality render already ran this frame (the
        // release queued it); show the layer and keep the overlay on top
        // until the redrawn tiles are all on screen.
        overlay.revealing = true;
        set_float_layer_visible(app, true);
    }
}

/// After this frame's tile uploads: once nothing is left to upload, the
/// canvas shows the rendered layer and the overlay can go.
pub(crate) fn float_overlay_uploaded(app: &mut PainterApp, more_tiles: bool) {
    if let Some(overlay) = app.layer_state.float_overlay.as_mut()
        && overlay.revealing
        && !more_tiles
    {
        overlay.showing = false;
        overlay.revealing = false;
    }
}

/// Draw the overlay where the transform puts it.
pub(crate) fn draw_float_overlay(
    app: &PainterApp,
    painter: &egui::Painter,
    map: &crate::app::view::render::ScreenMap,
) {
    let (Some(overlay), Tool::Transform(info)) = (&app.layer_state.float_overlay, app.active_tool)
    else {
        return;
    };
    if !overlay.showing {
        return;
    }
    let params = info.params();
    painter.add(egui::Shape::mesh(overlay_mesh(
        overlay.texture.id(),
        overlay.area,
        &params,
        &|p| map.to_screen(p),
    )));
}

/// The floating texture as a grid of triangles, each vertex placed where
/// the transform puts it. Two triangles can only stretch the picture
/// evenly, so a perspective looked different while dragging than once
/// applied; a fine grid follows any of the transforms closely.
fn overlay_mesh(
    texture: egui::TextureId,
    area: egui::Rect,
    params: &TransformParams,
    to_screen: &dyn Fn(Vec2) -> egui::Pos2,
) -> egui::Mesh {
    // A warp uses the renderer's own mesh, so both show the same bends.
    let steps = if params.warp.is_some() {
        crate::canvas::storage::warp::MESH_STEPS as u32
    } else if params.distort.is_some() {
        32
    } else {
        1
    };
    let mut mesh = egui::Mesh::with_texture(texture);
    for j in 0..=steps {
        for i in 0..=steps {
            let (u, v) = (i as f32 / steps as f32, j as f32 / steps as f32);
            let p = Vec2::new(
                area.min.x + u * area.width(),
                area.min.y + v * area.height(),
            );
            mesh.vertices.push(egui::epaint::Vertex {
                pos: to_screen(params.forward(p)),
                uv: egui::pos2(u, v),
                color: Color32::WHITE,
            });
        }
    }
    let row = steps + 1;
    for j in 0..steps {
        for i in 0..steps {
            let k = j * row + i;
            mesh.add_triangle(k, k + 1, k + row + 1);
            mesh.add_triangle(k, k + row + 1, k + row);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::selection::{SelectionMode, SelectionShape};

    const RED: Color32 = Color32::from_rgb(255, 0, 0);
    const BLUE: Color32 = Color32::from_rgb(0, 0, 255);

    fn pixel(app: &PainterApp, layer: usize, x: i32, y: i32) -> Color32 {
        app.canvas
            .get_layer_tile_data(layer, x / 64, y / 64)
            .and_then(|t| t.get(((y % 64) * 64 + x % 64) as usize).copied())
            .unwrap_or(Color32::TRANSPARENT)
    }

    /// Layer 1: a red square in tile (0,0), blue paint filling tile (2,0).
    fn app() -> PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(256, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let mut red = vec![Color32::TRANSPARENT; 64 * 64];
        for y in 10..30 {
            for x in 10..30 {
                red[y * 64 + x] = RED;
            }
        }
        app.canvas_mut().set_layer_tile_data(1, 0, 0, red);
        app.canvas_mut()
            .set_layer_tile_data(1, 2, 0, vec![BLUE; 64 * 64]);
        app.selection_manager.canvas_size = [256, 64];
        app
    }

    /// Select the red square and move it by `dx` with the Transform tool.
    fn move_square(app: &mut PainterApp, dx: f32) {
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(10.0, 10.0),
                end: Vec2::new(30.0, 30.0),
            },
            SelectionMode::Replace,
        );
        app.active_tool = Tool::Transform(TransformInfo::default());
        transform_press(app, Vec2::new(20.0, 20.0));
        transform_drag(app, Vec2::new(20.0 + dx, 20.0), false);
        flush_transform_preview(app);
        transform_release(app);
        commit_floating_layer(app);
    }

    fn select_rect(app: &mut PainterApp, from: (f32, f32), to: (f32, f32)) {
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(from.0, from.1),
                end: Vec2::new(to.0, to.1),
            },
            SelectionMode::Replace,
        );
    }

    #[test]
    fn an_empty_selection_is_transformed_itself() {
        let mut app = app();
        // Nothing painted in 40..60 × 20..40.
        select_rect(&mut app, (40.0, 20.0), (60.0, 40.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        transform_press(&mut app, Vec2::new(50.0, 30.0));
        assert!(
            app.layer_state.floating_layer_idx.is_none(),
            "no pixels lifted"
        );
        assert!(transform_running(&app));
        assert!(
            matches!(app.active_tool, Tool::Transform(i) if i.bounds.is_some()),
            "a box to drag"
        );
        transform_drag(&mut app, Vec2::new(150.0, 30.0), false);
        transform_release(&mut app);
        assert!(
            app.selection_manager.contains(Vec2::new(50.0, 30.0)),
            "applied on commit"
        );
        commit_floating_layer(&mut app);
        assert!(!transform_running(&app));
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(150.0, 30.0)), "the outline moved");
        assert!(!sel.contains(Vec2::new(50.0, 30.0)));
        assert_eq!(pixel(&app, 1, 20, 20), RED, "the paint didn't move");
        assert_eq!(pixel(&app, 1, 148, 20), BLUE);
        // One undo step, back to the old outline; redo moves it again.
        app.apply_history(false);
        assert!(app.selection_manager.contains(Vec2::new(50.0, 30.0)));
        assert!(!app.selection_manager.contains(Vec2::new(150.0, 30.0)));
        app.apply_history(true);
        assert!(app.selection_manager.contains(Vec2::new(150.0, 30.0)));
    }

    #[test]
    fn cancelling_a_selection_transform_keeps_the_selection() {
        let mut app = app();
        select_rect(&mut app, (40.0, 20.0), (60.0, 40.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        transform_press(&mut app, Vec2::new(50.0, 30.0));
        transform_drag(&mut app, Vec2::new(150.0, 30.0), false);
        cancel_floating_layer(&mut app);
        assert!(!transform_running(&app));
        assert!(app.selection_manager.contains(Vec2::new(50.0, 30.0)));
        assert!(app.layer_state.history.stacks().0.is_empty());
        // Leaving the tool mid-drag applies it, like a pixel transform.
        transform_press(&mut app, Vec2::new(50.0, 30.0));
        transform_drag(&mut app, Vec2::new(60.0, 30.0), false);
        transform_release(&mut app);
        app.active_tool = Tool::Select(crate::selection::SelectionType::Rectangle);
        commit_floating_layer(&mut app);
        assert!(app.selection_manager.contains(Vec2::new(68.0, 30.0)));
    }

    #[test]
    fn a_position_locked_layer_stays_put() {
        let mut app = app();
        app.canvas_mut().layers[1].position_locked = true;
        // The whole layer: nothing floats, and a notice says why.
        app.active_tool = Tool::Transform(TransformInfo::default());
        transform_press(&mut app, Vec2::new(20.0, 20.0));
        transform_drag(&mut app, Vec2::new(60.0, 20.0), false);
        transform_release(&mut app);
        commit_floating_layer(&mut app);
        assert!(app.layer_state.floating_layer_idx.is_none());
        assert!(app.export_state.message.is_some());
        // A selection moves as an outline; the pixels stay.
        move_square(&mut app, 40.0);
        commit_floating_layer(&mut app);
        assert_eq!(app.canvas.layers.len(), 2);
        assert_eq!(pixel(&app, 1, 20, 20), RED);
        assert_eq!(pixel(&app, 1, 60, 20).a(), 0);
    }

    #[test]
    fn a_corner_never_folds_the_quad() {
        let mut app = app();
        select_rect(&mut app, (10.0, 10.0), (30.0, 30.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        create_floating_layer(&mut app);
        set_corner_mode(&mut app, Some(DistortKind::Perspective));
        let corners = |app: &PainterApp| match app.active_tool {
            Tool::Transform(i) => i.corners.unwrap(),
            _ => panic!(),
        };
        let before = corners(&app);
        // Grab the bottom-right corner and drag it past the top-left one.
        transform_press(&mut app, before[2]);
        transform_drag(&mut app, before[0] - Vec2::splat(10.0), false);
        assert_eq!(corners(&app), before, "the corner waits");
        // Back somewhere valid: it follows the pointer again.
        transform_drag(&mut app, before[2] + Vec2::new(8.0, 4.0), false);
        assert!((corners(&app)[2] - (before[2] + Vec2::new(8.0, 4.0))).length() < 1e-3);
        transform_release(&mut app);
    }

    #[test]
    fn distort_bends_the_pixels_and_the_selection_with_its_grid() {
        let mut app = app();
        select_rect(&mut app, (10.0, 10.0), (30.0, 30.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        create_floating_layer(&mut app);
        set_corner_mode(&mut app, Some(DistortKind::Warp));
        let grid = |app: &PainterApp| match app.active_tool {
            Tool::Transform(i) => i.warp.unwrap(),
            _ => panic!(),
        };
        assert_eq!(grid(&app).n, 4, "3×3 cells by default");
        // Drag a point of the right edge (row 1, column 3) out and down.
        let point = grid(&app).points[4 + 3];
        transform_press(&mut app, point);
        assert!(
            matches!(app.active_tool, Tool::Transform(i) if i.state == TransformState::Corner(7))
        );
        transform_drag(&mut app, point + Vec2::new(8.0, 5.0), false);
        transform_release(&mut app);
        flush_transform_preview(&mut app);
        // A finer grid keeps the bend.
        set_warp_size(&mut app, 6);
        assert_eq!(grid(&app).n, 6);
        commit_floating_layer(&mut app);
        // The square's right edge bulged out near that point; the corners
        // stayed where they were.
        assert_eq!(pixel(&app, 1, 34, 18), RED, "pushed out past the old edge");
        assert_eq!(pixel(&app, 1, 20, 20), RED);
        assert_eq!(pixel(&app, 1, 31, 10).a(), 0, "corner unmoved");
        let sel = &app.selection_manager;
        assert!(
            sel.contains(Vec2::new(34.0, 18.0)),
            "the selection bent too"
        );
        assert!(!sel.contains(Vec2::new(32.0, 10.5)));
        // The mode (and grid size) survive into the next session.
        assert!(matches!(app.active_tool, Tool::Transform(i)
            if i.distort_kind == DistortKind::Warp && i.warp.map(|w| w.n) == Some(6)));
        // Undo puts the square back.
        app.apply_history(false);
        assert_eq!(pixel(&app, 1, 34, 18).a(), 0);
    }

    #[test]
    fn a_distort_keeps_the_selection_on_the_pixels() {
        let mut app = app();
        select_rect(&mut app, (10.0, 10.0), (30.0, 30.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        create_floating_layer(&mut app);
        set_corner_mode(&mut app, Some(DistortKind::Perspective));
        // Push the right corners 40 px further right.
        if let Tool::Transform(ref mut info) = app.active_tool {
            let c = info.corners.as_mut().unwrap();
            c[1].x += 40.0;
            c[2].x += 40.0;
        }
        app.layer_state.transform_preview_pending = true;
        flush_transform_preview(&mut app);
        commit_floating_layer(&mut app);
        let sel = &app.selection_manager;
        assert!(sel.has_selection(), "not dropped");
        assert!(
            sel.contains(Vec2::new(60.0, 20.0)),
            "stretched with the pixels"
        );
        assert!(!sel.contains(Vec2::new(80.0, 20.0)));
        assert_eq!(pixel(&app, 1, 60, 20), RED);
    }

    #[test]
    fn a_gamma_document_commits_what_the_preview_showed() {
        let mut app = app();
        app.canvas_mut().blend_space = crate::canvas::blend_modes::BlendSpace::Gamma;
        // Half-transparent black at 10..30, opaque white at 50..70.
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        for y in 10..30 {
            for x in 10..30 {
                tile[y * 64 + x] = Color32::from_black_alpha(128);
            }
            for x in 50..64 {
                tile[y * 64 + x] = Color32::WHITE;
            }
        }
        app.canvas_mut().set_layer_tile_data(1, 0, 0, tile);
        select_rect(&mut app, (10.0, 10.0), (30.0, 30.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        transform_press(&mut app, Vec2::new(20.0, 20.0));
        transform_drag(&mut app, Vec2::new(60.0, 20.0), false);
        flush_transform_preview(&mut app);
        transform_release(&mut app);
        commit_floating_layer(&mut app);
        // Black at half alpha over white, mixed as sRGB values: mid grey
        // (mixed in linear light it would be 187).
        let p = pixel(&app, 1, 55, 20);
        assert_eq!(p.a(), 255);
        assert!(p.r().abs_diff(127) <= 1, "{p:?}");
    }

    #[test]
    fn leaving_the_tool_applies_the_move_as_an_undo_step() {
        let mut app = app();
        select_rect(&mut app, (10.0, 10.0), (30.0, 30.0));
        app.active_tool = Tool::Transform(TransformInfo::default());
        transform_press(&mut app, Vec2::new(20.0, 20.0));
        transform_drag(&mut app, Vec2::new(60.0, 20.0), false);
        flush_transform_preview(&mut app);
        transform_release(&mut app);
        // Picking another tool: the frame's settle step commits.
        app.active_tool = Tool::Brush;
        commit_floating_layer(&mut app);
        assert_eq!(pixel(&app, 1, 60, 20), RED);
        assert!(
            app.selection_manager.contains(Vec2::new(60.0, 20.0)),
            "selection followed"
        );
        app.apply_history(false);
        assert_eq!(pixel(&app, 1, 20, 20), RED, "undo puts it back");
        assert_eq!(pixel(&app, 1, 60, 20).a(), 0);
    }

    #[test]
    fn undoing_a_move_onto_paint_keeps_the_paint_that_was_there() {
        let mut app = app();
        move_square(&mut app, 128.0);
        assert_eq!(pixel(&app, 1, 148, 20), RED, "moved onto the blue");
        assert_eq!(pixel(&app, 1, 20, 20).a(), 0, "lifted from its place");
        app.apply_history(false);
        assert_eq!(pixel(&app, 1, 20, 20), RED, "back where it was");
        assert_eq!(pixel(&app, 1, 148, 20), BLUE, "the blue under it is back");
        assert_eq!(pixel(&app, 1, 180, 50), BLUE, "the rest of the blue too");
        app.apply_history(true);
        assert_eq!(pixel(&app, 1, 148, 20), RED, "redo moves it again");
        assert_eq!(pixel(&app, 1, 180, 50), BLUE);
        assert_eq!(pixel(&app, 1, 20, 20).a(), 0);
    }
}
