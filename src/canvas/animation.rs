//! Frame-by-frame animation: a document has a timeline (its frame rate and
//! the frames it plays) and a current frame. An animated layer is a folder
//! marked [`Anim::Track`] whose children are its drawings, each marked
//! [`Anim::Frame`] with the frame it starts on; a drawing shows from its
//! frame until the next one's (it's held), and nothing shows before the
//! first. Only the drawing showing is composited (with, on screen, onion
//! skins of the ones around it).
//!
//! Because each drawing is a layer entry of its own, painting, undo,
//! saving and the rest work on drawings as on any layer: undoing a stroke
//! made on frame 12 puts frame 12's drawing back whatever frame is
//! showing.

use super::storage::{Anim, Canvas, LayerId, LayerKind};
use serde::{Deserialize, Serialize};

/// The frames a document plays and how fast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timeline {
    /// Frames a second.
    pub fps: u32,
    /// The first and last frame played (inclusive).
    pub start: u32,
    pub end: u32,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            fps: 12,
            start: 0,
            end: 23,
        }
    }
}

impl Timeline {
    /// How many frames are played.
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start) + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// The frame after `t`, back to the start past the end.
    pub fn next(&self, t: u32) -> u32 {
        if t >= self.end || t < self.start {
            self.start
        } else {
            t + 1
        }
    }

    /// The frame before `t`, round to the end before the start.
    pub fn previous(&self, t: u32) -> u32 {
        if t <= self.start || t > self.end {
            self.end
        } else {
            t - 1
        }
    }
}

/// Drawings around the current one shown faintly (on screen only): those
/// before in red, those after in green.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnionSkin {
    pub enabled: bool,
    /// How many drawings before and after.
    pub before: u32,
    pub after: u32,
    /// The nearest one's opacity (further ones fade).
    pub opacity: f32,
}

impl Default for OnionSkin {
    fn default() -> Self {
        Self {
            enabled: false,
            before: 1,
            after: 1,
            opacity: 0.35,
        }
    }
}

/// An onion skin's colour: drawings before the current one red, after it
/// green (linear light).
pub const ONION_BEFORE: [f32; 3] = [0.8, 0.05, 0.05];
pub const ONION_AFTER: [f32; 3] = [0.05, 0.6, 0.1];

impl Canvas {
    /// Whether any layer is animated.
    pub fn is_animated(&self) -> bool {
        self.layers.iter().any(|l| l.anim == Some(Anim::Track))
    }

    /// The animated layers' indices.
    pub fn tracks(&self) -> Vec<usize> {
        (0..self.layers.len())
            .filter(|&i| self.layers[i].anim == Some(Anim::Track))
            .collect()
    }

    /// An animated layer's drawings: (start frame, layer index), in frame
    /// order.
    pub fn frames_of(&self, track: LayerId) -> Vec<(u32, usize)> {
        let mut frames: Vec<(u32, usize)> = (self.layers.iter().enumerate())
            .filter(|(_, l)| l.parent == Some(track))
            .filter_map(|(i, l)| match l.anim {
                Some(Anim::Frame(at)) => Some((at, i)),
                _ => None,
            })
            .collect();
        frames.sort_unstable();
        frames
    }

    /// Which of `track`'s drawings shows at frame `time`: the last one
    /// starting at or before it.
    pub fn frame_at(&self, track: LayerId, time: u32) -> Option<usize> {
        let frames = self.frames_of(track);
        frames
            .iter()
            .rev()
            .find(|(at, _)| *at <= time)
            .map(|&(_, i)| i)
    }

    /// The drawings around the one showing on `track` at `time`, for onion
    /// skins: (layer index, how far: -1 the one before, 1 the one after...).
    pub fn onion_frames(&self, track: LayerId, time: u32) -> Vec<(usize, i32)> {
        let frames = self.frames_of(track);
        let Some(pos) = frames.iter().rposition(|(at, _)| *at <= time) else {
            // Before the first drawing: only those after.
            return (frames.iter().take(self.onion.after as usize))
                .enumerate()
                .map(|(k, &(_, i))| (i, k as i32 + 1))
                .collect();
        };
        let mut out = Vec::new();
        for k in 1..=self.onion.before as usize {
            if let Some(&(_, i)) = pos.checked_sub(k).and_then(|p| frames.get(p)) {
                out.push((i, -(k as i32)));
            }
        }
        for k in 1..=self.onion.after as usize {
            if let Some(&(_, i)) = frames.get(pos + k) {
                out.push((i, k as i32));
            }
        }
        out
    }

    /// Whether layer `i` shows at the current frame as far as animation
    /// goes: a drawing only if it's its layer's current one.
    pub fn shown_in_time(&self, i: usize) -> bool {
        let layer = &self.layers[i];
        match (layer.anim, layer.parent) {
            (Some(Anim::Frame(_)), Some(track)) => self.frame_at(track, self.time) == Some(i),
            _ => true,
        }
    }

    /// Go to frame `time`. The selected layer follows: on a drawing of an
    /// animated layer, the drawing showing there (if any). Returns whether
    /// anything shown changed.
    pub fn set_time(&mut self, time: u32) -> bool {
        if time == self.time {
            return false;
        }
        let before: Vec<Option<usize>> = (self.tracks().into_iter())
            .map(|t| self.frame_at(self.layers[t].id, self.time))
            .collect();
        let active_track = self
            .layers
            .get(self.active_layer_idx)
            .and_then(|l| match l.anim {
                Some(Anim::Frame(_)) => l.parent,
                _ => None,
            });
        self.time = time;
        if let Some(track) = active_track
            && let Some(i) = self.frame_at(track, time)
        {
            self.active_layer_idx = i;
        }
        let after: Vec<Option<usize>> = (self.tracks().into_iter())
            .map(|t| self.frame_at(self.layers[t].id, time))
            .collect();
        before != after || self.onion.enabled
    }

