//! The Liquify tool. A session starts on the first stroke and keeps the
//! original layer plus a displacement field, so strokes build on each
//! other and Restore can undo any area. Apply (Enter, or switching tools)
//! makes it one undo step; Cancel (Esc) puts the layer back.

use crate::PainterApp;
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::canvas::liquify::{LiquifyField, LiquifyMode};
use crate::canvas::storage::{LayerId, LayerKind};
use eframe::egui::{Color32, Vec2};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LiquifySettings {
    pub mode: LiquifyMode,
    /// Brush radius in canvas pixels.
    pub radius: f32,
    /// 0..1.
    pub strength: f32,
}

impl Default for LiquifySettings {
    fn default() -> Self {
        Self {
            mode: LiquifyMode::Push,
            radius: 60.0,
            strength: 0.5,
        }
    }
}

pub struct LiquifySession {
    layer_id: LayerId,
    source: HashMap<(i32, i32), Vec<Color32>>,
    field: LiquifyField,
    /// Tiles written into the layer so far.
    written: HashSet<(i32, i32)>,
    /// Where the brush last dabbed, while it's down: dabs are spaced along
    /// the path from here, however finely the pen reports its moves.
    last: Option<Vec2>,
    /// Where the pen is, while it's down (held-still modes work here).
    pen: Option<Vec2>,
    /// Canvas area changed since the layer was last redrawn: pen samples
    /// only edit the field, and the layer is rendered once a frame.
    pending: Option<[i32; 4]>,
    /// Zoomed out with the pen down: the touched tiles rendered
    /// `preview_block` times smaller for the screen (as Krita previews
    /// liquify at screen resolution), the layer itself left as it was
    /// until the pen comes up.
    preview: HashMap<(i32, i32), Vec<Color32>>,
    preview_block: usize,
    /// What the preview stands in for: drawn into the layer on pen-up.
    previewed: Option<[i32; 4]>,
}

impl LiquifySession {
    /// The zoomed-out preview of tile `(tx, ty)` at `block`, with the
    /// layer's index, if it has one.
    pub(crate) fn preview_pixels(
        &self,
        canvas: &crate::canvas::Canvas,
        tx: usize,
        ty: usize,
        block: usize,
    ) -> Option<(usize, &[Color32])> {
        if self.preview_block != block {
            return None;
        }
        let pixels = self.preview.get(&(tx as i32, ty as i32))?;
        Some((canvas.layer_index_of(self.layer_id)?, pixels))
    }

    /// A preview is on screen in place of the layer.
    pub(crate) fn previewing(&self) -> bool {
        !self.preview.is_empty()
    }
}

impl PainterApp {
    /// Start a session on the active layer if none is running.
    fn liquify_begin(&mut self) -> bool {
        let active = self.canvas.active_layer_idx;
        let Some(layer_id) = self.canvas.layer_id_at(active) else {
            return false;
        };
        if let Some(session) = &self.layer_state.liquify {
            if session.layer_id == layer_id {
                return true;
            }
            self.liquify_commit();
        }
        let Some(layer) = self.canvas.layers.get(active) else {
            return false;
        };
        if layer.locked || matches!(layer.kind, LayerKind::Group) {
            return false;
        }
        self.release_canvas();
        self.layer_state.liquify = Some(LiquifySession {
            layer_id,
            source: self.canvas.capture_layer_pixels(active),
            field: LiquifyField::new(
                self.canvas.tile_size(),
                self.canvas.width(),
                self.canvas.height(),
            ),
            written: HashSet::new(),
            last: None,
            pen: None,
            pending: None,
            preview: HashMap::new(),
            preview_block: 1,
            previewed: None,
        });
        true
    }

    pub(crate) fn liquify_press(&mut self, pos: Vec2) {
        if !self.liquify_begin() {
            return;
        }
        if let Some(s) = self.layer_state.liquify.as_mut() {
            s.last = Some(pos);
            s.pen = Some(pos);
        }
        if self.workspace.liquify.mode.is_continuous() {
            self.liquify_hold(1.0 / 30.0);
        }
    }

