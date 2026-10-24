//! Others' strokes painted here as they paint them, with their brush.
//!
//! The stroke worker reports what it's given (see
//! [`StrokeWorker::set_tap`]): the brush and seed a stroke begins with,
//! then its pen samples. They're sent as they come, a batch a frame. The
//! brush goes as a preset file, once per brush.
//!
//! Here each painter gets a stroke worker of their own, painting on a
//! scratch canvas of the target layer's settings (the brush engine paints
//! the active layer, and the document's is this painter's). The tiles a
//! stroke is about to reach are copied onto it first. Each frame, the
//! pixels it changed are copied onto the layer.
//!
//! The pixels the painter sends when the stroke ends are still what the
//! document keeps (a stroke painted twice comes out a little different:
//! timing, floating point). Where this painting differs from them, it's
//! put back as it was before the stroke.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use eframe::egui::{Color32, Vec2};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::PainterApp;
use crate::brush_engine::brush::{Brush, BrushPreset};
use crate::brush_engine::dynamics::{PenBarrel, PenTilt};
use crate::brush_engine::stroke_worker::{StrokeSetup, StrokeWorker, Tapped};
use crate::brush_engine::symmetry::{Symmetry, SymmetryMode};
use crate::canvas::Canvas;
use crate::canvas::storage::LayerId;

use super::Patch;

/// A stroke's part, as sent.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(super) enum StrokeEvent {
    Begin {
        layer: u64,
        /// Which of the sender's brushes (the latest sent with this number).
        brush: u64,
        second: [u8; 4],
        symmetry: WireSymmetry,
        view_scale: f32,
        wrap: bool,
        wet_gravity: [f32; 2],
        seed: u64,
    },
    Sample {
        pos: [f32; 2],
        pressure: f32,
        /// Seconds on the sender's clock.
        time: f64,
        tilt: Option<[f32; 2]>,
        rotation: Option<f32>,
        wheel: Option<f32>,
        awaiting: Option<u64>,
    },
    Update {
        index: u64,
        pressure: f32,
    },
    End,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub(super) struct WireSymmetry {
    mode: u8,
    center: [f32; 2],
    angle: f32,
    count: u32,
    mirrored: bool,
}

impl From<Symmetry> for WireSymmetry {
    fn from(s: Symmetry) -> Self {
        let mode = match s.mode {
            SymmetryMode::Off => 0,
            SymmetryMode::Vertical => 1,
            SymmetryMode::Horizontal => 2,
            SymmetryMode::Both => 3,
            SymmetryMode::Radial => 4,
        };
        Self {
            mode,
            center: s.center.into(),
            angle: s.angle,
            count: s.count,
            mirrored: s.mirrored,
        }
    }
}

impl From<WireSymmetry> for Symmetry {
    fn from(w: WireSymmetry) -> Self {
        let mode = match w.mode {
            1 => SymmetryMode::Vertical,
            2 => SymmetryMode::Horizontal,
            3 => SymmetryMode::Both,
            4 => SymmetryMode::Radial,
            _ => SymmetryMode::Off,
        };
        Self {
            mode,
            center: w.center.into(),
            angle: w.angle,
            count: w.count.clamp(2, crate::brush_engine::symmetry::MAX_COUNT),
            mirrored: w.mirrored,
        }
    }
}

/// This painter's strokes on their way out.
pub(super) struct Outgoing {
    rx: Receiver<Tapped>,
    /// The brush last sent, and its number.
    sent: Option<(u64, Brush)>,
    /// A stroke is on that the others can paint (not one on no layer).
    painting: bool,
}

impl Outgoing {
    pub(super) fn new(rx: Receiver<Tapped>) -> Self {
        Self {
            rx,
            sent: None,
            painting: false,
        }
    }

    /// Someone new is in: the brush goes again with the next stroke.
    pub(super) fn forget_brush(&mut self) {
        self.sent = None;
    }

