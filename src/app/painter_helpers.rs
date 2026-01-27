/// Helper structs for painter operations to reduce parameter count and improve clarity

use crate::brush_engine::brush_options::PixelBrushShape;
use eframe::egui::TextureHandle;

/// Represents pixel coordinate bounds
#[derive(Debug, Clone, Copy)]
pub struct PixelBounds {
    pub min_x: i32,
    pub max_x: i32,
    pub min_y: i32,
    pub max_y: i32,
}

/// Represents a range of tiles
#[derive(Debug, Clone, Copy)]
pub struct TileRange {
    pub min_tx: usize,
    pub max_tx: usize,
    pub min_ty: usize,
    pub max_ty: usize,
}

/// Atlas layout information
#[derive(Debug, Clone, Copy)]
pub struct AtlasLayout {
    pub cols: usize,
    pub capacity: usize,
}

/// Position within an atlas
#[derive(Debug, Clone, Copy)]
pub struct AtlasPosition {
    pub atlas_idx: usize,
    pub x: usize,
    pub y: usize,
}

/// Brush tip data extracted from image
pub struct BrushData {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl BrushData {
    /// Convert brush data into a loadable brush tip tuple
    pub fn into_brush_tip(self, texture: TextureHandle) -> (String, PixelBrushShape, Option<TextureHandle>) {
        let name = format!("{}x{}", self.width, self.height);
        let shape = PixelBrushShape::Custom {
            width: self.width,
            height: self.height,
            data: self.data,
        };
        (name, shape, Some(texture))
    }
}
