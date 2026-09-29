//! The Layers panel: the layer tree with folders and masks, drag to
//! reorder, visibility, opacity, blend mode and locks.

use crate::PainterApp;
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::storage::{LayerId, LayerKind};
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::*;
use crate::ui::widgets::{draw_checkerboard, icon_button, icon_toggle, percent_of_unit};
use eframe::egui::{self, Color32, RichText, Stroke};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Thumbnail box; the image keeps the canvas aspect ratio inside it.
const THUMB_W: f32 = 44.0;
const THUMB_H: f32 = 34.0;
/// Thumbnail texture resolution (2× the box, for HiDPI).
const THUMB_PX_W: f32 = THUMB_W * 2.0;
const THUMB_PX_H: f32 = THUMB_H * 2.0;
/// Minimum time between thumbnail rebuilds.
const THUMB_INTERVAL: Duration = Duration::from_millis(300);

/// Default row tint; a layer whose UI color differs gets a colored tag.
const UNTAGGED: Color32 = Color32::from_gray(40);

/// Rebuild layer thumbnails when the canvas changed, at most every
/// [`THUMB_INTERVAL`] and never mid-stroke (the stroke worker holds tiles).
pub fn refresh_thumbnails(app: &mut PainterApp, ctx: &egui::Context) {
    let layer_count = app.canvas.layers.len();
    let state = &mut app.layer_state;
    let stale = state.thumbnails.len() != layer_count;
    if !(state.thumbnails_dirty || stale) || app.brush_state.is_drawing {
        return;
    }
    if let Some(built) = state.thumbnails_built_at
        && built.elapsed() < THUMB_INTERVAL
        && !stale
    {
        ctx.request_repaint_after(THUMB_INTERVAL - built.elapsed());
        return;
    }

    let canvas = &app.canvas;
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    let scale = (THUMB_PX_W / w).min(THUMB_PX_H / h);
    let tw = ((w * scale).round() as usize).max(1);
    let th = ((h * scale).round() as usize).max(1);
    let tile_size = canvas.tile_size();

    // Which thumbnail pixels sample which tile, so each tile locks once.
    let mut by_tile: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();
    for y in 0..th {
        let cy = (((y as f32 + 0.5) / scale) as usize).min(canvas.height() - 1);
        for x in 0..tw {
            let cx = (((x as f32 + 0.5) / scale) as usize).min(canvas.width() - 1);
            let local = (cy % tile_size) * tile_size + cx % tile_size;
            by_tile
                .entry((cx / tile_size, cy / tile_size))
                .or_default()
                .push((y * tw + x, local));
        }
    }

    state.thumbnails.resize_with(layer_count, || None);
    for idx in 0..layer_count {
        let kind = canvas.layers[idx].kind;
        if kind == LayerKind::Group {
            state.thumbnails[idx] = None; // folders show an icon
            continue;
        }
        let is_mask = matches!(kind, LayerKind::Mask { .. });
        // The background shows the clear color under its paint; a mask shows
        // its coverage as gray (white = shown), and missing tiles are white.
        let base = if idx == 0 {
            canvas.clear_color()
        } else if is_mask {
            Color32::WHITE
        } else {
            Color32::TRANSPARENT
        };
        let mut image = egui::ColorImage::new([tw, th], base);
        for (&(tx, ty), samples) in &by_tile {
            let Some(cell) = canvas.lock_layer_tile_if_exists(idx, tx, ty) else {
                continue;
            };
            let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty && !is_mask {
                continue;
            }
            if let Some(data) = &guard.data {
                for &(dst, src) in samples {
                    image.pixels[dst] = if is_mask {
                        let px = data[src];
                        Color32::from_gray(
                            ((px.r() as u16 + px.g() as u16 + px.b() as u16) / 3) as u8,
                        )
                    } else {
                        data[src]
                    };
                }
            }
        }
        match &mut state.thumbnails[idx] {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            slot => {
                *slot = Some(ctx.load_texture(
                    format!("layer_thumb_{idx}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ))
            }
        }
    }
    state.thumbnails_dirty = false;
    state.thumbnails_built_at = Some(Instant::now());
}

/// A row of the layers panel: a layer or folder (masks are drawn inside
/// their owner's row), in display order, with its nesting depth.
#[derive(Clone, Copy, Debug)]
struct RowInfo {
    idx: usize,
    id: LayerId,
    parent: Option<LayerId>,
    depth: usize,
    is_group: bool,
    rect: egui::Rect,
}

/// Layers and folders top-to-bottom as the panel shows them: each level
/// top of stack first, children under their open folder.
fn display_order(canvas: &crate::canvas::Canvas) -> Vec<(usize, usize)> {
    let ids: std::collections::HashSet<LayerId> = canvas.layers.iter().map(|l| l.id).collect();
    // A parent that no longer exists counts as top level, so nothing is lost.
    let parent_of = |i: usize| canvas.layers[i].parent.filter(|p| ids.contains(p));
    let mut out = Vec::new();
    fn visit(
        canvas: &crate::canvas::Canvas,
        parent: Option<LayerId>,
        depth: usize,
        parent_of: &dyn Fn(usize) -> Option<LayerId>,
        out: &mut Vec<(usize, usize)>,
    ) {
        if depth > canvas.layers.len() {
            return;
        }
        for i in (0..canvas.layers.len()).rev() {
            let layer = &canvas.layers[i];
            if parent_of(i) != parent || matches!(layer.kind, LayerKind::Mask { .. }) {
                continue;
            }
            out.push((i, depth));
            if layer.kind == LayerKind::Group && layer.expanded {
                visit(canvas, Some(layer.id), depth + 1, parent_of, out);
            }
        }
    }
    visit(canvas, None, 0, &parent_of, &mut out);
    out
}

/// Sidebar that manages the canvas layer stack.
pub fn layers_panel(ctx: &egui::Context, ui: &mut egui::Ui, app: &mut PainterApp) {
    // The layers stay as they are until the mask is turned back into the
    // selection.
    if app.workspace.select.quick_mask.is_some() {
        ui.label(RichText::new("Quick mask").strong());
        ui.label(
            RichText::new("Paint to select, erase to deselect. The red shows what isn't selected.")
                .color(TEXT_DIM),
        );
        if ui.button("Leave Quick Mask").clicked() {
            app.quick_mask_leave();
        }
        return;
    }
    let m = metrics(ctx);
    let row_height = m.layer_row_height;
    let mut add_layer = false;
    let mut add_folder = false;
    let mut add_mask = false;
    let mut to_delete = None;
    let mut duplicate: Option<usize> = None;
    let mut edit_text: Option<usize> = None;
    let mut rasterise_text: Option<usize> = None;
    let mut active_idx = app.canvas.active_layer_idx;
    let mut needs_refresh = false;
    let mut rows: Vec<RowInfo> = Vec::new();
    let mut pending_move: Option<(usize, usize, Option<LayerId>)> = None;
    let mut toggle_expanded: Option<usize> = None;
    let mut toggle_mask: Option<usize> = None;
    let mut select_paint: Option<(usize, crate::selection::SelectionMode)> = None;
    let renaming_id = ui.id().with("renaming_layer");
    let mut renaming: Option<usize> = ui.data(|d| d.get_temp(renaming_id));

    // The row that owns the selection (a selected mask highlights its layer).
    let active_owner = match app.canvas.layers.get(active_idx).map(|l| l.kind) {
        Some(LayerKind::Mask { owner }) => app.canvas.layer_index_of(owner).unwrap_or(active_idx),
        _ => active_idx,
    };

    // Header: counts and add/delete buttons.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let count = app
            .canvas
            .layers
            .iter()
            .filter(|l| l.kind == LayerKind::Paint)
            .count();
        ui.label(
            RichText::new(format!("{count} layers"))
                .small()
                .color(TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_delete = app.canvas.layers.len() > 1 && active_idx != 0;
            let delete = ui.add_enabled_ui(can_delete, |ui| {
                icon_button(
                    ui,
                    Icon::Trash,
                    m.header_button,
                    false,
                    "Delete (with its mask / folder contents)",
                )
            });
            if delete.inner.clicked() {
                to_delete = Some(active_idx);
            }
            let can_mask = active_owner != 0
                && app
                    .canvas
                    .layers
                    .get(active_owner)
                    .is_some_and(|l| l.kind == LayerKind::Paint);
            let mask = ui.add_enabled_ui(can_mask, |ui| {
                icon_button(
                    ui,
                    Icon::Mask,
                    m.header_button,
                    false,
                    "Add layer mask (paint black to hide, white to show)",
                )
            });
            if mask.inner.clicked() {
                add_mask = true;
            }
            if icon_button(ui, Icon::Folder, m.header_button, false, "New folder").clicked() {
                add_folder = true;
            }
            if icon_button(
                ui,
                Icon::Plus,
                m.header_button,
                false,
                "New layer (Ctrl+Shift+N)",
            )
            .clicked()
            {
                add_layer = true;
            }
        });
    });
    // Blend mode of the selected layer or folder.
    if let Some(layer) = app.canvas.layers.get(active_owner) {
        let mut blend = layer.blend;
        let mut alpha_locked = layer.alpha_locked;
        let mut clipped = layer.clipped;
        let can_alpha_lock = layer.kind == LayerKind::Paint;
        let can_clip = active_owner != 0;
        let changed = ui
            .horizontal(|ui| {
                if can_alpha_lock {
                    ui.toggle_value(&mut alpha_locked, "α Lock").on_hover_text(
                        "Lock transparency (/): paint and fills only recolour what's already there",
                    );
                }
                if can_clip {
                    ui.toggle_value(&mut clipped, "Clip").on_hover_text(
                        "Clip to the layer below (Ctrl+Alt+G): show only where it has paint",
                    );
                }
                ui.label(RichText::new("Blend").color(TEXT_DIM));
                blend_mode_picker(ui, &mut blend)
            })
            .inner;
        if changed {
            app.canvas_mut().layers[active_owner].blend = blend;
            needs_refresh = true;
        }
        if clipped != app.canvas.layers[active_owner].clipped {
            app.canvas_mut().layers[active_owner].clipped = clipped;
            needs_refresh = true;
        }
        if alpha_locked != app.canvas.layers[active_owner].alpha_locked {
            app.canvas_mut().layers[active_owner].alpha_locked = alpha_locked;
        }
        layer_flags(app, ui, active_owner);
    }
    ui.add_space(2.0);

    let order = display_order(&app.canvas);
    egui::ScrollArea::vertical()
        .id_salt("layers_scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for &(i, depth) in &order {
                let mut vis_changed = false;
                let mut opacity_released = false;
                // Widgets edit copies; the canvas is only borrowed exclusively
                // (which ends an in-progress stroke) when something changed.
                let current = &app.canvas.layers[i];
                let mut edited = (
                    current.visible,
                    current.locked,
                    current.name.clone(),
                    current.opacity,
                );
                let alpha_locked = current.alpha_locked;
                let (position_locked, draft, reference) =
                    (current.position_locked, current.draft, current.reference);
                let clipped = current.clipped;
                let adjustment = current.adjustment.is_some();
                let is_text = current.text.is_some();
                let (id, kind, parent, expanded, blend) = (
                    current.id,
                    current.kind,
                    current.parent,
                    current.expanded,
                    current.blend,
                );
                let is_group = kind == LayerKind::Group;
                let mask_idx = app.canvas.mask_index_of(id);
                let is_active = i == active_owner;

                let (row, row_response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_height),
                    egui::Sense::click_and_drag(),
                );
                rows.push(RowInfo { idx: i, id, parent, depth, is_group, rect: row });

                let bg = if is_active {
                    BG_RAISED
                } else if row_response.hovered() {
                    Color32::from_gray(40)
                } else {
                    BG_PANEL
                };
                ui.painter().rect_filled(row, 0.0, bg);
                if is_active {
                    let bar = egui::Rect::from_min_size(row.min, egui::vec2(3.0, row.height()));
                    ui.painter().rect_filled(bar, 0.0, ACCENT);
                }

                let indent = depth as f32 * INDENT;
                let mut content = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(row.shrink2(egui::vec2(8.0, 4.0)).with_min_x(row.left() + 8.0 + indent))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                content.spacing_mut().item_spacing.x = 4.0;
                // Nesting guide for rows inside folders.
                if depth > 0 {
                    let x = row.left() + 8.0 + indent - INDENT * 0.5;
                    ui.painter().vline(x, row.y_range(), Stroke::new(1.0_f32, BORDER_LIGHT));
                }
                let (visible, locked, name, opacity) = &mut edited;

                vis_changed |= icon_toggle(&mut content, visible, Icon::Eye, Icon::EyeOff, "Toggle visibility");
                if is_group {
                    // Disclosure arrow + folder icon in place of lock/thumbnail.
                    let (arrow_rect, arrow) = content
                        .allocate_exact_size(egui::vec2(m.row_toggle, m.row_toggle), egui::Sense::click());
                    paint_disclosure(content.painter(), arrow_rect, expanded, arrow.hovered());
                    if arrow.on_hover_text(if expanded { "Close folder" } else { "Open folder" }).clicked() {
                        toggle_expanded = Some(i);
                    }
                    let (folder_rect, _) =
                        content.allocate_exact_size(egui::vec2(THUMB_W, THUMB_H), egui::Sense::hover());
                    paint_icon(content.painter(), folder_rect.shrink(4.0), Icon::Folder, TEXT_DIM);
                } else {
                    icon_toggle(&mut content, locked, Icon::Lock, Icon::Unlock, "Lock layer");
                    let layer_targeted = active_idx == i;
                    let thumb = thumbnail(&mut content, app, i, layer_targeted && mask_idx.is_some());
                    if thumb.clicked() {
                        match paint_select_mode(&content) {
                            Some(mode) => select_paint = Some((i, mode)),
                            None => active_idx = i,
                        }
                    }
                    if let Some(mi) = mask_idx {
                        let enabled = app.canvas.layers[mi].visible;
                        let mask_thumb = thumbnail(&mut content, app, mi, active_idx == mi);
                        if !enabled {
                            let r = mask_thumb.rect;
                            content.painter().line_segment([r.left_top(), r.right_bottom()], Stroke::new(2.0_f32, Color32::from_rgb(214, 76, 76)));
                        }
                        let tip = "Mask: click to paint it (black hides, white shows). Double-click to enable/disable.";
                        let mask_thumb = mask_thumb.on_hover_text(tip);
                        if mask_thumb.double_clicked() {
                            toggle_mask = Some(mi);
                        } else if mask_thumb.clicked() {
                            match paint_select_mode(&content) {
                                Some(mode) => select_paint = Some((mi, mode)),
                                None => active_idx = mi,
                            }
                        }
                    }
                    // Color tag strip left of the thumbnails.
                    let tag = app.layer_state.layer_ui_colors.get(i).copied().unwrap_or(UNTAGGED);
                    if tag != UNTAGGED {
                        let strip = egui::Rect::from_min_size(
                            egui::pos2(row.left() + 4.0 + indent, row.top() + 6.0),
                            egui::vec2(3.0, row.height() - 12.0),
                        );
                        ui.painter().rect_filled(strip, 0.0, tag);
                    }
                }

                // Name (double-click to rename) above an opacity slider.
                content.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let width = ui.available_width();
                    if renaming == Some(i) {
                        let response = ui.add(
                            egui::TextEdit::singleline(name)
                                .desired_width(width)
                                .hint_text("Layer name"),
                        );
                        if !response.has_focus() && !response.lost_focus() {
                            response.request_focus();
                        }
                        if response.lost_focus() {
                            renaming = None;
                        }
                    } else {
                        let color = if is_active { TEXT_STRONG } else { TEXT };
                        let mut text = RichText::new(name.as_str()).color(color);
                        if is_group {
                            text = text.strong();
                        }
                        ui.horizontal(|ui| {
                            if clipped {
                                ui.label(RichText::new("↓").color(ACCENT))
                                    .on_hover_text("Clipped to the layer below");
                            }
                            if is_text {
                                ui.label(RichText::new("T").strong().color(ACCENT))
                                    .on_hover_text("Text layer: double-click to edit the text");
                            }
                            ui.add(egui::Label::new(text).truncate());
                            if blend != LayerBlend::Normal {
                                ui.label(RichText::new(blend.label()).small().color(ACCENT));
                            }
                            if adjustment {
                                ui.label(egui::RichText::new("◐").color(ACCENT))
                                    .on_hover_text("Adjustment layer: double-click to change it");
                            }
                            if alpha_locked {
                                ui.label(RichText::new("α").small().color(ACCENT))
                                    .on_hover_text("Transparency locked");
                            }
                            if position_locked {
                                ui.label(RichText::new("pos").small().color(ACCENT))
                                    .on_hover_text("Position locked: can't be moved or transformed");
                            }
                            if draft {
                                ui.label(RichText::new("draft").small().color(ACCENT))
                                    .on_hover_text("Draft: left out of export, merging and \"all layers\"");
                            }
                            if reference {
                                ui.label(RichText::new("ref").small().color(ACCENT))
                                    .on_hover_text("Reference layer for fills and the magic wand");
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        if !m.touch {
                            ui.spacing_mut().interact_size.y = 16.0;
                        }
                        ui.spacing_mut().slider_width = (width - 44.0).max(40.0);
                        let response = ui.add(crate::ui::widgets::reset(&mut *opacity, |v| percent_of_unit(egui::Slider::new(v, 0.0..=1.0))));
                        opacity_released =
                            response.drag_stopped() || (response.changed() && !response.dragged());
                    });
                });

                if row_response.clicked() {
                    active_idx = i;
                }
                // Double-click renames (folders open and close with their
                // arrow).
                if row_response.double_clicked() {
                    active_idx = i;
                    if adjustment {
                        app.workspace.filter.editing = Some(id);
                    } else if is_text {
                        edit_text = Some(i);
                    } else {
                        renaming = Some(i);
                    }
                }
                // The background stays at the bottom.
                if row_response.drag_started() && i != 0 {
                    app.layer_state.layer_dragging = Some(i);
                }

                row_response.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        active_idx = i;
                        renaming = Some(i);
                        ui.close_menu();
                    }
                    if adjustment && ui.button("Edit adjustment…").clicked() {
                        app.workspace.filter.editing = Some(id);
                        ui.close_menu();
                    }
                    if is_text && ui.button("Edit text…").clicked() {
                        edit_text = Some(i);
                        ui.close_menu();
                    }
                    if is_text && ui.button("Rasterise text").clicked() {
                        rasterise_text = Some(i);
                        ui.close_menu();
                    }
                    if !is_group && i != 0 && ui.button("Duplicate").clicked() {
                        duplicate = Some(i);
                        ui.close_menu();
                    }
                    if !is_group && i != 0 {
                        match mask_idx {
                            None => {
                                if ui.button("Add mask").clicked() {
                                    active_idx = i;
                                    add_mask = true;
                                    ui.close_menu();
                                }
                            }
                            Some(mi) => {
                                let enabled = app.canvas.layers[mi].visible;
                                if ui.button(if enabled { "Disable mask" } else { "Enable mask" }).clicked() {
                                    toggle_mask = Some(mi);
                                    ui.close_menu();
                                }
                                if ui.button("Delete mask").clicked() {
                                    to_delete = Some(mi);
                                    ui.close_menu();
                                }
                            }
                        }
                    }
                    if let Some(color) = app.layer_state.layer_ui_colors.get_mut(i)
                        && color_tag_picker(ui, color)
                    {
                        ui.close_menu();
                    }
                    ui.separator();
                    let can_delete = app.canvas.layers.len() > 1 && i != 0;
                    let label = if is_group { "Delete folder and contents" } else { "Delete" };
                    if ui.add_enabled(can_delete, egui::Button::new(label)).clicked() {
                        to_delete = Some(i);
                        ui.close_menu();
                    }
                });

                if let Some(layer) = app.canvas.layers.get(i) {
                    let (visible, locked, name, opacity) = edited;
                    if (visible, locked, opacity) != (layer.visible, layer.locked, layer.opacity)
                        || name != layer.name
                    {
                        let layer = &mut app.canvas_mut().layers[i];
                        layer.visible = visible;
                        layer.locked = locked;
                        layer.name = name;
                        layer.opacity = opacity;
                    }
                }
                if vis_changed || opacity_released {
                    needs_refresh = true;
                    app.mark_layer_tiles_with_data_dirty(i);
                }
            }

            // The drop is resolved here, once every row has a rect: while
            // rows are still being laid out, the ones below aren't known yet.
            if let Some(from) = app.layer_state.layer_dragging {
                let (pointer, released) = ctx.input(|i| {
                    (i.pointer.interact_pos(), !i.pointer.primary_down())
                });
                let canvas = &app.canvas;
                let target = pointer.and_then(|p| {
                    drop_target(
                        &rows,
                        from,
                        p.y,
                        |id, ancestor| canvas.is_within(id, ancestor),
                        |folder| last_child_index(canvas, folder),
                    )
                });
                if released {
                    app.layer_state.layer_dragging = None;
                    if let Some(t) = target
                        && !t.is_noop(canvas, from)
                    {
                        pending_move = Some((from, t.to, t.parent));
                    }
                } else if let Some(pointer) = pointer {
                    paint_drag_feedback(ui, app, &rows, from, target, pointer, row_height);
                    autoscroll(ui, pointer);
                    ctx.request_repaint();
                }
            }
        });

    if let Some((i, mode)) = select_paint {
        app.select_layer_paint(i, mode);
    }
    // Structural changes after the rows, so this frame's edits were written
    // to the right layers before indices shift.
    if let Some(i) = toggle_expanded {
        let layer = &mut app.canvas_mut().layers[i];
        layer.expanded = !layer.expanded;
    }
    if let Some(mi) = toggle_mask {
        let layer = &mut app.canvas_mut().layers[mi];
        layer.visible = !layer.visible;
        needs_refresh = true;
    }
    if let Some((from, to, parent)) = pending_move {
        app.move_layer(from, to, parent);
        active_idx = app.canvas.active_layer_idx;
        renaming = None;
        needs_refresh = true;
    }

    ui.data_mut(|d| match renaming {
        Some(i) => d.insert_temp(renaming_id, i),
        None => d.remove::<usize>(renaming_id),
    });

    if active_idx != app.canvas.active_layer_idx {
        app.canvas_mut().active_layer_idx = active_idx;
    }

    if add_layer {
        app.add_layer_and_select();
    }
    if add_folder {
        app.add_folder();
    }
    if add_mask {
        app.add_mask_to_active();
    }
    if let Some(idx) = duplicate {
        app.canvas_mut().active_layer_idx = idx;
        app.duplicate_layer();
    }
    if let Some(idx) = edit_text {
        app.text_edit_layer(idx);
    }
    if let Some(idx) = rasterise_text {
        app.rasterise_text_layer(idx);
        needs_refresh = true;
    }
    if let Some(idx) = to_delete {
        app.remove_layer(idx);
        renaming = None;
        let _ = renaming;
        needs_refresh = true;
    }

    if needs_refresh {
        app.mark_all_tiles_dirty();
        app.layer_state.thumbnails_dirty = true;
        ctx.request_repaint();
    }
}

