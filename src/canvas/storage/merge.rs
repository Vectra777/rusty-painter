//! Merging layers (Merge Down, Merge Visible, Flatten Image) and the views
//! of the canvas that leave draft layers out (export, copy merged).
//!
//! A merge composites through the same code the screen uses, on a view of
//! the canvas that shares its tiles with only the merged layers showing.
//! The layers it replaces are moved into a [`LayerSwap`], so undo puts
//! them back exactly as they were.

use rayon::prelude::*;
use std::collections::HashSet;
use std::sync::Mutex;

use eframe::egui::{Color32, ColorImage};

use super::{Canvas, Layer, LayerId, LayerKind, TileCell};
use crate::canvas::blend_modes::LayerBlend;

/// Entries taken out of the layer list and others put in their place, as
/// one step (a merge). Applying it again reverses it, so undo and redo
/// both call [`Canvas::swap_layers`].
pub struct LayerSwap {
    /// Entries to put in, at these final positions (ascending).
    pub layers: Vec<(usize, Layer)>,
    /// Ids of the entries to take out first.
    pub remove: Vec<LayerId>,
    /// The selected entry after the swap.
    pub active_layer_idx: usize,
    /// Positions touched by the last swap: those taken out (before it) and
    /// those put in (after it), ascending, for mirroring per-layer state.
    pub applied: (Vec<usize>, Vec<usize>),
}

impl Layer {
    /// This layer's settings over the same tiles (shared, not copied), for
    /// a view that only reads them.
    fn share(&self) -> Layer {
        let layer = self.shell();
        *layer.tiles.lock().unwrap_or_else(|e| e.into_inner()) =
            self.tiles.lock().unwrap_or_else(|e| e.into_inner()).clone();
        layer
    }

    /// Settings of a merge's result: a plain paint layer named and
    /// identified like `self`, with its flags.
    fn merged_from(&self) -> Layer {
        let mut layer = self.shell();
        layer.kind = LayerKind::Paint;
        layer.adjustment = None;
        layer
    }
}

impl Canvas {
    /// Take out and put in the entries `swap` says, leaving in `swap` what
    /// reverses it.
    pub fn swap_layers(&mut self, swap: &mut LayerSwap) {
        let mut out: Vec<usize> = swap
            .remove
            .iter()
            .filter_map(|&id| self.layer_index_of(id))
            .collect();
        out.sort_unstable();
        out.dedup();
        let mut taken: Vec<(usize, Layer)> = out
            .iter()
            .rev()
            .map(|&i| (i, self.layers.remove(i)))
            .collect();
        taken.reverse();
        let incoming = std::mem::take(&mut swap.layers);
        swap.remove = incoming.iter().map(|(_, l)| l.id).collect();
        let mut put = Vec::with_capacity(incoming.len());
        for (i, layer) in incoming {
            let i = i.min(self.layers.len());
            self.next_layer_id = self.next_layer_id.max(layer.id.0 + 1);
            self.layers.insert(i, layer);
            put.push(i);
        }
        swap.layers = taken;
        std::mem::swap(&mut self.active_layer_idx, &mut swap.active_layer_idx);
        self.active_layer_idx = self
            .active_layer_idx
            .min(self.layers.len().saturating_sub(1));
        swap.applied = (out, put);
    }

