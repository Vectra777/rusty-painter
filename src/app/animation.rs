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
}

impl PainterApp {
    /// Go to frame `time`: what shows changes, the selected drawing follows.
    pub(crate) fn go_to_frame(&mut self, time: u32) {
        if time == self.canvas.time {
            return;
        }
        self.release_canvas();
        if self.canvas_mut().set_time(time) {
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
        let state = &mut self.workspace.animation;
        if !state.playing {
            state.last_tick = None;
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
            self.go_to_frame(t);
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
    pub(crate) fn animate_active_layer(&mut self) {
        let i = self.canvas.active_layer_idx;
        self.document_step(move |canvas| canvas.animate_layer(i).is_some());
        self.workspace.animation.show_timeline = true;
    }

    /// A new drawing of the selected animated layer at the frame showing:
    /// blank, or a copy of the one showing.
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

    /// Change the document's structure with `change` (which says whether
    /// it changed anything) as one undo step: the whole document before it
    /// comes back on undo (its layers share their tiles, which this
    /// doesn't change).
    fn document_step(&mut self, change: impl FnOnce(&mut crate::canvas::Canvas) -> bool) {
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
        self.after_document_swap();
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
