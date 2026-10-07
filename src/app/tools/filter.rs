//! Filters on the active layer (Filter menu). A filter with settings opens
//! a dialog and previews on the layer as its sliders move; OK keeps it as
//! one undo step, Cancel puts the layer back. Inside a selection only the
//! selected pixels change (soft edges blend).
//!
//! A filter too slow to rerun on every slider step (a big layer) shows a
//! quick preview while the slider is dragged instead: only the part on
//! screen, at about the screen's resolution, from a shrunk copy of the
//! layer made once. The layer itself gets the exact result when the slider
//! is let go. Adjustment layers' sliders preview the same way (the screen
//! composites at the screen's resolution until they're let go).

use crate::app::PainterApp;
use crate::app::view::render::PreviewView;
use crate::canvas::filters::{Filter, MAX_REACH};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::{LayerId, LayerKind, Region, shrink_tile};
use crate::selection::SelectionMask;
use eframe::egui::{self, Color32};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// A filter being previewed.
pub struct FilterSession {
    pub filter: Filter,
    /// By id: the layers panel stays usable while the dialog is open.
    layer: LayerId,
    /// The area's tiles before the filter (every preview starts from them;
    /// shared with the stroke worker, which runs the filter).
    original: Arc<Region>,
    /// The selection's coverage over the area, if there's a selection.
    coverage: Option<Arc<SelectionMask>>,
    /// A run is on the stroke worker: no other is queued meanwhile.
    running: bool,
    /// Needs running again (a setting changed).
    pub dirty: bool,
    /// The last run took long enough to stall a slider drag: while one is
    /// dragged, show a quick preview instead.
    slow: bool,
    /// The layer holds this filter's exact result (not a preview's).
    exact: bool,
    /// The area's pixels shrunk for previews, made once per resolution.
    source: Option<ShrunkSource>,
    /// What the screen shows while a slider is dragged.
    preview: Option<FilterPreview>,
}

/// The session's area before the filter, shrunk by `block` (each
/// `block`×`block` square of the canvas averaged), with the selection's
/// coverage shrunk the same way.
struct ShrunkSource {
    block: usize,
    /// In shrunk pixels: `[u0, v0, u1, v1)`.
    bounds: [i32; 4],
    pixels: Vec<Color32>,
    coverage: Option<Vec<u8>>,
}

/// The layer's tiles on screen as the filter would make them, shrunk by
/// `block`, for the screen to composite in place of the layer's.
struct FilterPreview {
    view: PreviewView,
    tiles: FxHashMap<(i32, i32), Vec<Color32>>,
}

impl ShrunkSource {
    fn new(original: &Region, coverage: Option<&SelectionMask>, block: usize, ts: usize) -> Self {
        let [x0, y0, x1, y1] = original.bounds;
        let pixels = original.pixels(ts);
        let b = block as i32;
        let bounds = [
            x0.div_euclid(b),
            y0.div_euclid(b),
            (x1 + b - 1).div_euclid(b),
            (y1 + b - 1).div_euclid(b),
        ];
        if block == 1 {
            let coverage = coverage.map(|mask| {
                (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| mask.value(x, y)))
                    .collect()
            });
            return Self {
                block,
                bounds,
                pixels,
                coverage,
            };
        }
        let w = (x1 - x0) as usize;
        let sw = (bounds[2] - bounds[0]) as usize;
        let sh = (bounds[3] - bounds[1]) as usize;
        // The canvas pixels of shrunk row `v` or column `u`, in the area.
        let span = |start: i32, lo: i32, hi: i32| (start * b).max(lo)..((start + 1) * b).min(hi);
        let mut shrunk = vec![Color32::TRANSPARENT; sw * sh];
        let mut shrunk_coverage = coverage.map(|_| vec![0u8; sw * sh]);
        let rows: Vec<(&mut [Color32], Option<&mut [u8]>)> = match shrunk_coverage.as_mut() {
            Some(c) => shrunk
                .chunks_mut(sw)
                .zip(c.chunks_mut(sw).map(Some))
                .collect(),
            None => shrunk.chunks_mut(sw).map(|r| (r, None)).collect(),
        };
        rows.into_par_iter()
            .enumerate()
            .for_each(|(row, (out, mut cov_out))| {
                let ys = span(bounds[1] + row as i32, y0, y1);
                for (col, px) in out.iter_mut().enumerate() {
                    let xs = span(bounds[0] + col as i32, x0, x1);
                    let mut sum = [0u32; 4];
                    let mut cov = 0u32;
                    for y in ys.clone() {
                        let base = (y - y0) as usize * w;
                        for x in xs.clone() {
                            let c = pixels[base + (x - x0) as usize].to_array();
                            for (s, v) in sum.iter_mut().zip(c) {
                                *s += v as u32;
                            }
                            if let Some(mask) = coverage {
                                cov += mask.value(x, y) as u32;
                            }
                        }
                    }
                    let n = (ys.len() * xs.len()).max(1) as u32;
                    let [r, g, bl, a] = sum.map(|v| ((v + n / 2) / n) as u8);
                    *px = Color32::from_rgba_premultiplied(r, g, bl, a);
                    if let Some(cov_out) = cov_out.as_deref_mut() {
                        cov_out[col] = ((cov + n / 2) / n) as u8;
                    }
                }
            });
        Self {
            block,
            bounds,
            pixels: shrunk,
            coverage: shrunk_coverage,
        }
    }
}

