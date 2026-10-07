use super::*;
use crate::app::tools::Tool;
use crate::selection::SelectionManager;

fn action_with_tile_pixels(pixels: usize, tag: i32) -> UndoAction {
    UndoAction {
        tiles: vec![TileSnapshot {
            tx: tag,
            ty: 0,
            layer_id: LayerId(1),
            x0: 0,
            y0: 0,
            width: pixels,
            height: 1,
            data: vec![Color32::TRANSPARENT; pixels].into(),
        }],
        selection: None,
        transform: None,
        layer_action: None,
    }
}

#[test]
fn trim_oldest_drops_oldest_actions_past_budget_but_keeps_newest() {
    // Each action holds 100 pixels = 400 bytes.
    let mut stack: Vec<UndoAction> = (0..5).map(|i| action_with_tile_pixels(100, i)).collect();
    trim_oldest(&mut stack, 1000);
    let kept: Vec<i32> = stack.iter().map(|a| a.tiles[0].tx).collect();
    assert_eq!(kept, vec![3, 4]);

    let mut huge = vec![action_with_tile_pixels(10_000, 9)];
    trim_oldest(&mut huge, 1000);
    assert_eq!(
        huge.len(),
        1,
        "the newest action is kept even if it alone exceeds the budget"
    );
}

#[test]
fn steps_keeping_whole_layers_count_towards_the_budget() {
    // Each resize keeps the picture as it was: a painted 256 px layer,
    // 256 KiB. Three of them don't fit in 600 KiB.
    let mut canvas = Canvas::new(256, 256, Color32::WHITE, 64);
    for ty in 0..4 {
        for tx in 0..4 {
            canvas.set_layer_tile_data(1, tx, ty, vec![Color32::RED; 64 * 64]);
        }
    }
    let mut stack: Vec<UndoAction> = (0..3)
        .map(|_| {
            let before = canvas.apply_image_op(crate::canvas::geometry::ImageOp::Reframe {
                x: 0,
                y: 0,
                w: 256,
                h: 256,
            });
            UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: Some(LayerHistoryOp::Document(std::sync::Arc::new(
                    std::sync::Mutex::new(before),
                ))),
            }
        })
        .collect();
    assert!(snapshot_bytes(&stack[0]) >= 256 * 256 * 4);
    assert_eq!(trim_oldest(&mut stack, 600 * 1024), 1);
    assert_eq!(stack.len(), 2);
}

#[test]
fn invalid_snapshot_is_ignored() {
    let mut canvas = Canvas::new(8, 8, Color32::WHITE, 4);
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
            data: vec![Color32::BLACK; 16].into(),
        }],
        selection: None,
        transform: None,
        layer_action: None,
    });

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (affected, layer_action) = history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(affected.is_empty());
    assert!(layer_action.is_none());
}

/// Regression test: a layer's recorded `Added { index, .. }` can go
/// stale if a DIFFERENT, earlier layer is removed afterward (shifting
/// this layer's actual position down without touching this layer's own
/// history stack at all). Undoing the Added action must still remove
/// the correct layer (resolved by LayerId), not whatever now happens to
/// sit at the originally-recorded index.
#[test]
fn undo_added_resolves_current_position_after_intervening_removal() {
    let tile_size = 4;
    let mut canvas = Canvas::new(tile_size, tile_size, Color32::WHITE, tile_size);
    // canvas: [Background(id0), Layer1(id1)]

    let layer2_id = canvas.add_layer();
    // canvas: [Background, Layer1, Layer2] — Layer2 added at index 2.
    let mut layer2_history = History::new();
    layer2_history.push_action(UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: Some(LayerHistoryOp::Added {
            index: 2,
            id: layer2_id,
            meta: None,
            active_before: 1,
            active_after: 2,
        }),
    });

    // Some other layer (Layer1, index 1) gets removed afterward,
    // shifting Layer2 down to index 1 — without touching Layer2's own
    // history stack at all.
    canvas.layers.remove(1);
    canvas.active_layer_idx = 1;
    assert_eq!(canvas.layer_index_of(layer2_id), Some(1));
    assert_eq!(canvas.layers.len(), 2);

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (_, layer_action) = layer2_history.undo(&mut canvas, &mut selection, &mut tool);

    // Layer2 must be gone — removed by resolving its current position
    // (1), not the stale recorded index (2), which would be out of
    // bounds for the now-2-layer canvas and silently no-op instead.
    assert_eq!(canvas.layer_index_of(layer2_id), None);
    assert_eq!(canvas.layers.len(), 1);

    match layer_action {
        Some(LayerHistoryOp::Added { index, id, .. }) => {
            assert_eq!(id, layer2_id);
            assert_eq!(
                index, 1,
                "exposed index must be the corrected/current position"
            );
        }
        _ => panic!("expected a corrected Added layer_action"),
    }
}

