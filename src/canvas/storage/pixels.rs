//! Pixel writers with undo: painting colours, masks and regions into a
//! layer, mapping its pixels, and capturing/restoring areas.

use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui::Color32;

use super::{Canvas, SharedCell, TileCell, TileKey};
use crate::canvas::color::{Color, ColorManipulation};
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::selection::SelectionManager;

/// Some of a layer's tiles as they were, for [`Canvas::paint_over_region`].
pub struct Region {
    /// The canvas area painted, `[x0, y0, x1, y1)`.
    pub bounds: [i32; 4],
    /// Each tile's pixels as they were, and the tile itself (looked up
    /// once: the layer's tile map is behind one lock).
    tiles: Vec<RegionTile>,
}

/// A tile's key, its pixels as they were, and the tile.
type RegionTile = ((i32, i32), Vec<Color32>, Arc<Mutex<TileCell>>);

impl Canvas {
    /// Clear the active layer to the provided color (or transparent for non-background).
    pub fn clear(&mut self, color: Color) {
        self.clear_color = color.to_color32();
        if let Some(layer) = self.layers.get(self.active_layer_idx) {
            let tiles = layer.tiles.lock().unwrap_or_else(|e| e.into_inner());
            for tile_arc in tiles.values() {
                let mut cell = tile_arc.lock().unwrap_or_else(|e| e.into_inner());
                cell.data = None;
                cell.is_empty = true;
            }
        }
    }

