//! Smudge and Blur: blending tools that work with the normal brush's size,
//! hardness, spacing, flow, opacity and pressure settings, but move or
//! soften the paint already on the layer instead of adding colour (like
//! Clip Studio's Blend tools).
//!
//! Each has more modes: Smudge can instead **deform** (push the paint
//! along, grow, shrink or swirl it) or
//! **clone** (paint with the pixels from another place, set with
//! Ctrl+click); Blur can instead **sharpen** or **adjust** colours (hue,
//! saturation, brightness) under the brush, or paint any of the Filter
//! menu's filters (**filter**).
//!
//! Smudge carries a patch of paint along the stroke: every dab mixes the
//! carried paint into the canvas under the tip, then picks up some of the
//! result (how much it keeps is the smudge length). With a colour rate it
//! is a wet mixing brush: each dab first mixes
//! that much of the brush colour into the carried paint, so it lays down
//! the brush colour blended with whatever it drags. Blur mixes each pixel
//! toward the average around it. Both are sequential per dab (each dab sees
//! the previous one's result), so they run their own small engine rather
//! than the batched brush pipeline: a [`BlendSession`], painted on the
//! stroke worker like a brush stroke, so a big tip never holds up the UI.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::brush_engine::brush_options::PixelBrushShape;
use crate::canvas::history::{LayerHistoryOp, TileSnapshot, UndoAction};
use crate::canvas::storage::LayerKind;
use eframe::egui::{Color32, Vec2};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BlendToolSettings {
    /// Smudge: how much of the carried paint stays with the brush each dab
    /// (0 = barely drags, 1 = smears a colour a long way).
    pub smudge_length: f32,
    /// Blur: size of the area averaged, relative to the brush radius.
    pub blur_size: f32,
    /// Smudge: how much of the brush colour is mixed into the carried paint
    /// per brush width travelled (0 = a plain smudge, 1 = all brush colour).
    pub color_rate: f32,
    /// What the Smudge tool does.
    pub smudge_mode: SmudgeMode,
    /// What the Blur tool does.
    pub filter_mode: FilterMode,
    /// Deform: how it moves the paint, and how far (0..1).
    pub deform_mode: DeformMode,
    pub deform_amount: f32,
    /// Sharpen: how much edges are strengthened (0..2).
    pub sharpen_amount: f32,
    /// Adjust: hue turn (degrees), saturation and brightness change (-1..1).
    pub adjust_hue: f32,
    pub adjust_saturation: f32,
    pub adjust_value: f32,
    /// Filter: the filter painted, with its settings.
    pub brush_filter: crate::canvas::filters::Filter,
    /// Clone: where to copy from (canvas), set with Ctrl+click.
    #[serde(skip)]
    pub clone_source: Option<Vec2>,
    /// Clone: keep the same offset from stroke to stroke (else each stroke
    /// starts again from the source).
    pub clone_aligned: bool,
    /// Clone: copy what's visible (all layers) rather than this layer.
    pub clone_merged: bool,
    /// Clone: the offset kept while aligned, from the first stroke.
    #[serde(skip)]
    clone_offset: Option<Vec2>,
}

impl Default for BlendToolSettings {
    fn default() -> Self {
        Self {
            smudge_length: 0.8,
            blur_size: 0.35,
            color_rate: 0.0,
            smudge_mode: SmudgeMode::Smudge,
            filter_mode: FilterMode::Blur,
            deform_mode: DeformMode::Push,
            deform_amount: 0.5,
            sharpen_amount: 0.8,
            adjust_hue: 30.0,
            adjust_saturation: 0.0,
            adjust_value: 0.0,
            brush_filter: crate::canvas::filters::Filter::GaussianBlur { radius: 4.0 },
            clone_source: None,
            clone_aligned: true,
            clone_merged: false,
            clone_offset: None,
        }
    }
}

impl BlendToolSettings {
    /// Set where Clone copies from (a new source forgets the old offset).
    pub fn set_clone_source(&mut self, at: Vec2) {
        self.clone_source = Some(at);
        self.clone_offset = None;
    }
}

/// What the Smudge tool does.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SmudgeMode {
    /// Carry the paint along (and mix in the brush colour).
    #[default]
    Smudge,
    /// Move the pixels under the brush (see [`DeformMode`]).
    Deform,
    /// Paint with the pixels from the clone source.
    Clone,
}

/// What the Blur tool does.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FilterMode {
    #[default]
    Blur,
    /// Strengthen edges (an unsharp mask).
    Sharpen,
    /// Shift hue, saturation and brightness.
    Adjust,
    /// Paint one of the Filter menu's filters (see
    /// [`BlendToolSettings::brush_filter`]).
    Filter,
}

/// How Deform moves the paint.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeformMode {
    /// Along with the brush.
    #[default]
    Push,
    /// Out from the middle (magnify).
    Grow,
    /// In towards the middle.
    Shrink,
    /// Round the middle, counter-clockwise or clockwise.
    SwirlLeft,
    SwirlRight,
}

impl DeformMode {
    pub const ALL: [DeformMode; 5] = [
        Self::Push,
        Self::Grow,
        Self::Shrink,
        Self::SwirlLeft,
        Self::SwirlRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Push => "Push",
            Self::Grow => "Grow",
            Self::Shrink => "Shrink",
            Self::SwirlLeft => "Swirl ↺",
            Self::SwirlRight => "Swirl ↻",
        }
    }
}

/// What one blend stroke does, fixed when it starts.
#[derive(Copy, Clone, Debug, PartialEq)]
enum BlendKind {
    Smudge,
    Blur,
    Sharpen(f32),
    Adjust([f32; 3]),
    /// A filter, over the layer as it was before the stroke.
    Filter(crate::canvas::filters::Filter),
    Deform(DeformMode, f32),
    /// Copy from `offset` pixels away (whole pixels), from all layers when
    /// `merged`.
    Clone {
        offset: (i32, i32),
        merged: bool,
    },
}

/// How a smudge stroke mixes: the Smudge tool's settings, or a mixing
/// brush's own (see [`crate::brush_engine::brush_options::Mixing`]).
#[derive(Clone)]
struct SmudgeMix {
    length: f32,
    color_rate: f32,
    pressure_length: bool,
    pressure_color: bool,
    /// How a mixing brush's paint goes onto the layer.
    blend: crate::canvas::blend_modes::LayerBlend,
    /// The stroke is the Brush tool's (ended with the brush's strokes).
    from_brush: bool,
    /// An imported brush's own colour smudge, instead of this app's.
    krita: Option<crate::brush_engine::brush_options::KritaSmudge>,
}

/// A patch of carried paint (linear premultiplied, 0..1 per channel).
struct Carry {
    side: usize,
    px: Vec<[f32; 4]>,
}

/// A blend stroke in progress, as the app knows it (the stroke worker
/// paints it).
#[derive(Clone, Copy)]
pub struct ActiveBlend {
    /// The Brush tool's (a mixing brush), not the Smudge or Blur tool's.
    from_brush: bool,
    kind: BlendKind,
}

/// A blend stroke as the stroke worker paints it: what it reads, captured
/// when it starts, and the stroke's own state.
pub struct BlendSession {
    canvas: Arc<crate::canvas::Canvas>,
    pool: Arc<rayon::ThreadPool>,
    selection: Option<crate::selection::SelectionManager>,
    brush: crate::brush_engine::brush::Brush,
    /// Blur: the area averaged, relative to the brush radius.
    blur_size: f32,
    wrap: bool,
    /// The layer painted.
    idx: usize,
    /// Canvas rectangles painted since the worker last collected them.
    damage: Vec<[i32; 4]>,
    stroke: BlendStroke,
    /// Smudging (the Smudge tool, a mixing brush): where its dabs go.
    dabber: Option<Box<Dabber>>,
}

/// The dabs of a smudge, placed and varied as the brush's own strokes'
/// are (spacing, stabiliser, dynamics, inputs, scatter, tip, angle), each
/// then mixed into the layer in turn.
struct Dabber {
    stroke: crate::brush_engine::stroke::StrokeState,
    /// Collects the dabs (nothing is painted through it).
    tiles: crate::brush_engine::stroke::StrokeTiles,
    undo: UndoAction,
    /// The brush that places them: the stroke's, without what a smudge
    /// can't do (an end taper drawn again, a dual tip's mask, wet edges).
    brush: crate::brush_engine::brush::Brush,
    seed: u64,
    /// The last sample's pressure (the smudge length and colour rate).
    pressure: f32,
    /// Post-correction: the samples so far, to smudge again along the
    /// smoothed path when the pen lifts.
    samples: Option<Vec<crate::brush_engine::stroke_worker::PenSample>>,
    /// Where the pen went down, and its pressure (the stroke's first dab,
    /// smudged again too).
    first: (Vec2, f32),
}

/// A dab placed by the brush, for one smudge dab.
struct Placed {
    dab: crate::brush_engine::dab::PlacedDab,
    strength: f32,
}

