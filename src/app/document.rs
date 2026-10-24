//! Document settings and limits: canvas size/unit/background for the New
//! Canvas dialog, the colour model, tile and atlas sizes, and the
//! per-tile redraw flags.

use crate::canvas::Canvas;
use eframe::egui::Color32;

pub const TILE_SIZE: usize = 64;
pub const ATLAS_SIZE: usize = 2048;
pub const MAX_CANVAS_DIMENSION: usize = 65_536;
pub const MAX_CANVAS_PIXELS: usize = 268_435_456;
pub const MAX_CANVAS_DPI: f32 = 4_800.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// What a new canvas's Background layer shows on its colour.
#[derive(Clone, Default)]
pub enum Paper {
    #[default]
    None,
    /// A paper texture, repeated: its valleys shade the colour by
    /// `strength`, the texture `scale` times its size.
    Texture {
        pattern: std::sync::Arc<crate::brush_engine::texture::Pattern>,
        strength: f32,
        scale: f32,
    },
    /// A picture covering the canvas (its edges cut to the canvas's shape).
    Picture {
        name: String,
        image: std::sync::Arc<image::RgbaImage>,
    },
}

impl Paper {
    /// The Background layer's tiles (`ts` pixels a side) for a `w`×`h`
    /// canvas of colour `background` (premultiplied; white when
    /// transparent); none for no paper.
    pub fn tiles(
        &self,
        (w, h): (usize, usize),
        ts: usize,
        background: Color32,
        model: ColorModel,
    ) -> Vec<((i32, i32), Vec<Color32>)> {
        let base = if background.a() == 0 {
            Color32::WHITE
        } else {
            background
        };
        let gray = |c: Color32| match model {
            ColorModel::Rgba => c,
            ColorModel::Grayscale => to_grayscale(c),
        };
        let size = (w as i32, h as i32);
        match self {
            Paper::None => Vec::new(),
            Paper::Texture {
                pattern,
                strength,
                scale,
            } => {
                let inv = 1.0 / scale.max(0.01);
                crate::app::import::pixels_to_tiles(ts as i32, (0, 0), size, |x, y| {
                    let k = 1.0 - strength * (1.0 - pattern.at(x as f32, y as f32, inv));
                    let [r, g, b, a] = base.to_array();
                    let shade = |v: u8| (v as f32 * k).round() as u8;
                    Color32::from_rgba_premultiplied(shade(r), shade(g), shade(b), a)
                })
            }
            Paper::Picture { image, .. } => {
                // Scaled to cover the canvas, its middle kept.
                let (iw, ih) = (image.width().max(1) as f32, image.height().max(1) as f32);
                let k = (w as f32 / iw).max(h as f32 / ih);
                let (sw, sh) = ((iw * k).ceil() as u32, (ih * k).ceil() as u32);
                let img = image::imageops::resize(
                    image.as_ref(),
                    sw.max(w as u32),
                    sh.max(h as u32),
                    image::imageops::FilterType::Triangle,
                );
                let (ox, oy) = ((img.width() - w as u32) / 2, (img.height() - h as u32) / 2);
                crate::app::import::pixels_to_tiles(ts as i32, (0, 0), size, |x, y| {
                    let [r, g, b, a] = img.get_pixel(x as u32 + ox, y as u32 + oy).0;
                    let top = Color32::from_rgba_unmultiplied(r, g, b, a).to_array();
                    // Over the colour, where the picture is see-through.
                    let under = base.to_array();
                    let c: [u8; 4] = std::array::from_fn(|i| {
                        top[i] + ((under[i] as u32 * (255 - a as u32) + 127) / 255) as u8
                    });
                    gray(Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                })
            }
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Paper::None => "None",
            Paper::Texture { pattern, .. } => &pattern.name,
            Paper::Picture { name, .. } => name,
        }
    }
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
    /// Colour space the new document blends in.
    pub blend_space: crate::canvas::blend_modes::BlendSpace,
    /// Bits the new document keeps for each channel.
    pub depth: crate::canvas::storage::Depth,
    pub paper: Paper,
}

/// A changed rectangle inside one tile, in tile-local pixels:
/// `[x0, y0, x1, y1)` (end exclusive).
pub type TileRect = [usize; 4];

pub struct CanvasTile {
    pub dirty: bool,
    pub tx: usize,
    pub ty: usize,
    /// When only part of a dirty tile changed (brush strokes), that part;
    /// `None` means the whole tile. Redraw composites and uploads just this.
    pub damage: Option<TileRect>,
}

impl CanvasTile {
    /// The whole tile needs redrawing.
    pub fn mark_full(&mut self) {
        self.dirty = true;
        self.damage = None;
    }

