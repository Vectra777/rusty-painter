//! Work that would hold up a frame (reading, decoding, encoding and writing
//! files, waiting on a file dialog) runs on its own thread; what it returns
//! is applied to the app on the UI thread once it's ready. The frame loop
//! never waits for it, so the window keeps answering the system.

use crate::PainterApp;
use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// A finished job's result, applied on the UI thread.
pub(crate) type Apply = Box<dyn FnOnce(&mut PainterApp) + Send>;

/// Work waiting for the stroke worker to let go of the canvas.
type AfterStrokes = Box<dyn FnOnce(&mut PainterApp)>;

struct Job {
    /// Shown while it runs; the canvas waits for it meanwhile (a document
    /// being opened would replace what's painted).
    label: Option<String>,
    /// A file dialog's (one at a time).
    dialog: bool,
    result: Receiver<Apply>,
    thread: std::thread::JoinHandle<()>,
}

#[derive(Default)]
pub struct Jobs {
    running: Vec<Job>,
    /// Waiting for the stroke worker to paint what's queued (and let go of
    /// the canvas), in order.
    after_strokes: Vec<AfterStrokes>,
    /// What the stroke worker finished, not yet applied (in order).
    pub(crate) finished: std::collections::VecDeque<crate::brush_engine::stroke_worker::Finished>,
    /// What the tasks queued on the stroke worker are doing, oldest first.
    worker_tasks: std::collections::VecDeque<String>,
    /// Shortcuts pressed while the worker was painting, run once it's done.
    pub(crate) deferred_actions: Vec<crate::app::input::keymap::Action>,
    /// To wake the frame loop when a job finishes.
    ctx: Option<egui::Context>,
    /// Tests: leave canvas work queued as the app does, rather than wait.
    #[cfg(test)]
    pub(crate) defer: bool,
}

impl Jobs {
    /// What the canvas is waiting for, if anything.
    pub(crate) fn blocking_label(&self) -> Option<&str> {
        self.worker_tasks
            .front()
            .map(String::as_str)
            .or_else(|| self.running.iter().find_map(|j| j.label.as_deref()))
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.running.is_empty() && self.after_strokes.is_empty()
    }
}

impl PainterApp {
    /// Run `work` on its own thread; the closure it returns is applied on
    /// the UI thread. With a `label`, it's shown while the work runs and the
    /// canvas takes no input meanwhile. A panic in `work` is reported, not
    /// fatal.
    pub(crate) fn spawn_job(
        &mut self,
        label: Option<&str>,
        work: impl FnOnce() -> Apply + Send + 'static,
    ) {
        self.spawn(label, false, work);
    }

