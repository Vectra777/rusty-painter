//! Quick mask: the selection shown as a red tint over what isn't selected,
//! painted with the ordinary tools (the brush adds, the eraser removes;
//! pressure and soft brushes select partly), then turned back into the
//! selection.
//!
//! While it's on, the selection lives in a hidden paint layer on top of
//! the stack (its alpha is the coverage), the active layer. The document's
//! undo history is set aside and quick mask gets one of its own, so undo
//! takes back strokes on the mask; leaving puts the history back and
//! records the whole session as one selection change. The layer is added
//! and removed outside any history, so the document's steps never see it.
//! Anything that changes the layer stack, the canvas geometry or writes a
//! file leaves quick mask first.

use crate::app::PainterApp;
use crate::canvas::Canvas;
use crate::canvas::history::History;
use crate::canvas::storage::{LayerId, LayerKind};
use crate::selection::SelectionShape;
use eframe::egui::{Color32, ColorImage};

/// A quick mask being painted.
pub struct QuickMaskSession {
    /// The hidden layer holding the mask.
    pub layer: LayerId,
    /// The document's history, back when quick mask ends.
    history: History,
    /// The layer that was active, selected again afterwards.
    active_before: Option<LayerId>,
    /// The selection on entering: what undo brings back.
    selection_before: Option<SelectionShape>,
}

/// Tint over the unselected area, and how strong it is where nothing is
/// selected.
const TINT: [u8; 3] = [230, 30, 30];
const TINT_STRENGTH: f32 = 0.5;

impl PainterApp {
    /// Index of the quick mask layer, while quick mask is on.
    pub(crate) fn quick_mask_layer(&self) -> Option<usize> {
        let session = self.workspace.select.quick_mask.as_ref()?;
        self.canvas.layer_index_of(session.layer)
    }

    /// Select → Quick Mask (Shift+Q).
    pub(crate) fn toggle_quick_mask(&mut self) {
        if self.workspace.select.quick_mask.is_some() {
            self.quick_mask_leave();
        } else {
            self.quick_mask_enter();
        }
    }

    /// Finish whatever would write to the layer or the selection.
    fn quick_mask_settle_sessions(&mut self) {
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.gradient_commit();
        self.shape_commit();
        self.filter_cancel();
        self.select_cancel();
        self.workspace.select.magnetic = None;
        // Files the stroke in progress in the history it was made in.
        self.release_canvas();
    }

