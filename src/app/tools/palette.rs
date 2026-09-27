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
}

impl Default for PaletteToolState {
    fn default() -> Self {
        Self {
            open: false,
            from_layer: false,
            count: 8,
            dither: false,
            extracted: Vec::new(),
        }
    }
}

/// Roughly how many pixels to cluster; more adds time, not quality.
const SAMPLE_PIXELS: usize = 250_000;

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
    }

    /// Add colours to the swatches, skipping ones already there.
    pub(crate) fn add_swatches(&mut self, colors: &[Color32]) {
        let swatches = &mut self.brush_state.swatches;
        for &c in colors {
            if !swatches.contains(&c) {
                swatches.push(c);
            }
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
            if let Some(history) = self.layer_state.histories.get_mut(idx) {
                history.push_action(action);
            }
            self.mark_tiles_in_bounds_dirty(rect);
            self.layer_state.thumbnails_dirty = true;
        }
    }
}
