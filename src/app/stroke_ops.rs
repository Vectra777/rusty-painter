use super::PainterApp;
use crate::{
    app::painter_state::StrokeSession,
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
        self.brush_state.session = Some(StrokeSession {
            stroke: StrokeState::new(),
            undo_action: UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            },
        });
        self.brush_state.is_drawing = true;
        self.render_cache.stroke_tiles.snapshotted.clear();
        self.render_cache.below_cache = None;
    }

    fn add_initial_stroke_point(&mut self, pos: Vec2) {
        if let Some(session) = &mut self.brush_state.session {
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
                &mut session.undo_action,
                &mut self.render_cache.stroke_tiles,
            );
            // No pressure sample is available for the synthetic first point
            // of a stroke; 1.0 preserves the pre-existing (unscaled) behavior.
            session
                .stroke
                .add_point(&mut self.brush_state.brush, pos, 1.0, &mut context);
            self.mark_modified_tiles_dirty();
        }
    }

    pub(crate) fn finish_stroke(&mut self) {
        self.end_current_stroke();
        self.save_undo_action_if_valid();
        self.clear_stroke_state();
    }

    fn end_current_stroke(&mut self) {
        if let Some(session) = &mut self.brush_state.session {
            session.stroke.end();
        }
    }

    fn save_undo_action_if_valid(&mut self) {
        if let Some(session) = self.brush_state.session.take()
            && !session.undo_action.tiles.is_empty()
            && let Some(hist) = self.active_history_mut()
        {
            hist.push_action(session.undo_action);
        }
    }

    fn clear_stroke_state(&mut self) {
        self.brush_state.session = None;
        self.brush_state.is_drawing = false;
        self.render_cache.below_cache = None;
    }

    fn active_history_mut(&mut self) -> Option<&mut History> {
        self.layer_state
            .histories
            .get_mut(self.canvas.active_layer_idx)
    }
}
