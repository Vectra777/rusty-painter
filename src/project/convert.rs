use crate::{
    app::state::ColorModel,
    selection::{
        SelectionShape,
        transform::{TransformInfo, TransformState},
    },
};
use eframe::egui::{Color32, Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct StoredColor([u8; 4]);

impl StoredColor {
    pub(super) fn from_color(color: Color32) -> Self {
        Self(color.to_array())
    }

    pub(super) fn to_color(self) -> Color32 {
        Color32::from_rgba_premultiplied(self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

#[derive(Serialize, Deserialize)]
pub(super) enum StoredSelectionShape {
    Rectangle { start: StoredVec2, end: StoredVec2 },
    Circle { center: StoredVec2, radius: f32 },
    Lasso { points: Vec<StoredVec2> },
}

impl From<&SelectionShape> for StoredSelectionShape {
    fn from(shape: &SelectionShape) -> Self {
        match shape {
            SelectionShape::Rectangle { start, end } => Self::Rectangle {
                start: StoredVec2::from(*start),
                end: StoredVec2::from(*end),
            },
            SelectionShape::Circle { center, radius } => Self::Circle {
                center: StoredVec2::from(*center),
                radius: *radius,
            },
            SelectionShape::Lasso { points } => Self::Lasso {
                points: points.iter().copied().map(StoredVec2::from).collect(),
            },
        }
    }
}

impl StoredSelectionShape {
    pub(super) fn into_shape(self) -> SelectionShape {
        match self {
            Self::Rectangle { start, end } => SelectionShape::Rectangle {
                start: start.into(),
                end: end.into(),
            },
            Self::Circle { center, radius } => SelectionShape::Circle {
                center: center.into(),
                radius,
            },
            Self::Lasso { points } => SelectionShape::Lasso {
                points: points.into_iter().map(Into::into).collect(),
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredTransformInfo {
    start_pos: Option<StoredVec2>,
    offset: StoredVec2,
    rotation: f32,
    scale: StoredVec2,
    bounds: Option<StoredRect>,
    state: StoredTransformState,
}

impl From<&TransformInfo> for StoredTransformInfo {
    fn from(info: &TransformInfo) -> Self {
        Self {
            start_pos: info.start_pos.map(StoredVec2::from),
            offset: StoredVec2::from(info.offset),
            rotation: info.rotation,
            scale: StoredVec2::from(info.scale),
            bounds: info.bounds.map(StoredRect::from),
            state: StoredTransformState::from(info.state),
        }
    }
}

impl StoredTransformInfo {
    pub(super) fn into_info(self) -> TransformInfo {
        TransformInfo {
            start_pos: self.start_pos.map(Into::into),
            offset: self.offset.into(),
            rotation: self.rotation,
            scale: self.scale.into(),
            bounds: self.bounds.map(Into::into),
            state: self.state.into(),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum StoredTransformState {
    None,
    Moving,
    Rotating,
    Scaling(usize),
}

impl From<TransformState> for StoredTransformState {
    fn from(state: TransformState) -> Self {
        match state {
            TransformState::None => Self::None,
            TransformState::Moving => Self::Moving,
            TransformState::Rotating => Self::Rotating,
            TransformState::Scaling(idx) => Self::Scaling(idx),
        }
    }
}

impl From<StoredTransformState> for TransformState {
    fn from(state: StoredTransformState) -> Self {
        match state {
            StoredTransformState::None => Self::None,
            StoredTransformState::Moving => Self::Moving,
            StoredTransformState::Rotating => Self::Rotating,
            StoredTransformState::Scaling(idx) => Self::Scaling(idx),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct StoredVec2 {
    x: f32,
    y: f32,
}

impl From<Vec2> for StoredVec2 {
    fn from(value: Vec2) -> Self {
        Self {
            x: value.x,
            y: value.y,
        }
    }
}

impl From<StoredVec2> for Vec2 {
    fn from(value: StoredVec2) -> Self {
        Self::new(value.x, value.y)
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredRect {
    min: StoredVec2,
    max: StoredVec2,
}

impl From<Rect> for StoredRect {
    fn from(value: Rect) -> Self {
        Self {
            min: StoredVec2 {
                x: value.min.x,
                y: value.min.y,
            },
            max: StoredVec2 {
                x: value.max.x,
                y: value.max.y,
            },
        }
    }
}

impl From<StoredRect> for Rect {
    fn from(value: StoredRect) -> Self {
        Rect::from_min_max(
            Pos2::new(value.min.x, value.min.y),
            Pos2::new(value.max.x, value.max.y),
        )
    }
}

#[derive(Serialize, Deserialize)]
pub(super) enum StoredColorModel {
    Rgba,
    Grayscale,
}

impl From<ColorModel> for StoredColorModel {
    fn from(value: ColorModel) -> Self {
        match value {
            ColorModel::Rgba => Self::Rgba,
            ColorModel::Grayscale => Self::Grayscale,
        }
    }
}

impl From<StoredColorModel> for ColorModel {
    fn from(value: StoredColorModel) -> Self {
        match value {
            StoredColorModel::Rgba => Self::Rgba,
            StoredColorModel::Grayscale => Self::Grayscale,
        }
    }
}