struct BlendStroke {
    kind: BlendKind,
    /// The way the brush last moved (unit), for Deform's push.
    dir: Vec2,
    /// Tiles as they were before the stroke first changed them.
    before: HashMap<(i32, i32), Vec<Color32>>,
    last: Option<Vec2>,
    /// The previous sample's pressure: dabs between samples blend from it.
    last_pressure: f32,
    /// Distance travelled since the last dab.
    travelled: f32,
    /// Smudge: the paint each mirror copy carries (copy 0 is the stroke
    /// itself).
    carries: Vec<Option<Carry>>,
    /// Mirror painting for this stroke.
    symmetry: crate::brush_engine::symmetry::Symmetry,
    copies: Vec<crate::brush_engine::symmetry::Copy2>,
    /// Filter: each tile of the layer filtered as it was before the stroke
    /// (worked out when the brush first reaches it), and how much of it the
    /// stroke has laid down so far (0..1 per pixel).
    filtered: HashMap<(i32, i32), Vec<Color32>>,
    coverage: HashMap<(i32, i32), Vec<f32>>,
    /// Smudge: how it mixes.
    mix: SmudgeMix,
    /// The imported smudge: where each mirror copy's last dab was (it reads the
    /// layer there), and the stroke's random state (a turning tip).
    krita_last: Vec<Option<Vec2>>,
    random: u32,
    /// Krita's colour smudge with a lightness tip: the layer's lightness
    /// map tiles as they were before the stroke first changed them.
    lightness_before: HashMap<(i32, i32), Option<Vec<u16>>>,
}

/// Pixels are mixed as linear-light premultiplied colour, the same space
/// the compositor works in. (Mixing the stored sRGB bytes and clamping each
/// channel to alpha darkened light colours at soft edges, as if another
/// colour were being picked up.)
fn to_f(c: Color32) -> [f32; 4] {
    crate::canvas::blend::LinearDecoder::new()
        .decode(c)
        .to_array()
}

fn to_c(v: [f32; 4]) -> Color32 {
    encode(crate::canvas::blend::LinearEncoder::new(), v)
}

#[inline]
fn encode(encoder: crate::canvas::blend::LinearEncoder, v: [f32; 4]) -> Color32 {
    let a = v[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return Color32::TRANSPARENT;
    }
    // Premultiplied: colour can't exceed alpha.
    let c = |x: f32| x.clamp(0.0, a);
    encoder.encode(eframe::egui::Rgba::from_rgba_premultiplied(
        c(v[0]),
        c(v[1]),
        c(v[2]),
        a,
    ))
}

/// [`to_f`] and [`to_c`] with their tables looked up once, for the loops
/// over a dab's pixels (once a pixel, the lookups cost more than the
/// conversion).
#[derive(Clone, Copy)]
struct Codec {
    decoder: crate::canvas::blend::LinearDecoder,
    encoder: crate::canvas::blend::LinearEncoder,
}

impl Codec {
    #[inline]
    fn new() -> Self {
        Self {
            decoder: crate::canvas::blend::LinearDecoder::new(),
            encoder: crate::canvas::blend::LinearEncoder::new(),
        }
    }

    #[inline]
    fn to_f(self, c: Color32) -> [f32; 4] {
        self.decoder.decode(c).to_array()
    }

    #[inline]
    fn to_c(self, v: [f32; 4]) -> Color32 {
        encode(self.encoder, v)
    }
}

/// Run `f` on each `side`-long row of `data` (with its index), on `pool`
/// when the dab is big enough for that to pay.
fn for_rows<T: Send>(
    pool: &rayon::ThreadPool,
    data: &mut [T],
    side: usize,
    f: impl Fn(usize, &mut [T]) + Sync,
) {
    use rayon::prelude::*;
    if side >= PARALLEL_SIDE {
        // Rows in batches of a few thousand pixels: a task per row costs
        // more in handing out than it saves.
        let rows = (PARALLEL_PIXELS / side).max(1);
        pool.install(|| {
            data.par_chunks_mut(side * rows)
                .enumerate()
                .for_each(|(batch, lines)| {
                    for (i, line) in lines.chunks_mut(side).enumerate() {
                        f(batch * rows + i, line);
                    }
                })
        });
    } else {
        for (row, line) in data.chunks_mut(side).enumerate() {
            f(row, line);
        }
    }
}

/// Dabs this wide or wider are worked on by rows in parallel, about this
/// many pixels a task.
const PARALLEL_SIDE: usize = 64;
const PARALLEL_PIXELS: usize = 4096;

/// The pixels of layer `source` (all visible layers when `None`) over the
/// `w`×`h` canvas rectangle at `origin`; with `wrap`, what's past an edge
/// comes from the other side.
fn read_patch(
    canvas: &crate::canvas::Canvas,
    source: Option<usize>,
    (x0, y0): (i32, i32),
    w: usize,
    h: usize,
    wrap: bool,
) -> Vec<Color32> {
    if !wrap {
        return canvas.render_reference(source, x0, y0, w, h);
    }
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    let mut out = vec![Color32::TRANSPARENT; w * h];
    for (sx, dx, pw) in wrap_pieces(x0, w, cw) {
        for (sy, dy, ph) in wrap_pieces(y0, h, ch) {
            let part = canvas.render_reference(source, sx, sy, pw, ph);
            for row in 0..ph {
                let at = (dy + row) * w + dx;
                out[at..at + pw].copy_from_slice(&part[row * pw..(row + 1) * pw]);
            }
        }
    }
    out
}

/// The span `start..start + len` on a canvas `size` long that wraps round:
/// its pieces as `(canvas start, offset in the span, length)`.
fn wrap_pieces(start: i32, len: usize, size: i32) -> Vec<(i32, usize, usize)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < len {
        let at = (start + offset as i32).rem_euclid(size);
        let run = ((size - at) as usize).min(len - offset);
        out.push((at, offset, run));
        offset += run;
    }
    out
}

/// A patch row's pieces on the canvas, as [`wrap_pieces`] gives them with
/// wrap-around, else just the part on the canvas.
fn wrap_pieces_or_clip(start: i32, len: usize, size: i32, wrap: bool) -> Vec<(i32, usize, usize)> {
    if wrap {
        return wrap_pieces(start, len, size);
    }
    let (from, to) = (start.max(0), (start + len as i32).min(size));
    if from >= to {
        return Vec::new();
    }
    vec![(from, (from - start) as usize, (to - from) as usize)]
}

/// Canvas row `y` (round with wrap-around); `None` off the canvas.
fn canvas_row(y: i32, size: i32, wrap: bool) -> Option<usize> {
    if wrap {
        Some(y.rem_euclid(size) as usize)
    } else {
        (0..size).contains(&y).then_some(y as usize)
    }
}

/// `px` (a `side`×`side` patch) at `p` (texel centres at +0.5), bilinear;
/// outside it reads its nearest edge.
fn sample_bilinear(px: &[[f32; 4]], side: usize, p: Vec2) -> [f32; 4] {
    let max = (side - 1) as f32;
    let (x, y) = ((p.x - 0.5).clamp(0.0, max), (p.y - 0.5).clamp(0.0, max));
    let (ix, iy) = (x as usize, y as usize);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let (jx, jy) = ((ix + 1).min(side - 1), (iy + 1).min(side - 1));
    let at = |x: usize, y: usize| px[y * side + x];
    let (a, b, c, d) = (at(ix, iy), at(jx, iy), at(ix, jy), at(jx, jy));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        top + (bottom - top) * fy
    })
}

