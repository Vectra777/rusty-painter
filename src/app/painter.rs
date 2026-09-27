use super::{
    layout,
    layout::ToolTab,
    painter_state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
};
use crate::app::input_handler;
use crate::app::render_helper;
use crate::app::stroke_ops::exclusive;
use crate::brush_engine::stroke_worker::StrokeWorker;
use crate::{canvas::Canvas, tablet::TabletInput, ui};
use eframe::egui;
use eframe::egui::{Color32, Vec2};
use egui_dock::DockState;
use std::sync::Arc;

use crate::selection::SelectionManager;

/// How long a frame waits for the stroke worker before drawing anyway.
const STROKE_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(5);

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
pub struct PainterApp {
    /// Shared with the stroke worker while a stroke is painted; use
    /// [`PainterApp::canvas_mut`] for exclusive access.
    pub(crate) canvas: Arc<Canvas>,
    pub(crate) stroke_worker: StrokeWorker,

    // Grouped state
    pub(crate) brush_state: BrushState,
    pub(crate) viewport: ViewportState,
    pub(crate) render_cache: RenderCache,
    pub(crate) layer_state: LayerState,
    pub(crate) modal_state: ModalState,
    pub(crate) export_state: ExportState,
    pub(crate) workspace: WorkspaceState,

    // Standalone components
    pub(crate) active_tool: super::tools::Tool,
    pub(crate) selection_manager: SelectionManager,
    pub(crate) dock_left: DockState<ToolTab>,
    pub(crate) dock_right: DockState<ToolTab>,
    pub(crate) tablet: Option<TabletInput>,
}

impl eframe::App for PainterApp {
    /// Handle UI, input, painting updates, and tile uploads each frame.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut needs_repaint = false;

        // Rebuild the style when touch mode changes (and on the first frame).
        let touch = self.workspace.touch_mode;
        if self.workspace.applied_touch_mode != Some(touch) {
            let first_frame = self.workspace.applied_touch_mode.is_none();
            crate::styling::apply_style(ctx, touch);
            // Small touch screens start with the brush panel tucked away,
            // and phone-sized ones with both panels.
            let width = ctx.screen_rect().width();
            if first_frame && touch && width < 1280.0 {
                self.workspace.show_left_panel = false;
            }
            if first_frame && width < layout::NARROW_WIDTH {
                self.workspace.show_left_panel = false;
                self.workspace.show_right_panel = false;
            }
            self.workspace.applied_touch_mode = Some(touch);
        }
        ui::style::set_touch_metrics(ctx, touch);
        layout::fit_panels_to_screen(self, ctx);
        let screen_size = ctx.screen_rect().size();
        let resized = self
            .workspace
            .screen_size
            .is_some_and(|prev| prev != screen_size);
        self.workspace.screen_size = Some(screen_size);
        self.selection_manager.canvas_size = [self.canvas.width(), self.canvas.height()];

        if super::shortcuts::handle_shortcuts(self, ctx) {
            needs_repaint = true;
        }

