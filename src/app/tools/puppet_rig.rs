//! Puppet Warp on a rig layer: pins on one of its parts (a mesh picture,
//! as Make Rig from Layers makes them) bend it at the frame showing, and
//! the bend is kept as a deform key there, in the rig's animation. Keys at
//! two frames and the rig works out the ones between (each point's offset
//! eased from one key to the next), so playback, export and onion skins
//! show it with no more work. The pins are kept with the part (where the
//! mesh puts them at any frame); their turns aren't.
//!
//! Every change is an undo step at once (a drag is one): there's nothing
//! to apply or cancel. Click a part to pin it, drag pins to bend it,
//! Shift+click to pick several, Alt+click a pin to remove it, Alt+drag
//! near a pin to turn the part round it.

use super::puppet::{Drag, PuppetOverlay};
use crate::PainterApp;
use crate::app::autosave::Version;
use crate::canvas::puppet::Puppet;
use crate::canvas::puppet::mesh::PuppetMesh;
use crate::canvas::rig::{Curve, DeformTrack, Rig, Shape, set_key};
use crate::canvas::storage::LayerId;
use eframe::egui::Vec2;

pub struct RigPins {
    layer_id: LayerId,
    /// The part: slot and attachment.
    slot: usize,
    attachment: usize,
    /// What it was made for: the frame, the document and the animation.
    time: u32,
    version: Version,
    animation: Option<usize>,
    puppet: Puppet,
    pub selected: Vec<usize>,
    drag: Option<Drag>,
    /// The bend changed since its key was written.
    moved: bool,
    /// Which points are pinned changed since they were written.
    pins_changed: bool,
    /// Keys written in the gesture going on (its undo step open).
    wrote: bool,
    /// Still bending towards the pins; all the way at the next frame.
    pending: bool,
    settle: bool,
}

/// The part of `rig` showing at `p` (canvas) in `pose`, topmost first:
/// (slot, attachment), and whether it's a mesh.
fn part_at(
    rig: &Rig,
    pose: &crate::canvas::rig::eval::Pose,
    p: [f32; 2],
) -> Option<(usize, usize, bool)> {
    (0..rig.slots.len()).rev().find_map(|s| {
        let ai = pose.attachments.get(s).copied().flatten()?;
        if pose.colors.get(s).is_some_and(|c| c[3] <= 0.0) {
            return None;
        }
        let (points, _, tris) = rig.attachment_mesh(pose, s, ai, true)?;
        let on = tris.iter().any(|t| {
            let t = t.map(|k| k as usize);
            if t.iter().any(|&k| k >= points.len()) {
                return false;
            }
            inside(p, points[t[0]], points[t[1]], points[t[2]])
        });
        let mesh = matches!(rig.slots[s].attachments[ai].shape, Shape::Mesh { .. });
        on.then_some((s, ai, mesh))
    })
}

fn inside(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let cross =
        |u: [f32; 2], v: [f32; 2]| (v[0] - u[0]) * (p[1] - u[1]) - (v[1] - u[1]) * (p[0] - u[0]);
    let (d0, d1, d2) = (cross(a, b), cross(b, c), cross(c, a));
    !((d0 < 0.0 || d1 < 0.0 || d2 < 0.0) && (d0 > 0.0 || d1 > 0.0 || d2 > 0.0))
}

/// The time (seconds) of the document's frame `time`.
fn seconds(time: u32, fps: u32) -> f32 {
    time as f32 / fps.max(1) as f32
}

/// The deform track for a part in the rig's animation, if there's one.
fn track(rig: &Rig, slot: usize, attachment: usize) -> Option<&DeformTrack> {
    let name = &rig.slots.get(slot)?.attachments.get(attachment)?.name;
    let anim = rig.animations.get(rig.animation?)?;
    anim.deforms
        .iter()
        .find(|t| t.slot == slot && &t.attachment == name)
}

impl PainterApp {
    /// Whether the selected layer is a rig (Puppet Warp pins its parts).
    pub(crate) fn puppet_on_rig(&self) -> bool {
        self.active_rig().is_some()
    }

