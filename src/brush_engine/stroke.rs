//! A stroke in progress: spacing and pressure between input samples,
//! and the per-tile buffers dabs accumulate into before being resolved
//! onto the layer.

use crate::brush_engine::brush::{Brush, BrushType, RibbonSeg, Target};
use crate::brush_engine::brush_options::{PixelBrushShape, TipOrder};
use crate::brush_engine::dynamics::{
    DabVar, FAST_SPEED, PenBarrel, PenTilt, compose, direction, tip_orientation,
};
use crate::brush_engine::stabilizer::Stabilizer;
use crate::brush_engine::symmetry::{Copy2, Symmetry};
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use crate::canvas::storage::DeepTile;
use crate::selection::SelectionManager;
use eframe::egui::Color32;
use eframe::egui::Vec2;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rayon::ThreadPool;
use rustc_hash::FxHashMap;
use std::collections::HashSet;
use std::sync::Mutex;

/// Per-tile state for indirect painting (a temporary stroke device):
/// dabs only accumulate `coverage`, and the tile's pixels are re-resolved from
/// `original` (the tile as it was when the stroke first touched it) plus the
/// coverage. Pixels are never re-quantized to 8 bits between dabs.
#[derive(Clone)]
pub(crate) struct StrokeBuffer {
    pub original: Vec<Color32>,
    /// In a deeper document, the same pixels at full depth.
    pub original_deep: Option<crate::canvas::storage::DeepTile>,
    /// Impasto: the tile's heights before the stroke (`None` when it lays
    /// none down).
    pub heights: Option<Vec<u16>>,
    pub coverage: Vec<f32>,
    /// Selection coverage for this tile (0..=1 per pixel), computed on first
    /// use; the selection can't change during a stroke.
    pub selection: Option<Vec<f32>>,
    /// Pixels changed since the display last collected them, tile-local
    /// `[x0, y0, x1, y1)`; `None` if nothing changed.
    pub damage: Option<[usize; 4]>,
    /// Coverage of the stroke's redrawable tail (an end taper to come), in
    /// two segments (older, newer), combined with `coverage` when
    /// resolving; `None` where a segment has nothing here.
    pub tail: [Option<Vec<f32>>; 2],
    /// Where each tail segment has coverage, tile-local `[x0, y0, x1, y1)`.
    pub tail_rect: [Option<[usize; 4]>; 2],
    /// For strokes whose dabs differ in colour: the colour laid down so far
    /// (premultiplied by coverage, in the document's blend space), and each
    /// tail segment's. `None` for single-colour strokes.
    pub colors: Option<Vec<[f32; 3]>>,
    pub tail_colors: [Option<Vec<[f32; 3]>>; 2],
    /// A dual brush's mask (its second tip's coverage), and where it grew
    /// since the pixels were last resolved with it.
    pub mask: Option<Vec<f32>>,
    pub mask_dirty: Option<[usize; 4]>,
}

/// Tiles touched by the current stroke.
#[derive(Default)]
pub struct StrokeTiles {
    /// Tiles painted since the last redraw; drained every frame so redraw
    /// cost tracks new dabs, not the whole stroke's footprint.
    pub dirty: HashSet<(usize, usize)>,
    /// One buffer per tile this stroke has touched (also marks the tiles
    /// already snapshotted for undo).
    pub(crate) buffers: FxHashMap<(usize, usize), Mutex<StrokeBuffer>>,
    /// Tiles each tail segment has touched.
    pub(crate) tail_tiles: [HashSet<(usize, usize)>; 2],
    /// Which tail segment is the newer (painted over the other).
    pub(crate) tail_newer: usize,
    /// Tiles a dual brush's mask grew in since they were last resolved.
    pub(crate) mask_tiles: HashSet<(usize, usize)>,
    /// Where the stroke's grain sits, for a texture that isn't pinned to
    /// the canvas (set by the first sample).
    pub(crate) grain: crate::brush_engine::texture::StrokeGrain,
    /// Wash mode: the running average of the dabs' opacity so far (`None`
    /// before the first dab, which starts it).
    pub(crate) wash_average: Option<f32>,
    /// Where [`Self::rewind`] goes back to, while one is kept.
    checkpoint: Option<Box<Checkpoint>>,
    /// When set, the stroke's dabs are handed back here (placed, varied,
    /// with their strength) rather than painted: a mixing brush lays each
    /// down itself.
    pub(crate) collect: Option<Vec<CollectedDab>>,
    /// A soaking bristle brush's hairs' colours, taken where the stroke
    /// started (`None` before its first dab, or where the layer was clear).
    pub(crate) hair_colors: Option<Vec<Option<[f32; 3]>>>,
}

/// A dab as the brush would paint it, for a stroke that lays its dabs down
/// itself (see [`StrokeTiles::collect`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CollectedDab {
    pub dab: crate::brush_engine::dab::PlacedDab,
    /// Its whole strength: the brush's opacity, flow and colour alpha as
    /// they were (pressure on them too), times the dab's own.
    pub strength: f32,
}

/// A tile's stroke buffer, pixels (and deep ones) and impasto heights,
/// saved.
type SavedTile = (
    StrokeBuffer,
    (Vec<Color32>, Option<DeepTile>),
    Option<Vec<u16>>,
);

/// The impasto heights of the layer `canvas` paints on, if it has some.
fn layer_heights(canvas: &Canvas) -> Option<&crate::canvas::impasto::HeightMap> {
    canvas
        .layers
        .get(canvas.active_layer_idx)
        .and_then(|l| l.height.as_deref())
}

/// The stroke's tiles as they were at a checkpoint: each tile painted
/// since, saved the first time it was about to be (`None`: it had no
/// buffer yet), and the rest of the stroke's tile state.
struct Checkpoint {
    saved: FxHashMap<(usize, usize), Option<SavedTile>>,
    tail_tiles: [HashSet<(usize, usize)>; 2],
    tail_newer: usize,
    mask_tiles: HashSet<(usize, usize)>,
    grain: crate::brush_engine::texture::StrokeGrain,
    wash_average: Option<f32>,
}

impl StrokeTiles {
    /// Take the stroke back off: every tile it touched gets its pixels from
    /// before the stroke again, and its buffer starts over (keeping the
    /// tile's undo snapshot, already taken), ready to paint the stroke anew.
    pub(crate) fn restart(&mut self, canvas: &Canvas) {
        let tile_size = canvas.tile_size();
        let heights = layer_heights(canvas);
        for (&key, buffer) in &self.buffers {
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let (Some(map), Some(before)) = (heights, &buffer.heights) {
                map.set_tile((key.0 as i32, key.1 as i32), Some(before.clone()));
            }
            if let Some(tile) = canvas.lock_tile(key.0, key.1) {
                let mut tile = tile.lock().unwrap_or_else(|e| e.into_inner());
                if tile.data().is_some() {
                    tile.set_both(Some(buffer.original.clone()), buffer.original_deep.clone());
                    tile.is_empty = buffer.original.iter().all(|&p| p == Color32::TRANSPARENT);
                }
            }
            buffer.coverage.fill(0.0);
            buffer.tail = [None, None];
            buffer.tail_rect = [None, None];
            buffer.colors = None;
            buffer.tail_colors = [None, None];
            buffer.mask = None;
            buffer.mask_dirty = None;
            buffer.damage = Some([0, 0, tile_size, tile_size]);
            self.dirty.insert(key);
        }
        self.tail_tiles = Default::default();
        self.tail_newer = 0;
        self.mask_tiles.clear();
        self.wash_average = None;
        self.checkpoint = None;
    }

    /// Tiles that paint nothing: the stroke's dabs are handed back (see
    /// [`Self::collect`]).
    pub(crate) fn collecting() -> Self {
        Self {
            collect: Some(Vec::new()),
            ..Default::default()
        }
    }

    /// Remember how the stroke's tiles are now, to [`Self::rewind`] to
    /// after painting on (tiles are saved as they're first painted since,
    /// so this costs little until then).
    pub(crate) fn checkpoint(&mut self, canvas: &Canvas) {
        self.checkpoint = Some(Box::new(Checkpoint {
            saved: FxHashMap::default(),
            tail_tiles: self.tail_tiles.clone(),
            tail_newer: self.tail_newer,
            mask_tiles: self.mask_tiles.clone(),
            grain: self.grain,
            wash_average: self.wash_average,
        }));
        // The tail and the mask are redrawn without new dabs there: save
        // their tiles now.
        let keys: Vec<_> = (self.tail_tiles.iter().flatten())
            .chain(&self.mask_tiles)
            .copied()
            .collect();
        for key in keys {
            self.save_for_checkpoint(key, canvas);
        }
    }

    /// Before tile `key` is painted: save it for the checkpoint, the first
    /// time since it was taken.
    pub(crate) fn save_for_checkpoint(&mut self, key: (usize, usize), canvas: &Canvas) {
        let Some(checkpoint) = self.checkpoint.as_mut() else {
            return;
        };
        if checkpoint.saved.contains_key(&key) {
            return;
        }
        let saved = self.buffers.get(&key).map(|buffer| {
            let buffer = buffer.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let pixels = (canvas.lock_tile(key.0, key.1))
                .map(|t| {
                    let t = t.lock().unwrap_or_else(|e| e.into_inner());
                    (t.data().cloned().unwrap_or_default(), t.deep().cloned())
                })
                .unwrap_or_default();
            let heights = layer_heights(canvas).and_then(|m| m.tile((key.0 as i32, key.1 as i32)));
            (buffer, pixels, heights)
        });
        checkpoint.saved.insert(key, saved);
    }

