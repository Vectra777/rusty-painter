//! Background stroke rendering, a stroke queue: the UI thread only queues pen samples, and a dedicated thread owns the stroke
//! session and paints them, so a heavy brush never stalls a frame.
//!
//! The canvas is shared through an `Arc`; tile contents are guarded by their
//! own mutexes, so the UI can composite while a stroke is being painted. Any
//! structural change to the canvas (layers, undo, loading) must first end the
//! stroke and [`StrokeWorker::wait_idle`], which releases the worker's `Arc`.

use crate::brush_engine::brush::{Brush, StabilizerAlgorithm};
use crate::brush_engine::dynamics::{PenBarrel, PenTilt};
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
    /// Wrap-around painting.
    pub wrap: bool,
    /// The enabled perspective assistants, for the Perspective input.
    pub perspective: Vec<crate::brush_engine::dynamics::PerspectiveGrid>,
}

/// A stroke painted by its own engine, dab after dab, each seeing the last
/// one's result (the Smudge and Blur tools, mixing brushes): the worker
/// runs it like a brush stroke.
pub trait SequentialStroke: Send {
    /// The stroke's first dab.
    fn start(&mut self);
    /// The pen moved to `pos`.
    fn drag(&mut self, pos: Vec2, pressure: f32);
    /// A pen sample with all the pen gave (when, its lean and barrel); by
    /// default only where it is and how hard.
    fn sample(&mut self, sample: PenSample) {
        self.drag(sample.pos, sample.pressure);
    }
    /// How often an airbrush dabs while the pen rests (a second; 0: it
    /// doesn't).
    fn airbrush_rate(&self) -> f32 {
        0.0
    }
    /// The airbrush's timer, at `now` (seconds).
    fn airbrush(&mut self, _now: f64) {}
    /// Canvas rectangles painted since the last call.
    fn take_damage(&mut self) -> Vec<[i32; 4]>;
    /// The canvas painted.
    fn canvas(&self) -> &Canvas;
    /// The layer painted, for filing the undo record.
    fn layer_idx(&self) -> usize;
    /// The pen lifted: the stroke's undo record, if it changed anything.
    fn finish(self: Box<Self>) -> Option<UndoAction>;
}

/// A pen sample, as a sequential stroke gets it.
#[derive(Clone, Copy, Debug)]
pub struct PenSample {
    pub pos: Vec2,
    pub pressure: f32,
    /// Seconds since the worker started.
    pub time: f64,
    pub tilt: Option<PenTilt>,
    pub barrel: PenBarrel,
}

/// Canvas work queued behind the strokes (a filter, a merge, a fill): it
/// shares the canvas the way a stroke does, so it runs here, in order with
/// them, and whatever waits for the strokes waits for it too.
pub type Task = Box<dyn FnOnce() -> Box<dyn std::any::Any + Send> + Send>;

/// What the worker finished, in the order it did.
pub enum Finished {
    Stroke(Box<FinishedStroke>),
    /// A task's result, or `Err` if it panicked.
    Task(Result<Box<dyn std::any::Any + Send>, String>),
}

impl Finished {
    /// The stroke, if it's one (tests).
    pub fn stroke(&self) -> Option<&FinishedStroke> {
        match self {
            Self::Stroke(s) => Some(s),
            Self::Task(_) => None,
        }
    }
}

/// A completed stroke's undo record.
pub struct FinishedStroke {
    pub layer_idx: usize,
    pub undo: UndoAction,
    /// Which stroke this was: the `n`th one ended (from 1), as
    /// [`StrokeWorker::ends_sent`] counts them.
    pub seq: u64,
}

/// Shortest and longest the worker sleeps between airbrush dabs (seconds):
/// no faster than a fast display refreshes, and often enough that a low
/// rate still starts promptly once the pen stops.
const MIN_AIRBRUSH_WAIT: f32 = 1.0 / 240.0;
const MAX_AIRBRUSH_WAIT: f32 = 0.05;

enum Job {
    Begin(Box<StrokeSetup>),
    /// A sequential stroke.
    BeginSequential(Box<dyn SequentialStroke>),
    /// `time`: seconds since the worker started, for stroke speed.
    Sample {
        pos: Vec2,
        pressure: f32,
        time: f64,
        tilt: Option<crate::brush_engine::dynamics::PenTilt>,
        barrel: PenBarrel,
    },
    End,
    Task(Task),
}