    /// Brush moved to `pos`: dab along the way, a spacing apart. A tablet
    /// reports many short moves where a mouse reports a few long ones; both
    /// get the same dabs (and the same strength), the rest carries over to
    /// the next move.
    pub(crate) fn liquify_drag(&mut self, pos: Vec2) {
        let settings = &self.workspace.liquify;
        let (mode, radius, strength) = (settings.mode, settings.radius, settings.strength);
        let Some(session) = self.layer_state.liquify.as_mut() else {
            return;
        };
        let Some(last) = session.last else {
            return;
        };
        session.pen = Some(pos);
        let travel = pos - last;
        let spacing = (radius * 0.12).max(1.0);
        let steps = (travel.length() / spacing).floor() as usize;
        if steps == 0 {
            return;
        }
        let step = travel.normalized() * spacing;
        for i in 1..=steps {
            let center = last + step * i as f32;
            let (amount, delta) = match mode {
                LiquifyMode::Push => (strength, step),
                _ => (strength * 0.15, Vec2::ZERO),
            };
            if let Some(r) = session.field.dab(mode, center, radius, amount, delta) {
                session.pending = Some(union(session.pending, r));
            }
        }
        session.last = Some(last + step * steps as f32);
    }

    /// Brush held still for `dt` seconds: continuous modes keep working.
    pub(crate) fn liquify_hold(&mut self, dt: f32) {
        let settings = &self.workspace.liquify;
        let (mode, radius, strength) = (settings.mode, settings.radius, settings.strength);
        if !mode.is_continuous() {
            return;
        }
        let Some(session) = self.layer_state.liquify.as_mut() else {
            return;
        };
        let Some(center) = session.pen else {
            return;
        };
        // About 1.75 rad/s of twirl (or 60% pinch/bloat per second) at the
        // centre at 50% strength; it was a sluggish third of that.
        let amount = (strength * dt * 10.0).min(1.0);
        if let Some(rect) = session.field.dab(mode, center, radius, amount, Vec2::ZERO) {
            session.pending = Some(union(session.pending, rect));
        }
    }

    pub(crate) fn liquify_release(&mut self) {
        if let Some(s) = self.layer_state.liquify.as_mut() {
            s.last = None;
            s.pen = None;
        }
        self.liquify_flush();
    }

    pub(crate) fn liquify_is_holding(&self) -> bool {
        self.layer_state
            .liquify
            .as_ref()
            .is_some_and(|s| s.pen.is_some())
    }

    /// Render what the field changed since the last call into the layer
    /// (once a frame, however many pen samples came in), and what a
    /// zoomed-out preview stood in for.
    pub(crate) fn liquify_flush(&mut self) {
        let Some(s) = self.layer_state.liquify.as_mut() else {
            return;
        };
        let rect = match (s.pending.take(), s.previewed.take()) {
            (Some(a), b) => Some(union(b, a)),
            (None, b) => b,
        };
        let shown: Vec<(i32, i32)> = s.preview.drain().map(|(k, _)| k).collect();
        if let Some(rect) = rect {
            self.liquify_redraw(rect);
        }
        for (tx, ty) in shown {
            self.mark_layer_tile_dirty(tx as usize, ty as usize);
        }
    }

    /// The frame's liquify work: with the pen down and the view zoomed out,
    /// only the touched tiles at about the screen's resolution (the layer is
    /// drawn in full once the pen is up); otherwise [`Self::liquify_flush`].
    pub(crate) fn liquify_frame(&mut self, view: &crate::app::view::render::PreviewView) {
        let block = view.block;
        let Some(s) = self.layer_state.liquify.as_mut() else {
            return;
        };
        if block <= 1 || s.pen.is_none() {
            self.liquify_flush();
            return;
        }
        let Some(rect) = s.pending.take() else {
            return;
        };
        s.previewed = Some(union(s.previewed, rect));
        // A new zoom: everything shown so far again, at the new size.
        let mut keys: HashSet<(i32, i32)> = HashSet::new();
        if s.preview_block != block {
            keys.extend(s.preview.drain().map(|(k, _)| k));
            s.preview_block = block;
        }
        let ts = self.canvas.tile_size() as i32;
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        for ty in rect[1].div_euclid(ts)..=(rect[3] - 1).div_euclid(ts) {
            for tx in rect[0].div_euclid(ts)..=(rect[2] - 1).div_euclid(ts) {
                keys.insert((tx, ty));
            }
        }
        let keys: Vec<(i32, i32)> = keys.into_iter().collect();
        let (field, source) = (&s.field, &s.source);
        let tiles: Vec<((i32, i32), Vec<Color32>)> = {
            use rayon::prelude::*;
            self.workspace.pool.install(|| {
                keys.par_iter()
                    .map(|&(tx, ty)| {
                        let w = ts.min(cw - tx * ts) as usize;
                        let h = ts.min(ch - ty * ts) as usize;
                        let px = field.render_tile_shrunk(source, (tx, ty), block, (w, h));
                        ((tx, ty), px)
                    })
                    .collect()
            })
        };
        s.preview.extend(tiles);
        for (tx, ty) in keys {
            self.mark_layer_tile_dirty(tx as usize, ty as usize);
        }
    }

