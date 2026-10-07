//! Animation in the app: the frame showing, playback, adding and removing
//! drawings (one undo step each) and exporting the animation as a video or
//! pictures. The model is in [`crate::canvas::animation`].

use crate::PainterApp;
use crate::canvas::history::{LayerHistoryOp, UndoAction};
use crate::canvas::storage::Anim;
use crate::project::video::{VideoFormat, VideoFrame};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct AnimationState {
    /// The timeline panel is open.
    pub show_timeline: bool,
    pub playing: bool,
    /// When the frame showing last moved during playback, and the share of
    /// a frame already gone since.
    last_tick: Option<Instant>,
    carry: f32,
    /// An export running.
    pub task: Option<std::thread::JoinHandle<Result<String, String>>>,
    /// The last export's format.
    pub format: Option<VideoFormat>,
    /// The timeline panel's zoom, scroll and the edit under way in it.
    pub view: crate::ui::timeline::TimelineView,
    /// Frames copied in the timeline.
    pub clipboard: Option<FramesClipboard>,
    /// Frames made ahead, to play from memory.
    pub cache: crate::app::playback::PlaybackCache,
    /// Changes to keys and rigs so far (a gesture's are one undo step, but
    /// each changes what plays).
    pub edits: u64,
    /// The undo steps there had been when every moved layer's shown copy
    /// was last made: going to a frame keeps a copy whose pose is the same
    /// only while no step changed pixels since.
    posed_at: u64,
}

/// Frames picked in the timeline: on these layers' rows (top first), from
/// frame `from` to `to`.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameSelection {
    pub rows: Vec<crate::canvas::storage::LayerId>,
    pub from: u32,
    pub to: u32,
}

impl FrameSelection {
    pub fn len(&self) -> u32 {
        self.to - self.from + 1
    }

    pub fn contains(&self, row: crate::canvas::storage::LayerId, t: u32) -> bool {
        self.rows.contains(&row) && (self.from..=self.to).contains(&t)
    }
}

/// Copied frames: a stretch of each row's, top first.
#[derive(Clone)]
pub struct FramesClipboard {
    pub rows: Vec<crate::canvas::animation::FramesClip>,
    pub len: u32,
}

impl PainterApp {
    /// Go to frame `time`: what shows changes, the selected drawing follows.
    pub(crate) fn go_to_frame(&mut self, time: u32) {
        self.leave_cached_frame();
        if time == self.canvas.time {
            return;
        }
        self.release_canvas();
        let mut changed = self.canvas_mut().set_time(time);
        let pushed = self.layer_state.history.push_count();
        if pushed != self.workspace.animation.posed_at {
            self.workspace.animation.posed_at = pushed;
            changed |= self.canvas.pose_motions();
        }
        if changed {
            self.mark_all_tiles_dirty();
            self.layer_state.thumbnails_dirty = true;
        }
    }

    /// Once a frame: playback moves on at the timeline's rate; a finished
    /// export reports.
    pub(crate) fn animation_tick(&mut self, ctx: &eframe::egui::Context) {
        if let Some(task) = self.workspace.animation.task.take_if(|t| t.is_finished()) {
            let result = task
                .join()
                .unwrap_or_else(|_| Err("The export stopped".into()));
            self.export_state.message = Some(result.unwrap_or_else(|e| e));
        }
        self.playback_cache_tick(ctx);
        let state = &mut self.workspace.animation;
        if !state.playing {
            state.last_tick = None;
            self.leave_cached_frame();
            return;
        }
        let now = Instant::now();
        let elapsed = state.last_tick.map_or(0.0, |t| (now - t).as_secs_f32());
        state.last_tick = Some(now);
        let timeline = self.canvas.timeline;
        state.carry += elapsed * timeline.fps.max(1) as f32;
        let steps = state.carry.floor() as u32;
        state.carry -= steps as f32;
        if steps > 0 {
            let mut t = self.canvas.time;
            // (A slow frame skips frames rather than slowing the animation.)
            for _ in 0..steps.min(timeline.len()) {
                t = timeline.next(t);
            }
            // From memory when it's there.
            if !self.go_to_cached_frame(t) {
                self.go_to_frame(t);
            }
        }
        ctx.request_repaint();
    }

