//! Undo history, one for the whole document: each step keeps the tiles as
//! they were (compressed once older, and naming their layer by stable id),
//! plus selection and layer tree changes; undo and redo swap them back.
//! One stack in the order things were done means undo always takes back
//! the last change, whichever layer it was on.

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
    pub data: SnapshotPixels,
}

/// A snapshot's pixels: raw while it's the newest undo step (instant undo),
/// zstd-compressed once older. Tiles compress extremely well (a transparent
/// 64x64 tile shrinks from 16 KiB to a few bytes), so the same memory budget
/// holds far more history.
#[derive(Clone)]
pub enum SnapshotPixels {
    Raw(Vec<Color32>),
    Compressed { bytes: Vec<u8>, len: usize },
}

impl From<Vec<Color32>> for SnapshotPixels {
    fn from(pixels: Vec<Color32>) -> Self {
        Self::Raw(pixels)
    }
}

impl SnapshotPixels {
    /// Number of pixels.
    pub fn len(&self) -> usize {
        match self {
            Self::Raw(pixels) => pixels.len(),
            Self::Compressed { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The pixels, decompressing if needed. A snapshot that fails to
    /// decompress (never expected) yields an empty vec, which callers
    /// reject as an invalid snapshot rather than restoring garbage.
    pub fn to_vec(&self) -> Vec<Color32> {
        match self {
            Self::Raw(pixels) => pixels.clone(),
            Self::Compressed { bytes, len } => zstd::bulk::decompress(bytes, len * 4)
                .map(|raw| {
                    raw.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Bytes of pixel data held in memory.
    fn held_bytes(&self) -> usize {
        match self {
            Self::Raw(pixels) => pixels.len() * std::mem::size_of::<Color32>(),
            Self::Compressed { bytes, .. } => bytes.len(),
        }
    }

    fn compress(&mut self) {
        let Self::Raw(pixels) = self else {
            return;
        };
        let raw: Vec<u8> = pixels.iter().flat_map(|p| p.to_array()).collect();
        if let Ok(bytes) = zstd::bulk::compress(&raw, 1) {
            *self = Self::Compressed {
                bytes,
                len: pixels.len(),
            };
        }
    }
}

/// Plain-data layer metadata (no tile content), enough to reconstruct an
/// empty layer shell for undo/redo of a layer add/remove. Pixel content is
/// restored separately through this action's `tiles` snapshots, which are
/// resolved by the same stable `LayerId`.
#[derive(Clone)]
pub struct LayerMeta {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub locked: bool,
    /// Painting keeps the existing transparency.
    pub alpha_locked: bool,
    pub kind: crate::canvas::storage::LayerKind,
    pub parent: Option<LayerId>,
    pub blend: crate::canvas::blend_modes::LayerBlend,
    pub clipped: bool,
    pub adjustment: Option<crate::canvas::filters::Filter>,
    /// A fill layer's content or a border around the paint.
    pub style: crate::canvas::layer_style::LayerStyle,
    pub text: Option<Box<crate::canvas::text::TextLayer>>,
    pub vector: Option<Box<crate::canvas::vector::VectorLayer>>,
    pub position_locked: bool,
    pub draft: bool,
    pub reference: bool,
}

/// A layer removed alongside the main one of a `Removed` op (its mask, or
/// a folder's contents), with its position before the removal.
#[derive(Clone)]
pub struct RemovedLayer {
    pub index: usize,
    pub id: LayerId,
    pub meta: LayerMeta,
}

/// A structural change to the layer list (as opposed to a pixel edit),
/// bundled into an `UndoAction` alongside whatever tile snapshots are needed
/// to restore its content.
#[derive(Clone)]
pub enum LayerHistoryOp {
    Added {
        index: usize,
        id: LayerId,
        /// What was added (folder, mask...); `None` means a plain layer.
        meta: Option<LayerMeta>,
        active_before: usize,
        active_after: usize,
    },
    Removed {
        index: usize,
        id: LayerId,
        meta: LayerMeta,
        /// Removed together with it: its mask, a folder's contents.
        also: Vec<RemovedLayer>,
        active_before: usize,
        active_after: usize,
    },
    Moved {
        id: LayerId,
        from: usize,
        to: usize,
        /// Folder before and after the move (moving into/out of folders).
        parent_before: Option<LayerId>,
        parent_after: Option<LayerId>,
        active_before: usize,
        active_after: usize,
    },
    /// The canvas was resized, cropped or turned: the whole document (its
    /// size and every layer) as it is on the other side of this step.
    /// Shared so the op stays `Clone`; undo and redo swap it in place.
    Document(std::sync::Arc<std::sync::Mutex<crate::canvas::storage::DocumentState>>),
    /// Text layers' source ([`crate::canvas::storage::Layer::text`]) as it
    /// is on the other side of this step: text edited or moved, or a text
    /// layer painted on (which makes it plain pixels). `inner` is the step's
    /// own change to the layer list, if it has one.
    Text {
        layers: Vec<(LayerId, Option<Box<crate::canvas::text::TextLayer>>)>,
        inner: Option<Box<LayerHistoryOp>>,
    },
    /// Vector layers' lines ([`crate::canvas::storage::Layer::vector`]) as
    /// they are on the other side of this step: a line drawn, erased or
    /// changed, or a vector layer painted on with a pixel tool (which makes
    /// it plain pixels). `inner` as for `Text`.
    Vector {
        layers: Vec<(LayerId, Option<Box<crate::canvas::vector::VectorLayer>>)>,
        inner: Option<Box<LayerHistoryOp>>,
    },
    /// Layers were merged: the entries on the other side of this step,
    /// swapped in place by undo and redo (see [`Canvas::swap_layers`]).
    Replaced(std::sync::Arc<std::sync::Mutex<crate::canvas::storage::LayerSwap>>),
}

impl LayerHistoryOp {
    /// The change to the layer list itself: this op, or the one a `Text`
    /// or `Vector` op carries (however they're nested).
    pub fn structural(op: Option<&LayerHistoryOp>) -> Option<&LayerHistoryOp> {
        let mut op = op;
        while let Some(LayerHistoryOp::Text { inner, .. } | LayerHistoryOp::Vector { inner, .. }) =
            op
        {
            op = inner.as_deref();
        }
        op
    }

    /// For `Removed`: every index involved, ascending (the main layer and
    /// `also`). Inserting at these in order restores the original layout.
    pub fn removed_indices(&self) -> Vec<usize> {
        match self {
            LayerHistoryOp::Removed { index, also, .. } => {
                let mut indices: Vec<usize> = std::iter::once(*index)
                    .chain(also.iter().map(|r| r.index))
                    .collect();
                indices.sort_unstable();
                indices
            }
            _ => Vec::new(),
        }
    }
}

/// Collection of tile snapshots captured during a single user operation.
#[derive(Clone)]
pub struct UndoAction {
    pub tiles: Vec<TileSnapshot>,
    pub selection: Option<Option<SelectionShape>>,
    pub transform: Option<TransformInfo>,
    /// Set when this action also adds, removes or moves a layer itself
    /// (not just its pixel content).
    pub layer_action: Option<LayerHistoryOp>,
}

/// Pixel memory one layer's undo stack may hold before its oldest actions are
/// dropped, counting older steps at their compressed size. Uncompressed, a
/// stroke across a 4K canvas saves about 4000 tiles, roughly 64 MiB.
const MAX_UNDO_BYTES: usize = 512 * 1024 * 1024;

fn snapshot_bytes(action: &UndoAction) -> usize {
    action.tiles.iter().map(|tile| tile.data.held_bytes()).sum()
}

/// Compress the pixels of every action except the newest. Usually only
/// the one before the newest has any left raw: the rest are passed over
/// without handing each to the thread pool (which, with hundreds of steps,
/// made every new step slower than the last).
fn compress_older_actions(stack: &mut [UndoAction]) {
    use rayon::iter::{IntoParallelIterator, ParallelIterator};
    let Some((_, older)) = stack.split_last_mut() else {
        return;
    };
    let raw: Vec<&mut SnapshotPixels> = older
        .iter_mut()
        .flat_map(|action| action.tiles.iter_mut())
        .map(|tile| &mut tile.data)
        .filter(|data| matches!(data, SnapshotPixels::Raw(_)))
        .collect();
    raw.into_par_iter().for_each(|data| data.compress());
}

/// Drop the oldest actions until the stack fits in `max_bytes`, always
/// keeping the newest one. Returns how many were dropped.
fn trim_oldest(stack: &mut Vec<UndoAction>, max_bytes: usize) -> usize {
    let mut total: usize = stack.iter().map(snapshot_bytes).sum();
    let mut dropped = 0;
    while total > max_bytes && dropped + 1 < stack.len() {
        total -= snapshot_bytes(&stack[dropped]);
        dropped += 1;
    }
    stack.drain(..dropped);
    dropped
}

/// What a step did, where the step itself says: layers added, removed,
/// moved or merged, the image resized, a transform, a selection.
fn describe(action: &UndoAction) -> Option<&'static str> {
    use crate::canvas::storage::LayerKind;
    let op = LayerHistoryOp::structural(action.layer_action.as_ref());
    Some(match op {
        Some(LayerHistoryOp::Added { meta, .. }) => match meta {
            Some(m) if m.text.is_some() => "Text",
            Some(m) if m.adjustment.is_some() => "New adjustment layer",
            Some(m) if m.kind == LayerKind::Group => "New folder",
            Some(m) if matches!(m.kind, LayerKind::Mask { .. }) => "Add mask",
            _ => "New layer",
        },
        Some(LayerHistoryOp::Removed { .. }) => "Delete layer",
        Some(LayerHistoryOp::Moved { .. }) => "Move layer",
        Some(LayerHistoryOp::Document(_)) => "Image size / rotation",
        Some(LayerHistoryOp::Replaced(_)) => "Merge layers",
        Some(LayerHistoryOp::Text { .. }) => "Text",
        None if matches!(action.layer_action, Some(LayerHistoryOp::Text { .. }))
            && action.tiles.is_empty() =>
        {
            "Text"
        }
        Some(LayerHistoryOp::Vector { .. }) => "Vector",
        None if action.transform.is_some() => "Transform",
        None if action.tiles.is_empty() && action.selection.is_some() => "Selection",
        None => return None,
    })
}

/// Stack-based undo/redo manager that swaps tile buffers in place.
#[derive(Clone)]
pub struct History {
    undo_stack: Vec<UndoAction>,
    redo_stack: Vec<UndoAction>,
    /// What each step did, for the History panel: one per step, in step
    /// with the stacks.
    undo_labels: Vec<String>,
    redo_labels: Vec<String>,
    /// The next step's name, when the command that makes it knows better
    /// (a filter, a paste).
    next_label: Option<String>,
    /// The tool in use: the name of a step that only changes pixels.
    tool_label: &'static str,
    /// Actions pushed so far, so callers can tell whether a given action was
    /// recorded (the stack itself drops its oldest entries).
    pushed: u64,
}

impl History {
    /// Create an empty history with no recorded actions.
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_labels: Vec::new(),
            redo_labels: Vec::new(),
            next_label: None,
            tool_label: "Edit",
            pushed: 0,
        }
    }

    /// Name the next step pushed (it does what the command says, whatever
    /// the tool in use).
    pub fn label_next(&mut self, label: impl Into<String>) {
        self.next_label = Some(label.into());
    }

    /// Rename the newest step (a command that knows what it did better
    /// than the tool in use).
    pub fn rename_last(&mut self, label: impl Into<String>) {
        if let Some(last) = self.undo_labels.last_mut() {
            *last = label.into();
        }
    }

    /// The tool in use, naming the pixel changes it makes.
    pub fn set_tool_label(&mut self, label: &'static str) {
        self.tool_label = label;
    }

    /// What each step did: those that can be undone (oldest first) and
    /// those that can be redone (next first... last).
    pub fn labels(&self) -> (&[String], &[String]) {
        (&self.undo_labels, &self.redo_labels)
    }

    fn label_of(&mut self, action: &UndoAction) -> String {
        self.next_label
            .take()
            .or_else(|| describe(action).map(str::to_string))
            .unwrap_or_else(|| self.tool_label.to_string())
    }

    /// Labels for stacks read from a file, which don't keep them.
    fn fill_labels(&mut self) {
        let name = |a: &UndoAction| describe(a).unwrap_or("Edit").to_string();
        self.undo_labels = self.undo_stack.iter().map(name).collect();
        self.redo_labels = self.redo_stack.iter().map(name).collect();
    }

    /// How many actions have been pushed (see [`History::push_action`]).
    pub fn push_count(&self) -> u64 {
        self.pushed
    }

    /// Forget everything that could be redone (e.g. a cancelled stroke that
    /// was just undone and should not come back).
    pub fn discard_redo(&mut self) {
        self.redo_stack.clear();
        self.redo_labels.clear();
    }

    /// Push a new action onto the undo stack and clear redo, dropping the
    /// oldest actions if the stack's tile snapshots exceed `MAX_UNDO_BYTES`.
    pub fn push_action(&mut self, action: UndoAction) {
        let label = self.label_of(&action);
        self.undo_stack.push(action);
        self.undo_labels.push(label);
        self.pushed += 1;
        self.redo_stack.clear();
        self.redo_labels.clear();
        compress_older_actions(&mut self.undo_stack);
        let dropped = trim_oldest(&mut self.undo_stack, MAX_UNDO_BYTES);
        self.undo_labels.drain(..dropped);
    }

    pub(crate) fn stacks(&self) -> (&[UndoAction], &[UndoAction]) {
        (&self.undo_stack, &self.redo_stack)
    }

    /// One history from several (older project files kept one per layer).
    /// Steps on different layers restore different tiles, so their relative
    /// order only matters for layer moves; the stacks are kept whole, in
    /// the order given.
    pub(crate) fn merged(histories: Vec<History>) -> Self {
        let mut merged = Self::new();
        for h in histories {
            merged.undo_stack.extend(h.undo_stack);
            merged.redo_stack.extend(h.redo_stack);
        }
        compress_older_actions(&mut merged.undo_stack);
        trim_oldest(&mut merged.undo_stack, MAX_UNDO_BYTES);
        merged.fill_labels();
        merged
    }

    pub(crate) fn from_stacks(undo_stack: Vec<UndoAction>, redo_stack: Vec<UndoAction>) -> Self {
        let mut history = Self {
            undo_stack,
            redo_stack,
            ..Self::new()
        };
        history.fill_labels();
        history
    }

    /// Undo the latest action, returning tile coordinates that changed and
    /// (if this action also touched the layer list itself) the structural
    /// change that was applied — the caller must mirror it onto its own
    /// per-layer side-car state (undo-history vec, render caches, UI
    /// colors), which `History` has no access to from here.
    pub fn undo(
        &mut self,
        canvas: &mut Canvas,
        selection_manager: &mut crate::selection::SelectionManager,
        active_tool: &mut crate::app::tools::Tool,
    ) -> (Vec<(i32, i32)>, Option<LayerHistoryOp>) {
        if let Some(mut action) = self.undo_stack.pop() {
            let op = LayerHistoryOp::structural(action.layer_action.as_ref());
            Self::prepare_for_undo(canvas, op);
            let tiles = self.swap_state(canvas, selection_manager, active_tool, &mut action);
            Self::swap_text(canvas, action.layer_action.as_mut());
            let op = LayerHistoryOp::structural(action.layer_action.as_ref());
            let layer_action = Self::finalize_after_undo(canvas, op);
            self.redo_stack.push(action);
            let label = self.undo_labels.pop().unwrap_or_default();
            self.redo_labels.push(label);
            (tiles, layer_action)
        } else {
            (Vec::new(), None)
        }
    }

    /// Redo the previously undone action. See `undo` for the return shape.
    pub fn redo(
        &mut self,
        canvas: &mut Canvas,
        selection_manager: &mut crate::selection::SelectionManager,
        active_tool: &mut crate::app::tools::Tool,
    ) -> (Vec<(i32, i32)>, Option<LayerHistoryOp>) {
        if let Some(mut action) = self.redo_stack.pop() {
            let op = LayerHistoryOp::structural(action.layer_action.as_ref());
            Self::prepare_for_redo(canvas, op);
            let tiles = self.swap_state(canvas, selection_manager, active_tool, &mut action);
            Self::swap_text(canvas, action.layer_action.as_mut());
            let op = LayerHistoryOp::structural(action.layer_action.as_ref());
            let layer_action = Self::finalize_after_redo(canvas, op);
            self.undo_stack.push(action);
            let label = self.redo_labels.pop().unwrap_or_default();
            self.undo_labels.push(label);
            (tiles, layer_action)
        } else {
            (Vec::new(), None)
        }
    }

    /// Exchange the text layers' source with the step's (both directions).
    /// Done while the same layers exist either way: after a removal is
    /// undone or an add redone, before an add is undone or a removal redone.
    /// Vector layers' lines likewise, wherever they're nested.
    fn swap_text(canvas: &mut Canvas, layer_action: Option<&mut LayerHistoryOp>) {
        let mut op = layer_action;
        loop {
            match op {
                Some(LayerHistoryOp::Text { layers, inner }) => {
                    for (id, text) in layers {
                        if let Some(idx) = canvas.layer_index_of(*id) {
                            std::mem::swap(&mut canvas.layers[idx].text, text);
                        }
                    }
                    op = inner.as_deref_mut();
                }
                Some(LayerHistoryOp::Vector { layers, inner }) => {
                    for (id, vector) in layers {
                        if let Some(idx) = canvas.layer_index_of(*id) {
                            std::mem::swap(&mut canvas.layers[idx].vector, vector);
                        }
                    }
                    op = inner.as_deref_mut();
                }
                _ => return,
            }
        }
    }

    /// Structural change applied BEFORE the tile-snapshot swap, so the swap
    /// has a layer to write pixel data into (undoing a removal needs the
    /// layer shell to exist again before its tiles can be restored).
    fn prepare_for_undo(canvas: &mut Canvas, layer_action: Option<&LayerHistoryOp>) {
        if let Some(LayerHistoryOp::Removed {
            index,
            id,
            meta,
            also,
            ..
        }) = layer_action
        {
            // Ascending original positions, so each lands where it was.
            let mut shells: Vec<(usize, LayerId, &LayerMeta)> =
                std::iter::once((*index, *id, meta))
                    .chain(also.iter().map(|r| (r.index, r.id, &r.meta)))
                    .collect();
            shells.sort_by_key(|(index, _, _)| *index);
            for (index, id, meta) in shells {
                canvas.insert_layer_with_meta(index, id, meta);
            }
        }
    }

    /// Structural change applied AFTER the tile-snapshot swap. Returns the
    /// action to expose to the caller, with `index` corrected to the
    /// position actually touched in `canvas.layers` — the recorded `index`
    /// can be stale by the time this specific action reaches the top of the
    /// stack (other layers may have been added/removed/reordered elsewhere
    /// in the meantime), and the caller uses this `index` to mirror the
    /// same structural change onto its own per-layer side-car vecs, so it
    /// must match the position actually mutated here, not the one recorded
    /// at the time this action was originally pushed.
    fn finalize_after_undo(
        canvas: &mut Canvas,
        layer_action: Option<&LayerHistoryOp>,
    ) -> Option<LayerHistoryOp> {
        match layer_action {
            Some(LayerHistoryOp::Added {
                id,
                index,
                meta,
                active_before,
                active_after,
            }) => {
                // The layer being un-added is one we ourselves added and
                // never removed since, so it's guaranteed to still exist —
                // resolve its actual current position rather than trusting
                // the possibly-stale recorded one.
                let removed_index = canvas.layer_index_of(*id);
                if let Some(current) = removed_index {
                    canvas.layers.remove(current);
                }
                canvas.active_layer_idx =
                    (*active_before).min(canvas.layers.len().saturating_sub(1));
                Some(LayerHistoryOp::Added {
                    id: *id,
                    index: removed_index.unwrap_or(*index),
                    meta: meta.clone(),
                    active_before: *active_before,
                    active_after: *active_after,
                })
            }
            Some(op @ LayerHistoryOp::Removed { active_before, .. }) => {
                canvas.active_layer_idx =
                    (*active_before).min(canvas.layers.len().saturating_sub(1));
                Some(op.clone())
            }
            Some(LayerHistoryOp::Moved {
                id,
                from,
                parent_before,
                parent_after,
                active_before,
                active_after,
                ..
            }) => {
                // Returned as the move actually applied (current -> target),
                // for the caller to mirror onto its per-layer state.
                let applied = Self::move_layer(canvas, *id, *from, *parent_before);
                canvas.active_layer_idx =
                    (*active_before).min(canvas.layers.len().saturating_sub(1));
                applied.map(|(current, target)| LayerHistoryOp::Moved {
                    id: *id,
                    from: current,
                    to: target,
                    parent_before: *parent_after,
                    parent_after: *parent_before,
                    active_before: *active_after,
                    active_after: *active_before,
                })
            }
            Some(op @ LayerHistoryOp::Document(doc)) => {
                canvas.swap_document(&mut doc.lock().unwrap_or_else(|e| e.into_inner()));
                Some(op.clone())
            }
            Some(op @ LayerHistoryOp::Replaced(swap)) => {
                canvas.swap_layers(&mut swap.lock().unwrap_or_else(|e| e.into_inner()));
                Some(op.clone())
            }
            // (Given the structural part: never a `Text` op.)
            Some(LayerHistoryOp::Text { .. } | LayerHistoryOp::Vector { .. }) | None => None,
        }
    }

    /// Move layer `id` to position `to` (clamped) inside folder `parent`.
    /// Returns `(from, to)` as applied.
    fn move_layer(
        canvas: &mut Canvas,
        id: LayerId,
        to: usize,
        parent: Option<LayerId>,
    ) -> Option<(usize, usize)> {
        let current = canvas.layer_index_of(id)?;
        let mut layer = canvas.layers.remove(current);
        layer.parent = parent;
        let target = to.min(canvas.layers.len());
        canvas.layers.insert(target, layer);
        Some((current, target))
    }

    fn prepare_for_redo(canvas: &mut Canvas, layer_action: Option<&LayerHistoryOp>) {
        if let Some(LayerHistoryOp::Added {
            index, id, meta, ..
        }) = layer_action
        {
            // Redoing an add: recreate the layer shell with the same
            // defaults `Canvas::add_layer` itself uses. Any content the
            // layer had is restored separately, in order, by whatever
            // pixel-edit redo entries sit above this one in the SAME
            // per-layer stack — reaching this entry at all requires the
            // whole stack to have been undone down to here first, so the
            // layer is guaranteed to have been empty at this point in its
            // history (opacity/visibility toggles aren't undo-tracked
            // either, matching the rest of this app's undo scope).
            let meta = meta.clone().unwrap_or_else(|| LayerMeta {
                name: format!("Layer {}", index + 1),
                visible: true,
                opacity: 1.0,
                locked: false,
                alpha_locked: false,
                kind: Default::default(),
                parent: None,
                blend: Default::default(),
                clipped: false,
                adjustment: None,
                style: Default::default(),
                text: None,
                vector: None,
                position_locked: false,
                draft: false,
                reference: false,
            });
            canvas.insert_layer_with_meta(*index, *id, &meta);
        }
    }

    /// See `finalize_after_undo` for why this returns a (possibly
    /// index-corrected) action rather than mutating in place.
    fn finalize_after_redo(
        canvas: &mut Canvas,
        layer_action: Option<&LayerHistoryOp>,
    ) -> Option<LayerHistoryOp> {
        match layer_action {
            Some(LayerHistoryOp::Removed {
                id,
                index,
                meta,
                also,
                active_before,
                active_after,
            }) => {
                // Re-applying a removal: the layer was re-inserted by this
                // same redo (via prepare_for_redo would be for Added, not
                // here — Removed's re-insertion happened on the matching
                // undo, so this layer has existed continuously since; still
                // resolve fresh rather than trust the original index).
                // Resolve every current position first, then remove from the
                // highest down so earlier removals don't shift later ones.
                let removed_index = canvas.layer_index_of(*id);
                let also: Vec<RemovedLayer> = also
                    .iter()
                    .map(|r| RemovedLayer {
                        index: canvas.layer_index_of(r.id).unwrap_or(r.index),
                        id: r.id,
                        meta: r.meta.clone(),
                    })
                    .collect();
                let mut ids: Vec<(usize, LayerId)> = removed_index
                    .map(|i| (i, *id))
                    .into_iter()
                    .chain(also.iter().map(|r| (r.index, r.id)))
                    .collect();
                ids.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
                for (_, layer_id) in ids {
                    if let Some(current) = canvas.layer_index_of(layer_id) {
                        canvas.layers.remove(current);
                    }
                }
                canvas.active_layer_idx =
                    (*active_after).min(canvas.layers.len().saturating_sub(1));
                Some(LayerHistoryOp::Removed {
                    id: *id,
                    index: removed_index.unwrap_or(*index),
                    meta: meta.clone(),
                    also,
                    active_before: *active_before,
                    active_after: *active_after,
                })
            }
            Some(op @ LayerHistoryOp::Added { active_after, .. }) => {
                canvas.active_layer_idx =
                    (*active_after).min(canvas.layers.len().saturating_sub(1));
                Some(op.clone())
            }
            Some(LayerHistoryOp::Moved {
                id,
                to,
                parent_before,
                parent_after,
                active_before,
                active_after,
                ..
            }) => {
                let applied = Self::move_layer(canvas, *id, *to, *parent_after);
                canvas.active_layer_idx =
                    (*active_after).min(canvas.layers.len().saturating_sub(1));
                applied.map(|(current, target)| LayerHistoryOp::Moved {
                    id: *id,
                    from: current,
                    to: target,
                    parent_before: *parent_before,
                    parent_after: *parent_after,
                    active_before: *active_before,
                    active_after: *active_after,
                })
            }
            Some(op @ LayerHistoryOp::Document(doc)) => {
                canvas.swap_document(&mut doc.lock().unwrap_or_else(|e| e.into_inner()));
                Some(op.clone())
            }
            Some(op @ LayerHistoryOp::Replaced(swap)) => {
                canvas.swap_layers(&mut swap.lock().unwrap_or_else(|e| e.into_inner()));
                Some(op.clone())
            }
            // (Given the structural part: never a `Text` op.)
            Some(LayerHistoryOp::Text { .. } | LayerHistoryOp::Vector { .. }) | None => None,
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
            // (Tiles may sit off the canvas, at negative coordinates: pixels
            // moved past its edge are kept.)
            // (Checked: a damaged file's numbers mustn't wrap round.)
            let fits =
                |at: usize, len: usize| at.checked_add(len).is_some_and(|end| end <= tile_size);
            if !fits(snapshot.x0, snapshot.width)
                || !fits(snapshot.y0, snapshot.height)
                || Some(snapshot.data.len()) != snapshot.width.checked_mul(snapshot.height)
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
            if let Some(tile_arc) = canvas.ensure_layer_tile(layer_idx, snapshot.tx, snapshot.ty) {
                let mut tile = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                // Ensure tile data exists
                if tile.data.is_none() {
                    tile.data = Some(vec![Color32::TRANSPARENT; tile_size * tile_size]);
                }
                let data = tile.data.as_mut().unwrap();
                let stored = snapshot.data.to_vec();
                if stored.len() != snapshot.width * snapshot.height {
                    log::error!("Skipping undo snapshot that failed to decompress");
                    continue;
                }

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
                        .copy_from_slice(&stored[src_start..src_start + len]);
                }

                // The tile may have gone from empty to painted or back: keep
                // its flag true to its pixels. (Left stale, a tile emptied by
                // e.g. liquify and then restored by undo stayed flagged empty,
                // so it drew — and every tool read it — as a transparent hole.)
                tile.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);

                // Store current region for redo/undo swap
                snapshot.data = current_region.into();
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
}
