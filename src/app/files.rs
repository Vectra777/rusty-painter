//! Picking files to open and places to save, on every platform: the
//! desktop's file dialogs answer at once, Android's system picker later
//! (polled each frame), and both end in [`PainterApp::open_picked`] or a
//! write of the bytes waiting to be saved.

use crate::PainterApp;
use std::path::PathBuf;

/// What a picked file is for.
#[derive(Clone, Debug)]
pub(crate) enum OpenFor {
    /// A project (ours, Photoshop, Krita, Clip Studio) to work on.
    Document,
    /// A document to copy into this project library folder.
    Library(PathBuf),
    /// Brush files (several at once).
    Brushes,
    /// An image whose colours become a palette.
    Palette,
}

/// A file picked to open: its name, contents, and its path where files have
/// one (desktop).
pub(crate) struct Picked {
    pub name: String,
    pub bytes: Vec<u8>,
    pub path: Option<PathBuf>,
}

/// A picker open on Android, and what its answer is for.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) enum PendingPick {
    Open(OpenFor),
    Save(Vec<u8>),
}

impl OpenFor {
    /// (filter name, lower-case extensions) for the desktop dialog.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    fn filter(&self) -> (&'static str, Vec<&'static str>) {
        let documents = || {
            let mut e = vec!["rpainter"];
            e.extend(crate::project::FOREIGN_EXTENSIONS);
            e
        };
        match self {
            OpenFor::Document | OpenFor::Library(_) => (
                "Rusty Painter, Photoshop, Krita or Clip Studio",
                documents(),
            ),
            OpenFor::Brushes => {
                let mut e = vec![crate::brush_engine::preset_file::EXTENSION];
                e.extend(crate::brush_engine::import::EXTENSIONS);
                ("Brushes", e)
            }
            OpenFor::Palette => ("Images", vec!["png", "jpg", "jpeg", "bmp", "tif", "tiff"]),
        }
    }

    /// MIME types for Android's picker. Our own formats have none, so
    /// documents and brushes list every file.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn mimes(&self) -> &'static [&'static str] {
        match self {
            OpenFor::Palette => &["image/*"],
            _ => &["*/*"],
        }
    }
}

impl PainterApp {
    /// Ask for a file (several for brushes) to open for `purpose`.
    #[cfg(not(target_os = "android"))]
    pub(crate) fn pick_open(&mut self, purpose: OpenFor) {
        let (label, extensions) = purpose.filter();
        // Upper-case too: file dialogs on Linux match case.
        let upper: Vec<String> = extensions.iter().map(|e| e.to_uppercase()).collect();
        let mut all: Vec<&str> = extensions.clone();
        all.extend(upper.iter().map(String::as_str));
        let dialog = crate::app::settings::file_dialog().add_filter(label, &all);
        let paths = if matches!(purpose, OpenFor::Brushes) {
            dialog.pick_files().unwrap_or_default()
        } else {
            dialog.pick_file().into_iter().collect()
        };
        if let Some(first) = paths.first() {
            crate::app::settings::remember_dir(first);
        }
        for path in paths {
            let picked = std::fs::read(&path)
                .map_err(|e| format!("Couldn't read {}: {e}", path.display()))
                .map(|bytes| Picked {
                    name: path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    bytes,
                    path: Some(path.clone()),
                });
            self.open_picked_or_report(&purpose, picked);
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn pick_open(&mut self, purpose: OpenFor) {
        let multiple = matches!(purpose, OpenFor::Brushes);
        match crate::android::picker_open(purpose.mimes(), multiple) {
            Ok(()) => self.workspace.file_pick = Some(PendingPick::Open(purpose)),
            Err(err) => self.report(err),
        }
    }

    /// Ask where to save `bytes` as `name` (extension `ext`), and write them
    /// there.
    #[cfg(not(target_os = "android"))]
    pub(crate) fn pick_save(&mut self, name: &str, ext: &str, _mime: &str, bytes: Vec<u8>) {
        let Some(path) = crate::app::settings::file_dialog()
            .add_filter(ext, &[ext])
            .set_file_name(name)
            .save_file()
            .inspect(|p| crate::app::settings::remember_dir(p))
        else {
            return;
        };
        if let Err(err) = crate::project::write_atomically(&path, &bytes) {
            self.report(format!("Couldn't write {}: {err}", path.display()));
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn pick_save(&mut self, name: &str, _ext: &str, mime: &str, bytes: Vec<u8>) {
        match crate::android::picker_create(mime, name) {
            Ok(()) => self.workspace.file_pick = Some(PendingPick::Save(bytes)),
            Err(err) => self.report(err),
        }
    }

    /// Once a frame: act on the Android picker's answer when it comes.
    #[cfg(target_os = "android")]
    pub(crate) fn poll_file_pick(&mut self) {
        if self.workspace.file_pick.is_none() {
            return;
        }
        let Some(uris) = crate::android::picker_poll() else {
            return;
        };
        match self.workspace.file_pick.take() {
            Some(PendingPick::Open(purpose)) => {
                for uri in uris {
                    let picked = crate::android::picker_read(&uri).map(|(name, bytes)| Picked {
                        name,
                        bytes,
                        path: None,
                    });
                    self.open_picked_or_report(&purpose, picked);
                }
            }
            Some(PendingPick::Save(bytes)) => {
                if let Some(uri) = uris.first() {
                    match crate::android::picker_write(uri, &bytes) {
                        Ok(()) => self.report("Saved".to_string()),
                        Err(err) => self.report(err),
                    }
                }
            }
            None => {}
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn poll_file_pick(&mut self) {}

    fn open_picked_or_report(&mut self, purpose: &OpenFor, picked: Result<Picked, String>) {
        if let Err(err) = picked.and_then(|p| self.open_picked(purpose, p)) {
            self.report(err);
        }
    }

    /// Use a picked file as `purpose` says.
    pub(crate) fn open_picked(&mut self, purpose: &OpenFor, picked: Picked) -> Result<(), String> {
        match purpose {
            OpenFor::Document => {
                self.leave_document();
                self.load_project_bytes(&picked.name, &picked.bytes)?;
                // Saving goes back to where it came from, if it was ours.
                self.workspace.library.project =
                    picked.path.filter(|p| crate::ui::library::is_project(p));
                Ok(())
            }
            OpenFor::Library(folder) => {
                self.leave_document();
                self.load_project_bytes(&picked.name, &picked.bytes)?;
                let stem = std::path::Path::new(&picked.name)
                    .file_stem()
                    .map_or_else(|| "Imported".into(), |s| s.to_string_lossy().into_owned());
                let path = crate::ui::library::unused_path(folder, &stem);
                self.save_project_to_path(&path)?;
                self.workspace.library.project = Some(path);
                self.workspace.library.refresh();
                Ok(())
            }
            OpenFor::Brushes => self
                .import_brushes_bytes(&picked.name, &picked.bytes)
                .map(|_| ()),
            OpenFor::Palette => {
                let name = std::path::Path::new(&picked.name)
                    .file_stem()
                    .map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned());
                self.extract_palette_from_image(&name, &picked.bytes)
            }
        }
    }

    /// Show `msg` to the user (and log it).
    pub(crate) fn report(&mut self, msg: String) {
        log::info!("{msg}");
        self.export_state.message = Some(msg);
    }
}
