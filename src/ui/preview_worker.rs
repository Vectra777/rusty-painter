//! Brush stroke previews (the presets list, the radial palette, the brush
//! settings strip) drawn on a thread of their own: a heavy brush, or a
//! hundred freshly imported ones, never holds up a frame, and a brush that
//! panics only loses its preview. Until a preview is ready, there's none
//! (or the previous one stays).

use crate::brush_engine::brush::Brush;
use crate::brush_engine::preview::stroke_preview_image;
use eframe::egui;
use rayon::ThreadPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

/// What a preview looks like.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PreviewLook {
    /// Pixels.
    pub size: [usize; 2],
    pub diameter: f32,
    pub ink: egui::Color32,
}

struct Request {
    key: String,
    /// Which request for `key` this is: an older one's result is stale.
    generation: u64,
    brush: Brush,
    look: PreviewLook,
    pool: Arc<ThreadPool>,
    ctx: egui::Context,
}

struct Done {
    key: String,
    generation: u64,
    image: Option<egui::ColorImage>,
}

pub(crate) struct PreviewWorker {
    requests: Sender<Request>,
    done: Receiver<Done>,
    /// The latest request per key.
    generations: HashMap<String, u64>,
    next_generation: u64,
}

impl Default for PreviewWorker {
    fn default() -> Self {
        let (requests, queue) = mpsc::channel::<Request>();
        let (finished, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("brush-previews".into())
            .spawn(move || {
                while let Ok(first) = queue.recv() {
                    // Everything asked meanwhile, the latest per key (a
                    // slider dragged asks again and again).
                    let mut batch: Vec<Request> = vec![first];
                    for r in queue.try_iter() {
                        batch.retain(|b| b.key != r.key);
                        batch.push(r);
                    }
                    for r in batch {
                        let mut brush = r.brush;
                        let look = r.look;
                        let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            stroke_preview_image(
                                &mut brush,
                                &r.pool,
                                look.size,
                                64,
                                look.ink,
                                look.diameter,
                            )
                        }))
                        .inspect_err(|_| log::error!("A brush preview panicked: {}", r.key))
                        .ok();
                        let sent = finished.send(Done {
                            key: r.key,
                            generation: r.generation,
                            image,
                        });
                        if sent.is_err() {
                            return;
                        }
                        r.ctx.request_repaint();
                    }
                }
            });
        if let Err(err) = spawned {
            log::error!("Couldn't start the brush preview thread: {err}");
        }
        Self {
            requests,
            done,
            generations: HashMap::new(),
            next_generation: 0,
        }
    }
}

impl PreviewWorker {
    /// Draw `brush`'s preview as `key` (replacing any asked before).
    pub(crate) fn request(
        &mut self,
        key: &str,
        brush: &Brush,
        look: PreviewLook,
        pool: &Arc<ThreadPool>,
        ctx: &egui::Context,
    ) {
        self.next_generation += 1;
        let generation = self.next_generation;
        self.generations.insert(key.to_string(), generation);
        let _ = self.requests.send(Request {
            key: key.to_string(),
            generation,
            brush: brush.clone(),
            look,
            pool: Arc::clone(pool),
            ctx: ctx.clone(),
        });
    }

    /// Whether `key`'s preview was asked for (it may be drawn, still
    /// drawing, or have failed: it's not asked again until forgotten).
    pub(crate) fn is_requested(&self, key: &str) -> bool {
        self.generations.contains_key(key)
    }

    /// `key`'s preview is out of date: what's being drawn for it is dropped.
    pub(crate) fn forget(&mut self, key: &str) {
        self.generations.remove(key);
    }

    /// The previews drawn since the last call, as textures (only the latest
    /// asked for each key).
    pub(crate) fn collect(&mut self, ctx: &egui::Context) -> Vec<(String, egui::TextureHandle)> {
        let mut out = Vec::new();
        for done in self.done.try_iter() {
            if self.generations.get(&done.key) != Some(&done.generation) {
                continue;
            }
            if let Some(image) = done.image {
                let texture =
                    ctx.load_texture("brush_preview", image, egui::TextureOptions::LINEAR);
                out.push((done.key, texture));
            }
        }
        out
    }
}
