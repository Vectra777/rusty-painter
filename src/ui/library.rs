//! The project library: projects kept in the app's own folder, sorted into
//! folders, each shown with its thumbnail. Android starts here (it has no
//! file system to save to); the desktop opens it from File → Projects.

use crate::PainterApp;
use crate::app::files::OpenFor;
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::{ACCENT, BG_CANVAS, BG_RAISED, TEXT_DIM, metrics};
use eframe::egui::{self, RichText};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

const EXTENSION: &str = "rpainter";

struct Entry {
    path: PathBuf,
    name: String,
    folder: bool,
    modified: SystemTime,
}

/// Work to do once unsaved work is dealt with.
#[derive(Clone)]
enum Leave {
    Open(PathBuf),
    New,
    Import,
}

enum Dialog {
    NewFolder(String),
    Rename(PathBuf, String),
    Delete(PathBuf),
    /// Name the document to save it into the shown folder.
    SaveAs(String),
    /// The document has unsaved work outside the library.
    Unsaved(Leave),
}

type Thumb = (PathBuf, SystemTime, Option<egui::ColorImage>);

pub struct LibraryState {
    pub open: bool,
    /// The file the document saves back to (in the library or, on the
    /// desktop, anywhere); `None` until it's first saved.
    pub project: Option<PathBuf>,
    /// The folder shown.
    folder: PathBuf,
    /// Its contents; `None` until listed again.
    entries: Option<Vec<Entry>>,
    thumbs: HashMap<PathBuf, (SystemTime, Option<egui::TextureHandle>)>,
    tx: Sender<Thumb>,
    rx: Receiver<Thumb>,
    dialog: Option<Dialog>,
}

impl Default for LibraryState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            // Android has nowhere else to keep work: start in the library.
            open: cfg!(target_os = "android"),
            project: None,
            folder: root(),
            entries: None,
            thumbs: HashMap::new(),
            tx,
            rx,
            dialog: None,
        }
    }
}

impl LibraryState {
    /// List the folder again (its contents changed).
    pub fn refresh(&mut self) {
        self.entries = None;
    }

    /// Ask for a name and save the document into the shown folder.
    pub fn ask_save_name(&mut self, name: &str) {
        self.dialog = Some(Dialog::SaveAs(name.to_string()));
    }
}

/// Where the library keeps its projects.
pub(crate) fn root() -> PathBuf {
    crate::app::init::data_dir().join("projects")
}

/// Whether `path` is one of our project files (not the autosave).
pub(crate) fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
        && path.file_name().is_some_and(|n| n != "autosave.rpainter")
}

/// A file name made of `name`: characters file systems refuse replaced.
fn file_stem(name: &str) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.').to_string();
    if clean.is_empty() {
        "Untitled".into()
    } else {
        clean
    }
}

/// `folder/name.rpainter`, numbered if taken.
pub(crate) fn unused_path(folder: &Path, name: &str) -> PathBuf {
    let stem = file_stem(name);
    (1..)
        .map(|n| match n {
            1 => folder.join(format!("{stem}.{EXTENSION}")),
            n => folder.join(format!("{stem} {n}.{EXTENSION}")),
        })
        .find(|p| !p.exists())
        .expect("some number is free")
}

/// `folder/name`, numbered if taken.
fn unused_folder(parent: &Path, name: &str) -> PathBuf {
    let stem = file_stem(name);
    (1..)
        .map(|n| match n {
            1 => parent.join(&stem),
            n => parent.join(format!("{stem} {n}")),
        })
        .find(|p| !p.exists())
        .expect("some number is free")
}

fn list(folder: &Path) -> Vec<Entry> {
    let _ = std::fs::create_dir_all(folder);
    let mut entries: Vec<Entry> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let meta = e.metadata().ok()?;
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let folder = meta.is_dir();
            // Hidden files: half-written saves.
            if name.starts_with('.') || !(folder || is_project(&path)) {
                return None;
            }
            let name = if folder {
                path.file_name()?.to_string_lossy().into_owned()
            } else {
                name
            };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Some(Entry {
                path,
                name,
                folder,
                modified,
            })
        })
        .collect();
    // Folders first by name, then the latest work first.
    entries.sort_by(|a, b| {
        b.folder.cmp(&a.folder).then_with(|| {
            if a.folder {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            } else {
                b.modified.cmp(&a.modified)
            }
        })
    });
    entries
}

/// Every folder of the library, for "Move to".
fn all_folders(dir: &Path, out: &mut Vec<PathBuf>) {
    out.push(dir.to_path_buf());
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subs.sort();
    for sub in subs {
        all_folders(&sub, out);
    }
}