        // Poll export tasks
        if let Some(handle) = self.export_state.task.as_ref()
            && handle.is_finished()
        {
            let result = self
                .export_state
                .task
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("Export thread panicked".to_string()));
            self.export_state.in_progress = false;
            match result {
                Ok(msg) => {
                    self.export_state.message = Some(msg);
                    self.export_state.show_modal = false;
                }
                Err(err) => {
                    self.export_state.message = Some(err);
                }
            }
        }

        // Drain progress updates
        if let Some(rx) = &self.export_state.progress_rx {
            for update in rx.try_iter() {
                self.export_state.progress = update.progress;
                if let Some(msg) = update.message {
                    self.export_state.message = Some(msg);
                }
            }
        }

        ui::layers::refresh_thumbnails(self, ctx);

        // Bars first so they span the full window width; the tool strip is
        // added before the docks so it sits at the far left.
        // Tablets have the menus in a sheet over the bottom bar, and adjust
        // size/opacity with the canvas faders instead of the options bar.
        if !touch {
            ui::top_bar::menu_bar(self, ctx);
            ui::top_bar::options_bar(self, ctx);
        }
        ui::status_bar::status_bar(self, ctx);
        if touch {
            ui::top_bar::menu_sheet(self, ctx);
        }
        ui::toolbar::toolbar(self, ctx);

        layout::show_tool_docks(self, ctx);

        let canvas_frame = egui::Frame::none().fill(ui::style::BG_CANVAS);
        egui::CentralPanel::default()
            .frame(canvas_frame)
            .show(ctx, |ui| {
                // Keep the canvas fitted while the window settles (the window
                // manager may resize it after the first frame), until the user
                // moves the view themselves.
                let available = ui.available_size();
                if self.workspace.auto_fit && self.workspace.fitted_to != Some(available) {
                    let canvas_w = self.canvas.width() as f32;
                    let canvas_h = self.canvas.height() as f32;

                    let zoom_x = available.x / canvas_w;
                    let zoom_y = available.y / canvas_h;
                    self.viewport.zoom = zoom_x.min(zoom_y) * 0.9; // 90% fit
                    let canvas_size = egui::vec2(canvas_w, canvas_h) * self.viewport.zoom;
                    let offset = (available - canvas_size) * 0.5;
                    self.viewport.offset = Vec2 {
                        x: offset.x,
                        y: offset.y,
                    };
                    self.workspace.fitted_to = Some(available);
                }

                // Keep the canvas still on screen while a side panel slides
                // (the view offset is relative to the canvas area's corner),
                // and keep the view centered when the window is resized or
                // the screen rotates.
                let area = ui.max_rect();
                if let Some(prev) = self.viewport.canvas_area
                    && !self.workspace.auto_fit
                {
                    if resized {
                        self.viewport.offset += (area.size() - prev.size()) * 0.5;
                    } else {
                        self.viewport.offset -= area.min - prev.min;
                    }
                }

                let view = render_helper::draw_canvas(self, ui);
                self.viewport.canvas_area = Some(view.response.rect);
                // Pen samples first: touch handling needs to know the pen is
                // down to tell a resting palm from a finger.
                let pen = self
                    .tablet
                    .as_mut()
                    .map(|t| t.poll(ctx))
                    .unwrap_or_default();
                self.viewport.touch.pen_active =
                    !pen.is_empty() || self.tablet.as_ref().is_some_and(|t| t.pen_active());
                if super::touch::handle_touch(self, ctx, &view.response) {
                    needs_repaint = true;
                }
                if !view.response.hovered() {
                    self.viewport.cursor_canvas = None;
                }
                let picking = matches!(
                    self.active_tool,
                    super::tools::Tool::Eyedropper | super::tools::Tool::Fill
                ) || (matches!(self.active_tool, super::tools::Tool::Brush)
                    && ctx.input(|i| i.modifiers.alt));
                if picking && view.response.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                // Selection and liquify brushes: show their size under the pointer.
                let ring = match self.active_tool {
                    super::tools::Tool::Select(crate::selection::SelectionType::Brush) => {
                        Some(self.selection_manager.brush_radius)
                    }
                    super::tools::Tool::Liquify => Some(self.workspace.liquify.radius),
                    super::tools::Tool::Smudge | super::tools::Tool::Blur => {
                        Some(self.brush_state.brush.brush_options.diameter * 0.5)
                    }
                    _ => None,
                };
                if let Some(radius) = ring
                    && let Some(pos) = view.response.hover_pos()
                {
                    let r = radius * self.viewport.zoom;
                    let painter = ui.painter();
                    painter.circle_stroke(pos, r, egui::Stroke::new(2.0_f32, egui::Color32::BLACK));
                    painter.circle_stroke(pos, r, egui::Stroke::new(1.0_f32, egui::Color32::WHITE));
                }

                self.import_dropped_files(ctx);
                // Leaving the Transform tool applies the running transform.
                if self.layer_state.floating_layer_idx.is_some()
                    && !matches!(self.active_tool, super::tools::Tool::Transform(_))
                {
                    super::transform::commit_floating_layer(self);
                }
                // A magnetic outline in progress belongs to its tool; a
                // double-click closes it.
                let magnetic = matches!(
                    self.active_tool,
                    super::tools::Tool::Select(crate::selection::SelectionType::Magnetic)
                );
                if !magnetic {
                    self.workspace.select.magnetic = None;
                } else if view.response.hovered()
                    && ctx.input(|i| {
                        i.pointer
                            .button_double_clicked(egui::PointerButton::Primary)
                    })
                {
                    self.magnetic_close();
                }
                // Likewise for liquify.
                if self.layer_state.liquify.is_some()
                    && !matches!(self.active_tool, super::tools::Tool::Liquify)
                {
                    self.liquify_commit();
                }
                input_handler::handle_input(
                    self,
                    ctx,
                    &view.response,
                    view.origin,
                    view.canvas_center,
                    &pen,
                );
                // Overlay first: on a drag's first frame it takes over at once,
                // so the layer isn't CPU-rendered even once while dragging.
                super::transform::update_float_overlay(self, ctx);
                super::transform::flush_transform_preview(self);
                // Twirl / pinch / bloat keep working while the brush is held.
                if self.liquify_is_holding() && self.workspace.liquify.mode.is_continuous() {
                    let dt = ctx.input(|i| i.stable_dt).min(0.1);
                    self.liquify_hold(dt);
                    needs_repaint = true;
                }
                // Give the stroke worker a short budget to paint this frame's
                // samples so they usually show this frame; a heavy brush can't
                // stall the frame beyond it and simply shows up next frame.
                if self.brush_state.is_drawing {
                    self.stroke_worker.wait_idle_for(STROKE_FRAME_BUDGET);
                }
                if self.sync_stroke_worker() {
                    needs_repaint = true;
                }
                if self.render_cache.tiles.iter().any(|t| t.dirty) {
                    self.layer_state.thumbnails_dirty = true;
                }
                // Composite and paint after input, so this frame's dabs and any
                // pan/zoom show up in this frame.
                let (uploads, more_tiles) =
                    render_helper::update_dirty_textures(self, &view, ui.clip_rect());
                if more_tiles {
                    needs_repaint = true;
                }
                super::transform::float_overlay_uploaded(self, more_tiles);
                render_helper::paint_canvas(self, ui, &view, uploads);

                if self.brush_state.is_drawing {
                    needs_repaint = true;
                }

                // Overlays follow the canvas exactly (zoom, pan and rotation).
                let map = render_helper::screen_map(self, &view);
                if !matches!(self.active_tool, super::tools::Tool::Transform(_)) {
                    self.selection_manager
                        .draw_overlay(ui.painter(), map.zoom(), &|p| map.to_screen(p));
                }
                super::select_tool::draw_magnetic(self, ui.painter(), &|p| map.to_screen(p));

                super::transform::draw_float_overlay(self, ui.painter(), &map);
                self.draw_transform_overlay(ui.painter(), &map);
                // Enclose-and-fill lasso in progress.
                if matches!(self.active_tool, super::tools::Tool::Fill)
                    && self.workspace.fill.path.len() > 1
                {
                    let pts: Vec<egui::Pos2> = self
                        .workspace
                        .fill
                        .path
                        .iter()
                        .map(|&p| map.to_screen(p))
                        .collect();
                    let painter = ui.painter();
                    painter.add(egui::Shape::line(
                        pts.clone(),
                        egui::Stroke::new(3.0_f32, egui::Color32::BLACK),
                    ));
                    painter.add(egui::Shape::line(
                        pts.clone(),
                        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                    ));
                    if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
                        painter.line_segment(
                            [*a, *b],
                            egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(110)),
                        );
                    }
                }
                ui::canvas_sliders::canvas_sliders(self, ctx, view.response.rect);
            });

        ui::canvas_creation::canvas_creation_modal(self, ctx);
        ui::general_settings::general_settings_modal(self, ctx);
        ui::palette_window::palette_window(self, ctx);
        ui::image_gallery::image_gallery(self, ctx);
        ui::general_settings::shortcuts_window(self, ctx);
        ui::brush_list::presets_window(self, ctx);
        ui::export_modal::export_modal(self, ctx);

        // Single consolidated repaint request
        if needs_repaint {
            ctx.request_repaint();
        }
    }
}