    /// `rect` changed. Grows an existing partial damage; a tile already
    /// fully dirty stays fully dirty.
    pub fn mark_rect(&mut self, rect: TileRect) {
        if !self.dirty {
            self.dirty = true;
            self.damage = Some(rect);
        } else if let Some(d) = &mut self.damage {
            *d = [
                d[0].min(rect[0]),
                d[1].min(rect[1]),
                d[2].max(rect[2]),
                d[3].max(rect[3]),
            ];
        }
    }

    pub fn clear(&mut self) {
        self.dirty = false;
        self.damage = None;
    }
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
            blend_space: canvas.blend_space,
            depth: canvas.depth(),
            paper: Paper::None,
        }
    }

    /// The canvas's size, in the unit picked.
    pub fn sync_from_canvas(&mut self, canvas: &Canvas) {
        let per_unit = self.px_per_unit();
        self.width = canvas.width() as f32 / per_unit;
        self.height = canvas.height() as f32 / per_unit;
        self.orientation = if self.width >= self.height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        };
    }

    pub fn dimensions_in_pixels(&self) -> (usize, usize) {
        self.validated_dimensions().unwrap_or((16_384, 16_384))
    }

    fn px_per_unit(&self) -> f32 {
        let dpi = self.resolution.max(1.0);
        match self.unit {
            CanvasUnit::Pixels => 1.0,
            CanvasUnit::Inches => dpi,
            CanvasUnit::Millimeters => dpi / 25.4,
            CanvasUnit::Centimeters => dpi / 2.54,
        }
    }

    pub fn validated_dimensions(&self) -> Result<(usize, usize), String> {
        validate_dpi(self.resolution.max(1.0))?;
        let to_px = |value: f32| value * self.px_per_unit();

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
    fn paper_shades_the_background_and_a_picture_covers_the_canvas() {
        use super::Paper;
        let pattern = crate::brush_engine::texture::builtin()[0].clone();
        let paper = Paper::Texture {
            pattern,
            strength: 0.5,
            scale: 1.0,
        };
        let tiles = paper.tiles((100, 70), 64, Color32::WHITE, ColorModel::Rgba);
        assert_eq!(tiles.len(), 4);
        let px: Vec<Color32> = tiles.iter().flat_map(|(_, t)| t.iter().copied()).collect();
        // Shaded by up to half, never see-through where the canvas is.
        assert!(px.iter().any(|c| c.r() < 200));
        assert!((px.iter()).all(|c| c.a() == 0 || (c.a() == 255 && c.r() >= 127)));
        // A 2×1 picture, red left and blue right, on a square canvas: its
        // middle, scaled up, so the canvas's left edge is red.
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 255, 255]));
        let paper = Paper::Picture {
            name: "p".into(),
            image: std::sync::Arc::new(img),
        };
        let tiles = paper.tiles((64, 64), 64, Color32::WHITE, ColorModel::Rgba);
        let tile = &tiles[0].1;
        assert!(tile[0].r() > 200 && tile[0].b() < 60, "{:?}", tile[0]);
        assert!(tile[63].b() > 200, "{:?}", tile[63]);
        assert!(tile.iter().all(|c| c.a() == 255));
        assert!(
            Paper::None
                .tiles((8, 8), 64, Color32::WHITE, ColorModel::Rgba)
                .is_empty()
        );
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
            blend_space: Default::default(),
            depth: Default::default(),
            paper: Paper::None,
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
            blend_space: Default::default(),
            depth: Default::default(),
            paper: Paper::None,
        };

        let [r, g, b, _] = settings
            .background_color32(ColorModel::Grayscale)
            .to_srgba_unmultiplied();
        assert_eq!(r, g);
        assert_eq!(g, b);
    }
}
