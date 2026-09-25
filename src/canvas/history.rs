use crate::canvas::Canvas;
use crate::canvas::storage::LayerId;
use crate::selection::SelectionShape;
use crate::selection::transform::TransformInfo;
use eframe::egui::Color32;

/// Snapshot of a rectangular tile region prior to modification.
#[derive(Clone)]
pub struct TileSnapshot {
    pub tx: i32,
    pub ty: i32,
    /// The layer this snapshot belongs to, by stable id rather than
    /// position: the layer may have been reordered since the snapshot was
    /// taken, so a raw index would silently target the wrong layer.
    pub layer_id: LayerId,
    pub x0: usize,
    pub y0: usize,
    pub width: usize,
    pub height: usize,
    pub data: Vec<Color32>,
}

/// Collection of tile snapshots captured during a single user operation.
#[derive(Clone)]
pub struct UndoAction {
    pub tiles: Vec<TileSnapshot>,
    pub selection: Option<Option<SelectionShape>>,
    pub transform: Option<TransformInfo>,
}

/// Stack-based undo/redo manager that swaps tile buffers in place.
#[derive(Clone)]
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

    pub(crate) fn stacks(&self) -> (&[UndoAction], &[UndoAction]) {
        (&self.undo_stack, &self.redo_stack)
    }

    pub(crate) fn from_stacks(undo_stack: Vec<UndoAction>, redo_stack: Vec<UndoAction>) -> Self {
        Self {
            undo_stack,
            redo_stack,
        }
    }

    /// Undo the latest action, returning tile coordinates that changed.
    pub fn undo(
        &mut self,
        canvas: &Canvas,
        selection_manager: &mut crate::selection::SelectionManager,
        active_tool: &mut crate::app::tools::Tool,
    ) -> Vec<(i32, i32)> {
        if let Some(mut action) = self.undo_stack.pop() {
            let tiles = self.swap_state(canvas, selection_manager, active_tool, &mut action);
            self.redo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    /// Redo the previously undone action, returning tile coordinates that changed.
    pub fn redo(
        &mut self,
        canvas: &Canvas,
        selection_manager: &mut crate::selection::SelectionManager,
        active_tool: &mut crate::app::tools::Tool,
    ) -> Vec<(i32, i32)> {
        if let Some(mut action) = self.redo_stack.pop() {
            let tiles = self.swap_state(canvas, selection_manager, active_tool, &mut action);
            self.undo_stack.push(action);
            tiles
        } else {
            Vec::new()
        }
    }

    /// Swap stored tile data with the canvas, producing a list of updated tiles.
    fn swap_state(
        &self,
        canvas: &Canvas,
        selection_manager: &mut crate::selection::SelectionManager,
        active_tool: &mut crate::app::tools::Tool,
        action: &mut UndoAction,
    ) -> Vec<(i32, i32)> {
        // Swap selection state
        if let Some(stored_selection) = &mut action.selection {
            std::mem::swap(stored_selection, &mut selection_manager.current_shape);
        }

        // Swap transform state
        if let Some(stored_transform) = &mut action.transform
            && let crate::app::tools::Tool::Transform(current_transform) = active_tool
        {
            std::mem::swap(stored_transform, current_transform);
        }

        let mut affected = Vec::new();
        for snapshot in &mut action.tiles {
            let tile_size = canvas.tile_size();
            if snapshot.tx < 0
                || snapshot.ty < 0
                || snapshot.x0 + snapshot.width > tile_size
                || snapshot.y0 + snapshot.height > tile_size
                || snapshot.data.len() != snapshot.width * snapshot.height
            {
                log::error!(
                    "Skipping invalid undo snapshot at tile ({}, {})",
                    snapshot.tx,
                    snapshot.ty
                );
                continue;
            }
            // Resolve by stable id: the layer may have been reordered since
            // this snapshot was recorded, so its position may have changed.
            let Some(layer_idx) = canvas.layer_index_of(snapshot.layer_id) else {
                log::error!(
                    "Skipping undo snapshot for a layer that no longer exists ({:?})",
                    snapshot.layer_id
                );
                continue;
            };
            canvas.ensure_layer_tile_exists(layer_idx, snapshot.tx as usize, snapshot.ty as usize);
            if let Some(tile_arc) =
                canvas.lock_layer_tile(layer_idx, snapshot.tx as usize, snapshot.ty as usize)
            {
                let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
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

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tools::Tool;
    use crate::selection::SelectionManager;

    #[test]
    fn invalid_snapshot_is_ignored() {
        let canvas = Canvas::new(8, 8, Color32::WHITE, 4);
        let mut history = History::new();
        history.push_action(UndoAction {
            tiles: vec![TileSnapshot {
                tx: 0,
                ty: 0,
                layer_id: LayerId(1),
                x0: 3,
                y0: 3,
                width: 4,
                height: 4,
                data: vec![Color32::BLACK; 16],
            }],
            selection: None,
            transform: None,
        });

        let mut selection = SelectionManager::new();
        let mut tool = Tool::Brush;
        assert!(history.undo(&canvas, &mut selection, &mut tool).is_empty());
    }

    /// Regression test for the layer-idx-staleness bug: an undo snapshot
    /// recorded against a layer must still target that same layer after the
    /// layer's position changes (e.g. via a reorder), because it's resolved
    /// by stable LayerId rather than by the position captured at record time.
    #[test]
    fn undo_targets_correct_layer_after_reorder() {
        let tile_size = 4;
        let canvas = Canvas::new(tile_size, tile_size, Color32::WHITE, tile_size);
        // Canvas::new gives LayerId(0) = "Background" at position 0,
        // LayerId(1) = "Layer 1" at position 1.
        let original = vec![Color32::TRANSPARENT; tile_size * tile_size];
        let painted = vec![Color32::BLACK; tile_size * tile_size];

        // Simulate a stroke on Layer 1 (LayerId(1)): record the pre-paint
        // state, then apply the paint.
        let mut history = History::new();
        history.push_action(UndoAction {
            tiles: vec![TileSnapshot {
                tx: 0,
                ty: 0,
                layer_id: LayerId(1),
                x0: 0,
                y0: 0,
                width: tile_size,
                height: tile_size,
                data: original.clone(),
            }],
            selection: None,
            transform: None,
        });
        canvas.set_layer_tile_data(1, 0, 0, painted.clone());

        // Reorder: Layer 1 (LayerId(1)) moves from position 1 to position 0.
        let mut canvas = canvas;
        canvas.layers.swap(0, 1);
        assert_eq!(canvas.layer_index_of(LayerId(1)), Some(0));
        assert_eq!(canvas.layer_index_of(LayerId(0)), Some(1));

        let mut selection = SelectionManager::new();
        let mut tool = Tool::Brush;
        let affected = history.undo(&canvas, &mut selection, &mut tool);
        assert_eq!(affected, vec![(0, 0)]);

        // The undo must restore LayerId(1)'s data at its NEW position (0),
        // not blindly write into position 1 (now the Background layer).
        assert_eq!(canvas.get_layer_tile_data(0, 0, 0), Some(original));
        assert_ne!(canvas.get_layer_tile_data(1, 0, 0), Some(painted));
    }
}
