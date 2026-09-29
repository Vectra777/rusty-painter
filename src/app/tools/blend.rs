//! Smudge and Blur: blending tools that work with the normal brush's size,
//! hardness, spacing, flow, opacity and pressure settings, but move or
//! soften the paint already on the layer instead of adding colour (like
//! Clip Studio's Blend tools).
//!
//! Each has more modes: Smudge can instead **deform** (push the paint
//! along, grow, shrink or swirl it, like Krita's deform brush) or
//! **clone** (paint with the pixels from another place, set with
//! Ctrl+click); Blur can instead **sharpen** or **adjust** colours (hue,
//! saturation, brightness) under the brush, or paint any of the Filter
//! menu's filters (**filter**), like Krita's filter brush.
//!
//! Smudge carries a patch of paint along the stroke: every dab mixes the
//! carried paint into the canvas under the tip, then picks up some of the
//! result (how much it keeps is the smudge length). With a colour rate it
//! is a wet mixing brush, like Krita's Color Smudge: each dab first mixes
//! that much of the brush colour into the carried paint, so it lays down
//! the brush colour blended with whatever it drags. Blur mixes each pixel
//! toward the average around it. Both are sequential per dab (each dab sees
//! the previous one's result), so they run their own small engine rather
//! than the batched brush pipeline.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::canvas::storage::{LayerId, LayerKind};
use eframe::egui::{Color32, Vec2};
use std::collections::HashMap;

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
    pub clone_source: Option<Vec2>,
    /// Clone: keep the same offset from stroke to stroke (else each stroke
    /// starts again from the source).
    pub clone_aligned: bool,
    /// Clone: copy what's visible (all layers) rather than this layer.
    pub clone_merged: bool,
    /// Clone: the offset kept while aligned, from the first stroke.
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
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
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

/// A patch of carried paint (linear premultiplied, 0..1 per channel).
struct Carry {
    side: usize,
    px: Vec<[f32; 4]>,
}

pub struct BlendStroke {
    layer_id: LayerId,
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
}

/// Pixels are mixed as linear-light premultiplied colour, the same space
/// the compositor works in. (Mixing the stored sRGB bytes and clamping each
/// channel to alpha darkened light colours at soft edges, as if another
/// colour were being picked up.)
fn to_f(c: Color32) -> [f32; 4] {
    let l = crate::canvas::blend::color32_to_linear(c);
    [l.r(), l.g(), l.b(), l.a()]
}

fn to_c(v: [f32; 4]) -> Color32 {
    let a = v[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return Color32::TRANSPARENT;
    }
    // Premultiplied: colour can't exceed alpha.
    let c = |x: f32| x.clamp(0.0, a);
    crate::canvas::blend::rgba_to_color32_fast(eframe::egui::Rgba::from_rgba_premultiplied(
        c(v[0]),
        c(v[1]),
        c(v[2]),
        a,
    ))
}

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
/// looks nearly Gaussian).
fn box_blur(src: &[[f32; 4]], side: usize, r: usize) -> Vec<[f32; 4]> {
    let pass = |input: &[[f32; 4]], horizontal: bool| -> Vec<[f32; 4]> {
        let mut out = vec![[0.0; 4]; side * side];
        for line in 0..side {
            let at = |i: usize| {
                if horizontal {
                    line * side + i
                } else {
                    i * side + line
                }
            };
            let mut sum = [0.0f32; 4];
            let mut count = 0.0f32;
            for i in 0..=r.min(side - 1) {
                let p = input[at(i)];
                for c in 0..4 {
                    sum[c] += p[c];
                }
                count += 1.0;
            }
            for i in 0..side {
                out[at(i)] = sum.map(|s| s / count);
                let add = i + r + 1;
                if add < side {
                    let p = input[at(add)];
                    for c in 0..4 {
                        sum[c] += p[c];
                    }
                    count += 1.0;
                }
                if i >= r {
                    let p = input[at(i - r)];
                    for c in 0..4 {
                        sum[c] -= p[c];
                    }
                    count -= 1.0;
                }
            }
        }
        out
    };
    let mut v = src.to_vec();
    for _ in 0..2 {
        v = pass(&v, true);
        v = pass(&v, false);
    }
    v
}