    /// Take back everything painted since the checkpoint (which goes): the
    /// tiles get their pixels and buffers as they were then (one first
    /// touched since starts over, keeping its undo snapshot).
    /// Whether a checkpoint is kept (see [`Self::rewind`]).
    pub(crate) fn holds_checkpoint(&self) -> bool {
        self.checkpoint.is_some()
    }

    pub(crate) fn rewind(&mut self, canvas: &Canvas) {
        let Some(checkpoint) = self.checkpoint.take() else {
            return;
        };
        let tile_size = canvas.tile_size();
        for (key, saved) in checkpoint.saved {
            let Some(buffer) = self.buffers.get(&key) else {
                continue;
            };
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            let heights = layer_heights(canvas);
            let pixels = match saved {
                Some((was, pixels, h)) => {
                    *buffer = was;
                    if let Some(map) = heights {
                        map.set_tile((key.0 as i32, key.1 as i32), h);
                    }
                    pixels
                }
                None => {
                    if let (Some(map), Some(before)) = (heights, &buffer.heights) {
                        map.set_tile((key.0 as i32, key.1 as i32), Some(before.clone()));
                    }
                    buffer.coverage.fill(0.0);
                    buffer.tail = [None, None];
                    buffer.tail_rect = [None, None];
                    buffer.colors = None;
                    buffer.tail_colors = [None, None];
                    buffer.mask = None;
                    buffer.mask_dirty = None;
                    (buffer.original.clone(), buffer.original_deep.clone())
                }
            };
            let (pixels, deep) = pixels;
            if let Some(tile) = canvas.lock_tile(key.0, key.1) {
                let mut tile = tile.lock().unwrap_or_else(|e| e.into_inner());
                if tile.data().is_some_and(|data| data.len() == pixels.len()) {
                    tile.is_empty = pixels.iter().all(|&p| p == Color32::TRANSPARENT);
                    tile.set_both(Some(pixels), deep);
                }
            }
            buffer.damage = Some([0, 0, tile_size, tile_size]);
            self.dirty.insert(key);
        }
        self.tail_tiles = checkpoint.tail_tiles;
        self.tail_newer = checkpoint.tail_newer;
        self.mask_tiles = checkpoint.mask_tiles;
        self.grain = checkpoint.grain;
        self.wash_average = checkpoint.wash_average;
    }
}

/// Shared drawing dependencies for adding points to a stroke.
pub struct StrokeContext<'a> {
    pool: &'a ThreadPool,
    canvas: &'a Canvas,
    selection: Option<&'a SelectionManager>,
    undo_action: &'a mut UndoAction,
    stroke_tiles: &'a mut StrokeTiles,
    /// Mirror painting: the settings and their precomputed copy maps.
    symmetry: Option<(&'a Symmetry, &'a [Copy2])>,
    /// Wrap-around: dabs past an edge also paint at the other.
    wrap: bool,
}

impl<'a> StrokeContext<'a> {
    pub fn new(
        pool: &'a ThreadPool,
        canvas: &'a Canvas,
        selection: Option<&'a SelectionManager>,
        undo_action: &'a mut UndoAction,
        stroke_tiles: &'a mut StrokeTiles,
    ) -> Self {
        Self {
            pool,
            canvas,
            selection,
            undo_action,
            stroke_tiles,
            symmetry: None,
            wrap: false,
        }
    }

    /// Wrap-around painting: a dab reaching past an edge of the canvas is
    /// painted again the canvas's width (or height) the other way.
    pub fn with_wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /// With wrap-around: each of `centers` put on the canvas (whole canvas
    /// sizes away), plus a copy across each edge it comes within `reach`
    /// of; the source index of each (for its variation) and its mirror map.
    fn wrapped(
        &self,
        centers: &[Vec2],
        orients: Option<&[[f32; 4]]>,
        reach: f32,
    ) -> (Vec<Vec2>, Vec<usize>, Option<Vec<[f32; 4]>>) {
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        let mut out = Vec::with_capacity(centers.len());
        let mut sources = Vec::with_capacity(centers.len());
        let mut out_orients = orients.map(|_| Vec::with_capacity(centers.len()));
        for (i, c) in centers.iter().enumerate() {
            let c = Vec2::new(c.x.rem_euclid(w), c.y.rem_euclid(h));
            for dy in [0.0, -h, h] {
                for dx in [0.0, -w, w] {
                    let p = c + Vec2::new(dx, dy);
                    let inside = p.x > -reach && p.x < w + reach && p.y > -reach && p.y < h + reach;
                    if (dx, dy) == (0.0, 0.0) || inside {
                        out.push(p);
                        sources.push(i);
                        if let (Some(o), Some(all)) = (out_orients.as_mut(), orients) {
                            o.push(all[i]);
                        }
                    }
                }
            }
        }
        (out, sources, out_orients)
    }

    /// Repeat every dab with `symmetry` (whose copy maps are `copies`).
    pub fn with_symmetry(mut self, symmetry: &'a Symmetry, copies: &'a [Copy2]) -> Self {
        if symmetry.is_active() && !copies.is_empty() {
            self.symmetry = Some((symmetry, copies));
        }
        self
    }

    /// Dabs varied by the brush's dynamics (`vars[i]` for `centers[i]`).
    fn dabs_varied(
        &mut self,
        brush: &mut Brush,
        centers: &[Vec2],
        vars: &[DabVar],
        target: Target,
    ) {
        if self.wrap {
            // Mirror copies first, then each wrapped round the canvas.
            let (mirrored, orients, sources) = match self.symmetry {
                Some((symmetry, copies)) => {
                    let (all, orients, sources) = symmetry.expand_indexed(copies, centers);
                    (all, Some(orients), sources)
                }
                None => (centers.to_vec(), None, (0..centers.len()).collect()),
            };
            let (all, wrap_sources, orients) =
                self.wrapped(&mirrored, orients.as_deref(), brush.wrap_reach());
            let vars: Vec<DabVar> = wrap_sources.iter().map(|&i| vars[sources[i]]).collect();
            brush.dabs_varied(
                self.pool,
                self.canvas,
                self.selection,
                &all,
                &vars,
                orients.as_deref(),
                target,
                self.undo_action,
                self.stroke_tiles,
            );
            return;
        }
        if let Some((symmetry, copies)) = self.symmetry {
            let (all, orients, sources) = symmetry.expand_indexed(copies, centers);
            let vars: Vec<DabVar> = sources.iter().map(|&i| vars[i]).collect();
            brush.dabs_varied(
                self.pool,
                self.canvas,
                self.selection,
                &all,
                &vars,
                Some(&orients),
                target,
                self.undo_action,
                self.stroke_tiles,
            );
            return;
        }
        brush.dabs_varied(
            self.pool,
            self.canvas,
            self.selection,
            centers,
            vars,
            None,
            target,
            self.undo_action,
            self.stroke_tiles,
        );
    }

    /// Watercolour edges on the whole stroke.
    fn wet_edges(&mut self, brush: &Brush) {
        brush.apply_wet_edges(self.pool, self.canvas, self.selection, self.stroke_tiles);
    }

    /// A ribbon brush's segments, with their mirror copies.
    fn ribbon(&mut self, brush: &Brush, segs: &[RibbonSeg]) {
        let mut all = segs.to_vec();
        if let Some((symmetry, copies)) = self.symmetry {
            for copy in copies {
                let point = |p: Vec2| symmetry.map(copy, p);
                let vector = |v: Vec2| symmetry.map(copy, symmetry.center + v) - symmetry.center;
                all.extend(segs.iter().map(|s| RibbonSeg {
                    p0: point(s.p0),
                    p1: point(s.p1),
                    n0: vector(s.n0),
                    n1: vector(s.n1),
                    ..*s
                }));
            }
        }
        if self.wrap {
            // Each segment moved onto the canvas, and copied across the
            // edges it reaches.
            let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
            let mut wrapped = Vec::with_capacity(all.len());
            for s in &all {
                let mid = (s.p0 + s.p1) * 0.5;
                let home = Vec2::new(mid.x.rem_euclid(w), mid.y.rem_euclid(h)) - mid;
                let reach = (s.p1 - s.p0).length() * 0.5 + s.w0.max(s.w1) + 2.0;
                for dy in [0.0, -h, h] {
                    for dx in [0.0, -w, w] {
                        let shift = home + Vec2::new(dx, dy);
                        let m = mid + shift;
                        let inside =
                            m.x > -reach && m.x < w + reach && m.y > -reach && m.y < h + reach;
                        if (dx, dy) == (0.0, 0.0) || inside {
                            wrapped.push(RibbonSeg {
                                p0: s.p0 + shift,
                                p1: s.p1 + shift,
                                ..*s
                            });
                        }
                    }
                }
            }
            all = wrapped;
        }
        brush.ribbon(
            self.pool,
            self.canvas,
            self.selection,
            &all,
            self.undo_action,
            self.stroke_tiles,
        );
    }

    /// Show the paint a dual brush's mask has uncovered since last time.
    fn resolve_mask(&mut self, brush: &Brush) {
        brush.resolve_mask_changes(self.canvas, self.selection, self.stroke_tiles);
    }

