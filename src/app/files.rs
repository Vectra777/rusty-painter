//! Picking files to open and places to save, on every platform: the
//! desktop's file dialogs answer at once, Android's and iOS's system pickers
//! later (polled each frame), and both end in [`PainterApp::open_picked`] or a
//! write of the bytes waiting to be saved.

use crate::PainterApp;
use crate::app::import::FileSource;
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
    /// Photoshop gradient files (several at once).
    Gradients,
    /// An image whose colours become a palette.
    Palette,
    /// Pictures to add as paper textures (several at once).
    Textures,
    /// A picture for a new canvas's Background layer.
    Paper,
    /// Other apps' palette files (several at once).
    PaletteFiles,
    /// An ICC profile, for what [`ProfileUse`] says.
    Profile(ProfileUse),
    /// A picture to import as a layer (iOS: from Photos; Android has its
    /// own gallery, the desktop its file dialog).
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    Image,
    /// A picture for the reference window (iOS, as [`OpenFor::Image`]).
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    Reference,
    /// A glTF / GLB model for the selected 3D figure layer (a figure or a
    /// prop).
    FigureModel,
    /// A photo to pose the selected figure from (Pose from Photo).
    PosePhoto,
}

/// What a picked ICC profile is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProfileUse {
    /// The document's numbers are its colours.
    Assign,
    /// The document's colours are converted to it.
    Convert,
    /// The monitor's.
    Monitor,
    /// The print (CMYK) profile, for proofing and CMYK export.
    Print,
}

/// A system picker open (Android, iOS), and what its answer is for.
#[cfg_attr(not(mobile), allow(dead_code))]
pub(crate) enum PendingPick {
    Open(OpenFor),
    Save(Vec<u8>),
}

impl OpenFor {
    /// (filter name, lower-case extensions) for the desktop dialog.
    #[cfg_attr(mobile, allow(dead_code))]
    fn filter(&self) -> (&'static str, Vec<&'static str>) {
        let documents = || {
            let mut e = vec!["rpainter"];
            e.extend(crate::project::FOREIGN_EXTENSIONS);
            e
        };
        match self {
            OpenFor::Document | OpenFor::Library(_) => (
                "Rusty Painter, Photoshop, Krita, Procreate or Clip Studio",
                documents(),
            ),
            OpenFor::Brushes => {
                let mut e = vec![crate::brush_engine::preset_file::EXTENSION];
                e.extend(crate::brush_engine::import::EXTENSIONS);
                // ibisPaint brushes: pictures of its QR codes.
                e.extend(crate::brush_engine::import::ibis::IMAGE_EXTENSIONS);
                ("Brushes", e)
            }
            OpenFor::Palette
            | OpenFor::Image
            | OpenFor::Reference
            | OpenFor::Textures
            | OpenFor::Paper
            | OpenFor::PosePhoto => ("Images", vec!["png", "jpg", "jpeg", "bmp", "tif", "tiff"]),
            OpenFor::Profile(_) => ("ICC profiles", vec!["icc", "icm"]),
            OpenFor::Gradients => ("Photoshop gradients", vec!["grd"]),
            OpenFor::FigureModel => ("3D models (glTF)", vec!["glb", "gltf"]),
            OpenFor::PaletteFiles => (
                "Palettes",
                crate::project::palette_files::EXTENSIONS.to_vec(),
            ),
        }
    }

    /// MIME types for the system picker (iOS shows Photos for images).
    /// Our own formats have none, so documents and brushes list every file.
    #[cfg_attr(not(mobile), allow(dead_code))]
    fn mimes(&self) -> &'static [&'static str] {
        match self {
            OpenFor::Palette
            | OpenFor::Image
            | OpenFor::Reference
            | OpenFor::Textures
            | OpenFor::Paper
            | OpenFor::PosePhoto => &["image/*"],
            _ => &["*/*"],
        }
    }
}

impl PainterApp {
    /// Ask for a file (several for brushes) to open for `purpose`. The
    /// dialog doesn't hold up the window, and the file is read and decoded
    /// on another thread.
    #[cfg(not(mobile))]
    pub(crate) fn pick_open(&mut self, purpose: OpenFor) {
        let (label, extensions) = purpose.filter();
        // Upper-case too: file dialogs on Linux match case.
        let upper: Vec<String> = extensions.iter().map(|e| e.to_uppercase()).collect();
        let mut all: Vec<&str> = extensions.clone();
        all.extend(upper.iter().map(String::as_str));
        let dialog = crate::app::settings::file_dialog().add_filter(label, &all);
        let pick = if matches!(
            purpose,
            OpenFor::Brushes | OpenFor::Gradients | OpenFor::PaletteFiles | OpenFor::Textures
        ) {
            crate::app::jobs::Pick::Files
        } else {
            crate::app::jobs::Pick::File
        };
        self.file_dialog_job(dialog, pick, move |app, paths| {
            for path in paths {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                app.open_picked(&purpose, name, FileSource::Path(path.clone()), Some(path));
            }
        });
    }

