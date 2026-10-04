//! Brush settings that are plain data: size, hardness, spacing, flow,
//! pixel shapes and painting modes.

use eframe::egui::Color32;

use crate::brush_engine::hardness::SoftnessCurve;
use crate::brush_engine::hardness::SoftnessSelector;

#[derive(Clone, Debug)]
pub enum PixelBrushShape {
    Circle,
    Square,
    /// An image tip (see [`crate::brush_engine::tip`]).
    Custom(std::sync::Arc<crate::brush_engine::tip::TipMask>),
}

impl PartialEq for PixelBrushShape {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Circle, Self::Circle) | (Self::Square, Self::Square) => true,
            // The same shared tip, or an identical one.
            (Self::Custom(a), Self::Custom(b)) => std::sync::Arc::ptr_eq(a, b) || a == b,
            _ => false,
        }
    }
}

/// Which of a brush's tips each dab uses, when it has several (like
/// GIMP's image hoses).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TipOrder {
    /// One after the other, over and over.
    #[default]
    Sequence,
    /// A tip at random for each dab.
    Random,
    /// Light pressure the first tip, full pressure the last.
    Pressure,
    /// By the direction the stroke goes, the turn split between the tips.
    Direction,
}

impl TipOrder {
    pub const ALL: [TipOrder; 4] = [
        Self::Sequence,
        Self::Random,
        Self::Pressure,
        Self::Direction,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sequence => "In turn",
            Self::Random => "Random",
            Self::Pressure => "Pressure",
            Self::Direction => "Direction",
        }
    }
}

/// How a colour tip's picture paints.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TipMapping {
    /// Its own colours.
    #[default]
    Colors,
    /// The brush colour, its lightness from the picture's (a lightness
    /// map: mid grey keeps the colour, black and white take it
    /// to black and white).
    Lightness,
    /// From the brush colour (dark) to the secondary colour (light) by the
    /// picture's lightness (a gradient map, foreground to background).
    Gradient,
}

impl TipMapping {
    pub const ALL: [TipMapping; 3] = [Self::Colors, Self::Lightness, Self::Gradient];

    pub fn label(self) -> &'static str {
        match self {
            Self::Colors => "Its colours",
            Self::Lightness => "Lightness",
            Self::Gradient => "Gradient",
        }
    }

    /// A tip texel's colour `c` (sRGB 0..1) as this paints it, with the
    /// brush colour `brush` and secondary colour `second` (sRGB 0..1).
    pub fn map(self, c: [f32; 3], brush: [f32; 3], second: [f32; 3]) -> [f32; 3] {
        let grey = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        match self {
            Self::Colors => c,
            Self::Lightness => {
                // Peter Schatz's curve through 0, the brush's
                // lightness at mid grey, and 1.
                let l = lightness(brush);
                let b = 4.0 * l - 1.0;
                let target = ((1.0 - b) * grey * grey + b * grey).clamp(0.0, 1.0);
                set_lightness(brush, target)
            }
            Self::Gradient => std::array::from_fn(|i| brush[i] + (second[i] - brush[i]) * grey),
        }
    }
}

/// HSL lightness: the middle of the brightest and darkest channel.
fn lightness(c: [f32; 3]) -> f32 {
    let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    (max + min) * 0.5
}

/// `c` moved to HSL lightness `l`, kept in range by pulling the channels
/// toward the lightness (hue kept).
fn set_lightness(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lightness(c);
    let c = c.map(|v| v + d);
    let l = lightness(c);
    let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let mut out = c;
    if min < 0.0 && l - min > 0.0 {
        out = out.map(|v| l + (v - l) * l / (l - min));
    }
    if max > 1.0 && max - l > 0.0 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (max - l));
    }
    out.map(|v| v.clamp(0.0, 1.0))
}

/// How a brush lays its tip down.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Placement {
    /// A dab at every step (every brush).
    #[default]
    Dabs,
    /// The tip's picture laid along the stroke and repeated, its height
    /// across the stroke (ribbons, lace, borders).
    Ribbon,
}

/// Colour mixing: each dab picks up the
/// paint under it, carries it along and mixes in the brush colour, so the
/// brush lays down its colour blended with whatever it drags.
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Mixing {
    /// How much of the carried paint stays with the brush each dab
    /// (0 = barely drags, 1 = smears a colour a long way).
    pub smudge_length: f32,
    /// How much brush colour is mixed into the carried paint per brush
    /// width travelled (0 = a blender: no colour of its own).
    pub color_rate: f32,
    /// Pen pressure scales the smudge length.
    pub pressure_length: bool,
    /// Pen pressure scales the colour rate.
    pub pressure_color: bool,
    /// An imported brush's own colour smudge, instead of this app's:
    /// `smudge_length` is then its smudge rate.
    pub krita: Option<KritaSmudge>,
}