    /// A session on part (`slot`, `attachment`) of the selected rig, at the
    /// frame showing: its pins put back. False if it isn't a mesh.
    fn rig_pins_open(&mut self, slot: usize, attachment: usize, keep_selected: bool) -> bool {
        let Some(layer_id) = self.canvas.layer_id_at(self.canvas.active_layer_idx) else {
            return false;
        };
        let Some(rig) = self.active_rig() else {
            return false;
        };
        let time = self.canvas.time;
        let pose = rig.pose(seconds(time, self.canvas.timeline.fps));
        let is_mesh = rig
            .slots
            .get(slot)
            .and_then(|s| s.attachments.get(attachment))
            .is_some_and(|a| matches!(a.shape, Shape::Mesh { .. }));
        if !is_mesh {
            return false;
        }
        let (Some((rest, _, tris)), Some((pos, _, _))) = (
            rig.attachment_mesh(&pose, slot, attachment, false),
            rig.attachment_mesh(&pose, slot, attachment, true),
        ) else {
            return false;
        };
        let pinned: Vec<usize> = track(rig, slot, attachment)
            .map(|t| t.pins.iter().map(|&k| k as usize).collect())
            .unwrap_or_default();
        let animation = rig.animation;
        let Some(mut puppet) = Puppet::fixed(PuppetMesh::from_triangles(rest, tris), pos) else {
            return false;
        };
        puppet.pin_points(&pinned);
        let selected = match &self.layer_state.rig_pins {
            Some(s)
                if keep_selected
                    && s.layer_id == layer_id
                    && s.slot == slot
                    && s.attachment == attachment =>
            {
                s.selected
                    .iter()
                    .copied()
                    .filter(|&i| i < puppet.pins().len())
                    .collect()
            }
            _ => Vec::new(),
        };
        self.layer_state.rig_pins = Some(RigPins {
            layer_id,
            slot,
            attachment,
            time,
            version: self.doc_version(),
            animation,
            puppet,
            selected,
            drag: None,
            moved: false,
            pins_changed: false,
            wrote: false,
            pending: false,
            settle: false,
        });
        true
    }

    /// The session made again if what it was made for changed (another
    /// frame, an undo, another layer), on the same part; or dropped. With
    /// none, the selected rig's first pinned part opens, so its pins show.
    pub(crate) fn rig_pins_sync(&mut self) {
        if !matches!(self.active_tool, crate::app::tools::Tool::Puppet) {
            self.layer_state.rig_pins = None;
            return;
        }
        let Some(rig) = self.active_rig() else {
            self.layer_state.rig_pins = None;
            return;
        };
        let layer_id = self.canvas.layer_id_at(self.canvas.active_layer_idx);
        let (time, version, animation) = (self.canvas.time, self.doc_version(), rig.animation);
        match &self.layer_state.rig_pins {
            Some(s) if s.drag.is_some() => {}
            Some(s) if Some(s.layer_id) == layer_id => {
                if s.time != time || s.version != version || s.animation != animation {
                    let (slot, ai) = (s.slot, s.attachment);
                    if !self.rig_pins_open(slot, ai, true) {
                        self.layer_state.rig_pins = None;
                    }
                }
            }
            _ => {
                // The first part with pins, if any.
                let first = (0..rig.slots.len()).find_map(|s| {
                    (0..rig.slots[s].attachments.len())
                        .find(|&a| track(rig, s, a).is_some_and(|t| !t.pins.is_empty()))
                        .map(|a| (s, a))
                });
                self.layer_state.rig_pins = None;
                if let Some((s, a)) = first {
                    self.rig_pins_open(s, a, false);
                }
            }
        }
    }

