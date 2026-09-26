use eframe::egui::Color32;

use crate::brush_engine::hardness::SoftnessCurve;
use crate::brush_engine::hardness::SoftnessSelector;

#[derive(Clone, Debug, PartialEq)]
pub enum PixelBrushShape {
    Circle,
    Square,
    Custom {
        width: usize,
        height: usize,
        data: Vec<u8>, // 0-255 mask
    },
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
        }
    }
}
