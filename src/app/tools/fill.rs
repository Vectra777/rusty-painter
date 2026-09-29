//! The Fill tool: bucket fill (click), enclose-and-fill (lasso) and lasso
//! delete (erase what the lasso encloses). Also deleting the selected
//! pixels (the Delete key).

use crate::PainterApp;
use crate::app::stroke_ops::exclusive;
use crate::canvas::fill::{self, FillSettings};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::{LayerKind, SampleLayers};
use crate::selection::SelectionMask;
use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FillMode {
    /// Click an area to fill it.
    Bucket,
    /// Draw around areas to fill everything enclosed.
    Enclose,
    /// Draw around an area to erase it.
    LassoDelete,
}

impl FillMode {
    /// The mode after this one (the G key cycles through them).
    pub fn next(self) -> Self {
        match self {
            FillMode::Bucket => FillMode::Enclose,
            FillMode::Enclose => FillMode::LassoDelete,
            FillMode::LassoDelete => FillMode::Bucket,
        }
    }

    /// Draws a lasso (rather than clicking).
    pub fn is_lasso(self) -> bool {
        self != FillMode::Bucket
    }
}

/// What the fill looks at to find the areas and lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FillSource {
    CurrentLayer,
    LayerBelow,
    AllVisible,
    /// The layers marked as reference (painting on the active one).
    Reference,
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
    fn fill_source_layer(&self) -> SampleLayers {
        let active = self.canvas.active_layer_idx;
        match self.workspace.fill.source {
            FillSource::CurrentLayer => SampleLayers::Layer(active),
            FillSource::LayerBelow => SampleLayers::Layer(active.saturating_sub(1)),
            FillSource::AllVisible => SampleLayers::AllVisible,
            FillSource::Reference => SampleLayers::Reference,
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
                    let reference = |x, y, w, h| canvas.render_sample(source, x, y, w, h);
                    fill::bucket_fill(
                        &reference,
                        canvas.width(),
                        canvas.height(),
                        (x, y),
                        settings,
                    )
                });
            }
            FillMode::Enclose | FillMode::LassoDelete => {
                self.workspace.fill.path.clear();
                self.workspace.fill.path.push(pos);
            }
        }
    }

    pub(crate) fn fill_drag(&mut self, pos: Vec2) {
        let min_step = 1.5 / self.viewport.zoom.max(0.01);
        let path = &mut self.workspace.fill.path;
        if self.workspace.fill.mode.is_lasso()
            && !path.is_empty()
            && path
                .last()
                .is_none_or(|last| (*last - pos).length() >= min_step)
        {
            path.push(pos);
        }
    }

    pub(crate) fn fill_release(&mut self) {
        let mode = self.workspace.fill.mode;
        if !mode.is_lasso() {
            return;
        }
        let path = std::mem::take(&mut self.workspace.fill.path);
        if path.len() < 3 {
            return;
        }
        if mode == FillMode::LassoDelete {
            let lasso = crate::selection::new_lasso_shape(path);
            let Some(bounds) = crate::selection::shape_bounds(&lasso).map(|b| self.pixel_bounds(b))
            else {
                return;
            };
            let mask = SelectionMask::rasterize(bounds, |y, x0, out| {
                crate::selection::shape_row_coverage(&lasso, y, x0, out)
            });
            self.erase_under(mask, true);
            return;
        }
        self.run_fill(|app, source, settings| {
            let canvas = &app.canvas;
            let reference = |x, y, w, h| canvas.render_sample(source, x, y, w, h);
            fill::enclose_fill(&reference, canvas.width(), canvas.height(), &path, settings)
        });
    }

    /// Delete the selected pixels of the active layer (Delete key). The
    /// selection stays.
    pub(crate) fn delete_selection_contents(&mut self) {
        if !self.selection_manager.has_selection() {
            return;
        }
        let Some(bounds) = self
            .selection_manager
            .get_bounds()
            .map(|b| self.pixel_bounds(b))
        else {
            return;
        };
        // The selection's own coverage is the mask (soft edges fade).
        let sel = &self.selection_manager;
        let mask = SelectionMask::rasterize(bounds, |y, x0, out| sel.row_coverage(y, x0, out));
        self.labelled("Delete", |app| app.erase_under(mask, false));
    }

    /// `rect` grown to whole pixels (plus one for soft edges), within the
    /// canvas: `[x0, y0, x1, y1)`.
    pub(crate) fn pixel_bounds(&self, rect: eframe::egui::Rect) -> [i32; 4] {
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        [
            (rect.min.x.floor() as i32).saturating_sub(1).clamp(0, w),
            (rect.min.y.floor() as i32).saturating_sub(1).clamp(0, h),
            (rect.max.x.ceil() as i32).saturating_add(1).clamp(0, w),
            (rect.max.y.ceil() as i32).saturating_add(1).clamp(0, h),
        ]
    }

    /// Erase the active layer under `mask`, limited to the selection when
    /// `clip`, as one undo step.
    pub(crate) fn erase_under(&mut self, mut mask: SelectionMask, clip: bool) {
        let Some(target) = self.fill_target() else {
            return;
        };
        self.release_canvas();
        if clip {
            self.clip_to_selection(&mut mask);
        }
        let mut action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let changed = exclusive(&mut self.canvas).erase_mask(target, &mask, &mut action);
        if let Some(rect) = changed {
            self.push_undo(action);
            self.mark_tiles_in_bounds_dirty(rect);
            self.layer_state.thumbnails_dirty = true;
        }
    }

    /// Limit `mask` to the selection's coverage, if there is a selection.
    fn clip_to_selection(&self, mask: &mut SelectionMask) {
        if !self.selection_manager.has_selection() {
            return;
        }
        let bounds = [
            mask.x0,
            mask.y0,
            mask.x0 + mask.w as i32,
            mask.y0 + mask.h as i32,
        ];
        let selection = &self.selection_manager;
        let sel = SelectionMask::rasterize(bounds, |y, x0, out| selection.row_coverage(y, x0, out));
        for (m, s) in mask.data.iter_mut().zip(&sel.data) {
            *m = ((*m as u32 * *s as u32 + 127) / 255) as u8;
        }
    }

    /// Compute a fill mask with `make`, clip it to the selection and paint
    /// it with the brush colour as one undo step.
    fn run_fill(
        &mut self,
        make: impl FnOnce(&Self, SampleLayers, &FillSettings) -> Option<SelectionMask>,
    ) {
        let Some(target) = self.fill_target() else {
            return;
        };
        let source = self.fill_source_layer();
        if source == SampleLayers::Reference && !self.canvas.has_reference_layer() {
            self.export_state.message =
                Some("No reference layer: mark one in the Layers panel".into());
            return;
        }
        self.release_canvas();
        let settings = self.workspace.fill.settings;
        let Some(mut mask) = make(self, source, &settings) else {
            return;
        };
        self.clip_to_selection(&mut mask);
        let color = self.brush_state.brush.brush_options.color;
        let mut action = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let changed = exclusive(&mut self.canvas).paint_mask(target, &mask, color, &mut action);
        if let Some(rect) = changed {
            self.push_undo(action);
            self.mark_tiles_in_bounds_dirty(rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::selection::{SelectionMode, SelectionShape};
    use eframe::egui::Color32;

    /// A 128×64 canvas whose paint layer is solid red.
    fn app() -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for tx in 0..2 {
            app.canvas_mut()
                .set_layer_tile_data(1, tx, 0, vec![Color32::RED; 64 * 64]);
        }
        app.selection_manager.canvas_size = [128, 64];
        app
    }

    fn alpha(app: &crate::PainterApp, x: i32, y: i32) -> u8 {
        let tile = app
            .canvas
            .get_layer_tile_data(1, x / 64, y / 64)
            .unwrap_or_default();
        tile.get(((y % 64) * 64 + x % 64) as usize)
            .map_or(0, |c| c.a())
    }

    fn select_rect(app: &mut crate::PainterApp, x0: f32, y0: f32, x1: f32, y1: f32) {
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(x0, y0),
                end: Vec2::new(x1, y1),
            },
            SelectionMode::Replace,
        );
    }

    #[test]
    fn delete_erases_the_selected_pixels_as_one_undo_step() {
        let mut app = app();
        select_rect(&mut app, 10.0, 10.0, 40.0, 40.0);
        app.delete_selection_contents();
        assert_eq!(alpha(&app, 20, 20), 0, "inside: erased");
        assert_eq!(alpha(&app, 60, 20), 255, "outside: kept");
        assert!(app.selection_manager.has_selection(), "the selection stays");
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(alpha(&app, 20, 20), 255, "undone");
    }

    #[test]
    fn a_soft_selection_fades_by_its_own_coverage() {
        let mut app = app();
        let half = SelectionMask::new(0, 0, 128, 64, vec![128; 128 * 64]);
        app.selection_manager.apply_shape(
            SelectionShape::Mask(std::sync::Arc::new(half)),
            SelectionMode::Replace,
        );
        app.delete_selection_contents();
        let a = alpha(&app, 20, 20);
        assert!((125..=129).contains(&a), "half erased, not a quarter: {a}");
    }

    #[test]
    fn lasso_delete_erases_what_it_encloses_inside_the_selection() {
        let mut app = app();
        app.workspace.fill.mode = FillMode::LassoDelete;
        select_rect(&mut app, 0.0, 0.0, 30.0, 64.0);
        app.fill_press(Vec2::new(10.0, 10.0));
        for p in [(50.0, 10.0), (50.0, 50.0), (10.0, 50.0)] {
            app.fill_drag(Vec2::new(p.0, p.1));
        }
        app.fill_release();
        assert_eq!(alpha(&app, 20, 30), 0, "in the lasso and the selection");
        assert_eq!(
            alpha(&app, 40, 30),
            255,
            "in the lasso, outside the selection"
        );
        assert_eq!(alpha(&app, 5, 5), 255, "outside the lasso");
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
    }

    #[test]
    fn nothing_is_erased_on_a_transparency_locked_layer() {
        let mut app = app();
        app.canvas_mut().layers[1].alpha_locked = true;
        select_rect(&mut app, 10.0, 10.0, 40.0, 40.0);
        app.delete_selection_contents();
        assert_eq!(alpha(&app, 20, 20), 255);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    #[test]
    fn g_cycles_through_the_fill_modes() {
        let mut mode = FillMode::Bucket;
        let mut seen = Vec::new();
        for _ in 0..3 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [FillMode::Enclose, FillMode::LassoDelete, FillMode::Bucket]
        );
    }
}