impl PainterApp {
    pub(crate) fn set_blend_tool(&mut self, smudge: bool) {
        // They use the brush's settings, not the eraser's.
        self.set_brush_tool(false);
        self.active_tool = if smudge { Tool::Smudge } else { Tool::Blur };
    }

    pub(crate) fn blend_press(&mut self, pos: Vec2, pressure: f32) {
        let pos = self.ruler_begin_stroke(pos);
        let idx = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        if layer.locked || matches!(layer.kind, LayerKind::Group) {
            return;
        }
        let layer_id = layer.id;
        let b = &mut self.workspace.blend;
        let kind = match (self.active_tool, b.smudge_mode, b.filter_mode) {
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
        };
        self.release_canvas();
        // Like a brush stroke: a second press ends the running one first.
        self.blend_release();
        self.mark_action();
        self.brush_state.blend_stroke = Some(BlendStroke {
            layer_id,
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
        });
        self.blend_mirrored(pos, pressure);
    }

    pub(crate) fn blend_drag(&mut self, pos: Vec2, pressure: f32) {
        let pos = self.ruler_snap(pos);
        let diameter = self.blend_diameter(pressure);
        let spacing = (diameter * self.brush_state.brush.brush_options.spacing / 100.0).max(1.0);
        let Some(stroke) = self.brush_state.blend_stroke.as_mut() else {
            return;
        };
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
        for (p, pr) in dabs {
            self.blend_mirrored(p, pr);
        }
    }