#[cfg(test)]
impl FilterSession {
    /// As if the last run had been slow.
    pub(crate) fn set_slow(&mut self) {
        self.slow = true;
    }
}

/// A run slower than this waits for the slider to be let go.
const LIVE_BUDGET: std::time::Duration = std::time::Duration::from_millis(80);

#[derive(Default)]
pub struct FilterState {
    pub session: Option<FilterSession>,
    /// The adjustment layer whose settings are open.
    pub editing: Option<LayerId>,
    /// The fill layer whose settings are open.
    pub fill_editing: Option<LayerId>,
    /// The layer whose border settings are open.
    pub border_editing: Option<LayerId>,
    /// One of its settings is being dragged.
    pub adjusting: bool,
}

impl FilterState {
    /// Whether the screen shows a quick preview (a setting is dragged).
    pub(crate) fn live(&self) -> bool {
        self.adjusting || self.session.as_ref().is_some_and(|s| s.preview.is_some())
    }

    /// The previewed layer's shrunk pixels for tile `(tx, ty)` at `block`,
    /// with the layer's index, if the preview has them.
    pub(crate) fn preview_pixels(
        &self,
        canvas: &crate::canvas::Canvas,
        tx: usize,
        ty: usize,
        block: usize,
    ) -> Option<(usize, &[Color32])> {
        let session = self.session.as_ref()?;
        let preview = session.preview.as_ref()?;
        if preview.view.block != block {
            return None;
        }
        let pixels = preview.tiles.get(&(tx as i32, ty as i32))?;
        Some((canvas.layer_index_of(session.layer)?, pixels))
    }
}

impl PainterApp {
    /// Start `filter` on the active layer: at once when it has no settings,
    /// else as a previewed session for the dialog.
    pub(crate) fn filter_open(&mut self, filter: Filter) {
        self.filter_cancel();
        let layer = self.canvas.active_layer_idx;
        let Some(l) = self.canvas.layers.get(layer) else {
            return;
        };
        if l.locked || l.kind == LayerKind::Group {
            return;
        }
        let layer_id = l.id;
        self.release_canvas();
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let filter = filter.fitted(w as usize, h as usize);
        // The selection's area plus what a blur reads around it, or the
        // whole canvas.
        // (On a moved layer: the selection carried onto its pixels.)
        let selection = self.layer_selection();
        let (bounds, coverage) = match selection.get_bounds() {
            Some(b) if selection.has_selection() => {
                let [x0, y0, x1, y1] = self.pixel_bounds(b);
                let bounds = [
                    (x0 - MAX_REACH).max(0),
                    (y0 - MAX_REACH).max(0),
                    (x1 + MAX_REACH).min(w),
                    (y1 + MAX_REACH).min(h),
                ];
                let sel = &selection;
                let mask =
                    SelectionMask::rasterize(bounds, |y, x0, out| sel.row_coverage(y, x0, out));
                (bounds, Some(mask))
            }
            _ => ([0, 0, w, h], None),
        };
        let pool = Arc::clone(&self.workspace.pool);
        let original = Arc::new(pool.install(|| self.canvas.capture_region(layer, bounds)));
        self.workspace.filter.session = Some(FilterSession {
            filter,
            layer: layer_id,
            original,
            coverage: coverage.map(Arc::new),
            running: false,
            dirty: true,
            slow: false,
            exact: false,
            source: None,
            preview: None,
        });
        if !filter.has_settings() {
            self.filter_commit();
        }
    }

