//! The brush: its tip (Gaussian, custom image, pixel) and rendering a
//! batch of dabs into canvas tiles in parallel.

use super::brush_options::{BrushOptions, ColorSource};
use crate::{
    brush_engine::{
        brush_options::{BlendMode, PaintingMode, PixelBrushShape},
        dab::{
            PIXELS_PER_THREAD, PlacedDab, TileBucket, TileRegion, bucket_by_tile, calc_dab_bounds,
            dab_reaches_tile, dispatch_over_buckets, tile_overlap, tile_overlaps_selection,
        },
        hardness::SoftnessSelector,
        stroke::{StrokeBuffer, StrokeTiles},
    },
    canvas::{
        Canvas,
        blend::{
            StrokeColor, resolve_stroke_erase, resolve_stroke_general, resolve_stroke_normal,
            resolve_stroke_normal_gamma, resolve_stroke_normal_simd,
        },
        blend_modes::{BlendSpace, LayerBlend},
        history::{TileSnapshot, UndoAction},
    },
    selection::SelectionManager,
};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPool;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::sync::Mutex;

mod render;
use render::srgb_to_linear;
pub(crate) use render::{RibbonSeg, SoftTip};

/// Available shapes for how a brush applies paint.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrushType {
    Soft,
    Pixel,
    /// A row of hairs, each painting its own line (see
    /// [`crate::brush_engine::bristle`]).
    Bristle,
    /// Its line, and fine lines to earlier points of the stroke nearby (see
    /// [`crate::brush_engine::sketch`]).
    Sketch,
    /// Parallel lines pinned to the canvas wherever it passes (see
    /// [`crate::brush_engine::hatching`]).
    Hatching,
    /// Each dab a cloud of small particles (see
    /// [`crate::brush_engine::engines::Spray`]).
    Spray,
    /// The tip broken up by a grain, filled more by pressing harder.
    Chalk,
    /// Curves swinging from points a while back to the pen.
    Curve,
    /// One shape in each cell of a grid it passes over.
    Grid,
    /// A normal map: the pen's tilt as the colour.
    TangentNormal,
    /// A swarm pulled along after the pen, each drawing its path.
    Particle,
}

impl BrushType {
    /// Every type, in the order the brush panel lists them.
    pub const ALL: [BrushType; 11] = [
        BrushType::Soft,
        BrushType::Pixel,
        BrushType::Bristle,
        BrushType::Sketch,
        BrushType::Hatching,
        BrushType::Spray,
        BrushType::Chalk,
        BrushType::Curve,
        BrushType::Grid,
        BrushType::TangentNormal,
        BrushType::Particle,
    ];

