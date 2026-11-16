//! Colorize masks (see [`crate::canvas::colorize`]): making one, keeping
//! its result up to date in the background as its hints and line art
//! change, its settings (Layer → Colorize Mask), and turning it into a
//! plain paint layer.
//!
//! The hints are the layer's own pixels, so the brush, the eraser, undo,
//! saving and painting together treat them as any layer's paint; only the
//! result is made here, after the pen lifts and the changes settle.

use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::PainterApp;
use crate::canvas::blend::Unmultiply;
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::colorize::{
    self, ColorizeLayer, ColorizeSettings, MAX_EXTEND, MAX_GAP, TileKey,
};
use crate::canvas::storage::{CanvasTileSnapshot, Layer, LayerId, LayerKind, LayerSwap};
use crate::ui::bar_slider::BarSlider;
use eframe::egui::Color32;
use rustc_hash::FxHashMap;

/// How long hints and line art stay unchanged (after the pen lifts)
/// before the result is made again.
const SETTLE: Duration = Duration::from_millis(250);

impl PainterApp {
    /// Whether layer `idx` is a colorize mask.
    pub(crate) fn is_colorize_layer(&self, idx: usize) -> bool {
        self.canvas.colorize_of(idx).is_some()
    }

    /// Layer → New Colorize Mask: a colorize mask just above the selected
    /// layer, which is its line art, selected for painting hints. It
    /// multiplies, so the lines show through the colours.
    pub(crate) fn add_colorize_mask(&mut self) {
        let active = self.canvas.active_layer_idx;
        let source = (self.canvas.layers.get(active))
            .filter(|l| l.kind == LayerKind::Paint && l.colorize.is_none())
            .map(|l| l.id.0);
        let Some(source) = source else {
            self.export_state.message =
                Some("New Colorize Mask: select the line-art layer first".into());
            return;
        };
        self.quick_mask_leave();
        crate::app::tools::transform::commit_floating_layer(self);
        let (index, parent) = self.insertion_point(false);
        let name = self.next_layer_name("Colorize");
        self.insert_entry_with(index, name, LayerKind::Paint, parent, true, |l| {
            l.colorize = Some(Box::new(ColorizeLayer::new(ColorizeSettings {
                source: Some(source),
                ..Default::default()
            })));
            l.blend = LayerBlend::Multiply;
        });
    }

    /// Each frame: start updating every colorize mask whose hints, line
    /// art or settings changed and have settled (never mid-stroke).
    pub(crate) fn colorize_tick(&mut self, ctx: &egui::Context) {
        if let Some(wait) = self.colorize_update(Some(SETTLE)) {
            ctx.request_repaint_after(wait);
        }
    }

    /// Start updating the colorize masks that need it, once their inputs
    /// have stayed the same for `settle` (`None`: at once). Returns how
    /// long until one is due, if one is waiting.
    pub(crate) fn colorize_update(&mut self, settle: Option<Duration>) -> Option<Duration> {
        if self.brush_state.is_drawing || self.brush_state.blend_stroke.is_some() {
            return None;
        }
        let mut wait: Option<Duration> = None;
        for idx in 0..self.canvas.layers.len() {
            let (Some(c), Some(key)) =
                (self.canvas.colorize_of(idx), self.canvas.colorize_key(idx))
            else {
                continue;
            };
            match c.due(key, settle) {
                Ok(()) => self.start_colorize(idx),
                Err(Some(left)) => wait = Some(wait.map_or(left, |w| w.min(left))),
                Err(None) => {}
            }
        }
        wait
    }