/// Regression test for the layer-idx-staleness bug: an undo snapshot
/// recorded against a layer must still target that same layer after the
/// layer's position changes (e.g. via a reorder), because it's resolved
/// by stable LayerId rather than by the position captured at record time.
#[test]
fn undo_targets_correct_layer_after_reorder() {
    let tile_size = 4;
    let mut canvas = Canvas::new(tile_size, tile_size, Color32::WHITE, tile_size);
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
            data: original.clone().into(),
        }],
        selection: None,
        transform: None,
        layer_action: None,
    });
    canvas.set_layer_tile_data(1, 0, 0, painted.clone());

    // Reorder: Layer 1 (LayerId(1)) moves from position 1 to position 0.
    canvas.layers.swap(0, 1);
    assert_eq!(canvas.layer_index_of(LayerId(1)), Some(0));
    assert_eq!(canvas.layer_index_of(LayerId(0)), Some(1));

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (affected, layer_action) = history.undo(&mut canvas, &mut selection, &mut tool);
    assert_eq!(affected, vec![(0, 0)]);
    assert!(layer_action.is_none());

    // The undo must restore LayerId(1)'s data at its NEW position (0),
    // not blindly write into position 1 (now the Background layer).
    assert_eq!(canvas.get_layer_tile_data(0, 0, 0), Some(original));
    assert_ne!(canvas.get_layer_tile_data(1, 0, 0), Some(painted));
}

fn one_tile_action(data: Vec<Color32>) -> UndoAction {
    UndoAction {
        tiles: vec![TileSnapshot {
            tx: 0,
            ty: 0,
            layer_id: LayerId(1),
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            data: data.into(),
        }],
        selection: None,
        transform: None,
        layer_action: None,
    }
}

#[test]
fn older_actions_are_compressed_losslessly_newest_stays_raw() {
    let patterned: Vec<Color32> = (0..4096u32)
        .map(|i| Color32::from_rgba_premultiplied(i as u8, (i / 16) as u8, 255 - i as u8, 200))
        .collect();
    let mut history = History::new();
    history.push_action(action_with_tile_pixels(4096, 1));
    history.push_action(UndoAction {
        tiles: vec![TileSnapshot {
            data: patterned.clone().into(),
            ..action_with_tile_pixels(4096, 2).tiles.remove(0)
        }],
        ..action_with_tile_pixels(0, 2)
    });
    history.push_action(action_with_tile_pixels(4096, 3));

    let (undo, _) = history.stacks();
    assert!(matches!(
        undo[0].tiles[0].data,
        SnapshotPixels::Compressed { .. }
    ));
    assert!(matches!(
        undo[1].tiles[0].data,
        SnapshotPixels::Compressed { .. }
    ));
    assert!(matches!(undo[2].tiles[0].data, SnapshotPixels::Raw(_)));
    assert!(
        snapshot_bytes(&undo[0]) < 100,
        "a transparent tile compresses to a few bytes"
    );
    assert_eq!(
        undo[0].tiles[0].data.to_vec(),
        vec![Color32::TRANSPARENT; 4096]
    );
    assert_eq!(undo[1].tiles[0].data.to_vec(), patterned);
}

