use super::{
    layout,
    layout::ToolTab,
    painter_state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
};
use crate::app::input_handler;
use crate::app::render_helper;
use crate::{canvas::Canvas, tablet::TabletInput, ui};
use eframe::egui;
use eframe::egui::{Color32, Vec2};
use egui_dock::DockState;

use crate::selection::SelectionManager;

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
pub struct PainterApp {
    pub(crate) canvas: Canvas,

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
            let active_idx = self.canvas.active_layer_idx;
            let affected = if shift_held {
                self.layer_state
                    .histories
                    .get_mut(active_idx)
                    .map(|h| {
                        h.redo(
                            &self.canvas,
                            &mut self.selection_manager,
                            &mut self.active_tool,
                        )
                    })
                    .unwrap_or_default()
            } else {
                self.layer_state
                    .histories
                    .get_mut(active_idx)
                    .map(|h| {
                        h.undo(
                            &self.canvas,
                            &mut self.selection_manager,
                            &mut self.active_tool,
                        )
                    })
                    .unwrap_or_default()
            };

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
            if self.workspace.first_frame {
                let available = ui.available_size();
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
                self.workspace.first_frame = false;
            }

            render_helper::update_dirty_textures(self);
            let view = render_helper::draw_canvas(self, ui);

            input_handler::handle_input(self, ctx, &view.response, view.origin, view.canvas_center);

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
                self.canvas.clear(Color32::WHITE);
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
