use super::{
    layout,
    layout::ToolTab,
    painter_state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
};
use crate::app::input_handler;
use crate::app::stroke_ops::exclusive;
use crate::app::render_helper;
use crate::brush_engine::stroke_worker::StrokeWorker;
use crate::{canvas::Canvas, tablet::TabletInput, ui};
use std::sync::Arc;
use eframe::egui;
use eframe::egui::{Color32, Vec2};
use egui_dock::DockState;

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

        // Cache input state for this frame
        let (ctrl_z_pressed, shift_held) = ctx.input(|i| {
            (
                i.modifiers.ctrl && i.key_pressed(egui::Key::Z),
                i.modifiers.shift,
            )
        });

        // Handle Undo/Redo
        if ctrl_z_pressed {
            // Finish any stroke first so it is in the history (and undoable).
            self.release_canvas();
        }
        if ctrl_z_pressed && self.canvas.active_layer_idx < self.layer_state.histories.len() {
            let active_idx = self.canvas.active_layer_idx;
            // Detach the active layer's History for the duration of the
            // call: `History::undo`/`redo` need `&mut Canvas` to reverse a
            // layer add/remove/move, and this avoids holding a live borrow
            // of `self.layer_state.histories` at the same time.
            let mut history =
                std::mem::take(&mut self.layer_state.histories[active_idx]);

            let (affected, layer_action) = if shift_held {
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
                    if shift_held {
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
                Some(LayerHistoryOp::Removed { index, .. }) => {
                    if shift_held {
                        self.remove_layer_state(*index);
                    } else {
                        self.insert_layer_state(*index);
                    }
                    // `history` belongs to the surviving active layer (per
                    // the "record onto whichever layer ends up active"
                    // rule), not the one just added/removed above — put it
                    // back wherever the canvas now says is active.
                    let new_active = self.canvas.active_layer_idx;
                    self.layer_state.histories[new_active] = history;
                }
                Some(LayerHistoryOp::Moved { .. }) => {
                    let new_active = self.canvas.active_layer_idx;
                    if new_active != active_idx {
                        self.reorder_layer_state(active_idx, new_active);
                    }
                    self.layer_state.histories[new_active] = history;
                }
                None => {
                    self.layer_state.histories[active_idx] = history;
                }
            }

            if layer_action.is_some() {
                self.mark_all_tiles_dirty();
            }
            for (tx, ty) in affected {
                if let Some(tile) = self.tile_mut(tx.max(0) as usize, ty.max(0) as usize) {
                    tile.dirty = true;
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

        ui::top_bar::top_bar(self, ctx);

        layout::show_tool_docks(self, ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
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

            let view = render_helper::draw_canvas(self, ui);

            input_handler::handle_input(self, ctx, &view.response, view.origin, view.canvas_center);
            // Give the stroke worker a short budget to paint this frame's
            // samples so they usually show this frame; a heavy brush can't
            // stall the frame beyond it and simply shows up next frame.
            if self.brush_state.is_drawing {
                self.stroke_worker.wait_idle_for(STROKE_FRAME_BUDGET);
            }
            if self.sync_stroke_worker() {
                needs_repaint = true;
            }
            // Composite and paint after input, so this frame's dabs and any
            // pan/zoom show up in this frame.
            let (uploads, more_tiles) =
                render_helper::update_dirty_textures(self, &view, ui.clip_rect());
            if more_tiles {
                needs_repaint = true;
            }
            render_helper::paint_canvas(self, ui, &view, uploads);

            if self.brush_state.is_drawing {
                needs_repaint = true;
            }

            if !matches!(self.active_tool, super::tools::Tool::Transform(_)) {
                self.selection_manager.draw_overlay(
                    ui.painter(),
                    self.viewport.zoom,
                    view.origin,
                    self.canvas.height() as f32,
                    None,
                );
            }

            self.draw_transform_overlay(ui.painter(), view.origin);

            // Cache keyboard input for this frame
            let (c_pressed, escape_pressed) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::C),
                    i.key_pressed(egui::Key::Escape),
                )
            });

            if c_pressed {
                self.canvas_mut().clear(Color32::WHITE);
                for tile in &mut self.render_cache.tiles {
                    tile.dirty = true;
                }
                needs_repaint = true;
            }

            if escape_pressed {
                self.selection_manager.clear_selection();
                needs_repaint = true;
            }
        });

        ui::canvas_creation::canvas_creation_modal(self, ctx);
        ui::general_settings::general_settings_modal(self, ctx);
        ui::export_modal::export_modal(self, ctx);

        // Single consolidated repaint request
        if needs_repaint {
            ctx.request_repaint();
        }
    }
}
