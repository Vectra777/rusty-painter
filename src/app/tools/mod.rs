//! The tools, one module each: what a press, drag and release do, their
//! settings, and any session that lasts between presses (a shape being
//! edited, a gradient being placed). `Tool` is the active tool.
pub(crate) mod assistants;
pub(crate) mod blend;
pub(crate) mod fill;
pub(crate) mod filter;
pub(crate) mod gradient;
pub(crate) mod gradient_colors;
pub(crate) mod guides;
pub(crate) mod liquify;
pub(crate) mod palette;
pub(crate) mod patch;
pub(crate) mod quick_mask;
pub(crate) mod quickshape;
pub(crate) mod select;
pub(crate) mod shape;
pub(crate) mod text;
pub(crate) mod transform;
pub(crate) mod vector;

use crate::selection::SelectionType;
use crate::selection::transform::TransformInfo;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
// The Transform variant carries the warp grid (a few hundred bytes); the
// tool is copied a handful of times a frame, and staying `Copy` keeps every
// `if let Tool::Transform(info) = app.active_tool` simple.
#[allow(clippy::large_enum_variant)]
pub enum Tool {
    Brush,
    Select(SelectionType),
    #[serde(skip)]
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
    /// Type text onto a new layer.
    Text,
    /// Move a vector line's points, widen it there, or move it whole.
    VectorEdit,
}

impl Tool {
    /// What the History panel calls a step this tool makes by changing
    /// pixels (steps that add layers, select or transform say so
    /// themselves).
    pub fn history_label(&self, eraser: bool) -> &'static str {
        match self {
            Tool::Brush if eraser => "Eraser",
            Tool::Brush => "Brush",
            Tool::Select(_) => "Selection",
            Tool::Transform(_) => "Transform",
            Tool::Eyedropper => "Edit",
            Tool::Fill => "Fill",
            Tool::Liquify => "Liquify",
            Tool::Smudge => "Smudge",
            Tool::Blur => "Blur",
            Tool::Shape(_) => "Shape",
            Tool::Gradient => "Gradient",
            Tool::Text => "Text",
            Tool::VectorEdit => "Edit line",
        }
    }
}
