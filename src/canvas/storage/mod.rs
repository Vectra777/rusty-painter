//! The canvas document: layers made of lazily allocated tiles, plus the
//! layer tree operations. Compositing, pixel writing and transforms live
//! in the submodules.

mod composite;
mod merge;
mod pixels;
#[cfg(test)]
mod tests;
mod transform;
pub mod warp;

pub use composite::{BelowComposite, SampleLayers};
pub(crate) use composite::{gamma_over, shrink_tile};
pub use merge::LayerSwap;
pub use pixels::Region;
pub(crate) use pixels::mix;
pub(crate) use transform::rect_corners;
pub use transform::{Distort, DistortKind, InverseMap, TransformParams, is_convex_quad};

use rustc_hash::FxHashMap;
use std::sync::{Arc, Mutex};

use eframe::egui::{Color32, Rgba};

use crate::canvas::blend::{alpha_over_batch, apply_opacity_scale, gamma_rgba_to_color32};
use crate::canvas::blend_modes::{BlendSpace, LayerBlend};
use crate::canvas::history::{LayerMeta, TileSnapshot, UndoAction};

/// Stable identity for a layer, independent of its current position in
/// `Canvas::layers`. Undo history and other data that outlives a single
/// frame must key off this instead of a raw index, since reordering,
/// inserting or removing layers changes every index after the edit point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u64);

// Tile lookups happen per dab per tile and per layer per composited tile; FxHash
// is several times cheaper than the default SipHash for small integer keys.
type TileMap = FxHashMap<(i32, i32), Arc<Mutex<TileCell>>>;
type RowTileCache = Vec<Option<(i32, Arc<Mutex<TileCell>>, Option<Vec<Rgba>>, bool)>>;

/// What a layer entry is. Folders and masks are entries in the same flat
/// list as paint layers, so painting, undo and saving handle them all alike;
/// the tree comes from `Layer::parent` and mask `owner` links, not from
/// positions (among siblings, list order is stacking order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerKind {
    #[default]
    Paint,
    /// A folder: composites its children (layers whose `parent` is this
    /// folder) on their own, then applies its opacity/visibility.
    Group,
    /// Mask of `owner`: white shows the owner, black or transparent hides
    /// it. Missing tiles count as white, so a new mask shows everything.
    /// The entry's `visible` flag enables/disables the mask.
    Mask { owner: LayerId },
}

#[derive(Debug)]
/// Single painting layer with its own opacity, visibility and tile storage.
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32, // 0..1
    pub locked: bool,
    /// Painting keeps the existing transparency.
    pub alpha_locked: bool,
    pub kind: LayerKind,
    /// Folder containing this layer (`None` = top level).
    pub parent: Option<LayerId>,
    /// Folder shown open in the layers panel.
    pub expanded: bool,
    /// How this layer (or folder) combines with what's below it.
    pub blend: LayerBlend,
    /// Clipping mask: shows only where the nearest unclipped layer below it
    /// (in the same folder) has paint.
    pub clipped: bool,
    /// An adjustment layer: this filter applies to everything below it
    /// (its own pixels aren't shown; its mask says where it applies).
    pub adjustment: Option<crate::canvas::filters::Filter>,
    /// A fill layer's content or a border around the paint.
    pub style: crate::canvas::layer_style::LayerStyle,
    /// A text layer: its pixels are this text, rendered, and the Text tool
    /// can edit it again.
    pub text: Option<Box<crate::canvas::text::TextLayer>>,
    /// A vector layer: its lines, which its pixels are drawn from.
    pub vector: Option<Box<crate::canvas::vector::VectorLayer>>,
    /// Can't be moved or transformed.
    pub position_locked: bool,
    /// A draft: shown, but left out of export, merging and "all layers"
    /// sampling.
    pub draft: bool,
    /// A reference layer: fills (and the wand) set to "Reference" find
    /// their areas in it.
    pub reference: bool,
    tiles: Mutex<TileMap>,
}

#[derive(Clone)]
pub struct CanvasTileSnapshot {
    pub tx: i32,
    pub ty: i32,
    pub data: Vec<Color32>,
}

