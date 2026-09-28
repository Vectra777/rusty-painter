//! The tools, one module each: what a press, drag and release do, their
//! settings, and any session that lasts between presses (a shape being
//! edited, a gradient being placed). `Tool` is the active tool.
pub(crate) mod assistants;
pub(crate) mod blend;
pub(crate) mod fill;
pub(crate) mod gradient;
pub(crate) mod gradient_colors;
pub(crate) mod guides;
pub(crate) mod liquify;
pub(crate) mod palette;
pub(crate) mod patch;
pub(crate) mod select;
pub(crate) mod shape;
pub(crate) mod transform;

use crate::selection::SelectionType;
use crate::selection::transform::TransformInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
// The Transform variant carries the warp grid (a few hundred bytes); the
// tool is copied a handful of times a frame, and staying `Copy` keeps every
// `if let Tool::Transform(info) = app.active_tool` simple.
#[allow(clippy::large_enum_variant)]
pub enum Tool {
    Brush,
    Select(SelectionType),
    Transform(TransformInfo),
    /// Click the canvas to pick the composited color into the brush.
    Eyedropper,
    /// Bucket fill, or enclose-and-fill with a lasso.
    Fill,
    /// Push, twirl, pinch or bloat pixels.
    Liquify,
    /// Smear the paint along the stroke, with the brush's settings.
    Smudge,
    /// Soften the paint under the brush, with the brush's settings.
    Blur,
    /// Line, rectangle, ellipse or polygon, drawn with the brush.
    Shape(crate::app::tools::shape::ShapeKind),
    /// Linear, radial, reflected or angle gradient.
    Gradient,
}