    #[cfg(mobile)]
    pub(crate) fn pick_open(&mut self, purpose: OpenFor) {
        let multiple = matches!(
            purpose,
            OpenFor::Brushes | OpenFor::Gradients | OpenFor::PaletteFiles | OpenFor::Textures
        );
        match crate::platform::picker_open(purpose.mimes(), multiple) {
            Ok(()) => self.workspace.file_pick = Some(PendingPick::Open(purpose)),
            Err(err) => self.report(err),
        }
    }

    /// Ask where to save `bytes` as `name` (extension `ext`), and write them
    /// there (the dialog and the write on other threads).
    #[cfg(not(mobile))]
    pub(crate) fn pick_save(&mut self, name: &str, ext: &str, _mime: &str, bytes: Vec<u8>) {
        let dialog = crate::app::settings::file_dialog()
            .add_filter(ext, &[ext])
            .set_file_name(name);
        self.file_dialog_job(dialog, crate::app::jobs::Pick::Save, move |app, paths| {
            let path = paths[0].clone();
            app.spawn_job(None, move || {
                let result = crate::project::write_atomically(&path, &bytes)
                    .map_err(|err| format!("Couldn't write {}: {err}", path.display()));
                Box::new(move |app: &mut PainterApp| {
                    if let Err(err) = result {
                        app.report(err);
                    }
                })
            });
        });
    }

    #[cfg(mobile)]
    pub(crate) fn pick_save(&mut self, name: &str, _ext: &str, mime: &str, bytes: Vec<u8>) {
        match crate::platform::picker_create(mime, name) {
            Ok(()) => self.workspace.file_pick = Some(PendingPick::Save(bytes)),
            Err(err) => self.report(err),
        }
    }

    /// Once a frame: act on the system picker's answer when it comes.
    #[cfg(mobile)]
    pub(crate) fn poll_file_pick(&mut self) {
        if self.workspace.file_pick.is_none() {
            return;
        }
        let Some(uris) = crate::platform::picker_poll() else {
            return;
        };
        match self.workspace.file_pick.take() {
            Some(PendingPick::Open(purpose)) => {
                for uri in uris {
                    // Opened here, read on a job's thread.
                    match crate::platform::picker_read(&uri) {
                        Ok((name, source)) => self.open_picked(&purpose, name, source, None),
                        Err(err) => self.report(err),
                    }
                }
            }
            Some(PendingPick::Save(bytes)) => {
                if let Some(uri) = uris.first() {
                    self.write_picked(uri, bytes);
                }
            }
            None => {}
        }
    }

    /// Write `bytes` into the file the picker created at `uri`: opened
    /// here, written on another thread.
    #[cfg(target_os = "android")]
    fn write_picked(&mut self, uri: &str, bytes: Vec<u8>) {
        let mut file = match crate::android::picker_open_file(uri) {
            Ok(file) => file,
            Err(err) => return self.report(err),
        };
        self.spawn_job(None, move || {
            use std::io::Write;
            let result = file.write_all(&bytes);
            // (A provider's descriptor may be a pipe, which can't sync.)
            let _ = file.sync_all();
            Box::new(move |app: &mut PainterApp| match result {
                Ok(()) => app.report("Saved".to_string()),
                Err(err) => app.report(format!("Couldn't write the file: {err}")),
            })
        });
    }

    /// iOS: the save is written to where [`crate::platform::picker_create`]
    /// said, then the picker opens, and says itself when the file is in
    /// place.
    #[cfg(target_os = "ios")]
    fn write_picked(&mut self, uri: &str, bytes: Vec<u8>) {
        if let Err(err) = crate::platform::picker_write(uri, &bytes) {
            self.report(err);
        }
    }

    #[cfg(not(mobile))]
    pub(crate) fn poll_file_pick(&mut self) {}

