//! The Fill tool: bucket fill (click) and enclose-and-fill (lasso).

use crate::PainterApp;
use crate::app::stroke_ops::exclusive;
use crate::canvas::fill::{self, FillSettings};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::LayerKind;
use crate::selection::SelectionMask;
use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillMode {
    /// Click an area to fill it.
    Bucket,
    /// Draw around areas to fill everything enclosed.
    Enclose,
}

/// What the fill looks at to find the areas and lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillSource {
    CurrentLayer,
    LayerBelow,
    AllVisible,
}

pub struct FillToolState {
    pub mode: FillMode,
    pub source: FillSource,
    pub settings: FillSettings,
    /// The enclose lasso being drawn.
    pub path: Vec<Vec2>,
}

impl Default for FillToolState {
    fn default() -> Self {
        Self {
            mode: FillMode::Bucket,
            // Line art usually sits on its own layer while colours go below:
            // looking at everything finds the lines wherever they are.
            source: FillSource::AllVisible,
            settings: FillSettings::default(),
            path: Vec::new(),
        }
    }
}

impl PainterApp {
    fn fill_source_layer(&self) -> Option<usize> {
        let active = self.canvas.active_layer_idx;
        match self.workspace.fill.source {
            FillSource::CurrentLayer => Some(active),
            FillSource::LayerBelow => Some(active.saturating_sub(1)),
            FillSource::AllVisible => None,
        }
    }

    /// Whether the active layer can take a fill.
    fn fill_target(&self) -> Option<usize> {
        let idx = self.canvas.active_layer_idx;
        let layer = self.canvas.layers.get(idx)?;
        (!layer.locked && !matches!(layer.kind, LayerKind::Group)).then_some(idx)
    }

    pub(crate) fn fill_press(&mut self, pos: Vec2) {
        match self.workspace.fill.mode {
            FillMode::Bucket => {
                let (x, y) = (pos.x.floor() as i32, pos.y.floor() as i32);
                self.run_fill(|app, source, settings| {
                    let canvas = &app.canvas;
                    let reference = |x, y, w, h| canvas.render_reference(source, x, y, w, h);
                    fill::bucket_fill(
                        &reference,
                        canvas.width(),
                        canvas.height(),
                        (x, y),
                        settings,
                    )
                });
            }
            FillMode::Enclose => {
                self.workspace.fill.path.clear();
                self.workspace.fill.path.push(pos);
            }
        }
    }

    pub(crate) fn fill_drag(&mut self, pos: Vec2) {
        let min_step = 1.5 / self.viewport.zoom.max(0.01);
        let path = &mut self.workspace.fill.path;
        if self.workspace.fill.mode == FillMode::Enclose
            && !path.is_empty()
            && path
                .last()
                .is_none_or(|last| (*last - pos).length() >= min_step)
        {
            path.push(pos);
        }
    }

    pub(crate) fn fill_release(&mut self) {
        if self.workspace.fill.mode != FillMode::Enclose {
            return;
        }
        let path = std::mem::take(&mut self.workspace.fill.path);
        if path.len() < 3 {
            return;
        }
        self.run_fill(|app, source, settings| {
            let canvas = &app.canvas;
            let reference = |x, y, w, h| canvas.render_reference(source, x, y, w, h);
            fill::enclose_fill(&reference, canvas.width(), canvas.height(), &path, settings)
        });
    }

    /// Compute a fill mask with `make`, clip it to the selection and paint
    /// it with the brush colour as one undo step.
    fn run_fill(
        &mut self,
        make: impl FnOnce(&Self, Option<usize>, &FillSettings) -> Option<SelectionMask>,
    ) {
        let Some(target) = self.fill_target() else {
            return;
        };
        self.release_canvas();
        let settings = self.workspace.fill.settings;
        let Some(mut mask) = make(self, self.fill_source_layer(), &settings) else {
            return;
        };
        if self.selection_manager.has_selection() {
            let bounds = [
                mask.x0,
                mask.y0,
                mask.x0 + mask.w as i32,
                mask.y0 + mask.h as i32,
            ];
            let sel = SelectionMask::rasterize(bounds, |y, x0, out| {
                self.selection_manager.row_coverage(y, x0, out)
            });
            for (m, s) in mask.data.iter_mut().zip(&sel.data) {
                *m = ((*m as u32 * *s as u32 + 127) / 255) as u8;
            }
        }
        let color = self.brush_state.brush.brush_options.color;
        let mut action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let changed = exclusive(&mut self.canvas).paint_mask(target, &mask, color, &mut action);
        if let Some(rect) = changed {
            if let Some(history) = self.layer_state.histories.get_mut(target) {
                history.push_action(action);
            }
            self.mark_tiles_in_bounds_dirty(rect);
        }
    }
}
