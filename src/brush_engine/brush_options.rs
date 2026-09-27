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

/// Blending strategy for how source color affects the destination.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Eraser,
}

/// How a stroke's dabs combine, as in Krita.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
    /// How pressure maps to each of size, opacity and flow (`None`:
    /// straight through).
    pub pressure_curves: PressureCurves,
}

/// A pressure response per setting it drives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PressureCurves {
    pub size: Option<SoftnessCurve>,
    pub opacity: Option<SoftnessCurve>,
    pub flow: Option<SoftnessCurve>,
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
}

impl BrushOptions {
    /// Create a standard soft brush with the given radius, hardness, base color and spacing.
    pub fn new(diameter: f32, hardness: f32, color: Color32, spacing: f32) -> Self {
        Self {
            diameter,
            hardness,
            softness_selector: SoftnessSelector::Gaussian,
            softness_curve: SoftnessCurve::default(),
            pixel_shape: PixelBrushShape::Circle,
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
            pressure_curves: PressureCurves::default(),
        }
    }
}