#[derive(Default)]
struct SharedState {
    /// Jobs queued or running.
    pending: usize,
    /// Tiles painted since the UI last collected them, with the tile-local
    /// rectangle that changed in each.
    dirty: HashMap<(usize, usize), [usize; 4]>,
    finished: Vec<Finished>,
    /// Strokes ended so far (painted to the end, with or without an undo
    /// record), counted once each one's record is in `finished`.
    ended: u64,
}

impl SharedState {
    /// A record for the stroke ending now.
    fn finish(&mut self, layer_idx: usize, undo: UndoAction) {
        let seq = self.ended + 1;
        self.finished
            .push(Finished::Stroke(Box::new(FinishedStroke {
                layer_idx,
                undo,
                seq,
            })));
    }
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
    /// Strokes ended so far ([`Self::end`] calls).
    ends_sent: std::sync::atomic::AtomicU64,
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
                let mut sequential: Option<Box<dyn SequentialStroke>> = None;
                loop {
                    // An airbrush keeps painting between samples: wake up
                    // when its next dab is due.
                    let airbrush = session
                        .as_ref()
                        .map(|s| s.setup.brush.airbrush_rate)
                        .or_else(|| sequential.as_ref().map(|s| s.airbrush_rate()))
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
                                                session.airbrush(&thread_shared, now);
                                            } else if let Some(stroke) = sequential.as_mut() {
                                                stroke.airbrush(now);
                                                hand_over_damage(stroke.as_mut(), &thread_shared);
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
                    let is_end = matches!(job, Job::End);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_job(&mut session, &mut sequential, job, &thread_shared)
                    }));
                    if result.is_err() {
                        log::error!("stroke worker panicked; dropping the current stroke");
                        session = None;
                        sequential = None;
                    }
                    let mut state = thread_shared.lock();
                    // (Counted even if it panicked: it's over either way.)
                    if is_end {
                        state.ended += 1;
                    }
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
            ends_sent: Default::default(),
            shared,
            thread: Some(thread),
        }
    }

    pub fn begin(&self, setup: StrokeSetup) {
        self.send(Job::Begin(Box::new(setup)));
    }

    /// Start a sequential stroke; samples and the end go to it until it
    /// ends.
    pub fn begin_sequential(&self, stroke: Box<dyn SequentialStroke>) {
        self.send(Job::BeginSequential(stroke));
    }

    pub fn sample(&self, pos: Vec2, pressure: f32) {
        self.sample_tilted(pos, pressure, None, PenBarrel::default());
    }

    /// [`Self::sample`] with how the pen leans and turns.
    pub fn sample_tilted(
        &self,
        pos: Vec2,
        pressure: f32,
        tilt: Option<crate::brush_engine::dynamics::PenTilt>,
        barrel: PenBarrel,
    ) {
        let time = self.epoch.elapsed().as_secs_f64();
        self.send(Job::Sample {
            pos,
            pressure,
            time,
            tilt,
            barrel,
        });
    }

    /// Run `task` once what's queued before it is done.
    pub fn task(&self, task: Task) {
        self.send(Job::Task(task));
    }

    pub fn end(&self) {
        self.ends_sent
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.send(Job::End);
    }

    /// How many strokes have been ended (sent [`Self::end`]) so far: the
    /// `seq` of the last one's [`FinishedStroke`].
    pub fn ends_sent(&self) -> u64 {
        self.ends_sent.load(std::sync::atomic::Ordering::Relaxed)
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
    /// Jobs queued or running (each sample is one).
    pub fn pending(&self) -> usize {
        self.shared.lock().pending
    }

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

    /// Undo records of strokes that have ended since the last call, in
    /// order, and how many strokes have ended in all (some leave no record).
    pub fn take_finished(&self) -> (Vec<Finished>, u64) {
        let mut state = self.shared.lock();
        (std::mem::take(&mut state.finished), state.ended)
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
    /// The stroke's random seed, to paint it again the same.
    seed: u64,
    /// The last sample, for a pulled string catching up when the pen lifts.
    last_sample: Option<Sample>,
    /// Post-correction: the brush as the stroke started, and everything
    /// the stroke was given, to paint it again along the smoothed path
    /// when the pen lifts.
    correction: Option<(Brush, Vec<Event>)>,
    /// Post-correction while drawing: how far it's painted for good.
    live: Option<LiveCorrection>,
}

/// Post-correction while drawing: the events painted along the smoothed
/// path for good (those whose place can't change any more), and the stroke
/// as it was then, the rest having been painted after it for now.
#[derive(Default)]
struct LiveCorrection {
    /// How many of the events are painted for good.
    committed: usize,
    /// The stroke and its brush at the checkpoint (the tiles keep theirs).
    saved: Option<(StrokeState, Brush)>,
}

/// Tests: the seed every stroke gets (0: a random one each).
#[cfg(test)]
static TEST_SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tests_seed() -> Option<u64> {
    #[cfg(test)]
    {
        let seed = TEST_SEED.load(std::sync::atomic::Ordering::Relaxed);
        if seed != 0 {
            return Some(seed);
        }
    }
    None
}

/// A pen sample as the worker got it.
#[derive(Clone, Copy)]
struct Sample {
    pos: Vec2,
    pressure: f32,
    time: f64,
    tilt: Option<PenTilt>,
    barrel: PenBarrel,
}

/// What a stroke was given, in order, for post-correction.
#[derive(Clone, Copy)]
enum Event {
    Sample(Sample),
    /// The airbrush's timer, at this time.
    Airbrush(f64),
}

fn run_job(
    session: &mut Option<Session>,
    sequential: &mut Option<Box<dyn SequentialStroke>>,
    job: Job,
    shared: &Shared,
) {
    // A sequential stroke takes the samples and the end while it runs.
    if let Some(stroke) = sequential.as_mut() {
        match job {
            Job::Sample {
                pos,
                pressure,
                time,
                tilt,
                barrel,
            } => {
                stroke.sample(PenSample {
                    pos,
                    pressure,
                    time,
                    tilt,
                    barrel,
                });
                hand_over_damage(stroke.as_mut(), shared);
                return;
            }
            Job::End => {
                if let Some(stroke) = sequential.take() {
                    let layer_idx = stroke.layer_idx();
                    if let Some(undo) = stroke.finish() {
                        shared.lock().finish(layer_idx, undo);
                    }
                }
                return;
            }
            // A new stroke: this one is left as it is (a stroke always
            // ends first).
            Job::Begin(_) | Job::BeginSequential(_) => *sequential = None,
            // (Tasks come after a stroke has ended.)
            Job::Task(task) => return run_task(task, shared),
        }
    }
    match job {
        Job::BeginSequential(mut stroke) => {
            *session = None;
            stroke.start();
            hand_over_damage(stroke.as_mut(), shared);
            *sequential = Some(stroke);
        }
        Job::Begin(setup) => {
            let copies = setup.symmetry.copies();
            let seed = match tests_seed() {
                Some(seed) => seed,
                None => rand::random(),
            };
            let mut stroke = StrokeState::with_seed(seed);
            stroke.view_scale = setup.view_scale;
            stroke.perspective = setup.perspective.clone();
            let brush = &setup.brush;
            let correction = (brush.stabilizer_algorithm == StabilizerAlgorithm::PostCorrection
                && brush.stabilizer_modes.correction > 0.0)
                .then(|| (brush.clone(), Vec::new()));
            let live = (correction.is_some() && brush.stabilizer_modes.correction_live)
                .then(LiveCorrection::default);
            *session = Some(Session {
                copies,
                seed,
                last_sample: None,
                correction,
                live,
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
            barrel,
        } => {
            let Some(session) = session else {
                return;
            };
            let sample = Sample {
                pos,
                pressure,
                time,
                tilt,
                barrel,
            };
            session.last_sample = Some(sample);
            if let Some((_, events)) = session.correction.as_mut() {
                events.push(Event::Sample(sample));
            }
            if session.live.is_some() {
                session.correct_live(shared, false);
                return;
            }
            session.paint(shared, |stroke, brush, context| {
                stroke.tilt = tilt;
                stroke.barrel = barrel;
                stroke.add_sample(brush, pos, pressure, Some(time), context);
            });
        }
        Job::Task(task) => run_task(task, shared),
        Job::End => {
            // The pen lifted: the end of the stroke (an end taper) first.
            if let Some(session) = session.as_mut() {
                session.catch_up(shared);
                if session.live.is_some() {
                    session.correct_live(shared, true);
                }
                session.correct(shared);
                session.paint(shared, |stroke, brush, context| {
                    stroke.finish(brush, context)
                });
            }
            // Dropping the session releases its `Arc<Canvas>` and stroke buffers.
            if let Some(Session { setup, undo, .. }) = session.take()
                && !undo.tiles.is_empty()
            {
                shared.lock().finish(setup.layer_idx, undo);
            }
        }
    }
}

/// Run `task`, its result (or its panic) to the UI.
fn run_task(task: Task, shared: &Shared) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).map_err(|panic| {
        panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "it stopped".to_string())
    });
    shared.lock().finished.push(Finished::Task(result));
}