    /// What was painted since, as batches of events, each with the bytes
    /// of the brush its stroke begins with if not sent yet (a frame's
    /// worth is one batch, unless two strokes in it need a brush each).
    pub(super) fn take(&mut self) -> Vec<(Vec<StrokeEvent>, Vec<u8>)> {
        let mut batches: Vec<(Vec<StrokeEvent>, Vec<u8>)> = Vec::new();
        let mut events = Vec::new();
        let mut blob = Vec::new();
        for tapped in self.rx.try_iter() {
            let event = match tapped {
                Tapped::Begin {
                    brush,
                    layer,
                    symmetry,
                    view_scale,
                    wrap,
                    wet_gravity,
                    seed,
                } => {
                    self.painting = layer.is_some();
                    let Some(layer) = layer else {
                        continue;
                    };
                    let same = self.sent.as_ref().is_some_and(|(_, b)| {
                        crate::brush_engine::preset_file::same_settings(b, &brush)
                    });
                    if !same {
                        let preset = BrushPreset {
                            name: "Painting together".into(),
                            brush: (*brush).clone(),
                            file: None,
                        };
                        let Ok(bytes) = crate::brush_engine::preset_file::encode(&[preset]) else {
                            self.painting = false;
                            continue;
                        };
                        if !blob.is_empty() {
                            batches.push((std::mem::take(&mut events), std::mem::take(&mut blob)));
                        }
                        blob = bytes;
                        let key = self.sent.as_ref().map_or(1, |(k, _)| k + 1);
                        self.sent = Some((key, (*brush).clone()));
                    }
                    let key = self.sent.as_ref().map_or(0, |(k, _)| *k);
                    StrokeEvent::Begin {
                        layer: layer.0,
                        brush: key,
                        second: brush.second_color.to_array(),
                        symmetry: symmetry.into(),
                        view_scale,
                        wrap,
                        wet_gravity: wet_gravity.into(),
                        seed,
                    }
                }
                _ if !self.painting => continue,
                Tapped::Sample {
                    pos,
                    pressure,
                    time,
                    tilt,
                    barrel,
                    awaiting,
                } => StrokeEvent::Sample {
                    pos: pos.into(),
                    pressure,
                    time,
                    tilt: tilt.map(|t| [t.lean, t.direction]),
                    rotation: barrel.rotation,
                    wheel: barrel.wheel,
                    awaiting,
                },
                Tapped::Update { index, pressure } => StrokeEvent::Update { index, pressure },
                Tapped::End => {
                    self.painting = false;
                    StrokeEvent::End
                }
            };
            events.push(event);
        }
        if !events.is_empty() {
            batches.push((events, blob));
        }
        batches
    }
}

/// Others' strokes being painted here, by painter.
#[derive(Default)]
pub(super) struct Replays {
    painters: HashMap<u32, Painter>,
}

#[derive(Default)]
struct Painter {
    /// Made with their first stroke.
    worker: Option<StrokeWorker>,
    /// Their brush, by its number.
    brush: Option<(u64, Brush)>,
    current: Option<Replayed>,
    /// Strokes ended whose pixels haven't come yet.
    done: Vec<Replayed>,
}

/// A stroke painted here.
struct Replayed {
    layer: LayerId,
    scratch: Arc<Canvas>,
    /// The layer's pixels where they were copied onto the scratch canvas
    /// (a missing tile as clear ones).
    base: FxHashMap<(i32, i32), Vec<Color32>>,
    /// What it last put on the document, per tile (from `base`): a dab
    /// painted for now and taken back again (drawn ahead of the pen, or
    /// with a pressure still to come) goes from the document too.
    shown: FxHashMap<(i32, i32), Vec<Color32>>,
    /// The last sample, for copying ahead of the brush.
    last: Option<Vec2>,
    /// How far from the pen it may paint (canvas pixels).
    reach: f32,
    symmetry: Symmetry,
    wrap: bool,
}

impl Replays {
    /// Someone left: their strokes go (what was painted of them stays,
    /// until their pixels or the host's checksums say otherwise).
    pub(super) fn forget(&mut self, id: u32) {
        self.painters.remove(&id);
    }
}

