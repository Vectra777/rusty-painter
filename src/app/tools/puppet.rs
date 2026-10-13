//! The Puppet Warp tool: pins placed on the selected layer's paint (or the
//! selected part of it) bend it, the rest following as rigidly as it can.
//! A session starts with the first pin and keeps the layer as it was; the
//! layer is redrawn once a frame while pins move. Apply (Enter, or picking
//! another tool) makes it one undo step; Cancel (Esc) puts the layer back.
//! Inside a session, undo and redo step through the pins' changes.
//!
//! Pins: click the paint to place one (exactly there), drag to move it
//! (the selected pins move together), Shift+click to add a pin to (or take
//! it from) the selection, Alt+click a pin to remove it, Alt+drag near a
//! pin to turn the paint round it. A layer remembers its warps: picking
//! Puppet Warp on it again (with nothing selected), while its pixels are
//! what the warp made, opens that warp again, on the paint as it was
//! before it.

use crate::PainterApp;
use crate::canvas::history::{TileSnapshot, UndoAction};
use crate::canvas::puppet::arap::PuppetMode;
use crate::canvas::puppet::mesh::{PuppetMesh, PuppetShape};
use crate::canvas::puppet::raster::{self, Picture};
use crate::canvas::puppet::{self, Coverage, Pin, Puppet, Tiles, WarpMemory};
use crate::canvas::storage::{LayerId, LayerKind};
use crate::ui::style::accent;
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A part bigger than this (pixels) is drawn roughly while a pin moves.
const ROUGH_PIXELS: f32 = 2.0e6;
/// Most steps undo goes back through inside a session.
const PIN_STEPS: usize = 200;

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PuppetSettings {
    pub mode: PuppetMode,
    /// The mesh's spacing, canvas pixels: smaller bends more finely.
    pub density: f32,
    /// How far past the paint the mesh reaches, pixels (beyond a few
    /// always there).
    pub expansion: f32,
    pub show_mesh: bool,
}

impl Default for PuppetSettings {
    fn default() -> Self {
        Self {
            mode: PuppetMode::Normal,
            density: 24.0,
            expansion: 0.0,
            show_mesh: true,
        }
    }
}

/// What the pointer is doing to the pins.
#[derive(Clone, Copy)]
pub(crate) enum Drag {
    /// Moving the selected pins, from where the pointer was pressed.
    Move { from: Vec2, moved: bool },
    /// Turning a pin (Alt): from the pointer's angle round it and the turn
    /// it had.
    Turn { pin: usize, from: f32, start: f32 },
    /// Alt pressed on a pin: a click removes it, a drag turns it.
    AltOnPin { pin: usize, from: Vec2 },
}

pub struct PuppetSession {
    layer_id: LayerId,
    /// The layer's tiles as the session found them (what Cancel and undo
    /// put back).
    source: Tiles,
    /// The paint the warp bends (the layer as it was before an earlier
    /// warp, when one is opened again).
    original: Arc<Tiles>,
    /// The part of it bent: a selection's coverage, or all of it.
    part: Option<Arc<Coverage>>,
    /// What stays put where the part's selection reaches: the paint
    /// outside it. Elsewhere it's the original (with a selection) or
    /// nothing.
    base: Tiles,
    picture: Picture,
    puppet: Puppet,
    /// The mesh's spacing and expansion it was made with.
    density: f32,
    expansion: f32,
    /// The mode it was last bent with.
    mode: PuppetMode,
    /// Tiles written into the layer so far.
    written: HashSet<(i32, i32)>,
    /// The pins moved since the layer was last drawn.
    pending: bool,
    /// Bend all the way (not a frame's worth) at the next draw.
    settle: bool,
    /// The selected pins.
    pub selected: Vec<usize>,
    drag: Option<Drag>,
    /// The pins (and the bend, and which were selected) before each
    /// change, for undo inside the session; and what undo took back, for
    /// redo.
    undo: Vec<PinStep>,
    redo: Vec<PinStep>,
    /// Draw the mesh as it is at the next draw (it was put back exactly).
    exact: bool,
}

/// The pins as they were at a step, the bend they made, and the selection.
struct PinStep {
    pins: Vec<Pin>,
    pos: Vec<[f32; 2]>,
    selected: Vec<usize>,
}