/// Lock position, draft and reference toggles for entry `idx` (a layer or
/// folder; the background can't be a draft).
fn layer_flags(app: &mut PainterApp, ui: &mut egui::Ui, idx: usize) {
    let layer = &app.canvas.layers[idx];
    let (mut position_locked, mut draft, mut reference) =
        (layer.position_locked, layer.draft, layer.reference);
    ui.horizontal(|ui| {
        ui.toggle_value(&mut position_locked, "Lock position")
            .on_hover_text("The layer can't be moved or transformed");
        if idx != 0 {
            ui.toggle_value(&mut draft, "Draft").on_hover_text(
                "Shown, but left out of export, merging, copy merged and \"all layers\" sampling",
            );
        }
        ui.toggle_value(&mut reference, "Reference").on_hover_text(
            "Fills and the magic wand set to \"Reference\" find their areas in this layer",
        );
    });
    let layer = &app.canvas.layers[idx];
    if (position_locked, draft, reference) != (layer.position_locked, layer.draft, layer.reference)
    {
        let layer = &mut app.canvas_mut().layers[idx];
        layer.position_locked = position_locked;
        layer.draft = draft;
        layer.reference = reference;
    }
}

/// Drop-down of every blend mode, in Photoshop's groups. Returns whether
/// the choice changed.
fn blend_mode_picker(ui: &mut egui::Ui, blend: &mut LayerBlend) -> bool {
    let before = *blend;
    egui::ComboBox::from_id_salt("layer_blend_mode")
        .selected_text(blend.label())
        .width(ui.available_width())
        .height(600.0)
        .show_ui(ui, |ui| {
            for (i, group) in LayerBlend::GROUPS.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                for &mode in *group {
                    ui.selectable_value(blend, mode, mode.label());
                }
            }
        });
    *blend != before
}

