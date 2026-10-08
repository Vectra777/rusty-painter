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
//! Smudge carries a patch of paint along the stroke: every dab picks up
//! the paint under the tip (how much of what it carried it keeps is the
//! smudge length) and mixes that into the canvas. With a colour rate it is
//! a wet mixing brush: each dab also mixes that much of the brush colour
//! into the carried paint, so it lays down the brush colour blended with
//! whatever it drags. Blur mixes each pixel toward the average around it. Both are sequential per dab (each dab sees
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
    /// The space the paint is mixed in.
    codec: Codec,
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
    /// In a deeper document, the same tiles at full depth.
    before_deep: HashMap<(i32, i32), crate::canvas::storage::DeepTile>,
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
    /// The imported smudge: the whole pixel each mirror copy's last dab was
    /// on (it reads the layer there), and the stroke's random state (a
    /// turning tip).
    krita_last: Vec<Option<(i32, i32)>>,
    random: u32,
    /// Krita's colour smudge with a lightness tip: the layer's lightness
    /// map tiles as they were before the stroke first changed them.
    lightness_before: HashMap<(i32, i32), Option<Vec<u16>>>,
    /// An 8-bit document: the tiles the stroke has painted, as the paint
    /// it mixed (in the codec's space) before it was rounded to 8 bits
    /// (an alpha below 0 where it hasn't painted).
    /// The stroke reads them back from here, as Krita paints a stroke on a
    /// 16-bit copy of the layer: a faint dab builds up rather than each
    /// rounding away.
    precise: HashMap<(i32, i32), Vec<[f32; 4]>>,
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

/// The paint a blend stroke mixes: premultiplied colour, 0..1 a channel,
/// in the document's blend space. Linear light by default, the space the
/// compositor works in (mixing the stored sRGB bytes and clamping each
/// channel to alpha darkened light colours at soft edges, as if another
/// colour were being picked up); in a document that blends in gamma space,
/// the stored sRGB values premultiplied, as Krita mixes an sRGB layer.
/// Its tables are looked up once, for the loops over a dab's pixels (once
/// a pixel, the lookups cost more than the conversion).
#[derive(Clone, Copy)]
struct Codec {
    decoder: crate::canvas::blend::LinearDecoder,
    encoder: crate::canvas::blend::LinearEncoder,
    /// Gamma space: how its stored pixels are read.
    gamma: Option<crate::canvas::blend::GammaReader>,
}

impl Codec {
    /// Linear light.
    #[inline]
    fn new() -> Self {
        Self {
            decoder: crate::canvas::blend::LinearDecoder::new(),
            encoder: crate::canvas::blend::LinearEncoder::new(),
            gamma: None,
        }
    }

    /// The space `canvas` blends in.
    fn for_canvas(canvas: &crate::canvas::Canvas) -> Self {
        let gamma = canvas.blend_space == crate::canvas::blend_modes::BlendSpace::Gamma;
        Self {
            gamma: gamma.then(crate::canvas::blend::GammaReader::new),
            ..Self::new()
        }
    }

    #[inline]
    fn to_f(self, c: Color32) -> [f32; 4] {
        match self.gamma {
            Some(reader) => reader.read(c).to_array(),
            None => self.decoder.decode(c).to_array(),
        }
    }

    #[inline]
    fn to_c(self, v: [f32; 4]) -> Color32 {
        match self.gamma {
            Some(_) => {
                let a = v[3].clamp(0.0, 1.0);
                let c = |x: f32| x.clamp(0.0, a);
                crate::canvas::blend::gamma_rgba_to_color32(
                    eframe::egui::Rgba::from_rgba_premultiplied(c(v[0]), c(v[1]), c(v[2]), a),
                )
            }
            None => encode(self.encoder, v),
        }
    }

    /// Linear premultiplied paint (a deeper layer's) in this space.
    #[inline]
    fn of_linear(self, v: [f32; 4]) -> [f32; 4] {
        match self.gamma {
            Some(_) => recurve(v, eframe::egui::ecolor::gamma_from_linear),
            None => v,
        }
    }

    /// Paint in this space as linear premultiplied paint.
    #[inline]
    fn to_linear(self, v: [f32; 4]) -> [f32; 4] {
        match self.gamma {
            Some(_) => recurve(v, eframe::egui::ecolor::linear_from_gamma),
            None => v,
        }
    }
}

/// Premultiplied `v` with `curve` on its unpremultiplied colour.
#[inline]
fn recurve(v: [f32; 4], curve: fn(f32) -> f32) -> [f32; 4] {
    let a = v[3];
    if a <= 0.0 {
        return [0.0; 4];
    }
    let c = |x: f32| curve((x / a).clamp(0.0, 1.0)) * a;
    [c(v[0]), c(v[1]), c(v[2]), a]
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
/// many pixels a task. (At a pen's pace the pool's threads sleep between
/// samples, and waking them for each part of a smaller dab cost more than
/// sharing it saved.)
const PARALLEL_SIDE: usize = 192;
const PARALLEL_PIXELS: usize = 32768;

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
    read_wrapped(canvas, (x0, y0), w, h, wrap, |x, y, w, h| {
        canvas.render_reference(source, x, y, w, h)
    })
}