    /// The animated layer the selected layer belongs to (it, or one of its
    /// drawings).
    pub(crate) fn active_track(&self) -> Option<crate::canvas::storage::LayerId> {
        let layer = self.canvas.layers.get(self.canvas.active_layer_idx)?;
        match layer.anim {
            Some(Anim::Track) => Some(layer.id),
            Some(Anim::Frame(_)) => layer.parent,
            None => None,
        }
    }

    /// Make the selected paint layer animated: its picture becomes the
    /// first drawing, at the frame showing.
    #[cfg(test)]
    pub(crate) fn animate_active_layer(&mut self) {
        let i = self.canvas.active_layer_idx;
        self.document_step(move |canvas| canvas.animate_layer(i).is_some());
        self.workspace.animation.show_timeline = true;
    }

    /// A new drawing of the selected animated layer at the frame showing:
    /// blank, or a copy of the one showing.
    #[cfg(test)]
    pub(crate) fn add_drawing(&mut self, copy: bool) {
        let Some(track) = self.active_track() else {
            return;
        };
        let time = self.canvas.time;
        self.document_step(move |canvas| match canvas.add_frame(track, time, copy) {
            Some(i) => {
                canvas.active_layer_idx = i;
                true
            }
            None => false,
        });
    }

    /// Take away the selected animated layer's drawing that starts at the
    /// frame showing.
    pub(crate) fn remove_drawing(&mut self) {
        let Some(track) = self.active_track() else {
            return;
        };
        let time = self.canvas.time;
        self.document_step(move |canvas| {
            let Some(&(_, i)) = canvas.frames_of(track).iter().find(|(at, _)| *at == time) else {
                return false;
            };
            canvas.layers.remove(i);
            let next = canvas
                .frame_at(track, time)
                .or_else(|| canvas.layer_index_of(track))
                .unwrap_or(0);
            canvas.active_layer_idx = next;
            true
        });
    }

    /// Move the drawing starting at the frame showing to `to` (if no other
    /// starts there).
    pub(crate) fn move_drawing(&mut self, to: u32) {
        let Some(track) = self.active_track() else {
            return;
        };
        let time = self.canvas.time;
        self.document_step(move |canvas| {
            let frames = canvas.frames_of(track);
            if frames.iter().any(|(at, _)| *at == to) {
                return false;
            }
            let Some(&(_, i)) = frames.iter().find(|(at, _)| *at == time) else {
                return false;
            };
            canvas.layers[i].anim = Some(Anim::Frame(to));
            canvas.layers[i].name = format!("Frame {to}");
            true
        });
        self.go_to_frame(to);
    }

    /// Copy the frames `sel` picks.
    pub(crate) fn copy_frames(&mut self, sel: &FrameSelection) {
        let rows = (sel.rows.iter())
            .map(|&id| self.canvas.copy_frames(id, sel.from, sel.to))
            .collect();
        self.workspace.animation.clipboard = Some(FramesClipboard {
            rows,
            len: sel.len(),
        });
    }

    /// The frames `sel` picks show nothing and have no keys (one undo step).
    pub(crate) fn delete_frames(&mut self, sel: &FrameSelection) {
        let sel = sel.clone();
        self.document_step(move |canvas| {
            let mut any = false;
            for &id in &sel.rows {
                any |= canvas.clear_frames(id, sel.from, sel.to);
            }
            any
        });
    }

    pub(crate) fn cut_frames(&mut self, sel: &FrameSelection) {
        self.copy_frames(sel);
        self.delete_frames(sel);
    }