/// A sequential stroke's painted rectangles to the UI, as tile-local
/// rectangles like a brush stroke's.
fn hand_over_damage(stroke: &mut dyn SequentialStroke, shared: &Shared) {
    let rects = stroke.take_damage();
    if rects.is_empty() {
        return;
    }
    let canvas = stroke.canvas();
    let ts = canvas.tile_size() as i32;
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let mut shared = shared.lock();
    for rect in rects {
        let [x0, y0, x1, y1] = [
            rect[0].max(0),
            rect[1].max(0),
            rect[2].min(w),
            rect[3].min(h),
        ];
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        for ty in y0 / ts..=(y1 - 1) / ts {
            for tx in x0 / ts..=(x1 - 1) / ts {
                let (ox, oy) = (tx * ts, ty * ts);
                let local = [
                    (x0.max(ox) - ox) as usize,
                    (y0.max(oy) - oy) as usize,
                    (x1.min(ox + ts) - ox) as usize,
                    (y1.min(oy + ts) - oy) as usize,
                ];
                shared
                    .dirty
                    .entry((tx as usize, ty as usize))
                    .and_modify(|d| {
                        *d = [
                            d[0].min(local[0]),
                            d[1].min(local[1]),
                            d[2].max(local[2]),
                            d[3].max(local[3]),
                        ]
                    })
                    .or_insert(local);
            }
        }
    }
}