    /// Draws its own lines or shapes in place of the stroke's dabs.
    pub fn replaces_dabs(self) -> bool {
        matches!(
            self,
            BrushType::Curve | BrushType::Grid | BrushType::Particle
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StabilizerAlgorithm {
    None,
    Simple,
    Dynamic,
    /// Pulled string (lazy mouse): the brush trails the pen on a string.
    String,
    /// The path is smoothed when the pen lifts, and the stroke repainted.
    PostCorrection,
    /// Jitter filtered out by speed: smooth when slow, direct when fast.
    MotionFilter,
}

/// Gaussian circle brush tip: per-pixel alpha as a function of distance from
/// the (sub-pixel quantized) dab center.
struct GaussianTip {
    r_ceil: i32,
    radius: f32,
    r_sq: f32,
    inv_radius: f32,
    hardness: f32,
    /// Krita's anti-aliased edge (see `masks::auto_tip_alpha`): from here
    /// out the falloff's value here fades linearly to nothing at the edge.
    fade_start: f32,
    fade_base: f32,
    inv_fade_width: f32,
}

impl GaussianTip {
    fn new(r: f32, hardness: f32) -> Self {
        let fade_start = (r - 1.0).max(0.0);
        let inv_radius = if r > 0.0 { 1.0 / r } else { 0.0 };
        Self {
            r_ceil: r.ceil() as i32,
            radius: r,
            r_sq: r * r,
            inv_radius,
            hardness,
            fade_start,
            fade_base: super::masks::gaussian_falloff(fade_start * inv_radius, hardness),
            inv_fade_width: if r > fade_start {
                1.0 / (r - fade_start)
            } else {
                0.0
            },
        }
    }

    /// Scalar reference for one pixel; [`row_kernel`] must match it bit for bit.
    #[cfg(test)]
    fn alpha(&self, dist_sq: f32, dist: f32, t: f32) -> f32 {
        if dist_sq >= self.r_sq {
            return 0.0;
        }
        let alpha_factor = if dist > self.fade_start {
            self.fade_base * ((self.radius - dist) * self.inv_fade_width)
        } else {
            super::masks::gaussian_falloff(t, self.hardness)
        };
        alpha_factor.clamp(0.0, 1.0)
    }

    /// Tip alphas for one dab row, for tip columns `mx0..mx0 + out.len()`.
    /// `pdy` is the row's vertical offset from the dab center.
    ///
    /// Runs the AVX2 build of [`row_kernel`] when the CPU has it (8 lanes),
    /// else the baseline build (4 lanes); both are bit-identical.
    /// Columns `0..len` of a row (starting at mask column `mx0`) that can be
    /// inside the circle, i.e. where `pdx² < r² - pdy²`. Widened by a pixel
    /// on each side so float rounding can never cut off a covered pixel.
    fn chord(&self, pdy: f32, frac_x: f32, mx0: usize, len: usize) -> Range<usize> {
        let room = self.r_sq - pdy * pdy;
        if room <= 0.0 {
            return 0..0;
        }
        let half = room.sqrt();
        // pdx(i) = i + offset, as in `row_kernel`.
        let offset = mx0 as f32 - self.r_ceil as f32 + 0.5 - frac_x;
        let lo = (-half - offset).floor() - 1.0;
        let hi = (half - offset).ceil() + 1.0;
        let start = lo.max(0.0) as usize;
        let end = (hi.max(0.0) as usize).min(len);
        start.min(end)..end
    }

    fn row(&self, pdy: f32, frac_x: f32, mx0: usize, out: &mut [f32]) {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2, checked just above.
            unsafe { row_kernel_avx2(self, pdy, frac_x, mx0, out) };
            return;
        }
        row_kernel(self, pdy, frac_x, mx0, out);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn row_kernel_avx2(tip: &GaussianTip, pdy: f32, frac_x: f32, mx0: usize, out: &mut [f32]) {
    row_kernel(tip, pdy, frac_x, mx0, out);
}

/// `GaussianTip::alpha` for a run of columns, written as a straight loop
/// with branch-free selects so the compiler vectorizes it to whatever width
/// the enclosing function's target features allow. Each select mirrors the
/// scalar branch (`x > 0 ? x : 0` is exactly SSE `maxps(x, 0)`, etc.), so
/// every lane is bit-identical to `GaussianTip::alpha`.
#[inline(always)]
fn row_kernel(tip: &GaussianTip, pdy: f32, frac_x: f32, mx0: usize, out: &mut [f32]) {
    let r_ceil = tip.r_ceil as f32;
    let pdy_sq = pdy * pdy;
    let hardness = tip.hardness;
    let soft = hardness < 1.0;
    let soft_span = 1.0 - hardness;
    let base = mx0 as i32;
    for (i, slot) in out.iter_mut().enumerate() {
        let pdx = (base + i as i32) as f32 - r_ceil + 0.5 - frac_x;
        let dist_sq = pdx * pdx + pdy_sq;
        let dist = dist_sq.sqrt();
        let t = dist * tip.inv_radius;
        let mut alpha = 1.0;
        if soft {
            let v = (t - hardness) / soft_span;
            let v = if v > 0.0 { v } else { 0.0 };
            let v = if v < 1.0 { v } else { 1.0 };
            let falloff = 1.0 - v;
            let smooth = falloff * falloff * (3.0 - 2.0 * falloff);
            alpha = if t < hardness { 1.0 } else { smooth };
        }
        let faded = tip.fade_base * ((tip.radius - dist) * tip.inv_fade_width);
        alpha = if dist > tip.fade_start { faded } else { alpha };
        alpha = if alpha > 0.0 { alpha } else { 0.0 };
        alpha = if alpha < 1.0 { alpha } else { 1.0 };
        *slot = if dist_sq < tip.r_sq { alpha } else { 0.0 };
    }
}

/// User-facing brush configuration.
#[derive(Clone, Debug)]
pub struct Brush {
    pub brush_options: BrushOptions,
    pub is_changed: bool,
    pub brush_type: BrushType,
    pub pixel_perfect: bool,
    pub anti_aliasing: bool,
    pub jitter: f32,
    pub stabilizer: f32, // 0..1 (0 = off, 1 = max smoothing) - Used for Simple
    pub stabilizer_algorithm: StabilizerAlgorithm,
    pub stabilizer_mass: f32, // 0.01..1.0
    pub stabilizer_drag: f32, // 0.0..1.0
    /// The other stabiliser modes' settings.
    pub stabilizer_modes: crate::brush_engine::stabilizer::StabilizerModes,
    /// What changes from dab to dab besides pressure: tip angle and squash,
    /// tapers, speed, randomness. All off by default.
    pub dynamics: crate::brush_engine::dynamics::BrushDynamics,
    /// Inputs driving dab settings, each through its own curve.
    pub inputs: Vec<crate::brush_engine::dynamics::InputMapping>,
    /// How the inputs driving a setting come together, for the settings
    /// whose inputs don't each work on their own (see
    /// [`crate::brush_engine::dynamics::Combine`]).
    pub input_combine: Vec<(
        crate::brush_engine::dynamics::DabSetting,
        crate::brush_engine::dynamics::Combine,
    )>,
    /// Paper grain taking paint away from each dab; `None` for none.
    pub texture: Option<crate::brush_engine::texture::BrushTexture>,
    /// How the paint blends onto the layer (multiply, screen, add…), like a
    /// layer's blend mode but per stroke.
    pub paint_blend: LayerBlend,
    /// Airbrush: dabs per second added where the pen is while it's down,
    /// so paint builds up when it's held still (0 = off).
    pub airbrush_rate: f32,
    /// Dual brush: a second tip that masks this one; `None` for none.
    pub dual: Option<crate::brush_engine::dual::DualTip>,
    /// Watercolour edges: how much the middle of a stroke thins when the
    /// pen lifts, its paint pooling at the rim (0 = off, up to 0.95).
    pub wet_edge: f32,
    /// How wide the pooled rim is, in canvas pixels.
    pub wet_edge_width: f32,
    /// The hairs of a [`BrushType::Bristle`] brush.
    pub bristles: crate::brush_engine::bristle::Bristles,
    /// The joining lines of a [`BrushType::Sketch`] brush.
    pub sketch: crate::brush_engine::sketch::Sketch,
    /// The lines of a [`BrushType::Hatching`] brush.
    pub hatching: crate::brush_engine::hatching::Hatching,
    /// The settings of the spray, chalk, curve, grid, tangent normal and
    /// particle types.
    pub engines: crate::brush_engine::engines::Engines,
    /// Impasto: the paint's thickness laid down with it (on the layer's
    /// heights); `None` for flat paint.
    pub impasto: Option<crate::canvas::impasto::Impasto>,
    /// Wet paint: laid down as water and pigment that spread, gather at
    /// their edges and dry (see [`crate::canvas::wet`]); `None` for paint
    /// that's dry at once.
    pub wet: Option<crate::canvas::wet::WetPaint>,
    /// Hard edges: tip coverage below this share
    /// (0..1) is dropped and the rest painted at full strength (0 = off).
    pub sharpness: f32,
    /// Hard edges' soft band (0..1): coverage
    /// down to this share below the cut keeps its own strength.
    pub sharpness_softness: f32,
    /// Colour mixing: the brush smudges the paint under it, mixing in its
    /// colour (it then paints through the Smudge tool's engine); `None` for
    /// a plain brush.
    pub mixing: Option<crate::brush_engine::brush_options::Mixing>,
    /// The secondary colour, for input mappings that mix it in (set when
    /// a stroke starts; not part of the brush's settings).
    pub second_color: Color32,
    /// Wash mode: the opacity pen pressure gives the dabs being painted
    /// (set with the pressure while painting; not a setting).
    pub wash_opacity: f32,
}

/// Shared inputs for painting one batch of dabs into the stroke buffers.
struct BatchCtx<'a> {
    canvas: &'a Canvas,
    selection: Option<&'a SelectionManager>,
    dabs: &'a [PlacedDab],
    buffers: &'a FxHashMap<(usize, usize), Mutex<StrokeBuffer>>,
    r: f32,
    blend_mode: BlendMode,
    /// The document's blending space (gamma documents mix stored values).
    space: BlendSpace,
    color: StrokeColor,
    /// Coverage multiplier when resolving: the stroke opacity in wash mode.
    cap: f32,
    /// Soft brushes use anti-aliased selection edges; pixel brushes keep
    /// hard, pixel-center edges (pixel art).
    antialiased_selection: bool,
    /// The layer's transparency is locked: paint only recolours.
    alpha_lock: bool,
    /// Where the dabs accumulate.
    target: Target,
    /// The brush's texture, applied to every dab.
    texture: Option<&'a crate::brush_engine::texture::BrushTexture>,
    /// The dabs differ in colour: their colours are accumulated per pixel.
    colored: bool,
    /// How the stroke blends onto the layer.
    mode: LayerBlend,
    /// Resolve with [`resolve_stroke_general`] (a blend mode or per-pixel
    /// colours) rather than the fast single-colour resolves.
    general: bool,
    /// Which tail segment is the newer.
    tail_newer: usize,
    /// The stroke's grain, when the texture is placed (moved, turned...).
    grain: Option<crate::brush_engine::texture::StrokeGrain>,
    /// A dual brush: how its mask combines with the coverage.
    dual: Option<crate::brush_engine::dual::DualMode>,
    /// The dabs paint their tips' own colours.
    tip_colors: bool,
    /// A hatching brush: its lines, over every dab.
    hatch: Option<&'a crate::brush_engine::hatching::Hatching>,
    /// A chalk brush: its grain, over every dab.
    chalk: Option<&'a crate::brush_engine::engines::Chalk>,
    /// Paint thickness going down with the paint (and whether it's taken
    /// away: an eraser), on a layer with impasto heights.
    impasto: Option<(crate::canvas::impasto::Impasto, bool)>,
    /// Hard edges: the brush's [`Brush::sharpness`] (0 = off) and its
    /// soft band.
    sharpness: f32,
    sharpness_softness: f32,
    /// The batch's stroke strength (before each dab's own), which a dab's
    /// stamped alphas are scaled by.
    strength: f32,
    /// Wash mode (alpha darken): the flow each dab moves the
    /// coverage toward its opacity with; its stamped alphas are then the
    /// tip's coverage alone.
    wash: Option<f32>,
}

/// Where a batch of dabs accumulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// The stroke's coverage, for good.
    Stroke,
    /// A segment (0 or 1) of the redrawable tail (an end taper still to
    /// come): merged into the stroke once it's far enough behind the pen,
    /// or cleared and drawn again, tapered, when the pen lifts.
    Tail(usize),
    /// A dual brush's mask: the second tip's coverage, which the stroke's
    /// is combined with when resolving (nothing is resolved on its own).
    Mask,
}

impl Brush {
    /// Whether any dynamics are on (fixed ones or input mappings).
    pub fn has_dynamics(&self) -> bool {
        self.dynamics.is_active() || !self.inputs.is_empty()
    }

    /// Wet paint, when the pen lifts: what the stroke covered becomes water
    /// and pigment over the paint that was there (on a layer that takes wet
    /// paint), noted in `undo_action` as it was.
    pub(crate) fn lay_wet(
        &self,
        canvas: &Canvas,
        stroke_tiles: &mut StrokeTiles,
        undo_action: &mut UndoAction,
    ) {
        let Some(paint) = self.wet else {
            return;
        };
        let Some(layer) = canvas.layers.get(canvas.active_layer_idx) else {
            return;
        };
        let Some(wet) = layer.wet.as_deref() else {
            return;
        };
        let o = &self.brush_options;
        // An eraser's stroke stays as it erased (the wet paint under it
        // dries as it is).
        if o.blend_mode == BlendMode::Eraser {
            return;
        }
        // (A brush whose dabs vary in colour lays its own colour wet.)
        let colour = crate::canvas::wet::linear(o.color);
        let side = canvas.tile_size();
        for (&key, buffer) in &stroke_tiles.buffers {
            let mut buffer = buffer.lock().unwrap_or_else(|e| e.into_inner());
            let k = (key.0 as i32, key.1 as i32);
            crate::canvas::wet::record_undo(undo_action, layer.id, k, wet.tile(k));
            // (The coverage has the stroke's opacity in it already.)
            let shown = wet.lay(
                k,
                &buffer.original,
                &buffer.coverage,
                |_| colour,
                paint,
                layer.alpha_locked,
            );
            if let Some(tile) = canvas.lock_tile(key.0, key.1) {
                let mut tile = tile.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(data) = tile.data_mut()
                    && data.len() == shown.len()
                {
                    data.copy_from_slice(&shown);
                    tile.is_empty = shown.iter().all(|&p| p == Color32::TRANSPARENT);
                }
            }
            buffer.damage = Some([0, 0, side, side]);
            stroke_tiles.dirty.insert(key);
        }
    }

    /// What this brush does to the paint's thickness on `canvas`'s active
    /// layer: its impasto, or an eraser's taking it away; `None` on a layer
    /// without heights.
    pub(crate) fn paints_heights(
        &self,
        canvas: &Canvas,
    ) -> Option<(crate::canvas::impasto::Impasto, bool)> {
        canvas
            .layers
            .get(canvas.active_layer_idx)?
            .height
            .as_ref()?;
        let erase = self.brush_options.blend_mode == BlendMode::Eraser;
        (erase || self.impasto.is_some()).then(|| (self.impasto.unwrap_or_default(), erase))
    }

    /// Whether the dabs' colours can differ (colour randomness, colour
    /// tips, inputs driving the colour).
    pub fn varies_color(&self) -> bool {
        matches!(self.brush_type, BrushType::TangentNormal | BrushType::Grid)
            || self.dynamics.random.has_color()
            || self.paints_tip_colors()
            || self.brush_options.color_source != ColorSource::Plain
            || self.inputs.iter().any(|m| m.setting.is_color())
    }

    /// Whether dabs can differ from one another (dynamics, several tips),
    /// so each is planned on its own.
    pub fn varies_per_dab(&self) -> bool {
        self.has_dynamics()
            || self.brush_options.tip_count() > 1
            || !matches!(self.brush_type, BrushType::Soft | BrushType::Pixel)
            || self.paints_tip_colors()
            || self.brush_options.color_source != ColorSource::Plain
    }

    /// How far from its centre a dab can paint, generously (for copying it
    /// across the canvas's edges with wrap-around): turned tips' corners,
    /// dynamics growing it, a bristle brush's hairs.
    pub fn wrap_reach(&self) -> f32 {
        let spread = match self.brush_type {
            BrushType::Bristle => self.bristles.spread.max(1.0) + self.bristles.thickness,
            BrushType::Sketch => self.sketch.reach,
            BrushType::Curve => 6.0,
            BrushType::Particle => 8.0,
            BrushType::Grid => 1.0 + self.engines.grid.cell / self.brush_options.diameter.max(1.0),
            _ => 1.0,
        };
        self.brush_options.diameter * 1.5 * spread + 4.0
    }

    /// The brush lays its image tip along the stroke as a ribbon.
    pub fn is_ribbon(&self) -> bool {
        self.brush_options.placement == crate::brush_engine::brush_options::Placement::Ribbon
            && matches!(self.brush_options.pixel_shape, PixelBrushShape::Custom(_))
    }

    /// Krita's colour smudge with a lightness tip: each dab also lays the
    /// tip's grey on the layer's lightness map (its relief).
    pub fn lays_lightness(&self) -> bool {
        let o = &self.brush_options;
        self.mixing.is_some_and(|m| m.krita.is_some())
            && o.tip_mapping == crate::brush_engine::brush_options::TipMapping::Lightness
            && matches!(&o.pixel_shape, PixelBrushShape::Custom(tip) if tip.has_colors())
    }

    /// The dabs paint their tips' own colours (smooth image tips only).
    pub fn paints_tip_colors(&self) -> bool {
        self.brush_options.paints_tip_colors()
            && self.anti_aliasing
            && self.brush_type != BrushType::Pixel
    }

    /// The tip turns with the stroke's direction (a bristle brush's hairs
    /// always lie across it).
    pub fn follows_stroke(&self) -> bool {
        self.dynamics.tip.follow_stroke || self.brush_type == BrushType::Bristle
    }

    /// The stroke smoothing this brush asks for.
    pub fn stabilizer_settings(&self) -> crate::brush_engine::stabilizer::StabilizerSettings {
        crate::brush_engine::stabilizer::StabilizerSettings {
            algorithm: self.stabilizer_algorithm,
            strength: self.stabilizer,
            mass: self.stabilizer_mass,
            drag: self.stabilizer_drag,
            modes: self.stabilizer_modes,
            view_scale: 1.0,
        }
    }
}

impl Brush {
    /// Create a standard soft brush with the given radius, hardness, base color and spacing.
    pub fn new(diameter: f32, hardness: f32, color: Color32, spacing: f32) -> Self {
        Self {
            brush_options: BrushOptions::new(diameter, hardness, color, spacing),
            brush_type: BrushType::Soft,
            pixel_perfect: false,
            anti_aliasing: true,
            jitter: 0.0,
            stabilizer: 0.0,
            stabilizer_algorithm: StabilizerAlgorithm::None,
            stabilizer_mass: 0.1,
            stabilizer_drag: 0.5,
            stabilizer_modes: Default::default(),
            is_changed: false,
            dynamics: Default::default(),
            inputs: Vec::new(),
            input_combine: Vec::new(),
            texture: None,
            paint_blend: LayerBlend::Normal,
            airbrush_rate: 0.0,
            dual: None,
            wet_edge: 0.0,
            wet_edge_width: 6.0,
            bristles: Default::default(),
            sketch: Default::default(),
            hatching: Default::default(),
            engines: Default::default(),
            impasto: None,
            wet: None,
            sharpness: 0.0,
            sharpness_softness: 0.0,
            mixing: None,
            second_color: Color32::WHITE,
            wash_opacity: 1.0,
        }
    }

    /// Convenience constructor for a pixel-perfect pen.
    pub fn new_pixel(diameter: f32, color: Color32) -> Self {
        Self {
            brush_options: BrushOptions::new(diameter, 100.0, color, 10.0),
            brush_type: BrushType::Pixel,
            pixel_perfect: true,
            anti_aliasing: false,
            jitter: 0.0,
            stabilizer: 0.0,
            stabilizer_algorithm: StabilizerAlgorithm::None,
            stabilizer_mass: 0.1,
            stabilizer_drag: 0.5,
            stabilizer_modes: Default::default(),
            is_changed: false,
            dynamics: Default::default(),
            inputs: Vec::new(),
            input_combine: Vec::new(),
            texture: None,
            paint_blend: LayerBlend::Normal,
            airbrush_rate: 0.0,
            dual: None,
            wet_edge: 0.0,
            wet_edge_width: 6.0,
            bristles: Default::default(),
            sketch: Default::default(),
            hatching: Default::default(),
            engines: Default::default(),
            impasto: None,
            wet: None,
            sharpness: 0.0,
            sharpness_softness: 0.0,
            mixing: None,
            second_color: Color32::WHITE,
            wash_opacity: 1.0,
        }
    }

    /// Paint a batch of dabs (in stroke order) with the current brush.
    ///
    /// Dabs are grouped per tile; each tile accumulates its dabs into the
    /// stroke's coverage buffer in order, then its touched pixels are
    /// resolved once. The result is identical to painting the dabs one by
    /// one, with each tile locked once per batch.
    pub(crate) fn dabs(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        self.dabs_oriented(
            pool,
            canvas,
            selection,
            centers,
            None,
            undo_action,
            stroke_tiles,
        );
    }

    /// [`Self::dabs`] with a tip orientation per dab (mirror copies turn a
    /// custom tip with them).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dabs_oriented(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        orients: Option<&[[f32; 4]]>,
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        let r = self.brush_options.diameter / 2.0;
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);
        let tile_size = canvas.tile_size();
        let dabs: Vec<PlacedDab> = centers
            .iter()
            .enumerate()
            .filter_map(|(i, &center)| {
                let bounds = calc_dab_bounds(center, r, canvas_w, canvas_h, tile_size)?;
                let mut dab = PlacedDab::new(center, bounds, r);
                if let Some(o) = orients.and_then(|o| o.get(i)) {
                    dab.orient = *o;
                    dab.rigid = is_rigid(*o);
                }
                Some(dab)
            })
            .collect();
        self.paint_placed(
            pool,
            canvas,
            selection,
            dabs,
            Target::Stroke,
            undo_action,
            stroke_tiles,
        );
    }

