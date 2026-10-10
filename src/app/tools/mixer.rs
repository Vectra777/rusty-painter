//! The mixing palette (Clip Studio's and Krita's): a small picture of its
//! own to lay paint on with the brush, smear with the smudge (or a mixing
//! brush) and pick colours from. Kept in `mixer.png` next to `brushes/`.

use crate::brush_engine::brush::Brush;
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::brush_engine::stroke_worker::SequentialStroke;
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use eframe::egui::{self, Color32, ColorImage, Vec2};
use rayon::ThreadPool;
use std::path::Path;
use std::sync::Arc;

pub const SIZE: [usize; 2] = [480, 320];
const TILE: usize = 64;

pub struct Mixer {
    canvas: Arc<Canvas>,
    stroke: Option<Stroke>,
    texture: Option<egui::TextureHandle>,
    /// The texture is behind the picture.
    stale: bool,
    /// Changed since it was last saved.
    pub unsaved: bool,
}

// One at a time, in one place: its size doesn't matter.
#[allow(clippy::large_enum_variant)]
enum Stroke {
    /// The brush's paint (its own copy of the brush).
    Paint {
        state: StrokeState,
        brush: Box<Brush>,
        undo: UndoAction,
        tiles: StrokeTiles,
    },
    /// The smudge, or a brush with colour mixing.
    Smudge(Box<crate::app::tools::blend::BlendSession>),
}

/// How a mixer stroke goes on: paint, or smudge with these rates.
pub enum Mode {
    Paint,
    Smudge { length: f32, color_rate: f32 },
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            canvas: Arc::new(blank()),
            stroke: None,
            texture: None,
            stale: true,
            unsaved: false,
        }
    }
}

fn blank() -> Canvas {
    Canvas::new(SIZE[0], SIZE[1], Color32::WHITE, TILE)
}

fn no_undo() -> UndoAction {
    UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    }
}

impl Mixer {
    pub fn clear(&mut self) {
        *self = Self {
            unsaved: true,
            ..Self::default()
        };
    }

    pub fn press(
        &mut self,
        brush: &Brush,
        pool: &Arc<ThreadPool>,
        mode: Mode,
        pos: Vec2,
        pressure: f32,
    ) {
        self.release(pool);
        let mut brush = brush.clone();
        // Mirrors, rulers and the like belong to the canvas.
        brush.stabilizer = 0.0;
        self.stroke = Some(match mode {
            Mode::Paint => {
                let mut stroke = Stroke::Paint {
                    state: StrokeState::default(),
                    brush: Box::new(brush),
                    undo: no_undo(),
                    tiles: StrokeTiles::default(),
                };
                if let Stroke::Paint {
                    state,
                    brush,
                    undo,
                    tiles,
                } = &mut stroke
                {
                    let mut cx = StrokeContext::new(pool, &self.canvas, None, undo, tiles);
                    state.add_sample(brush, pos, pressure, None, &mut cx);
                }
                stroke
            }
            Mode::Smudge { length, color_rate } => {
                let mut session = crate::app::tools::blend::BlendSession::offscreen(
                    Arc::clone(&self.canvas),
                    Arc::clone(pool),
                    brush,
                    length,
                    color_rate,
                    pos,
                    pressure,
                );
                session.start();
                Stroke::Smudge(Box::new(session))
            }
        });
        self.painted();
    }

    pub fn drag(&mut self, pool: &ThreadPool, pos: Vec2, pressure: f32) {
        match &mut self.stroke {
            Some(Stroke::Paint {
                state,
                brush,
                undo,
                tiles,
            }) => {
                let mut cx = StrokeContext::new(pool, &self.canvas, None, undo, tiles);
                state.add_sample(brush, pos, pressure, None, &mut cx);
            }
            Some(Stroke::Smudge(session)) => session.drag(pos, pressure),
            None => return,
        }
        self.painted();
    }

    pub fn release(&mut self, pool: &ThreadPool) {
        match self.stroke.take() {
            Some(Stroke::Paint {
                mut state,
                mut brush,
                mut undo,
                mut tiles,
            }) => {
                let mut cx = StrokeContext::new(pool, &self.canvas, None, &mut undo, &mut tiles);
                state.finish(&mut brush, &mut cx);
            }
            Some(Stroke::Smudge(session)) => {
                session.finish();
            }
            None => return,
        }
        self.painted();
    }

    pub fn is_painting(&self) -> bool {
        self.stroke.is_some()
    }