    /// Make colorize mask `idx`'s result in the background.
    fn start_colorize(&mut self, idx: usize) {
        let Some((key, job)) = self.canvas.colorize_job(idx) else {
            return;
        };
        let Some(c) = self.canvas.colorize_of(idx) else {
            return;
        };
        c.set_busy(true);
        let id = self.canvas.layers[idx].id;
        let pool = Arc::clone(&self.workspace.pool);
        self.spawn_job(None, move || {
            let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.install(|| colorize::compute(&job))
            }));
            Box::new(move |app: &mut PainterApp| app.finish_colorize(id, key, made.ok()))
        });
    }

    /// Show colorize mask `id`'s new result (made from inputs `key`).
    fn finish_colorize(
        &mut self,
        id: LayerId,
        key: u64,
        tiles: Option<FxHashMap<TileKey, Vec<Color32>>>,
    ) {
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        let Some(c) = self.canvas.colorize_of(idx) else {
            return;
        };
        let Some(tiles) = tiles else {
            c.set_busy(false);
            self.export_state.message = Some("Couldn't update the colorize mask".into());
            return;
        };
        let changed = c.set_result(key, tiles);
        for (tx, ty) in changed {
            if let (Ok(tx), Ok(ty)) = (usize::try_from(tx), usize::try_from(ty)) {
                self.mark_tile_dirty(tx, ty);
            }
        }
        self.render_cache.below_cache = None;
        self.layer_state.thumbnails_dirty = true;
    }

    /// Change colorize mask `idx`'s settings (the result follows once the
    /// change settles; showing the hints or not shows at once).
    fn set_colorize_settings(&mut self, idx: usize, settings: ColorizeSettings) {
        let Some(c) = self.canvas.colorize_of(idx) else {
            return;
        };
        if c.settings == settings {
            return;
        }
        let hints_changed = c.settings.show_hints != settings.show_hints;
        if let Some(c) = self.canvas_mut().layers[idx].colorize.as_mut() {
            c.settings = settings;
        }
        if hints_changed {
            self.mark_all_tiles_dirty();
            self.layer_state.thumbnails_dirty = true;
        }
    }

    /// Make colorize mask `idx`'s result again now.
    pub(crate) fn update_colorize_now(&mut self, idx: usize) {
        if let Some(c) = self.canvas.colorize_of(idx) {
            c.invalidate();
            if !c.is_busy() {
                self.start_colorize(idx);
            }
        }
    }

    /// Layer → Colorize Mask → Convert to Paint Layer: the result as a
    /// plain paint layer in its place (the hints go), as one undo step.
    /// Made first if it's out of date.
    pub(crate) fn convert_colorize(&mut self, idx: usize) {
        if !self.is_colorize_layer(idx) {
            return;
        }
        self.settle_layers();
        let id = self.canvas.layers[idx].id;
        let canvas = Arc::clone(&self.canvas);
        self.run_on_worker("Colorizing…", move || {
            let fresh = canvas.layer_index_of(id).and_then(|i| {
                let c = canvas.colorize_of(i)?;
                let (key, job) = canvas.colorize_job(i)?;
                (c.made_from() != Some(key)).then(|| (key, colorize::compute(&job)))
            });
            drop(canvas);
            Box::new(move |app: &mut PainterApp| app.finish_convert(id, fresh))
        });
    }

    fn finish_convert(
        &mut self,
        id: LayerId,
        fresh: Option<(u64, FxHashMap<TileKey, Vec<Color32>>)>,
    ) {
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        let Some(c) = self.canvas.colorize_of(idx) else {
            return;
        };
        if let Some((key, tiles)) = fresh {
            c.set_result(key, tiles);
        }
        let plain = converted(&self.canvas.layers[idx]);
        let swap = LayerSwap {
            layers: vec![(idx, plain)],
            remove: vec![id],
            active_layer_idx: idx,
            applied: Default::default(),
        };
        self.apply_swap(swap, "Convert colorize mask");
    }
}

/// Colorize mask `layer` as a plain paint layer of its result (same id,
/// name and settings; its pixels shared with the result).
fn converted(layer: &Layer) -> Layer {
    let mut snapshot = layer.snapshot();
    let colorize = snapshot.colorize.take();
    snapshot.tiles = colorize.map_or_else(Vec::new, |c| {
        let mut tiles: Vec<CanvasTileSnapshot> = (c.result_tiles().into_iter())
            .filter_map(|((tx, ty), cell)| {
                let g = cell.lock().unwrap_or_else(|e| e.into_inner());
                Some(CanvasTileSnapshot {
                    tx,
                    ty,
                    data: g.shared_data()?.clone(),
                    deep: None,
                })
            })
            .collect();
        tiles.sort_by_key(|t| (t.ty, t.tx));
        tiles
    });
    Layer::from_snapshot(snapshot)
}