impl PuppetSession {
    /// The pins as they are, before a change (for undo).
    fn remember(&mut self) {
        let step = self.step();
        self.undo.push(step);
        if self.undo.len() > PIN_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn step(&self) -> PinStep {
        PinStep {
            pins: self.puppet.pins().to_vec(),
            pos: self.puppet.pos.clone(),
            selected: self.selected.clone(),
        }
    }

    /// The last remembered pins dropped, if nothing changed since.
    fn forget_if_unchanged(&mut self) {
        if self
            .undo
            .last()
            .is_some_and(|step| step.pins.as_slice() == self.puppet.pins())
        {
            self.undo.pop();
        }
    }

    fn redraw_soon(&mut self) {
        self.pending = true;
        self.settle = true;
    }
}

impl PainterApp {
    /// Start a session on the selected layer, if none is running there.
    fn puppet_begin(&mut self) -> bool {
        let active = self.canvas.active_layer_idx;
        let Some(layer_id) = self.canvas.layer_id_at(active) else {
            return false;
        };
        if let Some(session) = &self.layer_state.puppet {
            if session.layer_id == layer_id {
                return true;
            }
            self.puppet_commit();
        }
        let Some(layer) = self.canvas.layers.get(active) else {
            return false;
        };
        if layer.rig.is_some() {
            return false;
        }
        if layer.locked || layer.kind != LayerKind::Paint {
            self.report("Puppet Warp works on an unlocked paint, text or vector layer".into());
            return false;
        }
        // Text and lines become pixels first (their own undo step).
        if layer.text.is_some() {
            self.rasterise_text_layer(active);
            self.report("The text layer was rasterised to warp it".into());
        } else if layer.vector.is_some() {
            self.rasterise_vector_layer(active);
            self.report("The vector layer was rasterised to warp it".into());
        }
        self.bake_impasto(active);
        self.release_canvas();
        let source = self.canvas.capture_layer_pixels(active);
        // A warp of these very pixels: open it again (unless a selection
        // says to warp a part of them anew).
        let fresh = self.selection_manager.has_selection();
        let memory = (!fresh && !self.canvas.layers[active].warps.is_empty())
            .then(|| {
                let now = puppet::checksum(&source);
                let warps = &self.canvas.layers[active].warps;
                warps.iter().rev().find(|w| w.result == now).cloned()
            })
            .flatten();
        let (original, part) = match &memory {
            Some(m) => (m.original.clone(), m.part.clone()),
            None => {
                let part = (self.selection_manager.has_selection())
                    .then(|| self.layer_selection().current_mask())
                    .flatten()
                    .and_then(|m| {
                        let [x0, y0, x1, y1] = m.content_bounds()?;
                        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
                        let data = (0..w * h)
                            .map(|i| m.value(x0 + (i % w) as i32, y0 + (i / w) as i32))
                            .collect();
                        Some(Arc::new(Coverage { x0, y0, w, h, data }))
                    });
                (Arc::new(source.clone()), part)
            }
        };
        if let Some(m) = &memory {
            let s = &mut self.workspace.puppet;
            (s.mode, s.density, s.expansion) = (m.mode, m.density, m.expansion);
        }
        let ts = self.canvas.tile_size();
        let (bent, base) = split(&original, part.as_deref(), ts);
        let Some(picture) = picture_of(&bent, ts) else {
            self.report("Nothing to warp here: the layer (or selection) is empty".into());
            return false;
        };
        let settings = self.workspace.puppet;
        let Some(mut puppet) =
            shape_of(&picture, settings.density, settings.expansion).and_then(Puppet::new)
        else {
            return false;
        };
        // Reopened: the tiles the old warp drew are redrawn, its pins back.
        let mut written = HashSet::new();
        if let Some(m) = &memory {
            puppet.set_pins(m.pins.clone());
            puppet.settle(settings.mode);
            written.extend(source.keys().copied());
            written.extend(original.keys().copied());
            self.report("Editing the layer's warp again".into());
        }
        self.layer_state.puppet = Some(PuppetSession {
            layer_id,
            source,
            original,
            part,
            base,
            picture,
            puppet,
            density: settings.density,
            expansion: settings.expansion,
            mode: settings.mode,
            written,
            pending: memory.is_some(),
            settle: memory.is_some(),
            selected: Vec::new(),
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            exact: false,
        });
        true
    }

    /// With the tool picked: open the selected layer's warp, if it
    /// remembers one for its pixels as they are, so its pins show before
    /// the first click (checked once per layer and document change).
    pub(crate) fn puppet_check_remembered(&mut self) {
        if !matches!(self.active_tool, crate::app::tools::Tool::Puppet)
            || self.layer_state.puppet.is_some()
        {
            return;
        }
        let key = (
            self.canvas.layer_id_at(self.canvas.active_layer_idx),
            self.doc_version(),
        );
        if self.workspace.puppet_checked.as_ref() == Some(&key) {
            return;
        }
        self.workspace.puppet_checked = Some(key);
        self.puppet_open_remembered();
    }

    /// Open the selected layer's warp if it remembers one for its pixels
    /// as they are.
    pub(crate) fn puppet_open_remembered(&mut self) {
        if self.layer_state.puppet.is_some() || self.selection_manager.has_selection() {
            return;
        }
        let active = self.canvas.active_layer_idx;
        let Some(layer) = self.canvas.layers.get(active) else {
            return;
        };
        if layer.warps.is_empty() {
            return;
        }
        let now = puppet::checksum(&self.canvas.capture_layer_pixels(active));
        if layer.warps.iter().any(|w| w.result == now) {
            self.puppet_begin();
        }
    }