    /// A press on a rig layer: grab pins, or pin the part under it (another
    /// part: its pins instead).
    pub(crate) fn rig_pins_press(&mut self, pos: Vec2, shift: bool, alt: bool) {
        self.rig_pins_sync();
        let radius = 10.0 / self.viewport.zoom.max(1e-3);
        let p = [pos.x, pos.y];
        let on_pin =
            (self.layer_state.rig_pins.as_ref()).and_then(|s| s.puppet.pin_near(p, radius));
        if on_pin.is_none() && !alt {
            // On another part (the top one under the pointer): open it.
            let Some(rig) = self.active_rig() else {
                return;
            };
            let pose = rig.pose(seconds(self.canvas.time, self.canvas.timeline.fps));
            match part_at(rig, &pose, p) {
                Some((_, _, false)) => {
                    self.report("Only mesh parts can be pinned (Layer → Rig → Make Rig from Layers makes them)".into());
                    return;
                }
                Some((slot, ai, true)) => {
                    let same = (self.layer_state.rig_pins.as_ref())
                        .is_some_and(|s| s.slot == slot && s.attachment == ai);
                    if !same && !self.rig_pins_open(slot, ai, false) {
                        return;
                    }
                }
                None => {
                    if let Some(s) = self.layer_state.rig_pins.as_mut()
                        && !shift
                    {
                        s.selected.clear();
                    }
                    return;
                }
            }
        }
        let Some(s) = self.layer_state.rig_pins.as_mut() else {
            return;
        };
        if alt {
            if let Some(pin) = on_pin {
                s.drag = Some(Drag::AltOnPin { pin, from: pos });
            } else if let Some(pin) = s.puppet.pin_near(p, radius * 4.0) {
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
            Some(pin) if shift => match s.selected.iter().position(|&q| q == pin) {
                Some(k) => {
                    s.selected.remove(k);
                }
                None => s.selected.push(pin),
            },
            Some(pin) => {
                if !s.selected.contains(&pin) {
                    s.selected = vec![pin];
                }
                s.drag = Some(Drag::Move {
                    from: pos,
                    moved: false,
                });
            }
            None => {
                let before = s.puppet.pins().len();
                if let Some(pin) = s.puppet.add_pin(p) {
                    s.pins_changed |= s.puppet.pins().len() != before;
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
                }
            }
        }
    }

    pub(crate) fn rig_pins_drag(&mut self, pos: Vec2) {
        let zoom = self.viewport.zoom.max(1e-3);
        let Some(s) = self.layer_state.rig_pins.as_mut() else {
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
                (s.moved, s.pending) = (true, true);
            }
            Some(Drag::Turn { pin, from, start }) => {
                if let Some(at) = s.puppet.pins().get(pin).map(|p| p.at) {
                    let now = (pos.y - at[1]).atan2(pos.x - at[0]);
                    s.puppet
                        .edit_pin(pin, |p| p.turn = Some(start + now - from));
                    (s.moved, s.pending) = (true, true);
                }
            }
            Some(Drag::AltOnPin { pin, from }) => {
                if (pos - from).length() * zoom > 3.0
                    && let Some(at) = s.puppet.pins().get(pin).map(|p| p.at)
                {
                    s.selected = vec![pin];
                    s.drag = Some(Drag::Turn {
                        pin,
                        from: (from.y - at[1]).atan2(from.x - at[0]),
                        start: s.puppet.turn_now(pin),
                    });
                    self.rig_pins_drag(pos);
                }
            }
            None => {}
        }
    }

    pub(crate) fn rig_pins_release(&mut self) {
        let Some(s) = self.layer_state.rig_pins.as_mut() else {
            return;
        };
        match s.drag.take() {
            Some(Drag::AltOnPin { pin, .. }) => {
                s.puppet.remove_pins(&[pin]);
                s.selected.clear();
                s.pins_changed = true;
            }
            // Let it settle now the pins are still, and key where it gets.
            Some(Drag::Move { moved: true, .. }) | Some(Drag::Turn { .. }) => {
                (s.moved, s.pending, s.settle) = (true, true, true);
            }
            _ => {}
        }
        self.rig_pins_flush();
    }

    /// The frame's work: bend towards the pins, and keep the bend as a key
    /// at this frame (a drag's keys one undo step, closed when it ends).
    pub(crate) fn rig_pins_flush(&mut self) {
        self.rig_pins_sync();
        let mode = self.workspace.puppet.mode;
        let Some(s) = self.layer_state.rig_pins.as_mut() else {
            return;
        };
        if s.pending {
            s.pending = false;
            if std::mem::take(&mut s.settle) {
                s.puppet.settle(mode);
            } else {
                s.pending = s.puppet.solve(mode) > 0.05;
            }
        }
        let (key, pins) = (s.moved, s.pins_changed);
        let done = s.drag.is_none() && !s.pending;
        if key || (pins && done) {
            self.rig_pins_write(key);
        }
        if done
            && let Some(s) = self.layer_state.rig_pins.as_mut()
            && std::mem::take(&mut s.wrote)
        {
            self.rig_edit_done();
        }
    }

