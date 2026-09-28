//! Background stroke rendering, in the spirit of Krita's stroke queue: the UI
//! thread only queues pen samples, and a dedicated thread owns the stroke
//! session and paints them, so a heavy brush never stalls a frame.
//!
//! The canvas is shared through an `Arc`; tile contents are guarded by their
//! own mutexes, so the UI can composite while a stroke is being painted. Any
//! structural change to the canvas (layers, undo, loading) must first end the
//! stroke and [`StrokeWorker::wait_idle`], which releases the worker's `Arc`.

use crate::brush_engine::brush::Brush;
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::brush_engine::symmetry::{Copy2, Symmetry};
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use crate::selection::SelectionManager;
use eframe::egui::Vec2;
use rayon::ThreadPool;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

/// Everything a stroke needs, captured when it starts (none of it can change
/// mid-stroke).
pub struct StrokeSetup {
    pub canvas: Arc<Canvas>,
    pub brush: Brush,
    pub selection: Option<SelectionManager>,
    pub pool: Arc<ThreadPool>,
    /// The layer being painted, for filing the undo record.
    pub layer_idx: usize,
    /// Mirror painting.
    pub symmetry: Symmetry,
    /// Canvas pixels → screen points (the view zoom), for stroke speed.
    pub view_scale: f32,
}

/// A completed stroke's undo record.
pub struct FinishedStroke {
    pub layer_idx: usize,
    pub undo: UndoAction,
}

/// Shortest and longest the worker sleeps between airbrush dabs (seconds):
/// no faster than a fast display refreshes, and often enough that a low
/// rate still starts promptly once the pen stops.
const MIN_AIRBRUSH_WAIT: f32 = 1.0 / 240.0;
const MAX_AIRBRUSH_WAIT: f32 = 0.05;

enum Job {
    Begin(Box<StrokeSetup>),
    /// `time`: seconds since the worker started, for stroke speed.
    Sample {
        pos: Vec2,
        pressure: f32,
        time: f64,
        tilt: Option<crate::brush_engine::dynamics::PenTilt>,
    },
    End,
}