    /// Re-render the tiles under `rect` into the layer. The session began
    /// with the canvas released, and tile writes go through the tiles' own
    /// locks: no wait for the stroke worker here.
    fn liquify_redraw(&mut self, rect: [i32; 4]) {
        let Some(session) = self.layer_state.liquify.as_mut() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(session.layer_id) else {
            return;
        };
        // Only the pixels this dab changed: render them and tell the display
        // exactly that area (not whole tiles).
        let pixels = session.field.render_rect(&session.source, rect);
        let ts = self.canvas.tile_size() as i32;
        for ty in rect[1].div_euclid(ts)..=(rect[3] - 1).div_euclid(ts) {
            for tx in rect[0].div_euclid(ts)..=(rect[2] - 1).div_euclid(ts) {
                session.written.insert((tx, ty));
            }
        }
        let (w, h) = ((rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize);
        self.canvas
            .write_layer_region(idx, (rect[0], rect[1], w, h), &pixels, None);
        self.mark_rect_damage(rect);
    }

    /// Keep the result as one undo step.
    pub(crate) fn liquify_commit(&mut self) {
        self.liquify_flush();
        let Some(session) = self.layer_state.liquify.take() else {
            return;
        };
        if session.written.is_empty() {
            return;
        }
        if self.canvas.layer_index_of(session.layer_id).is_none() {
            return;
        }
        let ts = self.canvas.tile_size();
        let tiles = session
            .written
            .iter()
            .map(|&(tx, ty)| TileSnapshot {
                tx,
                ty,
                layer_id: session.layer_id,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: session
                    .source
                    .get(&(tx, ty))
                    .cloned()
                    .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts])
                    .into(),
            })
            .collect();
        let action = UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: None,
        };
        self.push_undo(action);
    }

    /// Put the layer back as it was before the session.
    pub(crate) fn liquify_cancel(&mut self) {
        let Some(session) = self.layer_state.liquify.take() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(session.layer_id) else {
            return;
        };
        self.release_canvas();
        let ts = self.canvas.tile_size();
        for &(tx, ty) in &session.written {
            let data = session
                .source
                .get(&(tx, ty))
                .cloned()
                .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts]);
            self.canvas.set_layer_tile_data(idx, tx, ty, data);
        }
        self.mark_all_tiles_dirty();
    }
}

#[cfg(test)]
impl LiquifySession {
    pub(crate) fn field_for_test(&mut self) -> &mut LiquifyField {
        &mut self.field
    }
}

#[cfg(test)]
impl PainterApp {
    pub(crate) fn liquify_redraw_for_test(&mut self, rect: [i32; 4]) {
        self.liquify_redraw(rect);
    }
}

