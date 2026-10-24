//! Palette window actions: extract a palette from the picture, keep it in
//! a palette, make ramps, open and save other apps' palette files, and
//! recolour a layer to a palette.

use crate::PainterApp;
use crate::app::stroke_ops::exclusive;
use crate::app::tools::gradient_colors::GradientColors;
use crate::app::tools::palettes::Palette;
use crate::canvas::history::UndoAction;
use crate::canvas::palette::{Recolor, extract_palette, grid, ramp, shades, variations};
use crate::canvas::storage::LayerKind;
use crate::project::palette_files::Format;
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
    /// What the ramp maker makes, how many steps, and how far its shades
    /// turn the hue (degrees).
    pub ramp_kind: RampKind,
    pub ramp_steps: usize,
    pub hue_shift: f32,
    /// The grid's corners (top-left, top-right, bottom-left,
    /// bottom-right), and its size (also the variations').
    pub grid_corners: [Color32; 4],
    pub grid_cols: usize,
    pub grid_rows: usize,
    /// How far the variations turn the hue (degrees) and change the
    /// lightness (Oklab, 0..1) either way.
    pub vary_hue: f32,
    pub vary_light: f32,
    /// The gradient colours are taken along.
    pub gradient: GradientColors,
    /// The palette's name being typed, while renaming it.
    pub renaming: Option<String>,
    /// A new group's name being typed.
    pub new_group: Option<String>,
}

/// What the ramp maker makes from the brush colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RampKind {
    /// The primary colour's shades, near black to near white.
    Shades,
    /// The primary colour to the secondary.
    Blend,
    /// A grid mixing four corner colours.
    Grid,
    /// The primary colour's neighbours: hues across, lightness down.
    Variations,
    /// Evenly spaced along a gradient.
    Gradient,
}