#[derive(Clone)]
pub struct CanvasLayerSnapshot {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub locked: bool,
    /// Painting keeps the existing transparency.
    pub alpha_locked: bool,
    pub kind: LayerKind,
    pub parent: Option<LayerId>,
    pub expanded: bool,
    pub blend: LayerBlend,
    pub clipped: bool,
    pub adjustment: Option<crate::canvas::filters::Filter>,
    /// A fill layer's content or a border around the paint.
    pub style: crate::canvas::layer_style::LayerStyle,
    pub text: Option<Box<crate::canvas::text::TextLayer>>,
    /// A vector layer: its lines, which its pixels are drawn from.
    pub vector: Option<Box<crate::canvas::vector::VectorLayer>>,
    pub position_locked: bool,
    pub draft: bool,
    pub reference: bool,
    pub tiles: Vec<CanvasTileSnapshot>,
}

/// A whole document's size and layers: what undo swaps back in after a
/// canvas resize, crop or rotation (see [`Canvas::swap_document`]).
pub struct DocumentState {
    pub width: usize,
    pub height: usize,
    pub layers: Vec<Layer>,
    pub active_layer_idx: usize,
}

impl Layer {
    /// Bytes of pixels this layer holds (for the undo history's budget).
    pub(crate) fn held_bytes(&self) -> usize {
        let tiles = self.tiles.lock().unwrap_or_else(|e| e.into_inner());
        tiles
            .values()
            .map(|t| {
                t.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .data
                    .as_ref()
                    .map_or(0, |d| d.len() * std::mem::size_of::<Color32>())
            })
            .sum()
    }

    /// The same layer (id and settings) with no pixels.
    pub(crate) fn shell(&self) -> Layer {
        Layer {
            id: self.id,
            name: self.name.clone(),
            visible: self.visible,
            opacity: self.opacity,
            locked: self.locked,
            alpha_locked: self.alpha_locked,
            kind: self.kind,
            parent: self.parent,
            expanded: self.expanded,
            blend: self.blend,
            clipped: self.clipped,
            adjustment: self.adjustment,
            style: self.style,
            text: self.text.clone(),
            vector: self.vector.clone(),
            position_locked: self.position_locked,
            draft: self.draft,
            reference: self.reference,
            tiles: Mutex::new(TileMap::default()),
        }
    }

    /// Set tile `(tx, ty)`'s pixels.
    pub(crate) fn set_tile(&self, tx: i32, ty: i32, data: Vec<Color32>) {
        let is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
        self.tiles.lock().unwrap_or_else(|e| e.into_inner()).insert(
            (tx, ty),
            Arc::new(Mutex::new(TileCell {
                data: Some(data),
                is_empty,
            })),
        );
    }

    /// Allocate a new layer backing store but keep tile data lazy.
    fn new(id: LayerId, name: String, _width: usize, _height: usize, _tile_size: usize) -> Self {
        Self {
            id,
            name,
            visible: true,
            opacity: 1.0,
            locked: false,
            alpha_locked: false,
            kind: LayerKind::Paint,
            parent: None,
            expanded: true,
            blend: LayerBlend::Normal,
            clipped: false,
            adjustment: None,
            style: Default::default(),
            text: None,
            vector: None,
            position_locked: false,
            draft: false,
            reference: false,
            tiles: Mutex::new(TileMap::default()),
        }
    }

