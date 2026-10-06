//! Conversions between in-memory types (colours, selections, transforms,
//! undo steps) and their stored, serialisable forms.

use crate::{
    app::document::ColorModel,
    canvas::blend_modes::LayerBlend,
    canvas::history::{LayerHistoryOp, LayerMeta, RemovedLayer},
    canvas::storage::{LayerId, LayerKind},
    canvas::text::{TextLayer, TextStyle},
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
    Rectangle {
        start: StoredVec2,
        end: StoredVec2,
    },
    Circle {
        center: StoredVec2,
        radius: f32,
    },
    Lasso {
        points: Vec<StoredVec2>,
    },
    /// Per-pixel selection: its box and zstd-compressed coverage bytes.
    Mask {
        x0: i32,
        y0: i32,
        w: usize,
        h: usize,
        coverage_zstd: Vec<u8>,
    },
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
            SelectionShape::Lasso { points, .. } => Self::Lasso {
                points: points.iter().copied().map(StoredVec2::from).collect(),
            },
            SelectionShape::Mask(mask) => Self::Mask {
                x0: mask.x0,
                y0: mask.y0,
                w: mask.w,
                h: mask.h,
                coverage_zstd: zstd::bulk::compress(&mask.data, 3).unwrap_or_default(),
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
            Self::Lasso { points } => {
                crate::selection::new_lasso_shape(points.into_iter().map(Into::into).collect())
            }
            Self::Mask {
                x0,
                y0,
                w,
                h,
                coverage_zstd,
            } => {
                // A mask that fails to decode becomes an empty selection of
                // the same size rather than failing the whole load.
                let data = zstd::bulk::decompress(&coverage_zstd, w * h)
                    .ok()
                    .filter(|d| d.len() == w * h)
                    .unwrap_or_else(|| vec![0; w * h]);
                SelectionShape::Mask(std::sync::Arc::new(crate::selection::SelectionMask::new(
                    x0, y0, w, h, data,
                )))
            }
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
    #[serde(default)]
    corners: Option<[StoredVec2; 4]>,
    /// Saved by one earlier build: the four corners stretched evenly (now
    /// a two-point warp grid).
    #[serde(default, skip_serializing)]
    bilinear: bool,
    /// Distort mode: points along a side, then the points row by row.
    #[serde(default)]
    warp: Option<(usize, Vec<StoredVec2>)>,
    #[serde(default)]
    warp_size: Option<usize>,
    #[serde(default)]
    warp_mode: bool,
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
            corners: info.corners.map(|c| c.map(StoredVec2::from)),
            bilinear: false,
            warp: info
                .warp
                .map(|w| (w.n, w.used().iter().map(|&p| StoredVec2::from(p)).collect())),
            warp_size: Some(info.warp_size),
            warp_mode: info.distort_kind == crate::canvas::storage::DistortKind::Warp,
        }
    }
}

impl StoredTransformInfo {
    pub(super) fn into_info(self) -> TransformInfo {
        let warp = self.stored_warp();
        TransformInfo {
            start_pos: self.start_pos.map(Into::into),
            offset: self.offset.into(),
            rotation: self.rotation,
            scale: self.scale.into(),
            bounds: self.bounds.map(Into::into),
            state: self.state.into(),
            corners: if self.bilinear {
                None
            } else {
                self.corners.map(|c| c.map(Into::into))
            },
            warp,
            distort_kind: if self.warp_mode || self.bilinear {
                crate::canvas::storage::DistortKind::Warp
            } else {
                crate::canvas::storage::DistortKind::Perspective
            },
            warp_size: self.warp_size.unwrap_or(4).clamp(
                crate::canvas::storage::warp::MIN_POINTS,
                crate::canvas::storage::warp::MAX_POINTS,
            ),
        }
    }

    fn stored_warp(&self) -> Option<crate::canvas::storage::warp::WarpGrid> {
        use crate::canvas::storage::warp::{MAX_POINTS, MIN_POINTS, WarpGrid};
        let mut grid = WarpGrid {
            n: 2,
            points: [eframe::egui::Vec2::ZERO; MAX_POINTS * MAX_POINTS],
        };
        if self.bilinear {
            // Corners clockwise → a 2×2 grid row by row.
            let c = self.corners.as_ref()?;
            for (slot, k) in [0, 1, 3, 2].into_iter().enumerate() {
                grid.points[slot] = c[k].into();
            }
            return Some(grid);
        }
        let (n, points) = self.warp.as_ref()?;
        if !(MIN_POINTS..=MAX_POINTS).contains(n) || points.len() != n * n {
            return None;
        }
        grid.n = *n;
        for (slot, p) in grid.points.iter_mut().zip(points) {
            *slot = (*p).into();
        }
        Some(grid)
    }
}