impl Session {
    /// The airbrush's timer: dabs where the pen rests.
    fn airbrush(&mut self, shared: &Shared, now: f64) {
        if let Some((_, events)) = self.correction.as_mut() {
            events.push(Event::Airbrush(now));
        }
        if self.live.is_some() {
            return self.correct_live(shared, false);
        }
        self.paint(shared, |stroke, brush, context| {
            stroke.airbrush(brush, now, context)
        });
    }

    /// A pulled string with catch-up: the line goes on to where the pen
    /// lifted.
    fn catch_up(&mut self, shared: &Shared) {
        let brush = &self.setup.brush;
        let Some(last) = self.last_sample else {
            return;
        };
        if brush.stabilizer_algorithm != StabilizerAlgorithm::String
            || !brush.stabilizer_modes.catch_up
            || self.stroke.last_pos == Some(last.pos)
        {
            return;
        }
        self.paint(shared, |stroke, brush, context| {
            brush.stabilizer_algorithm = StabilizerAlgorithm::None;
            stroke.tilt = last.tilt;
            stroke.barrel = last.barrel;
            stroke.add_sample(brush, last.pos, last.pressure, Some(last.time), context);
            brush.stabilizer_algorithm = StabilizerAlgorithm::String;
        });
    }

    /// Post-correction: take the stroke off and paint it again along its
    /// path smoothed, in the same undo step (the tiles' snapshots from
    /// before the stroke stay as they are).
    fn correct(&mut self, shared: &Shared) {
        let Some((brush, events)) = self.correction.take() else {
            return;
        };
        let points: Vec<Vec2> = events
            .iter()
            .filter_map(|e| match e {
                Event::Sample(s) => Some(s.pos),
                Event::Airbrush(_) => None,
            })
            .collect();
        if points.len() < 3 {
            return;
        }
        let smoothed = crate::brush_engine::stabilizer::smooth_path(
            &points,
            brush.stabilizer_modes.correction,
            self.setup.view_scale,
        );
        self.tiles.restart(&self.setup.canvas);
        self.stroke = StrokeState::with_seed(self.seed);
        self.stroke.view_scale = self.setup.view_scale;
        self.stroke.perspective = self.setup.perspective.clone();
        self.setup.brush = brush;
        self.paint(shared, |stroke, brush, context| {
            let mut smoothed = smoothed.into_iter();
            for event in events {
                match event {
                    Event::Sample(s) => {
                        let pos = smoothed.next().unwrap_or(s.pos);
                        stroke.tilt = s.tilt;
                        stroke.barrel = s.barrel;
                        stroke.add_sample(brush, pos, s.pressure, Some(s.time), context);
                    }
                    Event::Airbrush(now) => stroke.airbrush(brush, now, context),
                }
            }
        });
    }