impl Default for Mixing {
    fn default() -> Self {
        Self {
            smudge_length: 0.8,
            color_rate: 0.5,
            pressure_length: false,
            pressure_color: false,
            krita: None,
        }
    }
}

/// How an imported colour smudge mixes:
/// each dab reads the layer where the previous dab was and lays it (or one
/// colour sampled there, in dulling mode) over the layer under it, then the
/// brush colour at the colour rate squared, through the tip.
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct KritaSmudge {
    /// Dulling: one colour sampled from under the previous dab (its
    /// weighted average), rather than the pixels themselves (smearing).
    pub dulling: bool,
    /// The paint's transparency is smeared too (copied), rather than the
    /// picked-up paint only going over what's there.
    pub smear_alpha: bool,
    /// Dulling: how much of the dab the colour is sampled from (0 its
    /// centre, 1 all of it).
    pub radius: f32,
}

impl Default for KritaSmudge {
    fn default() -> Self {
        Self {
            dulling: false,
            smear_alpha: true,
            radius: 0.0,
        }
    }
}

/// Krita's auto-tip extras for round and square tips.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AutoTip {
    /// The tip's shape repeated this many times round its centre: with the
    /// tip squashed, a star of that many points (2 = the plain shape).
    pub spikes: u32,
    /// Horizontal and vertical fade: solid out to this share of the tip's
    /// width and height, then fading to its edge (1 = no fade).
    pub fade: [f32; 2],
    /// Share of the tip's pixels painted, the rest left out at random
    /// (1 = all of them).
    pub density: f32,
    /// How much each pixel's strength varies at random: a grainy, rough
    /// tip (0 = none).
    pub randomness: f32,
}

impl Default for AutoTip {
    fn default() -> Self {
        Self {
            spikes: 2,
            fade: [1.0; 2],
            density: 1.0,
            randomness: 0.0,
        }
    }
}

impl AutoTip {
    pub fn is_active(&self) -> bool {
        self.spikes > 2 || self.has_fade() || self.density < 1.0 || self.randomness > 0.0
    }

    pub fn has_fade(&self) -> bool {
        self.fade[0] < 1.0 || self.fade[1] < 1.0
    }

    /// The spikes' folding, worked out once for a batch of dabs.
    pub fn spikes(&self) -> Option<Spikes> {
        (self.spikes > 2).then(|| Spikes::new(self.spikes))
    }

    /// The fade's coefficients at `softness` for [`Self::fade_with`]: one
    /// over each fade squared.
    #[inline]
    pub fn fade_coeffs(&self, softness: f32) -> [f32; 2] {
        self.fade.map(|f| (f * softness).max(1e-4).powi(-2))
    }

    /// The fade at `(u, v)`, the offset over the tip's half width and half
    /// height (1 at the edge), with the fades scaled by `softness`
    /// (Krita's default mask: solid inside the fade ellipse, then falling
    /// to nothing at the edge).
    #[inline]
    pub fn fade_at(&self, u: f32, v: f32, softness: f32) -> f32 {
        Self::fade_with(u, v, self.fade_coeffs(softness))
    }

    /// [`Self::fade_at`] with the coefficients worked out.
    #[inline]
    pub fn fade_with(u: f32, v: f32, [kh, kv]: [f32; 2]) -> f32 {
        let (u2, v2) = (u * u, v * v);
        let n = u2 + v2;
        let nf = u2 * kh + v2 * kv;
        if nf <= 1.0 {
            return 1.0;
        }
        if n >= 1.0 {
            return 0.0;
        }
        (1.0 - n * (nf - 1.0) / (nf - n)).clamp(0.0, 1.0)
    }

    /// A pixel's strength factor from the density and randomness, `seed`
    /// telling dabs apart (the same pixel of the same dab always gets the
    /// same).
    #[inline]
    pub fn grain(&self, seed: u32, gx: usize, gy: usize) -> f32 {
        let h = hash3(seed, gx as u32, gy as u32);
        if self.density < 1.0 && unit(h) >= self.density {
            return 0.0;
        }
        if self.randomness > 0.0 {
            1.0 - self.randomness * unit(h.rotate_left(16).wrapping_mul(0x9E37_79B9))
        } else {
            1.0
        }
    }
}

