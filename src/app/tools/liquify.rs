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
    /// Where the brush last dabbed, while it's down.
    last: Option<Vec2>,
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
        });
        true
    }

    pub(crate) fn liquify_press(&mut self, pos: Vec2) {
        if !self.liquify_begin() {
            return;
        }
        if let Some(s) = self.layer_state.liquify.as_mut() {
            s.last = Some(pos);
        }
        if self.workspace.liquify.mode.is_continuous() {
            self.liquify_hold(1.0 / 30.0);
        }
    }

    /// Brush moved to `pos`: dab along the way.
    pub(crate) fn liquify_drag(&mut self, pos: Vec2) {
        let settings = &self.workspace.liquify;
        let (mode, radius, strength) = (settings.mode, settings.radius, settings.strength);
        let Some(session) = self.layer_state.liquify.as_mut() else {
            return;
        };
        let Some(last) = session.last else {
            return;
        };
        let travel = pos - last;
        let spacing = (radius * 0.12).max(1.0);
        let steps = (travel.length() / spacing).ceil().max(1.0) as usize;
        let step = travel / steps as f32;
        let mut changed: Option<[i32; 4]> = None;
        for i in 1..=steps {
            let center = last + step * i as f32;
            let (amount, delta) = match mode {
                LiquifyMode::Push => (strength, step),
                _ => (strength * 0.15, Vec2::ZERO),
            };
            if let Some(r) = session.field.dab(mode, center, radius, amount, delta) {
                changed = Some(union(changed, r));
            }
        }
        session.last = Some(pos);
        if let Some(rect) = changed {
            self.liquify_redraw(rect);
        }
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
        let Some(center) = session.last else {
            return;
        };
        // About 1.75 rad/s of twirl (or 60% pinch/bloat per second) at the
        // centre at 50% strength; it was a sluggish third of that.
        let amount = (strength * dt * 10.0).min(1.0);
        if let Some(rect) = session.field.dab(mode, center, radius, amount, Vec2::ZERO) {
            self.liquify_redraw(rect);
        }
    }

    pub(crate) fn liquify_release(&mut self) {
        if let Some(s) = self.layer_state.liquify.as_mut() {
            s.last = None;
        }
    }

    pub(crate) fn liquify_is_holding(&self) -> bool {
        self.layer_state
            .liquify
            .as_ref()
            .is_some_and(|s| s.last.is_some())
    }

    /// Re-render the tiles under `rect` into the layer.
    fn liquify_redraw(&mut self, rect: [i32; 4]) {
        self.release_canvas();
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