#[derive(Default)]
struct SharedState {
    /// Jobs queued or running.
    pending: usize,
    /// Tiles painted since the UI last collected them, with the tile-local
    /// rectangle that changed in each.
    dirty: HashMap<(usize, usize), [usize; 4]>,
    finished: Vec<FinishedStroke>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<SharedState>,
    idle: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SharedState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct StrokeWorker {
    /// Sample times count from here.
    epoch: std::time::Instant,
    jobs: Option<Sender<Job>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Default for StrokeWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl StrokeWorker {
    pub fn new() -> Self {
        let (jobs, receiver) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let thread_shared = Arc::clone(&shared);
        let epoch = std::time::Instant::now();
        let thread = std::thread::Builder::new()
            .name("stroke-worker".into())
            .spawn(move || {
                let mut session: Option<Session> = None;
                loop {
                    // An airbrush keeps painting between samples: wake up
                    // when its next dab is due.
                    let airbrush = session
                        .as_ref()
                        .map(|s| s.setup.brush.airbrush_rate)
                        .filter(|&rate| rate > 0.0);
                    let job = match airbrush {
                        None => match receiver.recv() {
                            Ok(job) => job,
                            Err(_) => break,
                        },
                        Some(rate) => {
                            let wait = std::time::Duration::from_secs_f32(
                                (1.0 / rate).clamp(MIN_AIRBRUSH_WAIT, MAX_AIRBRUSH_WAIT),
                            );
                            match receiver.recv_timeout(wait) {
                                Ok(job) => job,
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    let now = epoch.elapsed().as_secs_f64();
                                    let result = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            if let Some(session) = session.as_mut() {
                                                session.paint(
                                                    &thread_shared,
                                                    |stroke, brush, context| {
                                                        stroke.airbrush(brush, now, context)
                                                    },
                                                );
                                            }
                                        }),
                                    );
                                    if result.is_err() {
                                        log::error!(
                                            "stroke worker panicked; dropping the current stroke"
                                        );
                                        session = None;
                                    }
                                    continue;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_job(&mut session, job, &thread_shared)
                    }));
                    if result.is_err() {
                        log::error!("stroke worker panicked; dropping the current stroke");
                        session = None;
                    }
                    let mut state = thread_shared.lock();
                    state.pending -= 1;
                    if state.pending == 0 {
                        thread_shared.idle.notify_all();
                    }
                }
            })
            .expect("failed to spawn the stroke worker thread");
        Self {
            epoch,
            jobs: Some(jobs),
            shared,
            thread: Some(thread),
        }
    }

    pub fn begin(&self, setup: StrokeSetup) {
        self.send(Job::Begin(Box::new(setup)));
    }

    pub fn sample(&self, pos: Vec2, pressure: f32) {
        self.sample_tilted(pos, pressure, None);
    }

    /// [`Self::sample`] with how the pen leans.
    pub fn sample_tilted(
        &self,
        pos: Vec2,
        pressure: f32,
        tilt: Option<crate::brush_engine::dynamics::PenTilt>,
    ) {
        let time = self.epoch.elapsed().as_secs_f64();
        self.send(Job::Sample {
            pos,
            pressure,
            time,
            tilt,
        });
    }

    pub fn end(&self) {
        self.send(Job::End);
    }

    fn send(&self, job: Job) {
        self.shared.lock().pending += 1;
        let sent = self
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok());
        if !sent {
            // Worker gone (only possible during shutdown): don't leave waiters hanging.
            let mut state = self.shared.lock();
            state.pending -= 1;
            if state.pending == 0 {
                self.shared.idle.notify_all();
            }
        }
    }

    /// Whether queued samples are still being painted.
    pub fn is_busy(&self) -> bool {
        self.shared.lock().pending > 0
    }

    /// Block until every queued job has been processed.
    pub fn wait_idle(&self) {
        let mut state = self.shared.lock();
        while state.pending > 0 {
            state = self
                .shared
                .idle
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Like [`Self::wait_idle`] but gives up after `timeout`; returns whether
    /// the worker went idle.
    pub fn wait_idle_for(&self, timeout: std::time::Duration) -> bool {
        let state = self.shared.lock();
        let (state, _) = self
            .shared
            .idle
            .wait_timeout_while(state, timeout, |state| state.pending > 0)
            .unwrap_or_else(|e| e.into_inner());
        state.pending == 0
    }

    /// Tiles painted since the last call, with the rectangle that changed.
    pub fn take_dirty(&self) -> HashMap<(usize, usize), [usize; 4]> {
        std::mem::take(&mut self.shared.lock().dirty)
    }

    /// Undo records of strokes that have ended since the last call.
    pub fn take_finished(&self) -> Vec<FinishedStroke> {
        std::mem::take(&mut self.shared.lock().finished)
    }
}

impl Drop for StrokeWorker {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Session {
    setup: StrokeSetup,
    stroke: StrokeState,
    undo: UndoAction,
    tiles: StrokeTiles,
    /// The symmetry's copy maps, computed once per stroke.
    copies: Vec<Copy2>,
}

fn run_job(session: &mut Option<Session>, job: Job, shared: &Shared) {
    match job {
        Job::Begin(setup) => {
            let copies = setup.symmetry.copies();
            let mut stroke = StrokeState::new();
            stroke.view_scale = setup.view_scale;
            *session = Some(Session {
                copies,
                setup: *setup,
                stroke,
                undo: UndoAction {
                    tiles: Vec::new(),
                    selection: None,
                    transform: None,
                    layer_action: None,
                },
                tiles: StrokeTiles::default(),
            });
        }
        Job::Sample {
            pos,
            pressure,
            time,
            tilt,
        } => {
            let Some(session) = session else {
                return;
            };
            session.paint(shared, |stroke, brush, context| {
                stroke.tilt = tilt;
                stroke.add_sample(brush, pos, pressure, Some(time), context);
            });
        }
        Job::End => {
            // The pen lifted: the end of the stroke (an end taper) first.
            if let Some(session) = session.as_mut() {
                session.paint(shared, |stroke, brush, context| {
                    stroke.finish(brush, context)
                });
            }
            // Dropping the session releases its `Arc<Canvas>` and stroke buffers.
            if let Some(Session { setup, undo, .. }) = session.take()
                && !undo.tiles.is_empty()
            {
                shared.lock().finished.push(FinishedStroke {
                    layer_idx: setup.layer_idx,
                    undo,
                });
            }
        }
    }
}

impl Session {
    /// Run `f` on the stroke, then hand the tiles it painted to the UI.
    fn paint(
        &mut self,
        shared: &Shared,
        f: impl FnOnce(&mut StrokeState, &mut Brush, &mut StrokeContext<'_>),
    ) {
        let Session {
            setup,
            stroke,
            undo,
            tiles,
            copies,
        } = self;
        let StrokeSetup {
            canvas,
            brush,
            selection,
            pool,
            symmetry,
            ..
        } = setup;
        let mut context = StrokeContext::new(pool, canvas, selection.as_ref(), undo, tiles)
            .with_symmetry(symmetry, copies);
        f(stroke, brush, &mut context);
        let touched = std::mem::take(&mut tiles.dirty);
        let mut shared = shared.lock();
        for key in touched {
            // A tile the dabs' rectangles reached but no pixel changed in
            // has no damage and needs no redraw.
            let Some(rect) = tiles
                .buffers
                .get(&key)
                .and_then(|b| b.lock().unwrap_or_else(|e| e.into_inner()).damage.take())
            else {
                continue;
            };
            shared
                .dirty
                .entry(key)
                .and_modify(|d| {
                    *d = [
                        d[0].min(rect[0]),
                        d[1].min(rect[1]),
                        d[2].max(rect[2]),
                        d[3].max(rect[3]),
                    ]
                })
                .or_insert(rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;
    use rayon::ThreadPoolBuilder;

    fn samples() -> Vec<(Vec2, f32)> {
        (0..40)
            .map(|i| {
                let t = i as f32 / 39.0;
                (
                    Vec2::new(10.0 + t * 100.0, 60.0 + (t * 7.0).sin() * 40.0),
                    0.4 + 0.6 * t,
                )
            })
            .collect()
    }

    fn tiles_of(canvas: &Canvas) -> Vec<Option<Vec<Color32>>> {
        (0..2)
            .flat_map(|ty| (0..2).map(move |tx| (tx, ty)))
            .map(|(tx, ty)| canvas.get_layer_tile_data(1, tx, ty))
            .collect()
    }

    #[test]
    fn worker_paints_like_synchronous_painting_and_releases_the_canvas() {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(2).build().unwrap());
        let mut brush = Brush::new(20.0, 30.0, Color32::from_rgb(40, 120, 220), 15.0);

        let sync_canvas = Canvas::new(128, 128, Color32::TRANSPARENT, 64);
        let mut undo = UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        };
        let mut tiles = StrokeTiles::default();
        let mut stroke = StrokeState::new();
        let mut context = StrokeContext::new(&pool, &sync_canvas, None, &mut undo, &mut tiles);
        for (pos, pressure) in samples() {
            stroke.add_point(&mut brush, pos, pressure, &mut context);
        }

        let canvas = Arc::new(Canvas::new(128, 128, Color32::TRANSPARENT, 64));
        let worker = StrokeWorker::new();
        worker.begin(StrokeSetup {
            canvas: Arc::clone(&canvas),
            brush,
            selection: None,
            pool,
            layer_idx: 1,
            symmetry: Default::default(),
            view_scale: 1.0,
        });
        for (pos, pressure) in samples() {
            worker.sample(pos, pressure);
        }
        worker.end();
        worker.wait_idle();

        assert!(tiles_of(&canvas) == tiles_of(&sync_canvas));
        let finished = worker.take_finished();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].layer_idx, 1);
        assert_eq!(finished[0].undo.tiles.len(), undo.tiles.len());
        assert!(!worker.take_dirty().is_empty());
        assert!(!worker.is_busy());
        assert_eq!(
            Arc::strong_count(&canvas),
            1,
            "an idle worker holds no canvas"
        );
    }

    #[test]
    fn the_worker_keeps_airbrushing_while_the_pen_is_held_still() {
        let centre_after_hold = |rate: f32| {
            let pool = Arc::new(ThreadPoolBuilder::new().num_threads(1).build().unwrap());
            let mut brush = Brush::new(30.0, 0.0, Color32::BLACK, 10.0);
            brush.brush_options.flow = 5.0;
            brush.airbrush_rate = rate;
            let canvas = Arc::new(Canvas::new(128, 128, Color32::TRANSPARENT, 64));
            let worker = StrokeWorker::new();
            worker.begin(StrokeSetup {
                canvas: Arc::clone(&canvas),
                brush,
                selection: None,
                pool,
                layer_idx: 1,
                symmetry: Default::default(),
                view_scale: 1.0,
            });
            worker.sample(Vec2::new(64.0, 64.0), 1.0);
            std::thread::sleep(std::time::Duration::from_millis(300));
            worker.end();
            worker.wait_idle();
            let tile = canvas.get_layer_tile_data(1, 1, 1).unwrap();
            tile[0].a()
        };
        let (held, plain) = (centre_after_hold(60.0), centre_after_hold(0.0));
        assert!(held > plain + 40, "{plain} → {held}");
    }
}
