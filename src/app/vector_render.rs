//! Vector layers drawn in the background: a change to the lines asks for
//! its area to be drawn again, the areas asked for meanwhile gather, and
//! each frame one job per layer draws them from a copy of the lines it
//! needs (never the canvas, which stays free for the next edit), on the
//! pool. Until its tiles come back the layer shows its old pixels, as
//! Krita's vector layers do.
//!
//! A tile asked for again after a job took it is newer than that job's
//! picture of it: what the job brings for it is dropped and the tile drawn
//! again. Saving, exporting, sampling and anything that needs the pixels
//! as they'll be waits for the drawing first ([`PainterApp::settle_vectors`]).

use crate::app::PainterApp;
use crate::canvas::storage::LayerId;
use crate::canvas::vector::{self, VectorItem};
use eframe::egui::Color32;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::{Arc, mpsc};

/// The drawing going on and asked for, per vector layer.
#[derive(Default)]
pub struct VectorRenders {
    layers: FxHashMap<LayerId, LayerRenders>,
    /// Counts every request: the newer, the higher.
    epoch: u64,
}

#[derive(Default)]
struct LayerRenders {
    /// Tiles to draw again.
    dirty: FxHashSet<(i32, i32)>,
    /// Each tile's latest request.
    asked: FxHashMap<(i32, i32), u64>,
    running: Option<Job>,
}

struct Job {
    /// The requests it covers: up to this one.
    epoch: u64,
    result: mpsc::Receiver<Vec<Drawn>>,
}

/// A tile drawn: its pixels, or `None` where it's all transparent.
type Drawn = ((i32, i32), Option<Vec<Color32>>);

/// What a job draws: rects of whole tiles, each with the items that reach
/// it (shared, so the layer can change meanwhile).
type Work = Vec<([i32; 4], Vec<Arc<VectorItem>>)>;

impl VectorRenders {
    pub(crate) fn busy(&self) -> bool {
        self.layers
            .values()
            .any(|l| l.running.is_some() || !l.dirty.is_empty())
    }
}

impl PainterApp {
    /// Draw vector layer `idx` again over `region` (canvas pixels), in the
    /// background.
    pub(crate) fn request_vector_redraw(&mut self, idx: usize, region: [i32; 4]) {
        self.request_vector_redraw_boxes(idx, &[region]);
    }

    /// [`Self::request_vector_redraw`] over the tiles `boxes` reach (a
    /// line's pieces: a long or diagonal one reaches far fewer tiles than
    /// its box).
    pub(crate) fn request_vector_redraw_boxes(&mut self, idx: usize, boxes: &[[i32; 4]]) {
        let Some(region) = vector::union(boxes.iter().copied()) else {
            return;
        };
        self.sharp_damage(region);
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        let id = layer.id;
        let mut tiles: Vec<(i32, i32)> = boxes.iter().flat_map(|&b| self.vector_tiles(b)).collect();
        tiles.sort_unstable();
        tiles.dedup();
        let renders = &mut self.workspace.vector.renders;
        renders.epoch += 1;
        let epoch = renders.epoch;
        let layer = renders.layers.entry(id).or_default();
        for t in tiles {
            layer.dirty.insert(t);
            layer.asked.insert(t, epoch);
        }
    }

    /// `region` drawn now (a pen's last stretch, small): newer than any
    /// job's picture of these tiles, which won't overwrite it.
    pub(crate) fn mark_vector_drawn(&mut self, idx: usize, region: [i32; 4]) {
        self.sharp_damage(region);
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        let id = layer.id;
        let tiles = self.vector_tiles(region);
        let renders = &mut self.workspace.vector.renders;
        let Some(layer) = renders.layers.get_mut(&id) else {
            return;
        };
        renders.epoch += 1;
        for t in tiles {
            layer.asked.insert(t, renders.epoch);
        }
    }