#[derive(Serialize, Deserialize)]
enum StoredTransformState {
    None,
    Moving,
    Rotating,
    Scaling(usize),
    Corner(usize),
}

impl From<TransformState> for StoredTransformState {
    fn from(state: TransformState) -> Self {
        match state {
            TransformState::None => Self::None,
            TransformState::Moving => Self::Moving,
            TransformState::Rotating => Self::Rotating,
            TransformState::Scaling(idx) => Self::Scaling(idx),
            TransformState::Corner(idx) => Self::Corner(idx),
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
            StoredTransformState::Corner(idx) => Self::Corner(idx),
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

/// Mirrors `LayerKind`. Absent in files saved before folders/masks.
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
pub(super) enum StoredLayerKind {
    #[default]
    Paint,
    Group,
    Mask {
        owner: u64,
    },
}

impl From<LayerKind> for StoredLayerKind {
    fn from(kind: LayerKind) -> Self {
        match kind {
            LayerKind::Paint => Self::Paint,
            LayerKind::Group => Self::Group,
            LayerKind::Mask { owner } => Self::Mask { owner: owner.0 },
        }
    }
}

impl From<StoredLayerKind> for LayerKind {
    fn from(kind: StoredLayerKind) -> Self {
        match kind {
            StoredLayerKind::Paint => Self::Paint,
            StoredLayerKind::Group => Self::Group,
            StoredLayerKind::Mask { owner } => Self::Mask {
                owner: LayerId(owner),
            },
        }
    }
}

/// Mirrors `TextLayer`: a text layer's source. Absent in older files.
#[derive(Serialize, Deserialize)]
pub(super) struct StoredText {
    text: String,
    font: String,
    #[serde(default)]
    style: TextStyle,
    color: StoredColor,
    pos: StoredVec2,
}

impl From<&TextLayer> for StoredText {
    fn from(text: &TextLayer) -> Self {
        Self {
            text: text.text.clone(),
            font: text.font.clone(),
            style: text.style,
            color: StoredColor::from_color(text.color),
            pos: text.pos.into(),
        }
    }
}

impl StoredText {
    pub(super) fn from_layer(text: Option<&TextLayer>) -> Option<Self> {
        text.map(Self::from)
    }

    pub(super) fn into_layer(stored: Option<Self>) -> Option<Box<TextLayer>> {
        stored.map(|s| {
            Box::new(TextLayer {
                text: s.text,
                font: s.font,
                style: s.style,
                color: s.color.to_color(),
                pos: s.pos.into(),
            })
        })
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredLayerMeta {
    name: String,
    visible: bool,
    opacity: f32,
    locked: bool,
    #[serde(default)]
    alpha_locked: bool,
    #[serde(default)]
    kind: StoredLayerKind,
    #[serde(default)]
    parent: Option<u64>,
    /// Blend mode key (`LayerBlend::key`); absent in older files = Normal.
    #[serde(default)]
    blend: Option<String>,
    #[serde(default)]
    clipped: bool,
    #[serde(default)]
    adjustment: Option<crate::canvas::filters::Filter>,
    /// Fill layer and border; absent in older files.
    #[serde(
        default,
        skip_serializing_if = "crate::canvas::layer_style::LayerStyle::is_plain"
    )]
    style: crate::canvas::layer_style::LayerStyle,
    #[serde(default)]
    text: Option<StoredText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vector: Option<crate::canvas::vector::VectorLayer>,
    /// Impasto heights, tile by tile; absent in older files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    heights: Option<Vec<(i32, i32, Vec<u16>)>>,
    shader: Option<crate::canvas::shader::ShaderLayer>,
    position_locked: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    reference: bool,
}

impl From<&LayerMeta> for StoredLayerMeta {
    fn from(meta: &LayerMeta) -> Self {
        Self {
            name: meta.name.clone(),
            visible: meta.visible,
            opacity: meta.opacity,
            locked: meta.locked,
            alpha_locked: meta.alpha_locked,
            kind: meta.kind.into(),
            parent: meta.parent.map(|p| p.0),
            blend: Some(meta.blend.key().to_string()),
            clipped: meta.clipped,
            adjustment: meta.adjustment,
            style: meta.style,
            text: StoredText::from_layer(meta.text.as_deref()),
            vector: meta.vector.as_deref().cloned(),
            heights: meta.height.as_ref().map(|m| {
                (m.tiles().into_iter())
                    .map(|((tx, ty), h)| (tx, ty, h))
                    .collect()
            }),
            shader: meta.shader.as_deref().cloned(),
            position_locked: meta.position_locked,
            draft: meta.draft,
            reference: meta.reference,
        }
    }
}

impl StoredLayerMeta {
    pub(super) fn into_meta(self) -> LayerMeta {
        LayerMeta {
            name: self.name,
            visible: self.visible,
            opacity: self.opacity,
            locked: self.locked,
            alpha_locked: self.alpha_locked,
            kind: self.kind.into(),
            parent: self.parent.map(LayerId),
            blend: self
                .blend
                .as_deref()
                .and_then(LayerBlend::from_key)
                .unwrap_or_default(),
            clipped: self.clipped,
            adjustment: self.adjustment,
            style: self.style,
            text: StoredText::into_layer(self.text),
            vector: self.vector.map(Box::new),
            height: self.heights.map(|tiles| {
                let map = crate::canvas::impasto::HeightMap::default();
                for (tx, ty, h) in tiles {
                    map.set_tile((tx, ty), Some(h));
                }
                Box::new(map)
            }),
            shader: self.shader.map(Box::new),
            position_locked: self.position_locked,
            draft: self.draft,
            reference: self.reference,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct StoredRemovedLayer {
    index: usize,
    id: u64,
    meta: StoredLayerMeta,
}

/// Mirrors `LayerHistoryOp`. A structural layer change (add/remove/move)
/// bundled with an undo action. Fields added with folders/masks default,
/// so older files still load.
#[derive(Serialize, Deserialize)]
pub(super) enum StoredLayerHistoryOp {
    Added {
        index: usize,
        id: u64,
        #[serde(default)]
        meta: Option<StoredLayerMeta>,
        active_before: usize,
        active_after: usize,
    },
    Removed {
        index: usize,
        id: u64,
        meta: StoredLayerMeta,
        #[serde(default)]
        also: Vec<StoredRemovedLayer>,
        active_before: usize,
        active_after: usize,
    },
    Moved {
        id: u64,
        from: usize,
        to: usize,
        #[serde(default)]
        parent_before: Option<u64>,
        #[serde(default)]
        parent_after: Option<u64>,
        active_before: usize,
        active_after: usize,
    },
    /// Text layers' source on the other side of the step, then the step's
    /// own layer change.
    Text {
        layers: Vec<(u64, Option<StoredText>)>,
        #[serde(default)]
        inner: Option<Box<StoredLayerHistoryOp>>,
    },
    /// Vector layers' lines on the other side of the step, then the step's
    /// own layer change.
    Vector {
        layers: Vec<(u64, Option<crate::canvas::vector::VectorLayer>)>,
        #[serde(default)]
        inner: Option<Box<StoredLayerHistoryOp>>,
    },
    /// Impasto heights on the other side of the step (tiles, and the whole
    /// map when the step added or took it away), then its layer change.
    Height {
        layer: u64,
        tiles: crate::canvas::impasto::HeightTiles,
        #[serde(default)]
        map: Option<Option<crate::canvas::impasto::MapTiles>>,
        #[serde(default)]
        inner: Option<Box<StoredLayerHistoryOp>>,
    },
}

impl From<&LayerHistoryOp> for StoredLayerHistoryOp {
    fn from(op: &LayerHistoryOp) -> Self {
        match op {
            LayerHistoryOp::Added {
                index,
                id,
                meta,
                active_before,
                active_after,
            } => Self::Added {
                index: *index,
                id: id.0,
                meta: meta.as_ref().map(StoredLayerMeta::from),
                active_before: *active_before,
                active_after: *active_after,
            },
            LayerHistoryOp::Removed {
                index,
                id,
                meta,
                also,
                active_before,
                active_after,
            } => Self::Removed {
                index: *index,
                id: id.0,
                meta: StoredLayerMeta::from(meta),
                also: also
                    .iter()
                    .map(|r| StoredRemovedLayer {
                        index: r.index,
                        id: r.id.0,
                        meta: StoredLayerMeta::from(&r.meta),
                    })
                    .collect(),
                active_before: *active_before,
                active_after: *active_after,
            },
            LayerHistoryOp::Moved {
                id,
                from,
                to,
                parent_before,
                parent_after,
                active_before,
                active_after,
            } => Self::Moved {
                id: id.0,
                from: *from,
                to: *to,
                parent_before: parent_before.map(|p| p.0),
                parent_after: parent_after.map(|p| p.0),
                active_before: *active_before,
                active_after: *active_after,
            },
            LayerHistoryOp::Document(_) => {
                unreachable!("a resize step is saved as StoredUndoAction::document")
            }
            LayerHistoryOp::Replaced(_) => {
                unreachable!("a merge step is saved as StoredUndoAction::merge")
            }
            LayerHistoryOp::Text { layers, inner } => Self::Text {
                layers: layers
                    .iter()
                    .map(|(id, text)| (id.0, StoredText::from_layer(text.as_deref())))
                    .collect(),
                inner: inner.as_deref().map(|op| Box::new(Self::from(op))),
            },
            LayerHistoryOp::Vector { layers, inner } => Self::Vector {
                layers: layers
                    .iter()
                    .map(|(id, v)| (id.0, v.as_deref().cloned()))
                    .collect(),
                inner: inner.as_deref().map(|op| Box::new(Self::from(op))),
            },
            // Wet paint isn't saved (it's as it shows, dry): what it carries,
            // or nothing.
            LayerHistoryOp::Wet { inner, .. } => match inner.as_deref() {
                Some(op) => Self::from(op),
                None => Self::Vector {
                    layers: Vec::new(),
                    inner: None,
                },
            },
            LayerHistoryOp::Height {
                layer,
                tiles,
                map,
                inner,
            } => Self::Height {
                layer: layer.0,
                tiles: tiles.clone(),
                map: map.as_ref().map(|m| m.as_ref().map(|m| m.tiles())),
                inner: inner.as_deref().map(|op| Box::new(Self::from(op))),
            },
        }
    }
}

impl StoredLayerHistoryOp {
    pub(super) fn into_op(self) -> LayerHistoryOp {
        match self {
            Self::Added {
                index,
                id,
                meta,
                active_before,
                active_after,
            } => LayerHistoryOp::Added {
                index,
                id: LayerId(id),
                meta: meta.map(StoredLayerMeta::into_meta),
                active_before,
                active_after,
            },
            Self::Removed {
                index,
                id,
                meta,
                also,
                active_before,
                active_after,
            } => LayerHistoryOp::Removed {
                index,
                id: LayerId(id),
                meta: meta.into_meta(),
                also: also
                    .into_iter()
                    .map(|r| RemovedLayer {
                        index: r.index,
                        id: LayerId(r.id),
                        meta: r.meta.into_meta(),
                    })
                    .collect(),
                active_before,
                active_after,
            },
            Self::Moved {
                id,
                from,
                to,
                parent_before,
                parent_after,
                active_before,
                active_after,
            } => LayerHistoryOp::Moved {
                id: LayerId(id),
                from,
                to,
                parent_before: parent_before.map(LayerId),
                parent_after: parent_after.map(LayerId),
                active_before,
                active_after,
            },
            Self::Text { layers, inner } => LayerHistoryOp::Text {
                layers: layers
                    .into_iter()
                    .map(|(id, text)| (LayerId(id), StoredText::into_layer(text)))
                    .collect(),
                inner: inner.map(|op| Box::new(op.into_op())),
            },
            Self::Vector { layers, inner } => LayerHistoryOp::Vector {
                layers: layers
                    .into_iter()
                    .map(|(id, v)| (LayerId(id), v.map(Box::new)))
                    .collect(),
                inner: inner.map(|op| Box::new(op.into_op())),
            },
            Self::Height {
                layer,
                tiles,
                map,
                inner,
            } => LayerHistoryOp::Height {
                layer: LayerId(layer),
                tiles,
                map: map.map(|m| {
                    m.map(|tiles| {
                        let map = crate::canvas::impasto::HeightMap::default();
                        for (key, h) in tiles {
                            map.set_tile(key, Some(h));
                        }
                        Box::new(map)
                    })
                }),
                inner: inner.map(|op| Box::new(op.into_op())),
            },
        }
    }
}