    pub(crate) fn blend_release(&mut self) {
        let Some(stroke) = self.brush_state.blend_stroke.take() else {
            return;
        };
        // The layer may be gone since (deleted mid-stroke).
        if self.canvas.layer_index_of(stroke.layer_id).is_none() {
            return;
        }
        if stroke.before.is_empty() {
            return;
        }
        let ts = self.canvas.tile_size();
        let tiles = stroke
            .before
            .into_iter()
            .map(|((tx, ty), data)| TileSnapshot {
                tx,
                ty,
                layer_id: stroke.layer_id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: data.into(),
            })
            .collect();
        self.push_undo(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: None,
        });
        self.layer_state.thumbnails_dirty = true;
    }

    fn blend_diameter(&self, pressure: f32) -> f32 {
        let o = &self.brush_state.brush.brush_options;
        let k = if o.pressure_size {
            o.pressure_min_size + (1.0 - o.pressure_min_size) * o.pressure_curves.size(pressure)
        } else {
            1.0
        };
        (o.diameter * k).max(1.0)
    }

    /// A dab at `center` and its mirror copies.
    fn blend_mirrored(&mut self, center: Vec2, pressure: f32) {
        let Some(stroke) = self.brush_state.blend_stroke.as_ref() else {
            return;
        };
        let positions = stroke.symmetry.positions(&stroke.copies, center);
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        for (copy, p) in positions {
            // Wrap-around: the dab put on the canvas (its patch then wraps
            // round the edges).
            let p = if self.workspace.wrap_around {
                Vec2::new(p.x.rem_euclid(w), p.y.rem_euclid(h))
            } else {
                p
            };
            self.blend_dab(p, pressure, copy);
        }
    }

    /// One dab at `center`, for mirror copy `copy`.
    fn blend_dab(&mut self, center: Vec2, pressure: f32, copy: usize) {
        let diameter = self.blend_diameter(pressure);
        let o = &self.brush_state.brush.brush_options;
        let r = diameter * 0.5;
        let mut strength = o.flow / 100.0 * o.opacity;
        if o.pressure_opacity {
            strength *= o.pressure_curves.opacity(pressure);
        }
        if o.pressure_flow {
            strength *= o.pressure_curves.flow(pressure);
        }
        let hardness = (o.hardness / 100.0).clamp(0.0, 1.0);
        let (length, blur_size) = (
            self.workspace.blend.smudge_length.clamp(0.0, 1.0),
            self.workspace.blend.blur_size,
        );
        // Brush colour added per brush width travelled, whatever the
        // spacing: per dab, the share that compounds to it over one width.
        let steps_per_width = (100.0 / o.spacing.max(1.0)).max(1.0);
        let color_rate = 1.0
            - (1.0 - self.workspace.blend.color_rate.clamp(0.0, 1.0)).powf(1.0 / steps_per_width);
        // The brush colour as carried paint (linear, premultiplied, opaque).
        let brush_paint = to_f(Color32::from_rgb(o.color.r(), o.color.g(), o.color.b()));
        let Some(stroke) = self.brush_state.blend_stroke.as_mut() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(stroke.layer_id) else {
            return;
        };
        let alpha_lock = self.canvas.layers[idx].alpha_locked;
        let rc = r.ceil() as i32;
        let side = (2 * rc + 1) as usize;
        let (x0, y0) = (center.x.floor() as i32 - rc, center.y.floor() as i32 - rc);

        // The brush tip, shaped like the brush's (hardness falloff), times
        // the stroke strength and the selection.
        let mut mask = vec![0.0f32; side * side];
        let mut row_sel = vec![1.0f32; side];
        for ly in 0..side {
            let y = y0 + ly as i32;
            if self.selection_manager.has_selection() {
                if y < 0 {
                    continue;
                }
                let start = x0.max(0);
                row_sel.fill(0.0);
                let skip = (start - x0) as usize;
                if skip < side {
                    self.selection_manager.row_coverage(
                        y as usize,
                        start as usize,
                        &mut row_sel[skip..],
                    );
                }
            }
            for lx in 0..side {
                let p = Vec2::new((x0 + lx as i32) as f32 + 0.5, y as f32 + 0.5);
                let t = (p - center).length() / r.max(0.5);
                if t < 1.0 {
                    mask[ly * side + lx] =
                        crate::brush_engine::masks::gaussian_falloff(t, hardness)
                            * strength
                            * row_sel[lx];
                }
            }
        }

        let wrap = self.workspace.wrap_around;
        let under: Vec<[f32; 4]> = read_patch(&self.canvas, Some(idx), (x0, y0), side, side, wrap)
            .into_iter()
            .map(to_f)
            .collect();
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
            let step = (diameter * self.brush_state.brush.brush_options.spacing / 100.0).max(1.0);
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
            .map(to_f)
            .collect();
            let local_center = center - Vec2::new(x0 as f32, y0 as f32);
            (0..side * side)
                .map(|i| {
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
                })
                .collect()
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
            .map(to_f)
            .collect()
        } else if let BlendKind::Sharpen(amount) = stroke.kind {
            let radius = ((r * blur_size).round() as usize).max(1);
            let soft = box_blur(&under, side, radius);
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
                // Wet paint (Krita's Color Smudge): the brush picks up the
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
            box_blur(&under, side, radius)
        };

        let mut result = Vec::with_capacity(side * side);
        let mut changed = false;
        for i in 0..side * side {
            let (u, t) = (under[i], target[i]);
            let m = if weights_are_mask {
                mask[i]
            } else if mask[i] > 0.0 {
                1.0
            } else {
                0.0
            };
            let mut v = [0.0; 4];
            for c in 0..4 {
                v[c] = u[c] + (t[c] - u[c]) * m;
            }
            let mut out = to_c(v);
            let before = to_c(u);
            if alpha_lock {
                out = crate::canvas::blend::with_alpha_of(out, before.a());
            }
            changed |= out != before;
            result.push(out);
        }
        // Smudge picks up the blended paint for the next dab (a wet brush
        // picked up the paint under it before painting).
        if color_rate <= 0.0
            && let Some(carry) = stroke.carries.get_mut(copy).and_then(|c| c.as_mut())
        {
            for (c, &res) in carry.px.iter_mut().zip(&result) {
                let res = to_f(res);
                for k in 0..4 {
                    c[k] = res[k] + (c[k] - res[k]) * length;
                }
            }
        }
        if !changed {
            return;
        }
        if !wrap {
            self.canvas.write_layer_region(
                idx,
                (x0, y0, side, side),
                &result,
                Some(&mut stroke.before),
            );
            self.mark_rect_damage([x0, y0, x0 + side as i32, y0 + side as i32]);
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
            self.mark_rect_damage(rect);
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
    let out = filter.apply(&src, side, side, origin);
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
            Some(BlendStroke {
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
mod tests {
    use super::*;

    #[test]
    fn box_blur_keeps_flat_areas_and_softens_edges() {
        let side = 9;
        let flat = vec![[10.0, 20.0, 30.0, 255.0]; side * side];
        let out = box_blur(&flat, side, 2);
        assert!(
            out.iter()
                .all(|p| (p[0] - 10.0).abs() < 1e-3 && (p[3] - 255.0).abs() < 1e-3)
        );
        // A hard vertical edge becomes a ramp.
        let edge: Vec<[f32; 4]> = (0..side * side)
            .map(|i| if i % side < 4 { [0.0; 4] } else { [1.0; 4] })
            .collect();
        let out = box_blur(&edge, side, 2);
        let mid = out[4 * side + 4][0];
        assert!(mid > 0.08 && mid < 0.92, "{mid}");
    }
}

#[cfg(test)]
mod mix_tests {
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    fn app(below: Option<Color32>) -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        if let Some(c) = below {
            for tx in 0..2 {
                app.canvas_mut()
                    .set_layer_tile_data(1, tx, 0, vec![c; 64 * 64]);
            }
        }
        app.active_tool = crate::app::tools::Tool::Smudge;
        let o = &mut app.brush_state.brush.brush_options;
        o.diameter = 20.0;
        o.hardness = 100.0;
        o.pressure_size = false;
        o.color = Color32::from_rgb(20, 40, 230);
        app
    }

    fn drag(app: &mut crate::PainterApp) {
        app.blend_press(Vec2::new(10.0, 32.0), 1.0);
        for i in 1..=20 {
            app.blend_drag(Vec2::new(10.0 + i as f32 * 5.0, 32.0), 1.0);
        }
        app.blend_release();
    }

    fn pixel(app: &crate::PainterApp, x: i32) -> Color32 {
        app.canvas
            .get_layer_tile_data(1, x / 64, 0)
            .map_or(Color32::TRANSPARENT, |t| t[(32 * 64 + x % 64) as usize])
    }

    #[test]
    fn a_wet_brush_paints_its_colour_on_empty_layers() {
        let mut app = app(None);
        app.workspace.blend.color_rate = 0.6;
        drag(&mut app);
        let p = pixel(&app, 60);
        assert!(
            p.a() > 200 && p.b() > p.r() * 3,
            "brush blue laid down: {p:?}"
        );
    }

    #[test]
    fn a_wet_brush_mixes_with_the_paint_under_it() {
        let red = Color32::from_rgb(230, 30, 20);
        let mut app = app(Some(red));
        app.workspace.blend.color_rate = 0.3;
        drag(&mut app);
        let p = pixel(&app, 60);
        assert!(p.r() > 40 && p.b() > 40, "red and blue mixed: {p:?}");
    }

    #[test]
    fn the_colour_rate_doesnt_depend_on_spacing() {
        let red = Color32::from_rgb(230, 30, 20);
        let at = |spacing: f32| {
            let mut app = app(Some(red));
            app.workspace.blend.color_rate = 0.3;
            app.brush_state.brush.brush_options.spacing = spacing;
            drag(&mut app);
            pixel(&app, 60)
        };
        let (dense, sparse) = (at(10.0), at(40.0));
        let diff = dense
            .to_array()
            .iter()
            .zip(sparse.to_array())
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap();
        assert!(diff <= 40, "{dense:?} vs {sparse:?}");
    }

    #[test]
    fn pressure_blends_along_a_blur_stroke() {
        // Hard stripes, blurred by a pen whose pressure rises from light to
        // full over one long segment: the blurred band widens gradually.
        let mut app = app(None);
        let stripes: Vec<Color32> = (0..64 * 64)
            .map(|i| {
                if (i % 64) % 4 < 2 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                }
            })
            .collect();
        for tx in 0..2 {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, 0, stripes.clone());
        }
        let before: Vec<Color32> = (0..128)
            .flat_map(|x| (0..64).map(move |y| (x, y)))
            .map(|(x, y)| px(&app, x, y))
            .collect();
        app.active_tool = crate::app::tools::Tool::Blur;
        let o = &mut app.brush_state.brush.brush_options;
        o.diameter = 40.0;
        o.pressure_size = true;
        o.pressure_min_size = 0.1;
        o.spacing = 5.0;
        app.blend_press(Vec2::new(10.0, 32.0), 0.1);
        app.blend_drag(Vec2::new(118.0, 32.0), 1.0);
        app.blend_release();
        let band = |x: i32| {
            (0..64)
                .filter(|&y| px(&app, x, y) != before[(x * 64 + y) as usize])
                .count() as i32
        };
        let widths: Vec<i32> = (20..100).step_by(4).map(band).collect();
        let jump = widths
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .max()
            .unwrap();
        assert!(
            widths[widths.len() - 1] > widths[0] + 10,
            "widens: {widths:?}"
        );
        assert!(jump <= 4, "no steps: {widths:?}");
    }

    fn px(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
        app.canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .map_or(Color32::TRANSPARENT, |t| {
                t[((y % 64) * 64 + x % 64) as usize]
            })
    }

    #[test]
    fn no_colour_rate_is_the_plain_smudge() {
        let red = Color32::from_rgb(230, 30, 20);
        let mut plain = app(Some(red));
        drag(&mut plain);
        let mut zero = app(Some(red));
        zero.workspace.blend.color_rate = 0.0;
        drag(&mut zero);
        for x in 0..128 {
            assert_eq!(pixel(&plain, x), pixel(&zero, x));
        }
        assert_eq!(
            pixel(&plain, 60),
            red,
            "smudging one colour changes nothing"
        );
    }
}

