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
/// Krita's animated brushes and GIMP's image hoses).
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

/// Colour mixing, like Krita's Colour Smudge engine: each dab picks up the
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
}

impl Default for Mixing {
    fn default() -> Self {
        Self {
            smudge_length: 0.8,
            color_rate: 0.5,
            pressure_length: false,
            pressure_color: false,
        }
    }
}

/// Blending strategy for how source color affects the destination.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlendMode {
    Normal,
    Eraser,
}

/// How a stroke's dabs combine, as in Krita.
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
    pub pixel_shape: PixelBrushShape,
    /// More image tips the dabs alternate with (after `pixel_shape`, when
    /// that's an image tip too); empty for one tip.
    pub extra_tips: Vec<std::sync::Arc<crate::brush_engine::tip::TipMask>>,
    /// How the dabs pick among the tips.
    pub tip_order: TipOrder,
    /// Paint with a colour tip's own colours rather than the brush colour
    /// (decorations: flowers, stitches, chains).
    pub tip_colors: bool,
    /// Dabs, or the tip laid along the stroke as a ribbon.
    pub placement: Placement,
    pub color: Color32,
    pub spacing: f32, // Percentage of diameter (0..100+)
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
            pixel_shape: PixelBrushShape::Circle,
            extra_tips: Vec::new(),
            tip_order: TipOrder::Sequence,
            tip_colors: false,
            placement: Placement::Dabs,
            color,
            spacing,
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
