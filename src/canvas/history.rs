use crate::canvas::canvas::Canvas;
use crate::selection::SelectionShape;
use crate::selection::transform::TransformInfo;
use eframe::egui::Color32;

/// Snapshot of a rectangular tile region prior to modification.
pub struct TileSnapshot {
    pub tx: i32,
    pub ty: i32,
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
    pub selection: Option<Option<SelectionShape>>,
    pub transform: Option<TransformInfo>,
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
    pub fn undo(&mut self, canvas: &Canvas, selection_manager: &mut crate::selection::SelectionManager, active_tool: &mut crate::app::tools::Tool) -> Vec<(i32, i32)> {
        if let Some(mut action) = self.undo_stack.pop() {
            let tiles = self.swap_state(canvas, selection_manager, active_tool, &mut action);
            self.redo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    /// Redo the previously undone action, returning tile coordinates that changed.
    pub fn redo(&mut self, canvas: &Canvas, selection_manager: &mut crate::selection::SelectionManager, active_tool: &mut crate::app::tools::Tool) -> Vec<(i32, i32)> {
        if let Some(mut action) = self.redo_stack.pop() {
            let tiles = self.swap_state(canvas, selection_manager, active_tool, &mut action);
            self.undo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    /// Swap stored tile data with the canvas, producing a list of updated tiles.
    fn swap_state(&self, canvas: &Canvas, selection_manager: &mut crate::selection::SelectionManager, active_tool: &mut crate::app::tools::Tool, action: &mut UndoAction) -> Vec<(i32, i32)> {
        // Swap selection state
        if let Some(stored_selection) = &mut action.selection {
            std::mem::swap(stored_selection, &mut selection_manager.current_shape);
        }

        // Swap transform state
        if let Some(stored_transform) = &mut action.transform {
            if let crate::app::tools::Tool::Transform(current_transform) = active_tool {
                std::mem::swap(stored_transform, current_transform);
            } else {
            }
        }

        let mut affected = Vec::new();
        for snapshot in &mut action.tiles {
            let tile_size = canvas.tile_size();
            canvas.ensure_layer_tile_exists_i32(snapshot.layer_idx, snapshot.tx, snapshot.ty);
            if let Some(tile_arc) =
                canvas.lock_layer_tile_i32(snapshot.layer_idx, snapshot.tx, snapshot.ty)
            {
                let mut tile = tile_arc.lock().unwrap();
                // Ensure tile data exists
                if tile.data.is_none() {
                    tile.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
                }
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