    /// Take the stroke's redrawable tail back off.
    fn clear_tails(&mut self, brush: &Brush) {
        brush.clear_tails(self.canvas, self.stroke_tiles);
    }

    /// Keep tail segment `k` for good.
    fn merge_tail(&mut self, brush: &Brush, k: usize) {
        brush.merge_tail(self.canvas, self.stroke_tiles, k);
    }

    fn dabs(&mut self, brush: &mut Brush, centers: &[Vec2]) {
        if self.wrap {
            let (mirrored, orients) = match self.symmetry {
                Some((symmetry, copies)) => {
                    let (all, orients) = symmetry.expand(copies, centers);
                    (all, Some(orients))
                }
                None => (centers.to_vec(), None),
            };
            let (all, _, orients) = self.wrapped(&mirrored, orients.as_deref(), brush.wrap_reach());
            brush.dabs_oriented(
                self.pool,
                self.canvas,
                self.selection,
                &all,
                orients.as_deref(),
                self.undo_action,
                self.stroke_tiles,
            );
            return;
        }
        if let Some((symmetry, copies)) = self.symmetry {
            let (centers, orients) = symmetry.expand(copies, centers);
            brush.dabs_oriented(
                self.pool,
                self.canvas,
                self.selection,
                &centers,
                Some(&orients),
                self.undo_action,
                self.stroke_tiles,
            );
            return;
        }
        brush.dabs(
            self.pool,
            self.canvas,
            self.selection,
            centers,
            self.undo_action,
            self.stroke_tiles,
        );
    }
}

/// A dab the stroke has placed: where, how far along the stroke, and the
/// direction of travel there (radians, screen counter-clockwise).
#[derive(Clone, Copy, Debug)]
struct Pending {
    pos: Vec2,
    /// How far along the segment from the previous sample (0..=1), for
    /// blending pressure.
    t: f32,
    /// Distance from the stroke's start, canvas pixels.
    along: f32,
    dir: Option<f32>,
}

/// A dab ready to paint with dynamics: its pressure level and variation.
#[derive(Clone, Copy, Debug)]
struct Plan {
    pos: Vec2,
    level: u32,
    var: DabVar,
    along: f32,
}

/// Where a ribbon brush's ribbon has got to.
#[derive(Clone, Copy, Debug)]
struct RibbonPoint {
    pos: Vec2,
    /// Across the stroke (towards the picture's bottom), once known.
    normal: Option<Vec2>,
    half: f32,
    /// Position along the repeating picture, in picture lengths.
    u: f32,
}

/// Tracks per-stroke state like the last position and spacing accumulator.
#[derive(Clone)]
pub struct StrokeState {
    pub last_pos: Option<Vec2>,
    stabilizer: Stabilizer,
    dist_until_next_blit: f32,
    /// Dabs generated by the current sample.
    pending: Vec<Pending>,
    /// Pressure of the previous sample; dabs between samples blend from it.
    last_pressure: Option<f32>,
    /// Dab positions of one pressure run, reused between samples.
    run: Vec<Vec2>,
    /// Distance travelled along the stroke to `last_pos`, canvas pixels.
    travel: f32,
    /// Stroke speed in screen points per second, smoothed; and what it was
    /// at the previous sample, so dabs between samples blend from it.
    speed: f32,
    prev_speed: f32,
    /// Distance moved (screen points) since the speed was last measured.
    speed_distance: f32,
    /// The last raw sample and when it came, for the speed.
    last_sample: Option<(Vec2, f64)>,
    /// Canvas pixels → screen points (the view zoom), for the speed.
    pub view_scale: f32,
    /// Direction of travel, for tips that follow the stroke.
    dir: Option<f32>,
    /// How the pen leans at the current sample (set before each one).
    pub tilt: Option<PenTilt>,
    /// The pen's barrel rotation and wheel at the current sample.
    pub barrel: PenBarrel,
    /// When the lean was last smoothed.
    last_lean_time: Option<f64>,
    /// The lean, smoothed over time (tablet readings are noisy), as a
    /// vector (direction × lean), now and at the previous sample; dabs in
    /// between blend from one to the other.
    lean: Option<Vec2>,
    prev_lean: Option<Vec2>,
    /// The direction of travel smoothed over distance, as a vector: a mouse
    /// moving in whole pixels gives short segments pointing only 0°, 45°,
    /// 90°…, so each segment's own direction would make a nib wobble.
    heading: Vec2,
    /// The way the last segment between samples went (a unit vector), for
    /// the curve through the next.
    last_chord: Option<Vec2>,
    rng: SmallRng,
    /// The last dabs (at least an end taper's length of the pen), drawn
    /// redrawably until the stroke goes on past them or ends.
    tail: Vec<Plan>,
    /// Where in `tail` the newer segment starts, and which buffer slot it
    /// paints into (the older segment has the other).
    newer_from: usize,
    newer_slot: usize,
    /// A follow-the-stroke tip's first dab, held until the direction is known.
    held: Option<Plan>,
    /// The current sample's time, and the previous one's (for the
    /// stabiliser).
    sample_time: Option<f64>,
    prev_sample_time: Option<f64>,
    /// When the airbrush's next dab is due (seconds), if it has one.
    airbrush_due: Option<f64>,
    /// Wet paint: when it last flowed while the pen was down (seconds).
    wet_flowed: Option<f64>,
    /// The next tip of a brush that uses its tips in turn.
    next_tip: usize,
    /// A ribbon brush: the last point of its ribbon.
    ribbon_last: Option<RibbonPoint>,
    /// A sketch brush: the stroke's points so far (the latest ones); a
    /// curve brush's too.
    sketch_points: Vec<Vec2>,
    /// A grid brush: the cells painted so far (each only once).
    grid_cells: rustc_hash::FxHashSet<crate::brush_engine::engines::GridCell>,
    /// A particle brush's swarm, once the stroke has begun.
    swarm: Option<crate::brush_engine::engines::Swarm>,
    /// A dual brush's second tip: where its last dab went, and how far
    /// along the stroke the next one is.
    mask_last: Option<Vec2>,
    mask_until_next: f32,
    /// The stroke's own random value (for "random each stroke"), and
    /// when it started (for "time").
    stroke_random: f32,
    start_time: Option<f64>,
    /// The highest pressure the stroke has reached (for "pressure in").
    max_pressure: f32,
    /// Dabs the inputs have read so far (for "fade").
    dabs: u32,
    /// The enabled perspective assistants (for "perspective").
    pub perspective: Vec<crate::brush_engine::dynamics::PerspectiveGrid>,
    /// The stroke's random grain shift (a share of the pattern) and the
    /// seed for each dab's.
    grain_random: ([f32; 2], u32),
    /// Every dab's variation as painted, for tests.
    #[cfg(test)]
    pub(crate) painted: Vec<DabVar>,
}

impl StrokeState {
    /// Create an empty stroke state.
    pub fn new() -> Self {
        Self::with_seed(rand::random())
    }

    /// With the randomness seeded (the same seed paints the same stroke).
    pub fn with_seed(seed: u64) -> Self {
        Self {
            last_pos: None,
            stabilizer: Stabilizer::default(),
            dist_until_next_blit: 0.0,
            pending: Vec::new(),
            last_pressure: None,
            run: Vec::new(),
            travel: 0.0,
            speed: 0.0,
            prev_speed: 0.0,
            speed_distance: 0.0,
            last_sample: None,
            view_scale: 1.0,
            dir: None,
            tilt: None,
            barrel: PenBarrel::default(),
            last_lean_time: None,
            lean: None,
            prev_lean: None,
            heading: Vec2::ZERO,
            last_chord: None,
            rng: SmallRng::seed_from_u64(seed),
            tail: Vec::new(),
            newer_from: 0,
            newer_slot: 0,
            held: None,
            sample_time: None,
            prev_sample_time: None,
            airbrush_due: None,
            wet_flowed: None,
            next_tip: 0,
            mask_last: None,
            ribbon_last: None,
            grid_cells: Default::default(),
            swarm: None,
            sketch_points: Vec::new(),
            mask_until_next: 0.0,
            stroke_random: SmallRng::seed_from_u64(seed ^ 0x5eed).random(),
            start_time: None,
            max_pressure: 0.0,
            dabs: 0,
            perspective: Vec::new(),
            grain_random: {
                let mut g = SmallRng::seed_from_u64(seed ^ 0x6ea1_0f75);
                ([g.random(), g.random()], g.random())
            },
            #[cfg(test)]
            painted: Vec::new(),
        }
    }

    /// Add a new sample to the stroke, interpolating dabs based on spacing and
    /// jitter. `pressure` (0..=1, `1.0` for a mouse) drives whatever the
    /// brush maps it to: diameter (down to `pressure_min_size`), opacity,
    /// flow. In wash mode pressure-opacity goes to the dabs' strength instead
    /// of the stroke's opacity cap, which must stay fixed for a whole stroke.
    ///
    /// These are temporarily overwritten on `brush` for the duration of this
    /// call and restored before returning, including on panic (via
    /// `catch_unwind`) so a mid-call panic can never leave the brush's
    /// settings corrupted for later strokes.
    pub fn add_point(
        &mut self,
        brush: &mut Brush,
        raw_pos: Vec2,
        pressure: f32,
        context: &mut StrokeContext<'_>,
    ) {
        self.add_sample(brush, raw_pos, pressure, None, context);
    }