    /// The layer's settings and painted tiles, sorted row by row.
    pub(crate) fn snapshot(&self) -> CanvasLayerSnapshot {
        let mut tiles: Vec<_> = self
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|(&(tx, ty), cell)| {
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty {
                    return None;
                }
                guard
                    .data
                    .clone()
                    .map(|data| CanvasTileSnapshot { tx, ty, data })
            })
            .collect();
        tiles.sort_by_key(|tile| (tile.ty, tile.tx));
        CanvasLayerSnapshot {
            id: self.id,
            name: self.name.clone(),
            visible: self.visible,
            opacity: self.opacity,
            locked: self.locked,
            alpha_locked: self.alpha_locked,
            kind: self.kind,
            parent: self.parent,
            expanded: self.expanded,
            blend: self.blend,
            clipped: self.clipped,
            adjustment: self.adjustment,
            style: self.style,
            text: self.text.clone(),
            vector: self.vector.clone(),
            position_locked: self.position_locked,
            draft: self.draft,
            reference: self.reference,
            tiles,
        }
    }

    pub(crate) fn from_snapshot(snapshot: CanvasLayerSnapshot) -> Self {
        let mut tiles = TileMap::default();
        for tile in snapshot.tiles {
            tiles.insert(
                (tile.tx, tile.ty),
                Arc::new(Mutex::new(TileCell {
                    is_empty: tile.data.iter().all(|&p| p == Color32::TRANSPARENT),
                    data: Some(tile.data),
                })),
            );
        }
        Self {
            id: snapshot.id,
            name: snapshot.name,
            visible: snapshot.visible,
            opacity: snapshot.opacity.clamp(0.0, 1.0),
            locked: snapshot.locked,
            alpha_locked: snapshot.alpha_locked,
            kind: snapshot.kind,
            parent: snapshot.parent,
            expanded: snapshot.expanded,
            blend: snapshot.blend,
            clipped: snapshot.clipped,
            adjustment: snapshot.adjustment,
            style: snapshot.style,
            text: snapshot.text,
            vector: snapshot.vector,
            position_locked: snapshot.position_locked,
            draft: snapshot.draft,
            reference: snapshot.reference,
            tiles: Mutex::new(tiles),
        }
    }
}

/// Main drawing surface that owns tile grids and blending rules across layers.
pub struct Canvas {
    width: usize,
    height: usize,
    tile_size: usize,
    clear_color: Color32,

    pub layers: Vec<Layer>,
    pub active_layer_idx: usize,
    next_layer_id: u64,
    /// Colour space layers and strokes blend in (per document).
    pub blend_space: BlendSpace,
}

/// A tile's `(tx, ty)` position.
type TileKey = (i32, i32);
/// A tile's shared, lockable storage.
type SharedCell = Arc<Mutex<TileCell>>;

#[derive(Debug)]
/// Tile container that is lazily filled with pixel data.
pub(crate) struct TileCell {
    pub data: Option<Vec<Color32>>,
    /// True if the tile contains only transparent pixels
    pub is_empty: bool,
}

fn layer_tile(layer: &Layer, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
    layer
        .tiles
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(tx, ty))
        .cloned()
}

impl Canvas {
    /// Create a new canvas with a single background layer and configured tile size.
    pub fn new(width: usize, height: usize, clear_color: Color32, tile_size: usize) -> Self {
        let mut bg_layer = Layer::new(
            LayerId(0),
            "Background".to_string(),
            width,
            height,
            tile_size,
        );
        bg_layer.locked = true;

        let layer1 = Layer::new(LayerId(1), "Layer 1".to_string(), width, height, tile_size);

        // Initialize background layer with clear color
        // We can't easily pre-fill all tiles without allocating massive memory.
        // The original code lazily allocated.
        // But if it's the background, it should probably be white (or clear_color).
        // The original code handled `None` as `clear_color` in `ensure_tile`.
        // We should preserve that behavior.

        Self {
            width,
            height,
            tile_size,
            clear_color,
            layers: vec![bg_layer, layer1],
            active_layer_idx: 1,
            next_layer_id: 2,
            blend_space: BlendSpace::Linear,
        }
    }

    /// Exchange the document's size and layers with `doc`'s.
    pub fn swap_document(&mut self, doc: &mut DocumentState) {
        std::mem::swap(&mut self.width, &mut doc.width);
        std::mem::swap(&mut self.height, &mut doc.height);
        std::mem::swap(&mut self.layers, &mut doc.layers);
        std::mem::swap(&mut self.active_layer_idx, &mut doc.active_layer_idx);
    }