/// Folding a tip into its first spike's wedge.
pub struct Spikes {
    inv_wedge: f32,
    /// cos and sin of each whole number of wedges, to turn back by.
    turns: Vec<(f32, f32)>,
}

impl Spikes {
    fn new(spikes: u32) -> Self {
        let wedge = std::f32::consts::TAU / spikes as f32;
        Self {
            inv_wedge: 1.0 / wedge,
            turns: (0..=spikes / 2 + 1)
                .map(|k| {
                    let (s, c) = (k as f32 * wedge).sin_cos();
                    (c, s)
                })
                .collect(),
        }
    }

    /// `(x, y)`, in tip pixels from the centre (squashed to `ratio`), folded
    /// into the first spike's wedge, as Krita folds it before squashing.
    #[inline]
    pub fn fold(&self, (x, y): (f32, f32), ratio: f32) -> (f32, f32) {
        let y = y.abs() * ratio;
        // The wedge the point is in, its angle 0..π (y folded up).
        let k = (fast_atan2(y, x) * self.inv_wedge + 0.5) as usize;
        let (c, s) = self.turns[k.min(self.turns.len() - 1)];
        // Turned back by k wedges.
        (c * x + s * y, (c * y - s * x) / ratio)
    }
}

/// `atan2(y, x)` to within 1e-5 radians, for `y >= 0` (0..π): a
/// polynomial rather than libm's, per pixel.
#[inline]
fn fast_atan2(y: f32, x: f32) -> f32 {
    let (ax, ay) = (x.abs(), y);
    let (lo, hi) = (ax.min(ay), ax.max(ay));
    if hi == 0.0 {
        return 0.0;
    }
    let z = lo / hi;
    let z2 = z * z;
    let mut a = z
        * (0.999_977_26
            + z2 * (-0.332_623_47
                + z2 * (0.193_543_46
                    + z2 * (-0.116_432_87 + z2 * (0.052_653_32 + z2 * -0.011_721_2)))));
    if ay > ax {
        a = std::f32::consts::FRAC_PI_2 - a;
    }
    if x < 0.0 { std::f32::consts::PI - a } else { a }
}

/// A well-mixed hash of three numbers.
#[inline]
pub fn hash3(a: u32, b: u32, c: u32) -> u32 {
    let mut h = a
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(b.wrapping_mul(0x85EB_CA77))
        .wrapping_add(c.wrapping_mul(0xC2B2_AE3D));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^ (h >> 15)
}

/// `h` as 0..1.
#[inline]
pub fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Where a brush's colour comes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ColorSource {
    /// The brush colour.
    #[default]
    Plain,
    /// A random colour for each dab.
    UniformRandom,
    /// A random colour for each pixel.
    TotalRandom,
    /// A picture pinned to the canvas, from the brush colour where it's
    /// dark to the secondary colour where it's light, at `scale`.
    Pattern {
        pattern: std::sync::Arc<crate::brush_engine::texture::Pattern>,
        scale: f32,
    },
}

impl ColorSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Plain => "Brush colour",
            Self::UniformRandom => "Random each dab",
            Self::TotalRandom => "Random each pixel",
            Self::Pattern { .. } => "Pattern",
        }
    }

    /// Each pixel gets its own colour.
    pub fn per_pixel(&self) -> bool {
        matches!(self, Self::TotalRandom | Self::Pattern { .. })
    }
}

/// Blending strategy for how source color affects the destination.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlendMode {
    Normal,
    Eraser,
}

/// How a stroke's dabs combine.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaintingMode {
    /// Every dab adds paint at flow × opacity, so overlaps keep building up.
    BuildUp,
    /// Dabs build up at flow, but the stroke as a whole never exceeds its
    /// opacity (overlapping dabs within one stroke don't darken past it).
    Wash,
}