    /// [`Self::add_point`] with the sample's time in seconds (for the
    /// stroke's speed).
    pub fn add_sample(
        &mut self,
        brush: &mut Brush,
        raw_pos: Vec2,
        pressure: f32,
        time: Option<f64>,
        context: &mut StrokeContext<'_>,
    ) {
        let o = &brush.brush_options;
        let original = (o.diameter, o.opacity, o.flow);
        let p = pressure.clamp(0.0, 1.0);
        let from = self.last_pressure.unwrap_or(p);
        self.prev_sample_time = self.sample_time;
        self.sample_time = time;
        if self.start_time.is_none() {
            self.start_time = time;
        }
        let dynamic = brush.has_dynamics();
        if dynamic {
            self.update_speed(raw_pos, time);
        }
        // (A tangent normal brush's colour is the lean.)
        if dynamic || brush.brush_type == BrushType::TangentNormal {
            self.update_lean(time);
        }
        if self.last_pos.is_none()
            && brush
                .texture
                .as_ref()
                .is_some_and(|t| t.placement.is_active())
        {
            let (offset, seed) = self.grain_random;
            context.stroke_tiles.grain = crate::brush_engine::texture::StrokeGrain {
                origin: [raw_pos.x, raw_pos.y],
                offset,
                seed,
            };
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Spacing and jitter follow this sample's pressure.
            apply_pressure(brush, original, p);
            let spacing =
                brush.brush_options.spacing_factor(p) * self.spacing_input(brush, raw_pos, p);
            self.add_point_at_pressure(brush, raw_pos, spacing);
            if brush.dual.is_some() {
                self.paint_mask(brush, original.0, context);
            }
            if !self.pending.is_empty()
                && let Some(t) = time
            {
                self.airbrush_due = Some(t + airbrush_interval(brush));
            }
            self.paint_pending(brush, original, from, p, context);
        }));

        let o = &mut brush.brush_options;
        (o.diameter, o.opacity, o.flow) = original;
        self.last_pressure = Some(p);

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    /// Airbrush: while the pen is down, keep adding dabs where it is at the
    /// brush's rate, so paint builds up while it's held still. Call it with
    /// the time now (seconds, the same clock as the samples); it paints the
    /// dabs due since the last sample or the last call.
    /// Wet paint flowing while the pen is down: a step when one is due at
    /// `time` (seconds), `gravity` down (see [`Brush::flow_wet`]).
    pub fn flow_wet(
        &mut self,
        brush: &Brush,
        time: f64,
        gravity: Vec2,
        context: &mut StrokeContext<'_>,
    ) {
        if brush.wet.is_none() {
            return;
        }
        let last = *self.wet_flowed.get_or_insert(time);
        if time - last < crate::canvas::wet::STEP {
            return;
        }
        self.wet_flowed = Some(time);
        brush.flow_wet(
            context.canvas,
            context.stroke_tiles,
            context.undo_action,
            gravity,
        );
    }

    pub fn airbrush(&mut self, brush: &mut Brush, time: f64, context: &mut StrokeContext<'_>) {
        if brush.airbrush_rate <= 0.0 || brush.pixel_perfect {
            return;
        }
        let (Some(pos), Some(p)) = (self.last_pos, self.last_pressure) else {
            return;
        };
        let interval = airbrush_interval(brush);
        let mut due = self.airbrush_due.unwrap_or(time + interval);
        let mut ticks = 0;
        while due <= time && ticks < MAX_AIRBRUSH_TICKS {
            due += interval;
            ticks += 1;
        }
        // A long stall (the worker was busy) doesn't bank dabs for later.
        if due <= time {
            due = time + interval;
        }
        self.airbrush_due = Some(due);
        if ticks == 0 {
            return;
        }
        let o = &brush.brush_options;
        let original = (o.diameter, o.opacity, o.flow);
        let dynamic = brush.has_dynamics();
        if dynamic {
            // Held still: the speed settles to nothing.
            self.prev_speed = self.speed;
            self.speed = 0.0;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_pressure(brush, original, p);
            let count = brush.dynamics.random.dabs_per_step();
            let defer = scatter_deferred(brush);
            for _ in 0..ticks * count {
                let at = if defer { pos } else { self.scatter(brush, pos) };
                self.pending.push(Pending {
                    pos: at,
                    t: 1.0,
                    along: self.travel,
                    dir: self.dir,
                });
            }
            self.paint_pending(brush, original, p, p, context);
        }));
        let o = &mut brush.brush_options;
        (o.diameter, o.opacity, o.flow) = original;
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    /// A sketch brush: join each of `points` (this sample's dabs, with
    /// their inputs) to some earlier points of the stroke nearby with fine
    /// lines.
    fn sketch_lines(
        &mut self,
        brush: &mut Brush,
        points: &[(Vec2, DabVar)],
        context: &mut StrokeContext<'_>,
    ) {
        use crate::brush_engine::sketch::{HISTORY, MAX_LINES};
        let base = brush.sketch;
        let base_r = (brush.brush_options.diameter * 0.5).max(0.25);
        let spacing = base.point_spacing();
        let mut centers = Vec::new();
        let mut vars = Vec::new();
        for &(p, var) in points {
            // This point's density, line width and offset, by its inputs.
            let sketch = crate::brush_engine::sketch::Sketch {
                density: base.density * var.sketch[0],
                thickness: base.thickness * var.sketch[1],
                offset: base.offset * var.sketch[2],
                ..base
            };
            let scale = (sketch.thickness * 0.5).max(0.3) / base_r;
            let step = sketch.step();
            if self
                .sketch_points
                .last()
                .is_some_and(|&last| (p - last).length() < spacing)
            {
                continue;
            }
            let mut lines = 0;
            for &q in self.sketch_points.iter().rev().take(HISTORY) {
                let d = (q - p).length();
                let strength = sketch.strength(d);
                // Points right behind the pen would only draw over its line.
                if d < spacing * 1.5
                    || strength <= 0.0
                    || self.rng.random::<f32>() >= sketch.density
                {
                    continue;
                }
                let (a, b) = sketch.ends(p, q);
                let n = ((b - a).length() / step).ceil().max(1.0) as usize;
                for k in 0..=n {
                    centers.push(a + (b - a) * (k as f32 / n as f32));
                    vars.push(DabVar {
                        scale,
                        strength,
                        ..DabVar::default()
                    });
                }
                lines += 1;
                if lines >= MAX_LINES {
                    break;
                }
            }
            self.sketch_points.push(p);
        }
        if self.sketch_points.len() > 2 * HISTORY {
            let excess = self.sketch_points.len() - HISTORY;
            self.sketch_points.drain(..excess);
        }
        if !centers.is_empty() {
            context.dabs_varied(brush, &centers, &vars, Target::Stroke);
        }
    }

    /// A curve, grid or particle brush: its own lines or shapes for these
    /// dabs (in their place).
    fn engine_dabs(&mut self, brush: &mut Brush, plans: &[Plan], context: &mut StrokeContext<'_>) {
        use crate::brush_engine::engines::{hash01, quadratic};
        let e = brush.engines;
        let base_r = (brush.brush_options.diameter * 0.5).max(0.25);
        let mut centers = Vec::new();
        let mut vars = Vec::new();
        // A line of round dabs `width` wide from `a` to `b`.
        let mut line = |points: &[Vec2], width: f32, strength: f32| {
            for &q in points {
                centers.push(q);
                vars.push(DabVar {
                    scale: (width * 0.5).max(0.3) / base_r,
                    strength,
                    ..DabVar::default()
                });
            }
        };
        let straight = |a: Vec2, b: Vec2, step: f32| {
            let n = ((b - a).length() / step).ceil().clamp(1.0, 4096.0) as usize;
            (0..=n)
                .map(|k| a + (b - a) * (k as f32 / n as f32))
                .collect::<Vec<_>>()
        };
        match brush.brush_type {
            BrushType::Curve => {
                let c = e.curve;
                // Dabs three quarters of a width apart still join smoothly.
                let step = (c.line_width * 0.75).max(0.5);
                for plan in plans {
                    if self
                        .sketch_points
                        .last()
                        .is_some_and(|&last| (plan.pos - last).length() < 4.0)
                    {
                        continue;
                    }
                    self.sketch_points.push(plan.pos);
                    let strength = c.opacity.clamp(0.0, 1.0) * plan.var.strength;
                    if let Some((a, m, b)) = c.curve(&self.sketch_points) {
                        line(&quadratic(a, m, b, step), c.line_width, strength);
                        if c.connection {
                            line(&straight(a, b, step), c.line_width, strength);
                        }
                    }
                }
                if self.sketch_points.len() > 400 {
                    let excess = self.sketch_points.len() - 200;
                    self.sketch_points.drain(..excess);
                }
            }
            BrushType::Grid => {
                let g = e.grid;
                for plan in plans {
                    let division = g.division(plan.var.pressure);
                    for cell in g.cells(plan.pos, base_r * plan.var.scale, division) {
                        // Once a stroke, unless every dab repaints its cells.
                        if !self.grid_cells.insert(cell) && !g.repaint {
                            continue;
                        }
                        let hue = (hash01(cell.0 as u32, cell.1 as u32) * 2.0 - 1.0) * g.hue_jitter;
                        // Across `rx`, up and down `ry`: the radius is the
                        // longer, the tip squashed the other way (dabs
                        // never reach past their radius).
                        let (rx, aspect) = g.shape(cell);
                        let ry = rx * aspect.max(0.01);
                        let radius = rx.max(ry);
                        centers.push(g.center(cell));
                        vars.push(DabVar {
                            scale: radius / base_r,
                            strength: plan.var.strength,
                            tip: plan.var.tip,
                            hsv: [hue, 0.0, 0.0],
                            orient: [radius / rx.max(1e-3), 0.0, 0.0, radius / ry.max(1e-3)],
                            ..DabVar::default()
                        });
                    }
                }
            }
            BrushType::Particle => {
                let p = e.particles;
                let step = (p.line_width * 0.75).max(0.5);
                for plan in plans {
                    if self.swarm.is_none() {
                        let seed = self.rng.random();
                        self.swarm = Some(p.start(plan.pos, base_r, seed));
                    }
                    let Some(swarm) = self.swarm.as_mut() else {
                        continue;
                    };
                    for (a, b) in p.step(swarm, plan.pos) {
                        if p.dots {
                            line(&[b], p.line_width, plan.var.strength);
                        } else {
                            line(&straight(a, b, step), p.line_width, plan.var.strength);
                        }
                    }
                }
            }
            _ => {}
        }
        if !centers.is_empty() {
            context.dabs_varied(brush, &centers, &vars, Target::Stroke);
        }
    }

    /// A ribbon brush: the stretch from the last ribbon point through this
    /// sample's points, the picture repeating along it.
    fn paint_ribbon(
        &mut self,
        brush: &Brush,
        original: (f32, f32, f32),
        from: f32,
        p: f32,
        pending: &[Pending],
        context: &mut StrokeContext<'_>,
    ) {
        let PixelBrushShape::Custom(tip) = &brush.brush_options.pixel_shape else {
            return;
        };
        let base_half = original.0 * 0.5;
        // The picture's length on the canvas at the brush's own size.
        let length = (tip.width as f32 / tip.height.max(1) as f32 * 2.0 * base_half).max(1.0);
        let o = &brush.brush_options;
        let mut segs = Vec::with_capacity(pending.len());
        for d in pending {
            let pressure = from + (p - from) * d.t;
            let size = if o.pressure_size {
                o.pressure_min_size + (1.0 - o.pressure_min_size) * o.pressure_curves.size(pressure)
            } else {
                1.0
            };
            let normal = d.dir.map(|a| Vec2::new(a.sin(), a.cos()));
            let point = RibbonPoint {
                pos: d.pos,
                normal,
                half: (base_half * size).max(0.5),
                u: d.along / length,
            };
            if let Some(last) = self.ribbon_last {
                let n1 = normal.or(last.normal).unwrap_or(Vec2::new(0.0, 1.0));
                let n0 = last.normal.unwrap_or(n1);
                if (point.pos - last.pos).length_sq() > 1e-6 {
                    segs.push(RibbonSeg {
                        p0: last.pos,
                        p1: point.pos,
                        n0,
                        n1,
                        w0: last.half,
                        w1: point.half,
                        u0: last.u,
                        u1: point.u,
                    });
                }
            }
            self.ribbon_last = Some(point);
        }
        if !segs.is_empty() {
            context.ribbon(brush, &segs);
        }
    }

    /// A dual brush: stamp its second tip along the stroke up to where it
    /// is now (spaced by its own size), into the stroke's mask, then show
    /// the paint the mask uncovered. `diameter` is the brush's unpressured
    /// size, which the second tip's follows.
    fn paint_mask(&mut self, brush: &Brush, diameter: f32, context: &mut StrokeContext<'_>) {
        let (Some(dual), Some(to)) = (&brush.dual, self.last_pos) else {
            return;
        };
        let step = dual.step(diameter);
        let mut centers = Vec::new();
        match self.mask_last {
            None => centers.push(to),
            Some(from) => {
                let length = (to - from).length();
                let mut walked = self.mask_until_next;
                while walked <= length {
                    centers.push(from + (to - from) * (walked / length.max(1e-6)));
                    walked += step;
                }
                self.mask_until_next = walked - length;
            }
        }
        if self.mask_last.is_none() {
            self.mask_until_next = step;
        }
        self.mask_last = Some(to);
        if centers.is_empty() {
            return;
        }
        let mut mask_brush = dual.brush(brush, diameter);
        let count = dual.count.clamp(1, 16);
        let mut points = Vec::with_capacity(centers.len() * count as usize);
        let mut vars = Vec::with_capacity(points.capacity());
        for c in centers {
            for _ in 0..count {
                points.push(self.scatter(&mask_brush, c));
                let orient = if dual.random_angle {
                    tip_orientation(self.rng.random::<f32>() * std::f32::consts::TAU, 1.0)
                } else {
                    crate::brush_engine::dynamics::IDENTITY
                };
                vars.push(DabVar {
                    orient,
                    ..DabVar::default()
                });
            }
        }
        context.dabs_varied(&mut mask_brush, &points, &vars, Target::Mask);
        context.resolve_mask(brush);
    }

    /// Paint the dabs queued in `self.pending`, their pressure blending from
    /// `from` to `p` along the segment.
    fn paint_pending(
        &mut self,
        brush: &mut Brush,
        original: (f32, f32, f32),
        from: f32,
        p: f32,
        context: &mut StrokeContext<'_>,
    ) {
        {
            let pending = std::mem::take(&mut self.pending);
            if brush.is_ribbon() {
                self.paint_ribbon(brush, original, from, p, &pending, context);
                self.pending = pending;
                self.pending.clear();
                return;
            }
            if brush.varies_per_dab() {
                let defer = scatter_deferred(brush);
                let plans: Vec<Plan> = pending
                    .iter()
                    .map(|d| {
                        let pressure = from + (p - from) * d.t;
                        let var = self.dab_var(brush, d, pressure);
                        // An input drives the scatter: now it's known.
                        let pos = if defer {
                            self.scatter_by(brush, d.pos, brush.jitter / 100.0 + var.scatter)
                        } else {
                            d.pos
                        };
                        Plan {
                            pos,
                            level: pressure_level(pressure),
                            var,
                            along: d.along,
                        }
                    })
                    .collect();
                if brush.brush_type.replaces_dabs() {
                    apply_pressure(brush, original, p);
                    self.engine_dabs(brush, &plans, context);
                    self.pending = pending;
                    self.pending.clear();
                    return;
                }
                let points: Vec<(Vec2, DabVar)> = if brush.brush_type == BrushType::Sketch {
                    plans.iter().map(|p| (p.pos, p.var)).collect()
                } else {
                    Vec::new()
                };
                self.paint_dynamic(brush, original, plans, context);
                if !points.is_empty() {
                    apply_pressure(brush, original, p);
                    self.sketch_lines(brush, &points, context);
                }
                self.pending = pending;
                self.pending.clear();
                return;
            }
            // Pressure blends along the segment, so a slow pen (or a big
            // pressure change between samples) doesn't paint visible steps.
            // Dabs are painted in runs of equal (rounded) pressure.
            let mut run = std::mem::take(&mut self.run);
            let mut level = None;
            for d in &pending {
                let q = pressure_level(from + (p - from) * d.t);
                if level != Some(q) {
                    if let Some(prev) = level
                        && !run.is_empty()
                    {
                        apply_pressure(brush, original, prev as f32 / PRESSURE_LEVELS);
                        context.dabs(brush, &run);
                        run.clear();
                    }
                    level = Some(q);
                }
                run.push(d.pos);
            }
            if let Some(last) = level
                && !run.is_empty()
            {
                apply_pressure(brush, original, last as f32 / PRESSURE_LEVELS);
                context.dabs(brush, &run);
            }
            run.clear();
            self.run = run;
            self.pending = pending;
            self.pending.clear();
        }
    }

    /// The pen lifted: finish what's still pending (a held first dab, the
    /// end taper). Every stroke should end with this; without dynamics it
    /// does nothing.
    pub fn finish(&mut self, brush: &mut Brush, context: &mut StrokeContext<'_>) {
        let o = &brush.brush_options;
        let original = (o.diameter, o.opacity, o.flow);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(mut held) = self.held.take() {
                // A tap: no direction ever came.
                held.var = self.orient(brush, held.var, None, 1.0);
                self.tail.push(held);
            }
            if self.tail.is_empty() {
                return;
            }
            let taper = brush.dynamics.taper;
            let end = self.travel;
            let mut tail = std::mem::take(&mut self.tail);
            self.newer_from = 0;
            for plan in &mut tail {
                let f = taper.factor(end - plan.along, taper.end);
                if taper.size {
                    plan.var.scale *= f;
                }
                if taper.opacity {
                    plan.var.strength *= f;
                }
            }
            // The tapered end first, while the tail still shows (together
            // they look like the tail), then the tail off: the line is never
            // shown without its end.
            paint_plans(brush, original, &tail, Target::Stroke, context);
            context.clear_tails(brush);
        }));
        let o = &mut brush.brush_options;
        (o.diameter, o.opacity, o.flow) = original;
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
        // Watercolour edges, on the whole stroke once it's down.
        if brush.wet_edge > 0.0 {
            context.wet_edges(brush);
        }
        // Wet paint: the stroke becomes water and pigment, drying from now.
        if brush.wet.is_some() {
            brush.lay_wet(context.canvas, context.stroke_tiles, context.undo_action);
        }
    }

    /// Smooth the pen's lean with this sample's (by time; see [`LEAN_SMOOTHING`]).
    fn update_lean(&mut self, time: Option<f64>) {
        self.prev_lean = self.lean;
        let Some(tilt) = self.tilt else {
            self.lean = None;
            return;
        };
        let raw = Vec2::new(tilt.direction.cos(), -tilt.direction.sin()) * tilt.lean;
        self.lean = Some(match (self.lean, time, self.last_lean_time) {
            (Some(prev), Some(t1), Some(t0)) => {
                let dt = (t1 - t0).max(0.0) as f32;
                let blend = 1.0 - (-dt / LEAN_SMOOTHING).exp();
                prev + (raw - prev) * blend
            }
            _ => raw,
        });
        if time.is_some() {
            self.last_lean_time = time;
        }
    }

    /// The pen's lean at a dab a share `t` of the way from the previous
    /// sample to this one.
    fn tilt_at(&self, t: f32) -> Option<PenTilt> {
        let now = self.lean?;
        let v = match self.prev_lean {
            Some(prev) => prev + (now - prev) * t,
            None => now,
        };
        let lean = v.length().min(1.0);
        Some(PenTilt {
            lean,
            direction: if lean > 1e-6 { (-v.y).atan2(v.x) } else { 0.0 },
        })
    }

    /// Update the smoothed screen speed with this sample.
    ///
    /// Input arrives unevenly: a mouse sends several moves per frame, stamped
    /// microseconds apart, then nothing until the next frame. So distance is
    /// gathered until at least [`SPEED_WINDOW`] has passed before measuring,
    /// and the smoothing goes by time (not per sample), so the speed is the
    /// same however the samples are bunched.
    fn update_speed(&mut self, raw: Vec2, time: Option<f64>) {
        self.prev_speed = self.speed;
        let Some(t1) = time else {
            return;
        };
        let Some((prev, t0)) = self.last_sample else {
            self.last_sample = Some((raw, t1));
            return;
        };
        self.speed_distance += (raw - prev).length() * self.view_scale;
        let dt = (t1 - t0) as f32;
        // Keep the older time: the distance keeps adding up against it.
        self.last_sample = Some((raw, t0));
        if dt < SPEED_WINDOW {
            return;
        }
        let v = self.speed_distance / dt;
        let blend = 1.0 - (-dt / SPEED_SMOOTHING).exp();
        self.speed = if self.speed == 0.0 && self.prev_speed == 0.0 {
            v
        } else {
            self.speed + (v - self.speed) * blend
        };
        self.speed_distance = 0.0;
        self.last_sample = Some((raw, t1));
    }

    /// One dab's variation: start taper, speed, randomness, tip.
    fn dab_var(&mut self, brush: &Brush, dab: &Pending, pressure: f32) -> DabVar {
        let d = &brush.dynamics;
        let hatch = if brush.brush_type == crate::brush_engine::brush::BrushType::Hatching {
            brush.hatching.level(pressure)
        } else {
            1
        };
        let mut v = DabVar {
            tip: self.pick_tip(brush, dab, pressure),
            along: dab.along,
            hatch,
            pressure,
            ..DabVar::default()
        };
        if d.taper.is_active() && d.taper.start > 0.0 {
            let f = d.taper.factor(dab.along, d.taper.start);
            if d.taper.size {
                v.scale *= f;
            }
            if d.taper.opacity {
                v.strength *= f;
            }
        }
        if d.speed.is_active() {
            // Blended along the segment from the previous sample's speed, so
            // a change doesn't step the width between samples.
            let speed = self.prev_speed + (self.speed - self.prev_speed) * dab.t;
            let s = (speed / FAST_SPEED).clamp(0.0, 1.0);
            v.scale *= (1.0 + d.speed.size * s).max(0.05);
            v.strength *= (1.0 + d.speed.opacity * s).max(0.0);
        }
        if d.tilt.is_active()
            && let Some(tilt) = self.tilt_at(dab.t)
        {
            v.scale *= (1.0 + d.tilt.size * tilt.lean).max(0.05);
            v.strength *= (1.0 + d.tilt.opacity * tilt.lean).max(0.0);
        }
        let r = &d.random;
        if r.size > 0.0 {
            v.scale *= 1.0 - r.size.min(1.0) * self.rng.random::<f32>();
        }
        if r.opacity > 0.0 {
            v.strength *= 1.0 - r.opacity.min(1.0) * self.rng.random::<f32>();
        }
        if r.has_color() {
            let mut spread = |amount: f32| (self.rng.random::<f32>() * 2.0 - 1.0) * amount;
            v.hsv = [spread(r.hue), spread(r.saturation), spread(r.value)];
        }
        if brush.brush_options.color_source
            == crate::brush_engine::brush_options::ColorSource::UniformRandom
        {
            v.base = Some(std::array::from_fn(|_| self.rng.random::<f32>()));
        }
        if !brush.inputs.is_empty() {
            let sensors = self.sensors(dab, pressure);
            crate::brush_engine::dynamics::apply_inputs(
                &brush.inputs,
                &brush.input_combine,
                &mut v,
                &sensors,
            );
        }
        if brush.brush_type == BrushType::TangentNormal {
            let n = brush.engines.normal;
            let (lean, direction) = match self.tilt_at(dab.t) {
                Some(tilt) => (tilt.lean, tilt.direction),
                None => (n.mouse_lean(), dab.dir.unwrap_or(0.0)),
            };
            v.base = Some(n.color(lean, direction));
        }
        self.orient(brush, v, dab.dir, dab.t)
    }

    /// The spacing factor the brush's Spacing inputs give at a sample (1
    /// with none), read there rather than per dab (the spacing places the
    /// dabs).
    fn spacing_input(&mut self, brush: &Brush, pos: Vec2, pressure: f32) -> f32 {
        use crate::brush_engine::dynamics::{DabSetting, DabVar};
        if !brush
            .inputs
            .iter()
            .any(|m| m.setting == DabSetting::Spacing)
        {
            return 1.0;
        }
        let at = Pending {
            pos,
            t: 1.0,
            along: self.travel,
            dir: self.dir,
        };
        // A reading, not a dab: the dab count stays.
        let dabs = self.dabs;
        let sensors = self.sensors(&at, pressure);
        self.dabs = dabs;
        let mut v = DabVar::default();
        let spacing: Vec<_> = (brush.inputs.iter())
            .filter(|m| m.setting == DabSetting::Spacing)
            .cloned()
            .collect();
        crate::brush_engine::dynamics::apply_inputs(
            &spacing,
            &brush.input_combine,
            &mut v,
            &sensors,
        );
        v.spacing
    }

    /// Every input a mapping can read, for one dab.
    fn sensors(
        &mut self,
        dab: &Pending,
        pressure: f32,
    ) -> crate::brush_engine::dynamics::SensorValues {
        let turn = |a: f32| a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
        let speed = self.prev_speed + (self.speed - self.prev_speed) * dab.t;
        let tilt = self.tilt_at(dab.t);
        let pressure = pressure.clamp(0.0, 1.0);
        self.max_pressure = self.max_pressure.max(pressure);
        let dabs = self.dabs as f32;
        self.dabs = self.dabs.saturating_add(1);
        crate::brush_engine::dynamics::SensorValues {
            pressure,
            pressure_in: self.max_pressure,
            speed: (speed / FAST_SPEED).clamp(0.0, 1.0),
            tilt: tilt.map_or(0.0, |t| t.lean),
            tilt_direction: tilt.map_or(0.0, |t| turn(t.direction)),
            direction: dab.dir.or(self.dir).map_or(0.0, turn),
            distance: dab.along,
            time: match (self.start_time, self.sample_time) {
                (Some(t0), Some(t)) => (t - t0) as f32,
                _ => 0.0,
            },
            random_dab: self.rng.random(),
            random_stroke: self.stroke_random,
            rotation: self.barrel.rotation.map_or(0.0, turn),
            wheel: self.barrel.wheel.unwrap_or(0.0),
            dabs,
            perspective: crate::brush_engine::dynamics::PerspectiveGrid::sensor(
                &self.perspective,
                dab.pos,
            ),
            // The lean's canvas x (right) and y (down, toward you).
            x_tilt: tilt.map_or(0.5, |t| 0.5 + 0.5 * t.lean * t.direction.cos()),
            y_tilt: tilt.map_or(0.5, |t| 0.5 - 0.5 * t.lean * t.direction.sin()),
        }
    }

    /// Which of the brush's tips a dab uses.
    fn pick_tip(&mut self, brush: &Brush, dab: &Pending, pressure: f32) -> u8 {
        let n = brush.brush_options.tip_count().min(u8::MAX as usize + 1);
        if n <= 1 {
            return 0;
        }
        let share = |x: f32| ((x * n as f32) as usize).min(n - 1);
        let tip = match brush.brush_options.tip_order {
            TipOrder::Sequence => {
                let tip = self.next_tip % n;
                self.next_tip = self.next_tip.wrapping_add(1);
                tip
            }
            TipOrder::Random => self.rng.random_range(0..n),
            TipOrder::Pressure => share(pressure.clamp(0.0, 1.0)),
            TipOrder::Direction => {
                let dir = dab.dir.or(self.dir).unwrap_or(0.0);
                share(dir.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU)
            }
        };
        tip as u8
    }

    /// The tip's turn and squash for a dab going in direction `dir`.
    fn orient(&mut self, brush: &Brush, mut v: DabVar, dir: Option<f32>, t: f32) -> DabVar {
        let tip = &brush.dynamics.tip;
        if tip.is_active() || brush.follows_stroke() || v.turn != 0.0 || v.squash != 1.0 {
            let mut angle = tip.angle.to_radians() + v.turn;
            if brush.follows_stroke() {
                angle += dir.or(self.dir).unwrap_or(0.0);
            }
            if tip.follow_tilt
                && let Some(tilt) = self.tilt_at(t)
            {
                angle += tilt.direction;
            }
            if tip.follow_barrel
                && let Some(rotation) = self.barrel.rotation
            {
                angle += rotation;
            }
            if tip.random_angle > 0.0 {
                angle += (self.rng.random::<f32>() * 2.0 - 1.0) * tip.random_angle.to_radians();
            }
            v.orient = tip_orientation(angle, tip.ratio * v.squash);
            // Mirrored in the tip's own frame: across its length, its width;
            // by its input when it has one (both ways at once), at random
            // otherwise.
            let by_input = brush
                .inputs
                .iter()
                .any(|m| m.setting == crate::brush_engine::dynamics::DabSetting::Mirror);
            let flip = |rng: &mut SmallRng| {
                if by_input {
                    v.mirror >= 0.5
                } else {
                    rng.random::<bool>()
                }
            };
            let (flip_x, flip_y) = (
                tip.random_flip_x && flip(&mut self.rng),
                tip.random_flip_y && flip(&mut self.rng),
            );
            if flip_x {
                v.orient = compose([-1.0, 0.0, 0.0, 1.0], v.orient);
            }
            if flip_y {
                v.orient = compose([1.0, 0.0, 0.0, -1.0], v.orient);
            }
        }
        v
    }

    /// Paint this sample's dabs with dynamics: all of them, or with an end
    /// taper, those now past its length for good and the rest as the
    /// redrawable tail.
    fn paint_dynamic(
        &mut self,
        brush: &mut Brush,
        original: (f32, f32, f32),
        mut plans: Vec<Plan>,
        context: &mut StrokeContext<'_>,
    ) {
        #[cfg(test)]
        self.painted.extend(plans.iter().map(|p| p.var));
        let follow = brush.follows_stroke();
        if follow {
            if self.held.is_some() {
                let Some(dir) = self.dir else {
                    // Still no direction: keep waiting.
                    return;
                };
                // The direction is known now: the first dab faces it.
                if let Some(mut held) = self.held.take() {
                    held.var = self.orient(brush, held.var, Some(dir), 1.0);
                    plans.insert(0, held);
                }
            } else if self.travel == 0.0 && self.tail.is_empty() && plans.len() == 1 {
                // The stroke's very first dab: wait for a direction.
                self.held = plans.pop();
                return;
            }
        }
        let taper = brush.dynamics.taper;
        let end = if taper.is_active() { taper.end } else { 0.0 };
        if end <= 0.0 {
            paint_plans(brush, original, &plans, Target::Stroke, context);
            return;
        }
        // The tail is two segments: new dabs go into the newer one; once it
        // is an end taper long, the older one is far enough behind the pen
        // to keep for good, and merging it redraws nothing. So each dab is
        // painted once while drawing, and the tail is redrawn only when the
        // pen lifts.
        paint_plans(
            brush,
            original,
            &plans,
            Target::Tail(self.newer_slot),
            context,
        );
        self.tail.extend(plans);
        let newer_start = self
            .tail
            .get(self.newer_from)
            .map_or(self.travel, |p| p.along);
        if self.travel - newer_start >= end {
            let older = 1 - self.newer_slot;
            context.merge_tail(brush, older);
            self.tail.drain(..self.newer_from);
            self.newer_slot = older;
            context.stroke_tiles.tail_newer = older;
            self.newer_from = self.tail.len();
        }
    }

    /// Queue the dabs for one sample into `self.pending`, `spacing` times
    /// the brush's spacing apart.
    fn add_point_at_pressure(&mut self, brush: &Brush, raw_pos: Vec2, spacing: f32) {
        if brush.pixel_perfect {
            self.add_point_pixel_perfect(raw_pos);
            return;
        }

        let dt = match (self.prev_sample_time, self.sample_time) {
            (Some(t0), Some(t1)) => Some((t1 - t0) as f32),
            _ => None,
        };
        let mut settings = brush.stabilizer_settings();
        settings.view_scale = self.view_scale;
        let pos = self
            .stabilizer
            .step_timed(&settings, self.last_pos, raw_pos, dt);

        let spacing_dist = if brush.brush_type == crate::brush_engine::brush::BrushType::Bristle {
            // Each hair draws a continuous line.
            brush.bristles.step()
        } else if brush.is_ribbon() {
            // Short segments, so the ribbon bends smoothly.
            (brush.brush_options.diameter * 0.15).clamp(1.0, 6.0)
        } else {
            brush
                .brush_options
                .spacing_px(brush.brush_options.diameter, spacing)
        };
        let spaced_by_tip = !brush.is_ribbon()
            && brush.brush_type != crate::brush_engine::brush::BrushType::Bristle;
        let spacing_dist = spacing_dist.max(0.5); // Avoid infinite loops
        let count = brush.dynamics.random.dabs_per_step();
        let defer = scatter_deferred(brush);

        if let Some(prev) = self.last_pos {
            let delta = pos - prev;
            let chord = delta.length();
            if chord == 0.0 {
                return;
            }
            let unit_step = delta / chord;
            let angle = |v: Vec2| (v.length_sq() > 1e-12).then(|| (-v.y).atan2(v.x));
            // A squashed or long tip, spaced by how far it reaches along
            // the way it goes.
            let reach = if spaced_by_tip {
                reach_along(brush, angle(unit_step).unwrap_or(0.0))
            } else {
                1.0
            };
            let spacing_dist = (spacing_dist * reach).max(0.5);
            if reach < 1.0 {
                // The step owed from before (the first sample's, before the
                // way was known), no longer than this one.
                self.dist_until_next_blit = self.dist_until_next_blit.min(spacing_dist);
            }
            // The way from the last sample: a curve when samples are far
            // apart (a fast stroke), as pieces.
            let path = curve_between(prev, pos, self.last_chord);
            self.last_chord = Some(unit_step);
            let length: f32 = (path.windows(2))
                .map(|w| (w[1] - w[0]).length())
                .sum::<f32>()
                .max(1e-6);

            // The heading, smoothed over the distance travelled: each dab
            // gets it at its own point along the way.
            let settle = (brush.brush_options.diameter * 0.5).max(6.0);
            let start_heading = if self.heading == Vec2::ZERO {
                unit_step
            } else {
                self.heading
            };
            let heading_at = |walked: f32, towards: Vec2| {
                let keep = (-walked / settle).exp();
                start_heading * keep + towards * (1.0 - keep)
            };
            let mut walked = 0.0;
            let mut towards = unit_step;
            for piece in path.windows(2) {
                let (from, to) = (piece[0], piece[1]);
                let piece_len = (to - from).length();
                if piece_len <= 0.0 {
                    continue;
                }
                towards = (to - from) / piece_len;
                let mut dist_left = piece_len;
                let mut cur_pos = from;
                while dist_left >= self.dist_until_next_blit {
                    // Take a step to the next blit point.
                    cur_pos += towards * self.dist_until_next_blit;
                    dist_left -= self.dist_until_next_blit;

                    // Blit.
                    let done = walked + piece_len - dist_left;
                    let dir = angle(heading_at(done, towards)).or(self.dir);
                    for _ in 0..count {
                        let p = if defer {
                            cur_pos
                        } else {
                            self.scatter(brush, cur_pos)
                        };
                        self.pending.push(Pending {
                            pos: p,
                            t: done / length,
                            along: self.travel + done,
                            dir,
                        });
                    }

                    self.dist_until_next_blit = spacing_dist;
                }
                // Take the partial step to the piece's end.
                self.dist_until_next_blit -= dist_left;
                walked += piece_len;
            }
            self.heading = heading_at(length, towards);
            self.dir = angle(self.heading).or(self.dir);
            self.travel += length;
        } else {
            // first point
            for _ in 0..count {
                let p = if defer { pos } else { self.scatter(brush, pos) };
                self.pending.push(Pending {
                    pos: p,
                    t: 1.0,
                    along: 0.0,
                    dir: None,
                });
            }
            self.dist_until_next_blit = spacing_dist;
        }

        self.last_pos = Some(pos);
    }

    /// `pos` moved by the brush's scatter (jitter), if it has one.
    fn scatter(&mut self, brush: &Brush, mut p: Vec2) -> Vec2 {
        if brush.jitter > 0.0 {
            let jitter_amount = (brush.jitter / 100.0) * brush.brush_options.diameter;
            p.x += self.rng.random_range(-jitter_amount..=jitter_amount);
            p.y += self.rng.random_range(-jitter_amount..=jitter_amount);
        }
        p
    }

    /// `pos` moved at random up to `amount` brush widths each way.
    fn scatter_by(&mut self, brush: &Brush, mut p: Vec2, amount: f32) -> Vec2 {
        if amount > 0.0 {
            let jitter_amount = amount * brush.brush_options.diameter;
            p.x += self.rng.random_range(-jitter_amount..=jitter_amount);
            p.y += self.rng.random_range(-jitter_amount..=jitter_amount);
        }
        p
    }

    /// Pixel-perfect Bresenham line stepping to avoid gaps when snapping to pixels.
    fn add_point_pixel_perfect(&mut self, pos: Vec2) {
        let x1 = pos.x.floor() as i32;
        let y1 = pos.y.floor() as i32;

        if let Some(prev) = self.last_pos {
            let x0 = prev.x.floor() as i32;
            let y0 = prev.y.floor() as i32;

            if x0 == x1 && y0 == y1 {
                return;
            }
            let dir = direction(prev, pos);

            let dx = (x1 - x0).abs();
            let dy = -(y1 - y0).abs();
            let sx = if x0 < x1 { 1 } else { -1 };
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut err = dx + dy;

            let mut x = x0;
            let mut y = y0;
            let steps = dx.max(-dy).max(1) as f32;
            let length = (pos - prev).length();

            loop {
                // Chebyshev distance walked, as a fraction of the line.
                let t = (x - x0).abs().max((y - y0).abs()) as f32 / steps;
                self.pending.push(Pending {
                    pos: Vec2 {
                        x: x as f32 + 0.5,
                        y: y as f32 + 0.5,
                    },
                    t,
                    along: self.travel + t * length,
                    dir,
                });

                if x == x1 && y == y1 {
                    break;
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y += sy;
                }
            }
            self.travel += length;
        } else {
            self.pending.push(Pending {
                pos: Vec2 {
                    x: x1 as f32 + 0.5,
                    y: y1 as f32 + 0.5,
                },
                t: 1.0,
                along: 0.0,
                dir: None,
            });
        }
        self.last_pos = Some(pos);
    }
}

