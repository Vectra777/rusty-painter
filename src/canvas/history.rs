use crate::canvas::canvas::Canvas;
use eframe::egui::Color32;

/// Snapshot of a rectangular tile region prior to modification.
pub struct TileSnapshot {
    pub tx: usize,
    pub ty: usize,
    pub layer_idx: usize,
    pub x0: usize,
    pub y0: usize,
    pub width: usize,
    pub height: usize,
    pub data: Vec<Color32>,
}

/// Collection of tile snapshots captured during a single user operation.
pub struct UndoAction {
    pub tiles: Vec<TileSnapshot>,
}

/// Stack-based undo/redo manager that swaps tile buffers in place.
pub struct History {
    undo_stack: Vec<UndoAction>,
    redo_stack: Vec<UndoAction>,
}

impl History {
    /// Create an empty history with no recorded actions.
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// Push a new action onto the undo stack and clear redo.
    pub fn push_action(&mut self, action: UndoAction) {
        self.undo_stack.push(action);
        self.redo_stack.clear();
    }

    /// Undo the latest action, returning tile coordinates that changed.
    pub fn undo(&mut self, canvas: &Canvas) -> Vec<(usize, usize)> {
        if let Some(mut action) = self.undo_stack.pop() {
            let tiles = self.swap_tiles(canvas, &mut action);
            self.redo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    /// Redo the previously undone action, returning tile coordinates that changed.
    pub fn redo(&mut self, canvas: &Canvas) -> Vec<(usize, usize)> {
        if let Some(mut action) = self.redo_stack.pop() {
            let tiles = self.swap_tiles(canvas, &mut action);
            self.undo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    /// Swap stored tile data with the canvas, producing a list of updated tiles.
    fn swap_tiles(&self, canvas: &Canvas, action: &mut UndoAction) -> Vec<(usize, usize)> {
        let mut affected = Vec::new();
        for snapshot in &mut action.tiles {
            let tile_size = canvas.tile_size();
            canvas.ensure_layer_tile_exists(snapshot.layer_idx, snapshot.tx, snapshot.ty);
            if let Some(mut tile) =
                canvas.lock_layer_tile(snapshot.layer_idx, snapshot.tx, snapshot.ty)
            {
                let data = tile.data.as_mut().unwrap();

                // Extract current region
                let mut current_region =
                    vec![Color32::TRANSPARENT; snapshot.width * snapshot.height];
                for row in 0..snapshot.height {
                    let src_start = (snapshot.y0 + row) * tile_size + snapshot.x0;
                    let dst_start = row * snapshot.width;
                    let len = snapshot.width;
                    current_region[dst_start..dst_start + len]
                        .copy_from_slice(&data[src_start..src_start + len]);
                }

                // Write stored snapshot into tile
                for row in 0..snapshot.height {
                    let dst_start = (snapshot.y0 + row) * tile_size + snapshot.x0;
                    let src_start = row * snapshot.width;
                    let len = snapshot.width;
                    data[dst_start..dst_start + len]
                        .copy_from_slice(&snapshot.data[src_start..src_start + len]);
                }

                // Store current region for redo/undo swap
                snapshot.data = current_region;
                affected.push((snapshot.tx, snapshot.ty));
            }
        }
        affected
    }
}