/// A pixel (linear premultiplied) with its hue turned by `hsv[0]` degrees
/// and its saturation and brightness moved by `hsv[1]`, `hsv[2]`.
fn adjust_hsv(v: [f32; 4], hsv: [f32; 3]) -> [f32; 4] {
    let c = to_c(v);
    if c.a() == 0 {
        return v;
    }
    let srgb = crate::brush_engine::dynamics::shift_hsv(c, hsv);
    let byte = |x: f32| (x * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    to_f(Color32::from_rgba_unmultiplied(
        byte(srgb[0]),
        byte(srgb[1]),
        byte(srgb[2]),
        c.a(),
    ))
}

/// Box blur of a `side`×`side` patch with radius `r` (two passes, so it
/// looks nearly Gaussian). Each output pixel averages what of its window
/// lies inside the patch. Rows go across the pool, a big patch's columns in
/// bands of rows (a window of whole rows sliding down, so memory is read in
/// order).
fn box_blur(pool: &rayon::ThreadPool, src: &[[f32; 4]], side: usize, r: usize) -> Vec<[f32; 4]> {
    let mut a = src.to_vec();
    let mut b = vec![[0.0f32; 4]; side * side];
    for _ in 0..2 {
        // Across.
        for_rows(pool, &mut b, side, |row, out| {
            blur_line(&a[row * side..(row + 1) * side], out, r);
        });
        // Down.
        blur_columns(pool, &b, &mut a, side, r);
    }
    a
}

/// One line of a box blur: `out[i]` averages `line` over `i - r..=i + r`
/// (what of it is inside).
fn blur_line(line: &[[f32; 4]], out: &mut [[f32; 4]], r: usize) {
    let n = line.len();
    let mut sum = [0.0f32; 4];
    let mut count = 0.0f32;
    for p in &line[..=r.min(n - 1)] {
        for c in 0..4 {
            sum[c] += p[c];
        }
        count += 1.0;
    }
    for i in 0..n {
        out[i] = sum.map(|s| s / count);
        if i + r + 1 < n {
            let p = line[i + r + 1];
            for c in 0..4 {
                sum[c] += p[c];
            }
            count += 1.0;
        }
        if i >= r {
            let p = line[i - r];
            for c in 0..4 {
                sum[c] -= p[c];
            }
            count -= 1.0;
        }
    }
}

/// The down pass of [`box_blur`]: `src`'s columns blurred into `out`.
fn blur_columns(
    pool: &rayon::ThreadPool,
    src: &[[f32; 4]],
    out: &mut [[f32; 4]],
    side: usize,
    r: usize,
) {
    use rayon::prelude::*;
    let row = |y: usize| &src[y * side..(y + 1) * side];
    // Output rows `first..first + lines.len() / side`, with their own window.
    let band = |first: usize, lines: &mut [[f32; 4]]| {
        let (lo, hi) = (first.saturating_sub(r), (first + r).min(side - 1));
        let mut sum = vec![[0.0f32; 4]; side];
        for y in lo..=hi {
            for (s, p) in sum.iter_mut().zip(row(y)) {
                for c in 0..4 {
                    s[c] += p[c];
                }
            }
        }
        let mut count = (hi - lo + 1) as f32;
        for (k, line) in lines.chunks_mut(side).enumerate() {
            let y = first + k;
            for (o, s) in line.iter_mut().zip(&sum) {
                *o = s.map(|v| v / count);
            }
            if y + r + 1 < side {
                for (s, p) in sum.iter_mut().zip(row(y + r + 1)) {
                    for c in 0..4 {
                        s[c] += p[c];
                    }
                }
                count += 1.0;
            }
            if y >= r {
                for (s, p) in sum.iter_mut().zip(row(y - r)) {
                    for c in 0..4 {
                        s[c] -= p[c];
                    }
                }
                count -= 1.0;
            }
        }
    };
    if side >= PARALLEL_SIDE {
        // Bands of a few thousand pixels' worth of rows (each starts its
        // window afresh: worth it once the band is longer than it).
        let rows = (PARALLEL_PIXELS / side).max(2 * r + 1).min(side);
        pool.install(|| {
            out.par_chunks_mut(side * rows)
                .enumerate()
                .for_each(|(i, lines)| band(i * rows, lines))
        });
    } else {
        band(0, out);
    }
}

/// Point `i` (from 1) of the Halton sequence in `base`, in 0..1.
fn halton(mut i: u32, base: u32) -> f32 {
    let (mut f, mut out) = (1.0f32, 0.0f32);
    while i > 0 {
        f /= base as f32;
        out += f * (i % base) as f32;
        i /= base;
    }
    out
}

/// Dulling's colour: the average of `source` (a `side`² patch) weighted by
/// `mask`, over the pixels within `reach` of the middle. Like Krita, it
/// samples spread-out pixels (a Halton sequence) until the colour stops
/// moving rather than reading them all: at least 64 (or 2%), then 16 at a
/// time until no channel moves more than 2/255. `None` if what it read
/// carried less than half a pixel's weight.
fn halton_dull(
    source: &[Color32],
    mask: &[f32],
    side: usize,
    reach: f32,
    codec: Codec,
) -> Option<[f32; 4]> {
    let mid = side as f32 * 0.5;
    let inside = |l: usize| (l as f32 + 0.5 - mid).abs() <= reach;
    let lo = (0..side).find(|&l| inside(l))?;
    let hi = (0..side).rfind(|&l| inside(l))? + 1;
    let n_side = hi - lo;
    let n = n_side * n_side;
    let first = n.min(64.max((n as f32 * 0.02).round() as usize));
    let (mut sum, mut weight) = ([0.0f32; 4], 0.0f32);
    let take = |i: u32, sum: &mut [f32; 4], weight: &mut f32| {
        let x = lo + ((halton(i, 2) * n_side as f32) as usize).min(n_side - 1);
        let y = lo + ((halton(i, 3) * n_side as f32) as usize).min(n_side - 1);
        let at = y * side + x;
        let w = mask[at];
        if w > 0.0 {
            let px = codec.to_f(source[at]);
            for c in 0..4 {
                sum[c] += px[c] * w;
            }
            *weight += w;
        }
    };
    let mean = |sum: [f32; 4], weight: f32| sum.map(|v| v / weight.max(1e-6));
    let mut i = 1u32;
    for _ in 0..first {
        take(i, &mut sum, &mut weight);
        i += 1;
    }
    let mut taken = first;
    let mut last = mean(sum, weight);
    while taken < n {
        let batch = (n - taken).min(16);
        for _ in 0..batch {
            take(i, &mut sum, &mut weight);
            i += 1;
        }
        taken += batch;
        let now = mean(sum, weight);
        let moved = (0..4).map(|c| (now[c] - last[c]).abs()).fold(0.0, f32::max);
        last = now;
        if moved * 255.0 <= 2.0 {
            break;
        }
    }
    (weight > 0.5).then_some(last)
}

impl PainterApp {
    pub(crate) fn set_blend_tool(&mut self, smudge: bool) {
        // They use the brush's settings, not the eraser's.
        self.set_brush_tool(false);
        self.active_tool = if smudge { Tool::Smudge } else { Tool::Blur };
    }

    pub(crate) fn blend_press(&mut self, pos: Vec2, pressure: f32) {
        let b = &self.workspace.blend;
        let mix = SmudgeMix {
            length: b.smudge_length,
            color_rate: b.color_rate,
            pressure_length: false,
            pressure_color: false,
            blend: crate::canvas::blend_modes::LayerBlend::Normal,
            from_brush: false,
            krita: None,
        };
        self.blend_begin(pos, pressure, None, mix);
    }

    /// A stroke of a brush with colour mixing: a smudge with the brush's own
    /// rates, tip and blend mode.
    pub(crate) fn mixing_press(&mut self, pos: Vec2, pressure: f32) {
        let brush = &self.brush_state.brush;
        let Some(m) = brush.mixing else {
            return;
        };
        let mix = SmudgeMix {
            length: m.smudge_length,
            color_rate: m.color_rate,
            pressure_length: m.pressure_length,
            pressure_color: m.pressure_color,
            blend: brush.paint_blend,
            from_brush: true,
            krita: m.krita,
        };
        self.blend_begin(pos, pressure, Some(BlendKind::Smudge), mix);
    }

    /// A smudge stroke of the Brush tool's (a brush with colour mixing) is
    /// in progress.
    pub(crate) fn mixing_stroke(&self) -> bool {
        self.brush_state
            .blend_stroke
            .as_ref()
            .is_some_and(|s| s.from_brush)
    }

    /// Start a blend stroke: `kind`, or the active tool's.
    fn blend_begin(&mut self, pos: Vec2, pressure: f32, kind: Option<BlendKind>, mix: SmudgeMix) {
        let pos = self.ruler_begin_stroke(pos);
        let idx = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        if layer.locked || matches!(layer.kind, LayerKind::Group) {
            return;
        }
        let b = &mut self.workspace.blend;
        let kind = match kind {
            Some(kind) => kind,
            None => match (self.active_tool, b.smudge_mode, b.filter_mode) {
                (Tool::Smudge, SmudgeMode::Smudge, _) => BlendKind::Smudge,
                (Tool::Smudge, SmudgeMode::Deform, _) => {
                    BlendKind::Deform(b.deform_mode, b.deform_amount.clamp(0.0, 1.0))
                }
                (Tool::Smudge, SmudgeMode::Clone, _) => {
                    let Some(source) = b.clone_source else {
                        // Nothing to copy from yet: Ctrl+click sets it.
                        return;
                    };
                    let offset = match (b.clone_aligned, b.clone_offset) {
                        (true, Some(offset)) => offset,
                        _ => (source - pos).round(),
                    };
                    if b.clone_aligned {
                        b.clone_offset = Some(offset);
                    }
                    BlendKind::Clone {
                        offset: (offset.x as i32, offset.y as i32),
                        merged: b.clone_merged,
                    }
                }
                (_, _, FilterMode::Blur) => BlendKind::Blur,
                (_, _, FilterMode::Sharpen) => BlendKind::Sharpen(b.sharpen_amount.clamp(0.0, 2.0)),
                (_, _, FilterMode::Adjust) => BlendKind::Adjust([
                    b.adjust_hue.clamp(-180.0, 180.0),
                    b.adjust_saturation.clamp(-1.0, 1.0),
                    b.adjust_value.clamp(-1.0, 1.0),
                ]),
                (_, _, FilterMode::Filter) => BlendKind::Filter(b.brush_filter),
            },
        };
        // Like a brush stroke: a second press ends the running one first
        // (without waiting for the worker to paint what's queued).
        self.finish_stroke();
        // A text or vector layer becomes pixels first (its undo step
        // brings it back).
        self.rasterise_text_for_stroke();
        self.rasterise_vector_for_stroke();
        self.mark_action();
        let mut brush = self.brush_state.brush.clone();
        brush.second_color = self.brush_state.secondary_color;
        // Krita's colour smudge with a lightness tip lays its tip's grey on
        // the layer's lightness map.
        if kind == BlendKind::Smudge && brush.lays_lightness() {
            self.ensure_lightness_map(idx);
        }
        let dabber = (kind == BlendKind::Smudge).then(|| {
            let mut placing = brush.clone();
            placing.dynamics.taper.end = 0.0;
            placing.dual = None;
            placing.wet_edge = 0.0;
            let seed = rand::random();
            let mut stroke = crate::brush_engine::stroke::StrokeState::with_seed(seed);
            stroke.view_scale = self.viewport.zoom;
            let correcting = placing.stabilizer_algorithm
                == crate::brush_engine::brush::StabilizerAlgorithm::PostCorrection
                && placing.stabilizer_modes.correction > 0.0;
            Box::new(Dabber {
                stroke,
                tiles: crate::brush_engine::stroke::StrokeTiles::collecting(),
                undo: UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                },
                brush: placing,
                seed,
                pressure,
                samples: correcting.then(Vec::new),
                first: (pos, pressure),
            })
        });
        let session = BlendSession {
            canvas: Arc::clone(&self.canvas),
            pool: Arc::clone(&self.workspace.pool),
            selection: self.selection_manager.has_selection().then(|| {
                crate::selection::SelectionManager::with_shape(self.layer_selection_shape())
            }),
            brush,
            blur_size: self.workspace.blend.blur_size,
            wrap: self.workspace.wrap_around,
            idx,
            damage: Vec::new(),
            dabber,
            stroke: BlendStroke {
                kind,
                dir: Vec2::ZERO,
                before: HashMap::new(),
                last: Some(pos),
                last_pressure: pressure,
                travelled: 0.0,
                carries: Vec::new(),
                symmetry: self.workspace.symmetry,
                copies: self.workspace.symmetry.copies(),
                filtered: HashMap::new(),
                coverage: HashMap::new(),
                mix,
                krita_last: Vec::new(),
                random: 0x9e37_79b9,
                lightness_before: HashMap::new(),
            },
        };
        self.brush_state.blend_stroke = Some(ActiveBlend {
            from_brush: session.stroke.mix.from_brush,
            kind,
        });
        // Painted on the stroke worker, like a brush stroke: a big tip
        // never holds up the frame.
        self.stroke_worker.begin_sequential(Box::new(session));
    }

    pub(crate) fn blend_drag(&mut self, pos: Vec2, pressure: f32) {
        if self.brush_state.blend_stroke.is_none() {
            return;
        }
        let pos = self.ruler_snap(pos);
        self.stroke_worker.sample(pos, pressure);
    }

    /// End the blend stroke; the worker files its undo step once it has
    /// painted what was queued.
    pub(crate) fn blend_release(&mut self) {
        if self.brush_state.blend_stroke.take().is_some() {
            self.stroke_worker.end();
        }
    }
}

