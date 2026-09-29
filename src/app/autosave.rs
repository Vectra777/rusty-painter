//! Autosave and crash recovery. Unsaved work is written to
//! `autosave.rpainter` (beside the brushes folder) once the app has been
//! idle for a moment, at most once a minute. Quitting with unsaved work
//! keeps that file, and so does a crash; the next start offers to recover
//! it. Saving, or quitting with nothing unsaved, deletes it.
//!
//! "Changed" means the undo history moved (a push, undo or redo); layer
//! settings that aren't undoable (visibility, opacity) don't count on
//! their own.

use crate::PainterApp;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Quiet time before an autosave, so the pause is never felt mid-work.
const IDLE: Duration = Duration::from_secs(2);
/// At most one autosave per this long.
const EVERY: Duration = Duration::from_secs(60);

/// Where the undo history stood: pushes, undo depth, redo depth.
type Version = (u64, usize, usize);

pub struct AutosaveState {
    /// The version last saved (or opened) by the user.
    saved: Version,
    /// The version last written to the autosave file.
    autosaved: Version,
    last_activity: Instant,
    last_write: Option<Instant>,
    /// A file left by the last session, offered for recovery.
    pub recovery: Option<PathBuf>,
}

impl AutosaveState {
    /// Checks for a file left by the last session.
    pub fn new(path: &std::path::Path) -> Self {
        Self {
            saved: (0, 0, 0),
            autosaved: (0, 0, 0),
            last_activity: Instant::now(),
            last_write: None,
            recovery: path.exists().then(|| path.to_path_buf()),
        }
    }
}

impl PainterApp {
    pub(crate) fn autosave_path(&self) -> PathBuf {
        self.brush_state
            .brushes_path
            .with_file_name("autosave.rpainter")
    }

    pub(crate) fn doc_version(&self) -> Version {
        let history = &self.layer_state.history;
        let (undo, redo) = history.stacks();
        (history.push_count(), undo.len(), redo.len())
    }

    /// Whether there is work the user hasn't saved.
    pub(crate) fn has_unsaved_work(&self) -> bool {
        self.doc_version() != self.workspace.autosave.saved
    }

    /// The document now matches what's on disk (just opened or created):
    /// no autosave is needed until it changes.
    pub(crate) fn mark_saved(&mut self) {
        let version = self.doc_version();
        let state = &mut self.workspace.autosave;
        state.saved = version;
        state.autosaved = version;
    }

    /// The user saved: the autosave has nothing more to keep.
    pub(crate) fn saved_by_user(&mut self) {
        self.mark_saved();
        if self.workspace.autosave.recovery.is_none() {
            let _ = std::fs::remove_file(self.autosave_path());
        }
    }

    /// The document has changes nothing on disk holds (a recovered file).
    pub(crate) fn mark_unsaved(&mut self) {
        self.workspace.autosave.saved = (u64::MAX, 0, 0);
        self.workspace.autosave.autosaved = self.doc_version();
    }

    /// Once a frame: autosave when there are changes and the app is idle.
    pub(crate) fn autosave_tick(&mut self, ctx: &eframe::egui::Context) {
        let now = Instant::now();
        let active = ctx.input(|i| !i.events.is_empty() || i.pointer.any_down());
        let state = &mut self.workspace.autosave;
        if active {
            state.last_activity = now;
        }
        // Not while a recovery offer is open: the file would be overwritten.
        if state.recovery.is_some() || self.doc_version() == self.workspace.autosave.autosaved {
            return;
        }
        let state = &self.workspace.autosave;
        let due = state.last_activity + IDLE;
        let due = state.last_write.map_or(due, |w| due.max(w + EVERY));
        if now < due {
            // Idle apps don't repaint: wake up when it's due.
            ctx.request_repaint_after(due - now);
            return;
        }
        if self.brush_state.is_drawing || self.session_running() {
            return;
        }
        self.write_autosave();
    }

    /// A tool session is previewing pixels that aren't applied yet.
    fn session_running(&self) -> bool {
        let ws = &self.workspace;
        ws.filter.session.is_some()
            || ws.text.session.is_some()
            || ws.gradient.session.is_some()
            || ws.shapes.session.is_some()
            || self.layer_state.liquify.is_some()
            || self.layer_state.floating_layer_idx.is_some()
            || ws.select.quick_mask.is_some()
    }

