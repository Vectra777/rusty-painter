//! Helper structs for painter operations.
use crate::brush_engine::brush_options::PixelBrushShape;
use eframe::egui::TextureHandle;

/// Brush tip data extracted from image
pub struct BrushData {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl BrushData {
    /// Convert brush data into a loadable brush tip tuple
    pub fn into_brush_tip(
        self,
        texture: TextureHandle,
    ) -> (String, PixelBrushShape, Option<TextureHandle>) {
        let name = format!("{}x{}", self.width, self.height);
        let shape = PixelBrushShape::Custom {
            width: self.width,
            height: self.height,
            data: self.data,
        };
        (name, shape, Some(texture))
    }
}