impl PainterApp {
    /// Clear every layer to white. Not undoable.
    pub(crate) fn clear_canvas(&mut self) {
        self.canvas_mut().clear(Color32::WHITE);
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Undo (or redo) the last action on the active layer.
    pub(crate) fn apply_history(&mut self, redo: bool) {
        // Undo inside a magnetic outline takes back its last anchor.
        if self.workspace.select.magnetic.is_some() {
            if !redo {
                self.magnetic_undo_anchor();
            }
            return;
        }
        self.forget_last_pick();
        // A running liquify or transform session becomes a normal step first,
        // so undo takes it back and redo brings it again (cancelling it
        // outright left nothing to redo).
        if self.layer_state.liquify.is_some() {
            self.liquify_commit();
        }
        if self.layer_state.floating_layer_idx.is_some() {
            super::transform::commit_floating_layer(self);
        }
        // Finish any stroke first so it is in the history (and undoable).
        self.release_canvas();
        if self.canvas.active_layer_idx < self.layer_state.histories.len() {
            let active_idx = self.canvas.active_layer_idx;
            // Detach the active layer's History for the duration of the
            // call: `History::undo`/`redo` need `&mut Canvas` to reverse a
            // layer add/remove/move, and this avoids holding a live borrow
            // of `self.layer_state.histories` at the same time.
            let mut history = std::mem::take(&mut self.layer_state.histories[active_idx]);

            let (affected, layer_action) = if redo {
                history.redo(
                    exclusive(&mut self.canvas),
                    &mut self.selection_manager,
                    &mut self.active_tool,
                )
            } else {
                history.undo(
                    exclusive(&mut self.canvas),
                    &mut self.selection_manager,
                    &mut self.active_tool,
                )
            };

            // A structural change (layer added/removed/moved) also needs
            // the per-layer side-car state (this same `histories` vec,
            // render caches, UI colors) mirrored to match — `History`
            // itself only has `&mut Canvas`, so it can't reach those here.
            use crate::canvas::history::LayerHistoryOp;
            match &layer_action {
                Some(LayerHistoryOp::Added { index, .. }) => {
                    if redo {
                        // Redo: the layer was just re-inserted into
                        // canvas.layers at `index`. Give it fresh side-car
                        // slots, then reattach this exact History object —
                        // it already carries whatever this layer's own
                        // undo/redo stacks held.
                        self.insert_layer_state(*index);
                        self.layer_state.histories[*index] = history;
                    } else {
                        // Undo: the layer was just removed from
                        // canvas.layers at `index`. Its own History
                        // (`history`, held locally) is intentionally
                        // dropped here — reaching this action at all means
                        // it's the very first entry ever pushed for this
                        // layer (nothing else could still be above it on
                        // the SAME per-layer stack), so nothing of value is
                        // lost except the ability to redo the add itself;
                        // adding the layer again is one click away.
                        self.remove_layer_state(*index);
                    }
                }
                Some(op @ LayerHistoryOp::Removed { .. }) => {
                    // The layer plus whatever went with it (mask, folder
                    // contents), at the positions actually touched.
                    let indices = op.removed_indices();
                    if redo {
                        self.remove_layer_states(&indices);
                    } else {
                        self.insert_layer_states(&indices);
                    }
                    // `history` belongs to the surviving active layer (per
                    // the "record onto whichever layer ends up active"
                    // rule), not the one just added/removed above — put it
                    // back wherever the canvas now says is active.
                    let new_active = self.canvas.active_layer_idx;
                    self.layer_state.histories[new_active] = history;
                }
                Some(LayerHistoryOp::Moved { from, to, .. }) => {
                    // `from`/`to` are the move just applied, which may be a
                    // layer other than the selected one. `history` came out of
                    // the selected layer's slot, so put it back before
                    // mirroring the move, then re-take it from where the
                    // selection now is.
                    self.layer_state.histories[active_idx] = history;
                    self.reorder_layer_state(*from, *to);
                }
                None => {
                    self.layer_state.histories[active_idx] = history;
                }
            }

            if layer_action.is_some() {
                self.mark_all_tiles_dirty();
            }
            for (tx, ty) in affected {
                // Off-canvas tiles (negative coordinates) aren't drawn.
                if tx < 0 || ty < 0 {
                    continue;
                }
                if let Some(tile) = self.tile_mut(tx as usize, ty as usize) {
                    tile.mark_full();
                }
            }

            // Reset transform tool state if active so it recalculates bounds
            // Only reset if the undo action didn't restore a transform state
            if let super::tools::Tool::Transform(ref mut info) = self.active_tool
                && info.bounds.is_none()
                && info.rotation == 0.0
                && info.offset.x == 0.0
                && info.offset.y == 0.0
            {
                *info = crate::selection::transform::TransformInfo::default();
            }

            self.layer_state.thumbnails_dirty = true;
        }
    }
}