#[test]
fn undo_restores_exact_pixels_from_a_compressed_snapshot() {
    let mut canvas = Canvas::new(4, 4, Color32::WHITE, 4);
    let before: Vec<Color32> = (0..16u8)
        .map(|i| Color32::from_rgba_premultiplied(i * 9, 255 - i * 7, i * 3, 200))
        .collect();
    let red = vec![Color32::RED; 16];
    canvas.set_layer_tile_data(1, 0, 0, before.clone());

    let mut history = History::new();
    history.push_action(one_tile_action(before.clone()));
    canvas.set_layer_tile_data(1, 0, 0, red.clone());
    history.push_action(one_tile_action(red.clone()));
    canvas.set_layer_tile_data(1, 0, 0, vec![Color32::BLUE; 16]);
    assert!(matches!(
        history.stacks().0[0].tiles[0].data,
        SnapshotPixels::Compressed { .. }
    ));

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert_eq!(canvas.get_layer_tile_data(1, 0, 0), Some(red));
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert_eq!(canvas.get_layer_tile_data(1, 0, 0), Some(before));
}

/// Undoing a layer-add removes the layer and restores the previously
/// active layer.
#[test]
fn undo_removes_added_layer() {
    let mut canvas = Canvas::new(4, 4, Color32::WHITE, 4);
    let active_before = canvas.active_layer_idx; // Canvas::new -> 1
    let new_id = canvas.add_layer();
    let new_idx = canvas.layers.len() - 1; // 2

    let mut history = History::new();
    history.push_action(UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: Some(LayerHistoryOp::Added {
            index: new_idx,
            id: new_id,
            meta: None,
            active_before,
            active_after: new_idx,
        }),
    });

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (affected, layer_action) = history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(affected.is_empty());
    assert!(matches!(layer_action, Some(LayerHistoryOp::Added { .. })));
    assert_eq!(canvas.layers.len(), 2);
    assert_eq!(canvas.active_layer_idx, active_before);
    assert!(canvas.layer_index_of(new_id).is_none());
}

/// Redoing a layer-add re-inserts a layer with the same id at the same
/// position.
#[test]
fn redo_readds_layer_with_same_id() {
    let mut canvas = Canvas::new(4, 4, Color32::WHITE, 4);
    let active_before = canvas.active_layer_idx;
    let new_id = canvas.add_layer();
    let new_idx = canvas.layers.len() - 1;

    let mut history = History::new();
    history.push_action(UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: Some(LayerHistoryOp::Added {
            index: new_idx,
            id: new_id,
            meta: None,
            active_before,
            active_after: new_idx,
        }),
    });

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    history.undo(&mut canvas, &mut selection, &mut tool);
    assert_eq!(canvas.layers.len(), 2);

    let (_, layer_action) = history.redo(&mut canvas, &mut selection, &mut tool);
    assert!(matches!(layer_action, Some(LayerHistoryOp::Added { .. })));
    assert_eq!(canvas.layers.len(), 3);
    assert_eq!(canvas.layer_index_of(new_id), Some(new_idx));
    assert_eq!(canvas.active_layer_idx, new_idx);
}

/// Undoing a layer removal restores the layer at its original position,
/// with its original id AND its original pixel content.
#[test]
fn undo_restores_removed_layer_with_content() {
    let tile_size = 4;
    let mut canvas = Canvas::new(tile_size, tile_size, Color32::WHITE, tile_size);
    let removed_id = canvas.add_layer();
    let removed_idx = canvas.layers.len() - 1; // 2
    let painted = vec![Color32::BLACK; tile_size * tile_size];
    canvas.set_layer_tile_data(removed_idx, 0, 0, painted.clone());

    let meta = canvas.layer_meta_at(removed_idx).unwrap();
    let tiles = canvas.snapshot_layer_tiles(removed_idx);
    assert_eq!(tiles.len(), 1);

    let active_before = canvas.active_layer_idx; // 2 (just added)
    canvas.layers.remove(removed_idx);
    canvas.active_layer_idx = active_before.min(canvas.layers.len() - 1); // clamps to 1

    let mut history = History::new();
    history.push_action(UndoAction {
        tiles,
        selection: None,
        transform: None,
        layer_action: Some(LayerHistoryOp::Removed {
            index: removed_idx,
            id: removed_id,
            meta,
            also: Vec::new(),
            active_before,
            active_after: canvas.active_layer_idx,
        }),
    });

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (affected, layer_action) = history.undo(&mut canvas, &mut selection, &mut tool);
    assert_eq!(affected, vec![(0, 0)]);
    assert!(matches!(layer_action, Some(LayerHistoryOp::Removed { .. })));
    assert_eq!(canvas.layers.len(), 3);
    let restored_idx = canvas.layer_index_of(removed_id).unwrap();
    assert_eq!(restored_idx, removed_idx);
    assert_eq!(
        canvas.get_layer_tile_data(restored_idx, 0, 0),
        Some(painted)
    );
    assert_eq!(canvas.active_layer_idx, active_before);
}

