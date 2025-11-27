use crate::{canvas::canvas::Canvas, utils::color::Color};
use eframe::egui::{Color32, TextureHandle};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    Bit8,
    Bit16,
    Float32,
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
    pub color_depth: ColorDepth,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaintBackend {
    Cpu,
}

pub struct CanvasTile {
    pub dirty: bool,
    pub atlas_idx: usize,
    pub atlas_x: usize,
    pub atlas_y: usize,
    pub pixel_w: usize,
    pub pixel_h: usize,
    pub tx: usize,
    pub ty: usize,
}

pub struct TextureAtlas {
    pub texture: TextureHandle,
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
            color_depth: ColorDepth::Bit8,
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
        let dpi = self.resolution.max(1.0);
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

        (w.round().max(1.0) as usize, h.round().max(1.0) as usize)
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
            ColorModel::Grayscale => color,
        }
    }
}

pub fn parse_backend_arg() -> PaintBackend {
    let mut backend = PaintBackend::Cpu;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cpu" | "--backend=cpu" => backend = PaintBackend::Cpu,
            "--backend" => {
                if let Some(next) = args.next() {
                    if next.eq_ignore_ascii_case("cpu") {
                        backend = PaintBackend::Cpu;
                    }
                }
            }
            _ => {}
        }
    }
    backend
}