    fn write_autosave(&mut self) {
        self.release_canvas();
        let path = self.autosave_path();
        // Written beside, then renamed: a crash mid-write keeps the last one.
        let tmp = path.with_extension("rpainter.tmp");
        let result = crate::project::encode_project(self)
            .and_then(|bytes| std::fs::write(&tmp, bytes).map_err(|e| e.to_string()))
            .and_then(|()| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()));
        if let Err(err) = result {
            log::error!("Autosave failed: {err}");
        }
        let version = self.doc_version();
        let state = &mut self.workspace.autosave;
        state.autosaved = version;
        state.last_write = Some(Instant::now());
    }

    /// Quitting: keep unsaved work for the next start, else tidy up.
    pub(crate) fn autosave_on_exit(&mut self) {
        if self.workspace.autosave.recovery.is_some() {
            return; // left as it was: still on offer next time
        }
        if self.has_unsaved_work() {
            if self.doc_version() != self.workspace.autosave.autosaved {
                self.write_autosave();
            }
        } else {
            let _ = std::fs::remove_file(self.autosave_path());
        }
    }

    /// Open the file left by the last session as the document.
    pub(crate) fn recover_autosave(&mut self) {
        let Some(path) = self.workspace.autosave.recovery.take() else {
            return;
        };
        match self.load_project_from_path(&path) {
            Ok(()) => self.mark_unsaved(),
            Err(err) => {
                log::error!("{err}");
                self.export_state.message = Some(format!("Couldn't recover the work: {err}"));
            }
        }
    }

    /// Drop the file left by the last session.
    pub(crate) fn discard_autosave(&mut self) {
        if let Some(path) = self.workspace.autosave.recovery.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The recovery offer shown at startup.
pub fn recovery_dialog(app: &mut PainterApp, ctx: &eframe::egui::Context) {
    use eframe::egui;
    let Some(path) = app.workspace.autosave.recovery.as_ref() else {
        return;
    };
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| match d.as_secs() / 60 {
            0 => "less than a minute ago".to_string(),
            1 => "a minute ago".to_string(),
            m if m < 120 => format!("{m} minutes ago"),
            m if m < 48 * 60 => format!("{} hours ago", m / 60),
            m => format!("{} days ago", m / (24 * 60)),
        })
        .unwrap_or_default();
    let (mut recover, mut discard) = (false, false);
    egui::Window::new("Recover unsaved work?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!(
                "Rusty Painter closed with work that wasn't saved (kept {age})."
            ));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                recover = ui.button("Recover").clicked();
                discard = ui.button("Discard").clicked();
            });
        });
    if recover {
        app.recover_autosave();
    } else if discard {
        app.discard_autosave();
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::history::UndoAction;
    use eframe::egui::Color32;

    fn app(dir: &std::path::Path) -> crate::PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        app.workspace.autosave.recovery = None;
        app.mark_saved();
        app
    }

    fn change(app: &mut crate::PainterApp) {
        app.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        });
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rp-autosave-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unsaved_work_survives_quitting_and_is_offered_again() {
        let dir = temp_dir("quit");
        let mut app = app(&dir);
        assert!(!app.has_unsaved_work());
        change(&mut app);
        assert!(app.has_unsaved_work());
        app.autosave_on_exit();
        let path = app.autosave_path();
        assert!(path.exists(), "kept for next time");

        let mut next = self::app(&dir);
        next.workspace.autosave = super::AutosaveState::new(&path);
        assert!(next.workspace.autosave.recovery.is_some(), "offered");
        next.recover_autosave();
        assert!(
            next.has_unsaved_work(),
            "recovered work isn't saved anywhere yet"
        );
        next.saved_by_user();
        assert!(!path.exists(), "saving tidies it up");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quitting_with_nothing_unsaved_leaves_no_file() {
        let dir = temp_dir("clean");
        let mut app = app(&dir);
        change(&mut app);
        app.autosave_on_exit();
        app.mark_saved();
        app.autosave_on_exit();
        assert!(!app.autosave_path().exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn undo_counts_as_a_change() {
        let dir = temp_dir("undo");
        let mut app = app(&dir);
        change(&mut app);
        app.mark_saved();
        app.apply_history(false);
        assert!(app.has_unsaved_work());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod timing {
    use crate::canvas::Canvas;
    use eframe::egui::Color32;

    #[test]
    #[ignore = "timing; run with --release --ignored --nocapture"]
    fn autosave_4k() {
        let app = crate::project::tests::test_app_pub(Canvas::new(4000, 4000, Color32::WHITE, 64));
        for ty in 0..63 {
            for tx in 0..63 {
                let data = (0..64 * 64)
                    .map(|i| Color32::from_gray(((i * 7 + tx * 13 + ty) % 256) as u8))
                    .collect();
                app.canvas.set_layer_tile_data(1, tx, ty, data);
            }
        }
        let t = std::time::Instant::now();
        let bytes = crate::project::encode_project(&app).unwrap();
        println!("encode: {:?}, {} MB", t.elapsed(), bytes.len() / 1_000_000);
    }
}