    /// Turn the selection into a mask to paint on.
    pub(crate) fn quick_mask_enter(&mut self) {
        if self.workspace.select.quick_mask.is_some() {
            return;
        }
        self.quick_mask_settle_sessions();
        let selection_before = self.selection_manager.current_shape.clone();
        let mask = self.selection_manager.current_mask();
        let active_before = self.canvas.layer_id_at(self.canvas.active_layer_idx);
        let end = self.canvas.layers.len();
        let id =
            self.canvas_mut()
                .insert_new_layer(end, "Quick Mask".into(), LayerKind::Paint, None);
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        // Hidden: it's shown as the tint instead of as paint.
        self.canvas_mut().layers[idx].visible = false;
        self.insert_layer_state(idx);
        if let Some(mask) = &mask {
            let ts = self.canvas.tile_size();
            let tiles = self
                .workspace
                .pool
                .install(|| mask_tiles(mask, ts, self.canvas.width(), self.canvas.height()));
            for ((tx, ty), data) in tiles {
                self.canvas.set_layer_tile_data(idx, tx, ty, data);
            }
        }
        self.canvas_mut().active_layer_idx = idx;
        self.selection_manager.clear_selection();
        let history = std::mem::take(&mut self.layer_state.history);
        self.workspace.select.quick_mask = Some(QuickMaskSession {
            layer: id,
            history,
            active_before,
            selection_before,
        });
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Turn the painted mask back into the selection, one undo step.
    pub(crate) fn quick_mask_leave(&mut self) {
        if self.workspace.select.quick_mask.is_none() {
            return;
        }
        self.quick_mask_settle_sessions();
        let Some(session) = self.workspace.select.quick_mask.take() else {
            return;
        };
        let idx = self.canvas.layer_index_of(session.layer);
        let canvas = &self.canvas;
        let mask = idx.and_then(|i| {
            self.workspace
                .pool
                .install(|| crate::app::tools::select::layer_paint_mask(canvas, i))
        });
        if let Some(i) = idx {
            self.canvas_mut().layers.remove(i);
            self.remove_layer_state(i);
        }
        let active = session
            .active_before
            .and_then(|id| self.canvas.layer_index_of(id))
            .unwrap_or(0)
            .min(self.canvas.layers.len().saturating_sub(1));
        self.canvas_mut().active_layer_idx = active;
        self.layer_state.history = session.history;
        self.selection_manager.clear_selection();
        self.selection_manager.current_shape = mask.map(|m| SelectionShape::Mask(m.into()));
        self.record_selection_change(session.selection_before);
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Once a frame: painting goes to the mask. Drops the session if its
    /// layer is gone.
    pub(crate) fn quick_mask_settle(&mut self) {
        let Some(session) = &self.workspace.select.quick_mask else {
            return;
        };
        match self.canvas.layer_index_of(session.layer) {
            Some(idx) if idx != self.canvas.active_layer_idx => {
                if !crate::app::tools::transform::transform_running(self) {
                    self.canvas_mut().active_layer_idx = idx;
                }
            }
            Some(_) => {}
            None => self.quick_mask_leave(),
        }
    }
}

/// `mask` as full tiles of white (premultiplied, alpha = coverage), the
/// empty ones left out.
fn mask_tiles(
    mask: &crate::selection::SelectionMask,
    ts: usize,
    width: usize,
    height: usize,
) -> Vec<((i32, i32), Vec<Color32>)> {
    use rayon::prelude::*;
    let (x0, y0) = (mask.x0.max(0), mask.y0.max(0));
    let x1 = (mask.x0 + mask.w as i32).min(width as i32);
    let y1 = (mask.y0 + mask.h as i32).min(height as i32);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    let t = ts as i32;
    let keys: Vec<(i32, i32)> = (y0 / t..=(y1 - 1) / t)
        .flat_map(|ty| (x0 / t..=(x1 - 1) / t).map(move |tx| (tx, ty)))
        .collect();
    keys.into_par_iter()
        .filter_map(|(tx, ty)| {
            let mut data = vec![Color32::TRANSPARENT; ts * ts];
            let mut any = false;
            for ly in 0..ts {
                for lx in 0..ts {
                    let v = mask.value(tx * t + lx as i32, ty * t + ly as i32);
                    if v > 0 {
                        data[ly * ts + lx] = Color32::from_rgba_premultiplied(v, v, v, v);
                        any = true;
                    }
                }
            }
            any.then_some(((tx, ty), data))
        })
        .collect()
}

/// Tint the composited `img` of tile `(tx, ty)` (its `rect`, tile-local,
/// shrunk by `block` when previewing zoomed out) red where the quick mask
/// layer `mask_idx` doesn't select.
pub(crate) fn tint_tile(
    canvas: &Canvas,
    mask_idx: usize,
    tx: usize,
    ty: usize,
    rect: [usize; 4],
    block: usize,
    img: &mut ColorImage,
) {
    let [w, h] = img.size;
    if w == 0 || h == 0 {
        return;
    }
    let ts = canvas.tile_size();
    let cell = canvas.lock_layer_tile_if_exists(mask_idx, tx, ty);
    let guard = cell
        .as_ref()
        .map(|c| c.lock().unwrap_or_else(|e| e.into_inner()));
    let tile = guard.as_ref().and_then(|g| g.data.as_deref());
    let block = block.max(1);
    let tint = |p: Color32, coverage: f32| {
        let t = TINT_STRENGTH * (1.0 - coverage);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t + 0.5) as u8;
        Color32::from_rgba_premultiplied(
            mix(p.r(), TINT[0]),
            mix(p.g(), TINT[1]),
            mix(p.b(), TINT[2]),
            mix(p.a(), 255),
        )
    };
    for oy in 0..h {
        for ox in 0..w {
            let coverage = match tile {
                None => 0.0,
                Some(tile) => {
                    // The block's average coverage.
                    let (bx, by) = (rect[0] + ox * block, rect[1] + oy * block);
                    let (mut sum, mut n) = (0u32, 0u32);
                    for y in by..(by + block).min(ts) {
                        for x in bx..(bx + block).min(ts) {
                            sum += tile[y * ts + x].a() as u32;
                            n += 1;
                        }
                    }
                    sum as f32 / (n.max(1) * 255) as f32
                }
            };
            let p = &mut img.pixels[oy * w + ox];
            *p = tint(*p, coverage);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::project::tests::test_app_pub;
    use crate::selection::{SelectionMode, SelectionShape};
    use eframe::egui::{Color32, Vec2};

    fn app() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.selection_manager.canvas_size = [128, 128];
        app.brush_state.brush.brush_options.color = Color32::BLACK;
        app.brush_state.brush.brush_options.diameter = 12.0;
        app
    }

    fn stroke(app: &mut crate::PainterApp, from: Vec2, to: Vec2) {
        app.start_stroke_with_pressure(from, 1.0);
        app.add_stroke_point(to, 1.0);
        app.release_canvas();
    }

    fn undo_depth(app: &crate::PainterApp) -> usize {
        app.layer_state.history.stacks().0.len()
    }

    #[test]
    fn painting_in_quick_mask_selects_what_was_painted() {
        let mut app = app();
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(10.0, 10.0),
                end: Vec2::new(40.0, 40.0),
            },
            SelectionMode::Replace,
        );
        app.record_selection_change(None);
        let (layers, depth) = (app.canvas.layers.len(), undo_depth(&app));

        app.toggle_quick_mask();
        let qm = app.quick_mask_layer().expect("quick mask layer");
        assert_eq!(app.canvas.active_layer_idx, qm);
        assert!(!app.canvas.layers[qm].visible, "shown as a tint, not paint");
        assert!(
            !app.selection_manager.has_selection(),
            "strokes aren't clipped"
        );
        assert_eq!(undo_depth(&app), 0, "quick mask has its own history");

        // A stroke, undone, then another one.
        stroke(&mut app, Vec2::new(70.0, 100.0), Vec2::new(110.0, 100.0));
        assert_eq!(undo_depth(&app), 1);
        app.apply_history(false);
        assert_eq!(undo_depth(&app), 0);
        stroke(&mut app, Vec2::new(70.0, 80.0), Vec2::new(110.0, 80.0));

        app.toggle_quick_mask();
        assert!(app.workspace.select.quick_mask.is_none());
        assert_eq!(app.canvas.layers.len(), layers, "the mask layer is gone");
        assert_eq!(app.canvas.active_layer_idx, 1);
        let sel = &app.selection_manager;
        assert!(
            sel.contains(Vec2::new(20.5, 20.5)),
            "the old selection stays"
        );
        assert!(sel.contains(Vec2::new(90.5, 80.5)), "the stroke was added");
        assert!(
            !sel.contains(Vec2::new(90.5, 100.5)),
            "the undone one wasn't"
        );
        assert!(!sel.contains(Vec2::new(60.5, 60.5)));
        // One step for the whole session, and undo brings the rectangle back.
        assert_eq!(undo_depth(&app), depth + 1);
        app.apply_history(false);
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(20.5, 20.5)));
        assert!(!sel.contains(Vec2::new(90.5, 80.5)));
        assert_eq!(undo_depth(&app), depth);
    }

    #[test]
    fn erasing_in_quick_mask_deselects() {
        let mut app = app();
        app.select_all();
        app.toggle_quick_mask();
        app.brush_state.brush.brush_options.blend_mode =
            crate::brush_engine::brush_options::BlendMode::Eraser;
        stroke(&mut app, Vec2::new(10.0, 64.0), Vec2::new(118.0, 64.0));
        app.toggle_quick_mask();
        let sel = &app.selection_manager;
        assert!(!sel.contains(Vec2::new(64.5, 64.5)), "erased");
        assert!(sel.contains(Vec2::new(64.5, 10.5)), "still selected");
    }

    #[test]
    fn layer_changes_and_saving_leave_quick_mask_first() {
        let mut app = app();
        app.toggle_quick_mask();
        stroke(&mut app, Vec2::new(10.0, 10.0), Vec2::new(50.0, 10.0));
        app.add_layer_and_select();
        assert!(app.workspace.select.quick_mask.is_none());
        assert!(app.selection_manager.contains(Vec2::new(30.5, 10.5)));
        // The new layer is undoable, in the document's history.
        let layers = app.canvas.layers.len();
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), layers - 1);

        // Saving takes the mask layer out before writing.
        app.toggle_quick_mask();
        let path = std::env::temp_dir().join(format!("rp-qm-{}.rpainter", std::process::id()));
        app.save_project_to_path(&path).unwrap();
        assert!(app.workspace.select.quick_mask.is_none());
        let loaded = crate::project::load_project(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.canvas.layers.len(), app.canvas.layers.len());
        assert!(
            loaded.canvas.layers.iter().all(|l| l.name != "Quick Mask"),
            "no mask layer saved"
        );
    }

    #[test]
    fn replacing_the_document_ends_quick_mask() {
        let mut app = app();
        app.toggle_quick_mask();
        app.modal_state.new_canvas.width = 64.0;
        app.modal_state.new_canvas.height = 64.0;
        app.modal_state.new_canvas.unit = crate::app::document::CanvasUnit::Pixels;
        app.apply_new_canvas();
        assert!(app.workspace.select.quick_mask.is_none());
        assert!(app.quick_mask_layer().is_none());
        assert_eq!(app.canvas.layers.len(), 2);
    }

    #[test]
    fn the_unselected_area_is_tinted() {
        let mut app = app();
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(32.0, 64.0),
            },
            SelectionMode::Replace,
        );
        app.toggle_quick_mask();
        let qm = app.quick_mask_layer().unwrap();
        let mut img = eframe::egui::ColorImage::new([64, 64], Color32::WHITE);
        super::tint_tile(&app.canvas, qm, 0, 0, [0, 0, 64, 64], 1, &mut img);
        assert_eq!(img.pixels[10], Color32::WHITE, "selected: untouched");
        let tinted = img.pixels[50];
        assert!(
            tinted.r() > 200 && tinted.g() < 160,
            "unselected: red {tinted:?}"
        );
        // Zoomed out, a block half in and half out is half tinted.
        let mut small = eframe::egui::ColorImage::new([1, 1], Color32::WHITE);
        super::tint_tile(&app.canvas, qm, 0, 0, [0, 0, 64, 64], 64, &mut small);
        let g = small.pixels[0].g();
        assert!(g > tinted.g() && g < 255, "{g}");
    }
}