/// Indentation per folder level.
const INDENT: f32 = 16.0;

/// A layer (or mask) thumbnail button; `targeted` outlines it as the
/// painting target.
/// Ctrl+click on a thumbnail selects the layer's paint: Shift adds it to the
/// selection, Alt takes it away, both keep the overlap.
fn paint_select_mode(ui: &egui::Ui) -> Option<crate::selection::SelectionMode> {
    use crate::selection::SelectionMode;
    let m = ui.input(|i| i.modifiers);
    m.command.then_some(match (m.shift, m.alt) {
        (true, true) => SelectionMode::Intersect,
        (true, false) => SelectionMode::Add,
        (false, true) => SelectionMode::Subtract,
        (false, false) => SelectionMode::Replace,
    })
}

fn thumbnail(ui: &mut egui::Ui, app: &PainterApp, idx: usize, targeted: bool) -> egui::Response {
    let (thumb_box, response) =
        ui.allocate_exact_size(egui::vec2(THUMB_W, THUMB_H), egui::Sense::click());
    if let Some(texture) = app.layer_state.thumbnails.get(idx).and_then(|t| t.as_ref()) {
        let size = texture.size_vec2();
        let fit = (THUMB_W / size.x).min(THUMB_H / size.y);
        let img_rect = egui::Rect::from_center_size(thumb_box.center(), size * fit);
        draw_checkerboard(ui.painter(), img_rect, 4.0);
        ui.painter().image(
            texture.id(),
            img_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        let stroke = if targeted {
            Stroke::new(2.0_f32, ACCENT)
        } else {
            Stroke::new(1.0_f32, BORDER)
        };
        ui.painter().rect_stroke(
            img_rect.expand(if targeted { 1.0 } else { 0.0 }),
            0.0,
            stroke,
        );
    } else {
        ui.painter().rect_filled(thumb_box, 0.0, BG_INSET);
    }
    response
}

/// Right-pointing (closed) or down-pointing (open) folder arrow.
fn paint_disclosure(painter: &egui::Painter, rect: egui::Rect, open: bool, hovered: bool) {
    let c = rect.center();
    let r = rect.width() * 0.2;
    let points = if open {
        vec![
            c + egui::vec2(-r, -r * 0.6),
            c + egui::vec2(r, -r * 0.6),
            c + egui::vec2(0.0, r * 0.8),
        ]
    } else {
        vec![
            c + egui::vec2(-r * 0.6, -r),
            c + egui::vec2(r * 0.8, 0.0),
            c + egui::vec2(-r * 0.6, r),
        ]
    };
    let color = if hovered { TEXT_STRONG } else { TEXT_DIM };
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// Index of the topmost direct child of `folder`, if it has any.
fn last_child_index(canvas: &crate::canvas::Canvas, folder: LayerId) -> Option<usize> {
    canvas
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.parent == Some(folder) && !matches!(l.kind, LayerKind::Mask { .. }))
        .map(|(i, _)| i)
        .max()
}

/// Tag colors offered in a layer's context menu (none first).
const TAG_COLORS: [(Color32, &str); 8] = [
    (UNTAGGED, "None"),
    (Color32::from_rgb(214, 76, 76), "Red"),
    (Color32::from_rgb(222, 138, 58), "Orange"),
    (Color32::from_rgb(214, 190, 64), "Yellow"),
    (Color32::from_rgb(92, 176, 96), "Green"),
    (Color32::from_rgb(70, 140, 222), "Blue"),
    (Color32::from_rgb(150, 100, 210), "Violet"),
    (Color32::from_rgb(140, 140, 140), "Gray"),
];

/// A row of one-click tag swatches. A full color picker here would open a
/// second popup, and clicking in it closes the context menu (and the picker
/// with it). Returns whether a tag was chosen.
fn color_tag_picker(ui: &mut egui::Ui, current: &mut Color32) -> bool {
    let mut chosen = false;
    ui.label(RichText::new("Color tag").small().color(TEXT_DIM));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let side = if metrics(ui.ctx()).touch { 30.0 } else { 18.0 };
        for (color, name) in TAG_COLORS {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            if color == UNTAGGED {
                ui.painter()
                    .rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(1.0_f32, TEXT_DIM));
                ui.painter().line_segment(
                    [rect.left_bottom(), rect.right_top()],
                    Stroke::new(1.0_f32, TEXT_DIM),
                );
            } else {
                ui.painter().rect_filled(rect, 0.0, color);
            }
            if *current == color || response.hovered() {
                ui.painter()
                    .rect_stroke(rect.expand(1.5), 0.0, Stroke::new(1.5_f32, TEXT_STRONG));
            }
            if response.on_hover_text(name).clicked() {
                *current = color;
                chosen = true;
            }
        }
    });
    chosen
}