/// "5 minutes ago" and so on.
pub(crate) fn ago(time: SystemTime) -> String {
    let Ok(age) = time.elapsed() else {
        return "just now".into();
    };
    match age.as_secs() / 60 {
        0 => "less than a minute ago".to_string(),
        1 => "a minute ago".to_string(),
        m if m < 120 => format!("{m} minutes ago"),
        m if m < 48 * 60 => format!("{} hours ago", m / 60),
        m => format!("{} days ago", m / (24 * 60)),
    }
}

/// `path` shown from the library's root: "Projects / Comics / Ch. 1".
fn relative_name(path: &Path) -> String {
    let rel = path.strip_prefix(root()).unwrap_or(path);
    std::iter::once("Projects".to_string())
        .chain(rel.iter().map(|c| c.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(" / ")
}

impl PainterApp {
    /// Before another document replaces this one: a library project with
    /// changes is saved back.
    pub(crate) fn leave_document(&mut self) {
        if let Some(path) = self.workspace.library.project.clone()
            && self.has_unsaved_work()
            && let Err(err) = self.save_project_to_path(&path)
        {
            self.report(err);
        }
    }

    /// Show the library, the document saved first if it's one of its own.
    pub(crate) fn open_library(&mut self) {
        self.leave_document();
        let lib = &mut self.workspace.library;
        if !lib.folder.starts_with(root()) || !lib.folder.is_dir() {
            lib.folder = root();
        }
        lib.open = true;
        lib.refresh();
    }

    /// Save the document as `name` in the library folder shown.
    fn save_to_library(&mut self, name: &str) -> Result<(), String> {
        let path = unused_path(&self.workspace.library.folder.clone(), name);
        let _ = std::fs::create_dir_all(&self.workspace.library.folder);
        self.save_project_to_path(&path)?;
        self.workspace.library.project = Some(path);
        self.workspace.library.refresh();
        Ok(())
    }

    /// Create the canvas set in the New Canvas dialog. From the library,
    /// and always on Android, it's a project of the library at once.
    pub(crate) fn create_new_canvas(&mut self) {
        self.leave_document();
        self.apply_new_canvas();
        self.workspace.library.project = None;
        if self.workspace.library.open || cfg!(target_os = "android") {
            let name = self.modal_state.new_canvas.name.clone();
            if let Err(err) = self.save_to_library(&name) {
                self.report(err);
            }
            self.workspace.library.open = false;
        }
    }

    fn do_leave(&mut self, leave: Leave) {
        match leave {
            Leave::Open(path) => {
                self.leave_document();
                match self.load_project_from_path(&path) {
                    Ok(()) => self.workspace.library.open = false,
                    Err(err) => self.report(err),
                }
            }
            Leave::New => crate::ui::menus::open_new_canvas_dialog(self),
            Leave::Import => {
                let folder = self.workspace.library.folder.clone();
                self.pick_open(OpenFor::Library(folder));
            }
        }
    }

    /// `leave`, after asking about unsaved work that has no file.
    fn leave_asking(&mut self, leave: Leave) {
        if self.workspace.library.project.is_none() && self.has_unsaved_work() {
            self.workspace.library.dialog = Some(Dialog::Unsaved(leave));
        } else {
            self.do_leave(leave);
        }
    }

    /// `from` (a project or folder) renamed or moved to `to`: the document
    /// follows if it was there.
    fn moved(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        std::fs::rename(from, to).map_err(|e| format!("Couldn't move {}: {e}", from.display()))?;
        let lib = &mut self.workspace.library;
        if let Some(p) = &lib.project
            && let Ok(rest) = p.strip_prefix(from)
        {
            lib.project = Some(to.join(rest));
        }
        // On screen this frame (see `retired_textures`).
        let old = lib.thumbs.remove(from).and_then(|(_, t)| t);
        lib.refresh();
        self.workspace.retired_textures.extend(old);
        Ok(())
    }

    fn delete_entry(&mut self, path: &Path) -> Result<(), String> {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        result.map_err(|e| format!("Couldn't delete {}: {e}", path.display()))?;
        let lib = &mut self.workspace.library;
        if lib.project.as_ref().is_some_and(|p| p.starts_with(path)) {
            lib.project = None;
        }
        lib.refresh();
        Ok(())
    }
}

/// What a tile's menu asked for.
enum Act {
    Open(PathBuf),
    Enter(PathBuf),
    Rename(PathBuf, String),
    Duplicate(PathBuf),
    MoveTo(PathBuf, PathBuf),
    Export(PathBuf),
    Delete(PathBuf),
}

/// The library, filling the window.
pub fn library_screen(app: &mut PainterApp, ctx: &egui::Context) {
    let ws = &mut app.workspace;
    let lib = &mut ws.library;
    if lib.entries.is_none() {
        lib.entries = Some(list(&lib.folder));
        load_thumbnails(lib, ctx);
    }
    while let Ok((path, modified, img)) = lib.rx.try_recv() {
        let tex = img.map(|img| {
            ctx.load_texture(
                format!("library-{}", path.display()),
                img,
                egui::TextureOptions::LINEAR,
            )
        });
        // Two loads of one picture can arrive together: the first one's
        // upload can't be freed in the same frame (see `retired_textures`).
        let old = lib.thumbs.insert(path, (modified, tex));
        ws.retired_textures.extend(old.and_then(|(_, t)| t));
    }

    let m = metrics(ctx);
    let mut act = None;
    let mut leave = None;
    let mut up = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(BG_CANVAS).inner_margin(12.0))
        .show(ctx, |ui| {
            let lib = &mut app.workspace.library;
            ui.horizontal_wrapped(|ui| {
                // Breadcrumbs: each folder up the path goes back there.
                let rel: Vec<PathBuf> = lib
                    .folder
                    .strip_prefix(root())
                    .map(|r| r.iter().map(PathBuf::from).collect())
                    .unwrap_or_default();
                if ui
                    .add(egui::Button::new(RichText::new("Projects").heading()).frame(false))
                    .clicked()
                {
                    up = Some(root());
                }
                let mut at = root();
                for part in rel {
                    at.push(&part);
                    ui.label(RichText::new("/").heading().color(TEXT_DIM));
                    let target = at.clone();
                    if ui
                        .add(
                            egui::Button::new(RichText::new(part.to_string_lossy()).heading())
                                .frame(false),
                        )
                        .clicked()
                    {
                        up = Some(target);
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let h = m.header_button;
                let big = |text: &str| egui::Button::new(text).min_size(egui::vec2(0.0, h));
                if ui.add(big("+ New canvas")).clicked() {
                    leave = Some(Leave::New);
                }
                if ui.add(big("New folder")).clicked() {
                    lib.dialog = Some(Dialog::NewFolder("New folder".into()));
                }
                if ui
                    .add(big("Import…"))
                    .on_hover_text("Copy a project (Rusty Painter, Photoshop, Krita, Clip Studio) into this folder")
                    .clicked()
                {
                    leave = Some(Leave::Import);
                }
                if ui
                    .add(big("Back to canvas"))
                    .on_hover_text("Keep working on the open document")
                    .clicked()
                {
                    lib.open = false;
                }
            });
            ui.separator();

            let entries = lib.entries.as_deref().unwrap_or_default();
            if entries.is_empty() {
                ui.add_space(20.0);
                ui.label(RichText::new("Nothing here yet: start a new canvas.").color(TEXT_DIM));
                return;
            }
            let cell = if m.touch { 168.0 } else { 148.0 };
            let open_project = lib.project.clone();
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    let rows = egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true);
                    ui.with_layout(rows, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        for entry in entries {
                            let thumb = lib.thumbs.get(&entry.path).and_then(|(_, t)| t.as_ref());
                            let current = open_project.as_deref() == Some(entry.path.as_path());
                            tile(ui, entry, thumb, cell, current, &mut act);
                        }
                    });
                });
        });

    if let Some(folder) = up {
        let lib = &mut app.workspace.library;
        lib.folder = folder;
        lib.refresh();
    }
    if let Some(leave) = leave {
        app.leave_asking(leave);
    }
    if let Some(act) = act {
        let result = match act {
            Act::Open(path) => {
                if app.workspace.library.project.as_ref() == Some(&path) {
                    app.workspace.library.open = false;
                } else {
                    app.leave_asking(Leave::Open(path));
                }
                Ok(())
            }
            Act::Enter(path) => {
                app.workspace.library.folder = path;
                app.workspace.library.refresh();
                Ok(())
            }
            Act::Rename(path, name) => {
                app.workspace.library.dialog = Some(Dialog::Rename(path, name));
                Ok(())
            }
            Act::Duplicate(path) => {
                let parent = path.parent().unwrap_or(Path::new("."));
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let copy = unused_path(parent, &format!("{stem} copy"));
                app.workspace.library.refresh();
                std::fs::copy(&path, &copy)
                    .map(|_| ())
                    .map_err(|e| format!("Couldn't copy: {e}"))
            }
            Act::MoveTo(path, folder) => {
                let name = path.file_name().unwrap_or_default();
                let to = folder.join(name);
                if to.exists() {
                    Err(format!(
                        "{} already has one called {}",
                        relative_name(&folder),
                        name.to_string_lossy()
                    ))
                } else {
                    app.moved(&path, &to)
                }
            }
            Act::Export(path) => std::fs::read(&path)
                .map_err(|e| format!("Couldn't read {}: {e}", path.display()))
                .map(|bytes| {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    app.pick_save(&name, EXTENSION, "application/octet-stream", bytes);
                }),
            Act::Delete(path) => {
                app.workspace.library.dialog = Some(Dialog::Delete(path));
                Ok(())
            }
        };
        if let Err(err) = result {
            app.report(err);
        }
    }

    let area = ctx.screen_rect().shrink(8.0);
    crate::app::layout::notices(app, ctx, area);
}