    fn painted(&mut self) {
        self.stale = true;
        self.unsaved = true;
    }

    /// The colour at `pos` (picture pixels), opaque.
    pub fn color_at(&self, pos: Vec2) -> Color32 {
        let x = (pos.x.max(0.0) as usize).min(SIZE[0] - 1);
        let y = (pos.y.max(0.0) as usize).min(SIZE[1] - 1);
        let mut image = ColorImage::new([1, 1], Color32::TRANSPARENT);
        self.canvas
            .write_region_to_color_image(x, y, 1, 1, &mut image, 1);
        let [r, g, b, _] = image.pixels[0].to_srgba_unmultiplied();
        Color32::from_rgb(r, g, b)
    }

    fn image(&self) -> ColorImage {
        let mut image = ColorImage::new(SIZE, Color32::TRANSPARENT);
        (self.canvas).write_region_to_color_image(0, 0, SIZE[0], SIZE[1], &mut image, 1);
        image
    }

    /// The picture as a texture, brought up to date.
    pub fn texture(&mut self, ctx: &egui::Context) -> &egui::TextureHandle {
        let image = (self.stale || self.texture.is_none()).then(|| self.image());
        self.stale = false;
        match (&mut self.texture, image) {
            (Some(t), Some(image)) => t.set(image, egui::TextureOptions::LINEAR),
            (None, image) => {
                let image = image.unwrap_or_else(|| self.image());
                self.texture = Some(ctx.load_texture("mixer", image, egui::TextureOptions::LINEAR));
            }
            _ => {}
        }
        self.texture.as_ref().expect("made above")
    }

    /// Read from `path` (a picture of another size is left as it is).
    pub fn load(path: &Path) -> Self {
        let mut mixer = Self::default();
        let Ok(img) = image::open(path).map(|i| i.to_rgba8()) else {
            return mixer;
        };
        if [img.width() as usize, img.height() as usize] != SIZE {
            return mixer;
        }
        let pixels: Vec<Color32> = img
            .pixels()
            .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        let canvas = blank();
        canvas.write_layer_region(
            canvas.active_layer_idx,
            (0, 0, SIZE[0], SIZE[1]),
            &pixels,
            None,
        );
        mixer.canvas = Arc::new(canvas);
        mixer
    }

    /// Write to `path` when it changed (errors are logged).
    pub fn save(&mut self, path: &Path) {
        if !self.unsaved || self.is_painting() {
            return;
        }
        self.unsaved = false;
        let image = self.image();
        let bytes: Vec<u8> = (image.pixels.iter())
            .flat_map(|c| c.to_srgba_unmultiplied())
            .collect();
        let path = path.to_path_buf();
        crate::app::jobs::write_later(path, "mixing palette", move || {
            let mut png = Vec::new();
            image::RgbaImage::from_raw(SIZE[0] as u32, SIZE[1] as u32, bytes)
                .ok_or("bad size")?
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            Ok(png)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smudging_blue_into_red_mixes_them() {
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        );
        let mut mixer = Mixer::default();
        let mut brush = Brush::new(40.0, 80.0, Color32::RED, 10.0);
        let row = |mixer: &mut Mixer, brush: &Brush, mode: Mode, y: f32| {
            mixer.press(brush, &pool, mode, Vec2::new(40.0, y), 1.0);
            for x in (40..440).step_by(4) {
                mixer.drag(&pool, Vec2::new(x as f32, y), 1.0);
            }
            mixer.release(&pool);
        };
        brush.brush_options.color = Color32::RED;
        row(&mut mixer, &brush, Mode::Paint, 140.0);
        brush.brush_options.color = Color32::BLUE;
        row(&mut mixer, &brush, Mode::Paint, 180.0);
        assert_eq!(mixer.color_at(Vec2::new(240.0, 140.0)), Color32::RED);
        // Dragged across both, down from the red into the blue.
        mixer.press(
            &brush,
            &pool,
            Mode::Smudge {
                length: 0.9,
                color_rate: 0.0,
            },
            Vec2::new(240.0, 120.0),
            1.0,
        );
        for y in (120..200).step_by(2) {
            mixer.drag(&pool, Vec2::new(240.0, y as f32), 1.0);
        }
        mixer.release(&pool);
        let mixed = mixer.color_at(Vec2::new(240.0, 180.0));
        assert!(mixed.r() > 40 && mixed.b() > 40, "{mixed:?}");
    }
}