/// How a dragged layer would be dropped.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DropKind {
    /// Between rows, at screen height `y`, indented to `depth`.
    Gap { y: f32, depth: usize },
    /// Into the folder shown at `rows[row]`, on top of its contents.
    Into { row: usize },
}

/// Where a dragged layer would land.
#[derive(Clone, Copy, Debug)]
struct DropTarget {
    /// Final layer index after the move (as `Vec::remove` + `insert`).
    to: usize,
    /// Folder it ends up in.
    parent: Option<LayerId>,
    kind: DropKind,
    /// Row named in the caption ("above X" / "into X").
    caption_row: usize,
}

impl DropTarget {
    fn is_noop(&self, canvas: &crate::canvas::Canvas, from: usize) -> bool {
        self.to == from
            && canvas
                .layers
                .get(from)
                .is_some_and(|l| l.parent == self.parent)
    }
}

/// Drop target for dragging layer `from` to pointer height `y`. `rows` are
/// in display order. Over the middle of a folder row the layer goes into
/// the folder; otherwise it goes into the gap between rows, directly above
/// the row below the gap and in that row's folder.
fn drop_target(
    rows: &[RowInfo],
    from: usize,
    y: f32,
    is_within: impl Fn(LayerId, LayerId) -> bool,
    last_child: impl Fn(LayerId) -> Option<usize>,
) -> Option<DropTarget> {
    let dragged = rows.iter().find(|r| r.idx == from)?;
    let final_index = |insert_at: usize| {
        if insert_at > from {
            insert_at - 1
        } else {
            insert_at
        }
    };

    // Into a folder: the middle half of its row.
    if let Some((pos, row)) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.rect.y_range().contains(y))
        && row.is_group
        && row.idx != from
    {
        let band = row.rect.height() * 0.25;
        if y > row.rect.top() + band && y < row.rect.bottom() - band {
            if is_within(row.id, dragged.id) {
                return None; // a folder can't go inside itself
            }
            let insert_at = last_child(row.id).map_or(row.idx, |c| c + 1);
            return Some(DropTarget {
                to: final_index(insert_at),
                parent: Some(row.id),
                kind: DropKind::Into { row: pos },
                caption_row: pos,
            });
        }
    }

    // Between rows. Below the last row (the background) is not allowed, so
    // that gap snaps to just above it.
    let slot = rows
        .iter()
        .filter(|r| r.rect.center().y < y)
        .count()
        .min(rows.len() - 1);
    let below = rows[slot];
    let gap_y = if slot == 0 {
        below.rect.top()
    } else {
        (rows[slot - 1].rect.bottom() + below.rect.top()) * 0.5
    };
    if let Some(p) = below.parent
        && is_within(p, dragged.id)
    {
        return None;
    }
    Some(DropTarget {
        to: final_index(below.idx + 1),
        parent: below.parent,
        kind: DropKind::Gap {
            y: gap_y,
            depth: below.depth,
        },
        caption_row: slot,
    })
}