#[cfg(test)]
mod mode_tests {
    use super::{DeformMode, FilterMode, SmudgeMode};
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    /// A 128×64 layer: `paint(x, y)` for each pixel.
    fn app(paint: impl Fn(i32, i32) -> Color32) -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for tx in 0..2 {
            let tile = (0..64 * 64)
                .map(|i| paint(tx * 64 + i % 64, i / 64))
                .collect();
            app.canvas_mut().set_layer_tile_data(1, tx, 0, tile);
        }
        let o = &mut app.brush_state.brush.brush_options;
        o.diameter = 30.0;
        o.hardness = 100.0;
        o.flow = 100.0;
        o.opacity = 1.0;
        o.pressure_size = false;
        o.spacing = 10.0;
        app
    }

    fn px(app: &crate::PainterApp, x: i32, y: i32) -> Color32 {
        app.canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .map_or(Color32::TRANSPARENT, |t| {
                t[((y % 64) * 64 + x % 64) as usize]
            })
    }

    fn all(app: &crate::PainterApp) -> Vec<Color32> {
        (0..64)
            .flat_map(|y| (0..128).map(move |x| (x, y)))
            .map(|(x, y)| px(app, x, y))
            .collect()
    }

    fn stroke(app: &mut crate::PainterApp, from: Vec2, to: Vec2) {
        app.blend_press(from, 1.0);
        for i in 1..=10 {
            app.blend_drag(from + (to - from) * (i as f32 / 10.0), 1.0);
        }
        app.blend_release();
    }

    fn undo(app: &mut crate::PainterApp) {
        let mut tool = app.active_tool;
        let canvas = std::sync::Arc::get_mut(&mut app.canvas).unwrap();
        app.layer_state
            .history
            .undo(canvas, &mut app.selection_manager, &mut tool);
    }

    fn dot(x: i32, y: i32) -> Color32 {
        if (x - 64).pow(2) + (y - 32).pow(2) <= 36 {
            Color32::BLACK
        } else {
            Color32::WHITE
        }
    }

    fn dark(app: &crate::PainterApp) -> usize {
        all(app).iter().filter(|c| c.r() < 128).count()
    }

    #[test]
    fn deform_grows_shrinks_and_undoes_exactly() {
        let deformed = |mode, amount| {
            let mut a = app(dot);
            a.active_tool = crate::app::tools::Tool::Smudge;
            a.workspace.blend.smudge_mode = SmudgeMode::Deform;
            a.workspace.blend.deform_mode = mode;
            a.workspace.blend.deform_amount = amount;
            let c = Vec2::new(64.0, 32.0);
            stroke(&mut a, c, c + Vec2::new(0.5, 0.0));
            a
        };
        let before = dark(&app(dot));
        let still = deformed(DeformMode::Grow, 0.0);
        assert_eq!(all(&still), all(&app(dot)), "no amount, no change");
        assert!(dark(&deformed(DeformMode::Grow, 0.8)) > before * 3 / 2);
        assert!(dark(&deformed(DeformMode::Shrink, 0.8)) < before * 2 / 3);
        let mut grown = deformed(DeformMode::Grow, 0.8);
        undo(&mut grown);
        assert_eq!(all(&grown), all(&app(dot)));
    }

    #[test]
    fn deform_pushes_the_paint_along_the_stroke() {
        // A black bar at x 40..44, pushed right.
        let bar = |x: i32, _| {
            if (40..44).contains(&x) {
                Color32::BLACK
            } else {
                Color32::WHITE
            }
        };
        let mut a = app(bar);
        a.active_tool = crate::app::tools::Tool::Smudge;
        a.workspace.blend.smudge_mode = SmudgeMode::Deform;
        a.workspace.blend.deform_mode = DeformMode::Push;
        a.workspace.blend.deform_amount = 1.0;
        stroke(&mut a, Vec2::new(30.0, 32.0), Vec2::new(90.0, 32.0));
        // At full amount the bar keeps up with the brush: from 40 to 100.
        let dark: Vec<i32> = (30..128).filter(|&x| px(&a, x, 32).r() < 128).collect();
        assert!(
            !dark.is_empty() && dark.iter().all(|&x| (96..108).contains(&x)),
            "{dark:?}"
        );
        // Rows away from the stroke keep the bar where it was.
        assert!(px(&a, 42, 2).r() < 50 && px(&a, 60, 2).r() > 200);
    }

    #[test]
    fn clone_copies_the_source_pixel_for_pixel() {
        let pattern =
            |x: i32, y: i32| Color32::from_rgb((x * 7 % 256) as u8, (y * 11 % 256) as u8, 90);
        let mut a = app(pattern);
        a.active_tool = crate::app::tools::Tool::Smudge;
        a.workspace.blend.smudge_mode = SmudgeMode::Clone;
        // Nothing happens before a source is set.
        stroke(&mut a, Vec2::new(90.0, 32.0), Vec2::new(100.0, 32.0));
        assert_eq!(a.layer_state.history.push_count(), 0);
        a.workspace.blend.set_clone_source(Vec2::new(30.0, 32.0));
        stroke(&mut a, Vec2::new(90.0, 32.0), Vec2::new(100.0, 32.0));
        for x in 88..102 {
            let (got, want) = (px(&a, x, 32), pattern(x - 60, 32));
            for (g, w) in got.to_array().iter().zip(want.to_array()) {
                assert!(g.abs_diff(w) <= 1, "x {x}: {got:?} vs {want:?}");
            }
        }
        // Aligned: the next stroke keeps the offset.
        stroke(&mut a, Vec2::new(70.0, 20.0), Vec2::new(71.0, 20.0));
        let (got, want) = (px(&a, 70, 20), pattern(10, 20));
        assert!(got.r().abs_diff(want.r()) <= 1, "{got:?} vs {want:?}");
        undo(&mut a);
        undo(&mut a);
        assert_eq!(all(&a), all(&app(pattern)));
    }

    #[test]
    fn with_wrap_around_blur_mixes_across_the_edge() {
        // Black on the left edge, white elsewhere; blurred at the edge.
        let left = |x: i32, _| {
            if x < 4 {
                Color32::BLACK
            } else {
                Color32::WHITE
            }
        };
        let blurred = |wrap: bool| {
            let mut a = app(left);
            a.workspace.wrap_around = wrap;
            a.active_tool = crate::app::tools::Tool::Blur;
            a.workspace.blend.blur_size = 0.5;
            stroke(&mut a, Vec2::new(1.0, 20.0), Vec2::new(1.0, 44.0));
            a
        };
        let (wrapped, plain) = (blurred(true), blurred(false));
        // The thin band spreads out evenly both sides of the edge.
        assert!(px(&wrapped, 126, 32).r() < 250, "darkened across the edge");
        assert!(px(&wrapped, 126, 32).r().abs_diff(px(&wrapped, 5, 32).r()) <= 3);
        assert_eq!(px(&plain, 126, 32), Color32::WHITE);
        // One undo step takes both sides back.
        let mut wrapped = wrapped;
        undo(&mut wrapped);
        assert_eq!(all(&wrapped), all(&app(left)));
    }

    #[test]
    fn sharpen_strengthens_an_edge_and_adjust_turns_the_hue() {
        let edge = |x: i32, _| {
            if x < 64 {
                Color32::from_gray(90)
            } else {
                Color32::from_gray(170)
            }
        };
        let mut a = app(edge);
        a.active_tool = crate::app::tools::Tool::Blur;
        a.workspace.blend.filter_mode = FilterMode::Sharpen;
        a.workspace.blend.sharpen_amount = 1.5;
        stroke(&mut a, Vec2::new(64.0, 20.0), Vec2::new(64.0, 44.0));
        assert!(
            px(&a, 62, 32).r() < 90,
            "darker beside the edge: {:?}",
            px(&a, 62, 32)
        );
        assert!(
            px(&a, 65, 32).r() > 170,
            "lighter beside it: {:?}",
            px(&a, 65, 32)
        );

        let red = |_, _| Color32::from_rgb(220, 30, 30);
        let mut a = app(red);
        a.active_tool = crate::app::tools::Tool::Blur;
        a.workspace.blend.filter_mode = FilterMode::Adjust;
        a.workspace.blend.adjust_hue = 120.0;
        stroke(&mut a, Vec2::new(40.0, 32.0), Vec2::new(80.0, 32.0));
        let c = px(&a, 60, 32);
        assert!(c.g() > 150 && c.r() < 80, "red turned green: {c:?}");
    }
    fn filter_brush(
        paint: impl Fn(i32, i32) -> Color32,
        filter: crate::canvas::filters::Filter,
    ) -> crate::PainterApp {
        let mut a = app(paint);
        a.active_tool = crate::app::tools::Tool::Blur;
        a.workspace.blend.filter_mode = FilterMode::Filter;
        a.workspace.blend.brush_filter = filter;
        a
    }

    fn checks(x: i32, y: i32) -> Color32 {
        if (x / 4 + y / 4) % 2 == 0 {
            Color32::from_rgb(200, 40, 30)
        } else {
            Color32::from_rgb(20, 90, 230)
        }
    }

    #[test]
    fn the_filter_brush_changes_only_what_it_covers_and_undoes_exactly() {
        use crate::canvas::filters::Filter;
        let mut a = filter_brush(checks, Filter::Invert);
        let before = all(&a);
        // There and back in one stroke: going over a spot again doesn't
        // invert it back (nothing is filtered twice).
        a.blend_press(Vec2::new(30.0, 32.0), 1.0);
        for i in 1..=20 {
            let t = if i <= 10 { i } else { 20 - i } as f32 / 10.0;
            a.blend_drag(Vec2::new(30.0 + 60.0 * t, 32.0), 1.0);
        }
        a.blend_release();
        assert_eq!(a.layer_state.history.push_count(), 1, "one undo step");
        let inverted = |c: Color32| Color32::from_rgb(255 - c.r(), 255 - c.g(), 255 - c.b());
        for y in 0..64 {
            for x in 0..128 {
                let got = px(&a, x, y);
                let was = before[(y * 128 + x) as usize];
                // The brush: 30 px across along y = 32, from x 30 to 90.
                let dx = ((x - 60).abs() - 30).max(0);
                let d = ((dx * dx + (y - 32) * (y - 32)) as f32).sqrt();
                if d > 16.0 {
                    assert_eq!(got, was, "({x}, {y}) is outside the brush");
                } else if d < 12.0 {
                    let want = inverted(was);
                    for (g, w) in got.to_array().iter().zip(want.to_array()) {
                        assert!(g.abs_diff(w) <= 1, "({x}, {y}): {got:?} vs {want:?}");
                    }
                }
            }
        }
        undo(&mut a);
        assert_eq!(all(&a), before);
    }

    #[test]
    fn the_filter_brush_reads_the_layer_as_it_was_before_the_stroke() {
        use crate::canvas::filters::Filter;
        let blur = Filter::GaussianBlur { radius: 3.0 };
        let mut a = filter_brush(checks, blur);
        let before = all(&a);
        // Back and forth, dabs overlapping many times.
        stroke(&mut a, Vec2::new(30.0, 32.0), Vec2::new(90.0, 32.0));
        let want = blur.apply(&before, 128, 64, (0, 0));
        let mut changed = 0;
        for x in 35..85 {
            for y in 26..38 {
                let (got, w) = (px(&a, x, y), want[(y * 128 + x) as usize]);
                for (g, v) in got.to_array().iter().zip(w.to_array()) {
                    assert!(g.abs_diff(v) <= 2, "({x}, {y}): {got:?} vs {w:?}");
                }
                changed += usize::from(got != before[(y * 128 + x) as usize]);
            }
        }
        assert!(changed > 400, "blurred: {changed}");
    }

    #[test]
    fn every_filter_paints_through_the_brush() {
        use crate::canvas::filters::Filter;
        for &filter in Filter::MENU.iter().flat_map(|g| g.iter()) {
            let filter = match filter {
                // Its defaults change nothing.
                Filter::BrightnessContrast { .. } => Filter::BrightnessContrast {
                    brightness: 0.4,
                    contrast: 0.2,
                },
                Filter::HueSaturation { .. } => Filter::HueSaturation {
                    hue: 90.0,
                    saturation: 0.0,
                    lightness: 0.0,
                },
                Filter::Levels { .. } => Filter::Levels {
                    black: 0.2,
                    white: 0.8,
                    gamma: 1.0,
                },
                f => f,
            };
            let mut a = filter_brush(checks, filter);
            let before = all(&a);
            stroke(&mut a, Vec2::new(30.0, 32.0), Vec2::new(90.0, 32.0));
            assert!(all(&a) != before, "{}", filter.name());
            assert_eq!(px(&a, 5, 5), before[5 * 128 + 5], "{}", filter.name());
            undo(&mut a);
            assert!(all(&a) == before, "{} undoes", filter.name());
        }
    }
}
