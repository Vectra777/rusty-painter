use crate::PainterApp;
use crate::app::stroke_ops::exclusive;
use crate::app::tools::Tool;
use crate::canvas::history::UndoAction;
use crate::canvas::storage::TransformParams;
use eframe::egui::{self, Vec2};

pub(crate) fn apply_simple_transform(app: &mut PainterApp, offset: Vec2) {
    app.release_canvas();
    let mut action = UndoAction {
        tiles: Vec::new(),
        selection: Some(app.selection_manager.current_shape.clone()),
        transform: None,
        layer_action: None,
    };

    let has_selection = app.selection_manager.has_selection();
    let selection = if has_selection {
        Some(&app.selection_manager)
    } else {
        None
    };

    let src_bounds = app
        .canvas
        .get_content_bounds(app.canvas.active_layer_idx, selection);

    let params = TransformParams::new(offset, 0.0, Vec2::new(1.0, 1.0), Vec2::new(0.0, 0.0));
    exclusive(&mut app.canvas).apply_transform(params, selection, Some(&mut action));

    push_history_if_changed(app, action);
    mark_transform_dirty(app, src_bounds, &params);
    app.selection_manager
        .apply_transform(offset, 0.0, Vec2::new(1.0, 1.0), Vec2::new(0.0, 0.0));
}

pub(crate) fn create_floating_layer(app: &mut PainterApp) {
    if app.selection_manager.has_selection() && app.layer_state.floating_layer_idx.is_none() {
        app.release_canvas();
        let sel_bounds = app.selection_manager.get_bounds();

        if let Some(idx) = exclusive(&mut app.canvas).float_selection(&app.selection_manager) {
            app.layer_state.floating_layer_idx = Some(idx);
            app.layer_state.floating_buffer = Some(app.canvas.capture_layer_pixels(idx));

            app.insert_layer_state(idx);

            if let Some(bounds) = sel_bounds {
                app.mark_tiles_in_bounds_dirty(bounds);
            } else {
                app.mark_all_tiles_dirty();
            }
        }
    }
}

pub(crate) fn commit_floating_layer(app: &mut PainterApp) {
    if let Some(idx) = app.layer_state.floating_layer_idx {
        let float_bounds = app.canvas.get_content_bounds(idx, None);
        let base_bounds = if idx > 0 {
            app.canvas.get_content_bounds(idx - 1, None)
        } else {
            None
        };

        let mut action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        app.canvas_mut().merge_layer_down(idx, Some(&mut action));
        app.layer_state.floating_layer_idx = None;
        app.layer_state.floating_buffer = None;
        app.selection_manager.clear_selection();

        if let Tool::Transform(ref mut info) = app.active_tool {
            *info = crate::selection::transform::TransformInfo::default();
        }

        app.remove_layer_state(idx);

        if action.layer_action.is_some() {
            let active = app.canvas.active_layer_idx;
            if let Some(hist) = app.layer_state.histories.get_mut(active) {
                hist.push_action(action);
            }
        }

        if let Some(b1) = float_bounds {
            if let Some(b2) = base_bounds {
                app.mark_tiles_in_bounds_dirty(b1.union(b2));
            } else {
                app.mark_tiles_in_bounds_dirty(b1);
            }
        } else if let Some(b2) = base_bounds {
            app.mark_tiles_in_bounds_dirty(b2);
        } else {
            app.mark_all_tiles_dirty();
        }
    }
}

pub(crate) fn has_transform(info: &crate::selection::transform::TransformInfo) -> bool {
    info.offset.x != 0.0
        || info.offset.y != 0.0
        || info.rotation != 0.0
        || info.scale.x != 1.0
        || info.scale.y != 1.0
}

pub(crate) fn get_transform_center(info: &crate::selection::transform::TransformInfo) -> Vec2 {
    if let Some(b) = info.bounds {
        Vec2::new(b.center().x, b.center().y)
    } else {
        Vec2::ZERO
    }
}

pub(crate) fn reset_transform(info: &mut crate::selection::transform::TransformInfo) {
    info.offset = Vec2::ZERO;
    info.rotation = 0.0;
    info.scale = Vec2::new(1.0, 1.0);
    info.bounds = None;
}

pub(crate) fn push_history_if_changed(app: &mut PainterApp, action: UndoAction) {
    if !action.tiles.is_empty()
        && let Some(history) = app
            .layer_state
            .histories
            .get_mut(app.canvas.active_layer_idx)
    {
        history.push_action(action);
    }
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
    let corners = [
        src_bounds.min,
        egui::pos2(src_bounds.max.x, src_bounds.min.y),
        src_bounds.max,
        egui::pos2(src_bounds.min.x, src_bounds.max.y),
    ];

    let (sin_r, cos_r) = params.rotation.sin_cos();
    let center = params.center;
    let scale = params.scale;
    let offset = params.offset;
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;

    for corner in corners {
        let dx = corner.x - center.x;
        let dy = corner.y - center.y;
        let sx = dx * scale.x;
        let sy = dy * scale.y;
        let rx = sx * cos_r - sy * sin_r;
        let ry = sx * sin_r + sy * cos_r;
        let tx = rx + center.x + offset.x;
        let ty = ry + center.y + offset.y;

        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx);
        max_y = max_y.max(ty);
    }

    let padding = 2.0;
    egui::Rect::from_min_max(
        egui::pos2(min_x - padding, min_y - padding),
        egui::pos2(max_x + padding, max_y + padding),
    )
}

pub(crate) fn update_rotation(
    info: &mut crate::selection::transform::TransformInfo,
    start: Vec2,
    current: Vec2,
) {
    if let Some(bounds) = info.bounds {
        let center = Vec2::new(bounds.center().x, bounds.center().y) + info.offset;
        let start_vec = start - center;
        let current_vec = current - center;
        let angle = current_vec.y.atan2(current_vec.x) - start_vec.y.atan2(start_vec.x);
        info.rotation += angle;
    }
}

pub(crate) fn update_scaling(
    info: &mut crate::selection::transform::TransformInfo,
    delta: Vec2,
    handle_idx: usize,
) {
    if let Some(bounds) = info.bounds {
        let (sin_r, cos_r) = info.rotation.sin_cos();
        let dx = delta.x * cos_r + delta.y * sin_r;
        let dy = -delta.x * sin_r + delta.y * cos_r;

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

        let w = bounds.width();
        let h = bounds.height();
        if w > 0.0 {
            info.scale.x += scale_delta.x / (w * 0.5);
        }
        if h > 0.0 {
            info.scale.y += scale_delta.y / (h * 0.5);
        }
        info.scale.x = info.scale.x.max(0.1);
        info.scale.y = info.scale.y.max(0.1);
    }
}

pub(crate) fn apply_live_transform_preview(
    app: &mut PainterApp,
    info: &crate::selection::transform::TransformInfo,
) {
    if app.layer_state.floating_buffer.is_none() {
        return;
    }
    app.release_canvas();
    if let Some(buffer) = &app.layer_state.floating_buffer
        && let Some(idx) = app.layer_state.floating_layer_idx
    {
        let center = get_transform_center(info);
        let params = TransformParams::new(info.offset, info.rotation, info.scale, center);

        exclusive(&mut app.canvas).preview_transform(idx, buffer, params);
        mark_transform_dirty(app, info.bounds, &params);
    }
}