    /// A read-only copy of this canvas sharing its tiles, each entry
    /// changed by `adjust` (hidden, moved out of its folder...).
    fn view(&self, mut adjust: impl FnMut(usize, &mut Layer)) -> Canvas {
        Canvas {
            width: self.width,
            height: self.height,
            tile_size: self.tile_size,
            clear_color: self.clear_color,
            layers: self
                .layers
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let mut layer = l.share();
                    adjust(i, &mut layer);
                    layer
                })
                .collect(),
            active_layer_idx: self.active_layer_idx,
            next_layer_id: self.next_layer_id,
            blend_space: self.blend_space,
        }
    }

    /// Whether entry `i` is a draft or inside a draft folder.
    pub fn is_draft(&self, i: usize) -> bool {
        let Some(layer) = self.layers.get(i) else {
            return false;
        };
        let id = match layer.kind {
            LayerKind::Mask { owner } => owner,
            _ => layer.id,
        };
        self.layers
            .iter()
            .any(|l| l.draft && self.is_within(id, l.id))
    }

    /// Whether any layer is a draft.
    pub fn has_drafts(&self) -> bool {
        self.layers
            .iter()
            .any(|l| l.draft && !matches!(l.kind, LayerKind::Mask { .. }))
    }

    /// The canvas without its draft layers (as exported), or `None` when
    /// it has none (the canvas itself is then the answer).
    pub fn without_drafts(&self) -> Option<Canvas> {
        self.has_drafts().then(|| {
            self.view(|_, l| {
                if l.draft && !matches!(l.kind, LayerKind::Mask { .. }) {
                    l.visible = false;
                }
            })
        })
    }

    /// The flattened picture as exported: [`Self::flatten`] without the
    /// draft layers.
    pub fn flatten_final(&self) -> ColorImage {
        match self.without_drafts() {
            Some(view) => view.flatten(),
            None => self.flatten(),
        }
    }

    /// Composites of whole tiles `keys` (on the canvas), in parallel;
    /// transparent ones are left out unless `keep_empty` (the background
    /// shows its colour where a tile is missing).
    fn composite_tiles(
        &self,
        keys: Vec<(i32, i32)>,
        keep_empty: bool,
    ) -> Vec<((i32, i32), Vec<Color32>)> {
        let ts = self.tile_size;
        keys.into_par_iter()
            .filter_map(|(tx, ty)| {
                let mut part = ColorImage::new([0, 0], Color32::TRANSPARENT);
                self.write_tile_to_color_image(tx as usize, ty as usize, &mut part, 1, None);
                let [pw, ph] = part.size;
                let mut data = vec![Color32::TRANSPARENT; ts * ts];
                for row in 0..ph {
                    data[row * ts..row * ts + pw]
                        .copy_from_slice(&part.pixels[row * pw..(row + 1) * pw]);
                }
                (keep_empty || data.iter().any(|&p| p != Color32::TRANSPARENT))
                    .then_some(((tx, ty), data))
            })
            .collect()
    }

    /// Tiles on the canvas that entries `layers` hold (every tile if the
    /// background is among them: its unpainted tiles show its colour).
    fn tiles_of(&self, layers: &[usize]) -> Vec<(i32, i32)> {
        let (tw, th) = (
            self.width.div_ceil(self.tile_size) as i32,
            self.height.div_ceil(self.tile_size) as i32,
        );
        if layers.contains(&0) {
            return (0..th)
                .flat_map(|ty| (0..tw).map(move |tx| (tx, ty)))
                .collect();
        }
        let mut keys: Vec<(i32, i32)> = layers
            .iter()
            .flat_map(|&i| self.layer_tile_keys(i))
            .filter(|&(tx, ty)| (0..tw).contains(&tx) && (0..th).contains(&ty))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        keys.sort_unstable();
        keys
    }

    /// A layer shaped like `template` holding `tiles`.
    fn layer_with_tiles(template: Layer, tiles: Vec<((i32, i32), Vec<Color32>)>) -> Layer {
        let map = tiles
            .into_iter()
            .map(|(key, data)| {
                let cell = TileCell {
                    is_empty: data.iter().all(|&p| p == Color32::TRANSPARENT),
                    data: Some(data),
                };
                (key, std::sync::Arc::new(Mutex::new(cell)))
            })
            .collect();
        *template.tiles.lock().unwrap_or_else(|e| e.into_inner()) = map;
        template
    }

    /// The nearest entry below `i` in the same folder (masks skipped).
    fn sibling_below(&self, i: usize) -> Option<usize> {
        let parent = self.layers.get(i)?.parent;
        (0..i).rev().find(|&j| {
            let l = &self.layers[j];
            l.parent == parent && !matches!(l.kind, LayerKind::Mask { .. })
        })
    }

    /// What clipped entry `i` clips to: the nearest unclipped sibling below.
    fn clip_base(&self, i: usize) -> Option<usize> {
        let mut j = self.sibling_below(i)?;
        while self.layers[j].clipped {
            j = self.sibling_below(j)?;
        }
        Some(j)
    }

    /// Replace the entries `removed` with `merged`, which goes where the
    /// lowest of them was. Returns what undoes it.
    fn replace_with_merged(&mut self, removed: Vec<usize>, merged: Layer) -> LayerSwap {
        let mut removed = removed;
        // Masks go with their layers.
        let owners: Vec<LayerId> = removed.iter().map(|&i| self.layers[i].id).collect();
        for owner in owners {
            if let Some(m) = self.mask_index_of(owner) {
                removed.push(m);
            }
        }
        removed.sort_unstable();
        removed.dedup();
        let lowest = removed[0];
        let at = (0..lowest).filter(|i| !removed.contains(i)).count();
        let mut swap = LayerSwap {
            layers: vec![(at, merged)],
            remove: removed.iter().map(|&i| self.layers[i].id).collect(),
            active_layer_idx: at,
            applied: (Vec::new(), Vec::new()),
        };
        self.swap_layers(&mut swap);
        swap
    }

    /// Merge entry `idx` (a layer, or a mask standing for its layer) into
    /// the layer under it, as it shows. Over a layer with a blend mode,
    /// the result keeps that mode and opacity (the upper layer is merged
    /// into it as it shows over it alone); otherwise opacity, masks and
    /// clipping are all baked in.
    pub fn merge_down(&mut self, idx: usize) -> Result<LayerSwap, &'static str> {
        let idx = match self.layers.get(idx).map(|l| l.kind) {
            Some(LayerKind::Mask { owner }) => self.layer_index_of(owner).ok_or("No layer")?,
            Some(_) => idx,
            None => return Err("No layer"),
        };
        if idx == 0 {
            return Err("Nothing below the background");
        }
        if self.layers[idx].kind == LayerKind::Group {
            return Err("Merge Down works on layers, not folders");
        }
        let below = self
            .sibling_below(idx)
            .ok_or("Nothing below to merge into")?;
        let lower = &self.layers[below];
        if lower.kind == LayerKind::Group {
            return Err("The layer below is a folder");
        }
        if lower.adjustment.is_some() {
            return Err("The layer below is an adjustment layer");
        }
        if lower.locked && below != 0 {
            return Err("The layer below is locked");
        }
        let keep_blend = lower.blend != LayerBlend::Normal;
        let both_clipped = lower.clipped && self.layers[idx].clipped;
        let ids = [self.layers[idx].id, lower.id];
        let view = self.view(|i, l| {
            if matches!(l.kind, LayerKind::Mask { owner } if ids.contains(&owner)) {
                return;
            }
            if i == below {
                l.parent = None;
                l.clipped = false;
                if keep_blend {
                    l.blend = LayerBlend::Normal;
                    l.opacity = 1.0;
                }
                // The merge happens whether or not they're shown.
                l.visible = true;
            } else if i == idx {
                l.parent = None;
                l.clipped &= !both_clipped;
                l.visible = true;
            } else {
                l.visible = false;
            }
        });
        let tiles = view.composite_tiles(self.tiles_of(&[below, idx]), below == 0);
        let mut merged = self.layers[below].merged_from();
        if !keep_blend {
            merged.opacity = 1.0;
        }
        let merged = Self::layer_with_tiles(merged, tiles);
        Ok(self.replace_with_merged(vec![below, idx], merged))
    }

    /// Merge every layer that shows (drafts aside) into one, where the
    /// lowest of them was; hidden layers stay as they are.
    pub fn merge_visible(&mut self) -> Result<LayerSwap, &'static str> {
        let n = self.layers.len();
        let shows = |i: usize| {
            let l = &self.layers[i];
            let mut current = Some(l.id);
            while let Some(id) = current {
                let Some(e) = self.layer_index_of(id).map(|j| &self.layers[j]) else {
                    break;
                };
                if !e.visible {
                    return false;
                }
                current = e.parent;
            }
            !self.is_draft(i)
        };
        let mut merged: Vec<usize> = (0..n)
            .filter(|&i| self.layers[i].kind == LayerKind::Paint && shows(i))
            .collect();
        // A clipped layer shows only through its base.
        merged.retain(|&i| !self.layers[i].clipped || self.clip_base(i).is_none_or(&shows));
        let chosen: HashSet<usize> = merged.iter().copied().collect();
        if chosen.len() < 2 {
            return Err("Nothing to merge: fewer than two layers show");
        }
        let view = self.view(|i, l| {
            if l.kind == LayerKind::Paint && !chosen.contains(&i) {
                l.visible = false;
            }
            if l.draft && !matches!(l.kind, LayerKind::Mask { .. }) {
                l.visible = false;
            }
        });
        let tiles = view.composite_tiles(self.tiles_of(&merged), merged[0] == 0);
        // Folders left empty go too (deepest first, so their parents see it).
        let mut removed = chosen.clone();
        loop {
            let empty: Vec<usize> = (0..n)
                .filter(|&i| {
                    let l = &self.layers[i];
                    l.kind == LayerKind::Group
                        && !removed.contains(&i)
                        && shows(i)
                        && self.layers.iter().enumerate().all(|(j, c)| {
                            c.parent != Some(l.id)
                                || matches!(c.kind, LayerKind::Mask { .. })
                                || removed.contains(&j)
                        })
                })
                .collect();
            if empty.is_empty() {
                break;
            }
            removed.extend(empty);
        }
        let lowest = *merged.first().expect("two layers");
        let mut layer = self.layers[lowest].merged_from();
        layer.parent = None;
        layer.opacity = 1.0;
        layer.blend = LayerBlend::Normal;
        layer.clipped = false;
        layer.visible = true;
        if lowest != 0 {
            layer.locked = false;
            layer.alpha_locked = false;
            layer.position_locked = false;
            layer.reference = false;
        }
        let layer = Self::layer_with_tiles(layer, tiles);
        let mut removed: Vec<usize> = removed.into_iter().collect();
        removed.sort_unstable();
        Ok(self.replace_with_merged(removed, layer))
    }

    /// Flatten everything into the background, as it shows. Hidden layers
    /// are dropped; draft layers (and the folders holding them) stay.
    pub fn flatten_image(&mut self) -> Result<LayerSwap, &'static str> {
        let n = self.layers.len();
        let kept: Vec<usize> = (1..n)
            .filter(|&i| {
                let id = self.layers[i].id;
                self.is_draft(i)
                    || (self.layers[i].kind == LayerKind::Group
                        && (0..n).any(|j| {
                            j != i && self.is_draft(j) && self.is_within(self.layers[j].id, id)
                        }))
            })
            .collect();
        let removed: Vec<usize> = (0..n).filter(|i| !kept.contains(i)).collect();
        if removed.len() < 2 {
            return Err("Nothing to flatten");
        }
        let view = self.view(|_, l| {
            if l.draft && !matches!(l.kind, LayerKind::Mask { .. }) {
                l.visible = false;
            }
        });
        let tiles = view.composite_tiles(self.tiles_of(&[0]), true);
        let mut background = self.layers[0].merged_from();
        background.parent = None;
        background.opacity = 1.0;
        background.blend = LayerBlend::Normal;
        background.clipped = false;
        background.visible = true;
        background.draft = false;
        let background = Self::layer_with_tiles(background, tiles);
        Ok(self.replace_with_merged(removed, background))
    }
}