    /// The copied frames onto `rows` (a copied row each, top first) from
    /// frame `t`, over what was there (one undo step). A plain layer that
    /// gets drawings becomes animated.
    pub(crate) fn paste_frames(&mut self, rows: &[crate::canvas::storage::LayerId], t: u32) {
        let Some(clip) = self.workspace.animation.clipboard.clone() else {
            return;
        };
        let rows = rows.to_vec();
        let start = self.canvas.timeline.start;
        self.document_step(move |canvas| {
            let mut any = false;
            for (&id, row) in rows.iter().zip(&clip.rows) {
                let Some(mut i) = canvas.layer_index_of(id) else {
                    continue;
                };
                let mut id = id;
                // Drawings onto a plain layer: it's animated first.
                if row.animated
                    && canvas.layers[i].anim.is_none()
                    && canvas.layers[i].kind == crate::canvas::storage::LayerKind::Paint
                    && i != 0
                    && let Some(track) = crate::app::motion::animate_at(canvas, i, start.min(t))
                {
                    id = track;
                    i = canvas.layer_index_of(track).unwrap_or(i);
                }
                if row.animated && canvas.layers[i].anim != Some(Anim::Track) && row.keys.is_empty()
                {
                    continue;
                }
                canvas.paste_frames(id, t, row, clip.len);
                any = true;
            }
            any
        });
    }

    /// The frames `sel` picks moved `delta` frames along, over what was
    /// there (one undo step). Returns where they went.
    pub(crate) fn move_frames(
        &mut self,
        sel: &FrameSelection,
        delta: i64,
    ) -> Option<FrameSelection> {
        let from = sel.from as i64 + delta;
        if delta == 0 || from < 0 {
            return None;
        }
        let clips: Vec<_> = (sel.rows.iter())
            .map(|&id| self.canvas.copy_frames(id, sel.from, sel.to))
            .collect();
        let (moved, len) = (sel.clone(), sel.len());
        self.document_step(move |canvas| {
            for &id in &moved.rows {
                canvas.clear_frames(id, moved.from, moved.to);
            }
            for (&id, clip) in moved.rows.iter().zip(&clips) {
                canvas.paste_frames(id, from as u32, clip, len);
            }
            true
        });
        Some(FrameSelection {
            rows: sel.rows.clone(),
            from: from as u32,
            to: from as u32 + len - 1,
        })
    }

