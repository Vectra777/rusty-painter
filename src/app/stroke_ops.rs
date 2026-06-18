use super::PainterApp;
use crate::{
    brush_engine::stroke::{StrokeContext, StrokeState},
    canvas::history::{History, UndoAction},
};
use eframe::egui::Vec2;

impl PainterApp {
    pub(crate) fn start_stroke(&mut self, pos: Vec2) {
        if self.is_active_layer_locked() {
            return;
        }
        self.initialize_stroke_state();
        self.add_initial_stroke_point(pos);
    }

    fn is_active_layer_locked(&self) -> bool {
        self.canvas
            .layers
            .get(self.canvas.active_layer_idx)
            .map(|l| l.locked)
            .unwrap_or(false)
    }

    fn initialize_stroke_state(&mut self) {
        self.brush_state.stroke = Some(StrokeState::new());
        self.brush_state.is_drawing = true;
        self.layer_state.current_undo_action = Some(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
        });
        self.render_cache.modified_tiles.clear();
    }

    fn add_initial_stroke_point(&mut self, pos: Vec2) {
        if let Some(stroke) = &mut self.brush_state.stroke {
            let has_selection = self.selection_manager.has_selection();
            let selection = if has_selection {
                Some(&self.selection_manager)
            } else {
                None
            };
            let mut context = StrokeContext::new(
                &self.workspace.pool,
                &self.canvas,
                selection,
                self.layer_state.current_undo_action.as_mut().unwrap(),
                &mut self.render_cache.modified_tiles,
            );
            stroke.add_point(&mut self.brush_state.brush, pos, &mut context);
            self.mark_modified_tiles_dirty();
        }
    }

    pub(crate) fn finish_stroke(&mut self) {
        self.end_current_stroke();
        self.save_undo_action_if_valid();
        self.clear_stroke_state();
    }

    fn end_current_stroke(&mut self) {
        if let Some(stroke) = &mut self.brush_state.stroke {
            stroke.end();
        }
    }

    fn save_undo_action_if_valid(&mut self) {
        if let Some(action) = self.layer_state.current_undo_action.take()
            && !action.tiles.is_empty()
            && let Some(hist) = self.active_history_mut()
        {
            hist.push_action(action);
        }
    }

    fn clear_stroke_state(&mut self) {
        self.brush_state.stroke = None;
        self.brush_state.is_drawing = false;
    }

    fn active_history_mut(&mut self) -> Option<&mut History> {
        self.layer_state
            .histories
            .get_mut(self.canvas.active_layer_idx)
    }
}