fn union(a: Option<[i32; 4]>, b: [i32; 4]) -> [i32; 4] {
    match a {
        None => b,
        Some(a) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::liquify::LiquifyMode;
    use crate::project::tests::test_app_pub;
    use eframe::egui::{Color32, Vec2};

    /// A layer of stripes, liquify active on it.
    fn app() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for ty in 0..4 {
            for tx in 0..4 {
                let tile = (0..64 * 64)
                    .map(|i| Color32::from_rgb(((tx * 64 + i % 64) * 3 % 256) as u8, 0, 90))
                    .collect();
                app.canvas_mut().set_layer_tile_data(1, tx, ty, tile);
            }
        }
        app.active_tool = crate::app::tools::Tool::Liquify;
        app.workspace.liquify.radius = 40.0;
        app
    }

    fn drag(app: &mut crate::PainterApp, mode: LiquifyMode, step: f32) {
        app.workspace.liquify.mode = mode;
        app.liquify_press(Vec2::new(60.0, 128.0));
        let mut x = 60.0;
        while x < 180.0 {
            x += step;
            app.liquify_drag(Vec2::new(x, 128.0));
        }
        app.liquify_release();
        app.liquify_commit();
    }

    fn layer(app: &crate::PainterApp) -> Vec<Option<Vec<Color32>>> {
        (0..16)
            .map(|i| app.canvas.get_layer_tile_data(1, i % 4, i / 4))
            .collect()
    }

    #[test]
    fn a_pens_many_short_moves_liquify_like_a_mouses_few_long_ones() {
        for mode in [LiquifyMode::Push, LiquifyMode::TwirlCw, LiquifyMode::Bloat] {
            let mut pen = app();
            drag(&mut pen, mode, 0.5);
            let mut mouse = app();
            drag(&mut mouse, mode, 12.0);
            // The same dabs (to float rounding in where they land).
            let worst = layer(&pen)
                .iter()
                .zip(layer(&mouse))
                .flat_map(|(a, b)| {
                    let (a, b) = (a.clone().unwrap_or_default(), b.unwrap_or_default());
                    a.iter()
                        .zip(b)
                        .map(|(p, q)| (0..4).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0))
                        .collect::<Vec<_>>()
                })
                .max()
                .unwrap_or(0);
            assert!(worst <= 2, "{mode:?}: off by {worst}");
            assert!(layer(&pen) != layer(&app()), "{mode:?} changed nothing");
        }
    }

    #[test]
    fn moves_edit_the_field_and_the_layer_is_drawn_once_a_frame() {
        let mut app = app();
        let before = layer(&app);
        app.workspace.liquify.mode = LiquifyMode::Push;
        app.liquify_press(Vec2::new(60.0, 128.0));
        for i in 1..=20 {
            app.liquify_drag(Vec2::new(60.0 + i as f32 * 2.0, 128.0));
        }
        assert!(layer(&app) == before, "not drawn yet");
        app.liquify_flush();
        assert!(layer(&app) != before, "drawn by the frame");
    }

    #[test]
    fn zoomed_out_the_screen_gets_a_preview_and_the_layer_waits_for_pen_up() {
        let view = crate::app::view::render::PreviewView {
            xs: 0..4,
            ys: 0..4,
            block: 4,
        };
        let stroke = |app: &mut crate::PainterApp, zoomed_out: bool| {
            app.workspace.liquify.mode = LiquifyMode::Push;
            app.liquify_press(Vec2::new(60.0, 128.0));
            for i in 1..=20 {
                app.liquify_drag(Vec2::new(60.0 + i as f32 * 4.0, 128.0));
                if zoomed_out {
                    app.liquify_frame(&view);
                } else {
                    app.liquify_flush();
                }
            }
        };
        let mut zoomed = app();
        let before = layer(&zoomed);
        stroke(&mut zoomed, true);
        assert!(layer(&zoomed) == before, "layer untouched while previewing");
        let s = zoomed.layer_state.liquify.as_ref().unwrap();
        let (_, px) = s
            .preview_pixels(&zoomed.canvas, 1, 2, 4)
            .expect("previewed");
        assert_eq!(px.len(), 16 * 16);
        zoomed.liquify_release();
        assert!(!zoomed.layer_state.liquify.as_ref().unwrap().previewing());
        let mut full = app();
        stroke(&mut full, false);
        full.liquify_release();
        assert!(
            layer(&zoomed) == layer(&full),
            "pen-up draws what 1:1 would"
        );
    }

    /// A soft edge keeps its colour (it went dark and grey).
    #[test]
    fn liquify_keeps_the_colour_of_soft_edges() {
        let mut app = test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        // A light disc with a soft edge on a transparent layer.
        for ty in 0..4 {
            for tx in 0..4 {
                let tile = (0..64 * 64)
                    .map(|i| {
                        let (x, y) = ((tx * 64 + i % 64) as f32, (ty * 64 + i / 64) as f32);
                        let d = ((x - 128.0).powi(2) + (y - 128.0).powi(2)).sqrt();
                        let a = ((60.0 - d) / 8.0).clamp(0.0, 1.0);
                        Color32::from_rgba_unmultiplied(250, 200, 120, (a * 255.0) as u8)
                    })
                    .collect();
                app.canvas_mut().set_layer_tile_data(1, tx, ty, tile);
            }
        }
        app.active_tool = crate::app::tools::Tool::Liquify;
        app.workspace.liquify.radius = 40.0;
        for mode in [LiquifyMode::Push, LiquifyMode::TwirlCw, LiquifyMode::Bloat] {
            drag(&mut app, mode, 4.0);
        }
        for (i, tile) in layer(&app).into_iter().enumerate() {
            for p in tile.unwrap_or_default() {
                if p.a() < 16 {
                    continue;
                }
                let [r, g, b, _] = p.to_srgba_unmultiplied();
                let off = 250u8
                    .abs_diff(r)
                    .max(200u8.abs_diff(g))
                    .max(120u8.abs_diff(b));
                assert!(off <= 8, "tile {i}: {p:?} unmultiplied {:?}", [r, g, b]);
            }
        }
    }
}
