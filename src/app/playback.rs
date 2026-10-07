//! Playback from memory: the timeline's frames rendered in the background
//! (at about screen size) while the animation plays or the app is idle
//! with the timeline open, then shown from there, so big canvases play at
//! their frame rate instead of being composited frame by frame. Any change
//! to the document starts the frames again; the ruler shows which are
//! ready.

use crate::PainterApp;
use eframe::egui::{self, Color32, ColorImage};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// The longest side of a cached frame (pixels).
const MAX_SIDE: usize = 1600;
/// How many bytes of frames are kept at most.
const MAX_BYTES: usize = 1 << 30;
/// How long the app is left idle before frames are made ahead of play.
const IDLE: Duration = Duration::from_millis(1200);

#[derive(Default)]
pub struct PlaybackCache {
    /// What the frames were made from (the document, its timeline).
    key: u64,
    frames: HashMap<u32, egui::TextureHandle>,
    job: Option<Job>,
    /// The frame the canvas shows from here (its own tiles weren't drawn
    /// for it).
    pub showing: Option<u32>,
    last_activity: Option<Instant>,
}

/// Frames being made in the background.
struct Job {
    key: u64,
    cancel: Arc<AtomicBool>,
    frames: Receiver<(u32, ColorImage)>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl PlaybackCache {
    /// Whether frame `t` is ready.
    pub fn has(&self, t: u32) -> bool {
        self.frames.contains_key(&t)
    }

    fn clear(&mut self, retired: &mut Vec<egui::TextureHandle>) {
        self.job = None;
        retired.extend(self.frames.drain().map(|(_, t)| t));
    }
}

impl PainterApp {
    /// What the cached frames depend on: every undo step, layers shown or
    /// hidden, their opacity, the timeline.
    fn playback_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.doc_version().hash(&mut h);
        self.workspace.animation.edits.hash(&mut h);
        let c = &self.canvas;
        (c.width(), c.height(), c.timeline.start, c.timeline.end).hash(&mut h);
        for l in &c.layers {
            (l.id.0, l.visible, l.opacity.to_bits()).hash(&mut h);
        }
        h.finish()
    }