    /// Still bending towards where the pins are (more frames to come).
    pub(crate) fn rig_pins_settling(&self) -> bool {
        self.layer_state
            .rig_pins
            .as_ref()
            .is_some_and(|s| s.pending)
    }

    /// The pins into the part's track, and (`key`) its bend as a key at the
    /// frame showing: the rig's animation made if it has none, and made
    /// longer if the frame is past its end.
    fn rig_pins_write(&mut self, key: bool) {
        let Some(s) = self.layer_state.rig_pins.as_ref() else {
            return;
        };
        let (slot, ai) = (s.slot, s.attachment);
        let pins: Vec<u32> = s.puppet.pins().iter().map(|p| p.point as u32).collect();
        let moves: Vec<[f32; 2]> = (s.puppet.pos.iter().zip(&s.puppet.mesh.rest))
            .map(|(p, r)| [p[0] - r[0], p[1] - r[1]])
            .collect();
        let (time, timeline) = (self.canvas.time, self.canvas.timeline);
        let made = self
            .active_rig()
            .is_some_and(|r| r.animation.is_none_or(|a| a >= r.animations.len()));
        self.rig_edit(move |rig| {
            write_key(rig, slot, ai, time, &timeline, &pins, key.then_some(&moves))
        });
        if made {
            self.report("The rig had no animation: one was made to key the pins in".into());
        }
        // What it's now made for.
        let (version, animation) = (
            self.doc_version(),
            self.active_rig().and_then(|r| r.animation),
        );
        if let Some(s) = self.layer_state.rig_pins.as_mut() {
            (s.version, s.animation) = (version, animation);
            if key {
                s.moved = false;
            }
            s.pins_changed = false;
            s.wrote = true;
        }
    }

    /// Key the part's shape as it is at this frame (to hold it here before
    /// bending it at another).
    pub(crate) fn rig_pins_key_now(&mut self) {
        if self.layer_state.rig_pins.is_none() {
            return;
        }
        self.rig_pins_write(true);
        if let Some(s) = self.layer_state.rig_pins.as_mut() {
            s.wrote = false;
        }
        self.rig_edit_done();
    }

    /// The part's key at this frame, if it has one.
    pub(crate) fn rig_pins_key_here(&self) -> Option<Curve> {
        let s = self.layer_state.rig_pins.as_ref()?;
        let rig = self.active_rig()?;
        let t = seconds(self.canvas.time, self.canvas.timeline.fps);
        let track = track(rig, s.slot, s.attachment)?;
        track
            .keys
            .iter()
            .find(|k| (k.time - t).abs() < 1e-4)
            .map(|k| k.curve)
    }

    /// Take the part's key at this frame out.
    pub(crate) fn rig_pins_delete_key(&mut self) {
        let Some(s) = self.layer_state.rig_pins.as_ref() else {
            return;
        };
        let (slot, ai) = (s.slot, s.attachment);
        let t = seconds(self.canvas.time, self.canvas.timeline.fps);
        if self.rig_pins_key_here().is_none() {
            return;
        }
        self.rig_edit(|rig| {
            let Some(name) = rig.slots[slot].attachments.get(ai).map(|a| a.name.clone()) else {
                return;
            };
            if let Some(anim) = rig.animation.and_then(|a| rig.animations.get_mut(a))
                && let Some(track) = anim
                    .deforms
                    .iter_mut()
                    .find(|t| t.slot == slot && t.attachment == name)
            {
                track.keys.retain(|k| (k.time - t).abs() >= 1e-4);
            }
        });
        self.rig_edit_done();
    }

    /// How the part goes on from its key at this frame to the next.
    pub(crate) fn rig_pins_set_ease(&mut self, curve: Curve) {
        let Some(s) = self.layer_state.rig_pins.as_ref() else {
            return;
        };
        let (slot, ai) = (s.slot, s.attachment);
        let t = seconds(self.canvas.time, self.canvas.timeline.fps);
        if self.rig_pins_key_here().is_none_or(|c| c == curve) {
            return;
        }
        self.rig_edit(|rig| {
            let Some(name) = rig.slots[slot].attachments.get(ai).map(|a| a.name.clone()) else {
                return;
            };
            if let Some(anim) = rig.animation.and_then(|a| rig.animations.get_mut(a))
                && let Some(track) = anim
                    .deforms
                    .iter_mut()
                    .find(|t| t.slot == slot && t.attachment == name)
                && let Some(k) = track.keys.iter_mut().find(|k| (k.time - t).abs() < 1e-4)
            {
                k.curve = curve;
            }
        });
        self.rig_edit_done();
    }