impl crate::brush_engine::stroke_worker::SequentialStroke for BlendSession {
    /// The stroke's first dab, where it started.
    fn start(&mut self) {
        let Some(pos) = self.stroke.last else {
            return;
        };
        let pressure = self.stroke.last_pressure;
        if self.dabber.is_some() {
            let dabs = self.place(|stroke, brush, context| {
                stroke.add_sample(brush, pos, pressure, None, context)
            });
            return self.lay(dabs);
        }
        let pool = Arc::clone(&self.pool);
        pool.install(|| self.blend_mirrored(pos, pressure));
    }

    fn sample(&mut self, s: crate::brush_engine::stroke_worker::PenSample) {
        let Some(dabber) = self.dabber.as_mut() else {
            return self.drag(s.pos, s.pressure);
        };
        dabber.pressure = s.pressure;
        if let Some(samples) = dabber.samples.as_mut() {
            samples.push(s);
        }
        let dabs = self.place(|stroke, brush, context| {
            stroke.tilt = s.tilt;
            stroke.barrel = s.barrel;
            stroke.add_sample(brush, s.pos, s.pressure, Some(s.time), context)
        });
        self.stroke.last = Some(s.pos);
        self.lay(dabs);
    }

    fn airbrush_rate(&self) -> f32 {
        self.dabber.as_ref().map_or(0.0, |d| d.brush.airbrush_rate)
    }

    fn airbrush(&mut self, now: f64) {
        if self.dabber.is_some() {
            let dabs = self.place(|stroke, brush, context| stroke.airbrush(brush, now, context));
            self.lay(dabs);
        }
    }

    /// The pen moved to `pos`: dabs along the way, `spacing` apart.
    fn drag(&mut self, pos: Vec2, pressure: f32) {
        let diameter = self.blend_diameter(pressure);
        let o = &self.brush.brush_options;
        let spacing = o.spacing_px(diameter, o.spacing_factor(pressure)).max(1.0);
        let stroke = &mut self.stroke;
        let Some(last) = stroke.last else {
            return;
        };
        let travel = pos - last;
        let length = travel.length();
        if length <= 0.0 {
            return;
        }
        let dir = travel / length;
        let mut t = spacing - stroke.travelled;
        let mut dabs = Vec::new();
        // Pressure blends along the segment, so a pen's change shows no
        // steps between samples.
        let from = stroke.last_pressure;
        while t <= length {
            dabs.push((last + dir * t, from + (pressure - from) * (t / length)));
            t += spacing;
        }
        stroke.travelled = length - (t - spacing);
        stroke.last = Some(pos);
        stroke.last_pressure = pressure;
        stroke.dir = dir;
        // On the pool from the start: the dabs' parallel parts then share
        // its threads without handing each one over (and waking them) anew.
        let pool = Arc::clone(&self.pool);
        pool.install(|| {
            for (p, pr) in dabs {
                self.blend_mirrored(p, pr);
            }
        });
    }

    /// The stroke's undo step: the tiles as they were before it (none if
    /// it changed nothing).
    fn finish(mut self: Box<Self>) -> Option<UndoAction> {
        if self.dabber.is_some() {
            self.correct();
            let dabs = self.place(|stroke, brush, context| stroke.finish(brush, context));
            self.lay(dabs);
        }
        if self.stroke.before.is_empty() && self.stroke.lightness_before.is_empty() {
            return None;
        }
        let ts = self.canvas.tile_size();
        let layer_id = self.canvas.layers.get(self.idx)?.id;
        // The lightness map's tiles as they were, sorted (the same step
        // every time).
        let mut lightness: Vec<_> = std::mem::take(&mut self.stroke.lightness_before)
            .into_iter()
            .collect();
        lightness.sort_by_key(|(k, _)| (k.1, k.0));
        let layer_action = (!lightness.is_empty()).then_some(LayerHistoryOp::Height {
            layer: layer_id,
            tiles: lightness,
            map: None,
            inner: None,
        });
        let tiles = self
            .stroke
            .before
            .into_iter()
            .map(|((tx, ty), data)| TileSnapshot {
                tx,
                ty,
                layer_id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: data.into(),
            })
            .collect();
        Some(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action,
        })
    }

    fn take_damage(&mut self) -> Vec<[i32; 4]> {
        std::mem::take(&mut self.damage)
    }

    fn canvas(&self) -> &crate::canvas::Canvas {
        &self.canvas
    }

    fn layer_idx(&self) -> usize {
        self.idx
    }
}

impl BlendSession {
    /// Run `f` on the dab placer: the dabs it placed.
    fn place(
        &mut self,
        f: impl FnOnce(
            &mut crate::brush_engine::stroke::StrokeState,
            &mut crate::brush_engine::brush::Brush,
            &mut crate::brush_engine::stroke::StrokeContext<'_>,
        ),
    ) -> Vec<crate::brush_engine::stroke::CollectedDab> {
        let Some(d) = self.dabber.as_mut() else {
            return Vec::new();
        };
        let Dabber {
            stroke,
            tiles,
            undo,
            brush,
            ..
        } = &mut **d;
        let mut context = crate::brush_engine::stroke::StrokeContext::new(
            &self.pool,
            &self.canvas,
            None,
            undo,
            tiles,
        );
        f(stroke, brush, &mut context);
        tiles.collect.replace(Vec::new()).unwrap_or_default()
    }