    fn spawn(
        &mut self,
        label: Option<&str>,
        dialog: bool,
        work: impl FnOnce() -> Apply + Send + 'static,
    ) {
        let (send, result) = mpsc::channel();
        let ctx = self.workspace.jobs.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("job".into())
            .spawn(move || {
                let apply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| {
                        Box::new(|app: &mut PainterApp| {
                            app.export_state.message =
                                Some("Something went wrong in the background".to_string());
                        })
                    });
                let _ = send.send(apply);
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
        match spawned {
            Ok(thread) => self.workspace.jobs.running.push(Job {
                label: label.map(str::to_string),
                dialog,
                result,
                thread,
            }),
            Err(err) => self.export_state.message = Some(format!("Couldn't start: {err}")),
        }
    }

    /// Run `work` on the stroke worker once what's queued there is done (a
    /// filter, a merge, a fill: canvas work, which shares the canvas the way
    /// strokes do). The closure it returns is applied on the UI thread, in
    /// order with the strokes, once the worker is idle. `label` is shown
    /// meanwhile, and the canvas takes no input. Whatever waits for the
    /// strokes (undo, the layer panel, saving) waits for it too.
    pub(crate) fn run_on_worker(
        &mut self,
        label: &str,
        work: impl FnOnce() -> Apply + Send + 'static,
    ) {
        self.finish_stroke();
        self.workspace
            .jobs
            .worker_tasks
            .push_back(label.to_string());
        self.stroke_worker.task(Box::new(move || {
            Box::new(work()) as Box<dyn std::any::Any + Send>
        }));
        if self.waits_in_tests() {
            self.release_canvas();
        }
    }

    /// Tests check results straight after: they wait (unless `defer`).
    fn waits_in_tests(&self) -> bool {
        #[cfg(test)]
        return !self.workspace.jobs.defer;
        #[cfg(not(test))]
        false
    }

    /// A task from [`Self::run_on_worker`] is done: apply what it returned.
    pub(crate) fn apply_task_result(
        &mut self,
        result: Result<Box<dyn std::any::Any + Send>, String>,
    ) {
        let label = self.workspace.jobs.worker_tasks.pop_front();
        match result.map(|r| r.downcast::<Apply>()) {
            Ok(Ok(apply)) => (*apply)(self),
            Ok(Err(_)) => log::error!("A canvas task returned something unexpected"),
            Err(why) => {
                let what = label
                    .as_deref()
                    .unwrap_or("Canvas work")
                    .trim_end_matches('…');
                self.report(format!("{what} failed: {why}"));
            }
        }
    }

    /// The pen is up but the stroke worker is still painting what was
    /// queued: what needs the canvas to itself (the layer panel, the menus,
    /// shortcuts, undo) waits rather than holding up the frames.
    pub(crate) fn strokes_settling(&self) -> bool {
        !self.brush_state.is_drawing
            && self.brush_state.blend_stroke.is_none()
            && (self.stroke_worker.is_busy() || !self.workspace.jobs.after_strokes.is_empty())
    }

    /// Run `f` with the canvas to itself, which needs the strokes ended
    /// and painted: at once if the stroke worker is idle, else on the first
    /// frame it is (rather than waiting for it here).
    pub(crate) fn when_strokes_painted(&mut self, f: impl FnOnce(&mut PainterApp) + 'static) {
        self.finish_stroke();
        if self.workspace.jobs.after_strokes.is_empty()
            && (self.waits_in_tests() || !self.stroke_worker.is_busy())
        {
            self.release_canvas();
            f(self);
        } else {
            self.workspace.jobs.after_strokes.push(Box::new(f));
        }
    }

    /// Quitting: let the files being written (a save, an autosave) finish,
    /// so none is lost. Dialogs still open are left.
    pub(crate) fn finish_jobs_on_exit(&mut self) {
        if !self.workspace.jobs.after_strokes.is_empty() {
            self.release_canvas();
            for f in std::mem::take(&mut self.workspace.jobs.after_strokes) {
                f(self);
            }
        }
        for job in std::mem::take(&mut self.workspace.jobs.running) {
            if !job.dialog {
                let _ = job.thread.join();
            }
        }
    }

    /// Wait for every job (and those they start) and apply them (tests).
    #[cfg(test)]
    pub(crate) fn run_jobs(&mut self) {
        loop {
            if !self.workspace.jobs.after_strokes.is_empty() {
                self.release_canvas();
                for f in std::mem::take(&mut self.workspace.jobs.after_strokes) {
                    f(self);
                }
            }
            let running = std::mem::take(&mut self.workspace.jobs.running);
            if running.is_empty() {
                return;
            }
            for job in running {
                if let Ok(apply) = job.result.recv() {
                    apply(self);
                }
            }
        }
    }

    /// Apply the jobs that finished (once a frame).
    pub(crate) fn poll_jobs(&mut self, ctx: &egui::Context) {
        self.workspace.jobs.ctx = Some(ctx.clone());
        if !self.workspace.jobs.after_strokes.is_empty() {
            // Not mid-stroke either: that would end the stroke.
            let drawing = self.brush_state.is_drawing || self.brush_state.blend_stroke.is_some();
            if drawing || self.stroke_worker.is_busy() {
                // The frame loop keeps going while the worker paints; look
                // again next frame.
                ctx.request_repaint();
            } else {
                self.release_canvas();
                for f in std::mem::take(&mut self.workspace.jobs.after_strokes) {
                    f(self);
                }
            }
        }
        let mut done = Vec::new();
        self.workspace
            .jobs
            .running
            .retain(|job| match job.result.try_recv() {
                Ok(apply) => {
                    done.push(apply);
                    false
                }
                Err(TryRecvError::Empty) => true,
                // (Only if its thread died before sending anything.)
                Err(TryRecvError::Disconnected) => false,
            });
        for apply in done {
            apply(self);
        }
    }

    /// While a labelled job runs: say what the canvas waits for.
    pub(crate) fn show_job_progress(&self, ctx: &egui::Context) {
        let Some(label) = self.workspace.jobs.blocking_label() else {
            return;
        };
        egui::Area::new(egui::Id::new("job_progress"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(label);
                    });
                });
            });
    }
}

/// A small file to keep (settings, a preset, the library): encoded and
/// written on its own thread, in order, so a slow disk's flush never holds
/// up a frame. A newer write of the same file replaces one not yet started.
/// Errors are logged, with `what` the file is.
pub(crate) fn write_later(
    path: std::path::PathBuf,
    what: &'static str,
    encode: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
) {
    let write = Write {
        path,
        what,
        encode: Box::new(encode),
    };
    // Tests read what was written straight after.
    if cfg!(test) {
        write.run();
        return;
    }
    let sent = writer()
        .lock()
        .ok()
        .is_some_and(|w| w.send(WriterJob::Write(write)).is_ok());
    if !sent {
        log::warn!("Couldn't save the {what}: the file writer stopped");
    }
}

