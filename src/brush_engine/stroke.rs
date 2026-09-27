//! A stroke in progress: spacing and pressure between input samples,
//! and the per-tile buffers dabs accumulate into before being resolved
//! onto the layer.

use crate::brush_engine::brush::{Brush, Target};
use crate::brush_engine::dynamics::{DabVar, FAST_SPEED, direction, tip_orientation};
use crate::brush_engine::stabilizer::Stabilizer;
use crate::brush_engine::symmetry::{Copy2, Symmetry};
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;
use eframe::egui::Color32;
use eframe::egui::Vec2;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rayon::ThreadPool;
use rustc_hash::FxHashMap;
use std::collections::HashSet;
use std::sync::Mutex;

/// Per-tile state for indirect painting (Krita's temporary stroke device):
/// dabs only accumulate `coverage`, and the tile's pixels are re-resolved from
/// `original` (the tile as it was when the stroke first touched it) plus the
/// coverage. Pixels are never re-quantized to 8 bits between dabs.
pub(crate) struct StrokeBuffer {
    pub original: Vec<Color32>,
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
        }
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

    /// Take the stroke's redrawable tail back off.
    fn clear_tails(&mut self, brush: &Brush) {
        brush.clear_tails(self.canvas, self.stroke_tiles);
    }

    /// Keep tail segment `k` for good.
    fn merge_tail(&mut self, brush: &Brush, k: usize) {
        brush.merge_tail(self.canvas, self.stroke_tiles, k);
    }

    fn dabs(&mut self, brush: &mut Brush, centers: &[Vec2]) {
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

/// Tracks per-stroke state like the last position and spacing accumulator.
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
    /// Stroke speed in screen points per second, smoothed.
    speed: f32,
    /// The last raw sample and when it came, for the speed.
    last_sample: Option<(Vec2, f64)>,
    /// Canvas pixels → screen points (the view zoom), for the speed.
    pub view_scale: f32,
    /// Direction of travel, for tips that follow the stroke.
    dir: Option<f32>,
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
            last_sample: None,
            view_scale: 1.0,
            dir: None,
            rng: SmallRng::seed_from_u64(seed),
            tail: Vec::new(),
            newer_from: 0,
            newer_slot: 0,
            held: None,
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
        let dynamic = brush.dynamics.is_active();
        if dynamic {
            self.update_speed(raw_pos, time);
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Spacing and jitter follow this sample's pressure.
            apply_pressure(brush, original, p);
            self.add_point_at_pressure(brush, raw_pos);
            let pending = std::mem::take(&mut self.pending);
            if dynamic {
                let plans: Vec<Plan> = pending
                    .iter()
                    .map(|d| Plan {
                        pos: d.pos,
                        level: pressure_level(from + (p - from) * d.t),
                        var: self.dab_var(brush, d),
                        along: d.along,
                    })
                    .collect();
                self.paint_dynamic(brush, original, plans, context);
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
        }));

        let o = &mut brush.brush_options;
        (o.diameter, o.opacity, o.flow) = original;
        self.last_pressure = Some(p);

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
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
                held.var = self.orient(brush, held.var, None);
                self.tail.push(held);
            }
            if self.tail.is_empty() {
                return;
            }
            context.clear_tails(brush);
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
            paint_plans(brush, original, &tail, Target::Stroke, context);
        }));
        let o = &mut brush.brush_options;
        (o.diameter, o.opacity, o.flow) = original;
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    /// Smoothed screen speed from this sample and the last.
    fn update_speed(&mut self, raw: Vec2, time: Option<f64>) {
        if let (Some((prev, t0)), Some(t1)) = (self.last_sample, time) {
            let dt = (t1 - t0) as f32;
            if dt > 1e-4 {
                let v = (raw - prev).length() * self.view_scale / dt;
                self.speed = if self.speed == 0.0 {
                    v
                } else {
                    self.speed * 0.7 + v * 0.3
                };
            }
        }
        if let Some(t) = time {
            self.last_sample = Some((raw, t));
        }
    }

    /// One dab's variation: start taper, speed, randomness, tip.
    fn dab_var(&mut self, brush: &Brush, dab: &Pending) -> DabVar {
        let d = &brush.dynamics;
        let mut v = DabVar::default();
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
            let s = (self.speed / FAST_SPEED).clamp(0.0, 1.0);
            v.scale *= (1.0 + d.speed.size * s).max(0.05);
            v.strength *= (1.0 + d.speed.opacity * s).max(0.0);
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
        self.orient(brush, v, dab.dir)
    }

    /// The tip's turn and squash for a dab going in direction `dir`.
    fn orient(&mut self, brush: &Brush, mut v: DabVar, dir: Option<f32>) -> DabVar {
        let tip = &brush.dynamics.tip;
        if tip.is_active() {
            let mut angle = tip.angle.to_radians();
            if tip.follow_stroke {
                angle += dir.or(self.dir).unwrap_or(0.0);
            }
            if tip.random_angle > 0.0 {
                angle += (self.rng.random::<f32>() * 2.0 - 1.0) * tip.random_angle.to_radians();
            }
            v.orient = tip_orientation(angle, tip.ratio);
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
        let follow = brush.dynamics.tip.follow_stroke;
        if follow {
            if self.held.is_some() {
                let Some(dir) = self.dir else {
                    // Still no direction: keep waiting.
                    return;
                };
                // The direction is known now: the first dab faces it.
                if let Some(mut held) = self.held.take() {
                    held.var = self.orient(brush, held.var, Some(dir));
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

    /// Queue the dabs for one sample into `self.pending`.
    fn add_point_at_pressure(&mut self, brush: &Brush, raw_pos: Vec2) {
        if brush.pixel_perfect {
            self.add_point_pixel_perfect(raw_pos);
            return;
        }

        let pos = self
            .stabilizer
            .step(&brush.stabilizer_settings(), self.last_pos, raw_pos);

        let spacing_dist = (brush.brush_options.spacing / 100.0) * brush.brush_options.diameter;
        let spacing_dist = spacing_dist.max(0.5); // Avoid infinite loops
        let count = brush.dynamics.random.dabs_per_step();

        if let Some(prev) = self.last_pos {
            let delta = pos - prev;
            let length = delta.length();
            let mut dist_left = length;

            if dist_left == 0.0 {
                return;
            }
            // Very short moves don't have a reliable direction.
            if length >= 1.0
                && let Some(dir) = direction(prev, pos)
            {
                self.dir = Some(dir);
            }
            let dir = self.dir;

            let unit_step = delta / dist_left;
            let mut cur_pos = prev;

            while dist_left >= self.dist_until_next_blit {
                // Take a step to the next blit point.
                cur_pos += unit_step * self.dist_until_next_blit;
                dist_left -= self.dist_until_next_blit;

                // Blit.
                let along = self.travel + (length - dist_left);
                for _ in 0..count {
                    let p = self.scatter(brush, cur_pos);
                    self.pending.push(Pending {
                        pos: p,
                        t: 1.0 - dist_left / length,
                        along,
                        dir,
                    });
                }

                self.dist_until_next_blit = spacing_dist;
            }

            // Take the partial step to land at the sample.
            self.dist_until_next_blit -= dist_left;
            self.travel += length;
        } else {
            // first point
            for _ in 0..count {
                let p = self.scatter(brush, pos);
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

/// Pressure steps dabs are grouped by; finer than any visible change.
const PRESSURE_LEVELS: f32 = 64.0;

fn pressure_level(p: f32) -> u32 {
    (p.clamp(0.0, 1.0) * PRESSURE_LEVELS).round() as u32
}

/// Set what pressure `p` drives on the brush, from its unpressured
/// `(diameter, opacity, flow)`. In wash mode pressure-opacity goes to the
/// dabs' strength instead of the stroke's opacity cap, which must stay fixed
/// for a whole stroke.
fn apply_pressure(brush: &mut Brush, (diameter, opacity, flow): (f32, f32, f32), p: f32) {
    let o = &mut brush.brush_options;
    (o.diameter, o.opacity, o.flow) = (diameter, opacity, flow);
    if o.pressure_size {
        let factor = o.pressure_min_size + (1.0 - o.pressure_min_size) * p;
        o.diameter = (diameter * factor).max(1.0);
    }
    if o.pressure_opacity {
        if o.painting_mode == crate::brush_engine::brush_options::PaintingMode::Wash {
            o.flow *= p;
        } else {
            o.opacity *= p;
        }
    }
    if o.pressure_flow {
        o.flow *= p;
    }
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new()
    }
}