    /// Look up a layer's current position by its stable id. O(layer count);
    /// layer counts are small, and this is never called from a pixel-stamp
    /// hot path.
    pub fn layer_index_of(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    /// The stable id of the layer currently at `idx`, if any.
    pub fn layer_id_at(&self, idx: usize) -> Option<LayerId> {
        self.layers.get(idx).map(|layer| layer.id)
    }

    /// Whether compositing needs the general tree compositor: any folder,
    /// mask or non-Normal blend mode, or gamma-space blending. Otherwise it's
    /// a plain Normal stack in linear light and the fast paths apply.
    pub fn needs_tree_compositing(&self) -> bool {
        self.blend_space != BlendSpace::Linear || !self.is_plain_stack()
    }

    /// How far past their paint layers show (the widest visible border).
    pub fn style_reach(&self) -> i32 {
        self.layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.style.reach())
            .max()
            .unwrap_or(0)
    }

    /// No folders, masks, clipping or blend modes: a plain stack of Normal
    /// layers (in either blend space).
    fn is_plain_stack(&self) -> bool {
        self.layers.iter().all(|l| {
            l.kind == LayerKind::Paint
                && l.parent.is_none()
                && l.blend == LayerBlend::Normal
                && !l.clipped
                && l.adjustment.is_none()
                && l.style.is_plain()
        })
    }

    /// Final 8-bit values of composites from the tree compositor (its
    /// tables looked up once, for loops over pixels).
    fn tree_encoder(&self) -> TreeEncoder {
        TreeEncoder {
            space: self.blend_space,
            linear: crate::canvas::blend::LinearEncoder::new(),
        }
    }

    /// Position of the mask entry belonging to layer `owner`, if any.
    pub fn mask_index_of(&self, owner: LayerId) -> Option<usize> {
        self.layers
            .iter()
            .position(|l| l.kind == LayerKind::Mask { owner })
    }

    /// Whether `id` is `ancestor` or nested (at any depth) inside it.
    pub fn is_within(&self, id: LayerId, ancestor: LayerId) -> bool {
        let mut current = Some(id);
        // Bounded by the layer count, in case of a (never expected) cycle.
        for _ in 0..=self.layers.len() {
            match current {
                Some(c) if c == ancestor => return true,
                Some(c) => {
                    current = self.layer_index_of(c).and_then(|i| self.layers[i].parent);
                }
                None => return false,
            }
        }
        false
    }

    /// Insert a new empty entry at `index` and return its id.
    pub fn insert_new_layer(
        &mut self,
        index: usize,
        name: String,
        kind: LayerKind,
        parent: Option<LayerId>,
    ) -> LayerId {
        let id = self.allocate_layer_id();
        let mut layer = Layer::new(id, name, self.width, self.height, self.tile_size);
        layer.kind = kind;
        layer.parent = parent;
        self.layers.insert(index.min(self.layers.len()), layer);
        id
    }

    fn allocate_layer_id(&mut self) -> LayerId {
        let id = LayerId(self.next_layer_id);
        self.next_layer_id = self.next_layer_id.saturating_add(1);
        id
    }

    pub fn add_layer(&mut self) -> LayerId {
        let name = format!("Layer {}", self.layers.len() + 1);
        let id = self.allocate_layer_id();
        let layer = Layer::new(id, name, self.width, self.height, self.tile_size);
        self.layers.push(layer);
        self.active_layer_idx = self.layers.len() - 1;
        id
    }

    /// Insert an empty layer with a specific (already-allocated) id and
    /// metadata at `index`, clamped to the current layer count. Used to
    /// reconstruct a layer shell for undo/redo of a layer add/remove; the
    /// caller is responsible for restoring pixel content separately (via
    /// `TileSnapshot`s resolved by the same id).
    pub fn insert_layer_with_meta(&mut self, index: usize, id: LayerId, meta: &LayerMeta) {
        let idx = index.min(self.layers.len());
        let mut layer = Layer::new(
            id,
            meta.name.clone(),
            self.width,
            self.height,
            self.tile_size,
        );
        layer.visible = meta.visible;
        layer.opacity = meta.opacity;
        layer.locked = meta.locked;
        layer.alpha_locked = meta.alpha_locked;
        layer.kind = meta.kind;
        layer.parent = meta.parent;
        layer.blend = meta.blend;
        layer.clipped = meta.clipped;
        layer.adjustment = meta.adjustment;
        layer.style = meta.style;
        layer.text = meta.text.clone();
        layer.vector = meta.vector.clone();
        layer.position_locked = meta.position_locked;
        layer.draft = meta.draft;
        layer.reference = meta.reference;
        self.layers.insert(idx, layer);
        // `id` is a reused (previously-allocated) id, not a new one, but
        // guard against ever handing out a colliding id afterward.
        self.next_layer_id = self.next_layer_id.max(id.0.saturating_add(1));
    }

