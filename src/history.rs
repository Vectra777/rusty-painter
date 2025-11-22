use eframe::egui::Color32;
use crate::canvas::Canvas;

pub struct TileSnapshot {
    pub tx: usize,
    pub ty: usize,
    pub data: Vec<Color32>,
}

pub struct UndoAction {
    pub tiles: Vec<TileSnapshot>,
}

pub struct History {
    undo_stack: Vec<UndoAction>,
    redo_stack: Vec<UndoAction>,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn push_action(&mut self, action: UndoAction) {
        self.undo_stack.push(action);
        self.redo_stack.clear();
    }

    pub fn undo(&mut self, canvas: &Canvas) -> Vec<(usize, usize)> {
        if let Some(mut action) = self.undo_stack.pop() {
            let tiles = self.swap_tiles(canvas, &mut action);
            self.redo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    pub fn redo(&mut self, canvas: &Canvas) -> Vec<(usize, usize)> {
        if let Some(mut action) = self.redo_stack.pop() {
            let tiles = self.swap_tiles(canvas, &mut action);
            self.undo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    fn swap_tiles(&self, canvas: &Canvas, action: &mut UndoAction) -> Vec<(usize, usize)> {
        let mut affected = Vec::new();
        for snapshot in &mut action.tiles {
            if let Some(current_data) = canvas.get_tile_data(snapshot.tx, snapshot.ty) {
                canvas.set_tile_data(snapshot.tx, snapshot.ty, snapshot.data.clone());
                snapshot.data = current_data;
                affected.push((snapshot.tx, snapshot.ty));
            } else {
                canvas.set_tile_data(snapshot.tx, snapshot.ty, snapshot.data.clone());
                affected.push((snapshot.tx, snapshot.ty));
            }
        }
        affected
    }
}