    /// Smudge with each of `dabs`, and their mirror copies, in turn.
    fn lay(&mut self, dabs: Vec<crate::brush_engine::stroke::CollectedDab>) {
        let pressure = self.dabber.as_ref().map_or(1.0, |d| d.pressure);
        let pool = Arc::clone(&self.pool);
        pool.install(|| {
            for c in dabs {
                let stroke = &self.stroke;
                let positions = stroke.symmetry.positions(&stroke.copies, c.dab.center);
                let turns: Vec<Option<[f32; 4]>> = (positions.iter())
                    .map(|&(copy, _)| (copy > 0).then(|| stroke.copies[copy - 1].tip_orientation()))
                    .collect();
                let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
                for ((copy, p), turn) in positions.into_iter().zip(turns) {
                    let p = if self.wrap {
                        Vec2::new(p.x.rem_euclid(w), p.y.rem_euclid(h))
                    } else {
                        p
                    };
                    let mut dab = c.dab;
                    dab.center = p;
                    if let Some(m) = turn {
                        dab.orient = crate::brush_engine::dynamics::compose(dab.orient, m);
                    }
                    let placed = Placed {
                        dab,
                        strength: c.strength,
                    };
                    self.blend_dab(p, pressure, copy, Some(&placed));
                }
            }
        });
    }

    /// Post-correction: put the layer back as it was and smudge the
    /// stroke again along its path smoothed (the same undo step).
    fn correct(&mut self) {
        let Some(d) = self.dabber.as_mut() else {
            return;
        };
        let Some(samples) = d.samples.take() else {
            return;
        };
        if samples.len() < 3 {
            return;
        }
        let (first, first_pressure) = d.first;
        let points: Vec<Vec2> = std::iter::once(first)
            .chain(samples.iter().map(|s| s.pos))
            .collect();
        let modes = d.brush.stabilizer_modes;
        let smoothed = crate::brush_engine::stabilizer::smooth_path(
            &points,
            modes.correction,
            d.stroke.view_scale,
        );
        let view_scale = d.stroke.view_scale;
        d.stroke = crate::brush_engine::stroke::StrokeState::with_seed(d.seed);
        d.stroke.view_scale = view_scale;
        let ts = self.canvas.tile_size();
        for (&(tx, ty), pixels) in &self.stroke.before {
            let rect = (tx * ts as i32, ty * ts as i32, ts, ts);
            self.canvas.write_layer_region(self.idx, rect, pixels, None);
            self.damage
                .push([rect.0, rect.1, rect.0 + ts as i32, rect.1 + ts as i32]);
        }
        // The lightness map as it was too (what it held is kept, for the
        // step).
        if let Some(map) = self
            .canvas
            .layers
            .get(self.idx)
            .and_then(|l| l.height.as_deref())
        {
            for (key, tile) in &self.stroke.lightness_before {
                map.set_tile(*key, tile.clone());
                let (x, y) = (key.0 * ts as i32, key.1 * ts as i32);
                self.damage.push([x, y, x + ts as i32, y + ts as i32]);
            }
        }
        self.stroke.carries.clear();
        self.stroke.krita_last.clear();
        self.stroke.random = 0x9e37_79b9;
        // The first dab, as `start` placed it, then the rest.
        let mut smoothed = smoothed.into_iter();
        let start = smoothed.next().unwrap_or(first);
        if let Some(d) = self.dabber.as_mut() {
            d.pressure = first_pressure;
        }
        let dabs = self.place(|stroke, brush, context| {
            stroke.add_sample(brush, start, first_pressure, None, context)
        });
        self.lay(dabs);
        for (s, pos) in samples.into_iter().zip(smoothed) {
            if let Some(d) = self.dabber.as_mut() {
                d.pressure = s.pressure;
            }
            let dabs = self.place(|stroke, brush, context| {
                stroke.tilt = s.tilt;
                stroke.barrel = s.barrel;
                stroke.add_sample(brush, pos, s.pressure, Some(s.time), context)
            });
            self.lay(dabs);
        }
    }

    /// A placed dab's tip (turned, squashed, any tip the brush has) with its
    /// texture and the selection, over the `side`×`side` patch at `origin`:
    /// how much of the result each pixel takes.
    fn placed_mask(&self, placed: &Placed, (x0, y0): (i32, i32), side: usize) -> Vec<f32> {
        let mut local = placed.dab;
        // In the patch's own frame (the tip's rows take whole pixels).
        local.center -= Vec2::new(x0 as f32, y0 as f32);
        local.strength = 1.0;
        let tip =
            crate::brush_engine::brush::SoftTip::new(&self.brush, std::slice::from_ref(&local));
        let strength = placed.strength.clamp(0.0, 1.0);
        let texture = self.brush.texture.as_ref();
        let grain = self.dabber.as_ref().map(|d| d.tiles.grain);
        let placing = texture.is_some_and(|t| t.placement.is_active());
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let wrap = self.wrap;
        let selection = self.selection.as_ref();
        let mut mask = vec![0.0f32; side * side];
        let round = tip.plain_round(&local);
        let r = local.r.max(0.5);
        for_rows(&self.pool, &mut mask, side, |ly, row| {
            match round {
                // The Gaussian falloff straight (no per-pixel tip work).
                Some(hardness) => {
                    let dy = ly as f32 + 0.5 - local.center.y;
                    for (lx, v) in row.iter_mut().enumerate() {
                        let dx = lx as f32 + 0.5 - local.center.x;
                        let t = (dx * dx + dy * dy).sqrt() / r;
                        *v = if t < 1.0 {
                            crate::brush_engine::masks::gaussian_falloff(t, hardness)
                        } else {
                            0.0
                        };
                    }
                }
                None => {
                    tip.row(&local, ly, 0, row, 1.0);
                }
            }
            let y = y0 + ly as i32;
            if let Some(t) = texture {
                // In canvas pixels (round the edges with wrap-around).
                for (sx, dx, w) in wrap_pieces_or_clip(x0, side, cw, wrap) {
                    let Some(ty) = canvas_row(y, ch, wrap) else {
                        break;
                    };
                    let part = &mut row[dx..dx + w];
                    match grain.filter(|_| placing) {
                        Some(grain) => t.apply_row_placed(
                            ty,
                            sx as usize,
                            part,
                            placed.dab.texture,
                            &grain,
                            [placed.dab.center.x, placed.dab.center.y],
                        ),
                        None => t.apply_row_scaled(ty, sx as usize, part, placed.dab.texture),
                    }
                }
            }
            for v in row.iter_mut() {
                *v *= strength;
            }
            if let Some(selection) = selection {
                let mut sel = vec![0.0f32; side];
                if let Some(ty) = canvas_row(y, ch, wrap) {
                    for (sx, dx, w) in wrap_pieces_or_clip(x0, side, cw, wrap) {
                        selection.row_coverage(ty, sx as usize, &mut sel[dx..dx + w]);
                    }
                }
                for (v, s) in row.iter_mut().zip(&sel) {
                    *v *= s;
                }
            }
        });
        mask
    }

    fn blend_diameter(&self, pressure: f32) -> f32 {
        let o = &self.brush.brush_options;
        let k = if o.pressure_size {
            o.pressure_min_size + (1.0 - o.pressure_min_size) * o.pressure_curves.size(pressure)
        } else {
            1.0
        };
        (o.diameter * k).max(1.0)
    }

    /// A dab at `center` and its mirror copies.
    fn blend_mirrored(&mut self, center: Vec2, pressure: f32) {
        let stroke = &self.stroke;
        let positions = stroke.symmetry.positions(&stroke.copies, center);
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        for (copy, p) in positions {
            // Wrap-around: the dab put on the canvas (its patch then wraps
            // round the edges).
            let p = if self.wrap {
                Vec2::new(p.x.rem_euclid(w), p.y.rem_euclid(h))
            } else {
                p
            };
            self.blend_dab(p, pressure, copy, None);
        }
    }