#[derive(Clone, Debug)]
pub struct BrushOptions {
    pub diameter: f32,
    pub hardness: f32, // 0..100
    pub softness_selector: SoftnessSelector,
    pub softness_curve: SoftnessCurve,
    /// How a curve tip softens for a dab whose Softness input is below
    /// full.
    pub softening: crate::brush_engine::hardness::Softening,
    pub pixel_shape: PixelBrushShape,
    /// More image tips the dabs alternate with (after `pixel_shape`, when
    /// that's an image tip too); empty for one tip.
    pub extra_tips: Vec<std::sync::Arc<crate::brush_engine::tip::TipMask>>,
    /// How the dabs pick among the tips.
    pub tip_order: TipOrder,
    /// Paint with a colour tip's own colours rather than the brush colour
    /// (decorations: flowers, stitches, chains).
    pub tip_colors: bool,
    /// How a colour tip's colours paint, with `tip_colors`.
    pub tip_mapping: TipMapping,
    /// Dabs, or the tip laid along the stroke as a ribbon.
    pub placement: Placement,
    pub color: Color32,
    pub spacing: f32, // Percentage of diameter (0..100+)
    /// Auto spacing: dabs this many times the square root of the
    /// diameter apart, rather than `spacing` (big tips closer together for
    /// their size, as Krita's auto spacing); `None` for off.
    pub auto_spacing: Option<f32>,
    /// Spikes, fades, density and randomness of a round or square tip.
    pub auto_tip: AutoTip,
    /// Where the colour comes from.
    pub color_source: ColorSource,
    pub flow: f32,    // 0..100
    pub opacity: f32, // 0..1
    pub blend_mode: BlendMode,
    pub painting_mode: PaintingMode,
    /// Pen pressure scales the diameter, down to `pressure_min_size` of it.
    pub pressure_size: bool,
    /// Diameter fraction (0..1) at zero pressure.
    pub pressure_min_size: f32,
    /// Pen pressure scales the opacity.
    pub pressure_opacity: bool,
    /// Pen pressure scales the flow.
    pub pressure_flow: bool,
    /// Pen pressure scales the spacing: lighter pressure, closer dabs.
    pub pressure_spacing: bool,
    /// How pressure maps to each of size, opacity, flow and spacing
    /// (`None`: straight through).
    pub pressure_curves: PressureCurves,
}

/// A pressure response per setting it drives.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PressureCurves {
    pub size: Option<SoftnessCurve>,
    pub opacity: Option<SoftnessCurve>,
    pub flow: Option<SoftnessCurve>,
    pub spacing: Option<SoftnessCurve>,
}

impl PressureCurves {
    #[inline]
    fn map(curve: &Option<SoftnessCurve>, p: f32) -> f32 {
        curve.as_ref().map_or(p, |c| c.eval(p).clamp(0.0, 1.0))
    }

    pub fn size(&self, p: f32) -> f32 {
        Self::map(&self.size, p)
    }

    pub fn opacity(&self, p: f32) -> f32 {
        Self::map(&self.opacity, p)
    }

    pub fn flow(&self, p: f32) -> f32 {
        Self::map(&self.flow, p)
    }

    pub fn spacing(&self, p: f32) -> f32 {
        Self::map(&self.spacing, p)
    }
}

impl BrushOptions {
    /// Dab spacing in canvas pixels for a tip `diameter` across, times
    /// `factor` (pressure, inputs).
    pub fn spacing_px(&self, diameter: f32, factor: f32) -> f32 {
        match self.auto_spacing {
            Some(coeff) => {
                let d = diameter.max(0.01);
                coeff * if d < 1.0 { d } else { d.sqrt() } * factor
            }
            None => self.spacing / 100.0 * diameter * factor,
        }
    }

    /// The spacing's share at pressure `p` (1 without pressure spacing);
    /// never so small the dabs pile up.
    pub fn spacing_factor(&self, p: f32) -> f32 {
        if self.pressure_spacing {
            self.pressure_curves.spacing(p).max(0.05)
        } else {
            1.0
        }
    }
}

impl BrushOptions {
    /// How many tips the dabs pick from (extra tips go with an image tip).
    pub fn tip_count(&self) -> usize {
        match self.pixel_shape {
            PixelBrushShape::Custom(_) => 1 + self.extra_tips.len(),
            _ => 1,
        }
    }

    /// Whether the dabs paint their tips' own colours: asked for, and some
    /// tip has colours.
    pub fn paints_tip_colors(&self) -> bool {
        self.tip_colors
            && match &self.pixel_shape {
                PixelBrushShape::Custom(tip) => {
                    tip.has_colors() || self.extra_tips.iter().any(|t| t.has_colors())
                }
                _ => false,
            }
    }