    /// Snapshot every non-empty tile of a layer as full-tile `TileSnapshot`s,
    /// keyed by the layer's current stable id. Used to preserve a layer's
    /// pixel content across undo/redo of an operation that removes it
    /// (layer removal, merge-down).
    pub fn snapshot_layer_tiles(&self, layer_idx: usize) -> Vec<TileSnapshot> {
        let (Some(layer), Some(id)) = (self.layers.get(layer_idx), self.layer_id_at(layer_idx))
        else {
            return Vec::new();
        };
        let tile_size = self.tile_size;
        let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
        tiles
            .iter()
            .filter_map(|(&(tx, ty), cell)| {
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty {
                    return None;
                }
                guard.data.clone().map(|data| TileSnapshot {
                    tx,
                    ty,
                    layer_id: id,
                    x0: 0,
                    y0: 0,
                    width: tile_size,
                    height: tile_size,
                    data: data.into(),
                })
            })
            .collect()
    }

    /// Metadata (name/visible/opacity/locked) for a layer, without its tile
    /// content. Used to build an undo record before removing a layer.
    pub fn layer_meta_at(&self, layer_idx: usize) -> Option<LayerMeta> {
        self.layers.get(layer_idx).map(|layer| LayerMeta {
            name: layer.name.clone(),
            visible: layer.visible,
            opacity: layer.opacity,
            locked: layer.locked,
            alpha_locked: layer.alpha_locked,
            kind: layer.kind,
            parent: layer.parent,
            blend: layer.blend,
            clipped: layer.clipped,
            adjustment: layer.adjustment,
            style: layer.style,
            text: layer.text.clone(),
            vector: layer.vector.clone(),
            position_locked: layer.position_locked,
            draft: layer.draft,
            reference: layer.reference,
        })
    }