    /// One dab at `center`, for mirror copy `copy`: as the brush placed
    /// it (`placed`: a smudge), else round, the brush's size at `pressure`.
    fn blend_dab(&mut self, center: Vec2, pressure: f32, copy: usize, placed: Option<&Placed>) {
        if self.stroke.mix.krita.is_some() {
            if let Some(placed) = placed {
                self.krita_smudge_dab(center, pressure, copy, placed);
            }
            return;
        }
        let diameter = placed.map_or_else(|| self.blend_diameter(pressure), |p| p.dab.r * 2.0);
        let o = &self.brush.brush_options;
        let r = diameter * 0.5;
        let mut strength = o.flow / 100.0 * o.opacity;
        if o.pressure_opacity {
            strength *= o.pressure_curves.opacity(pressure);
        }
        if o.pressure_flow {
            strength *= o.pressure_curves.flow(pressure);
        }
        let hardness = (o.hardness / 100.0).clamp(0.0, 1.0);
        let blur_size = self.blur_size;
        // The brush colour as carried paint (linear, premultiplied, opaque):
        // the dab's own when its colour varies (inputs, colour source).
        let brush_paint = match placed {
            // (The dab's colour is in the document's blend space.)
            Some(p) if self.brush.varies_color() => {
                if self.canvas.blend_space == crate::canvas::blend_modes::BlendSpace::Linear {
                    let [r, g, b] = p.dab.color;
                    [r, g, b, 1.0]
                } else {
                    let [r, g, b] = p
                        .dab
                        .color
                        .map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
                    to_f(Color32::from_rgb(r, g, b))
                }
            }
            _ => to_f(Color32::from_rgb(o.color.r(), o.color.g(), o.color.b())),
        };
        let spacing = o.spacing;
        let idx = self.idx;
        let alpha_lock = self.canvas.layers[idx].alpha_locked;
        let rc = placed.map_or(r, |p| p.dab.reach).ceil() as i32;
        let side = (2 * rc + 1) as usize;
        let (x0, y0) = (center.x.floor() as i32 - rc, center.y.floor() as i32 - rc);
        let pool = Arc::clone(&self.pool);
        let codec = Codec::new();
        let mask = match placed {
            Some(p) => self.placed_mask(p, (x0, y0), side),
            // A round tip, shaped like the brush's (hardness falloff),
            // times the stroke strength and the selection. (A big dab's
            // rows, and its other per-pixel work below, run in parallel.)
            None => {
                let mut mask = vec![0.0f32; side * side];
                let selection = self.selection.as_ref();
                let r_mask = r.max(0.5);
                for_rows(&pool, &mut mask, side, |ly, mask_row| {
                    let y = y0 + ly as i32;
                    let mut row_sel = vec![1.0f32; side];
                    if let Some(selection) = selection {
                        if y < 0 {
                            return;
                        }
                        let start = x0.max(0);
                        row_sel.fill(0.0);
                        let skip = (start - x0) as usize;
                        if skip < side {
                            selection.row_coverage(
                                y as usize,
                                start as usize,
                                &mut row_sel[skip..],
                            );
                        }
                    }
                    let dy = y as f32 + 0.5 - center.y;
                    for lx in 0..side {
                        let dx = (x0 + lx as i32) as f32 + 0.5 - center.x;
                        let t = (dx * dx + dy * dy).sqrt() / r_mask;
                        if t < 1.0 {
                            mask_row[lx] =
                                crate::brush_engine::masks::gaussian_falloff(t, hardness)
                                    * strength
                                    * row_sel[lx];
                        }
                    }
                });
                mask
            }
        };
        // A mixing brush's inputs scale its smudge length and colour rate.
        let (by_length, by_rate) = placed.map_or((1.0, 1.0), |p| (p.dab.smudge, p.dab.color_rate));
        let stroke = &mut self.stroke;
        let mix = &stroke.mix;
        let by_pressure = |on: bool| if on { pressure.clamp(0.0, 1.0) } else { 1.0 };
        let length = (mix.length * by_pressure(mix.pressure_length) * by_length).clamp(0.0, 1.0);
        // Brush colour added per brush width travelled, whatever the
        // spacing: per dab, the share that compounds to it over one width.
        let steps_per_width = (100.0 / spacing.max(1.0)).max(1.0);
        let rate = (mix.color_rate * by_pressure(mix.pressure_color) * by_rate).clamp(0.0, 1.0);
        let color_rate = 1.0 - (1.0 - rate).powf(1.0 / steps_per_width);
        let paint_blend = mix.blend;

        let wrap = self.wrap;
        // The pixels as stored (kept exactly where the dab doesn't reach),
        // and as linear paint.
        let stored = read_patch(&self.canvas, Some(idx), (x0, y0), side, side, wrap);
        let mut under = vec![[0.0f32; 4]; side * side];
        for_rows(&pool, &mut under, side, |row, line| {
            for (u, &c) in line.iter_mut().zip(&stored[row * side..]) {
                *u = codec.to_f(c);
            }
        });
        // Deform moves the pixels themselves: each takes its colour from
        // where the displacement says, at full weight (the mask sets how
        // far it moves).
        let mut weights_are_mask = true;
        let target: Vec<[f32; 4]> = if let BlendKind::Deform(mode, amount) = stroke.kind {
            weights_are_mask = false;
            let dir = if copy == 0 {
                stroke.dir
            } else {
                let s = &stroke.symmetry;
                s.map(&stroke.copies[copy - 1], s.center + stroke.dir) - s.center
            };
            let step = self.brush.brush_options.spacing_px(diameter, 1.0).max(1.0);
            let margin = (r * amount * 0.6).max(step).ceil() as i32 + 2;
            let big_side = side + 2 * margin as usize;
            let big: Vec<[f32; 4]> = read_patch(
                &self.canvas,
                Some(idx),
                (x0 - margin, y0 - margin),
                big_side,
                big_side,
                wrap,
            )
            .into_iter()
            .map(|c| codec.to_f(c))
            .collect();
            let local_center = center - Vec2::new(x0 as f32, y0 as f32);
            let moved = |i: usize| {
                let m = mask[i];
                if m <= 0.0 {
                    return under[i];
                }
                let p = Vec2::new((i % side) as f32 + 0.5, (i / side) as f32 + 0.5);
                let off = p - local_center;
                let src = match mode {
                    // As far as the brush moved since the last dab: at
                    // full amount the paint keeps up with the brush.
                    DeformMode::Push => p - dir * (amount * m * step),
                    DeformMode::Grow => local_center + off * (1.0 - amount * m * 0.5),
                    DeformMode::Shrink => local_center + off * (1.0 + amount * m * 0.5),
                    DeformMode::SwirlLeft | DeformMode::SwirlRight => {
                        let turn = amount * m * 0.8;
                        let a = if mode == DeformMode::SwirlLeft {
                            turn
                        } else {
                            -turn
                        };
                        let (s, c) = a.sin_cos();
                        local_center + Vec2::new(c * off.x + s * off.y, -s * off.x + c * off.y)
                    }
                };
                sample_bilinear(&big, big_side, src + Vec2::splat(margin as f32))
            };
            let mut target = vec![[0.0f32; 4]; side * side];
            for_rows(&pool, &mut target, side, |row, line| {
                for (lx, t) in line.iter_mut().enumerate() {
                    *t = moved(row * side + lx);
                }
            });
            target
        } else if let BlendKind::Clone { offset, merged } = stroke.kind {
            let source = if merged { None } else { Some(idx) };
            read_patch(
                &self.canvas,
                source,
                (x0 + offset.0, y0 + offset.1),
                side,
                side,
                wrap,
            )
            .into_iter()
            .map(|c| codec.to_f(c))
            .collect()
        } else if let BlendKind::Sharpen(amount) = stroke.kind {
            let radius = ((r * blur_size).round() as usize).max(1);
            let soft = box_blur(&pool, &under, side, radius);
            under
                .iter()
                .zip(&soft)
                .map(|(u, b)| {
                    let a = u[3];
                    let mut v = *u;
                    for k in 0..3 {
                        v[k] = (u[k] + (u[k] - b[k]) * amount).clamp(0.0, a);
                    }
                    v
                })
                .collect()
        } else if let BlendKind::Filter(filter) = stroke.kind {
            // The filtered layer laid down through the brush, its coverage
            // building up like paint: going over a spot again never
            // filters it twice.
            weights_are_mask = false;
            let ts = self.canvas.tile_size() as i32;
            let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
            let mut target = under.clone();
            for (i, &m) in mask.iter().enumerate() {
                if m <= 0.0 {
                    continue;
                }
                let (mut x, mut y) = (x0 + (i % side) as i32, y0 + (i / side) as i32);
                if wrap {
                    (x, y) = (x.rem_euclid(cw), y.rem_euclid(ch));
                } else if x < 0 || y < 0 || x >= cw || y >= ch {
                    continue;
                }
                let key = (x.div_euclid(ts), y.div_euclid(ts));
                let at = ((y - key.1 * ts) * ts + (x - key.0 * ts)) as usize;
                if !stroke.filtered.contains_key(&key) {
                    let tile = filter_tile(&self.canvas, idx, &stroke.before, key, filter, wrap);
                    stroke.filtered.insert(key, tile);
                }
                let filtered = to_f(stroke.filtered[&key][at]);
                let original = stroke.before.get(&key).map_or(under[i], |t| to_f(t[at]));
                let cov = &mut stroke
                    .coverage
                    .entry(key)
                    .or_insert_with(|| vec![0.0; (ts * ts) as usize])[at];
                *cov += m * (1.0 - *cov);
                let c = *cov;
                target[i] = std::array::from_fn(|k| original[k] + (filtered[k] - original[k]) * c);
            }
            target
        } else if let BlendKind::Adjust(hsv) = stroke.kind {
            under.iter().map(|&u| adjust_hsv(u, hsv)).collect()
        } else if stroke.kind == BlendKind::Smudge {
            // The carried paint, resized if pressure changed the tip size.
            if stroke.carries.len() <= copy {
                stroke.carries.resize_with(copy + 1, || None);
            }
            let carry = match stroke.carries[copy].take() {
                Some(c) if c.side == side => c,
                Some(c) => Carry {
                    side,
                    px: (0..side * side)
                        .map(|i| {
                            let (x, y) = (i % side * c.side / side, i / side * c.side / side);
                            c.px[y * c.side + x]
                        })
                        .collect(),
                },
                None => Carry {
                    side,
                    px: under.clone(),
                },
            };
            let mut carry = carry;
            if color_rate > 0.0 {
                // Wet paint: the brush picks up the
                // paint under it (keeping the smudge length's share of what
                // it carried), then adds its own colour.
                for (c, u) in carry.px.iter_mut().zip(&under) {
                    for k in 0..4 {
                        c[k] = u[k] + (c[k] - u[k]) * length;
                        c[k] += (brush_paint[k] - c[k]) * color_rate;
                    }
                }
            }
            let px = carry.px.clone();
            stroke.carries[copy] = Some(carry);
            px
        } else {
            let radius = ((r * blur_size).round() as usize).max(1);
            box_blur(&pool, &under, side, radius)
        };

        let mut result = stored.clone();
        let changed = std::sync::atomic::AtomicBool::new(false);
        for_rows(&pool, &mut result, side, |row, line| {
            let mut row_changed = false;
            for (lx, out) in line.iter_mut().enumerate() {
                let i = row * side + lx;
                // Outside the dab: the stored pixel as it is (no round
                // trip through linear light, and nothing to work out).
                if mask[i] <= 0.0 {
                    continue;
                }
                let (u, t) = (under[i], target[i]);
                let m = if weights_are_mask { mask[i] } else { 1.0 };
                // A mixing brush's blend mode: its paint over what's there.
                let t = if paint_blend == crate::canvas::blend_modes::LayerBlend::Normal {
                    t
                } else {
                    let rgba = |v: [f32; 4]| {
                        eframe::egui::Rgba::from_rgba_premultiplied(v[0], v[1], v[2], v[3])
                    };
                    crate::canvas::blend_modes::composite(paint_blend, rgba(t), rgba(u), 0.0)
                        .to_array()
                };
                let mut v = [0.0; 4];
                for c in 0..4 {
                    v[c] = u[c] + (t[c] - u[c]) * m;
                }
                let before = *out;
                let mut new = codec.to_c(v);
                if alpha_lock {
                    new = crate::canvas::blend::with_alpha_of(new, before.a());
                }
                row_changed |= new != before;
                *out = new;
            }
            if row_changed {
                changed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        // Smudge picks up the blended paint for the next dab (a wet brush
        // picked up the paint under it before painting).
        if color_rate <= 0.0
            && let Some(carry) = stroke.carries.get_mut(copy).and_then(|c| c.as_mut())
        {
            let result = &result;
            for_rows(&pool, &mut carry.px, side, |row, line| {
                for (c, &res) in line.iter_mut().zip(&result[row * side..]) {
                    let res = codec.to_f(res);
                    for k in 0..4 {
                        c[k] = res[k] + (c[k] - res[k]) * length;
                    }
                }
            });
        }
        if !changed.into_inner() {
            return;
        }
        if !wrap {
            self.canvas.write_layer_region(
                idx,
                (x0, y0, side, side),
                &result,
                Some(&mut stroke.before),
            );
            self.damage
                .push([x0, y0, x0 + side as i32, y0 + side as i32]);
            return;
        }
        // Wrap-around: each piece of the patch where it lands on the canvas.
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let mut damage = Vec::new();
        for (sx, dx, w) in wrap_pieces(x0, side, cw) {
            for (sy, dy, h) in wrap_pieces(y0, side, ch) {
                let piece: Vec<Color32> = (0..h)
                    .flat_map(|row| {
                        let start = (dy + row) * side + dx;
                        result[start..start + w].iter().copied()
                    })
                    .collect();
                self.canvas.write_layer_region(
                    idx,
                    (sx, sy, w, h),
                    &piece,
                    Some(&mut stroke.before),
                );
                damage.push([sx, sy, sx + w as i32, sy + h as i32]);
            }
        }
        for rect in damage {
            self.damage.push(rect);
        }
    }
}

impl BlendSession {
    /// One dab of an imported colour smudge: the layer under the dab, with the layer where the last
    /// dab was laid over it (smearing; in dulling mode one colour sampled
    /// there), at the smudge rate × opacity; then the brush colour at the
    /// colour rate² × opacity (by the brush's blend mode); the result put
    /// down through the tip. The stroke's first dab only says where it is.
    fn krita_smudge_dab(&mut self, center: Vec2, pressure: f32, copy: usize, placed: &Placed) {
        let brush = &self.brush;
        let o = &brush.brush_options;
        let r = placed.dab.r;
        let p = pressure.clamp(0.0, 1.0);
        // The dab's strength: opacity and flow, by pressure as the brush
        // says, and its inputs.
        let opacity = placed.strength.clamp(0.0, 1.0);
        let Some(m) = brush.mixing else {
            return;
        };
        let Some(k) = m.krita else {
            return;
        };
        let by = |on: bool| if on { p } else { 1.0 };
        let rate = m.smudge_length * by(m.pressure_length) * placed.dab.smudge;
        let color_rate = m.color_rate * by(m.pressure_color) * placed.dab.color_rate;
        let smear = if k.dulling { 0.8 } else { 1.0 } * rate * opacity;
        let colour = color_rate * color_rate * opacity;
        let paint = to_f(Color32::from_rgb(o.color.r(), o.color.g(), o.color.b()));
        // The colour tables looked up once, not once a pixel.
        let codec = Codec::new();
        let paint_blend = brush.paint_blend;
        let wrap = self.wrap;
        let idx = self.idx;
        let stroke = &mut self.stroke;
        if stroke.krita_last.len() <= copy {
            stroke.krita_last.resize(copy + 1, None);
        }
        let Some(last) = stroke.krita_last[copy].replace(center) else {
            return;
        };
        let alpha_lock = self.canvas.layers[idx].alpha_locked;
        let rc = placed.dab.reach.ceil() as i32;
        let side = (2 * rc + 1) as usize;
        let (x0, y0) = (center.x.floor() as i32 - rc, center.y.floor() as i32 - rc);
        // The tip (turned, textured, selected): how much of the result
        // each pixel takes.
        let shape = Placed {
            dab: placed.dab,
            strength: 1.0,
        };
        let mask = self.placed_mask(&shape, (x0, y0), side);
        let pool = Arc::clone(&self.pool);
        // Each row's part under the tip: the rest is left as it is.
        let spans: Vec<(usize, usize)> = mask
            .chunks(side)
            .map(|row| {
                let first = row.iter().position(|&m| m > 0.0).unwrap_or(side);
                let last = row.iter().rposition(|&m| m > 0.0).map_or(first, |i| i + 1);
                (first, last)
            })
            .collect();

        let stored = read_patch(&self.canvas, Some(idx), (x0, y0), side, side, wrap);
        // Where the last dab was, in whole pixels (read aligned).
        let shift = (last - center).round();
        let (sx, sy) = (x0 + shift.x as i32, y0 + shift.y as i32);
        let source = read_patch(&self.canvas, Some(idx), (sx, sy), side, side, wrap);
        // Dulling: one colour, the weighted average (by the tip and the
        // paint's coverage, premultiplied) of the middle of the source, out
        // to the smudge radius, widened while that holds no paint.
        let dulled = k.dulling.then(|| {
            let mut radius = k.radius;
            loop {
                if let Some(c) = halton_dull(&source, &mask, side, (r * radius).max(0.5), codec) {
                    break c;
                }
                if radius >= 1.0 {
                    break [0.0; 4];
                }
                radius = (radius + 0.05).min(1.0);
            }
        });

        // One pixel under the tip: the picked-up paint over the layer,
        // then the brush colour, put down through the tip.
        let smudge_pixel = |i: usize| -> Color32 {
            let u = codec.to_f(stored[i]);
            let s = dulled.unwrap_or_else(|| codec.to_f(source[i]));
            // The picked-up paint over the layer (copied, alpha too, with
            // smear alpha).
            let mut v: [f32; 4] = if k.smear_alpha {
                std::array::from_fn(|c| u[c] + (s[c] - u[c]) * smear)
            } else {
                std::array::from_fn(|c| s[c] * smear + u[c] * (1.0 - s[3] * smear))
            };
            // Then the brush colour.
            if colour > 0.0 {
                v = if paint_blend == crate::canvas::blend_modes::LayerBlend::Normal {
                    std::array::from_fn(|c| paint[c] * colour + v[c] * (1.0 - colour))
                } else {
                    let rgba = |x: [f32; 4]| {
                        eframe::egui::Rgba::from_rgba_premultiplied(x[0], x[1], x[2], x[3])
                    };
                    let src = paint.map(|c| c * colour);
                    crate::canvas::blend_modes::composite(paint_blend, rgba(src), rgba(v), 0.0)
                        .to_array()
                };
            }
            let m = mask[i].min(1.0);
            let mut out = codec.to_c(std::array::from_fn(|c| u[c] + (v[c] - u[c]) * m));
            if alpha_lock {
                out = crate::canvas::blend::with_alpha_of(out, stored[i].a());
            }
            out
        };

        let mut result = stored.clone();
        let changed = pool.install(|| {
            result
                .par_chunks_mut(side)
                .enumerate()
                .map(|(ly, row)| {
                    let (first, last) = spans[ly];
                    let mut changed = false;
                    for (lx, px) in row.iter_mut().enumerate().take(last).skip(first) {
                        let i = ly * side + lx;
                        if mask[i] <= 0.0 {
                            continue;
                        }
                        let out = smudge_pixel(i);
                        changed |= out != *px;
                        *px = out;
                    }
                    changed
                })
                .reduce(|| false, |a, b| a || b)
        });
        if changed {
            self.write_blend_patch(idx, (x0, y0), side, &result, wrap);
        }
        // A lightness tip: its grey on the lightness map, as much as the
        // paint thickness says (overwriting: at the dab's opacity; else as
        // much more as the smudge length leaves).
        if self.brush.lays_lightness() {
            let thickness = (k.thickness * placed.dab.thickness).clamp(0.0, 1.0);
            let share = if k.overwrite {
                1.0
            } else {
                (rate - 0.01) + (1.0 - (rate - 0.01)) * thickness
            };
            self.lay_lightness(placed, (x0, y0), side, opacity * share, thickness);
        }
    }

    /// The dab's tip grey (pulled toward mid grey at less than full paint
    /// `thickness`) over the layer's lightness map, at `strength` times
    /// the tip's coverage (and the selection's), as Krita lays its colour
    /// smudge's heightmap.
    fn lay_lightness(
        &mut self,
        placed: &Placed,
        (x0, y0): (i32, i32),
        side: usize,
        strength: f32,
        thickness: f32,
    ) {
        let tips = self.brush.brush_options.tip_shapes();
        let d = placed.dab;
        let Some(PixelBrushShape::Custom(tip)) = tips.get(d.tip as usize) else {
            return;
        };
        let Some(map) = self
            .canvas
            .layers
            .get(self.idx)
            .and_then(|l| l.height.as_deref())
        else {
            return;
        };
        if strength <= 0.0 {
            return;
        }
        let sampler = tip.sampler(d.r);
        let [a, b, c, e] = d.orient;
        let ts = self.canvas.tile_size() as i32;
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let wrap = self.wrap;
        // Per tile: each pixel's index, grey and coverage laid.
        type Laid = Vec<(usize, f32, f32)>;
        let mut edits: HashMap<(i32, i32), Laid> = HashMap::new();
        let (mut cover, mut colours, mut sel) = (
            vec![0.0f32; side],
            vec![[0.0f32; 3]; side],
            vec![1.0f32; side],
        );
        for ly in 0..side {
            let gy = y0 + ly as i32;
            let Some(y) = canvas_row(gy, ch, wrap) else {
                continue;
            };
            let (pdx, pdy) = (x0 as f32 + 0.5 - d.center.x, gy as f32 + 0.5 - d.center.y);
            let start = (a * pdx + b * pdy, c * pdx + e * pdy);
            tip.row(&sampler, start, (a, c), &mut cover);
            if cover.iter().all(|&v| v <= 0.0) {
                continue;
            }
            tip.color_row(&sampler, start, (a, c), &cover, 1.0, &mut colours);
            if let Some(selection) = &self.selection {
                sel.fill(0.0);
                for (sx, dx, w) in wrap_pieces_or_clip(x0, side, cw, wrap) {
                    selection.row_coverage(y, sx as usize, &mut sel[dx..dx + w]);
                }
            }
            for (sx, dx, w) in wrap_pieces_or_clip(x0, side, cw, wrap) {
                for k in 0..w {
                    let lx = dx + k;
                    let coverage = cover[lx].min(1.0) * sel[lx] * strength;
                    if coverage <= 0.0 {
                        continue;
                    }
                    let [r, g, bl] = colours[lx];
                    let grey = 0.299 * r + 0.587 * g + 0.114 * bl;
                    let grey = (grey - 0.5) * thickness + 0.5;
                    let (x, y) = (sx + k as i32, y as i32);
                    let key = (x.div_euclid(ts), y.div_euclid(ts));
                    let i = (y.rem_euclid(ts) * ts + x.rem_euclid(ts)) as usize;
                    edits.entry(key).or_default().push((i, grey, coverage));
                }
            }
        }
        let before = &mut self.stroke.lightness_before;
        for (key, list) in edits {
            before.entry(key).or_insert_with(|| map.tile(key));
            map.edit_tile(key, ts as usize, |t| {
                for (i, grey, coverage) in list {
                    t[i] = crate::canvas::impasto::lay_lightness(t[i], grey, coverage);
                }
            });
            let (x, y) = (key.0 * ts, key.1 * ts);
            self.damage.push([x, y, x + ts, y + ts]);
        }
    }

    /// Put a blend stroke's `side`² `result` on layer `idx` at `origin`
    /// (round the edges with wrap-around), keeping the tiles' pixels from
    /// before the stroke for its undo.
    fn write_blend_patch(
        &mut self,
        idx: usize,
        (x0, y0): (i32, i32),
        side: usize,
        result: &[Color32],
        wrap: bool,
    ) {
        let stroke = &mut self.stroke;
        if !wrap {
            self.canvas.write_layer_region(
                idx,
                (x0, y0, side, side),
                result,
                Some(&mut stroke.before),
            );
            self.damage
                .push([x0, y0, x0 + side as i32, y0 + side as i32]);
            return;
        }
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let mut damage = Vec::new();
        for (sx, dx, w) in wrap_pieces(x0, side, cw) {
            for (sy, dy, h) in wrap_pieces(y0, side, ch) {
                let piece: Vec<Color32> = (0..h)
                    .flat_map(|row| {
                        let start = (dy + row) * side + dx;
                        result[start..start + w].iter().copied()
                    })
                    .collect();
                self.canvas.write_layer_region(
                    idx,
                    (sx, sy, w, h),
                    &piece,
                    Some(&mut stroke.before),
                );
                damage.push([sx, sy, sx + w as i32, sy + h as i32]);
            }
        }
        for rect in damage {
            self.damage.push(rect);
        }
    }
}

/// Tile `key` of layer `idx` as it was before the stroke (`before` holds
/// the tiles it changed), through `filter`: read with the margin the filter
/// reaches into, so its edges come out as they would filtering the layer.
fn filter_tile(
    canvas: &crate::canvas::Canvas,
    idx: usize,
    before: &HashMap<(i32, i32), Vec<Color32>>,
    key: (i32, i32),
    filter: crate::canvas::filters::Filter,
    wrap: bool,
) -> Vec<Color32> {
    let ts = canvas.tile_size() as i32;
    let reach = filter.reach().max(0);
    let side = (ts + 2 * reach) as usize;
    let origin = (key.0 * ts - reach, key.1 * ts - reach);
    let mut src = read_patch(canvas, Some(idx), origin, side, side, wrap);
    if !before.is_empty() {
        let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
        // The last tile looked up, and its pixels before the stroke.
        let mut last_key = None;
        let mut last_tile = None;
        for (i, px) in src.iter_mut().enumerate() {
            let (mut x, mut y) = (origin.0 + (i % side) as i32, origin.1 + (i / side) as i32);
            if wrap {
                (x, y) = (x.rem_euclid(cw), y.rem_euclid(ch));
            } else if x < 0 || y < 0 || x >= cw || y >= ch {
                continue;
            }
            let k = (x.div_euclid(ts), y.div_euclid(ts));
            if last_key != Some(k) {
                last_key = Some(k);
                last_tile = before.get(&k);
            }
            let tile = last_tile;
            if let Some(t) = tile {
                *px = t[((y - k.1 * ts) * ts + (x - k.0 * ts)) as usize];
            }
        }
    }
    let out = filter
        .fitted(canvas.width(), canvas.height())
        .apply(&src, side, side, origin);
    let (r, t) = (reach as usize, ts as usize);
    (0..t)
        .flat_map(|row| {
            out[(row + r) * side + r..(row + r) * side + r + t]
                .iter()
                .copied()
        })
        .collect()
}

/// Clone: a cross where it copies from (following the brush during a
/// stroke).
pub(crate) fn draw_clone_source(
    app: &PainterApp,
    painter: &eframe::egui::Painter,
    to_screen: &dyn Fn(Vec2) -> eframe::egui::Pos2,
) {
    use eframe::egui::{Stroke, vec2};
    let b = &app.workspace.blend;
    if !matches!(app.active_tool, Tool::Smudge) || b.smudge_mode != SmudgeMode::Clone {
        return;
    }
    let at = match (&app.brush_state.blend_stroke, app.viewport.cursor_canvas) {
        (
            Some(ActiveBlend {
                kind: BlendKind::Clone { offset, .. },
                ..
            }),
            Some(cursor),
        ) => cursor + Vec2::new(offset.0 as f32, offset.1 as f32),
        _ => match b.clone_source {
            Some(source) => source,
            None => return,
        },
    };
    let c = to_screen(at);
    for (width, color) in [
        (3.0_f32, Color32::from_black_alpha(160)),
        (1.0_f32, Color32::WHITE),
    ] {
        let stroke = Stroke::new(width, color);
        painter.line_segment([c - vec2(8.0, 0.0), c + vec2(8.0, 0.0)], stroke);
        painter.line_segment([c - vec2(0.0, 8.0), c + vec2(0.0, 8.0)], stroke);
        painter.circle_stroke(c, 5.0, stroke);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod mix_tests;

#[cfg(test)]
mod mode_tests;
