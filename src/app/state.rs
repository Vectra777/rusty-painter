use crate::canvas::Canvas;
use eframe::egui::Color32;

pub const TILE_SIZE: usize = 64;
pub const ATLAS_SIZE: usize = 2048;
pub const MAX_CANVAS_DIMENSION: usize = 65_536;
pub const MAX_CANVAS_PIXELS: usize = 268_435_456;
pub const MAX_CANVAS_DPI: f32 = 4_800.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasUnit {
    Pixels,
    Inches,
    Millimeters,
    Centimeters,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundChoice {
    Transparent,
    White,
    Black,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorModel {
    Rgba,
    Grayscale,
}

#[derive(Clone)]
pub struct NewCanvasSettings {
    pub name: String,
    pub width: f32,
    pub height: f32,
    pub unit: CanvasUnit,
    pub resolution: f32,
    pub orientation: Orientation,
    pub background: BackgroundChoice,
    pub custom_bg: Color32,
    pub color_model: ColorModel,
}

pub struct CanvasTile {
    pub dirty: bool,
    pub tx: usize,
    pub ty: usize,
}

impl CanvasUnit {
    pub fn label(&self) -> &'static str {
        match self {
            CanvasUnit::Pixels => "px",
            CanvasUnit::Inches => "in",
            CanvasUnit::Millimeters => "mm",
            CanvasUnit::Centimeters => "cm",
        }
    }
}

impl NewCanvasSettings {
    pub fn from_canvas(canvas: &Canvas) -> Self {
        let width = canvas.width() as f32;
        let height = canvas.height() as f32;
        let orientation = if width >= height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        };
        Self {
            name: "Untitled".to_string(),
            width,
            height,
            unit: CanvasUnit::Pixels,
            resolution: 300.0,
            orientation,
            background: BackgroundChoice::White,
            custom_bg: Color32::WHITE,
            color_model: ColorModel::Rgba,
        }
    }

    pub fn sync_from_canvas(&mut self, canvas: &Canvas) {
        self.width = canvas.width() as f32;
        self.height = canvas.height() as f32;
        self.orientation = if self.width >= self.height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        };
    }

    pub fn dimensions_in_pixels(&self) -> (usize, usize) {
        self.validated_dimensions().unwrap_or((16_384, 16_384))
    }

    pub fn validated_dimensions(&self) -> Result<(usize, usize), String> {
        let dpi = self.resolution.max(1.0);
        validate_dpi(dpi)?;
        let to_px = |value: f32| -> f32 {
            match self.unit {
                CanvasUnit::Pixels => value,
                CanvasUnit::Inches => value * dpi,
                CanvasUnit::Millimeters => value / 25.4 * dpi,
                CanvasUnit::Centimeters => value / 2.54 * dpi,
            }
        };

        let mut w = to_px(self.width.max(1.0));
        let mut h = to_px(self.height.max(1.0));

        match self.orientation {
            Orientation::Portrait if w > h => std::mem::swap(&mut w, &mut h),
            Orientation::Landscape if h > w => std::mem::swap(&mut w, &mut h),
            _ => {}
        }

        let width = w.round().max(1.0) as usize;
        let height = h.round().max(1.0) as usize;
        validate_canvas_size(width, height)?;
        Ok((width, height))
    }

    pub fn background_color32(&self, model: ColorModel) -> Color32 {
        let base = match self.background {
            BackgroundChoice::Transparent => Color32::TRANSPARENT,
            BackgroundChoice::White => Color32::WHITE,
            BackgroundChoice::Black => Color32::BLACK,
            BackgroundChoice::Custom => self.custom_bg,
        };

        let color = base;
        match model {
            ColorModel::Rgba => color,
            ColorModel::Grayscale => to_grayscale(color),
        }
    }
}

pub fn to_grayscale(color: Color32) -> Color32 {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let y = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32).round() as u8;
    Color32::from_rgba_unmultiplied(y, y, y, a)
}

pub fn validate_canvas_size(width: usize, height: usize) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("Canvas dimensions must be at least 1 px".to_string());
    }
    if width > MAX_CANVAS_DIMENSION || height > MAX_CANVAS_DIMENSION {
        return Err(format!(
            "Canvas edge is too large. Maximum is {MAX_CANVAS_DIMENSION} px."
        ));
    }
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| "Canvas dimensions overflow usize".to_string())?;
    if pixels > MAX_CANVAS_PIXELS {
        return Err(format!(
            "Canvas is too large. Maximum is {} megapixels.",
            MAX_CANVAS_PIXELS / 1_000_000
        ));
    }
    Ok(())
}

pub fn validate_dpi(dpi: f32) -> Result<(), String> {
    if !(1.0..=MAX_CANVAS_DPI).contains(&dpi) {
        return Err(format!("DPI must be between 1 and {MAX_CANVAS_DPI:.0}."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_generous_canvas_limit() {
        assert!(validate_canvas_size(16_384, 16_384).is_ok());
        assert!(validate_canvas_size(16_384, 16_385).is_err());
        assert!(validate_canvas_size(0, 10).is_err());
    }

    #[test]
    fn unit_conversion_is_capped() {
        let settings = NewCanvasSettings {
            width: 2.0,
            height: 3.0,
            unit: CanvasUnit::Inches,
            resolution: MAX_CANVAS_DPI,
            orientation: Orientation::Portrait,
            name: "Huge".to_string(),
            background: BackgroundChoice::White,
            custom_bg: Color32::WHITE,
            color_model: ColorModel::Rgba,
        };
        assert_eq!(settings.validated_dimensions().unwrap(), (9_600, 14_400));
    }

    #[test]
    fn grayscale_background_is_converted() {
        let settings = NewCanvasSettings {
            width: 1.0,
            height: 1.0,
            unit: CanvasUnit::Pixels,
            resolution: 72.0,
            orientation: Orientation::Portrait,
            name: "Gray".to_string(),
            background: BackgroundChoice::Custom,
            custom_bg: Color32::from_rgb(255, 0, 0),
            color_model: ColorModel::Grayscale,
        };

        let [r, g, b, _] = settings
            .background_color32(ColorModel::Grayscale)
            .to_srgba_unmultiplied();
        assert_eq!(r, g);
        assert_eq!(g, b);
    }
}