/// An input drives the brush's scatter, so each dab is scattered once its
/// variation is known rather than when it's placed.
fn scatter_deferred(brush: &Brush) -> bool {
    !brush.pixel_perfect
        && brush
            .inputs
            .iter()
            .any(|m| m.setting == crate::brush_engine::dynamics::DabSetting::Scatter)
}

/// How far the tip reaches along a stroke going `dir` (radians,
/// counter-clockwise), as a share of its diameter: its outline taken as an
/// ellipse, squashed and turned as the tip is (Krita's anisotropic
/// spacing). A thin nib dragged edge-on is spaced by its thickness, so it
/// doesn't leave beads; a round tip is 1.
fn reach_along(brush: &Brush, dir: f32) -> f32 {
    let tip = &brush.dynamics.tip;
    // A turn that isn't known until the dab (random, tilt, barrel): as
    // round.
    if tip.random_angle > 0.0 || tip.follow_tilt || tip.follow_barrel {
        return 1.0;
    }
    let (w, h) = match &brush.brush_options.pixel_shape {
        crate::brush_engine::brush_options::PixelBrushShape::Custom(t) => {
            let side = t.width.max(t.height).max(1) as f32;
            (t.width as f32 / side, t.height as f32 / side)
        }
        _ => (1.0, 1.0),
    };
    let h = h * tip.ratio.clamp(0.02, 1.0);
    if w >= 1.0 && h >= 1.0 {
        return 1.0;
    }
    // The stroke's way in the tip's own frame.
    let turn = if brush.follows_stroke() {
        -tip.angle.to_radians()
    } else {
        dir - tip.angle.to_radians()
    };
    let (s, c) = turn.sin_cos();
    (1.0 / ((c / w.max(0.02)).powi(2) + (s / h.max(0.02)).powi(2)).sqrt()).clamp(0.05, 1.0)
}