/// Wait until every [`write_later`] so far is on disk (quitting).
pub(crate) fn flush_writes() {
    let (done, wait) = mpsc::channel();
    let sent = writer()
        .lock()
        .ok()
        .is_some_and(|w| w.send(WriterJob::Flush(done)).is_ok());
    if sent {
        let _ = wait.recv();
    }
}

struct Write {
    path: std::path::PathBuf,
    what: &'static str,
    encode: Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>,
}

impl Write {
    fn run(self) {
        let result = (self.encode)().and_then(|bytes| {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            crate::project::write_atomically(&self.path, &bytes)
        });
        if let Err(err) = result {
            log::warn!("Couldn't save the {}: {err}", self.what);
        }
    }
}

enum WriterJob {
    Write(Write),
    Flush(mpsc::Sender<()>),
}

fn writer() -> &'static std::sync::Mutex<mpsc::Sender<WriterJob>> {
    static WRITER: std::sync::OnceLock<std::sync::Mutex<mpsc::Sender<WriterJob>>> =
        std::sync::OnceLock::new();
    WRITER.get_or_init(|| {
        let (send, jobs) = mpsc::channel::<WriterJob>();
        let spawned = std::thread::Builder::new()
            .name("file-writer".into())
            .spawn(move || {
                while let Ok(first) = jobs.recv() {
                    // What's queued meanwhile, the latest write per file.
                    let mut batch = vec![first];
                    for job in jobs.try_iter() {
                        if let WriterJob::Write(w) = &job {
                            batch.retain(
                                |b| !matches!(b, WriterJob::Write(old) if old.path == w.path),
                            );
                        }
                        batch.push(job);
                    }
                    for job in batch {
                        match job {
                            WriterJob::Write(w) => {
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        w.run()
                                    }));
                            }
                            WriterJob::Flush(done) => {
                                let _ = done.send(());
                            }
                        }
                    }
                }
            });
        if let Err(err) = spawned {
            log::error!("Couldn't start the file writer: {err}");
        }
        std::sync::Mutex::new(send)
    })
}

/// What a file dialog picks.
#[cfg(not(mobile))]
#[derive(Clone, Copy)]
pub(crate) enum Pick {
    File,
    Files,
    Save,
}