impl PainterApp {
    /// Paint `events` from painter `from` (`blob`: a brush, if one came).
    pub(super) fn replay_events(
        &mut self,
        replays: &mut Replays,
        from: u32,
        events: Vec<StrokeEvent>,
        blob: &[u8],
    ) {
        let painter = replays.painters.entry(from).or_default();
        for event in events {
            match event {
                StrokeEvent::Begin {
                    layer,
                    brush,
                    second,
                    symmetry,
                    view_scale,
                    wrap,
                    wet_gravity,
                    seed,
                } => {
                    if !blob.is_empty() && painter.brush.as_ref().is_none_or(|(k, _)| *k != brush) {
                        let decoded = crate::brush_engine::preset_file::decode(blob);
                        if let Some(preset) = decoded.ok().and_then(|p| p.into_iter().next()) {
                            painter.brush = Some((brush, preset.brush));
                        }
                    }
                    if let Some(r) = painter.current.take() {
                        // (Its end never came.)
                        if let Some(w) = &painter.worker {
                            w.end();
                        }
                        painter.done.push(r);
                    }
                    painter.current = self.replay_begin(
                        painter,
                        LayerId(layer),
                        brush,
                        Color32::from_rgba_premultiplied(
                            second[0], second[1], second[2], second[3],
                        ),
                        symmetry.into(),
                        view_scale,
                        wrap,
                        Vec2::from(wet_gravity),
                        seed,
                    );
                }
                StrokeEvent::Sample {
                    pos,
                    pressure,
                    time,
                    tilt,
                    rotation,
                    wheel,
                    awaiting,
                } => {
                    let (Some(r), Some(worker)) = (painter.current.as_mut(), &painter.worker)
                    else {
                        continue;
                    };
                    let pos = Vec2::from(pos);
                    if !pos.is_finite() {
                        continue;
                    }
                    self.copy_ahead(r, pos);
                    let tilt = tilt.map(|[lean, direction]| PenTilt { lean, direction });
                    let barrel = PenBarrel { rotation, wheel };
                    let pressure = if pressure.is_finite() {
                        pressure.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    worker.sample_awaiting(pos, pressure, tilt, barrel, Some(time), awaiting);
                }
                StrokeEvent::Update { index, pressure } => {
                    if let (Some(_), Some(worker)) = (&painter.current, &painter.worker) {
                        worker.update(index, pressure.clamp(0.0, 1.0));
                    }
                }
                StrokeEvent::End => {
                    if let Some(r) = painter.current.take() {
                        if let Some(w) = &painter.worker {
                            w.end();
                        }
                        painter.done.push(r);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn replay_begin(
        &self,
        painter: &mut Painter,
        layer: LayerId,
        brush: u64,
        second: Color32,
        symmetry: Symmetry,
        view_scale: f32,
        wrap: bool,
        wet_gravity: Vec2,
        seed: u64,
    ) -> Option<Replayed> {
        let (_, brush) = painter.brush.as_ref().filter(|(k, _)| *k == brush)?;
        // Wet paint and impasto paint the layer's own state besides its
        // pixels: those strokes come as pixels only.
        if brush.wet.is_some() || brush.impasto.is_some() {
            return None;
        }
        let mut brush = brush.clone();
        brush.second_color = second;
        let idx = self.canvas.layer_index_of(layer)?;
        let scratch = Arc::new(self.canvas.scratch_for(idx)?);
        let worker = painter.worker.get_or_insert_with(StrokeWorker::new);
        let reach = brush.brush_options.diameter * 3.0 + self.canvas.tile_size() as f32;
        worker.begin(StrokeSetup {
            canvas: Arc::clone(&scratch),
            brush,
            selection: None,
            pool: Arc::clone(&self.workspace.pool),
            layer_idx: 0,
            symmetry,
            view_scale: if view_scale.is_finite() {
                view_scale.max(0.01)
            } else {
                1.0
            },
            wrap,
            perspective: Vec::new(),
            wet_gravity,
            inside: None,
            seed: Some(seed),
        });
        Some(Replayed {
            layer,
            scratch,
            base: FxHashMap::default(),
            shown: FxHashMap::default(),
            last: None,
            reach,
            symmetry,
            wrap,
        })
    }

    /// Copy the layer's tiles the stroke may reach going on to `pos`
    /// (and its mirror images) onto its scratch canvas, before the worker
    /// gets there.
    fn copy_ahead(&self, r: &mut Replayed, pos: Vec2) {
        let Some(idx) = self.canvas.layer_index_of(r.layer) else {
            return;
        };
        let from = r.last.unwrap_or(pos);
        r.last = Some(pos);
        let (lo, hi) = (
            from.min(pos) - Vec2::splat(r.reach),
            from.max(pos) + Vec2::splat(r.reach),
        );
        let corners = [lo, hi, Vec2::new(lo.x, hi.y), Vec2::new(hi.x, lo.y)];
        let mut boxes = vec![(lo, hi)];
        let center = r.symmetry.center;
        for copy in r.symmetry.copies() {
            let [a, b, c, d] = copy.m;
            let mapped = corners.map(|p| {
                let v = p - center;
                center + Vec2::new(a * v.x + b * v.y, c * v.x + d * v.y)
            });
            let min = mapped.iter().fold(Vec2::splat(f32::MAX), |m, p| m.min(*p));
            let max = mapped.iter().fold(Vec2::splat(f32::MIN), |m, p| m.max(*p));
            boxes.push((min, max));
        }
        let ts = self.canvas.tile_size() as f32;
        let cols = self.canvas.width().div_ceil(ts as usize) as i32;
        let rows = self.canvas.height().div_ceil(ts as usize) as i32;
        for (min, max) in boxes {
            let (tx0, ty0) = ((min.x / ts).floor() as i32, (min.y / ts).floor() as i32);
            let (tx1, ty1) = ((max.x / ts).floor() as i32, (max.y / ts).floor() as i32);
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    let (tx, ty) = if r.wrap {
                        (tx.rem_euclid(cols), ty.rem_euclid(rows))
                    } else if tx < 0 || ty < 0 || tx >= cols || ty >= rows {
                        continue;
                    } else {
                        (tx, ty)
                    };
                    if r.base.contains_key(&(tx, ty)) {
                        continue;
                    }
                    let tile = self.canvas.get_layer_tile_data(idx, tx, ty);
                    let ts = ts as usize;
                    let base = match tile {
                        Some(tile) => {
                            r.scratch.set_layer_tile_data(0, tx, ty, tile.clone());
                            tile
                        }
                        None => vec![Color32::TRANSPARENT; ts * ts],
                    };
                    r.base.insert((tx, ty), base);
                }
            }
        }
    }

    /// Copy what the strokes painted since last time onto the document.
    pub(super) fn replay_copy(&mut self, replays: &mut Replays) {
        for painter in replays.painters.values_mut() {
            let Some(worker) = &painter.worker else {
                continue;
            };
            let dirty = worker.take_dirty();
            // (Their undo records: the painter's own pixels are what count.)
            let _ = worker.take_finished();
            for r in painter.done.iter_mut().chain(&mut painter.current) {
                for &(tx, ty) in dirty.keys() {
                    self.replay_copy_tile(r, tx as i32, ty as i32, None);
                }
            }
        }
    }

    /// Bring tile `(tx, ty)` of the document up to date with what `r`
    /// painted there. `revert`: put back as before the stroke instead,
    /// where `covered` (the painter's own pixels) doesn't say otherwise.
    /// Only pixels still as this stroke left them are touched (no one
    /// wrote over them since).
    fn replay_copy_tile(
        &mut self,
        r: &mut Replayed,
        tx: i32,
        ty: i32,
        revert: Option<&dyn Fn(usize) -> bool>,
    ) {
        let (Some(base), Some(idx)) = (r.base.get(&(tx, ty)), self.canvas.layer_index_of(r.layer))
        else {
            return;
        };
        // (No tile: nothing painted there.)
        let painted = (r.scratch.get_layer_tile_data(0, tx, ty)).unwrap_or_else(|| base.clone());
        let shown = r.shown.entry((tx, ty)).or_insert_with(|| base.clone());
        let ts = self.canvas.tile_size();
        let mut tile = self
            .canvas
            .get_layer_tile_data(idx, tx, ty)
            .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts]);
        let mut changed = false;
        let n = tile
            .len()
            .min(painted.len())
            .min(base.len())
            .min(shown.len());
        for i in 0..n {
            let want = match revert {
                None => painted[i],
                Some(covered) if covered(i) => continue,
                Some(_) => base[i],
            };
            if want == shown[i] || tile[i] != shown[i] {
                continue;
            }
            tile[i] = want;
            shown[i] = want;
            changed = true;
        }
        if changed {
            self.canvas.set_layer_tile_data(idx, tx, ty, tile);
            self.mark_tile_dirty(tx as usize, ty as usize);
            self.layer_state.thumbnails_dirty = true;
        }
    }

    /// Painter `from`'s pixels have come: their ended strokes painted here
    /// are done with, put back as before where they differ from them.
    pub(super) fn replay_settle(&mut self, replays: &mut Replays, from: u32, patches: &[Patch]) {
        let Some(painter) = replays.painters.get_mut(&from) else {
            return;
        };
        if painter.done.is_empty() {
            return;
        }
        if let Some(worker) = &painter.worker {
            worker.wait_idle();
            let _ = worker.take_dirty();
            let _ = worker.take_finished();
        }
        let done = std::mem::take(&mut painter.done);
        let ts = self.canvas.tile_size();
        for mut r in done {
            let keys: Vec<(i32, i32)> = r.base.keys().copied().collect();
            for (tx, ty) in keys {
                // Pixels the painter's own cover, by tile-local index.
                let ours: Vec<&Patch> = patches
                    .iter()
                    .filter(|p| p.at.layer == r.layer.0 && p.at.tx == tx && p.at.ty == ty)
                    .collect();
                let covered = |i: usize| {
                    let (x, y) = (i % ts, i / ts);
                    ours.iter().any(|p| {
                        let (x0, y0) = (p.at.x0 as usize, p.at.y0 as usize);
                        let (w, h) = (p.at.w as usize, p.at.h as usize);
                        (x0..x0 + w).contains(&x)
                            && (y0..y0 + h).contains(&y)
                            && p.mask[(y - y0) * w + (x - x0)]
                    })
                };
                // The last of it first, then back where it's not theirs.
                self.replay_copy_tile(&mut r, tx, ty, None);
                self.replay_copy_tile(&mut r, tx, ty, Some(&covered));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color32 = Color32::from_rgb(255, 0, 0);

    /// A dab painted for now and taken back (drawn ahead of the pen, or
    /// with a pressure still to come) goes from the canvas too.
    #[test]
    fn a_dab_taken_back_goes_from_the_canvas_too() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        let scratch = Arc::new(app.canvas.scratch_for(1).unwrap());
        let clear = vec![Color32::TRANSPARENT; 64 * 64];
        let mut r = Replayed {
            layer: LayerId(1),
            scratch: Arc::clone(&scratch),
            base: [((0, 0), clear.clone())].into_iter().collect(),
            shown: FxHashMap::default(),
            last: None,
            reach: 0.0,
            symmetry: Symmetry::default(),
            wrap: false,
        };
        let shown = |app: &PainterApp| app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[5];
        let mut dab = clear.clone();
        dab[5] = RED;
        scratch.set_layer_tile_data(0, 0, 0, dab);
        app.replay_copy_tile(&mut r, 0, 0, None);
        assert_eq!(shown(&app), RED);
        // Taken back on the scratch canvas.
        scratch.set_layer_tile_data(0, 0, 0, clear.clone());
        app.replay_copy_tile(&mut r, 0, 0, None);
        assert_eq!(shown(&app), Color32::TRANSPARENT);
        // Painted again, then the painter's pixels don't have it: put back.
        let mut dab = clear;
        dab[5] = RED;
        scratch.set_layer_tile_data(0, 0, 0, dab);
        app.replay_copy_tile(&mut r, 0, 0, None);
        app.replay_copy_tile(&mut r, 0, 0, Some(&|_| false));
        assert_eq!(shown(&app), Color32::TRANSPARENT);
    }
}