    /// A press: grab the pins (Shift: pick pins; Alt: remove or turn), or
    /// place a new one on the paint.
    pub(crate) fn puppet_press(&mut self, pos: Vec2, shift: bool, alt: bool) {
        if self.puppet_on_rig() {
            // A rig's parts: their pins are keys.
            self.puppet_commit_paint();
            self.rig_pins_press(pos, shift, alt);
            return;
        }
        self.rig_pins_finish();
        if !self.puppet_begin() {
            return;
        }
        let radius = 10.0 / self.viewport.zoom.max(1e-3);
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        let p = [pos.x, pos.y];
        let on_pin = s.puppet.pin_near(p, radius);
        if alt {
            if let Some(pin) = on_pin {
                s.drag = Some(Drag::AltOnPin { pin, from: pos });
            } else if let Some(pin) = s.puppet.pin_near(p, radius * 4.0) {
                // Near a pin: turn the paint round it.
                s.remember();
                let at = s.puppet.pins()[pin].at;
                s.selected = vec![pin];
                s.drag = Some(Drag::Turn {
                    pin,
                    from: (pos.y - at[1]).atan2(pos.x - at[0]),
                    start: s.puppet.turn_now(pin),
                });
            }
            return;
        }
        match on_pin {
            Some(pin) if shift => {
                // Into (or out of) the selection.
                match s.selected.iter().position(|&q| q == pin) {
                    Some(k) => {
                        s.selected.remove(k);
                    }
                    None => s.selected.push(pin),
                }
            }
            Some(pin) => {
                if !s.selected.contains(&pin) {
                    s.selected = vec![pin];
                }
                s.remember();
                s.drag = Some(Drag::Move {
                    from: pos,
                    moved: false,
                });
            }
            None => {
                s.remember();
                match s.puppet.add_pin(p) {
                    Some(pin) => {
                        if shift {
                            if !s.selected.contains(&pin) {
                                s.selected.push(pin);
                            }
                        } else {
                            s.selected = vec![pin];
                        }
                        s.drag = Some(Drag::Move {
                            from: pos,
                            moved: false,
                        });
                        s.redraw_soon();
                    }
                    None => {
                        s.forget_if_unchanged();
                        if !shift {
                            s.selected.clear();
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn puppet_drag(&mut self, pos: Vec2) {
        if self.layer_state.rig_pins.is_some() {
            self.rig_pins_drag(pos);
            return;
        }
        let zoom = self.viewport.zoom.max(1e-3);
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        match s.drag {
            Some(Drag::Move { from, .. }) => {
                let by = pos - from;
                for &i in &s.selected.clone() {
                    s.puppet.edit_pin(i, |p| {
                        p.at[0] += by.x;
                        p.at[1] += by.y;
                    });
                }
                s.drag = Some(Drag::Move {
                    from: pos,
                    moved: true,
                });
                s.pending = true;
            }
            Some(Drag::Turn { pin, from, start }) => {
                let at = s.puppet.pins().get(pin).map(|p| p.at);
                if let Some(at) = at {
                    let now = (pos.y - at[1]).atan2(pos.x - at[0]);
                    s.puppet
                        .edit_pin(pin, |p| p.turn = Some(start + now - from));
                    s.pending = true;
                }
            }
            Some(Drag::AltOnPin { pin, from }) => {
                // Dragged off: it's a turn after all.
                if (pos - from).length() * zoom > 3.0
                    && let Some(at) = s.puppet.pins().get(pin).map(|p| p.at)
                {
                    s.remember();
                    s.selected = vec![pin];
                    s.drag = Some(Drag::Turn {
                        pin,
                        from: (from.y - at[1]).atan2(from.x - at[0]),
                        start: s.puppet.turn_now(pin),
                    });
                    self.puppet_drag(pos);
                }
            }
            None => {}
        }
    }

    pub(crate) fn puppet_release(&mut self) {
        if self.layer_state.rig_pins.is_some() {
            self.rig_pins_release();
            return;
        }
        if let Some(s) = self.layer_state.puppet.as_mut() {
            match s.drag.take() {
                Some(Drag::AltOnPin { pin, .. }) => {
                    // Alt+click on a pin: it goes.
                    s.remember();
                    s.puppet.remove_pins(&[pin]);
                    s.selected.clear();
                }
                Some(Drag::Move { moved: false, .. }) | Some(Drag::Turn { .. }) | None => {
                    s.forget_if_unchanged();
                }
                Some(Drag::Move { moved: true, .. }) => {}
            }
            // Let it settle now the pins are still.
            s.redraw_soon();
        }
        self.puppet_flush();
    }

    /// Take the selected pins out.
    pub(crate) fn puppet_remove_pins(&mut self) -> bool {
        if self.layer_state.rig_pins.is_some() {
            return self.rig_pins_remove(false);
        }
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return false;
        };
        if s.selected.is_empty() {
            return false;
        }
        s.remember();
        let which = std::mem::take(&mut s.selected);
        s.puppet.remove_pins(&which);
        s.drag = None;
        s.redraw_soon();
        true
    }

    /// Take every pin out.
    pub(crate) fn puppet_remove_all_pins(&mut self) {
        if self.layer_state.rig_pins.is_some() {
            self.rig_pins_remove(true);
            return;
        }
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        if s.puppet.pins().is_empty() {
            return;
        }
        s.remember();
        let all: Vec<usize> = (0..s.puppet.pins().len()).collect();
        s.puppet.remove_pins(&all);
        s.selected.clear();
        s.drag = None;
        s.redraw_soon();
    }

    /// The selected pin's turn: fixed at `turn` radians, or (`None`) left
    /// to the bend.
    pub(crate) fn puppet_set_turn(&mut self, turn: Option<f32>) {
        if self.layer_state.rig_pins.is_some() {
            self.rig_pins_set_turn(turn);
            return;
        }
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        let [pin] = s.selected[..] else {
            return;
        };
        if s.puppet.pins().get(pin).map(|p| p.turn) == Some(turn) {
            return;
        }
        s.remember();
        s.puppet.edit_pin(pin, |p| p.turn = turn);
        s.redraw_soon();
    }

    /// The selected pins' parts brought forward (`by` > 0) or sent back.
    pub(crate) fn puppet_depth(&mut self, by: i32) {
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        if s.selected.is_empty() {
            return;
        }
        s.remember();
        for &i in &s.selected.clone() {
            s.puppet.edit_pin(i, |p| p.depth += by);
        }
        s.pending = true;
    }

    /// The selected pin's turn (radians), fixed or as the bend has it, if
    /// one pin is selected: and whether it's fixed.
    pub(crate) fn puppet_selected_turn(&self) -> Option<(f32, bool)> {
        if self.layer_state.rig_pins.is_some() {
            return self.rig_pins_selected_turn();
        }
        let s = self.layer_state.puppet.as_ref()?;
        let [pin] = s.selected[..] else {
            return None;
        };
        let fixed = s.puppet.pins().get(pin)?.turn.is_some();
        Some((s.puppet.turn_now(pin), fixed))
    }

    /// Undo (or redo) inside a session: the pins' last change. False when
    /// there's nothing more to step through (undo then cancels the
    /// session).
    pub(crate) fn puppet_step(&mut self, redo: bool) -> bool {
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return false;
        };
        let (from, to) = if redo {
            (&mut s.redo, &mut s.undo)
        } else {
            (&mut s.undo, &mut s.redo)
        };
        let Some(step) = from.pop() else {
            return false;
        };
        let now = PinStep {
            pins: s.puppet.pins().to_vec(),
            pos: s.puppet.pos.clone(),
            selected: s.selected.clone(),
        };
        to.push(now);
        let exact = s.puppet.restore(step.pins, step.pos);
        s.selected = (step.selected.into_iter())
            .filter(|&i| i < s.puppet.pins().len())
            .collect();
        s.drag = None;
        s.redraw_soon();
        s.exact = exact;
        true
    }

    /// Still bending towards where the pins are (more frames to come).
    pub(crate) fn puppet_settling(&self) -> bool {
        self.layer_state.puppet.as_ref().is_some_and(|s| s.pending) || self.rig_pins_settling()
    }

    /// The frame's work: a new mesh if its spacing or reach changed, and
    /// the layer redrawn if the pins moved.
    pub(crate) fn puppet_flush(&mut self) {
        if self.layer_state.rig_pins.is_some() || self.puppet_on_rig() {
            self.rig_pins_flush();
        }
        let settings = self.workspace.puppet;
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        if (s.density != settings.density || s.expansion != settings.expansion)
            && let Some(shape) = shape_of(&s.picture, settings.density, settings.expansion)
        {
            s.puppet.set_shape(shape);
            (s.density, s.expansion) = (settings.density, settings.expansion);
            s.redraw_soon();
        }
        if s.mode != settings.mode {
            s.mode = settings.mode;
            s.redraw_soon();
        }
        if !s.pending {
            return;
        }
        s.pending = false;
        if std::mem::take(&mut s.exact) {
            // Put back as it was: drawn as it is.
            s.settle = false;
        } else if std::mem::take(&mut s.settle) {
            s.puppet.settle(s.mode);
        } else {
            // Not there yet: carry on next frame.
            s.pending = s.puppet.solve(s.mode) > 0.05;
        }
        self.puppet_redraw();
    }

    /// Draw the bent part over what stays into the layer.
    fn puppet_redraw(&mut self) {
        let Some(s) = self.layer_state.puppet.as_mut() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(s.layer_id) else {
            return;
        };
        let ts = self.canvas.tile_size();
        let size = (self.canvas.width(), self.canvas.height());
        let mut drawn = if s.puppet.is_identity() {
            // At rest: exactly the paint as it was, where it was drawn.
            s.written
                .iter()
                .filter_map(|k| Some((*k, s.original.get(k)?.clone())))
                .collect::<HashMap<_, _>>()
        } else {
            // A big part is drawn roughly while a pin is dragged (about two
            // million pixels' worth), in full once it's let go.
            let area = s.picture.width * s.picture.height;
            let step = if s.drag.is_some() {
                ((area as f32 / ROUGH_PIXELS).sqrt().ceil() as usize).clamp(1, 4)
            } else {
                1
            };
            let part = raster::render(
                &s.picture,
                &s.puppet.mesh,
                &s.puppet.pos,
                s.puppet.order(),
                ts,
                size,
                step,
            );
            let mut out: HashMap<(i32, i32), Vec<Color32>> = part.into_iter().collect();
            // Over what stays, on the tiles drawn now or before.
            let keys: Vec<(i32, i32)> = out.keys().chain(&s.written).copied().collect();
            for key in keys {
                let base = s
                    .base
                    .get(&key)
                    .or_else(|| s.part.is_some().then(|| s.original.get(&key)).flatten());
                let Some(base) = base else {
                    continue;
                };
                match out.get_mut(&key) {
                    Some(top) => {
                        for (t, &b) in top.iter_mut().zip(base) {
                            *t = over(*t, b);
                        }
                    }
                    None => {
                        out.insert(key, base.clone());
                    }
                }
            }
            out
        };
        let mut touched: Vec<(i32, i32)> = s.written.iter().copied().collect();
        touched.extend(drawn.keys().filter(|k| !s.written.contains(k)));
        let mut damage: Option<[i32; 4]> = None;
        for key in touched {
            match drawn.remove(&key) {
                Some(data) => self.canvas.set_layer_tile_data(idx, key.0, key.1, data),
                None => self.canvas.clear_layer_tile(idx, key.0, key.1),
            }
            s.written.insert(key);
            let t = ts as i32;
            let r = [key.0 * t, key.1 * t, key.0 * t + t, key.1 * t + t];
            damage = Some(match damage {
                None => r,
                Some(d) => [
                    d[0].min(r[0]),
                    d[1].min(r[1]),
                    d[2].max(r[2]),
                    d[3].max(r[3]),
                ],
            });
        }
        if let Some(rect) = damage {
            self.mark_rect_damage(rect);
        }
    }

    /// Keep the result as one undo step, and remember the warp with the
    /// layer.
    pub(crate) fn puppet_commit(&mut self) {
        self.rig_pins_finish();
        self.puppet_commit_paint();
    }

    /// Keep a paint layer's warp as one undo step, and remember it with the
    /// layer.
    fn puppet_commit_paint(&mut self) {
        if let Some(s) = self.layer_state.puppet.as_mut()
            && (s.pending || s.drag.is_some())
        {
            s.drag = None;
            s.redraw_soon();
        }
        self.puppet_flush();
        let Some(session) = self.layer_state.puppet.take() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(session.layer_id) else {
            return;
        };
        let now = self.canvas.capture_layer_pixels(idx);
        let changed = session.written.iter().any(|k| {
            let blank = |t: Option<&Vec<Color32>>| t.is_none_or(|t| t.iter().all(|c| c.a() == 0));
            match (now.get(k), session.source.get(k)) {
                (a, b) if blank(a) && blank(b) => false,
                (a, b) => a != b,
            }
        });
        if !changed {
            self.puppet_restore(&session);
            return;
        }
        // Remembered: the paint it bent, the pins, and what it made.
        if !session.puppet.pins().is_empty() {
            let memory = Arc::new(WarpMemory {
                original: session.original.clone(),
                part: session.part.clone(),
                pins: session.puppet.pins().to_vec(),
                mode: session.mode,
                density: session.density,
                expansion: session.expansion,
                result: puppet::checksum(&now),
            });
            let warps = &mut self.canvas_mut().layers[idx].warps;
            warps.push(memory);
            if warps.len() > puppet::REMEMBERED {
                warps.remove(0);
            }
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
        self.push_undo(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: None,
        });
    }

    /// Put the layer back as it was before the session.
    pub(crate) fn puppet_cancel(&mut self) {
        // (A rig's pins are already undo steps.)
        self.rig_pins_finish();
        let Some(session) = self.layer_state.puppet.take() else {
            return;
        };
        self.puppet_restore(&session);
    }

    fn puppet_restore(&mut self, session: &PuppetSession) {
        let Some(idx) = self.canvas.layer_index_of(session.layer_id) else {
            return;
        };
        self.release_canvas();
        for &(tx, ty) in &session.written {
            match session.source.get(&(tx, ty)) {
                Some(data) => self.canvas.set_layer_tile_data(idx, tx, ty, data.clone()),
                None => self.canvas.clear_layer_tile(idx, tx, ty),
            }
        }
        self.mark_all_tiles_dirty();
    }

    /// The pins (and the mesh, if shown) of the running session, if any.
    pub(crate) fn puppet_overlay(&self) -> Option<PuppetOverlay<'_>> {
        let s = self.layer_state.puppet.as_ref()?;
        Some(PuppetOverlay {
            mesh: self.workspace.puppet.show_mesh.then_some(&s.puppet.mesh),
            pos: &s.puppet.pos,
            pins: s.puppet.pins().iter().map(|p| (p.at, p.turn)).collect(),
            selected: &s.selected,
        })
    }

    /// Whether the running session has pins selected, and any pins.
    pub(crate) fn puppet_pins_state(&self) -> (bool, bool) {
        if self.layer_state.rig_pins.is_some() {
            return self.rig_pins_state();
        }
        self.layer_state
            .puppet
            .as_ref()
            .map_or((false, false), |s| {
                (!s.selected.is_empty(), !s.puppet.pins().is_empty())
            })
    }
}

pub(crate) struct PuppetOverlay<'a> {
    pub(crate) mesh: Option<&'a PuppetMesh>,
    pub(crate) pos: &'a [[f32; 2]],
    pub(crate) pins: Vec<([f32; 2], Option<f32>)>,
    pub(crate) selected: &'a [usize],
}

/// The mesh and the pins over the canvas.
pub(crate) fn draw_puppet(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> Pos2,
) {
    if !matches!(app.active_tool, crate::app::tools::Tool::Puppet) {
        return;
    }
    let Some(o) = app.puppet_overlay().or_else(|| app.rig_pins_overlay()) else {
        return;
    };
    let at = |p: [f32; 2]| to_screen(app.to_canvas_space(Vec2::new(p[0], p[1])));
    if let Some(mesh) = o.mesh {
        let line = Stroke::new(1.0_f32, Color32::from_white_alpha(70));
        let mut seen = HashSet::new();
        for t in &mesh.tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if seen.insert((a.min(b), a.max(b))) {
                    painter.line_segment([at(o.pos[a as usize]), at(o.pos[b as usize])], line);
                }
            }
        }
    }
    let shadow = Stroke::new(3.0_f32, Color32::from_black_alpha(140));
    for (i, (p, turn)) in o.pins.iter().enumerate() {
        let c = at(*p);
        let selected = o.selected.contains(&i);
        painter.circle_stroke(c, 6.0, shadow);
        painter.circle(
            c,
            5.0,
            if selected { accent() } else { Color32::WHITE },
            Stroke::new(1.5_f32, Color32::BLACK),
        );
        if let Some(t) = turn {
            // A pin with a fixed turn: a ring with a tick showing it.
            painter.circle_stroke(c, 11.0, Stroke::new(1.5_f32, accent()));
            let tip = c + Vec2::angled(*t) * 11.0;
            painter.line_segment([c, tip], Stroke::new(1.5_f32, accent()));
        }
    }
}

/// `original`'s tiles split by `part`: what's bent (all of it without a
/// part) and what stays (only where the part reaches).
fn split(original: &Tiles, part: Option<&Coverage>, ts: usize) -> (Tiles, Tiles) {
    let Some(m) = part else {
        return (original.clone(), Tiles::new());
    };
    let [x0, y0, x1, y1] = m.bounds();
    let t = ts as i32;
    let (mut bent, mut base) = (Tiles::new(), Tiles::new());
    for (&(tx, ty), tile) in original {
        // Tiles the part doesn't reach stay as they are.
        if tx * t >= x1 || ty * t >= y1 || tx * t + t <= x0 || ty * t + t <= y0 {
            continue;
        }
        let (mut inside, mut outside) = (tile.clone(), tile.clone());
        for (i, (a, b)) in inside.iter_mut().zip(outside.iter_mut()).enumerate() {
            let x = tx * t + (i % ts) as i32;
            let y = ty * t + (i / ts) as i32;
            let k = m.value(x, y) as f32 / 255.0;
            *a = a.gamma_multiply(k);
            *b = b.gamma_multiply(1.0 - k);
        }
        bent.insert((tx, ty), inside);
        base.insert((tx, ty), outside);
    }
    (bent, base)
}

/// `top` over `bottom`, premultiplied.
fn over(top: Color32, bottom: Color32) -> Color32 {
    if top.a() == 255 || bottom.a() == 0 {
        return top;
    }
    let keep = 255 - top.a() as u32;
    let mix = |t: u8, b: u8| (t as u32 + (b as u32 * keep + 127) / 255).min(255) as u8;
    Color32::from_rgba_premultiplied(
        mix(top.r(), bottom.r()),
        mix(top.g(), bottom.g()),
        mix(top.b(), bottom.b()),
        mix(top.a(), bottom.a()),
    )
}

/// The painted pixels of `tiles` as one picture (cropped to the paint).
fn picture_of(tiles: &Tiles, ts: usize) -> Option<Picture> {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (&(tx, ty), tile) in tiles {
        for (i, c) in tile.iter().enumerate() {
            if c.a() == 0 {
                continue;
            }
            let (x, y) = (
                tx * ts as i32 + (i % ts) as i32,
                ty * ts as i32 + (i / ts) as i32,
            );
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut pixels = vec![Color32::TRANSPARENT; w * h];
    for (&(tx, ty), tile) in tiles {
        for (i, &c) in tile.iter().enumerate() {
            let (x, y) = (
                tx * ts as i32 + (i % ts) as i32,
                ty * ts as i32 + (i / ts) as i32,
            );
            if c.a() > 0 && x >= x0 && y >= y0 && x < x1 && y < y1 {
                pixels[(y - y0) as usize * w + (x - x0) as usize] = c;
            }
        }
    }
    Some(Picture {
        x0,
        y0,
        width: w,
        height: h,
        pixels,
    })
}

/// The picture's shape: points `density` apart, reaching `expansion`
/// pixels further past the paint.
fn shape_of(picture: &Picture, density: f32, expansion: f32) -> Option<PuppetShape> {
    let (x0, y0, w) = (picture.x0, picture.y0, picture.width);
    PuppetShape::from_paint(
        [x0, y0, x0 + w as i32, y0 + picture.height as i32],
        density,
        expansion.max(0.0).round() as usize,
        |x, y| picture.pixels[(y - y0) as usize * w + (x - x0) as usize].a() > 0,
    )
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::project::tests::test_app_pub;
    use eframe::egui::{Color32, Vec2};

    /// A bar of colour on layer 1, Puppet Warp picked.
    fn app() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for ty in 0..4 {
            for tx in 0..4 {
                let tile = (0..64 * 64)
                    .map(|i| {
                        let (x, y) = (tx * 64 + i % 64, ty * 64 + i / 64);
                        if (40..216).contains(&x) && (110..146).contains(&y) {
                            Color32::from_rgb((x % 256) as u8, 60, 200)
                        } else {
                            Color32::TRANSPARENT
                        }
                    })
                    .collect();
                app.canvas_mut().set_layer_tile_data(1, tx, ty, tile);
            }
        }
        app.active_tool = crate::app::tools::Tool::Puppet;
        app
    }

    /// The layer's tiles (an empty tile as none).
    fn layer(app: &crate::PainterApp) -> Vec<Option<Vec<Color32>>> {
        (0..16)
            .map(|i| app.canvas.get_layer_tile_data(1, i % 4, i / 4))
            .map(|t| t.filter(|t| t.iter().any(|c| c.a() > 0)))
            .collect()
    }

    fn px(l: &[Option<Vec<Color32>>], x: usize, y: usize) -> Option<Color32> {
        l[(y / 64) * 4 + x / 64]
            .as_ref()
            .map(|t| t[(y % 64) * 64 + x % 64])
            .filter(|c| c.a() > 0)
    }

    fn click(app: &mut crate::PainterApp, at: Vec2, shift: bool, alt: bool) {
        app.puppet_press(at, shift, alt);
        app.puppet_release();
    }

    fn drag(app: &mut crate::PainterApp, from: Vec2, by: Vec2) {
        app.puppet_press(from, false, false);
        for i in 1..=10 {
            app.puppet_drag(from + by * (i as f32 / 10.0));
            app.puppet_flush();
        }
        app.puppet_release();
    }

    fn pins(app: &crate::PainterApp) -> usize {
        app.layer_state
            .puppet
            .as_ref()
            .map_or(0, |s| s.puppet.pins().len())
    }

    /// Pins at both ends and the middle; the right end pulled down.
    fn bend(app: &mut crate::PainterApp) {
        for x in [50.0, 128.0] {
            click(app, Vec2::new(x, 128.0), false, false);
        }
        drag(app, Vec2::new(206.0, 128.0), Vec2::new(0.0, 40.0));
    }

    #[test]
    fn a_bend_applies_as_one_step_and_undoes_exactly() {
        let mut app = app();
        let before = layer(&app);
        bend(&mut app);
        assert!(layer(&app) != before, "bent while the session runs");
        app.puppet_commit();
        assert!(app.layer_state.puppet.is_none());
        let bent = layer(&app);
        assert!(bent != before);
        // By the left end's pin the paint stays (to a fraction of a
        // pixel: a pin holds a point, not the paint round it).
        let (a, b) = (px(&bent, 45, 128).unwrap(), px(&before, 45, 128).unwrap());
        assert!((0..4).all(|k| a[k].abs_diff(b[k]) <= 3), "{a:?} vs {b:?}");
        // The right end went down: paint below where the bar was.
        assert!(px(&bent, 206, 160).is_some());
        app.apply_history(false);
        assert!(layer(&app) == before, "undo puts it back");
        app.apply_history(true);
        assert!(layer(&app) == bent, "redo bends it again");
    }

    #[test]
    fn cancel_puts_the_layer_back_and_pins_alone_change_nothing() {
        let mut app = app();
        let before = layer(&app);
        bend(&mut app);
        app.puppet_cancel();
        assert!(layer(&app) == before);
        // Pins placed but nothing moved: nothing to keep.
        click(&mut app, Vec2::new(60.0, 128.0), false, false);
        app.puppet_commit();
        assert!(layer(&app) == before);
        assert!(app.canvas.layers[1].warps.is_empty());
    }

    #[test]
    fn with_a_selection_only_the_selected_part_bends() {
        let mut app = app();
        app.selection_manager.canvas_size = [256, 256];
        app.selection_manager.apply_shape(
            crate::selection::SelectionShape::Rectangle {
                start: Vec2::new(128.0, 100.0),
                end: Vec2::new(230.0, 160.0),
            },
            crate::selection::SelectionMode::Replace,
        );
        let before = layer(&app);
        click(&mut app, Vec2::new(140.0, 128.0), false, false);
        drag(&mut app, Vec2::new(206.0, 128.0), Vec2::new(0.0, 40.0));
        app.puppet_commit();
        let after = layer(&app);
        for y in 0..256 {
            for x in 0..120 {
                assert_eq!(px(&after, x, y), px(&before, x, y), "{x},{y}");
            }
        }
        assert!(after != before);
    }

    #[test]
    fn selected_pins_move_together_and_shift_picks_them() {
        let mut app = app();
        for x in [60.0, 100.0, 180.0] {
            click(&mut app, Vec2::new(x, 128.0), false, false);
        }
        // Shift+click the first two: with the third, three are picked.
        click(&mut app, Vec2::new(60.0, 128.0), true, false);
        click(&mut app, Vec2::new(100.0, 128.0), true, false);
        assert_eq!(app.layer_state.puppet.as_ref().unwrap().selected.len(), 3);
        // Shift+click again takes one out.
        click(&mut app, Vec2::new(100.0, 128.0), true, false);
        assert_eq!(app.layer_state.puppet.as_ref().unwrap().selected.len(), 2);
        drag(&mut app, Vec2::new(180.0, 128.0), Vec2::new(0.0, 20.0));
        let s = app.layer_state.puppet.as_ref().unwrap();
        let at: Vec<[f32; 2]> = s.puppet.pins().iter().map(|p| p.at).collect();
        assert!((at[0][1] - 148.0).abs() < 0.01, "{at:?}");
        assert!((at[1][1] - 128.0).abs() < 0.01, "left alone: {at:?}");
        assert!((at[2][1] - 148.0).abs() < 0.01, "{at:?}");
    }

    #[test]
    fn alt_click_removes_a_pin_and_remove_all_takes_the_rest() {
        let mut app = app();
        for x in [60.0, 120.0, 180.0] {
            click(&mut app, Vec2::new(x, 128.0), false, false);
        }
        click(&mut app, Vec2::new(120.0, 128.0), false, true);
        assert_eq!(pins(&app), 2);
        app.puppet_remove_all_pins();
        assert_eq!(pins(&app), 0);
    }

    #[test]
    fn undo_inside_steps_through_the_pins_and_past_them_cancels() {
        let mut app = app();
        let before = layer(&app);
        for x in [50.0, 128.0, 206.0] {
            click(&mut app, Vec2::new(x, 128.0), false, false);
        }
        drag(&mut app, Vec2::new(206.0, 128.0), Vec2::new(0.0, 40.0));
        let bent = layer(&app);
        assert_eq!(pins(&app), 3);
        // The drag back, then the pins one by one.
        app.apply_history(false);
        app.puppet_flush();
        assert_eq!(pins(&app), 3);
        assert!(layer(&app) == before, "the drag undone: at rest again");
        app.apply_history(true);
        app.puppet_flush();
        assert!(layer(&app) == bent, "redone");
        for left in [3, 2, 1, 0] {
            app.apply_history(false);
            assert_eq!(pins(&app), left);
        }
        app.apply_history(false);
        assert!(
            app.layer_state.puppet.is_none(),
            "past the first pin: cancelled"
        );
        assert!(layer(&app) == before);
    }

    #[test]
    fn a_layer_reopens_its_warp_on_the_paint_before_it() {
        let mut app = app();
        let before = layer(&app);
        bend(&mut app);
        app.puppet_commit();
        assert_eq!(app.canvas.layers[1].warps.len(), 1);
        // Picking the tool on it again: its pins are back.
        app.puppet_open_remembered();
        assert_eq!(pins(&app), 3);
        // Taking every pin out shows the paint from before the warp.
        app.puppet_remove_all_pins();
        app.puppet_flush();
        assert!(layer(&app) == before);
        app.puppet_cancel();
        // Undone, the warp's pixels are gone: nothing to reopen. Redone,
        // they're back, and so is the warp.
        app.apply_history(false);
        app.puppet_open_remembered();
        assert!(app.layer_state.puppet.is_none());
        app.apply_history(true);
        app.puppet_open_remembered();
        assert_eq!(pins(&app), 3);
        app.puppet_cancel();
        // Painted over (any other change): forgotten.
        let ts = app.canvas.tile_size();
        app.canvas
            .set_layer_tile_data(1, 0, 0, vec![Color32::RED; ts * ts]);
        app.puppet_open_remembered();
        assert!(app.layer_state.puppet.is_none());
    }

    #[test]
    fn a_fixed_turn_turns_the_paint_and_auto_lets_it_go() {
        let mut app = app();
        click(&mut app, Vec2::new(60.0, 128.0), false, false);
        let before = layer(&app);
        app.puppet_set_turn(Some(0.4));
        assert_eq!(app.puppet_selected_turn(), Some((0.4, true)));
        app.puppet_flush();
        // The far end swung down round the pin.
        assert!(px(&layer(&app), 206, 128).is_none(), "the end moved away");
        assert!(layer(&app) != before);
        app.puppet_set_turn(None);
        app.puppet_flush();
        assert!(layer(&app) == before, "one pin, no turn: at rest");
    }

    #[test]
    fn with_a_selection_a_new_warp_starts_instead() {
        let mut app = app();
        bend(&mut app);
        app.puppet_commit();
        app.selection_manager.canvas_size = [256, 256];
        app.selection_manager.apply_shape(
            crate::selection::SelectionShape::Rectangle {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(256.0, 256.0),
            },
            crate::selection::SelectionMode::Replace,
        );
        app.puppet_open_remembered();
        assert!(app.layer_state.puppet.is_none());
        click(&mut app, Vec2::new(60.0, 128.0), false, false);
        assert_eq!(pins(&app), 1, "a new warp, its own pins");
    }

    #[test]
    fn remembered_warps_are_saved_with_the_document() {
        let mut app = app();
        bend(&mut app);
        app.puppet_commit();
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let mut again = test_app_pub(loaded.canvas);
        again.canvas_mut().active_layer_idx = 1;
        again.active_tool = crate::app::tools::Tool::Puppet;
        assert_eq!(again.canvas.layers[1].warps.len(), 1);
        again.puppet_open_remembered();
        assert_eq!(pins(&again), 3);
    }

    #[test]
    fn a_text_layer_is_rasterised_to_warp_it() {
        let mut app = app();
        app.canvas_mut().layers[1].text = Some(Box::new(crate::canvas::text::TextLayer {
            text: "Hi".into(),
            font: String::new(),
            style: Default::default(),
            color: Color32::BLACK,
            pos: Vec2::ZERO,
        }));
        click(&mut app, Vec2::new(100.0, 128.0), false, false);
        assert!(app.canvas.layers[1].text.is_none());
        assert_eq!(pins(&app), 1);
    }
}