/// The way from sample `a` to `b` as points, `a` first and `b` last:
/// straight, or when they're far apart (a fast stroke, a tablet reporting
/// slowly) and the stroke came in along `incoming` without a sharp turn, a
/// curve leaving `a` that way and arriving along `a`→`b` (a Hermite
/// curve). Each curve starts the way the last one ended, so sparse samples
/// join smoothly without waiting for the next sample.
fn curve_between(a: Vec2, b: Vec2, incoming: Option<Vec2>) -> Vec<Vec2> {
    let chord = b - a;
    let length = chord.length();
    let Some(incoming) = incoming.filter(|d| length > 8.0 && d.dot(chord / length) > 0.5) else {
        return vec![a, b];
    };
    let (m0, m1) = (incoming * length, chord);
    let pieces = (length / 2.0).ceil().clamp(2.0, 64.0) as usize;
    (0..=pieces)
        .map(|i| {
            let t = i as f32 / pieces as f32;
            let (t2, t3) = (t * t, t * t * t);
            a * (2.0 * t3 - 3.0 * t2 + 1.0)
                + m0 * (t3 - 2.0 * t2 + t)
                + b * (3.0 * t2 - 2.0 * t3)
                + m1 * (t3 - t2)
        })
        .collect()
}

/// Paint `plans` in runs of equal pressure level, each with its variation.
fn paint_plans(
    brush: &mut Brush,
    original: (f32, f32, f32),
    plans: &[Plan],
    target: Target,
    context: &mut StrokeContext<'_>,
) {
    let mut start = 0;
    while start < plans.len() {
        let level = plans[start].level;
        let end = plans[start..]
            .iter()
            .position(|p| p.level != level)
            .map_or(plans.len(), |n| start + n);
        apply_pressure(brush, original, level as f32 / PRESSURE_LEVELS);
        let run = &plans[start..end];
        let centers: Vec<Vec2> = run.iter().map(|p| p.pos).collect();
        let vars: Vec<DabVar> = run.iter().map(|p| p.var).collect();
        context.dabs_varied(brush, &centers, &vars, target);
        start = end;
    }
}