/// Undoing a layer reorder restores both layers' original positions.
#[test]
fn undo_moves_layer_back() {
    let mut canvas = Canvas::new(4, 4, Color32::WHITE, 4);
    let id2 = canvas.add_layer(); // now at idx 2
    let from = 2;
    let to = 1;
    let moved_id = canvas.layer_id_at(from).unwrap();
    assert_eq!(moved_id, id2);
    let other_id = canvas.layer_id_at(to).unwrap();

    let active_before = canvas.active_layer_idx; // 2, just added
    let layer = canvas.layers.remove(from);
    canvas.layers.insert(to, layer);
    let active_after = to;
    canvas.active_layer_idx = active_after;

    let mut history = History::new();
    history.push_action(UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: Some(LayerHistoryOp::Moved {
            id: moved_id,
            from,
            to,
            parent_before: None,
            parent_after: None,
            active_before,
            active_after,
        }),
    });

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (_, layer_action) = history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(matches!(layer_action, Some(LayerHistoryOp::Moved { .. })));
    assert_eq!(canvas.layer_index_of(moved_id), Some(from));
    assert_eq!(canvas.layer_index_of(other_id), Some(to));
    assert_eq!(canvas.active_layer_idx, active_before);
}

/// Undoing a merge-down restores both the bottom layer's pre-merge
/// content and the fully-merged-away top layer (same id, same pixels).
#[test]
fn undo_restores_both_layers_after_merge_down() {
    let tile_size = 4;
    let mut canvas = Canvas::new(tile_size, tile_size, Color32::WHITE, tile_size);
    let bottom_id = canvas.layer_id_at(1).unwrap();
    let bottom_before =
        vec![Color32::from_rgba_unmultiplied(10, 10, 10, 255); tile_size * tile_size];
    canvas.set_layer_tile_data(1, 0, 0, bottom_before.clone());

    let top_id = canvas.add_layer();
    let top_idx = canvas.layers.len() - 1;
    let top_data = vec![Color32::from_rgba_unmultiplied(200, 0, 0, 255); tile_size * tile_size];
    canvas.set_layer_tile_data(top_idx, 0, 0, top_data.clone());

    let mut action = UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    };
    canvas.merge_layer_down(top_idx, Some(&mut action));
    assert_eq!(canvas.layers.len(), 2);
    assert!(action.layer_action.is_some());
    // Bottom layer now holds the blended result, not its original data.
    assert_ne!(
        canvas.get_layer_tile_data(1, 0, 0),
        Some(bottom_before.clone())
    );

    let mut history = History::new();
    history.push_action(action);

    let mut selection = SelectionManager::new();
    let mut tool = Tool::Brush;
    let (affected, layer_action) = history.undo(&mut canvas, &mut selection, &mut tool);
    assert!(!affected.is_empty());
    assert!(matches!(layer_action, Some(LayerHistoryOp::Removed { .. })));
    assert_eq!(canvas.layers.len(), 3);

    let restored_top_idx = canvas.layer_index_of(top_id).unwrap();
    assert_eq!(
        canvas.get_layer_tile_data(restored_top_idx, 0, 0),
        Some(top_data)
    );
    let bottom_idx = canvas.layer_index_of(bottom_id).unwrap();
    assert_eq!(
        canvas.get_layer_tile_data(bottom_idx, 0, 0),
        Some(bottom_before)
    );
}
