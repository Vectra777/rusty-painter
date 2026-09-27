use crate::selection::SelectionType;
use crate::selection::transform::TransformInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
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
    Shape(crate::app::shape_tool::ShapeKind),
}