#[cfg(not(mobile))]
impl PainterApp {
    /// Show `dialog` without holding up the frames (the window would stop
    /// answering while it's open); `then` gets the paths picked, on the UI
    /// thread, unless it was cancelled. One dialog at a time.
    pub(crate) fn file_dialog_job(
        &mut self,
        dialog: rfd::AsyncFileDialog,
        pick: Pick,
        then: impl FnOnce(&mut PainterApp, Vec<std::path::PathBuf>) + Send + 'static,
    ) {
        if self.workspace.jobs.running.iter().any(|j| j.dialog) {
            return;
        }
        self.spawn(None, true, move || {
            let paths: Vec<std::path::PathBuf> = pollster::block_on(async move {
                match pick {
                    Pick::File => dialog.pick_file().await.into_iter().collect(),
                    Pick::Files => dialog.pick_files().await.unwrap_or_default(),
                    Pick::Save => dialog.save_file().await.into_iter().collect(),
                }
            })
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect();
            if let Some(first) = paths.first() {
                crate::app::settings::remember_dir(first);
            }
            Box::new(move |app: &mut PainterApp| {
                if !paths.is_empty() {
                    then(app, paths);
                }
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::history::UndoAction;
    use crate::project::tests::test_app_pub;
    use eframe::egui::Color32;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rp-jobs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn change(app: &mut crate::PainterApp) {
        app.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        });
    }

    #[test]
    fn a_background_save_keeps_the_document_as_it_was_when_saved() {
        let dir = temp_dir("save");
        let mut app = test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        app.canvas_mut()
            .set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        change(&mut app);
        let path = dir.join("doc.rpainter");
        app.save_project_in_background(&path);
        // Painting goes on while it's written.
        app.canvas_mut()
            .set_layer_tile_data(1, 0, 0, vec![Color32::BLUE; 64 * 64]);
        change(&mut app);
        app.run_jobs();
        assert!(app.workspace.jobs.is_idle());
        assert!(app.has_unsaved_work(), "changed since the save");

        let mut opened = test_app_pub(Canvas::new(8, 8, Color32::WHITE, 64));
        opened.open_project_in_background(&path, |_| {});
        opened.run_jobs();
        assert_eq!(opened.canvas.width(), 64);
        let tile = opened.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(tile[0], Color32::RED);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_open_is_reported_and_keeps_the_document() {
        let mut app = test_app_pub(Canvas::new(32, 32, Color32::WHITE, 64));
        app.open_project_in_background("/nonexistent/doc.rpainter", |_| {});
        app.run_jobs();
        assert_eq!(app.canvas.width(), 32);
        assert!(app.export_state.message.is_some());
    }

    #[test]
    fn a_panicking_job_is_reported_not_fatal() {
        let mut app = test_app_pub(Canvas::new(32, 32, Color32::WHITE, 64));
        app.spawn_job(Some("Working…"), || panic!("boom"));
        app.run_jobs();
        assert!(app.workspace.jobs.is_idle());
        assert!(app.export_state.message.is_some());
    }

    #[test]
    fn brushes_imported_in_the_background_are_written_and_listed() {
        let dir = temp_dir("brushes");
        let mut app = test_app_pub(Canvas::new(32, 32, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        app.brush_state.presets = crate::PainterApp::default_brush_presets();
        let existing = app.brush_state.presets[0].clone();
        let bytes =
            crate::brush_engine::preset_file::encode(std::slice::from_ref(&existing)).unwrap();
        let count = app.brush_state.presets.len();
        app.import_brushes_in_background(
            "mine.rpbrush".into(),
            crate::app::import::FileSource::Bytes(bytes.into()),
        );
        app.run_jobs();
        assert_eq!(app.brush_state.presets.len(), count + 1);
        let added = app.brush_state.presets.last().unwrap();
        assert_ne!(added.name, existing.name, "a name of its own");
        assert!(added.file.as_ref().is_some_and(|f| f.exists()), "written");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A 1024 px app with a painted layer 1, active.
    fn painted() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(1024, 1024, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        for ty in 0..16 {
            for tx in 0..16 {
                let tile = (0..64 * 64)
                    .map(|i| {
                        Color32::from_rgb((i % 64 * 4) as u8, (tx * 16) as u8, (ty * 16) as u8)
                    })
                    .collect();
                app.canvas_mut().set_layer_tile_data(1, tx, ty, tile);
            }
        }
        app
    }

    /// Frames until the canvas work is done (as the frame loop does).
    fn frames_until_done(app: &mut crate::PainterApp) -> usize {
        for frame in 1..10_000 {
            app.sync_stroke_worker();
            if app.workspace.jobs.blocking_label().is_none() {
                return frame;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("never done");
    }

    #[test]
    fn a_filter_runs_on_the_worker_without_holding_up_the_frame() {
        use crate::canvas::filters::Filter;
        let mut app = painted();
        app.workspace.jobs.defer = true;
        let before = app.canvas.get_layer_tile_data(1, 5, 5);
        let depth = app.layer_state.history.push_count();
        let started = std::time::Instant::now();
        app.filter_open(Filter::GaussianBlur { radius: 30.0 });
        app.filter_commit();
        let queued = started.elapsed();
        assert!(
            app.workspace.filter.session.is_none(),
            "the dialog closes at once"
        );
        assert!(
            app.workspace.jobs.blocking_label().is_some(),
            "the canvas waits"
        );
        assert!(app.strokes_settling(), "the layer panel and menus wait");
        frames_until_done(&mut app);
        assert!(app.canvas.get_layer_tile_data(1, 5, 5) != before, "blurred");
        assert_eq!(
            app.layer_state.history.push_count(),
            depth + 1,
            "one undo step"
        );
        assert!(queued < std::time::Duration::from_millis(200), "{queued:?}");
        // Undo waits its turn, then puts it back.
        app.apply_history(false);
        frames_until_done(&mut app);
        app.run_jobs();
        assert_eq!(app.canvas.get_layer_tile_data(1, 5, 5), before);
    }

    #[test]
    fn a_merge_and_a_fill_finish_on_the_worker() {
        let mut app = painted();
        app.workspace.jobs.defer = true;
        app.add_layer_and_select();
        app.release_canvas();
        let layers = app.canvas.layers.len();
        app.merge_down();
        assert_eq!(app.canvas.layers.len(), layers, "not yet");
        frames_until_done(&mut app);
        assert_eq!(app.canvas.layers.len(), layers - 1, "merged");

        app.workspace.fill.settings.tolerance = 255;
        app.brush_state.brush.brush_options.color = Color32::from_rgb(1, 2, 3);
        app.fill_press(eframe::egui::Vec2::new(10.0, 10.0));
        frames_until_done(&mut app);
        let tile = app
            .canvas
            .get_layer_tile_data(app.canvas.active_layer_idx, 0, 0)
            .unwrap();
        assert_eq!(tile[0], Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn a_canvas_task_that_panics_is_reported_and_the_canvas_freed() {
        let mut app = painted();
        app.workspace.jobs.defer = true;
        app.run_on_worker("Exploding…", || panic!("boom"));
        frames_until_done(&mut app);
        let message = app.export_state.message.clone().unwrap_or_default();
        assert!(message.contains("Exploding failed: boom"), "{message}");
        // The worker let go of the canvas.
        app.canvas_mut().active_layer_idx = 0;
    }
}