    /// Where the drawings and keys of the selected layer are (or of every
    /// animated layer, with none), for jumping between them.
    pub(crate) fn navigation_keys(&self) -> Vec<u32> {
        let mut keys: Vec<u32> = match self.active_track() {
            Some(track) => (self.canvas.frames_of(track).iter()).map(|f| f.0).collect(),
            None => (self.canvas.tracks().into_iter())
                .flat_map(|t| self.canvas.frames_of(self.canvas.layers[t].id))
                .map(|f| f.0)
                .collect(),
        };
        if let Some(motion) = (self.motion_target(self.canvas.active_layer_idx))
            .and_then(|t| self.canvas.layers[t].motion.as_ref())
        {
            keys.extend(motion.all_key_frames());
        }
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// The drawing showing on the selected animated layer held a frame
    /// longer (`delta` 1) or shorter (-1): the later drawings follow.
    pub(crate) fn change_hold(&mut self, delta: i32) {
        let Some(track) = self.active_track() else {
            return;
        };
        let time = self.canvas.time;
        let frames = self.canvas.frames_of(track);
        let start = frames.iter().rev().find(|f| f.0 <= time).map(|f| f.0);
        let next = frames.iter().find(|f| f.0 > time).map(|f| f.0);
        if let (Some(start), Some(next)) = (start, next)
            && (delta > 0 || next - start > 1)
        {
            self.shift_drawings(track, next, delta);
        }
    }

    /// An animation command from the keyboard.
    pub(crate) fn animation_shortcut(&mut self, action: crate::app::input::keymap::Action) {
        use crate::app::input::keymap::Action;
        let (time, timeline) = (self.canvas.time, self.canvas.timeline);
        let active = self.canvas.active_layer_idx;
        match action {
            Action::FirstFrame => self.go_to_frame(timeline.start),
            Action::LastFrame => self.go_to_frame(timeline.end),
            Action::PreviousDrawing => {
                if let Some(&k) = self.navigation_keys().iter().rev().find(|&&k| k < time) {
                    self.go_to_frame(k);
                }
            }
            Action::NextDrawing => {
                if let Some(&k) = self.navigation_keys().iter().find(|&&k| k > time) {
                    self.go_to_frame(k);
                }
            }
            Action::NewDrawing => self.drawing_at(active, time, false),
            Action::CopyDrawing => self.drawing_at(active, time, true),
            Action::RemoveDrawing => self.remove_drawing(),
            Action::HoldLonger => self.change_hold(1),
            Action::HoldShorter => self.change_hold(-1),
            Action::ToggleOnion => {
                let onion = &mut self.canvas_mut().onion;
                onion.enabled = !onion.enabled;
                self.canvas.pose_motions();
                self.mark_all_tiles_dirty();
            }
            Action::KeyMotion => {
                if let Some(target) = self.motion_target(active) {
                    self.key_all_here(target);
                }
            }
            Action::NewAnimationLayer => self.new_animation_layer(),
            _ => {}
        }
        // Making something to animate opens the timeline.
        if matches!(
            action,
            Action::NewDrawing
                | Action::CopyDrawing
                | Action::KeyMotion
                | Action::NewAnimationLayer
        ) {
            self.workspace.animation.show_timeline = true;
        }
    }

    /// Shift `track`'s drawings starting at or after frame `from` by
    /// `delta` frames, the later ones following: the drawing before `from`
    /// is held longer or shorter. Refused if one would land on or before
    /// that drawing, or before frame 0.
    pub(crate) fn shift_drawings(
        &mut self,
        track: crate::canvas::storage::LayerId,
        from: u32,
        delta: i32,
    ) {
        if delta == 0 {
            return;
        }
        self.document_step(move |canvas| {
            let frames = canvas.frames_of(track);
            let before = (frames.iter())
                .rev()
                .find(|(at, _)| *at < from)
                .map(|f| f.0);
            let moved: Vec<(u32, usize)> =
                frames.into_iter().filter(|(at, _)| *at >= from).collect();
            let Some(&(first, _)) = moved.first() else {
                return false;
            };
            let landed = first as i64 + delta as i64;
            if landed < 0 || before.is_some_and(|b| landed <= b as i64) {
                return false;
            }
            for (at, i) in moved {
                let to = (at as i64 + delta as i64) as u32;
                canvas.layers[i].anim = Some(Anim::Frame(to));
                canvas.layers[i].name = format!("Frame {to}");
            }
            true
        });
        self.mark_all_tiles_dirty();
    }

    /// Change the document's structure with `change` (which says whether
    /// it changed anything) as one undo step: the whole document before it
    /// comes back on undo (its layers share their tiles, which this
    /// doesn't change).
    pub(crate) fn document_step(
        &mut self,
        change: impl FnOnce(&mut crate::canvas::Canvas) -> bool,
    ) {
        self.release_canvas();
        let canvas = crate::app::stroke_ops::exclusive(&mut self.canvas);
        let before = crate::canvas::storage::DocumentState {
            width: canvas.width(),
            height: canvas.height(),
            layers: canvas.layers.iter().map(|l| l.share()).collect(),
            active_layer_idx: canvas.active_layer_idx,
            depth: canvas.depth(),
            profile: canvas.profile.clone(),
        };
        if !change(canvas) {
            return;
        }
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Document(Arc::new(std::sync::Mutex::new(
                before,
            )))),
        });
        self.after_layers_change();
    }

    /// Export the timeline's frames as `format` to `path`, on a worker
    /// thread.
    pub(crate) fn export_animation(&mut self, path: PathBuf, format: VideoFormat) {
        if self.workspace.animation.task.is_some() {
            self.export_state.message = Some("An animation export is already running".into());
            return;
        }
        self.bake_shader_layers();
        let canvas = self.canvas.detached_copy();
        self.workspace.animation.format = Some(format);
        self.export_state.message = Some("Exporting the animation…".into());
        self.workspace.animation.task = Some(std::thread::spawn(move || {
            let timeline = canvas.timeline;
            let frames = animation_frames(canvas);
            let to = crate::project::video::write_video(&path, format, timeline.fps, true, frames)?;
            Ok(format!("Animation saved to {}", to.display()))
        }));
    }
}