    /// Current canvas width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Current canvas height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn clear_color(&self) -> Color32 {
        self.clear_color
    }

    pub fn layer_snapshots(&self) -> Vec<CanvasLayerSnapshot> {
        self.layers.iter().map(Layer::snapshot).collect()
    }

    pub fn replace_layers_from_snapshots(
        &mut self,
        layers: Vec<CanvasLayerSnapshot>,
        active_layer_idx: usize,
    ) {
        self.layers = layers.into_iter().map(Layer::from_snapshot).collect();
        if self.layers.is_empty() {
            let id = self.allocate_layer_id();
            self.layers.push(Layer::new(
                id,
                "Background".to_string(),
                self.width,
                self.height,
                self.tile_size,
            ));
        }
        self.active_layer_idx = active_layer_idx.min(self.layers.len().saturating_sub(1));
        // Loaded layers may carry ids from the saved file (or position-based
        // fallback ids for files predating LayerId); make sure new layers
        // added after this never collide with them.
        self.next_layer_id = self
            .layers
            .iter()
            .map(|layer| layer.id.0)
            .max()
            .map_or(0, |max_id| max_id.saturating_add(1));
    }

    /// Size of a tile edge in pixels.
    pub fn tile_size(&self) -> usize {
        self.tile_size
    }

    /// Access a specific layer's tile by index (used for compositing).
    fn layer_tile_cell(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];
        let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
        tiles.get(&(tx, ty)).cloned()
    }

    /// Ensure the tile exists on a specific layer, initializing it if needed.
    pub(crate) fn ensure_layer_tile(
        &self,
        layer_idx: usize,
        tx: i32,
        ty: i32,
    ) -> Option<Arc<Mutex<TileCell>>> {
        if layer_idx >= self.layers.len() {
            return None;
        }
        let layer = &self.layers[layer_idx];

        let tile_arc = {
            let mut tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            tiles
                .entry((tx, ty))
                .or_insert_with(|| {
                    Arc::new(Mutex::new(TileCell {
                        data: None,
                        is_empty: true,
                    }))
                })
                .clone()
        };

        {
            let mut guard = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
            if guard.data.is_none() {
                let fill_color = if layer_idx == 0 {
                    self.clear_color
                } else if matches!(layer.kind, LayerKind::Mask { .. }) {
                    // A mask starts out showing everything.
                    Color32::WHITE
                } else {
                    Color32::TRANSPARENT
                };

                let data = vec![fill_color; self.tile_size * self.tile_size];
                guard.is_empty = fill_color == Color32::TRANSPARENT;
                guard.data = Some(data);
            }
        }
        Some(tile_arc)
    }

    /// Ensure the active layer has storage for the given tile.
    fn ensure_tile(&self, tx: i32, ty: i32) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(self.active_layer_idx, tx, ty)
    }

    /// Guarantee a tile exists on the active layer.
    pub fn ensure_tile_exists(&self, tx: usize, ty: usize) {
        let _ = self.ensure_tile(tx as i32, ty as i32);
    }

    /// Guarantee a tile exists on the specified layer.
    /// Guarantee a tile exists on the specified layer.
    pub fn ensure_layer_tile_exists(&self, layer_idx: usize, tx: usize, ty: usize) {
        let _ = self.ensure_layer_tile(layer_idx, tx as i32, ty as i32);
    }

    /// Lock a tile in the active layer, initializing it if absent.
    pub(crate) fn lock_tile(&self, tx: usize, ty: usize) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_tile(tx as i32, ty as i32)
    }

    /// Lock a tile in a specific layer, initializing it if absent.
    pub(crate) fn lock_layer_tile(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.ensure_layer_tile(layer_idx, tx as i32, ty as i32)
    }

    /// Lock a tile in a specific layer only if it already exists; avoids allocating new data.
    pub(crate) fn lock_layer_tile_if_exists(
        &self,
        layer_idx: usize,
        tx: usize,
        ty: usize,
    ) -> Option<Arc<Mutex<TileCell>>> {
        self.layer_tile_cell(layer_idx, tx as i32, ty as i32)
    }

    /// The tiles layer `layer_idx` holds (painted or not), in no order.
    pub fn layer_tile_keys(&self, layer_idx: usize) -> Vec<(i32, i32)> {
        self.layers.get(layer_idx).map_or_else(Vec::new, |layer| {
            layer
                .tiles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .copied()
                .collect()
        })
    }

    /// Drop tile `(tx, ty)` of layer `layer_idx` (it reads as unpainted).
    pub fn clear_layer_tile(&self, layer_idx: usize, tx: i32, ty: i32) {
        if let Some(layer) = self.layers.get(layer_idx) {
            layer
                .tiles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(tx, ty));
        }
    }

    /// Clone the raw pixel buffer for a tile in a given layer.
    pub fn get_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32) -> Option<Vec<Color32>> {
        let cell = self.layer_tile_cell(layer_idx, tx, ty)?;
        let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
        guard.data.clone()
    }

    /// Overwrite a tile's pixel buffer for a given layer.
    pub fn set_layer_tile_data(&self, layer_idx: usize, tx: i32, ty: i32, data: Vec<Color32>) {
        // Ensure tile exists
        if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
            let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
            let is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
            guard.is_empty = is_empty;
            guard.data = Some(data);
        }
    }

    /// Merge `layer_idx` down into the layer below it. If `history` is
    /// given, records enough to undo the merge: the bottom layer's
    /// pre-merge tile content (to reverse the blend), the top layer's full
    /// content (to restore it), and a `Removed` structural op describing
    /// the top layer itself (recreated as an empty shell on undo, before
    /// the tile snapshots refill both layers).
    pub fn merge_layer_down(&mut self, layer_idx: usize, mut history: Option<&mut UndoAction>) {
        if layer_idx == 0 || layer_idx >= self.layers.len() {
            return;
        }

        let active_before = self.active_layer_idx;
        let Some(top_id) = self.layer_id_at(layer_idx) else {
            return;
        };
        let Some(bottom_id) = self.layer_id_at(layer_idx - 1) else {
            return;
        };
        let top_meta = self.layer_meta_at(layer_idx);

        // Remove the top layer (source)
        let top_layer = self.layers.remove(layer_idx);
        let tile_size = self.tile_size;

        {
            // Get the bottom layer (destination)
            // Note: indices shifted after remove, so the layer that was at layer_idx - 1 is still at layer_idx - 1
            let bottom_layer = &mut self.layers[layer_idx - 1];

            let top_tiles = top_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut bottom_tiles = bottom_layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut src_with_opacity = Vec::new();
            let mut blended = Vec::new();

            for ((tx, ty), top_tile_arc) in top_tiles.iter() {
                let top_guard = top_tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(top_data) = &top_guard.data {
                    // Skip empty top tiles
                    if top_guard.is_empty {
                        continue;
                    }

                    // Ensure bottom tile exists
                    let bottom_tile_arc = bottom_tiles.entry((*tx, *ty)).or_insert_with(|| {
                        Arc::new(Mutex::new(TileCell {
                            data: None,
                            is_empty: true,
                        }))
                    });

                    let mut bottom_guard =
                        bottom_tile_arc.lock().unwrap_or_else(|e| e.into_inner());

                    // Capture pre-merge state for undo, before either tile
                    // is touched: the bottom layer's current content (or
                    // transparent, matching what the init below would
                    // otherwise produce) and the top layer's full content.
                    if let Some(action) = history.as_deref_mut() {
                        let bottom_before = bottom_guard
                            .data
                            .clone()
                            .unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_size * tile_size]);
                        action.tiles.push(TileSnapshot {
                            tx: *tx,
                            ty: *ty,
                            layer_id: bottom_id,
                            x0: 0,
                            y0: 0,
                            width: tile_size,
                            height: tile_size,
                            data: bottom_before.into(),
                        });
                        action.tiles.push(TileSnapshot {
                            tx: *tx,
                            ty: *ty,
                            layer_id: top_id,
                            x0: 0,
                            y0: 0,
                            width: tile_size,
                            height: tile_size,
                            data: top_data.clone().into(),
                        });
                    }

                    // Initialize bottom data if missing
                    if bottom_guard.data.is_none() {
                        bottom_guard.data =
                            Some(vec![Color32::TRANSPARENT; self.tile_size * self.tile_size]);
                    }

                    if let Some(bottom_data) = &mut bottom_guard.data {
                        // Use SIMD batch processing for better performance
                        let tile_len = bottom_data.len();

                        // Apply opacity to source pixels and prepare for batch blend
                        src_with_opacity.resize(tile_len, Color32::TRANSPARENT);
                        for i in 0..tile_len {
                            src_with_opacity[i] =
                                apply_opacity_scale(top_data[i], top_layer.opacity);
                        }

                        // Create temporary output buffer
                        blended.resize(tile_len, Color32::TRANSPARENT);

                        // Batch blend using SIMD
                        alpha_over_batch(&src_with_opacity, bottom_data, &mut blended);

                        // Copy result back
                        bottom_data.copy_from_slice(&blended);

                        // Update is_empty flag
                        bottom_guard.is_empty =
                            bottom_data.iter().all(|&p| p == Color32::TRANSPARENT);
                    }
                }
            }
        }

        // Adjust active layer index if needed
        if self.active_layer_idx >= self.layers.len() {
            self.active_layer_idx = self.layers.len() - 1;
        }

        if let (Some(action), Some(meta)) = (history, top_meta) {
            action.layer_action = Some(crate::canvas::history::LayerHistoryOp::Removed {
                index: layer_idx,
                id: top_id,
                meta,
                also: Vec::new(),
                active_before,
                active_after: self.active_layer_idx,
            });
        }
    }
}

/// Composited pixels to what's stored, in the canvas's blend space.
#[derive(Clone, Copy)]
struct TreeEncoder {
    space: BlendSpace,
    linear: crate::canvas::blend::LinearEncoder,
}

impl TreeEncoder {
    #[inline]
    fn encode(self, c: Rgba) -> Color32 {
        match self.space {
            BlendSpace::Linear => self.linear.encode(c),
            BlendSpace::Gamma => gamma_rgba_to_color32(c),
        }
    }
}