/// Dim the dragged row, show where it would land (a line between rows, or
/// an outline around the folder it would go into), and a floating label
/// naming the layer it would sit above or the folder it would enter.
fn paint_drag_feedback(
    ui: &egui::Ui,
    app: &PainterApp,
    rows: &[RowInfo],
    from: usize,
    target: Option<DropTarget>,
    pointer: egui::Pos2,
    row_height: f32,
) {
    let Some(dragged) = rows.iter().find(|r| r.idx == from) else {
        return;
    };
    let from_rect = dragged.rect;
    let painter = ui.painter();
    painter.rect_filled(from_rect, 0.0, BG_CANVAS.gamma_multiply(0.7));

    let name_of = |idx: usize| {
        app.canvas
            .layers
            .get(idx)
            .map(|l| l.name.as_str())
            .unwrap_or("")
    };
    let moves = target.is_some_and(|t| !t.is_noop(&app.canvas, from));
    let caption = match target {
        Some(t) if moves => {
            let row = rows[t.caption_row];
            match t.kind {
                DropKind::Into { row: pos } => {
                    painter.rect_stroke(
                        rows[pos].rect.shrink(1.0),
                        0.0,
                        Stroke::new(2.0_f32, ACCENT),
                    );
                    format!("into {}", name_of(row.idx))
                }
                DropKind::Gap { y, depth } => {
                    let left = from_rect.left() + 8.0 + depth as f32 * INDENT;
                    let x = egui::Rangef::new(left, from_rect.right());
                    painter.hline(x, y, Stroke::new(3.0_f32, ACCENT));
                    for end in [x.min, x.max] {
                        let marker =
                            egui::Rect::from_center_size(egui::pos2(end, y), egui::vec2(7.0, 9.0));
                        painter.rect_filled(marker, 0.0, ACCENT);
                    }
                    format!("above {}", name_of(row.idx))
                }
            }
        }
        Some(_) => "no change".to_string(),
        None => "can't go inside itself".to_string(),
    };

    // Ghost row following the pointer, above everything else.
    let layer = egui::LayerId::new(egui::Order::Tooltip, ui.id().with("layer_drag_ghost"));
    let painter = ui.ctx().layer_painter(layer);
    let ghost = egui::Rect::from_min_size(
        egui::pos2(from_rect.left() + 12.0, pointer.y - row_height * 0.5),
        egui::vec2(from_rect.width() - 12.0, row_height),
    );
    painter.rect_filled(ghost, 0.0, BG_RAISED.gamma_multiply(0.95));
    painter.rect_stroke(ghost, 0.0, Stroke::new(1.0_f32, ACCENT));
    painter.text(
        ghost.left_center() + egui::vec2(12.0, -8.0),
        egui::Align2::LEFT_CENTER,
        name_of(from),
        egui::TextStyle::Body.resolve(ui.style()),
        TEXT_STRONG,
    );
    painter.text(
        ghost.left_center() + egui::vec2(12.0, 10.0),
        egui::Align2::LEFT_CENTER,
        caption,
        egui::TextStyle::Small.resolve(ui.style()),
        if moves { ACCENT } else { TEXT_DIM },
    );
}