    /// Tip `i` of [`Self::tip_count`] (0 is `pixel_shape`).
    pub fn tip_shapes(&self) -> std::borrow::Cow<'_, [PixelBrushShape]> {
        if self.tip_count() == 1 {
            return std::borrow::Cow::Borrowed(std::slice::from_ref(&self.pixel_shape));
        }
        std::iter::once(self.pixel_shape.clone())
            .chain(self.extra_tips.iter().cloned().map(PixelBrushShape::Custom))
            .collect()
    }

    /// Create a standard soft brush with the given radius, hardness, base color and spacing.
    pub fn new(diameter: f32, hardness: f32, color: Color32, spacing: f32) -> Self {
        Self {
            diameter,
            hardness,
            softness_selector: SoftnessSelector::Gaussian,
            softness_curve: SoftnessCurve::default(),
            softening: Default::default(),
            pixel_shape: PixelBrushShape::Circle,
            extra_tips: Vec::new(),
            tip_order: TipOrder::Sequence,
            tip_colors: false,
            tip_mapping: TipMapping::Colors,
            placement: Placement::Dabs,
            color,
            spacing,
            auto_spacing: None,
            auto_tip: AutoTip::default(),
            color_source: ColorSource::Plain,
            flow: 100.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            painting_mode: PaintingMode::BuildUp,
            pressure_size: true,
            pressure_min_size: 0.0,
            pressure_opacity: false,
            pressure_flow: false,
            pressure_spacing: false,
            pressure_curves: PressureCurves::default(),
        }
    }
}

#[cfg(test)]
mod auto_tip_tests {
    use super::{AutoTip, BrushOptions};

    #[test]
    fn auto_spacing_goes_by_the_square_root_of_the_size() {
        let mut o = BrushOptions::new(36.0, 100.0, eframe::egui::Color32::BLACK, 10.0);
        assert!((o.spacing_px(36.0, 1.0) - 3.6).abs() < 1e-4);
        o.auto_spacing = Some(0.5);
        assert!((o.spacing_px(36.0, 1.0) - 3.0).abs() < 1e-4);
        assert!((o.spacing_px(36.0, 2.0) - 6.0).abs() < 1e-4);
    }

    #[test]
    fn a_fade_is_solid_inside_and_gone_at_the_edge() {
        let t = AutoTip {
            fade: [0.5, 1.0],
            ..Default::default()
        };
        assert_eq!(t.fade_at(0.4, 0.0, 1.0), 1.0);
        assert_eq!(t.fade_at(0.0, 0.9, 1.0), 1.0);
        let mid = t.fade_at(0.75, 0.0, 1.0);
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert!(t.fade_at(0.999, 0.0, 1.0) < 0.01);
        // Softer: the fade starts nearer the centre.
        assert!(t.fade_at(0.4, 0.0, 0.5) < 1.0);
    }

    #[test]
    fn spikes_fold_into_the_first_wedge_as_the_exact_turn_does() {
        let spikes = AutoTip {
            spikes: 5,
            ..Default::default()
        }
        .spikes()
        .unwrap();
        let wedge = std::f32::consts::TAU / 5.0;
        for i in 0..400 {
            let a = i as f32 / 400.0 * std::f32::consts::TAU;
            let (x, y) = (7.0 * a.cos(), 7.0 * a.sin());
            // Exactly: the angle (y folded up) turned back into ±half a wedge.
            let t = y.abs().atan2(x);
            let t = (t + wedge * 0.5).rem_euclid(wedge) - wedge * 0.5;
            let (fx, fy) = spikes.fold((x, y), 1.0);
            // Either side of a wedge's edge is the same point folded.
            if (t.abs() - wedge * 0.5).abs() < 1e-3 {
                continue;
            }
            assert!((fx - 7.0 * t.cos()).abs() < 1e-3, "{a}: {fx} {fy}");
            assert!((fy - 7.0 * t.sin()).abs() < 1e-3, "{a}: {fx} {fy}");
        }
        assert!((super::fast_atan2(1.0, -1.0) - 3.0 * std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    }
}

#[cfg(test)]
mod tip_mapping_tests {
    use super::TipMapping;

    #[test]
    fn lightness_mapping_keeps_the_colour_at_mid_grey_and_reaches_black_and_white() {
        let red = [0.8, 0.2, 0.2];
        let at = |g: f32| TipMapping::Lightness.map([g; 3], red, [1.0; 3]);
        let mid = at(0.5);
        for (m, r) in mid.iter().zip(red) {
            assert!((m - r).abs() < 1e-4, "{mid:?}");
        }
        assert!(at(0.0).iter().all(|&v| v < 1e-4), "{:?}", at(0.0));
        assert!(at(1.0).iter().all(|&v| v > 1.0 - 1e-4), "{:?}", at(1.0));
        // Gradient: the brush colour where the picture is dark, the
        // secondary where it's light.
        let g = TipMapping::Gradient.map([0.0; 3], red, [0.0, 0.0, 1.0]);
        assert_eq!(g, red);
        let g = TipMapping::Gradient.map([1.0; 3], red, [0.0, 0.0, 1.0]);
        assert_eq!(g, [0.0, 0.0, 1.0]);
    }
}