    /// Use a picked file (`name`, its bytes from `source`, its `path` where
    /// files have one) as `purpose` says: read and decoded on another
    /// thread, then taken in.
    pub(crate) fn open_picked(
        &mut self,
        purpose: &OpenFor,
        name: String,
        source: FileSource,
        path: Option<PathBuf>,
    ) {
        match purpose {
            OpenFor::Document | OpenFor::Library(_) => {
                let purpose = purpose.clone();
                self.spawn_job(Some("Opening…"), move || {
                    let doc = source
                        .read()
                        .and_then(|bytes| crate::project::decode_document(&name, &bytes));
                    Box::new(move |app: &mut PainterApp| match doc {
                        // The old document goes once the stroke worker lets
                        // go of it.
                        Ok(doc) => app.when_strokes_painted(move |app| {
                            if let Err(err) = app.take_picked_document(&purpose, &name, doc, path) {
                                app.report(err);
                            }
                        }),
                        Err(err) => app.report(err),
                    })
                });
            }
            OpenFor::Brushes => self.import_brushes_in_background(name, source),
            OpenFor::Gradients => self.import_gradients_in_background(name, source),
            OpenFor::FigureModel => self.import_figure_model_in_background(name, source),
            OpenFor::PosePhoto => self.pose_photo_in_background(name, source),
            OpenFor::PaletteFiles => self.import_palette_in_background(name, source),
            OpenFor::Palette => {
                let stem = std::path::Path::new(&name)
                    .file_stem()
                    .map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned());
                self.palette_from_image_in_background(stem, source);
            }
            OpenFor::Image => {
                let stem = std::path::Path::new(&name)
                    .file_stem()
                    .map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned());
                self.import_image_in_background(stem, source);
            }
            OpenFor::Reference => self.open_reference_in_background(name, source),
            OpenFor::Paper => self.spawn_job(None, move || {
                let image = (source.read())
                    .and_then(|bytes| crate::app::import::decode_image(&bytes))
                    .map_err(|e| format!("{name}: {e}"));
                Box::new(move |app: &mut PainterApp| match image {
                    Ok(image) => {
                        let name = std::path::Path::new(&name)
                            .file_stem()
                            .map_or_else(|| "Picture".into(), |s| s.to_string_lossy().into_owned());
                        app.modal_state.new_canvas.paper = crate::app::document::Paper::Picture {
                            name,
                            image: std::sync::Arc::new(image),
                        };
                    }
                    Err(err) => app.report(err),
                })
            }),
            OpenFor::Textures => self.spawn_job(None, move || {
                let bytes = source.read().map(std::borrow::Cow::into_owned);
                Box::new(move |app: &mut PainterApp| match bytes {
                    Ok(bytes) => app.add_texture(&name, &bytes),
                    Err(err) => app.report(err),
                })
            }),
            OpenFor::Profile(target) => {
                let target = *target;
                let stem = std::path::Path::new(&name).file_stem().map_or_else(
                    || "ICC profile".into(),
                    |s| s.to_string_lossy().into_owned(),
                );
                self.spawn_job(None, move || {
                    let bytes = source.read().map(std::borrow::Cow::into_owned);
                    Box::new(move |app: &mut PainterApp| match bytes {
                        Ok(bytes) => app.use_profile(target, bytes, &stem),
                        Err(err) => app.report(err),
                    })
                });
            }
        }
    }

    /// A picked document, decoded: the document now (and, for the library,
    /// one of its projects).
    fn take_picked_document(
        &mut self,
        purpose: &OpenFor,
        name: &str,
        doc: crate::project::OpenedDocument,
        path: Option<PathBuf>,
    ) -> Result<(), String> {
        self.leave_document();
        self.open_document(doc);
        self.set_title_from(name);
        match purpose {
            OpenFor::Library(folder) => {
                let stem = std::path::Path::new(name)
                    .file_stem()
                    .map_or_else(|| "Imported".into(), |s| s.to_string_lossy().into_owned());
                let path = crate::project::with_project_extension(
                    &crate::ui::library::unused_path(folder, &stem),
                );
                self.workspace.library.project = Some(path.clone());
                self.save_into_library(path);
            }
            // Saving goes back to where it came from, if it was ours.
            _ => {
                self.workspace.library.project = path.filter(|p| crate::ui::library::is_project(p));
                self.workspace.library.open = false;
            }
        }
        Ok(())
    }

    /// Save the document just imported as the library's `path`: encoded
    /// and written on another thread, then listed in the library.
    fn save_into_library(&mut self, path: PathBuf) {
        // As a save copies it: every pixel painted, no quick mask, shader
        // layers at their frame.
        self.settle_vectors();
        self.quick_mask_leave();
        self.bake_shader_layers();
        let snapshot = crate::project::ProjectSnapshot::capture(self);
        let version = self.doc_version();
        self.spawn_job(Some("Saving…"), move || {
            let result = snapshot.write(&path);
            Box::new(move |app: &mut PainterApp| {
                match result {
                    Ok(()) => app.saved_by_user_at(version),
                    Err(err) => {
                        app.report(format!("Save failed: {err}"));
                        // Not written: the document isn't that project.
                        let project = &mut app.workspace.library.project;
                        if project.as_ref() == Some(&path) {
                            *project = None;
                        }
                    }
                }
                app.workspace.library.refresh();
            })
        });
    }

    /// Show `msg` to the user (and log it).
    pub(crate) fn report(&mut self, msg: String) {
        log::info!("{msg}");
        self.export_state.message = Some(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use eframe::egui::Color32;

    #[test]
    fn a_document_imported_into_the_library_is_saved_there_in_the_background() {
        let dir = std::env::temp_dir().join(format!("rp-files-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let picked = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::RED, 64));
        let bytes = crate::project::encode_project(&picked).unwrap();
        let mut app = crate::project::tests::test_app_pub(Canvas::new(32, 32, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        let source = FileSource::Bytes(bytes.into());
        let purpose = OpenFor::Library(dir.clone());
        app.open_picked(&purpose, "Mine.rpainter".into(), source, None);
        app.run_jobs();
        let path = dir.join("Mine.rpainter");
        assert_eq!(
            app.workspace.library.project.as_deref(),
            Some(path.as_path())
        );
        assert!(path.exists(), "written");
        assert!(!app.has_unsaved_work(), "saved");
        assert_eq!(app.canvas.width(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
