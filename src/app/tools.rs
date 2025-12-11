use crate::selection::SelectionType;
use crate::selection::transform::TransformInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tool {
    Brush,
    Select(SelectionType),
    Transform(TransformInfo),
}