/// One project or folder: tap to open, menu (right-click, long-press or
/// the ... button) for the rest.
fn tile(
    ui: &mut egui::Ui,
    entry: &Entry,
    thumb: Option<&egui::TextureHandle>,
    cell: f32,
    current: bool,
    act: &mut Option<Act>,
) {
    ui.allocate_ui_with_layout(
        egui::vec2(cell, cell + 44.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(cell);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(cell, cell), egui::Sense::click());
            let painter = ui.painter();
            painter.rect_filled(rect, 4.0, BG_RAISED);
            if entry.folder {
                paint_icon(painter, rect.shrink(cell * 0.3), Icon::Folder, TEXT_DIM);
            } else if let Some(tex) = thumb {
                let size = tex.size_vec2();
                let k = ((cell - 12.0) / size.x).min((cell - 12.0) / size.y);
                let r = egui::Rect::from_center_size(rect.center(), size * k);
                crate::ui::widgets::draw_checkerboard(painter, r, 8.0);
                painter.image(
                    tex.id(),
                    r,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            if current || response.hovered() {
                let width = if current { 3.0_f32 } else { 2.0 };
                painter.rect_stroke(rect, 4.0, egui::Stroke::new(width, ACCENT));
            }
            if response.clicked() {
                *act = Some(if entry.folder {
                    Act::Enter(entry.path.clone())
                } else {
                    Act::Open(entry.path.clone())
                });
            }
            response.context_menu(|ui| entry_menu(ui, entry, act));
            ui.horizontal(|ui| {
                ui.set_width(cell);
                ui.add(egui::Label::new(RichText::new(&entry.name).strong()).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("...", |ui| entry_menu(ui, entry, act));
                });
            });
            if !entry.folder {
                ui.label(RichText::new(ago(entry.modified)).small().color(TEXT_DIM));
            }
        },
    );
}