/// Layer `idx` of a deeper document over the `w`×`h` canvas rectangle at
/// `origin`, at its full depth, as linear paint (premultiplied).
fn read_deep(
    canvas: &crate::canvas::Canvas,
    idx: usize,
    origin: (i32, i32),
    w: usize,
    h: usize,
    wrap: bool,
) -> Vec<[f32; 4]> {
    read_wrapped(canvas, origin, w, h, wrap, |x, y, w, h| {
        canvas.read_layer_linear(idx, (x, y, w, h))
    })
}

/// The `w`×`h` canvas rectangle at `origin` as `read(x, y, w, h)` reads
/// it; with `wrap`, read in pieces round the edges.
fn read_wrapped<T: Copy + Default>(
    canvas: &crate::canvas::Canvas,
    (x0, y0): (i32, i32),
    w: usize,
    h: usize,
    wrap: bool,
    read: impl Fn(i32, i32, usize, usize) -> Vec<T>,
) -> Vec<T> {
    if !wrap {
        return read(x0, y0, w, h);
    }
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    let mut out = vec![T::default(); w * h];
    for (sx, dx, pw) in wrap_pieces(x0, w, cw) {
        for (sy, dy, ph) in wrap_pieces(y0, h, ch) {
            let part = read(sx, sy, pw, ph);
            for row in 0..ph {
                let at = (dy + row) * w + dx;
                out[at..at + pw].copy_from_slice(&part[row * pw..(row + 1) * pw]);
            }
        }
    }
    out
}

/// The `w`×`h` block at `(dx, dy)` of a `side`-wide patch (the patch
/// itself when that's all of it).
fn patch_block<T: Copy>(
    patch: &[T],
    side: usize,
    (dx, dy, w, h): (usize, usize, usize, usize),
) -> std::borrow::Cow<'_, [T]> {
    if w == side && h * side == patch.len() {
        return std::borrow::Cow::Borrowed(patch);
    }
    (0..h)
        .flat_map(|row| {
            let start = (dy + row) * side + dx;
            patch[start..start + w].iter().copied()
        })
        .collect()
}