    /// Post-correction while drawing, after each event: what was painted
    /// for now is taken back, the events whose smoothed place is now final
    /// are painted for good, and the rest along the path smoothed as it
    /// stands, for now (all for good when the pen has lifted, `last`).
    /// Painted in the same order as [`Self::correct`] paints them, so the
    /// line comes out the same.
    fn correct_live(&mut self, shared: &Shared, last: bool) {
        let (Some(live), Some((_, events))) = (self.live.as_mut(), self.correction.as_mut()) else {
            return;
        };
        if let Some((stroke, brush)) = live.saved.take() {
            self.tiles.rewind(&self.setup.canvas);
            self.stroke = stroke;
            self.setup.brush = brush;
        }
        let events = std::mem::take(events);
        let points: Vec<Vec2> = events
            .iter()
            .filter_map(|e| match e {
                Event::Sample(s) => Some(s.pos),
                Event::Airbrush(_) => None,
            })
            .collect();
        let strength = self.setup.brush.stabilizer_modes.correction;
        let scale = self.setup.view_scale;
        let settled = if last {
            points.len()
        } else {
            crate::brush_engine::stabilizer::smooth_path_settled(&points, strength, scale)
        };
        let committed = live.committed;
        let first = (events[..committed].iter())
            .filter(|e| matches!(e, Event::Sample(_)))
            .count();
        let smoothed =
            crate::brush_engine::stabilizer::smooth_path_from(&points, strength, scale, first);
        // The events from `committed` on, each with its smoothed place, and
        // how many of them are final.
        let mut place = smoothed.into_iter();
        let rest: Vec<(Event, Option<Vec2>)> = events[committed..]
            .iter()
            .map(|&e| match e {
                Event::Sample(s) => (e, Some(place.next().unwrap_or(s.pos))),
                Event::Airbrush(_) => (e, None),
            })
            .collect();
        // Final: up to (not including) the first sample not yet settled.
        let mut fixed = 0;
        let mut n_samples = first;
        for (e, _) in &rest {
            if let Event::Sample(_) = e {
                if n_samples >= settled {
                    break;
                }
                n_samples += 1;
            }
            fixed += 1;
        }
        let paint_events = |stroke: &mut StrokeState,
                            brush: &mut Brush,
                            context: &mut StrokeContext<'_>,
                            events: &[(Event, Option<Vec2>)]| {
            for &(event, pos) in events {
                match event {
                    Event::Sample(s) => {
                        stroke.tilt = s.tilt;
                        stroke.barrel = s.barrel;
                        let pos = pos.unwrap_or(s.pos);
                        stroke.add_sample(brush, pos, s.pressure, Some(s.time), context);
                    }
                    Event::Airbrush(now) => stroke.airbrush(brush, now, context),
                }
            }
        };
        let (now, later) = rest.split_at(fixed);
        self.paint(shared, |stroke, brush, context| {
            paint_events(stroke, brush, context, now)
        });
        if let Some(live) = self.live.as_mut() {
            live.committed = committed + fixed;
        }
        // The rest for now, unless more is waiting (it would be taken back
        // straight away: the pen outruns the painting).
        let more_waiting = shared.lock().pending > 1;
        if !later.is_empty() && !more_waiting {
            let saved = (self.stroke.clone(), self.setup.brush.clone());
            if let Some(live) = self.live.as_mut() {
                live.saved = Some(saved);
            }
            self.tiles.checkpoint(&self.setup.canvas);
            self.paint(shared, |stroke, brush, context| {
                paint_events(stroke, brush, context, later)
            });
        }
        if let Some((_, slot)) = self.correction.as_mut() {
            *slot = events;
        }
        if last {
            // Painted along the final path: nothing to correct after.
            self.correction = None;
        }
    }

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
            ..
        } = self;
        let StrokeSetup {
            canvas,
            brush,
            selection,
            pool,
            symmetry,
            wrap,
            ..
        } = setup;
        let mut context = StrokeContext::new(pool, canvas, selection.as_ref(), undo, tiles)
            .with_symmetry(symmetry, copies)
            .with_wrap(*wrap);
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
            perspective: Vec::new(),
            wrap: false,
        });
        for (pos, pressure) in samples() {
            worker.sample(pos, pressure);
        }
        worker.end();
        worker.wait_idle();

        assert!(tiles_of(&canvas) == tiles_of(&sync_canvas));
        let (finished, ended) = worker.take_finished();
        assert_eq!(finished.len(), 1);
        let stroke = finished[0].stroke().unwrap();
        assert_eq!((stroke.seq, ended), (1, 1));
        assert_eq!(stroke.layer_idx, 1);
        assert_eq!(stroke.undo.tiles.len(), undo.tiles.len());
        assert!(!worker.take_dirty().is_empty());
        assert!(!worker.is_busy());
        assert_eq!(
            Arc::strong_count(&canvas),
            1,
            "an idle worker holds no canvas"
        );
    }

    /// `samples` painted with `brush` on the worker, on a fresh canvas.
    fn paint_on_worker(
        brush: Brush,
        samples: &[(Vec2, f32)],
    ) -> (Arc<Canvas>, Vec<FinishedStroke>) {
        paint_on_worker_paced(brush, samples, false)
    }

    /// [`paint_on_worker`], each sample painted before the next is sent
    /// (`paced`) as a pen slower than the painting gives them.
    fn paint_on_worker_paced(
        brush: Brush,
        samples: &[(Vec2, f32)],
        paced: bool,
    ) -> (Arc<Canvas>, Vec<FinishedStroke>) {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(2).build().unwrap());
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
            perspective: Vec::new(),
            wrap: false,
        });
        for &(pos, pressure) in samples {
            worker.sample(pos, pressure);
            if paced {
                worker.wait_idle();
            }
        }
        worker.end();
        worker.wait_idle();
        let (finished, _) = worker.take_finished();
        let strokes = finished
            .into_iter()
            .filter_map(|f| match f {
                Finished::Stroke(s) => Some(*s),
                Finished::Task(_) => None,
            })
            .collect();
        (canvas, strokes)
    }

    #[test]
    fn post_correction_while_drawing_paints_the_same_line_as_at_pen_up() {
        // Same random draws for every stroke here.
        TEST_SEED.store(0x5eed, std::sync::atomic::Ordering::Relaxed);
        // Shaky, and back over itself (the part painted for now must leave
        // nothing behind).
        let mut path: Vec<(Vec2, f32)> = samples()
            .into_iter()
            .enumerate()
            .map(|(i, (p, pressure))| (p + Vec2::new(0.0, (i % 2) as f32 * 6.0 - 3.0), pressure))
            .collect();
        let back: Vec<_> = path
            .iter()
            .rev()
            .map(|&(p, pr)| (p + Vec2::new(0.0, 9.0), pr))
            .collect();
        path.extend(back);
        let round = Brush::new(8.0, 80.0, Color32::from_rgb(200, 40, 40), 10.0);
        let mut textured = Brush::new(24.0, 60.0, Color32::from_rgb(20, 90, 40), 12.0);
        let mut t = crate::brush_engine::texture::BrushTexture::new(
            crate::brush_engine::texture::builtin()[1].clone(),
        );
        t.strength = 1.0;
        textured.texture = Some(t);
        let mut random = Brush::new(14.0, 70.0, Color32::from_rgb(30, 30, 160), 20.0);
        random.dynamics.random.size = 0.6;
        random.dynamics.random.opacity = 0.5;
        random.jitter = 0.4;
        let mut wash = Brush::new(18.0, 50.0, Color32::from_rgb(120, 60, 10), 8.0);
        wash.brush_options.painting_mode = crate::brush_engine::brush_options::PaintingMode::Wash;
        for (name, mut brush) in [
            ("round", round),
            ("texture", textured),
            ("random", random),
            ("wash", wash),
        ] {
            brush.stabilizer_algorithm = StabilizerAlgorithm::PostCorrection;
            brush.stabilizer_modes.correction = 0.7;
            let (at_end, end_strokes) = paint_on_worker(brush.clone(), &path);
            brush.stabilizer_modes.correction_live = true;
            // Paced: the stretch near the pen is painted for now, and taken
            // back, after every sample.
            let (live, live_strokes) = paint_on_worker_paced(brush.clone(), &path, true);
            assert!(tiles_of(&live) == tiles_of(&at_end), "{name}");
            let (queued, _) = paint_on_worker(brush, &path);
            assert!(tiles_of(&queued) == tiles_of(&at_end), "{name}, queued");
            assert_eq!(live_strokes.len(), 1, "{name}: one undo step");
            assert_eq!(end_strokes.len(), 1);
            // Undo takes it all back: every tile it touched, blank.
            let undo = &live_strokes[0].undo;
            assert!(
                undo.tiles.iter().all(|t| t
                    .data
                    .to_vec()
                    .iter()
                    .all(|&p| p == Color32::TRANSPARENT))
            );
            let touched = tiles_of(&live)
                .iter()
                .filter(|t| t.as_ref().is_some_and(|d| d.iter().any(|&p| p.a() > 0)))
                .count();
            let mut keys: Vec<_> = undo.tiles.iter().map(|t| (t.tx, t.ty)).collect();
            keys.sort_unstable();
            keys.dedup();
            assert!(keys.len() >= touched, "{name}");
        }
        TEST_SEED.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn post_correction_repaints_the_stroke_along_the_smoothed_path() {
        let shaky: Vec<(Vec2, f32)> = samples()
            .into_iter()
            .enumerate()
            .map(|(i, (p, pressure))| {
                let wobble = if i % 2 == 0 { 3.0 } else { -3.0 };
                (p + Vec2::new(0.0, wobble), pressure)
            })
            .collect();
        let mut brush = Brush::new(8.0, 80.0, Color32::from_rgb(200, 40, 40), 10.0);
        brush.stabilizer_algorithm = StabilizerAlgorithm::PostCorrection;
        brush.stabilizer_modes.correction = 0.8;
        let (canvas, finished) = paint_on_worker(brush.clone(), &shaky);
        assert_eq!(finished.len(), 1, "one undo step");

        // The same as painting the smoothed path straight away.
        let points: Vec<Vec2> = shaky.iter().map(|s| s.0).collect();
        let smoothed = crate::brush_engine::stabilizer::smooth_path(&points, 0.8, 1.0);
        let along: Vec<(Vec2, f32)> = smoothed
            .iter()
            .zip(&shaky)
            .map(|(&p, s)| (p, s.1))
            .collect();
        brush.stabilizer_algorithm = StabilizerAlgorithm::None;
        let (direct, _) = paint_on_worker(brush.clone(), &along);
        assert!(tiles_of(&canvas) == tiles_of(&direct));
        // Not as painted raw.
        let (raw, raw_finished) = paint_on_worker(brush, &shaky);
        assert!(tiles_of(&canvas) != tiles_of(&raw));

        // Undo restores the canvas as it was before the stroke: every tile
        // either stroke touched is snapshotted once, blank.
        let undo = &finished[0].undo;
        let mut keys: Vec<_> = undo.tiles.iter().map(|t| (t.tx, t.ty)).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), undo.tiles.len(), "one snapshot per tile");
        assert!(undo.tiles.len() >= raw_finished[0].undo.tiles.len());
        for t in &undo.tiles {
            assert!(t.data.to_vec().iter().all(|&p| p == Color32::TRANSPARENT));
        }
    }

    #[test]
    fn a_pulled_string_catches_up_with_the_pen_when_it_lifts() {
        let line: Vec<(Vec2, f32)> = (0..=40)
            .map(|i| (Vec2::new(10.0 + i as f32 * 2.5, 64.0), 1.0))
            .collect();
        let end_painted = |catch_up: bool| {
            let mut brush = Brush::new(6.0, 100.0, Color32::BLACK, 10.0);
            brush.stabilizer_algorithm = StabilizerAlgorithm::String;
            brush.stabilizer_modes.string_length = 30.0;
            brush.stabilizer_modes.catch_up = catch_up;
            let (canvas, _) = paint_on_worker(brush, &line);
            let tile = canvas.get_layer_tile_data(1, 1, 1).unwrap();
            // (108, 64): near the pen's last position, 110.
            let near_end = tile[108 - 64].a();
            // (75, 64): well behind the string's length from the end.
            let behind = tile[75 - 64].a();
            (near_end, behind)
        };
        let (with, behind) = end_painted(true);
        assert!(behind > 200 && with > 200, "{behind} {with}");
        let (without, behind) = end_painted(false);
        assert!(behind > 200, "{behind}");
        assert_eq!(without, 0, "the brush stops the string's length short");
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
                perspective: Vec::new(),
                wrap: false,
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