    /// Take the selected pins (or all) out: the part keeps its shape.
    pub(crate) fn rig_pins_remove(&mut self, all: bool) -> bool {
        let Some(s) = self.layer_state.rig_pins.as_mut() else {
            return false;
        };
        let which: Vec<usize> = if all {
            (0..s.puppet.pins().len()).collect()
        } else {
            std::mem::take(&mut s.selected)
        };
        if which.is_empty() {
            return false;
        }
        s.puppet.remove_pins(&which);
        s.selected.clear();
        s.drag = None;
        s.pins_changed = true;
        self.rig_pins_flush();
        true
    }

    /// The selected pin's turn fixed (or left to the bend).
    pub(crate) fn rig_pins_set_turn(&mut self, turn: Option<f32>) {
        let Some(s) = self.layer_state.rig_pins.as_mut() else {
            return;
        };
        let [pin] = s.selected[..] else {
            return;
        };
        s.puppet.edit_pin(pin, |p| p.turn = turn);
        (s.moved, s.pending, s.settle) = (true, true, true);
    }

    pub(crate) fn rig_pins_selected_turn(&self) -> Option<(f32, bool)> {
        let s = self.layer_state.rig_pins.as_ref()?;
        let [pin] = s.selected[..] else {
            return None;
        };
        let fixed = s.puppet.pins().get(pin)?.turn.is_some();
        Some((s.puppet.turn_now(pin), fixed))
    }

    pub(crate) fn rig_pins_state(&self) -> (bool, bool) {
        self.layer_state
            .rig_pins
            .as_ref()
            .map_or((false, false), |s| {
                (!s.selected.is_empty(), !s.puppet.pins().is_empty())
            })
    }

    /// The pins and mesh of the part being pinned, for the overlay.
    pub(crate) fn rig_pins_overlay(&self) -> Option<PuppetOverlay<'_>> {
        let s = self.layer_state.rig_pins.as_ref()?;
        Some(PuppetOverlay {
            mesh: self.workspace.puppet.show_mesh.then_some(&s.puppet.mesh),
            pos: &s.puppet.pos,
            pins: s.puppet.pins().iter().map(|p| (p.at, p.turn)).collect(),
            selected: &s.selected,
        })
    }

    /// End what's going on (picking another tool, Enter): the last bend
    /// kept.
    pub(crate) fn rig_pins_finish(&mut self) {
        if let Some(s) = self.layer_state.rig_pins.as_mut() {
            if s.drag.take().is_some() || s.pending {
                (s.pending, s.settle) = (true, true);
            }
        } else {
            return;
        }
        self.rig_pins_flush();
        self.layer_state.rig_pins = None;
    }
}

/// Into `rig`'s animation (made if it has none, longer if `time` is past
/// it): the part's pinned points, and (`moves`: each point's move on the
/// canvas from where its bones put it) its deform key at frame `time`.
fn write_key(
    rig: &mut Rig,
    slot: usize,
    ai: usize,
    time: u32,
    timeline: &crate::canvas::animation::Timeline,
    pins: &[u32],
    moves: Option<&Vec<[f32; 2]>>,
) {
    let Some(name) = rig
        .slots
        .get(slot)
        .and_then(|s| s.attachments.get(ai))
        .map(|a| a.name.clone())
    else {
        return;
    };
    if rig.animation.is_none_or(|a| a >= rig.animations.len()) {
        rig.animations
            .push(crate::app::rig::timeline_animation("Animation", timeline));
        rig.animation = Some(rig.animations.len() - 1);
    }
    let fps = timeline.fps.max(1) as f32;
    let t = seconds(time, timeline.fps);
    let a = rig.animation.unwrap_or(0);
    // Past its end it would loop back to its start: made longer.
    if rig.animations[a].duration <= t + 1e-4 {
        rig.animations[a].duration = t + 1.0 / fps;
    }
    let offsets = moves.map(|moves| {
        let world = rig.pose(t).world;
        let back = rig.to_canvas().inverse().unwrap_or_default();
        let back = crate::canvas::rig::Affine {
            t: [0.0, 0.0],
            ..back
        };
        let moves: Vec<[f32; 2]> = moves.iter().map(|&m| back.apply(m)).collect();
        let s = &rig.slots[slot];
        match &s.attachments[ai].shape {
            Shape::Mesh { vertices, .. } => {
                crate::canvas::rig::skin::moves_to_offsets(&world, vertices, s.bone, &moves)
            }
            Shape::Region { .. } => Vec::new(),
        }
    });
    let anim = &mut rig.animations[a];
    let k = match anim
        .deforms
        .iter()
        .position(|d| d.slot == slot && d.attachment == name)
    {
        Some(k) => k,
        None => {
            anim.deforms.push(DeformTrack {
                slot,
                attachment: name,
                ..Default::default()
            });
            anim.deforms.len() - 1
        }
    };
    let track = &mut anim.deforms[k];
    track.pins = pins.to_vec();
    if let Some(offsets) = offsets {
        set_key(&mut track.keys, t, offsets);
    }
}

