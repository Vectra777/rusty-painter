//! Palette window actions: extract a palette from the picture, keep it as
//! swatches, and recolour a layer to it.

use crate::PainterApp;
use crate::app::stroke_ops::exclusive;
use crate::canvas::history::UndoAction;
use crate::canvas::palette::{Recolor, extract_palette};
use crate::canvas::storage::LayerKind;
use eframe::egui::Color32;

pub struct PaletteToolState {
    pub open: bool,
    /// Extract from the selected layer (else everything visible).
    pub from_layer: bool,
    pub count: usize,
    pub dither: bool,
    /// The last extracted palette.
    pub extracted: Vec<Color32>,
    /// The picture it came from, when it wasn't the canvas.
    pub source_image: Option<String>,
    /// Where the window was last frame: a file dropped on it gives the
    /// palette instead of a new layer.
    pub window_rect: Option<eframe::egui::Rect>,
}

impl Default for PaletteToolState {
    fn default() -> Self {
        Self {
            open: false,
            from_layer: false,
            count: 8,
            dither: false,
            extracted: Vec::new(),
            source_image: None,
            window_rect: None,
        }
    }
}

/// Roughly how many pixels to cluster; more adds time, not quality.
const SAMPLE_PIXELS: usize = 250_000;

/// About [`SAMPLE_PIXELS`] of `img`'s pixels (area-averaged when it's
/// bigger), premultiplied like the canvas.
fn image_samples(img: &image::RgbaImage) -> Vec<Color32> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let k = ((w * h) as f32 / SAMPLE_PIXELS as f32).sqrt();
    let small;
    let img = if k > 1.0 {
        small = crate::app::import::downscale(
            img,
            ((w as f32 / k) as u32).max(1),
            ((h as f32 / k) as u32).max(1),
        );
        &small
    } else {
        img
    };
    img.pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect()
}

impl PainterApp {
    /// A spread-out sample of the pixels the palette is taken from.
    fn palette_samples(&self) -> Vec<Color32> {
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let step = ((w * h) as f32 / SAMPLE_PIXELS as f32)
            .sqrt()
            .ceil()
            .max(1.0) as usize;
        // Every `step`-th pixel of every `step`-th row, tile by tile in
        // parallel through the fast reference renderer.
        use rayon::prelude::*;
        let source = self
            .workspace
            .palette
            .from_layer
            .then_some(self.canvas.active_layer_idx);
        let ts = self.canvas.tile_size();
        let keys: Vec<(usize, usize)> = (0..h.div_ceil(ts))
            .flat_map(|ty| (0..w.div_ceil(ts)).map(move |tx| (tx, ty)))
            .collect();
        let canvas = &self.canvas;
        keys.par_iter()
            .flat_map_iter(|&(tx, ty)| {
                let (x0, y0) = (tx * ts, ty * ts);
                let (tw, th) = (ts.min(w - x0), ts.min(h - y0));
                let px = canvas.render_reference(source, x0 as i32, y0 as i32, tw, th);
                (0..th)
                    .filter(move |y| (y0 + y) % step == 0)
                    .flat_map(move |y| {
                        (0..tw)
                            .filter(move |x| (x0 + x) % step == 0)
                            .map(move |x| y * tw + x)
                    })
                    .map(move |i| px[i])
            })
            .collect()
    }

    pub(crate) fn extract_palette(&mut self) {
        self.release_canvas();
        let samples = self.palette_samples();
        self.workspace.palette.extracted = extract_palette(&samples, self.workspace.palette.count);
        self.workspace.palette.source_image = None;
    }

    /// Take the palette from a picture file instead of the canvas.
    pub(crate) fn extract_palette_from_image(
        &mut self,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        let img = image::load_from_memory(bytes)
            .map_err(|e| format!("Couldn't open the image: {e}"))?
            .to_rgba8();
        self.workspace.palette.extracted =
            extract_palette(&image_samples(&img), self.workspace.palette.count);
        self.workspace.palette.source_image = Some(name.to_string());
        Ok(())
    }

    /// Whether a drop at the pointer lands on the Palette window.
    pub(crate) fn drop_is_on_palette(&self, ctx: &eframe::egui::Context) -> bool {
        let palette = &self.workspace.palette;
        palette.open
            && palette.window_rect.is_some_and(|rect| {
                ctx.input(|i| i.pointer.latest_pos())
                    .is_some_and(|p| rect.contains(p))
            })
    }

    /// Add colours to the swatches, skipping ones already there.
    pub(crate) fn add_swatches(&mut self, colors: &[Color32]) {
        let swatches = &mut self.brush_state.swatches;
        for &c in colors {
            if !swatches.contains(&c) {
                swatches.push(c);
            }
        }
        self.save_swatches();
    }

    /// Recolour the selected layer (inside the selection) to `palette`.
    pub(crate) fn recolor_layer(&mut self, palette: &[Color32]) {
        let idx = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        if layer.locked || layer.kind != LayerKind::Paint {
            return;
        }
        let Some(recolor) = Recolor::new(palette, self.workspace.palette.dither) else {
            return;
        };
        self.release_canvas();
        let mut action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let selection = self
            .selection_manager
            .has_selection()
            .then_some(&self.selection_manager);
        let changed = exclusive(&mut self.canvas).map_layer_pixels(
            idx,
            selection,
            crate::canvas::palette::RecolorCache::default,
            |cache, c, x, y| recolor.apply_cached(c, x, y, cache),
            &mut action,
        );
        if let Some(rect) = changed {
            self.push_undo(action);
            self.mark_tiles_in_bounds_dirty(rect);
            self.layer_state.thumbnails_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    #[test]
    fn a_picture_gives_its_colours() {
        let img = image::RgbaImage::from_fn(900, 700, |x, _| {
            if x < 450 {
                image::Rgba([230, 20, 20, 255])
            } else {
                image::Rgba([20, 20, 230, 255])
            }
        });
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut app = crate::project::tests::test_app_pub(Canvas::new(
            64,
            64,
            Color32::WHITE,
            crate::app::document::TILE_SIZE,
        ));
        app.workspace.palette.count = 2;
        app.extract_palette_from_image("pic", &png).unwrap();
        let mut got = app.workspace.palette.extracted.clone();
        got.sort_by_key(|c| c.r());
        let near = |c: Color32, r: u8, b: u8| c.r().abs_diff(r) < 8 && c.b().abs_diff(b) < 8;
        assert_eq!(got.len(), 2);
        assert!(near(got[0], 20, 230) && near(got[1], 230, 20), "{got:?}");
        assert_eq!(app.workspace.palette.source_image.as_deref(), Some("pic"));
        assert!(
            app.extract_palette_from_image("bad", b"not an image")
                .is_err()
        );
    }
}