    /// Once a frame: the cache follows the document, takes in finished
    /// frames, and makes more while playing or idle.
    pub(crate) fn playback_cache_tick(&mut self, ctx: &egui::Context) {
        let animated = self.canvas.is_animated() || self.canvas.has_motion();
        let key = self.playback_key();
        let cache = &mut self.workspace.animation.cache;
        if cache.key != key {
            let retired = &mut self.workspace.retired_textures;
            cache.clear(retired);
            cache.key = key;
        }
        if ctx.input(|i| !i.events.is_empty() || i.pointer.any_down()) {
            cache.last_activity = Some(Instant::now());
        }
        // Finished frames, as textures.
        if let Some(job) = &cache.job {
            let mut done = false;
            loop {
                match job.frames.try_recv() {
                    Ok((t, image)) if job.key == key => {
                        let texture = ctx.load_texture(
                            format!("playback_{t}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        cache.frames.insert(t, texture);
                    }
                    Ok(_) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
            if done {
                cache.job = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        }
        let state = &self.workspace.animation;
        let idle = cache_idle(&state.cache);
        let wanted = animated && (state.playing || (state.show_timeline && idle));
        let timeline = self.canvas.timeline;
        let missing = (timeline.start..=timeline.end).any(|t| !state.cache.has(t));
        if wanted && missing && state.cache.job.is_none() {
            self.start_playback_job(key);
        } else if animated && state.show_timeline && missing && !idle {
            // Check again once idle.
            ctx.request_repaint_after(IDLE);
        }
    }

    /// Make the frames not cached yet, from the one showing on, in the
    /// background.
    fn start_playback_job(&mut self, key: u64) {
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let step = w.max(h).div_ceil(MAX_SIDE).max(1);
        let frame_bytes = w.div_ceil(step) * h.div_ceil(step) * 4;
        let timeline = self.canvas.timeline;
        let budget = (MAX_BYTES / frame_bytes.max(1)).max(1) as u32;
        let cache = &self.workspace.animation.cache;
        let now = self.canvas.time.clamp(timeline.start, timeline.end);
        let order: Vec<u32> = (now..=timeline.end)
            .chain(timeline.start..now)
            .filter(|t| !cache.has(*t))
            .take((budget as usize).saturating_sub(cache.frames.len()))
            .collect();
        if order.is_empty() {
            return;
        }
        let mut canvas = self.canvas.shared_copy();
        canvas.onion.enabled = false;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let stop = Arc::clone(&cancel);
        std::thread::spawn(move || {
            for t in order {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                canvas.set_time(t);
                let mut image = ColorImage::new([0, 0], Color32::TRANSPARENT);
                canvas.write_region_to_color_image(0, 0, w, h, &mut image, step);
                if tx.send((t, image)).is_err() {
                    return;
                }
            }
        });
        self.workspace.animation.cache.job = Some(Job {
            key,
            cancel,
            frames: rx,
        });
    }

    /// Go to frame `t` during playback from the cache when it's there
    /// (nothing composited). Returns whether it was.
    pub(crate) fn go_to_cached_frame(&mut self, t: u32) -> bool {
        if !self.workspace.animation.cache.has(t) {
            return false;
        }
        self.canvas_mut().time = t;
        self.workspace.animation.cache.showing = Some(t);
        true
    }

    /// The canvas shows its own frame again (after playing from the cache).
    pub(crate) fn leave_cached_frame(&mut self) {
        if self.workspace.animation.cache.showing.take().is_some() {
            self.canvas_mut().refresh_time();
            self.mark_all_tiles_dirty();
            self.layer_state.thumbnails_dirty = true;
        }
    }
}

fn cache_idle(cache: &PlaybackCache) -> bool {
    cache.last_activity.is_none_or(|t| t.elapsed() >= IDLE)
}

/// The cached frame showing, over the canvas (as the view turns and flips
/// it).
pub(crate) fn draw_cached_frame(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(egui::Vec2) -> egui::Pos2,
) {
    let cache = &app.workspace.animation.cache;
    let Some(texture) = cache.showing.and_then(|t| cache.frames.get(&t)) else {
        return;
    };
    let (w, h) = (app.canvas.width() as f32, app.canvas.height() as f32);
    let corners =
        [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(x, y)| to_screen(egui::vec2(x, y)));
    let uvs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    // Under it, what shows through a transparent canvas.
    let mut under = egui::Mesh::default();
    let mut mesh = egui::Mesh::with_texture(texture.id());
    for (c, (u, v)) in corners.iter().zip(uvs) {
        under.colored_vertex(*c, crate::ui::style::CHECKERBOARD_LIGHT);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *c,
            uv: egui::pos2(u, v),
            color: Color32::WHITE,
        });
    }
    for m in [&mut under, &mut mesh] {
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
    }
    painter.add(egui::Shape::mesh(under));
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::motion::Prop;
    use eframe::egui::{self, Color32};

    #[test]
    fn frames_are_made_in_the_background_and_played_from_memory() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.canvas_mut().timeline = crate::canvas::animation::Timeline {
            fps: 12,
            start: 0,
            end: 3,
        };
        app.motion_step(1, |m| m.set(Prop::Opacity, 3, [0.5, 0.0]));
        app.workspace.animation.playing = true;
        let ctx = egui::Context::default();
        for _ in 0..200 {
            let _ = ctx.run(Default::default(), |ctx| app.playback_cache_tick(ctx));
            if (0..=3).all(|t| app.workspace.animation.cache.has(t)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!((0..=3).all(|t| app.workspace.animation.cache.has(t)));
        assert!(app.go_to_cached_frame(2));
        assert_eq!(app.canvas.time, 2);
        app.leave_cached_frame();
        assert!(app.workspace.animation.cache.showing.is_none());
        // A change starts them again.
        app.motion_step(1, |m| m.set(Prop::Opacity, 0, [0.2, 0.0]));
        let _ = ctx.run(Default::default(), |ctx| app.playback_cache_tick(ctx));
        assert!(!app.workspace.animation.cache.has(2));
    }
}