fn entry_menu(ui: &mut egui::Ui, entry: &Entry, act: &mut Option<Act>) {
    let path = &entry.path;
    if ui.button("Open").clicked() {
        *act = Some(if entry.folder {
            Act::Enter(path.clone())
        } else {
            Act::Open(path.clone())
        });
        ui.close_menu();
    }
    if ui.button("Rename…").clicked() {
        *act = Some(Act::Rename(path.clone(), entry.name.clone()));
        ui.close_menu();
    }
    if !entry.folder && ui.button("Duplicate").clicked() {
        *act = Some(Act::Duplicate(path.clone()));
        ui.close_menu();
    }
    ui.menu_button("Move to", |ui| {
        let mut folders = Vec::new();
        all_folders(&root(), &mut folders);
        let here = path.parent();
        for folder in folders {
            // Not where it is, nor into itself.
            if Some(folder.as_path()) == here || folder.starts_with(path) {
                continue;
            }
            if ui.button(relative_name(&folder)).clicked() {
                *act = Some(Act::MoveTo(path.clone(), folder));
                ui.close_menu();
            }
        }
    });
    if !entry.folder && ui.button("Export file…").clicked() {
        *act = Some(Act::Export(path.clone()));
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Delete…").clicked() {
        *act = Some(Act::Delete(path.clone()));
        ui.close_menu();
    }
}