/// Shortest time a speed is measured over (seconds).
const SPEED_WINDOW: f32 = 0.008;
/// Time constant of the speed's smoothing (seconds).
const SPEED_SMOOTHING: f32 = 0.06;
/// Time constant of the pen lean's smoothing (seconds).
const LEAN_SMOOTHING: f32 = 0.04;

/// Most airbrush dabs painted at once, however long since the last ones.
const MAX_AIRBRUSH_TICKS: u32 = 8;

/// Seconds between airbrush dabs.
fn airbrush_interval(brush: &Brush) -> f64 {
    1.0 / f64::from(brush.airbrush_rate.max(0.1))
}

/// Pressure steps dabs are grouped by; finer than any visible change.
const PRESSURE_LEVELS: f32 = 64.0;

fn pressure_level(p: f32) -> u32 {
    (p.clamp(0.0, 1.0) * PRESSURE_LEVELS).round() as u32
}

/// Set what pressure `p` drives on the brush, from its unpressured
/// `(diameter, opacity, flow)`. In wash mode pressure-opacity is each dab's
/// opacity (`Brush::wash_opacity`) instead of the stroke's opacity cap,
/// which must stay fixed for a whole stroke.
fn apply_pressure(brush: &mut Brush, (diameter, opacity, flow): (f32, f32, f32), p: f32) {
    let o = &mut brush.brush_options;
    (o.diameter, o.opacity, o.flow) = (diameter, opacity, flow);
    let curves = &o.pressure_curves;
    if o.pressure_size {
        let factor = o.pressure_min_size + (1.0 - o.pressure_min_size) * curves.size(p);
        o.diameter = (diameter * factor).max(1.0);
    }
    let wash = o.painting_mode == crate::brush_engine::brush_options::PaintingMode::Wash;
    let pressure_opacity = o.pressure_opacity.then(|| curves.opacity(p));
    if !wash && let Some(p) = pressure_opacity {
        o.opacity *= p;
    }
    if o.pressure_flow {
        o.flow *= curves.flow(p);
    }
    // Wash: pressure sets each dab's opacity, which the stroke moves toward
    // (its own opacity caps the whole stroke).
    brush.wash_opacity = if wash {
        pressure_opacity.unwrap_or(1.0)
    } else {
        1.0
    };
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod spacing_tests {
    use super::*;

    #[test]
    fn a_squashed_tip_reaches_its_thickness_edge_on_and_its_width_along() {
        let mut b = Brush::new(40.0, 100.0, eframe::egui::Color32::BLACK, 25.0);
        let up = std::f32::consts::FRAC_PI_2;
        assert_eq!(reach_along(&b, up), 1.0);
        b.dynamics.tip.ratio = 0.1;
        assert!((reach_along(&b, up) - 0.1).abs() < 1e-4);
        assert!((reach_along(&b, 0.0) - 1.0).abs() < 1e-4);
        // Turned a quarter, the other way round.
        b.dynamics.tip.angle = 90.0;
        assert!((reach_along(&b, 0.0) - 0.1).abs() < 1e-4);
    }

    #[test]
    fn far_samples_join_in_a_curve_that_leaves_the_way_the_stroke_came() {
        let (a, b) = (Vec2::ZERO, Vec2::new(40.0, 40.0));
        let curve = curve_between(a, b, Some(Vec2::new(1.0, 0.0)));
        assert_eq!((curve[0], *curve.last().unwrap()), (a, b));
        // Coming in along x, it bends that way of the straight line.
        let mid = curve[curve.len() / 2];
        assert!(mid.x - mid.y > 4.0, "{mid:?}");
        // A sharp turn, or close samples: straight.
        assert_eq!(curve_between(a, b, Some(Vec2::new(-1.0, 0.0))), vec![a, b]);
        let near = Vec2::new(4.0, 4.0);
        assert_eq!(
            curve_between(a, near, Some(Vec2::new(1.0, 0.0))),
            vec![a, near]
        );
    }
}