    /// Make paint layer `i` animated: a new animated layer in its place,
    /// holding it as its first drawing (at the current frame). Returns the
    /// animated layer's id.
    pub fn animate_layer(&mut self, i: usize) -> Option<LayerId> {
        let layer = self.layers.get(i)?;
        if layer.kind != LayerKind::Paint || i == 0 || layer.anim.is_some() {
            return None;
        }
        let (name, parent) = (layer.name.clone(), layer.parent);
        let track = self.insert_new_layer(i + 1, name, LayerKind::Group, parent);
        let t = self.layer_index_of(track)?;
        self.layers[t].anim = Some(Anim::Track);
        self.layers[t].expanded = false;
        self.layers[i].parent = Some(track);
        self.layers[i].anim = Some(Anim::Frame(self.time));
        self.layers[i].name = format!("Frame {}", self.time);
        self.active_layer_idx = i;
        Some(track)
    }

    /// A new drawing of `track` at frame `at` (replacing none: `None` if
    /// one starts there already), blank or a copy of the one showing
    /// there. Returns its index.
    pub fn add_frame(&mut self, track: LayerId, at: u32, copy: bool) -> Option<usize> {
        if self.frames_of(track).iter().any(|(t, _)| *t == at) {
            return None;
        }
        let t = self.layer_index_of(track)?;
        let source = copy.then(|| self.frame_at(track, at)).flatten();
        // Drawings sit under their layer's entry, in frame order.
        let id = self.insert_new_layer(t, format!("Frame {at}"), LayerKind::Paint, Some(track));
        let i = self.layer_index_of(id)?;
        self.layers[i].anim = Some(Anim::Frame(at));
        if let Some(src) = source {
            // (The source moved up one: the new entry went in below it.)
            let src = if src >= i { src + 1 } else { src };
            for (tx, ty) in self.layer_tile_keys(src) {
                if let Some(data) = self.get_layer_tile_data(src, tx, ty) {
                    match self.get_layer_tile_deep(src, tx, ty) {
                        Some(deep) => self.set_layer_tile_deep(i, tx, ty, &deep),
                        None => self.set_layer_tile_data(i, tx, ty, data),
                    }
                }
            }
        }
        Some(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, ColorImage};

    fn one_pixel(canvas: &Canvas) -> Color32 {
        let mut img = ColorImage::new([1, 1], Color32::TRANSPARENT);
        canvas.write_region_to_color_image(5, 5, 1, 1, &mut img, 1);
        img.pixels[0]
    }

    /// A canvas whose layer 1 is animated: red at frame 0, blue at frame 4.
    fn animated() -> (Canvas, LayerId) {
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let track = canvas.animate_layer(1).unwrap();
        let blue = canvas.add_frame(track, 4, false).unwrap();
        canvas.set_layer_tile_data(blue, 0, 0, vec![Color32::BLUE; 64 * 64]);
        (canvas, track)
    }

    #[test]
    fn a_drawing_shows_until_the_next_one() {
        let (mut canvas, track) = animated();
        assert!(canvas.is_animated());
        assert_eq!(
            canvas
                .frames_of(track)
                .iter()
                .map(|f| f.0)
                .collect::<Vec<_>>(),
            [0, 4]
        );
        for (time, want) in [
            (0, Color32::RED),
            (3, Color32::RED),
            (4, Color32::BLUE),
            (9, Color32::BLUE),
        ] {
            canvas.set_time(time);
            assert_eq!(one_pixel(&canvas), want, "frame {time}");
        }
        // The flattened picture is the current frame's too.
        canvas.set_time(2);
        assert_eq!(canvas.flatten_final().pixels[0], Color32::RED);
    }

    #[test]
    fn the_selected_drawing_follows_the_frame_and_copies_copy() {
        let (mut canvas, track) = animated();
        canvas.set_time(0);
        let red = canvas.frame_at(track, 0).unwrap();
        canvas.active_layer_idx = red;
        canvas.set_time(5);
        let blue = canvas.frame_at(track, 5).unwrap();
        assert_eq!(canvas.active_layer_idx, blue);
        // A copy of the drawing showing at frame 2 (the red one).
        let copy = canvas.add_frame(track, 2, true).unwrap();
        assert_eq!(
            canvas.get_layer_tile_data(copy, 0, 0).unwrap()[0],
            Color32::RED
        );
        assert!(
            canvas.add_frame(track, 2, false).is_none(),
            "one drawing a frame"
        );
        assert_eq!(canvas.frame_at(track, 3), Some(copy));
    }

    #[test]
    fn onion_skins_find_the_drawings_around() {
        let (mut canvas, track) = animated();
        let extra = canvas.add_frame(track, 8, false).unwrap();
        canvas.onion = OnionSkin {
            enabled: true,
            before: 1,
            after: 2,
            ..Default::default()
        };
        let frames = canvas.frames_of(track);
        let around = canvas.onion_frames(track, 5);
        assert_eq!(around, [(frames[0].1, -1), (extra, 1)]);
        assert_eq!(
            canvas.onion_frames(track, 0),
            [(frames[1].1, 1), (extra, 2)]
        );
    }

    #[test]
    fn the_timeline_loops() {
        let t = Timeline {
            fps: 24,
            start: 2,
            end: 5,
        };
        assert_eq!((t.next(5), t.next(3), t.previous(2), t.len()), (2, 4, 5, 4));
    }
}