#[cfg(test)]
mod tests {
    use crate::PainterApp;
    use crate::app::tools::Tool;
    use crate::canvas::Canvas;
    use crate::canvas::rig::Curve;
    use eframe::egui::{Color32, Vec2};

    /// An app with a red bar (x 24..104, y 24..40) on a layer over white.
    fn app_with_bar() -> PainterApp {
        let canvas = Canvas::new(128, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        let tiles =
            crate::app::import::pixels_to_tiles(64, (24, 24), (80, 16), |_, _| Color32::RED);
        app.add_layer_with_tiles("bar".into(), tiles, |_| {});
        app
    }

    /// The middle (y) of the red at column `x`, at frame `t`.
    fn red_middle(app: &mut PainterApp, t: u32, x: usize) -> f32 {
        app.go_to_frame(t);
        let img = app.canvas.flatten_final();
        let ys: Vec<usize> = (0..64)
            .filter(|&y| {
                let c = img.pixels[y * 128 + x];
                c.r() > 200 && c.g() < 60
            })
            .collect();
        assert!(!ys.is_empty(), "no red at frame {t}");
        ys.iter().sum::<usize>() as f32 / ys.len() as f32
    }

    fn drag(app: &mut PainterApp, from: [f32; 2], to: [f32; 2]) {
        app.puppet_press(Vec2::new(from[0], from[1]), false, false);
        for k in 1..=4 {
            let u = k as f32 / 4.0;
            app.puppet_drag(Vec2::new(
                from[0] + (to[0] - from[0]) * u,
                from[1] + (to[1] - from[1]) * u,
            ));
            app.puppet_flush();
        }
        app.puppet_release();
    }

    fn keys(app: &PainterApp) -> Vec<f32> {
        let rig = app.active_rig().unwrap();
        let anim = &rig.animations[rig.animation.unwrap()];
        anim.deforms
            .iter()
            .flat_map(|d| d.keys.iter().map(|k| k.time))
            .collect()
    }

    #[test]
    fn a_layer_becomes_a_rig_that_looks_the_same() {
        let mut app = app_with_bar();
        let before = app.canvas.flatten_final();
        let source = app.canvas.active_layer_idx;
        app.make_rig_from_layers();
        let rig_layer = app.canvas.active_layer_idx;
        assert_ne!(rig_layer, source);
        assert!(app.canvas.layers[rig_layer].rig.is_some());
        assert!(!app.canvas.layers[source].visible, "the source is hidden");
        let after = app.canvas.flatten_final();
        let worst = (before.pixels.iter().zip(&after.pixels))
            .map(|(a, b)| {
                (0..4)
                    .map(|k| (a.to_array()[k] as i32 - b.to_array()[k] as i32).abs())
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst <= 2, "the rig looks like the layer ({worst})");
        // One undo step takes it all back.
        app.apply_history(false);
        assert!(app.canvas.layers.iter().all(|l| l.rig.is_none()));
        assert!(app.canvas.layers[source].visible);
    }

    #[test]
    fn pins_moved_at_two_frames_make_keys_and_the_frames_between() {
        let mut app = app_with_bar();
        app.make_rig_from_layers();
        app.active_tool = Tool::Puppet;
        let rest = red_middle(&mut app, 0, 98);
        assert!((rest - 31.5).abs() < 0.6, "{rest}");
        // Frame 1: the left end held, the right end up.
        app.go_to_frame(0);
        app.puppet_press(Vec2::new(28.0, 32.0), false, false);
        app.puppet_release();
        assert!(keys(&app).is_empty(), "a pin alone is no key");
        drag(&mut app, [100.0, 32.0], [100.0, 22.0]);
        assert_eq!(keys(&app), vec![0.0]);
        // Frame 12: down.
        app.go_to_frame(11);
        app.puppet_flush();
        assert_eq!(
            app.layer_state
                .rig_pins
                .as_ref()
                .unwrap()
                .puppet
                .pins()
                .len(),
            2,
            "the pins show again"
        );
        let pin_at = red_middle(&mut app, 11, 100);
        drag(&mut app, [100.0, pin_at], [100.0, pin_at + 20.0]);
        assert_eq!(keys(&app).len(), 2);
        let (a, mid, b) = (
            red_middle(&mut app, 0, 98),
            red_middle(&mut app, 5, 98),
            red_middle(&mut app, 11, 98),
        );
        assert!(a < rest - 6.0, "up at frame 1: {a}");
        assert!(b > rest + 6.0, "down at frame 12: {b}");
        assert!(mid > a + 3.0 && mid < b - 3.0, "between: {a} {mid} {b}");
        // The left end held still throughout.
        assert!((red_middle(&mut app, 5, 30) - rest).abs() < 1.5);
        // Eased from the first key: slower out of it.
        let linear = red_middle(&mut app, 2, 98);
        app.go_to_frame(0);
        app.puppet_flush();
        app.rig_pins_set_ease(Curve::Bezier([0.42, 0.0, 0.58, 1.0]));
        let eased = red_middle(&mut app, 2, 98);
        assert!(eased < linear - 0.5, "eased {eased} vs linear {linear}");
        // Undo takes the ease back, then the frame-12 key.
        app.apply_history(false);
        app.apply_history(false);
        assert_eq!(keys(&app).len(), 1);
        app.apply_history(true);
        assert_eq!(keys(&app).len(), 2);
        // Saved and opened again: the keys and the pins.
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let rig = loaded
            .canvas
            .layers
            .iter()
            .find_map(|l| l.rig.as_deref())
            .unwrap();
        let track = &rig.animations[rig.animation.unwrap()].deforms[0];
        assert_eq!((track.keys.len(), track.pins.len()), (2, 2));
        // Exported frames differ.
        let frames: Vec<_> = [0, 5, 11]
            .iter()
            .map(|&t| {
                app.go_to_frame(t);
                app.canvas.flatten_final().pixels
            })
            .collect();
        assert!(frames[0] != frames[1] && frames[1] != frames[2]);
        // Onion skins: at frame 5, the part as frame 0 has it (five
        // before), faintly, where frame 5's isn't; none in export.
        let pixel = |app: &PainterApp, x: usize, y: usize| {
            let mut img = eframe::egui::ColorImage::new([1, 1], Color32::TRANSPARENT);
            app.canvas
                .write_region_to_color_image(x, y, 1, 1, &mut img, 1);
            img.pixels[0]
        };
        let (y0, y5) = (red_middle(&mut app, 0, 100), red_middle(&mut app, 5, 100));
        assert!(y5 - y0 > 4.0, "{y0} {y5}");
        let probe = (y0 - 6.0).round() as usize;
        {
            let onion = &mut app.canvas_mut().onion;
            (onion.enabled, onion.before, onion.after) = (true, 5, 0);
        }
        app.go_to_frame(5);
        app.canvas.render_rig_onions();
        assert_eq!(pixel(&app, 100, probe - 12), Color32::WHITE);
        let exported = app.canvas.detached_copy().flatten_final();
        assert_eq!(
            exported.pixels[probe * 128 + 100],
            Color32::WHITE,
            "not exported"
        );
        let ghost = pixel(&app, 100, probe);
        assert!(
            ghost != Color32::WHITE,
            "an onion skin at {probe}: {ghost:?}"
        );
        app.canvas_mut().onion.enabled = false;
        app.canvas.render_rig_onions();
        assert_eq!(pixel(&app, 100, probe), Color32::WHITE);
    }
}