/// Scroll the layer list when dragging near its top or bottom edge.
fn autoscroll(ui: &mut egui::Ui, pointer: egui::Pos2) {
    let clip = ui.clip_rect();
    let edge = 28.0;
    let speed = 8.0;
    if pointer.y < clip.top() + edge {
        ui.scroll_with_delta(egui::vec2(0.0, speed));
    } else if pointer.y > clip.bottom() - edge {
        ui.scroll_with_delta(egui::vec2(0.0, -speed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows 10pt tall in display order; `spec` is (idx, id, parent, is_group).
    fn rows(spec: &[(usize, u64, Option<u64>, bool)]) -> Vec<RowInfo> {
        spec.iter()
            .enumerate()
            .map(|(pos, &(idx, id, parent, is_group))| {
                let top = pos as f32 * 10.0;
                RowInfo {
                    idx,
                    id: LayerId(id),
                    parent: parent.map(LayerId),
                    depth: usize::from(parent.is_some()),
                    is_group,
                    rect: egui::Rect::from_min_max(
                        egui::pos2(0.0, top),
                        egui::pos2(100.0, top + 10.0),
                    ),
                }
            })
            .collect()
    }

    /// Flat stack: layers 3 (top) .. 0 (background).
    fn flat() -> Vec<RowInfo> {
        rows(&[
            (3, 3, None, false),
            (2, 2, None, false),
            (1, 1, None, false),
            (0, 0, None, false),
        ])
    }

    fn target(rows: &[RowInfo], from: usize, y: f32) -> Option<DropTarget> {
        drop_target(rows, from, y, |a, b| a == b, |_| None)
    }

    #[test]
    fn dragging_down_past_rows_below_moves_the_layer_down() {
        // Top layer dropped between layers 2 and 1 ends just above layer 1:
        // [bg, 1, 2, 3] -> [bg, 1, 3, 2].
        let t = target(&flat(), 3, 19.0).unwrap();
        assert_eq!(t.to, 2);
        assert_eq!(t.parent, None);
    }

    #[test]
    fn nothing_goes_below_the_background() {
        // Below the last row snaps to just above the background.
        assert_eq!(target(&flat(), 3, 39.0).unwrap().to, 1);
    }

    #[test]
    fn dragging_up_moves_the_layer_up() {
        let t = target(&flat(), 1, 1.0).unwrap();
        assert_eq!(t.to, 3);
        assert!(matches!(t.kind, DropKind::Gap { .. }));
    }

    #[test]
    fn dropping_on_the_middle_of_a_folder_goes_inside() {
        // Folder 3 (empty) above layers 2, 1 and the background.
        let r = rows(&[
            (3, 30, None, true),
            (2, 2, None, false),
            (1, 1, None, false),
            (0, 0, None, false),
        ]);
        let t = target(&r, 1, 5.0).unwrap();
        assert_eq!(t.parent, Some(LayerId(30)));
        assert!(matches!(t.kind, DropKind::Into { row: 0 }));
        // Near the folder row's edge it's a gap instead.
        let t = target(&r, 1, 9.5).unwrap();
        assert!(matches!(t.kind, DropKind::Gap { .. }));
    }

    #[test]
    fn a_gap_inside_an_open_folder_moves_into_it() {
        // Folder 4 containing layer 3, then layer 2, layer 1, background.
        let r = rows(&[
            (4, 40, None, true),
            (3, 3, Some(40), false),
            (2, 2, None, false),
            (1, 1, None, false),
            (0, 0, None, false),
        ]);
        // Between the folder row and its child: above layer 3, inside.
        let t = target(&r, 1, 10.0).unwrap();
        assert_eq!(t.parent, Some(LayerId(40)));
        // Inserted above index 3; removing layer 1 first shifts that to 3.
        assert_eq!(t.to, 3);
    }

    #[test]
    fn a_folder_cannot_move_inside_itself() {
        let r = rows(&[
            (4, 40, None, true),
            (3, 3, Some(40), false),
            (1, 1, None, false),
            (0, 0, None, false),
        ]);
        // Dropping the folder into the gap above its own child.
        assert!(drop_target(&r, 4, 10.0, |a, b| a == b, |_| Some(3)).is_none());
    }
}