/// [`crate::canvas::blend::with_alpha_of`] for linear paint: `v` with its
/// alpha replaced by `a`, keeping its unpremultiplied colour.
fn with_alpha_linear(v: [f32; 4], a: f32) -> [f32; 4] {
    if a <= 0.0 {
        return [0.0; 4];
    }
    if v[3] <= 0.0 {
        return [0.0, 0.0, 0.0, a];
    }
    let k = a / v[3];
    [v[0] * k, v[1] * k, v[2] * k, a]
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

/// A pixel (premultiplied, in `codec`'s space) with its hue turned by
/// `hsv[0]` degrees and its saturation and brightness moved by `hsv[1]`,
/// `hsv[2]`.
fn adjust_hsv(codec: Codec, v: [f32; 4], hsv: [f32; 3]) -> [f32; 4] {
    let c = codec.to_c(v);
    if c.a() == 0 {
        return v;
    }
    let srgb = crate::brush_engine::dynamics::shift_hsv(c, hsv);
    let byte = |x: f32| (x * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    codec.to_f(Color32::from_rgba_unmultiplied(
        byte(srgb[0]),
        byte(srgb[1]),
        byte(srgb[2]),
        c.a(),
    ))
}

/// Box blur of a `side`×`side` patch with radius `r` (two passes: a
/// tent-shaped kernel, softer than one box). Each output pixel averages
/// what of its window lies inside the patch. Rows go across the pool, a big
/// patch's columns in bands of rows (a window of whole rows sliding down,
/// so memory is read in order).
#[cfg(test)]
fn box_blur(pool: &rayon::ThreadPool, src: &[[f32; 4]], side: usize, r: usize) -> Vec<[f32; 4]> {
    box_blur_inside(pool, src, side, r, 0)
}

/// [`box_blur`]'s output inside the `side`-wide patch's margin `pad` (the
/// middle `side - 2 pad` square): the second pass works out only that, and
/// the rows the last one reads.
fn box_blur_inside(
    pool: &rayon::ThreadPool,
    src: &[[f32; 4]],
    side: usize,
    r: usize,
    pad: usize,
) -> Vec<[f32; 4]> {
    let inner = side - 2 * pad;
    // The first pass, all of it.
    let mut across = vec![[0.0f32; 4]; side * side];
    for_rows(pool, &mut across, side, |row, out| {
        blur_line(&src[row * side..(row + 1) * side], out, r, 0);
    });
    let mut once = vec![[0.0f32; 4]; side * side];
    blur_columns(pool, &across, (side, side), r, &mut once, 0);
    // The second: across, the middle columns of every row; down, the
    // middle rows.
    let mut across = vec![[0.0f32; 4]; side * inner];
    for_rows(pool, &mut across, inner, |row, out| {
        blur_line(&once[row * side..(row + 1) * side], out, r, pad);
    });
    let mut out = vec![[0.0f32; 4]; inner * inner];
    blur_columns(pool, &across, (inner, side), r, &mut out, pad);
    out
}

/// One line of a box blur: `out[i]` averages `line` over `from + i - r
/// ..= from + i + r` (what of it is inside).
fn blur_line(line: &[[f32; 4]], out: &mut [[f32; 4]], r: usize, from: usize) {
    let n = line.len();
    let mut sum = [0.0f32; 4];
    let mut count = 0.0f32;
    for p in &line[from.saturating_sub(r)..=(from + r).min(n - 1)] {
        for c in 0..4 {
            sum[c] += p[c];
        }
        count += 1.0;
    }
    for (k, o) in out.iter_mut().enumerate() {
        let i = from + k;
        *o = sum.map(|s| s / count);
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

/// The down pass of a box blur: the columns of `src` (`w`×`h`) blurred into
/// `out`, its rows from `from` on.
fn blur_columns(
    pool: &rayon::ThreadPool,
    src: &[[f32; 4]],
    (w, h): (usize, usize),
    r: usize,
    out: &mut [[f32; 4]],
    from: usize,
) {
    use rayon::prelude::*;
    let row = |y: usize| &src[y * w..(y + 1) * w];
    // Output rows `first..first + lines.len() / w`, with their own window.
    let band = |first: usize, lines: &mut [[f32; 4]]| {
        let (lo, hi) = (first.saturating_sub(r), (first + r).min(h - 1));
        let mut sum = vec![[0.0f32; 4]; w];
        for y in lo..=hi {
            for (s, p) in sum.iter_mut().zip(row(y)) {
                for c in 0..4 {
                    s[c] += p[c];
                }
            }
        }
        let mut count = (hi - lo + 1) as f32;
        for (k, line) in lines.chunks_mut(w).enumerate() {
            let y = first + k;
            for (o, s) in line.iter_mut().zip(&sum) {
                *o = s.map(|v| v / count);
            }
            if y + r + 1 < h {
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
    let n = out.len() / w.max(1);
    if w >= PARALLEL_SIDE {
        // Bands of a few thousand pixels' worth of rows (each starts its
        // window afresh: worth it once the band is longer than it).
        let rows = (PARALLEL_PIXELS / w).max(2 * r + 1).min(n.max(1));
        pool.install(|| {
            out.par_chunks_mut(w * rows)
                .enumerate()
                .for_each(|(i, lines)| band(from + i * rows, lines))
        });
    } else {
        band(from, out);
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
/// `mask`, over the pixels within `reach` of the middle; without a mask
/// (Krita's older engine), every pixel alike. Like Krita, it samples
/// spread-out pixels (a Halton sequence) until the colour stops moving
/// rather than reading them all: at least 64 (or 2%), then 16 at a time
/// until no channel moves more than 2/255. Also whether what it read
/// carried at least half a pixel's weight (else Krita looks wider); with
/// no weight at all the colour is transparent.
fn halton_dull(
    source: &[[f32; 4]],
    mask: Option<&[f32]>,
    side: usize,
    reach: f32,
) -> ([f32; 4], bool) {
    let mid = side as f32 * 0.5;
    let inside = |l: usize| (l as f32 + 0.5 - mid).abs() <= reach;
    // At least the middle pixel.
    let lo = (0..side).find(|&l| inside(l)).unwrap_or(side / 2);
    let hi = (0..side).rfind(|&l| inside(l)).map_or(lo + 1, |l| l + 1);
    let n_side = hi - lo;
    let n = n_side * n_side;
    let first = n.min(64.max((n as f32 * 0.02).round() as usize));
    let (mut sum, mut weight) = ([0.0f32; 4], 0.0f32);
    let take = |i: u32, sum: &mut [f32; 4], weight: &mut f32| {
        let x = lo + ((halton(i, 2) * n_side as f32) as usize).min(n_side - 1);
        let y = lo + ((halton(i, 3) * n_side as f32) as usize).min(n_side - 1);
        let at = y * side + x;
        let w = mask.map_or(1.0, |m| m[at]);
        if w > 0.0 {
            let px = source[at];
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
    (last, mask.is_none() || weight > 0.5)
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
            codec: Codec::for_canvas(&self.canvas),
            stroke: BlendStroke {
                kind,
                dir: Vec2::ZERO,
                before: HashMap::new(),
                before_deep: HashMap::new(),
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
                precise: HashMap::new(),
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
        let mut before_deep = self.stroke.before_deep;
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
                data: match before_deep.remove(&(tx, ty)) {
                    Some(deep) => crate::canvas::history::SnapshotPixels::Deep(deep),
                    None => data.into(),
                },
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
            match self.stroke.before_deep.get(&(tx, ty)) {
                Some(deep) => self.canvas.set_layer_tile_deep(self.idx, tx, ty, deep),
                None => self.canvas.write_layer_region(self.idx, rect, pixels, None),
            }
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
        self.stroke.precise.clear();
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
        // The share of a brush width between dabs.
        let gap = (o.spacing_px(diameter, o.spacing_factor(pressure)).max(1.0) / diameter.max(1.0))
            .min(1.0);
        let blur_size = self.blur_size;
        let brush_paint = self.paint_of(placed);
        let idx = self.idx;
        let alpha_lock = self.canvas.layers[idx].alpha_locked;
        let rc = placed.map_or(r, |p| p.dab.reach).ceil() as i32;
        let side = (2 * rc + 1) as usize;
        let (x0, y0) = (center.x.floor() as i32 - rc, center.y.floor() as i32 - rc);
        let pool = Arc::clone(&self.pool);
        let codec = self.codec;
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
        let mix = &self.stroke.mix;
        let by_pressure = |on: bool| if on { pressure.clamp(0.0, 1.0) } else { 1.0 };
        // (Per dab: the dabs overlapping pick up again the paint the brush
        // has just laid, so how far it carries hardly changes with the
        // spacing.)
        let length = (mix.length * by_pressure(mix.pressure_length) * by_length).clamp(0.0, 1.0);
        // Brush colour added per brush width travelled, whatever the
        // spacing: per dab, the share that compounds to it over one width.
        let rate = (mix.color_rate * by_pressure(mix.pressure_color) * by_rate).clamp(0.0, 1.0);
        let color_rate = 1.0 - (1.0 - rate).powf(gap);
        let paint_blend = mix.blend;

        // The pixels as stored (kept exactly where the dab doesn't reach),
        // and as paint; blurring, also blurred.
        let blur_radius = ((r * blur_size).round() as usize).max(1);
        let (stored, under, mut soft) = match self.stroke.kind {
            BlendKind::Blur | BlendKind::Sharpen(_) => {
                let (stored, under, soft) = self.blurred_patch((x0, y0), side, blur_radius);
                (stored, under, Some(soft))
            }
            _ => {
                let (stored, under) = self.read_layer((x0, y0), side, side);
                (stored, under, None)
            }
        };
        // Deform moves the pixels themselves: each takes its colour from
        // where the displacement says, at full weight (the mask sets how
        // far it moves).
        let mut weights_are_mask = true;
        let target: Vec<[f32; 4]> = match self.stroke.kind {
            BlendKind::Deform(mode, amount) => {
                weights_are_mask = false;
                let stroke = &self.stroke;
                let dir = if copy == 0 {
                    stroke.dir
                } else {
                    let s = &stroke.symmetry;
                    s.map(&stroke.copies[copy - 1], s.center + stroke.dir) - s.center
                };
                let step = self.brush.brush_options.spacing_px(diameter, 1.0).max(1.0);
                let margin = (r * amount * 0.6).max(step).ceil() as i32 + 2;
                let big_side = side + 2 * margin as usize;
                let (_, big) = self.read_layer((x0 - margin, y0 - margin), big_side, big_side);
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
            }
            BlendKind::Clone { offset, merged } => {
                let origin = (x0 + offset.0, y0 + offset.1);
                if merged {
                    let patch = read_patch(&self.canvas, None, origin, side, side, self.wrap);
                    patch.into_iter().map(|c| codec.to_f(c)).collect()
                } else {
                    self.read_layer(origin, side, side).1
                }
            }
            BlendKind::Sharpen(amount) => {
                let soft = soft.as_deref().unwrap_or(&under);
                under
                    .iter()
                    .zip(soft)
                    .map(|(u, b)| {
                        let a = u[3];
                        let mut v = *u;
                        for k in 0..3 {
                            v[k] = (u[k] + (u[k] - b[k]) * amount).clamp(0.0, a);
                        }
                        v
                    })
                    .collect()
            }
            BlendKind::Filter(filter) => {
                // The filtered layer laid down through the brush, its
                // coverage building up like paint: going over a spot again
                // never filters it twice.
                weights_are_mask = false;
                let stroke = &mut self.stroke;
                let ts = self.canvas.tile_size() as i32;
                let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
                let wrap = self.wrap;
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
                        let tile =
                            filter_tile(&self.canvas, idx, &stroke.before, key, filter, wrap);
                        stroke.filtered.insert(key, tile);
                    }
                    let filtered = codec.to_f(stroke.filtered[&key][at]);
                    let original = stroke
                        .before
                        .get(&key)
                        .map_or(under[i], |t| codec.to_f(t[at]));
                    let cov = &mut stroke
                        .coverage
                        .entry(key)
                        .or_insert_with(|| vec![0.0; (ts * ts) as usize])[at];
                    *cov += m * (1.0 - *cov);
                    let c = *cov;
                    target[i] =
                        std::array::from_fn(|k| original[k] + (filtered[k] - original[k]) * c);
                }
                target
            }
            BlendKind::Adjust(hsv) => under.iter().map(|&u| adjust_hsv(codec, u, hsv)).collect(),
            BlendKind::Smudge => {
                // The carried paint, resized if pressure changed the tip size.
                let carries = &mut self.stroke.carries;
                if carries.len() <= copy {
                    carries.resize_with(copy + 1, || None);
                }
                let mut carry = match carries[copy].take() {
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
                // The brush picks up the paint under it, keeping the smudge
                // length's share of what it carried, then (a wet brush)
                // mixes in its own colour; that is what it lays down.
                // (Picked up before it paints, so the length counts under
                // all of the tip: picking up what it had just laid kept all
                // it carried wherever the tip was at full strength.)
                let under = &under;
                for_rows(&pool, &mut carry.px, side, |row, line| {
                    for (c, u) in line.iter_mut().zip(&under[row * side..]) {
                        for k in 0..4 {
                            c[k] = u[k] + (c[k] - u[k]) * length;
                            c[k] += (brush_paint[k] - c[k]) * color_rate;
                        }
                    }
                });
                let px = carry.px.clone();
                carries[copy] = Some(carry);
                px
            }
            BlendKind::Blur => soft.take().unwrap_or_else(|| under.clone()),
        };

        // Pixel `i` under the dab: the target mixed into what's there.
        let mixed = |i: usize| {
            let (u, t) = (under[i], target[i]);
            let m = if weights_are_mask { mask[i] } else { 1.0 };
            // A mixing brush's blend mode: its paint over what's there.
            let t = if paint_blend == crate::canvas::blend_modes::LayerBlend::Normal {
                t
            } else {
                let rgba = |v: [f32; 4]| {
                    eframe::egui::Rgba::from_rgba_premultiplied(v[0], v[1], v[2], v[3])
                };
                crate::canvas::blend_modes::composite(paint_blend, rgba(t), rgba(u), 0.0).to_array()
            };
            let mut v = [0.0; 4];
            for c in 0..4 {
                v[c] = u[c] + (t[c] - u[c]) * m;
            }
            v
        };
        let spans = spans_of(&mask, side);
        let mut result = under.clone();
        let changed = lay_spans(&pool, &mut result, side, &spans, &mask, |i| {
            let v = mixed(i);
            if alpha_lock {
                with_alpha_linear(v, under[i][3])
            } else {
                v
            }
        });
        if changed {
            self.write_blend_patch((x0, y0), side, &stored, &result, &mask);
        }
    }

    /// The brush colour as paint (premultiplied, opaque, in the stroke's
    /// space): the placed dab's own when its colour varies (inputs, colour
    /// source, randomness).
    fn paint_of(&self, placed: Option<&Placed>) -> [f32; 4] {
        match placed {
            // (The dab's colour is in the document's blend space, as the
            // stroke's paint is.)
            Some(p) if self.brush.varies_color() => {
                let [r, g, b] = p.dab.color;
                [r, g, b, 1.0]
            }
            _ => {
                let c = self.brush.brush_options.color;
                self.codec.to_f(Color32::from_rgb(c.r(), c.g(), c.b()))
            }
        }
    }

    /// The layer under the `side`² patch at `origin` (as stored and as
    /// paint, as [`Self::read_layer`] gives it), and box-blurred with
    /// `radius`: read that much wider all round, so near the patch's edge
    /// the blur takes in what lies beyond it, as filtering the layer would
    /// (cut off there, a hard tip's edge pixels averaged only what was
    /// inside it).
    fn blurred_patch(
        &self,
        (x0, y0): (i32, i32),
        side: usize,
        radius: usize,
    ) -> (Vec<Color32>, Vec<[f32; 4]>, Vec<[f32; 4]>) {
        let pad = radius as i32;
        let wide = side + 2 * radius;
        let (stored, paint) = self.read_layer((x0 - pad, y0 - pad), wide, wide);
        let soft = box_blur_inside(&self.pool, &paint, wide, radius, radius);
        let inside = (radius, radius, side, side);
        let stored = if stored.is_empty() {
            stored
        } else {
            patch_block(&stored, wide, inside).into_owned()
        };
        (stored, patch_block(&paint, wide, inside).into_owned(), soft)
    }

    /// The layer over the `w`×`h` rectangle at `origin` (round the edges
    /// with wrap-around): its pixels as stored (an 8-bit document's, else
    /// none), and as paint in the stroke's space. What the stroke has
    /// painted comes back as it mixed it, not rounded to 8 bits.
    fn read_layer(&self, origin: (i32, i32), w: usize, h: usize) -> (Vec<Color32>, Vec<[f32; 4]>) {
        let codec = self.codec;
        if self.canvas.depth().is_deep() {
            let mut paint = read_deep(&self.canvas, self.idx, origin, w, h, self.wrap);
            if codec.gamma.is_some() {
                for v in &mut paint {
                    *v = codec.of_linear(*v);
                }
            }
            return (Vec::new(), paint);
        }
        let stored = read_patch(&self.canvas, Some(self.idx), origin, w, h, self.wrap);
        let mut paint = vec![[0.0f32; 4]; w * h];
        for_rows(&self.pool, &mut paint, w, |row, line| {
            for (p, &c) in line.iter_mut().zip(&stored[row * w..]) {
                *p = codec.to_f(c);
            }
        });
        let precise = &self.stroke.precise;
        if !precise.is_empty() {
            tile_blocks(&self.canvas, origin, (w, h), self.wrap, |key, block| {
                let Some(t) = precise.get(&key) else {
                    return;
                };
                let n = block.run;
                for (at, from) in block.rows() {
                    for (p, v) in paint[from..from + n].iter_mut().zip(&t[at..at + n]) {
                        if v[3] >= 0.0 {
                            *p = *v;
                        }
                    }
                }
            });
        }
        (stored, paint)
    }
}

impl BlendSession {
    /// One dab of an imported colour smudge: the layer under the dab, with the layer where the last
    /// dab was laid over it (smearing; in dulling mode one colour sampled
    /// there), at the smudge rate × opacity; then the brush colour at the
    /// colour rate² × opacity (by the brush's blend mode); the result put
    /// down through the tip. Krita's older engine copies what it picks up
    /// whole, adds less colour and puts the result down at the smudge
    /// rate. The stroke's first dab only says where it is.
    fn krita_smudge_dab(&mut self, center: Vec2, pressure: f32, copy: usize, placed: &Placed) {
        let brush = &self.brush;
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
        // (A lightness tip always takes the new engine, as in Krita.)
        let legacy = k.legacy && !brush.lays_lightness();
        let by = |on: bool| if on { p } else { 1.0 };
        let rate = m.smudge_length * by(m.pressure_length) * placed.dab.smudge;
        let color_rate = m.color_rate * by(m.pressure_color) * placed.dab.color_rate;
        // How much of what's picked up goes over the layer, how much brush
        // colour then, and how much of the result is put down.
        let (smear, colour, laid) = if legacy {
            // The brush colour at most what the most smudge leaves (at
            // least a fifth).
            let most = (1.0 - m.smudge_length).max(0.2);
            let colour = (most * color_rate * opacity).clamp(0.0, 1.0);
            (1.0, colour, (rate * opacity).clamp(0.0, 1.0))
        } else {
            let smear = if k.dulling { 0.8 } else { 1.0 } * rate * opacity;
            (smear, color_rate * color_rate * opacity, 1.0)
        };
        let paint = self.paint_of(Some(placed));
        let paint_blend = brush.paint_blend;
        // Krita puts a smearing dab on whole pixels and reads the layer
        // where the last one was, as many whole pixels away: the paint
        // moves in whole pixels, never resampled (resampling it each dab
        // blurred a long smear), and a slow stroke's moves add up as its
        // dabs cross pixels.
        let at = (center.x.floor() as i32, center.y.floor() as i32);
        let stroke = &mut self.stroke;
        if stroke.krita_last.len() <= copy {
            stroke.krita_last.resize(copy + 1, None);
        }
        let Some(last) = stroke.krita_last[copy].replace(at) else {
            return;
        };
        let alpha_lock = self.canvas.layers[self.idx].alpha_locked;
        let rc = placed.dab.reach.ceil() as i32;
        let side = (2 * rc + 1) as usize;
        let (x0, y0) = (at.0 - rc, at.1 - rc);
        // The tip (turned, textured, selected): how much of the result
        // each pixel takes.
        let shape = Placed {
            dab: placed.dab,
            strength: 1.0,
        };
        let mask = self.placed_mask(&shape, (x0, y0), side);
        let pool = Arc::clone(&self.pool);
        // Each row's part under the tip: the rest is left as it is.
        let spans = spans_of(&mask, side);
        let (stored, under) = self.read_layer((x0, y0), side, side);
        let from = (x0 + last.0 - at.0, y0 + last.1 - at.1);
        let source = self.read_layer(from, side, side).1;
        // Dulling: one colour sampled where the last dab was.
        let dulled = k.dulling.then(|| {
            if legacy {
                // Every pixel alike, over the dab's square scaled by the
                // radius (up to three times it: past the dab), at least
                // its middle pixel.
                let grow = (side as f32 * 0.5 * (k.radius - 1.0)) as i32;
                let n = if k.radius > 0.0 {
                    (side as i32 + 2 * grow).max(1) as usize
                } else {
                    1
                };
                let half = (n / 2) as i32;
                let around = self
                    .read_layer((from.0 + rc - half, from.1 + rc - half), n, n)
                    .1;
                halton_dull(&around, None, n, n as f32).0
            } else {
                // Weighted by the tip and the paint's coverage
                // (premultiplied), out to the smudge radius, widened while
                // that holds no paint.
                let mut radius = k.radius.min(1.0);
                loop {
                    let (c, enough) =
                        halton_dull(&source, Some(&mask), side, (r * radius).max(0.5));
                    if enough || radius >= 1.0 {
                        break c;
                    }
                    radius = (radius + 0.05).min(1.0);
                }
            }
        });
        let normal = paint_blend == crate::canvas::blend_modes::LayerBlend::Normal;
        // With both going over the layer (no smear alpha, a Normal brush),
        // Krita mixes the brush colour into the dulled colour first, and
        // lays that at the dulling rate.
        let fused = !legacy && k.dulling && colour > 0.0 && !k.smear_alpha && normal;
        let dulled = dulled.map(|d| {
            if fused {
                std::array::from_fn(|c| paint[c] * colour + d[c] * (1.0 - colour))
            } else {
                d
            }
        });
        let over = |s: [f32; 4], u: [f32; 4], o: f32| -> [f32; 4] {
            std::array::from_fn(|c| s[c] * o + u[c] * (1.0 - s[3] * o))
        };
        let lerp = |u: [f32; 4], s: [f32; 4], o: f32| -> [f32; 4] {
            std::array::from_fn(|c| u[c] + (s[c] - u[c]) * o)
        };

        // One pixel under the tip: the picked-up paint over the layer,
        // then the brush colour, put down through the tip.
        let smudge_pixel = |i: usize| -> [f32; 4] {
            let u = under[i];
            let s = dulled.unwrap_or(source[i]);
            // The picked-up paint over the layer: copied (alpha too) with
            // smear alpha, and always by the older engine.
            let mut v = if legacy || k.smear_alpha {
                lerp(u, s, smear)
            } else {
                over(s, u, smear)
            };
            // Then the brush colour.
            if colour > 0.0 && !fused {
                v = if normal {
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
            // Put down through the tip: copied, or over the layer by the
            // older engine without smear alpha.
            let m = mask[i].min(1.0) * laid;
            if legacy && !k.smear_alpha {
                over(v, u, m)
            } else {
                lerp(u, v, m)
            }
        };

        let mut result = under.clone();
        let changed = lay_spans(&pool, &mut result, side, &spans, &mask, |i| {
            let v = smudge_pixel(i);
            if alpha_lock {
                with_alpha_linear(v, under[i][3])
            } else {
                v
            }
        });
        if changed {
            self.write_blend_patch((x0, y0), side, &stored, &result, &mask);
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

    /// Put `paint` (a `side`² patch at `origin`, in the stroke's space) on
    /// layer where `mask` reaches (round the edges with wrap-around),
    /// keeping the tiles' pixels from before the stroke for its undo. A
    /// deeper document takes it at full depth; an 8-bit one rounded
    /// (`stored`, its pixels as they were, kept exactly where the mask
    /// doesn't reach), the paint itself kept for the stroke to read back.
    fn write_blend_patch(
        &mut self,
        (x0, y0): (i32, i32),
        side: usize,
        stored: &[Color32],
        paint: &[[f32; 4]],
        mask: &[f32],
    ) {
        let (idx, wrap, codec) = (self.idx, self.wrap, self.codec);
        // Wrap-around: each piece of the patch where it lands on the canvas.
        let pieces: Vec<_> = if wrap {
            let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
            let rows = wrap_pieces(y0, side, ch);
            wrap_pieces(x0, side, cw)
                .into_iter()
                .flat_map(|(sx, dx, w)| rows.iter().map(move |&(sy, dy, h)| (sx, sy, dx, dy, w, h)))
                .collect()
        } else {
            vec![(x0, y0, 0, 0, side, side)]
        };
        let deep = self.canvas.depth().is_deep();
        let linear: std::borrow::Cow<'_, [[f32; 4]]> = match deep && codec.gamma.is_some() {
            true => paint.iter().map(|&v| codec.to_linear(v)).collect(),
            false => std::borrow::Cow::Borrowed(paint),
        };
        let mut result = Vec::new();
        if !deep {
            result = stored.to_vec();
            for_rows(&self.pool, &mut result, side, |row, line| {
                for (lx, out) in line.iter_mut().enumerate() {
                    let i = row * side + lx;
                    if mask[i] > 0.0 {
                        *out = codec.to_c(paint[i]);
                    }
                }
            });
        }
        let stroke = &mut self.stroke;
        for (sx, sy, dx, dy, w, h) in pieces {
            let rect = (sx, sy, w, h);
            if deep {
                self.canvas.write_layer_region_deep(
                    idx,
                    rect,
                    &patch_block(&linear, side, (dx, dy, w, h)),
                    &mut stroke.before,
                    &mut stroke.before_deep,
                );
            } else {
                self.canvas.write_layer_region(
                    idx,
                    rect,
                    &patch_block(&result, side, (dx, dy, w, h)),
                    Some(&mut stroke.before),
                );
            }
            self.damage.push([sx, sy, sx + w as i32, sy + h as i32]);
        }
        if deep {
            return;
        }
        // The paint, for the stroke to read back (a tile met for the first
        // time holds none: an alpha below 0).
        let ts = self.canvas.tile_size();
        let precise = &mut stroke.precise;
        tile_blocks(&self.canvas, (x0, y0), (side, side), wrap, |key, block| {
            let tile = precise
                .entry(key)
                .or_insert_with(|| vec![[0.0, 0.0, 0.0, -1.0]; ts * ts]);
            let n = block.run;
            for (at, from) in block.rows() {
                tile[at..at + n].copy_from_slice(&paint[from..from + n]);
            }
        });
    }
}

/// Each row's part of a `side`-wide `mask` that it reaches, as
/// `(first, last)` (empty when none).
fn spans_of(mask: &[f32], side: usize) -> Vec<(usize, usize)> {
    mask.chunks(side)
        .map(|row| {
            let first = row.iter().position(|&m| m > 0.0).unwrap_or(side);
            let last = row.iter().rposition(|&m| m > 0.0).map_or(first, |i| i + 1);
            (first, last)
        })
        .collect()
}

/// The `w`×`h` canvas rectangle at `origin` (round the edges with
/// wrap-around, else what's on the canvas) in blocks each within one tile:
/// `f(tile, block)` once a block, its rows' runs in it.
fn tile_blocks(
    canvas: &crate::canvas::Canvas,
    (x0, y0): (i32, i32),
    (w, h): (usize, usize),
    wrap: bool,
    mut f: impl FnMut((i32, i32), TileBlock),
) {
    let ts = canvas.tile_size();
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    // A span's pieces on the canvas, cut at the tiles' edges: (tile, start
    // in the tile, start in the rectangle, length).
    let split = |start: i32, len: usize, size: i32| {
        let mut out = Vec::new();
        for (at, offset, run) in wrap_pieces_or_clip(start, len, size, wrap) {
            let (mut c, end) = (at as usize, at as usize + run);
            while c < end {
                let to = ((c / ts + 1) * ts).min(end);
                out.push(((c / ts) as i32, c % ts, offset + c - at as usize, to - c));
                c = to;
            }
        }
        out
    };
    let columns = split(x0, w, cw);
    for (ty, in_y, dy, rows) in split(y0, h, ch) {
        for &(tx, in_x, dx, run) in &columns {
            f(
                (tx, ty),
                TileBlock {
                    at: in_y * ts + in_x,
                    from: dy * w + dx,
                    run,
                    rows,
                    tile_stride: ts,
                    stride: w,
                },
            );
        }
    }
}

/// A block of a rectangle within one tile (see [`tile_blocks`]).
struct TileBlock {
    /// Where it starts in the tile and in the rectangle.
    at: usize,
    from: usize,
    /// Its width and height.
    run: usize,
    rows: usize,
    /// The tile's and the rectangle's widths.
    tile_stride: usize,
    stride: usize,
}

impl TileBlock {
    /// Each row's (start in the tile, start in the rectangle).
    fn rows(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        (0..self.rows).map(|r| (self.at + r * self.tile_stride, self.from + r * self.stride))
    }
}

/// Set each pixel of the `side`-wide `patch` under the tip (by row, the
/// span `spans` gives, where `mask` reaches) to `f` of its index, rows on
/// `pool`: whether any changed.
fn lay_spans<T: PartialEq + Send>(
    pool: &rayon::ThreadPool,
    patch: &mut [T],
    side: usize,
    spans: &[(usize, usize)],
    mask: &[f32],
    f: impl Fn(usize) -> T + Sync,
) -> bool {
    let lay_row = |ly: usize, row: &mut [T]| {
        let (first, last) = spans[ly];
        let mut changed = false;
        for (lx, px) in row.iter_mut().enumerate().take(last).skip(first) {
            let i = ly * side + lx;
            if mask[i] <= 0.0 {
                continue;
            }
            let out = f(i);
            changed |= out != *px;
            *px = out;
        }
        changed
    };
    if side < PARALLEL_SIDE {
        return (patch.chunks_mut(side).enumerate())
            .fold(false, |c, (ly, row)| lay_row(ly, row) | c);
    }
    let rows = (PARALLEL_PIXELS / side).max(1);
    pool.install(|| {
        patch
            .par_chunks_mut(side * rows)
            .enumerate()
            .map(|(batch, lines)| {
                (lines.chunks_mut(side).enumerate())
                    .fold(false, |c, (i, row)| lay_row(batch * rows + i, row) | c)
            })
            .reduce(|| false, |a, b| a || b)
    })
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