    pub fn capture_layer_pixels(&self, layer_idx: usize) -> HashMap<(i32, i32), Vec<Color32>> {
        let Some(layer) = self.layers.get(layer_idx) else {
            return HashMap::new();
        };
        let cells: Vec<(TileKey, SharedCell)> = layer
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, c)| (k, c.clone()))
            .collect();
        // Copy the tiles in parallel (a whole 4K layer is ~64 MB).
        cells
            .par_iter()
            .filter_map(|(key, cell)| {
                let guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                guard.data.clone().map(|d| (*key, d))
            })
            .collect()
    }

    /// Replace every pixel of layer `layer_idx` by `f(state, pixel, x, y)`
    /// (inside the selection, if any), recording changed tiles in
    /// `history`. Each tile gets its own `state` from `init` (a cache, say).
    /// Returns the changed canvas area.
    pub fn map_layer_pixels<S>(
        &self,
        layer_idx: usize,
        selection: Option<&SelectionManager>,
        init: impl Fn() -> S + Sync,
        f: impl Fn(&mut S, Color32, i32, i32) -> Color32 + Sync,
        history: &mut UndoAction,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let layer_id = layer.id;
        let ts = self.tile_size;
        let cells: Vec<(TileKey, SharedCell)> = layer
            .tiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, c)| (k, c.clone()))
            .collect();
        let changed: Vec<(TileKey, TileSnapshot)> = cells
            .par_iter()
            .filter_map(|((tx, ty), cell)| {
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                if cell.is_empty {
                    return None;
                }
                let data = cell.data.as_mut()?;
                let before = data.clone();
                let mut inside = vec![true; ts];
                let mut any = false;
                let mut state = init();
                for ly in 0..ts {
                    let y = *ty * ts as i32 + ly as i32;
                    if y < 0 {
                        continue;
                    }
                    if let Some(sel) = selection {
                        sel.row_mask(y as usize, (*tx * ts as i32).max(0) as usize, &mut inside);
                    }
                    for (lx, _) in inside.iter().enumerate().filter(|(_, i)| **i) {
                        let i = ly * ts + lx;
                        let out = f(&mut state, data[i], *tx * ts as i32 + lx as i32, y);
                        if out != data[i] {
                            data[i] = out;
                            any = true;
                        }
                    }
                }
                if !any {
                    return None;
                }
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                Some((
                    (*tx, *ty),
                    TileSnapshot {
                        tx: *tx,
                        ty: *ty,
                        layer_id,
                        x0: 0,
                        y0: 0,
                        width: ts,
                        height: ts,
                        data: before.into(),
                    },
                ))
            })
            .collect();
        if changed.is_empty() {
            return None;
        }
        let (mut min, mut max) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for ((tx, ty), snap) in changed {
            min = (min.0.min(tx), min.1.min(ty));
            max = (max.0.max(tx + 1), max.1.max(ty + 1));
            history.tiles.push(snap);
        }
        let t = ts as f32;
        Some(eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(min.0 as f32 * t, min.1 as f32 * t),
            eframe::egui::pos2(max.0 as f32 * t, max.1 as f32 * t),
        ))
    }

    /// Write `pixels` (row-major, `w`×`h`) into layer `layer_idx` at `(x, y)`,
    /// the region given as `(x, y, w, h)`
    /// (clipped to the canvas). Each tile's content before its first write
    /// is saved into `before` (if given), for undo.
    pub fn write_layer_region(
        &self,
        layer_idx: usize,
        (x, y, w, h): (i32, i32, usize, usize),
        pixels: &[Color32],
        mut before: Option<&mut HashMap<(i32, i32), Vec<Color32>>>,
    ) {
        let ts = self.tile_size as i32;
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = (
            (x + w as i32).min(self.width as i32),
            (y + h as i32).min(self.height as i32),
        );
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for ty in y0 / ts..=(y1 - 1) / ts {
            for tx in x0 / ts..=(x1 - 1) / ts {
                let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) else {
                    continue;
                };
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; (ts * ts) as usize]);
                if let Some(before) = before.as_deref_mut() {
                    before.entry((tx, ty)).or_insert_with(|| data.clone());
                }
                let (ox, oy) = (tx * ts, ty * ts);
                let (cx0, cx1) = (x0.max(ox), x1.min(ox + ts));
                for py in y0.max(oy)..y1.min(oy + ts) {
                    let src = ((py - y) as usize) * w + (cx0 - x) as usize;
                    let dst = ((py - oy) * ts + (cx0 - ox)) as usize;
                    let n = (cx1 - cx0) as usize;
                    data[dst..dst + n].copy_from_slice(&pixels[src..src + n]);
                }
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
            }
        }
    }

    /// Paint `color` (unmultiplied) into layer `layer_idx` with `mask` as
    /// coverage, recording the touched tiles in `history`. Alpha-locked
    /// layers only recolour. Returns the changed canvas area.
    pub fn paint_mask(
        &self,
        layer_idx: usize,
        mask: &crate::selection::SelectionMask,
        color: Color32,
        history: &mut UndoAction,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let layer_id = layer.id;
        let alpha_lock = layer.alpha_locked;
        let ts = self.tile_size as i32;
        let [bx0, by0, bx1, by1] = mask.content_bounds()?;
        let (bx0, by0) = (bx0.max(0), by0.max(0));
        let (bx1, by1) = (bx1.min(self.width as i32), by1.min(self.height as i32));
        if bx1 <= bx0 || by1 <= by0 {
            return None;
        }
        let mut cells = Vec::new();
        for ty in by0 / ts..=(by1 - 1) / ts {
            for tx in bx0 / ts..=(bx1 - 1) / ts {
                if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
                    cells.push(((tx, ty), cell));
                }
            }
        }
        let tile_len = (ts * ts) as usize;
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        // The fill colour at every coverage level, as pixels (egui colours
        // are the canvas's pixel format).
        let colors: Vec<Color32> = (0..=255u32)
            .map(|cov| {
                let alpha = ((a as u32 * cov + 127) / 255) as u8;
                Color32::from_rgba_unmultiplied(r, g, b, alpha)
            })
            .collect();
        let snapshots: Vec<TileSnapshot> = cells
            .par_iter()
            .filter_map(|&((tx, ty), ref cell)| {
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; tile_len]);
                let before = data.clone();
                let mut changed = false;
                for ly in 0..ts {
                    for lx in 0..ts {
                        let cov = mask.value(tx * ts + lx, ty * ts + ly);
                        if cov == 0 {
                            continue;
                        }
                        let i = (ly * ts + lx) as usize;
                        let dst = data[i];
                        if alpha_lock && dst.a() == 0 {
                            continue;
                        }
                        let src = colors[cov as usize];
                        let mut out = crate::canvas::blend::alpha_over(src, dst);
                        if alpha_lock {
                            out = crate::canvas::blend::with_alpha_of(out, dst.a());
                        }
                        changed |= out != dst;
                        data[i] = out;
                    }
                }
                if !changed {
                    return None;
                }
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                Some(TileSnapshot {
                    tx,
                    ty,
                    layer_id,
                    x0: 0,
                    y0: 0,
                    width: tile_len / ts as usize,
                    height: ts as usize,
                    data: before.into(),
                })
            })
            .collect();
        if snapshots.is_empty() {
            return None;
        }
        history.tiles.extend(snapshots);
        Some(eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(bx0 as f32, by0 as f32),
            eframe::egui::pos2(bx1 as f32, by1 as f32),
        ))
    }

    /// Paint over layer `layer_idx` inside `bounds` (`[x0, y0, x1, y1)`,
    /// canvas pixels) with the premultiplied colour `color_at(x, y)` returns
    /// for each pixel (transparent paints nothing), recording the touched
    /// tiles in `history`. Alpha-locked layers only recolour. Tiles are
    /// processed in parallel. Returns the changed canvas area.
    pub fn paint_region(
        &self,
        layer_idx: usize,
        bounds: [i32; 4],
        color_at: impl Fn(i32, i32) -> Color32 + Sync,
        history: &mut UndoAction,
    ) -> Option<eframe::egui::Rect> {
        let layer = self.layers.get(layer_idx)?;
        let layer_id = layer.id;
        let alpha_lock = layer.alpha_locked;
        let ts = self.tile_size as i32;
        let [bx0, by0, bx1, by1] = bounds;
        let (bx0, by0) = (bx0.max(0), by0.max(0));
        let (bx1, by1) = (bx1.min(self.width as i32), by1.min(self.height as i32));
        if bx1 <= bx0 || by1 <= by0 {
            return None;
        }
        let mut cells = Vec::new();
        for ty in by0 / ts..=(by1 - 1) / ts {
            for tx in bx0 / ts..=(bx1 - 1) / ts {
                if let Some(cell) = self.ensure_layer_tile(layer_idx, tx, ty) {
                    cells.push(((tx, ty), cell));
                }
            }
        }
        let tile_len = (ts * ts) as usize;
        let snapshots: Vec<TileSnapshot> = cells
            .par_iter()
            .filter_map(|&((tx, ty), ref cell)| {
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let data = cell
                    .data
                    .get_or_insert_with(|| vec![Color32::TRANSPARENT; tile_len]);
                let mut before: Option<Vec<Color32>> = None;
                // Only the part of the tile inside the bounds.
                let (lx0, ly0) = ((bx0 - tx * ts).max(0), (by0 - ty * ts).max(0));
                let (lx1, ly1) = ((bx1 - tx * ts).min(ts), (by1 - ty * ts).min(ts));
                for ly in ly0..ly1 {
                    for lx in lx0..lx1 {
                        let src = color_at(tx * ts + lx, ty * ts + ly);
                        if src.a() == 0 {
                            continue;
                        }
                        let i = (ly * ts + lx) as usize;
                        let dst = data[i];
                        if alpha_lock && dst.a() == 0 {
                            continue;
                        }
                        let mut out = crate::canvas::blend::alpha_over(src, dst);
                        if alpha_lock {
                            out = crate::canvas::blend::with_alpha_of(out, dst.a());
                        }
                        if out != dst {
                            before.get_or_insert_with(|| data.clone());
                            data[i] = out;
                        }
                    }
                }
                let before = before?;
                cell.is_empty = data.iter().all(|&p| p == Color32::TRANSPARENT);
                Some(TileSnapshot {
                    tx,
                    ty,
                    layer_id,
                    x0: 0,
                    y0: 0,
                    width: ts as usize,
                    height: ts as usize,
                    data: before.into(),
                })
            })
            .collect();
        if snapshots.is_empty() {
            return None;
        }
        history.tiles.extend(snapshots);
        Some(eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(bx0 as f32, by0 as f32),
            eframe::egui::pos2(bx1 as f32, by1 as f32),
        ))
    }

    /// The layer's tiles covering `bounds` (`[x0, y0, x1, y1)`), as they are
    /// now: the originals a repeatedly previewed paint (the gradient) is
    /// composited over. Missing tiles are created (as their empty colour).
    pub fn capture_region(&self, layer_idx: usize, bounds: [i32; 4]) -> Region {
        let ts = self.tile_size as i32;
        let [bx0, by0, bx1, by1] = self.clip_bounds(bounds);
        let mut keys = Vec::new();
        if bx1 > bx0 && by1 > by0 {
            for ty in by0 / ts..=(by1 - 1) / ts {
                for tx in bx0 / ts..=(bx1 - 1) / ts {
                    keys.push((tx, ty));
                }
            }
        }
        let tile_len = (ts * ts) as usize;
        let cells: Vec<_> = keys
            .iter()
            .filter_map(|&(tx, ty)| Some(((tx, ty), self.ensure_layer_tile(layer_idx, tx, ty)?)))
            .collect();
        let tiles = cells
            .into_par_iter()
            .map(|(key, cell)| {
                let data = cell
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .data
                    .clone()
                    .unwrap_or_else(|| vec![Color32::TRANSPARENT; tile_len]);
                (key, data, cell)
            })
            .collect();
        Region {
            bounds: [bx0, by0, bx1, by1],
            tiles,
        }
    }

    fn clip_bounds(&self, [x0, y0, x1, y1]: [i32; 4]) -> [i32; 4] {
        [
            x0.max(0),
            y0.max(0),
            x1.min(self.width as i32),
            y1.min(self.height as i32),
        ]
    }

    /// Set the layer's pixels in `region` to its originals with
    /// `row(x0, y, out)`'s premultiplied colours (one row of the region's
    /// part of a tile at a time) painted over them. Alpha-locked layers only
    /// recolour. Tiles are processed in parallel.
    pub fn paint_over_region(
        &self,
        layer_idx: usize,
        region: &Region,
        row: impl Fn(i32, i32, &mut [Color32]) + Sync,
    ) {
        let Some(layer) = self.layers.get(layer_idx) else {
            return;
        };
        let alpha_lock = layer.alpha_locked;
        let ts = self.tile_size as i32;
        let [bx0, by0, bx1, by1] = region.bounds;
        region
            .tiles
            .par_iter()
            .for_each(|((tx, ty), original, cell)| {
                let (tx, ty) = (*tx, *ty);
                let (lx0, ly0) = ((bx0 - tx * ts).max(0), (by0 - ty * ts).max(0));
                let (lx1, ly1) = ((bx1 - tx * ts).min(ts), (by1 - ty * ts).min(ts));
                let mut out = original.clone();
                let mut src = vec![Color32::TRANSPARENT; (lx1 - lx0).max(0) as usize];
                for ly in ly0..ly1 {
                    row(tx * ts + lx0, ty * ts + ly, &mut src);
                    let base = (ly * ts) as usize;
                    for (lx, &s) in (lx0..lx1).zip(&src) {
                        if s.a() == 0 {
                            continue;
                        }
                        let i = base + lx as usize;
                        let dst = original[i];
                        if alpha_lock && dst.a() == 0 {
                            continue;
                        }
                        let mut px = crate::canvas::blend::alpha_over(s, dst);
                        if alpha_lock {
                            px = crate::canvas::blend::with_alpha_of(px, dst.a());
                        }
                        out[i] = px;
                    }
                }
                let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                cell.is_empty = out.iter().all(|&p| p == Color32::TRANSPARENT);
                cell.data = Some(out);
            });
    }

    /// Put the layer's pixels in `region` back to its originals.
    pub fn restore_region(&self, region: &Region) {
        region.tiles.par_iter().for_each(|(_, original, cell)| {
            let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
            cell.is_empty = original.iter().all(|&p| p == Color32::TRANSPARENT);
            cell.data = Some(original.clone());
        });
    }

    /// Undo snapshots of the tiles in `region` whose pixels changed since it
    /// was captured.
    pub fn region_snapshots(&self, layer_idx: usize, region: &Region) -> Vec<TileSnapshot> {
        let Some(layer) = self.layers.get(layer_idx) else {
            return Vec::new();
        };
        let (layer_id, ts) = (layer.id, self.tile_size);
        region
            .tiles
            .par_iter()
            .filter_map(|((tx, ty), original, cell)| {
                let (tx, ty) = (*tx, *ty);
                let cell = cell.lock().unwrap_or_else(|e| e.into_inner());
                let changed = cell.data.as_ref().is_some_and(|now| now != original);
                changed.then(|| TileSnapshot {
                    tx,
                    ty,
                    layer_id,
                    x0: 0,
                    y0: 0,
                    width: ts,
                    height: ts,
                    data: original.clone().into(),
                })
            })
            .collect()
    }
}