impl RampKind {
    pub const ALL: [RampKind; 5] = [
        RampKind::Shades,
        RampKind::Blend,
        RampKind::Grid,
        RampKind::Variations,
        RampKind::Gradient,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RampKind::Shades => "Shades",
            RampKind::Blend => "Primary to secondary",
            RampKind::Grid => "Grid between colours",
            RampKind::Variations => "Variations of the colour",
            RampKind::Gradient => "Along a gradient",
        }
    }
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
            ramp_kind: RampKind::Shades,
            ramp_steps: 7,
            hue_shift: 20.0,
            grid_corners: [
                Color32::from_rgb(230, 70, 60),
                Color32::from_rgb(250, 210, 80),
                Color32::from_rgb(40, 60, 140),
                Color32::from_rgb(240, 240, 235),
            ],
            grid_cols: 6,
            grid_rows: 5,
            vary_hue: 30.0,
            vary_light: 0.25,
            gradient: GradientColors::Preset(0),
            renaming: None,
            new_group: None,
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
    /// The colours of the picture `source` holds, found on another thread.
    pub(crate) fn palette_from_image_in_background(
        &mut self,
        name: String,
        source: crate::app::import::FileSource,
    ) {
        let count = self.workspace.palette.count;
        self.spawn_job(None, move || {
            let result = source
                .read()
                .and_then(|bytes| crate::app::import::decode_image(&bytes))
                .map(|img| extract_palette(&image_samples(&img), count));
            Box::new(move |app: &mut PainterApp| match result {
                Ok(colors) => {
                    app.workspace.palette.extracted = colors;
                    app.workspace.palette.source_image = Some(name);
                }
                Err(err) => app.report(err),
            })
        });
    }

    #[cfg(test)]
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

    /// Add colours to the palette shown, as new rows, leaving out ones
    /// already there.
    pub(crate) fn add_to_palette(&mut self, colors: &[Color32]) {
        let palette = self.brush_state.palettes.edit_active();
        let new: Vec<Color32> = colors
            .iter()
            .copied()
            .filter(|&c| !palette.contains(c))
            .collect();
        palette.add_rows(0, &new);
    }

    /// A new palette of `colors` in rows of a hue each, shown.
    pub(crate) fn new_palette_from(&mut self, name: &str, colors: &[Color32]) {
        let mut palette = Palette::new(name, colors);
        palette.arrange_by_hue();
        self.brush_state.palettes.add(palette, false);
    }

    /// The colours the Palette window's maker offers, and how many to a
    /// row.
    pub(crate) fn palette_ramp(&self) -> (Vec<Color32>, usize) {
        let p = &self.workspace.palette;
        let primary = self.brush_state.brush.brush_options.color;
        let secondary = self.brush_state.secondary_color;
        let n = p.ramp_steps;
        match p.ramp_kind {
            RampKind::Shades => (shades(primary, n, p.hue_shift.to_radians()), n),
            RampKind::Blend => (ramp(primary, secondary, n), n),
            RampKind::Grid => (grid(p.grid_corners, p.grid_cols, p.grid_rows), p.grid_cols),
            RampKind::Variations => (
                variations(
                    primary,
                    p.grid_cols,
                    p.grid_rows,
                    p.vary_hue.to_radians(),
                    p.vary_light,
                ),
                p.grid_cols,
            ),
            RampKind::Gradient => (gradient_steps(&self.gradient_stops(p.gradient), n), n),
        }
    }

    /// The colours of gradient `colors` (the brush colours where it uses
    /// them).
    pub(crate) fn gradient_stops(
        &self,
        colors: GradientColors,
    ) -> crate::canvas::gradient::GradientStops {
        self.workspace.gradient.library.stops(
            colors,
            self.brush_state.brush.brush_options.color,
            self.brush_state.secondary_color,
        )
    }

    /// `colors` (rows of `cols`) as new rows of the palette shown.
    pub(crate) fn add_rows_to_palette(&mut self, colors: &[Color32], cols: usize) {
        let palette = self.brush_state.palettes.edit_active();
        for row in colors.chunks(cols.max(1)) {
            palette.add_rows(0, row);
        }
    }

    /// A new palette called `name` of `colors`, `cols` to a row, shown.
    pub(crate) fn new_palette_of_rows(&mut self, name: &str, colors: &[Color32], cols: usize) {
        let mut palette = Palette::new(name, colors);
        palette.columns = cols.clamp(1, crate::app::tools::palettes::MAX_COLUMNS);
        self.brush_state.palettes.add(palette, false);
    }

    /// Open the palette file `name` (from `source`) as a new palette of
    /// the user's, shown.
    pub(crate) fn import_palette_in_background(
        &mut self,
        name: String,
        source: crate::app::import::FileSource,
    ) {
        self.spawn_job(None, move || {
            let result = source
                .read()
                .and_then(|bytes| crate::project::palette_files::read(&name, &bytes));
            Box::new(move |app: &mut PainterApp| match result {
                Ok(palette) => {
                    let message = format!("{} colours from {name}", palette.len());
                    app.brush_state.palettes.add(palette, false);
                    app.export_state.message = Some(message);
                }
                Err(err) => app.report(format!("{name}: {err}")),
            })
        });
    }

    /// Ask where to save the palette shown as `format`.
    pub(crate) fn export_palette(&mut self, format: Format) {
        let palette = self.brush_state.palettes.active();
        match crate::project::palette_files::write(palette, format) {
            Ok(bytes) => {
                let name = format!("{}.{}", palette.name, format.extension());
                self.pick_save(&name, format.extension(), format.mime(), bytes);
            }
            Err(err) => self.report(err),
        }
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

/// `n` colours evenly spaced along `stops`, ends included, opaque (a
/// palette's colours have no transparency).
pub(crate) fn gradient_steps(
    stops: &crate::canvas::gradient::GradientStops,
    n: usize,
) -> Vec<Color32> {
    let last = n.saturating_sub(1).max(1) as f32;
    (0..n)
        .map(|i| {
            let [r, g, b, _] = stops.eval(
                i as f32 / last,
                crate::canvas::blend_modes::BlendSpace::Linear,
            );
            Color32::from_rgb(r.round() as u8, g.round() as u8, b.round() as u8)
        })
        .collect()
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

    #[test]
    fn a_documents_palettes_are_saved_in_it_and_editing_them_is_a_change() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(
            64,
            64,
            Color32::WHITE,
            crate::app::document::TILE_SIZE,
        ));
        app.mark_saved();
        let which = app.brush_state.palettes.add(
            Palette::new("Skin", &[Color32::from_rgb(200, 150, 120)]),
            true,
        );
        assert!(app.has_unsaved_work());
        app.mark_saved();
        app.brush_state
            .palettes
            .edit(which)
            .unwrap()
            .push(Color32::BLACK);
        assert!(app.has_unsaved_work());

        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        assert_eq!(loaded.palettes, app.brush_state.palettes.document);
        // A new document has none of its own.
        app.replace_document(Canvas::new(32, 32, Color32::WHITE, 64), Default::default());
        assert!(app.brush_state.palettes.document.is_empty());
        assert!(!app.has_unsaved_work());
    }
}