/// Read the thumbnails not loaded yet (or changed since) in the background.
fn load_thumbnails(lib: &mut LibraryState, ctx: &egui::Context) {
    let wanted: Vec<(PathBuf, SystemTime)> = (lib.entries.iter().flatten())
        .filter(|e| !e.folder)
        .filter(|e| {
            lib.thumbs
                .get(&e.path)
                .is_none_or(|(t, _)| *t != e.modified)
        })
        .map(|e| (e.path.clone(), e.modified))
        .collect();
    if wanted.is_empty() {
        return;
    }
    let (tx, ctx) = (lib.tx.clone(), ctx.clone());
    std::thread::spawn(move || {
        for (path, modified) in wanted {
            let img = crate::project::zip::read_entry_from_file(&path, "Thumbnails/thumbnail.png")
                .ok()
                .and_then(|png| image::load_from_memory(&png).ok())
                .map(|img| {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
                });
            if tx.send((path, modified, img)).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
}

/// The library's questions (names, confirmations); also over the canvas,
/// for Save to the library.
pub fn library_dialogs(app: &mut PainterApp, ctx: &egui::Context) {
    let Some(dialog) = app.workspace.library.dialog.take() else {
        return;
    };
    let folder = app.workspace.library.folder.clone();
    let title = match &dialog {
        Dialog::NewFolder(_) => "New folder",
        Dialog::Rename(..) => "Rename",
        Dialog::Delete(_) => "Delete?",
        Dialog::SaveAs(_) => "Save to library",
        Dialog::Unsaved(_) => "Unsaved work",
    };
    let mut keep = true;
    let mut dialog = dialog;
    let mut done: Option<Result<(), String>> = None;
    let mut then = None;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut keep)
        .show(ctx, |ui| {
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            match &mut dialog {
                Dialog::NewFolder(name) => {
                    ui.text_edit_singleline(name).request_focus();
                    if ui.button("Create").clicked() || enter {
                        let path = unused_folder(&folder, name);
                        done = Some(
                            std::fs::create_dir_all(&path)
                                .map_err(|e| format!("Couldn't create the folder: {e}")),
                        );
                    }
                }
                Dialog::Rename(path, name) => {
                    ui.text_edit_singleline(name).request_focus();
                    if ui.button("Rename").clicked() || enter {
                        let parent = path.parent().unwrap_or(Path::new("."));
                        let to = if path.is_dir() {
                            parent.join(file_stem(name))
                        } else {
                            parent.join(format!("{}.{EXTENSION}", file_stem(name)))
                        };
                        done = Some(if to == *path {
                            Ok(())
                        } else if to.exists() {
                            Err(format!("There's already one called {}", file_stem(name)))
                        } else {
                            app.moved(path, &to)
                        });
                    }
                }
                Dialog::Delete(path) => {
                    let name = path.file_stem().unwrap_or_default().to_string_lossy();
                    ui.label(if path.is_dir() {
                        format!("Delete the folder {name} and everything in it?")
                    } else {
                        format!("Delete {name}?")
                    });
                    ui.label(RichText::new("This can't be undone.").color(TEXT_DIM));
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            done = Some(app.delete_entry(path));
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(Ok(()));
                        }
                    });
                }
                Dialog::SaveAs(name) => {
                    ui.label(
                        RichText::new(format!("In {}", relative_name(&folder))).color(TEXT_DIM),
                    );
                    ui.text_edit_singleline(name).request_focus();
                    if ui.button("Save").clicked() || enter {
                        done = Some(app.save_to_library(name));
                    }
                }
                Dialog::Unsaved(leave) => {
                    ui.label("This document hasn't been saved.");
                    ui.horizontal(|ui| {
                        if ui.button("Save to library").clicked() {
                            let name = app.modal_state.new_canvas.name.clone();
                            done = Some(app.save_to_library(&name));
                            then = Some(leave.clone());
                        }
                        if ui.button("Discard").clicked() {
                            done = Some(Ok(()));
                            then = Some(leave.clone());
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(Ok(()));
                        }
                    });
                }
            }
        });
    match done {
        Some(Err(err)) => {
            app.report(err);
            app.workspace.library.dialog = Some(dialog);
        }
        Some(Ok(())) => {
            app.workspace.library.refresh();
            if let Some(leave) = then {
                app.do_leave(leave);
            }
        }
        None if keep => app.workspace.library.dialog = Some(dialog),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_files_that_dont_clash() {
        let dir = std::env::temp_dir().join(format!("rp-library-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unused_path(&dir, "a/b:c");
        assert_eq!(first, dir.join("a_b_c.rpainter"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(unused_path(&dir, "a/b:c"), dir.join("a_b_c 2.rpainter"));
        assert_eq!(unused_path(&dir, "  "), dir.join("Untitled.rpainter"));
        assert!(is_project(&first));
        assert!(!is_project(&dir.join("autosave.rpainter")));
        assert_eq!(list(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