    /// The canvas tiles `region` reaches (on the canvas).
    fn vector_tiles(&self, region: [i32; 4]) -> Vec<(i32, i32)> {
        let ts = self.canvas.tile_size() as i32;
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let [x0, y0, x1, y1] = [
            region[0].max(0),
            region[1].max(0),
            region[2].min(w),
            region[3].min(h),
        ];
        if x0 >= x1 || y0 >= y1 {
            return Vec::new();
        }
        (y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts))
            .flat_map(|ty| (x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts)).map(move |tx| (tx, ty)))
            .collect()
    }

    /// Whether any vector layer has drawing to come.
    pub(crate) fn vector_busy(&self) -> bool {
        self.workspace.vector.renders.busy()
    }

    /// Once a frame: put in what the jobs drew, and start the next ones.
    /// Returns whether there's drawing still to come.
    pub(crate) fn vector_tick(&mut self) -> bool {
        let ids: Vec<LayerId> = self
            .workspace
            .vector
            .renders
            .layers
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.take_vector_job(id, false);
            self.start_vector_job(id);
        }
        self.workspace
            .vector
            .renders
            .layers
            .retain(|_, l| l.running.is_some() || !l.dirty.is_empty());
        self.vector_busy()
    }

    /// Wait for every vector layer's drawing and put it in: the pixels are
    /// then the lines as they are.
    pub(crate) fn settle_vectors(&mut self) {
        let ids: Vec<LayerId> = self
            .workspace
            .vector
            .renders
            .layers
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.take_vector_job(id, true);
            if let Some(work) = self.vector_work(id) {
                let epoch = self.workspace.vector.renders.epoch;
                let drawn = self.workspace.pool.install(|| draw(work));
                self.put_vector_tiles(id, epoch, drawn);
            }
        }
        self.workspace.vector.renders.layers.clear();
    }

    /// Layer `id`'s finished job, if any (waiting for it with `wait`), put in.
    fn take_vector_job(&mut self, id: LayerId, wait: bool) {
        let renders = &mut self.workspace.vector.renders;
        let Some(layer) = renders.layers.get_mut(&id) else {
            return;
        };
        let Some(job) = &layer.running else {
            return;
        };
        let drawn = if wait {
            job.result.recv().ok()
        } else {
            match job.result.try_recv() {
                Ok(drawn) => Some(drawn),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let epoch = job.epoch;
        layer.running = None;
        if let Some(drawn) = drawn {
            self.put_vector_tiles(id, epoch, drawn);
        }
    }

    /// Start drawing layer `id`'s dirty tiles, unless a job is on already
    /// (what's asked for meanwhile waits for the next: that's what gathers
    /// the requests).
    fn start_vector_job(&mut self, id: LayerId) {
        if (self.workspace.vector.renders.layers.get(&id)).is_none_or(|l| l.running.is_some()) {
            return;
        }
        let Some(work) = self.vector_work(id) else {
            return;
        };
        let renders = &mut self.workspace.vector.renders;
        let epoch = renders.epoch;
        let (send, result) = mpsc::channel();
        if let Some(layer) = renders.layers.get_mut(&id) {
            layer.running = Some(Job { epoch, result });
        }
        self.workspace.pool.spawn(move || {
            let _ = send.send(draw(work));
        });
    }

    /// Layer `id`'s dirty tiles as rects to draw, each with its lines (the
    /// tiles taken: they're being drawn). `None` if there's nothing to.
    fn vector_work(&mut self, id: LayerId) -> Option<Work> {
        let idx = self.canvas.layer_index_of(id);
        let layer = self.workspace.vector.renders.layers.get_mut(&id)?;
        let tiles = std::mem::take(&mut layer.dirty);
        let lines = idx.and_then(|i| self.canvas.layers[i].vector.as_deref())?;
        if tiles.is_empty() {
            return None;
        }
        let ts = self.canvas.tile_size() as i32;
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let work = tile_rects(tiles)
            .into_iter()
            .map(|[tx0, ty0, tx1, ty1]| {
                let rect = [tx0 * ts, ty0 * ts, (tx1 * ts).min(w), (ty1 * ts).min(h)];
                let near = lines.candidates(rect);
                let strokes = near.iter().map(|&i| lines.items()[i].clone()).collect();
                (rect, strokes)
            })
            .collect();
        Some(work)
    }

    /// Put a job's tiles into layer `id`, but not those asked for again
    /// since (`epoch`): they're drawn again instead.
    fn put_vector_tiles(&mut self, id: LayerId, epoch: u64, drawn: Vec<Drawn>) {
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        if self.canvas.layers[idx].vector.is_none() {
            return;
        }
        let ts = self.canvas.tile_size() as i32;
        let mut rect = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        let layer = self.workspace.vector.renders.layers.get_mut(&id);
        let mut stale = Vec::new();
        for ((tx, ty), pixels) in drawn {
            if layer
                .as_ref()
                .is_some_and(|l| l.asked.get(&(tx, ty)).is_some_and(|&e| e > epoch))
            {
                stale.push((tx, ty));
                continue;
            }
            match pixels {
                Some(pixels) => self.canvas.set_layer_tile_data(idx, tx, ty, pixels),
                None if self.canvas.get_layer_tile_data(idx, tx, ty).is_some() => {
                    let empty = vec![Color32::TRANSPARENT; (ts * ts) as usize];
                    self.canvas.set_layer_tile_data(idx, tx, ty, empty);
                }
                None => continue,
            }
            rect = [
                rect[0].min(tx * ts),
                rect[1].min(ty * ts),
                rect[2].max((tx + 1) * ts),
                rect[3].max((ty + 1) * ts),
            ];
        }
        if let Some(layer) = self.workspace.vector.renders.layers.get_mut(&id) {
            layer.dirty.extend(stale);
            // Requests this job answered no longer stand.
            layer.asked.retain(|_, e| *e > epoch);
        }
        if rect[0] < rect[2] {
            self.vector_painted(idx, rect);
        }
    }

    /// Vector layer `idx`'s pixels changed over `rect`: show it (where the
    /// layer is moved, its shown copy too).
    pub(crate) fn vector_painted(&mut self, idx: usize, rect: [i32; 4]) {
        let posed = self.canvas.layers.get(idx).is_some_and(|l| l.is_posed());
        let shown = posed
            .then(|| self.canvas.repose_region(idx, rect.map(|v| v as f32)))
            .flatten();
        match shown {
            Some(s) => self.mark_rect_damage([
                s[0].floor() as i32 - 1,
                s[1].floor() as i32 - 1,
                s[2].ceil() as i32 + 1,
                s[3].ceil() as i32 + 1,
            ]),
            None => self.mark_rect_damage(rect),
        }
        self.layer_state.thumbnails_dirty = true;
    }
}

/// `tiles` as few rects (tile coordinates, `[x0, y0, x1, y1)`): each row's
/// runs, runs alike in rows one under another merged.
fn tile_rects(tiles: FxHashSet<(i32, i32)>) -> Vec<[i32; 4]> {
    let mut sorted: Vec<(i32, i32)> = tiles.into_iter().map(|(x, y)| (y, x)).collect();
    sorted.sort_unstable();
    // Each row's runs of tiles side by side.
    let mut runs: Vec<[i32; 3]> = Vec::new();
    for (y, x) in sorted {
        match runs.last_mut() {
            Some(r) if r[0] == y && r[2] == x => r[2] = x + 1,
            _ => runs.push([y, x, x + 1]),
        }
    }
    let mut rects: Vec<[i32; 4]> = Vec::new();
    let mut open: FxHashMap<(i32, i32), usize> = FxHashMap::default();
    for [y, x0, x1] in runs {
        match open.get(&(x0, x1)).copied() {
            Some(r) if rects[r][3] == y => rects[r][3] = y + 1,
            _ => {
                open.insert((x0, x1), rects.len());
                rects.push([x0, y, x1, y + 1]);
            }
        }
    }
    rects
}

/// Draw `work`, cut into tiles: the rects side by side on the pool (a
/// long line's tiles make many small ones).
fn draw(work: Work) -> Vec<Drawn> {
    use rayon::prelude::*;
    work.into_par_iter()
        .flat_map_iter(|(rect, strokes)| draw_rect(rect, &strokes))
        .collect()
}

/// One rect of [`draw`]'s, cut into tiles.
fn draw_rect(rect: [i32; 4], strokes: &[Arc<VectorItem>]) -> Vec<Drawn> {
    let ts = crate::app::document::TILE_SIZE as i32;
    let pixels = vector::render_region(strokes.iter().map(|s| &**s), rect);
    let w = (rect[2] - rect[0]) as usize;
    let mut out = Vec::new();
    for ty in rect[1] / ts..(rect[3] + ts - 1) / ts {
        for tx in rect[0] / ts..(rect[2] + ts - 1) / ts {
            let (ox, oy) = (tx * ts, ty * ts);
            let mut tile = vec![Color32::TRANSPARENT; (ts * ts) as usize];
            let mut any = false;
            for y in oy.max(rect[1])..(oy + ts).min(rect[3]) {
                let x0 = ox.max(rect[0]);
                let x1 = (ox + ts).min(rect[2]);
                let src = (y - rect[1]) as usize * w + (x0 - rect[0]) as usize;
                let row = &pixels[src..src + (x1 - x0) as usize];
                any |= row.iter().any(|&p| p != Color32::TRANSPARENT);
                let dst = ((y - oy) * ts + x0 - ox) as usize;
                tile[dst..dst + row.len()].copy_from_slice(row);
            }
            out.push(((tx, ty), any.then_some(tile)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_gather_into_few_rects() {
        let tiles: FxHashSet<(i32, i32)> = (0..4)
            .flat_map(|y| (2..7).map(move |x| (x, y)))
            .chain([(0, 9), (1, 9), (5, 9)])
            .collect();
        let mut rects = tile_rects(tiles);
        rects.sort();
        assert_eq!(rects, [[0, 9, 2, 10], [2, 0, 7, 4], [5, 9, 6, 10]]);
    }
}