    /// Run the filter again if its settings changed, once a frame (so only
    /// the latest value of a dragged slider). `dragging` is what's on
    /// screen while the pointer is down: a slow filter then previews there
    /// instead, and runs exactly once the pointer is up.
    pub(crate) fn filter_update(&mut self, dragging: Option<&PreviewView>) {
        if dragging.is_none() {
            self.workspace.filter.adjusting = false;
        }
        let Some(session) = self.workspace.filter.session.as_ref() else {
            return;
        };
        match dragging {
            Some(view) if session.slow => {
                let moved = session.preview.as_ref().is_some_and(|p| p.view != *view);
                if session.dirty || moved {
                    self.filter_preview(view);
                }
            }
            _ => {
                // (One run at a time: the next goes once it's back.)
                if (session.dirty || !session.exact) && !session.running {
                    self.filter_run();
                }
            }
        }
    }

    /// Show the filter's effect on the part of the layer on screen, from
    /// the shrunk source (see the module docs). The layer isn't changed.
    fn filter_preview(&mut self, view: &PreviewView) {
        let Some(layer) = self.filter_layer() else {
            return;
        };
        let pool = Arc::clone(&self.workspace.pool);
        let canvas = &self.canvas;
        let alpha_lock = canvas.layers[layer].alpha_locked;
        let (ts, cw, ch) = (canvas.tile_size(), canvas.width(), canvas.height());
        let Some(session) = self.workspace.filter.session.as_mut() else {
            return;
        };
        let block = view.block;
        let preview_tiles = pool.install(|| {
            if session.source.as_ref().is_none_or(|s| s.block != block) {
                session.source = Some(ShrunkSource::new(
                    &session.original,
                    session.coverage.as_deref(),
                    block,
                    ts,
                ));
            }
            let Some(source) = session.source.as_ref() else {
                return FxHashMap::default();
            };
            let shown: Vec<((i32, i32), &[Color32])> = session
                .original
                .original_tiles()
                .filter(|&((tx, ty), _)| view.contains(tx as usize, ty as usize))
                .collect();
            if shown.is_empty() {
                return FxHashMap::default();
            }
            // The shrunk area those tiles cover, plus what the filter reads
            // around it.
            let (b, tsb) = (block as i32, (ts / block) as i32);
            let margin = (session.filter.reach() + b - 1) / b + 1;
            let (mut u0, mut v0, mut u1, mut v1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &((tx, ty), _) in &shown {
                (u0, v0) = (u0.min(tx * tsb), v0.min(ty * tsb));
                (u1, v1) = (u1.max((tx + 1) * tsb), v1.max((ty + 1) * tsb));
            }
            let [su0, sv0, su1, sv1] = source.bounds;
            let (u0, v0) = ((u0 - margin).max(su0), (v0 - margin).max(sv0));
            let (u1, v1) = ((u1 + margin).min(su1), (v1 + margin).min(sv1));
            if u1 <= u0 || v1 <= v0 {
                return FxHashMap::default();
            }
            let (w, h) = ((u1 - u0) as usize, (v1 - v0) as usize);
            let sw = (su1 - su0) as usize;
            let mut src = Vec::with_capacity(w * h);
            for v in v0..v1 {
                let row = (v - sv0) as usize * sw + (u0 - su0) as usize;
                src.extend_from_slice(&source.pixels[row..row + w]);
            }
            let out = session.filter.shrunk(block).apply(&src, w, h, (u0, v0));
            let at = |u: i32, v: i32| -> Option<usize> {
                ((u0..u1).contains(&u) && (v0..v1).contains(&v))
                    .then(|| (v - v0) as usize * w + (u - u0) as usize)
            };
            let in_source = |u: i32, v: i32| -> Option<usize> {
                ((su0..su1).contains(&u) && (sv0..sv1).contains(&v))
                    .then(|| (v - sv0) as usize * sw + (u - su0) as usize)
            };
            shown
                .into_par_iter()
                .map(|((tx, ty), original)| {
                    let tw = ts.min(cw.saturating_sub(tx as usize * ts));
                    let th = ts.min(ch.saturating_sub(ty as usize * ts));
                    let mut pixels = shrink_tile(original, ts, tw, th, block);
                    let row_len = tw.div_ceil(block);
                    for (i, px) in pixels.iter_mut().enumerate() {
                        let (u, v) = (
                            tx * tsb + (i % row_len) as i32,
                            ty * tsb + (i / row_len) as i32,
                        );
                        let (Some(o), Some(k)) = (at(u, v), in_source(u, v)) else {
                            continue;
                        };
                        let old = *px;
                        let mut new = out[o];
                        if let Some(cov) = source.coverage.as_ref() {
                            new = crate::canvas::storage::mix(old, new, cov[k]);
                        }
                        if alpha_lock {
                            new = crate::canvas::blend::with_alpha_of(new, old.a());
                        }
                        *px = new;
                    }
                    ((tx, ty), pixels)
                })
                .collect()
        });
        session.dirty = false;
        session.exact = false;
        let keys: Vec<(i32, i32)> = preview_tiles.keys().copied().collect();
        session.preview = Some(FilterPreview {
            view: view.clone(),
            tiles: preview_tiles,
        });
        for (tx, ty) in keys {
            self.mark_layer_tile_dirty(tx as usize, ty as usize);
        }
    }

    /// The session's layer index, or `None` (and the session dropped) when
    /// that layer is gone.
    fn filter_layer(&mut self) -> Option<usize> {
        let id = self.workspace.filter.session.as_ref()?.layer;
        let idx = self.canvas.layers.iter().position(|l| l.id == id);
        if idx.is_none() {
            self.workspace.filter.session = None;
        }
        idx
    }

    /// Run the filter on the layer, on the stroke worker (a big blur takes
    /// a while): the frames go on meanwhile, the canvas waiting for it.
    fn filter_run(&mut self) {
        let Some(layer) = self.filter_layer() else {
            return;
        };
        let Some(session) = self.workspace.filter.session.as_mut() else {
            return;
        };
        session.running = true;
        session.dirty = false;
        let id = session.layer;
        let (filter, original) = (session.filter, Arc::clone(&session.original));
        let coverage = session.coverage.clone();
        let pool = Arc::clone(&self.workspace.pool);
        let canvas = Arc::clone(&self.canvas);
        self.run_on_worker(&format!("{}…", filter.name()), move || {
            let [x0, y0, x1, y1] = original.bounds;
            let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
            let started = std::time::Instant::now();
            pool.install(|| {
                // A deeper document and a colour adjustment: at full depth.
                let deep = filter.adjust_linear([0.5, 0.5, 0.5, 1.0]).is_some()
                    && canvas.map_region_deep(layer, &original, coverage.as_deref(), |px| {
                        filter.adjust_linear(px).unwrap_or(px)
                    });
                if !deep {
                    let src = original.pixels(canvas.tile_size());
                    let out = filter.apply(&src, w, h, (x0, y0));
                    canvas.replace_region(layer, &original, &out, coverage.as_deref());
                }
            });
            // The worker lets go of the canvas before it says it's done.
            drop(canvas);
            let slow = started.elapsed() > LIVE_BUDGET;
            Box::new(move |app: &mut PainterApp| {
                if let Some(session) = app.workspace.filter.session.as_mut()
                    && session.layer == id
                {
                    session.running = false;
                    // (Unless a setting changed meanwhile: then it runs
                    // again.)
                    session.exact = !session.dirty;
                    session.preview = None;
                    session.slow = slow;
                }
                app.mark_tiles_in_bounds_dirty(egui::Rect::from_min_max(
                    egui::pos2(x0 as f32, y0 as f32),
                    egui::pos2(x1 as f32, y1 as f32),
                ));
                app.layer_state.thumbnails_dirty = true;
            })
        });
    }

    /// Keep the filter as one undo step. The dialog closes at once; the
    /// filter finishes on the stroke worker.
    pub(crate) fn filter_commit(&mut self) {
        let needs_run = self
            .workspace
            .filter
            .session
            .as_ref()
            .is_some_and(|s| s.dirty || (!s.exact && !s.running));
        if needs_run {
            self.filter_run();
        }
        let Some(layer) = self.filter_layer() else {
            return;
        };
        let Some(session) = self.workspace.filter.session.take() else {
            return;
        };
        let pool = Arc::clone(&self.workspace.pool);
        let canvas = Arc::clone(&self.canvas);
        let name = session.filter.name();
        // After the run: what the layer holds then.
        self.run_on_worker(&format!("{name}…"), move || {
            let tiles = pool.install(|| canvas.region_snapshots(layer, &session.original));
            drop(canvas);
            Box::new(move |app: &mut PainterApp| {
                if tiles.is_empty() {
                    return;
                }
                app.layer_state.history.label_next(name);
                app.push_undo(UndoAction {
                    tiles,
                    selection: None,
                    transform: None,
                    layer_action: None,
                });
            })
        });
    }

    /// Put the layer back as it was (after any run still going).
    pub(crate) fn filter_cancel(&mut self) {
        let Some(session) = self.workspace.filter.session.take() else {
            return;
        };
        let canvas = Arc::clone(&self.canvas);
        self.run_on_worker("Cancelling…", move || {
            canvas.restore_region(&session.original);
            drop(canvas);
            let [x0, y0, x1, y1] = session.original.bounds;
            Box::new(move |app: &mut PainterApp| {
                app.mark_tiles_in_bounds_dirty(egui::Rect::from_min_max(
                    egui::pos2(x0 as f32, y0 as f32),
                    egui::pos2(x1 as f32, y1 as f32),
                ));
                app.layer_state.thumbnails_dirty = true;
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::filters::Filter;
    use crate::selection::{SelectionMode, SelectionShape};
    use eframe::egui::{Color32, Vec2};

    /// A 128×64 canvas whose paint layer is solid red.
    fn app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for tx in 0..2 {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, 0, vec![Color32::RED; 64 * 64]);
        }
        app.selection_manager.canvas_size = [128, 64];
        app
    }

    fn pixel(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
        let tile = app
            .canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .unwrap_or_default();
        tile.get(((y % 64) * 64 + x % 64) as usize)
            .copied()
            .unwrap_or_default()
    }

    #[test]
    fn a_filter_without_settings_applies_as_one_undo_step() {
        let mut app = app();
        app.filter_open(Filter::Invert);
        assert!(app.workspace.filter.session.is_none());
        assert_eq!(pixel(&app, 10, 10), Color32::from_rgb(0, 255, 255));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(pixel(&app, 10, 10), Color32::RED);
    }

    #[test]
    fn the_preview_shows_and_cancel_puts_the_layer_back() {
        let mut app = app();
        app.filter_open(Filter::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        app.filter_update(None);
        assert_eq!(
            pixel(&app, 10, 10),
            Color32::from_rgb(0, 255, 0),
            "previewed"
        );
        app.filter_cancel();
        assert_eq!(pixel(&app, 10, 10), Color32::RED);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    #[test]
    fn changed_settings_are_what_ok_keeps() {
        let mut app = app();
        app.filter_open(Filter::HueSaturation {
            hue: 120.0,
            saturation: 0.0,
            lightness: 0.0,
        });
        app.filter_update(None);
        let session = app.workspace.filter.session.as_mut().unwrap();
        session.filter = Filter::HueSaturation {
            hue: -120.0,
            saturation: 0.0,
            lightness: 0.0,
        };
        session.dirty = true;
        app.filter_commit();
        assert_eq!(pixel(&app, 10, 10), Color32::from_rgb(0, 0, 255));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
    }

    #[test]
    fn only_the_selection_changes() {
        let mut app = app();
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(32.0, 64.0),
            },
            SelectionMode::Replace,
        );
        app.filter_open(Filter::Invert);
        assert_eq!(pixel(&app, 10, 10), Color32::from_rgb(0, 255, 255));
        assert_eq!(pixel(&app, 100, 10), Color32::RED);
    }

    #[test]
    fn a_locked_layer_is_left_alone() {
        let mut app = app();
        app.canvas_mut().layers[1].locked = true;
        app.filter_open(Filter::Invert);
        assert_eq!(pixel(&app, 10, 10), Color32::RED);
    }
}