/// Layer → Colorize Mask: the selected colorize mask's settings and
/// actions.
pub(crate) fn colorize_menu(app: &mut PainterApp, ui: &mut egui::Ui) {
    let idx = app.canvas.active_layer_idx;
    let Some(c) = app.canvas.colorize_of(idx) else {
        return;
    };
    let mut settings = c.settings;
    let busy = c.is_busy();
    if ui
        .button(if busy { "Updating…" } else { "Update Now" })
        .clicked()
    {
        app.update_colorize_now(idx);
    }
    ui.checkbox(&mut settings.show_hints, "Show hints")
        .on_hover_text("Show the hints over the colours (they're never exported or merged)");
    ui.add(
        BarSlider::new(&mut settings.gap, 0..=MAX_GAP)
            .suffix(" px")
            .text("Close gaps"),
    )
    .on_hover_text("Openings in the lines up to this wide don't let colour through");
    ui.add(
        BarSlider::new(&mut settings.extend, 0..=MAX_EXTEND)
            .suffix(" px")
            .text("Extend under lines"),
    )
    .on_hover_text("How far colour goes in under the lines from their edge");
    // The line art: any other paint layer.
    let current = app.canvas.colorize_source(idx);
    let name = current.map_or("None", |s| app.canvas.layers[s].name.as_str());
    egui::ComboBox::from_label("Line art")
        .selected_text(name)
        .show_ui(ui, |ui| {
            for (i, l) in app.canvas.layers.iter().enumerate().rev() {
                if i == idx || l.kind != LayerKind::Paint || l.colorize.is_some() {
                    continue;
                }
                if ui.selectable_label(current == Some(i), &l.name).clicked() {
                    settings.source = Some(l.id.0);
                }
            }
        });
    ui.horizontal(|ui| {
        ui.color_edit_button_srgb(&mut settings.transparent);
        ui.label("Transparent key");
        if ui
            .small_button("Brush colour")
            .on_hover_text("Hints in the brush colour leave their area empty")
            .clicked()
        {
            let [r, g, b, _] = app.brush_state.brush.brush_options.color.unmultiplied();
            settings.transparent = [r, g, b];
        }
    })
    .response
    .on_hover_text("Hints in this colour leave their area empty (mark the background with it)");
    app.set_colorize_settings(idx, settings);
    ui.separator();
    if ui.button("Convert to Paint Layer").clicked() {
        app.convert_colorize(idx);
        ui.close_menu();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use eframe::egui::Vec2;

    const RED: Color32 = Color32::from_rgb(200, 30, 30);
    const BLUE: Color32 = Color32::from_rgb(30, 30, 200);

    /// An app with a 192×128 canvas whose layer 1 is line art: two boxes,
    /// x 8..96 and 96..184, y 8..120 (3-px walls), and a colorize mask on
    /// it, selected.
    fn app() -> PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(192, 128, Color32::WHITE, 64));
        app.selection_manager.canvas_size = [192, 128];
        let canvas = app.canvas_mut();
        canvas.active_layer_idx = 1;
        let mut px = vec![Color32::TRANSPARENT; 192 * 128];
        for y in 8..120 {
            for x in 8..184 {
                let wall = |v: usize, at: usize| (at..at + 3).contains(&v);
                if wall(y, 8) || wall(y, 117) || wall(x, 8) || wall(x, 96) || wall(x, 181) {
                    px[y * 192 + x] = Color32::BLACK;
                }
            }
        }
        set_pixels(canvas, 1, &px);
        app.add_colorize_mask();
        app
    }

    fn set_pixels(canvas: &Canvas, idx: usize, px: &[Color32]) {
        let (w, h) = (canvas.width(), canvas.height());
        for ty in 0..h.div_ceil(64) {
            for tx in 0..w.div_ceil(64) {
                let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
                for ly in 0..64.min(h - ty * 64) {
                    for lx in 0..64.min(w - tx * 64) {
                        tile[ly * 64 + lx] = px[(ty * 64 + ly) * w + tx * 64 + lx];
                    }
                }
                canvas.set_layer_tile_data(idx, tx as i32, ty as i32, tile);
            }
        }
    }

    fn dab(app: &mut PainterApp, color: Color32, x: f32, y: f32) {
        app.brush_state.brush.brush_options.color = color;
        app.brush_state.brush.brush_options.diameter = 10.0;
        app.start_stroke_with_pressure(Vec2::new(x, y), 1.0);
        app.add_stroke_point(Vec2::new(x + 4.0, y + 2.0), 1.0);
        app.finish_stroke();
        app.stroke_worker.wait_idle();
        app.sync_stroke_worker();
    }

    /// Bring every colorize mask up to date.
    fn update(app: &mut PainterApp) {
        app.colorize_update(None);
        app.run_jobs();
    }

    /// The composite at `(x, y)`, as shown.
    fn shown(app: &PainterApp, x: usize, y: usize) -> Color32 {
        app.canvas.flatten()[(x, y)]
    }

    /// Layer `idx`'s pixel at `(x, y)` as it shows (a colorize mask's
    /// result, hints left out).
    fn result(app: &PainterApp, idx: usize, x: usize, y: usize) -> Color32 {
        let c = app.canvas.colorize_of(idx).unwrap();
        let tiles = c.result_tiles();
        tiles
            .get(&((x / 64) as i32, (y / 64) as i32))
            .map_or(Color32::TRANSPARENT, |cell| {
                cell.lock().unwrap().data().unwrap()[(y % 64) * 64 + x % 64]
            })
    }

    #[test]
    fn hints_fill_their_areas_and_undo_takes_them_back() {
        let mut app = app();
        let mask = app.canvas.active_layer_idx;
        assert_eq!(app.canvas.colorize_source(mask), Some(1));
        dab(&mut app, RED, 40.0, 60.0);
        dab(&mut app, BLUE, 140.0, 60.0);
        update(&mut app);
        assert_eq!(result(&app, mask, 20, 20), RED);
        assert_eq!(result(&app, mask, 170, 110), BLUE);
        // Multiplied over white paper: the colour; the lines stay black.
        assert_eq!(shown(&app, 20, 110), RED);
        assert_eq!(shown(&app, 97, 60), Color32::BLACK);
        // Nothing to do while nothing changes.
        assert_eq!(app.colorize_update(None), None);
        assert!(!app.canvas.colorize_of(mask).unwrap().is_busy());

        // Undo the blue hint: its area takes red (the only hint left).
        app.apply_history_now(false);
        update(&mut app);
        assert_eq!(result(&app, mask, 170, 110), RED);
        app.apply_history_now(true);
        update(&mut app);
        assert_eq!(result(&app, mask, 170, 110), BLUE);

        // Changing the line art updates it too: a thick wall across the
        // right box leaves its middle (deeper in than `extend`) empty.
        assert_eq!(result(&app, mask, 170, 83), BLUE);
        let key = app.canvas.colorize_key(mask);
        for (tx, ty) in [(1, 1), (2, 1)] {
            let mut tile = app.canvas.get_layer_tile_data(1, tx, ty).unwrap();
            for y in 80..87 {
                for x in 96..184 {
                    if x / 64 == tx as usize {
                        tile[(y - 64) * 64 + x % 64] = Color32::BLACK;
                    }
                }
            }
            app.canvas.set_layer_tile_data(1, tx, ty, tile);
        }
        assert_ne!(app.canvas.colorize_key(mask), key);
        update(&mut app);
        assert_eq!(result(&app, mask, 170, 83), Color32::TRANSPARENT);
        assert_eq!(result(&app, mask, 170, 80), BLUE);
    }

    #[test]
    fn hints_never_export() {
        let mut app = app();
        let mask = app.canvas.active_layer_idx;
        dab(&mut app, BLUE, 40.0, 60.0);
        update(&mut app);
        let painted = app.canvas.flatten();
        let exported = app.canvas.flatten_final();
        // The hint's own pixels (blue) show on screen over the result...
        let at = (42, 61);
        assert_eq!(painted[at], BLUE);
        assert_eq!(exported[at], BLUE);
        // ...but a hint in the transparent key shows only on screen.
        dab(&mut app, Color32::from_rgb(255, 0, 255), 140.0, 60.0);
        update(&mut app);
        let painted = app.canvas.flatten();
        let exported = app.canvas.flatten_final();
        assert_eq!(painted[(142, 61)], Color32::from_rgb(255, 0, 255));
        assert_eq!(exported[(142, 61)], Color32::WHITE);
        assert_eq!(result(&app, mask, 170, 110), Color32::TRANSPARENT);
    }

    #[test]
    fn convert_makes_a_paint_layer_of_the_result_and_undoes() {
        let mut app = app();
        let mask = app.canvas.active_layer_idx;
        let id = app.canvas.layers[mask].id;
        dab(&mut app, RED, 40.0, 60.0);
        dab(&mut app, Color32::from_rgb(255, 0, 255), 140.0, 60.0);
        // (Out of date: made on the way.)
        app.convert_colorize(mask);
        app.run_jobs();
        let layer = &app.canvas.layers[mask];
        assert_eq!(layer.id, id);
        assert!(layer.colorize.is_none());
        assert_eq!(layer.blend, LayerBlend::Multiply);
        let px = |app: &PainterApp, x: i32, y: i32| {
            app.canvas
                .get_layer_tile_data(mask, x / 64, y / 64)
                .map_or(Color32::TRANSPARENT, |t| t[(y % 64 * 64 + x % 64) as usize])
        };
        assert_eq!(px(&app, 20, 20), RED);
        assert_eq!(px(&app, 170, 110), Color32::TRANSPARENT);
        // The hints went: where the transparent key was is empty.
        assert_eq!(px(&app, 142, 61), Color32::TRANSPARENT);
        assert_eq!(shown(&app, 20, 110), RED);

        app.apply_history_now(false);
        assert!(app.is_colorize_layer(mask));
        assert_eq!(px(&app, 142, 61), Color32::from_rgb(255, 0, 255));
        app.apply_history_now(true);
        assert!(!app.is_colorize_layer(mask));
        assert_eq!(px(&app, 20, 20), RED);
    }

    #[test]
    fn a_colorize_mask_round_trips_through_the_project_file() {
        let mut app = app();
        let mask = app.canvas.active_layer_idx;
        let settings = ColorizeSettings {
            gap: 12,
            extend: 5,
            show_hints: false,
            transparent: [1, 2, 3],
            ..app.canvas.colorize_of(mask).unwrap().settings
        };
        app.set_colorize_settings(mask, settings);
        dab(&mut app, RED, 40.0, 60.0);
        update(&mut app);
        let before = app.canvas.flatten();
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let mut app = crate::project::tests::test_app_pub(loaded.canvas);
        let c = app.canvas.colorize_of(mask).expect("still a colorize mask");
        assert_eq!(c.settings, settings);
        assert_eq!(app.canvas.colorize_source(mask), Some(1));
        // The result is made again from the hints.
        assert_eq!(c.made_from(), None);
        update(&mut app);
        assert_eq!(app.canvas.flatten().pixels, before.pixels);
    }

    #[test]
    fn deleting_and_undoing_keeps_it_a_colorize_mask() {
        let mut app = app();
        let mask = app.canvas.active_layer_idx;
        let settings = app.canvas.colorize_of(mask).unwrap().settings;
        app.delete_picked();
        assert!(!app.canvas.layers.iter().any(|l| l.colorize.is_some()));
        app.apply_history_now(false);
        let c = app.canvas.colorize_of(mask).expect("back");
        assert_eq!(c.settings, settings);
    }

    #[test]
    fn new_colorize_mask_needs_a_paint_layer() {
        let mut app = app();
        let count = app.canvas.layers.len();
        // On the colorize mask itself: no.
        app.add_colorize_mask();
        assert_eq!(app.canvas.layers.len(), count);
        assert!(app.export_state.message.is_some());
    }
}