/// The timeline's frames of `canvas`, each flattened (unmultiplied RGBA).
pub(crate) fn animation_frames(
    mut canvas: crate::canvas::Canvas,
) -> impl Iterator<Item = VideoFrame> {
    let timeline = canvas.timeline;
    (timeline.start..=timeline.end).map(move |t| {
        canvas.set_time(t);
        let img = canvas.flatten_final();
        let rgba = (img.pixels.iter())
            .flat_map(|&p| crate::canvas::blend::unmultiply(p))
            .collect();
        VideoFrame {
            width: img.size[0],
            height: img.size[1],
            rgba,
        }
    })
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::storage::Anim;
    use eframe::egui::{Color32, Vec2};

    fn app() -> crate::PainterApp {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app
    }

    fn shown(app: &crate::PainterApp) -> Color32 {
        app.canvas.flatten_final().pixels[0]
    }

    #[test]
    fn drawings_are_added_moved_and_removed_with_undo() {
        let mut app = app();
        app.animate_active_layer();
        assert!(app.canvas.is_animated());
        let track = app.active_track().unwrap();
        app.go_to_frame(3);
        app.add_drawing(false);
        assert_eq!(app.canvas.frames_of(track).len(), 2);
        assert_eq!(shown(&app), Color32::WHITE, "a blank drawing at frame 3");
        // Painting on it, then going back: frame 0's red is there.
        let blank = app.canvas.active_layer_idx;
        app.canvas
            .set_layer_tile_data(blank, 0, 0, vec![Color32::BLUE; 64 * 64]);
        app.go_to_frame(1);
        assert_eq!(shown(&app), Color32::RED);
        app.go_to_frame(5);
        assert_eq!(shown(&app), Color32::BLUE);
        // Moved to frame 6: frame 5 shows red again.
        app.go_to_frame(3);
        app.move_drawing(6);
        app.go_to_frame(5);
        assert_eq!(shown(&app), Color32::RED);
        app.apply_history(false); // the move
        assert!(app.canvas.frames_of(track).iter().any(|(at, _)| *at == 3));
        // Removed: back to red everywhere, then undone.
        app.go_to_frame(3);
        app.remove_drawing();
        assert_eq!(app.canvas.frames_of(track).len(), 1);
        app.apply_history(false);
        assert_eq!(app.canvas.frames_of(track).len(), 2);
        app.go_to_frame(4);
        assert_eq!(shown(&app), Color32::BLUE);
    }

    #[test]
    fn holds_lengthen_and_shorten_with_the_later_drawings_following() {
        let mut app = app();
        app.animate_active_layer();
        let track = app.active_track().unwrap();
        for at in [2, 5] {
            app.go_to_frame(at);
            app.add_drawing(false);
        }
        let starts = |app: &crate::PainterApp| -> Vec<u32> {
            app.canvas.frames_of(track).iter().map(|f| f.0).collect()
        };
        app.shift_drawings(track, 2, 3);
        assert_eq!(starts(&app), [0, 5, 8]);
        app.shift_drawings(track, 8, -2);
        assert_eq!(starts(&app), [0, 5, 6]);
        // Not onto the drawing before.
        app.shift_drawings(track, 6, -1);
        assert_eq!(starts(&app), [0, 5, 6]);
        app.apply_history(false);
        assert_eq!(starts(&app), [0, 5, 8]);
    }

    #[test]
    fn frame_ranges_move_and_paste_across_layers_with_undo() {
        use super::FrameSelection;
        let mut app = app();
        app.animate_active_layer(); // red from 0
        let track = app.active_track().unwrap();
        app.go_to_frame(2);
        app.add_drawing(false);
        let blank = app.canvas.active_layer_idx;
        app.canvas
            .set_layer_tile_data(blank, 0, 0, vec![Color32::BLUE; 64 * 64]);
        let starts = |app: &crate::PainterApp, id| -> Vec<u32> {
            app.canvas.frames_of(id).iter().map(|f| f.0).collect()
        };
        // Frames 2..=3 (blue) moved 3 along, as cells move: empty where
        // they were, blue from 5, and still blue after them.
        let sel = FrameSelection {
            rows: vec![track],
            from: 2,
            to: 3,
        };
        let moved = app.move_frames(&sel, 3).unwrap();
        assert_eq!((moved.from, moved.to), (5, 6));
        for (t, want) in [
            (1, Color32::RED),
            (3, Color32::WHITE),
            (5, Color32::BLUE),
            (8, Color32::BLUE),
        ] {
            app.go_to_frame(t);
            assert_eq!(shown(&app), want, "frame {t}");
        }
        app.apply_history(false);
        assert_eq!(starts(&app, track), [0, 2], "one undo step");
        // Copied onto a new plain layer at 10: it becomes animated.
        app.copy_frames(&FrameSelection {
            rows: vec![track],
            from: 2,
            to: 2,
        });
        let plain = app.canvas_mut().insert_layer_for_tests();
        app.after_layers_change();
        app.paste_frames(&[plain], 10);
        let pasted = app.canvas.layer_index_of(plain).and_then(|i| {
            let l = &app.canvas.layers[i];
            l.parent.filter(|_| l.anim.is_some())
        });
        let pasted = pasted.expect("animated");
        assert!(starts(&app, pasted).contains(&10));
    }

    #[test]
    fn a_stroke_undoes_on_its_own_drawing_whatever_frame_shows() {
        let mut app = app();
        app.animate_active_layer();
        app.go_to_frame(4);
        app.add_drawing(true); // a copy of frame 0's red
        let drawing = app.canvas.active_layer_idx;
        assert!(matches!(
            app.canvas.layers[drawing].anim,
            Some(Anim::Frame(4))
        ));
        // A gradient over the copy at frame 4.
        app.brush_state.brush.brush_options.color = Color32::from_rgb(0, 200, 0);
        app.gradient_press(Vec2::new(0.0, 0.0));
        app.gradient_drag(Vec2::new(64.0, 0.0), false);
        app.gradient_commit();
        assert_ne!(shown(&app), Color32::RED);
        // Back at frame 0, undo takes the gradient off frame 4's drawing.
        app.go_to_frame(0);
        app.apply_history(false);
        app.go_to_frame(4);
        assert_eq!(shown(&app), Color32::RED);
    }

    #[test]
    fn playback_moves_on_and_the_export_holds_every_frame() {
        let mut app = app();
        app.animate_active_layer();
        app.canvas_mut().timeline = crate::canvas::animation::Timeline {
            fps: 10,
            start: 0,
            end: 3,
        };
        app.go_to_frame(2);
        app.add_drawing(false);
        let frames: Vec<_> = super::animation_frames(app.canvas.detached_copy()).collect();
        assert_eq!(frames.len(), 4);
        assert_eq!(&frames[0].rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &frames[2].rgba[..4],
            &[255, 255, 255, 255],
            "the blank drawing"
        );
        // A file of it.
        let path = std::env::temp_dir().join(format!("rp-anim-{}.gif", std::process::id()));
        crate::project::video::write_video(
            &path,
            crate::project::video::VideoFormat::Gif,
            10,
            true,
            frames.into_iter(),
        )
        .unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 50);
        let _ = std::fs::remove_file(path);
    }
}