    /// [`Self::dabs_oriented`] with each dab varied by its dynamics
    /// (`vars[i]` for dab `i`; mirror copies share their original's): its
    /// size, strength and the tip's turn and squash.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dabs_varied(
        &self,
        pool: &ThreadPool,
        canvas: &Canvas,
        selection: Option<&SelectionManager>,
        centers: &[Vec2],
        vars: &[crate::brush_engine::dynamics::DabVar],
        orients: Option<&[[f32; 4]]>,
        target: Target,
        undo_action: &mut UndoAction,
        stroke_tiles: &mut StrokeTiles,
    ) {
        use crate::brush_engine::dynamics::compose;
        let base_r = self.brush_options.diameter / 2.0;
        // A bristle brush: each dab is one small dab per hair.
        let hair_dabs;
        let (centers, vars, orients) = if self.brush_type == BrushType::Bristle {
            hair_dabs = self.hair_dabs(centers, vars, orients);
            (&hair_dabs.0[..], &hair_dabs.1[..], None)
        } else if self.brush_type == BrushType::Spray {
            hair_dabs = self.spray_dabs(centers, vars);
            (&hair_dabs.0[..], &hair_dabs.1[..], None)
        } else {
            (centers, vars, orients)
        };
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);
        let tile_size = canvas.tile_size();
        let colored = self.varies_color();
        let linear = canvas.blend_space == BlendSpace::Linear;
        // How far past its radius a turned tip reaches: a square's (or an
        // image's) corners.
        let tips = self.brush_options.tip_shapes();
        let corner_reach = tips
            .iter()
            .map(|shape| match shape {
                PixelBrushShape::Circle => 1.0,
                PixelBrushShape::Square => std::f32::consts::SQRT_2,
                PixelBrushShape::Custom(tip) => tip.corner_reach(),
            })
            .fold(1.0, f32::max);
        let last_tip = (tips.len() - 1) as u8;
        let dabs: Vec<PlacedDab> = centers
            .iter()
            .enumerate()
            .filter_map(|(i, &center)| {
                let var = vars.get(i % vars.len().max(1)).copied().unwrap_or_default();
                let r = (base_r * var.scale).max(0.25);
                if var.strength <= 0.0 || base_r * var.scale < 0.1 {
                    return None;
                }
                let mirror = orients.and_then(|o| o.get(i)).copied();
                let orient = match mirror {
                    Some(m) => compose(var.orient, m),
                    None => var.orient,
                };
                let upright = orient == crate::brush_engine::dynamics::IDENTITY;
                let reach = if upright { r } else { r * corner_reach };
                let bounds = calc_dab_bounds(center, reach, canvas_w, canvas_h, tile_size)?;
                let mut dab = PlacedDab::new(center, bounds, r);
                dab.orient = orient;
                dab.rigid = is_rigid(orient);
                dab.reach = reach;
                dab.strength = var.strength;
                dab.tip = var.tip.min(last_tip);
                dab.hatch = var.hatch;
                dab.hardness = var.hardness;
                dab.texture = var.texture;
                dab.sharp = var.sharpness;
                dab.soft = crate::brush_engine::dab::soft_level(var.softness);
                dab.flow = var.flow;
                dab.lightness = var.lightness;
                dab.smudge = var.smudge;
                dab.color_rate = var.color_rate;
                dab.thickness = var.thickness;
                if colored {
                    let own = var.base.map_or(self.brush_options.color, |c| {
                        let [r, g, b] = c.map(|v| (v * 255.0).round() as u8);
                        Color32::from_rgb(r, g, b)
                    });
                    let base = if var.mix > 0.0 {
                        mix_colors(own, self.second_color, var.mix)
                    } else {
                        own
                    };
                    let srgb = crate::brush_engine::dynamics::shift_hsv(base, var.hsv)
                        .map(|c| c * var.darken.clamp(0.0, 1.0));
                    dab.color = if linear {
                        srgb.map(srgb_to_linear)
                    } else {
                        srgb
                    };
                }
                Some(dab)
            })
            .collect();
        self.paint_placed(
            pool,
            canvas,
            selection,
            dabs,
            target,
            undo_action,
            stroke_tiles,
        );
    }

    /// A spray brush's dabs: each of `centers` (with its variation) as a
    /// cloud of particles, the same for the same place.
    fn spray_dabs(
        &self,
        centers: &[Vec2],
        vars: &[crate::brush_engine::dynamics::DabVar],
    ) -> (Vec<Vec2>, Vec<crate::brush_engine::dynamics::DabVar>) {
        use crate::brush_engine::dynamics::{compose, tip_orientation};
        let base_r = self.brush_options.diameter * 0.5;
        let spray = &self.engines.spray;
        let mut out = (Vec::new(), Vec::new());
        for (i, &center) in centers.iter().enumerate() {
            let var = vars.get(i % vars.len().max(1)).copied().unwrap_or_default();
            let seed = center.x.to_bits() ^ center.y.to_bits().rotate_left(11) ^ i as u32;
            for p in spray.particles(seed, base_r * var.scale) {
                out.0.push(center + p.offset);
                let mut v = var;
                v.scale *= p.scale;
                if p.angle != 0.0 {
                    v.orient = compose(var.orient, tip_orientation(p.angle, 1.0));
                }
                out.1.push(v);
            }
        }
        out
    }

    /// A bristle brush's dabs, one per hair of each of `centers` (with its
    /// variation and mirror copy): the hairs' centres and variations.
    fn hair_dabs(
        &self,
        centers: &[Vec2],
        vars: &[crate::brush_engine::dynamics::DabVar],
        orients: Option<&[[f32; 4]]>,
    ) -> (Vec<Vec2>, Vec<crate::brush_engine::dynamics::DabVar>) {
        use crate::brush_engine::dynamics::{DabVar, IDENTITY, compose};
        let b = &self.bristles;
        let hairs = b.hairs();
        let base_r = (self.brush_options.diameter / 2.0).max(0.25);
        let mut out_centers = Vec::with_capacity(centers.len() * hairs.len());
        let mut out_vars = Vec::with_capacity(out_centers.capacity());
        for (i, &center) in centers.iter().enumerate() {
            let var = vars.get(i % vars.len().max(1)).copied().unwrap_or_default();
            let orient = match orients.and_then(|o| o.get(i)) {
                Some(&m) => compose(var.orient, m),
                None => var.orient,
            };
            // Tip frame → canvas: the inverse of `orient`.
            let [a, bb, c, d] = orient;
            let det = a * d - bb * c;
            let inv = if det.abs() > 1e-9 {
                [d / det, -bb / det, -c / det, a / det]
            } else {
                IDENTITY
            };
            let spread = base_r * var.scale * b.spread;
            for hair in &hairs {
                let ink = b.ink_left(hair, var.along);
                if ink <= 0.0 {
                    continue;
                }
                let (tx, ty) = (hair.offset.x * spread, hair.offset.y * spread);
                let offset = Vec2::new(inv[0] * tx + inv[1] * ty, inv[2] * tx + inv[3] * ty);
                out_centers.push(center + offset);
                let hair_r = (b.thickness * 0.5 * hair.thickness).max(0.3);
                out_vars.push(DabVar {
                    scale: hair_r / base_r,
                    strength: var.strength * hair.strength * ink,
                    orient: IDENTITY,
                    ..var
                });
            }
        }
        (out_centers, out_vars)
    }
}

/// `a` and `b` mixed, `t` (0..1) of the way to `b`, in unmultiplied sRGB.
fn mix_colors(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let (a, b) = (a.to_srgba_unmultiplied(), b.to_srgba_unmultiplied());
    let ch = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
    Color32::from_rgb(ch(0), ch(1), ch(2))
}

/// Whether `m` only turns or mirrors (keeps a circle a circle).
#[inline]
fn is_rigid(m: [f32; 4]) -> bool {
    let [a, b, c, d] = m;
    ((a * a + c * c) - 1.0).abs() < 1e-4
        && ((b * b + d * d) - 1.0).abs() < 1e-4
        && (a * b + c * d).abs() < 1e-4
}

/// Named preset that can be displayed in the UI and cloned into the active brush.
#[derive(Clone, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub brush: Brush,
    /// Where a preset the user saved or imported is kept; `None` for the
    /// built-in ones.
    pub file: Option<std::path::PathBuf>,
}

#[cfg(test)]
mod tests;
