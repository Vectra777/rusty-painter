//! The project library: projects kept in the app's own folder, sorted into
//! folders, each shown with its thumbnail. The app starts here: no canvas
//! is worked on until one is created or opened (File → Projects comes
//! back to it).

use crate::PainterApp;
use crate::app::files::OpenFor;
use crate::ui::icons::{Icon, paint_icon};
use crate::ui::style::{ACCENT, BG_CANVAS, BG_RAISED, TEXT_DIM, metrics};
use crate::ui::widgets::FitScreen;
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
    /// A folder's latest projects (up to 4), shown on its tile.
    previews: Vec<(PathBuf, SystemTime)>,
}

/// A project or folder being dragged to another place or into a folder.
#[derive(Default)]
struct DragState {
    item: Option<PathBuf>,
    /// Touch: the tile pressed and when (it lifts once held still a moment;
    /// until then a swipe scrolls).
    press: Option<(PathBuf, f64)>,
    /// Touch: the lifted tile was moved (a drop), not let go where it was
    /// (its menu).
    moved: bool,
    /// The press that lifted a tile isn't also a tap on it.
    lifted_press: bool,
}

/// Touch: how long a tile is held still before it lifts to be dragged.
const LIFT_SECONDS: f64 = 0.45;

/// Each folder's own arrangement: its entries' file names, one a line
/// (hidden, so the listing skips it).
const ORDER_FILE: &str = ".order";

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn read_order(folder: &Path) -> Vec<String> {
    std::fs::read_to_string(folder.join(ORDER_FILE))
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn write_order(folder: &Path, names: &[String]) -> Result<(), String> {
    std::fs::write(folder.join(ORDER_FILE), names.join("\n"))
        .map_err(|e| format!("Couldn't save the arrangement: {e}"))
}

/// `from` renamed or moved to `to`: a rename keeps its place in its
/// folder's arrangement, a move leaves it (it shows first where it lands).
fn rename_in_order(from: &Path, to: &Path) {
    let Some(parent) = from.parent() else {
        return;
    };
    let mut order = read_order(parent);
    let Some(i) = order.iter().position(|n| *n == file_name(from)) else {
        return;
    };
    if to.parent() == Some(parent) {
        order[i] = file_name(to);
    } else {
        order.remove(i);
    }
    let _ = write_order(parent, &order);
}

/// The latest `n` projects right inside `dir`, newest first.
fn latest_projects(dir: &Path, n: usize) -> Vec<(PathBuf, SystemTime)> {
    let mut found: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let hidden = path.file_name()?.to_string_lossy().starts_with('.');
            if hidden || !is_project(&path) {
                return None;
            }
            let modified = e
                .metadata()
                .ok()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((path, modified))
        })
        .collect();
    found.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    found.truncate(n);
    found
}

/// Work to do once unsaved work is dealt with.
#[derive(Clone)]
enum Leave {
    Open(PathBuf),
    New,
    Import,
    /// A document from anywhere (the desktop's file dialog).
    OpenFile,
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
    /// A document has been created or opened (until then there's no canvas
    /// to go back to).
    pub has_document: bool,
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
    drag: DragState,
    /// The tiles as last drawn (tests drive drags with them).
    #[cfg(test)]
    drawn: Vec<(PathBuf, egui::Rect, bool)>,
}

impl Default for LibraryState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            // Start in the library: no canvas until one is wanted.
            open: true,
            has_document: false,
            project: None,
            folder: root(),
            entries: None,
            thumbs: HashMap::new(),
            tx,
            rx,
            dialog: None,
            drag: DragState::default(),
            #[cfg(test)]
            drawn: Vec::new(),
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
            let previews = if folder {
                latest_projects(&path, 4)
            } else {
                Vec::new()
            };
            Some(Entry {
                path,
                name,
                folder,
                modified,
                previews,
            })
        })
        .collect();
    // Folders first by name, then the latest work first...
    entries.sort_by(|a, b| {
        b.folder.cmp(&a.folder).then_with(|| {
            if a.folder {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            } else {
                b.modified.cmp(&a.modified)
            }
        })
    });
    // ...then as arranged: what the arrangement doesn't name yet (new
    // work) first, the rest in its order.
    let order = read_order(folder);
    entries.sort_by_key(|e| order.iter().position(|n| *n == file_name(&e.path)));
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
            Leave::OpenFile => self.pick_open(OpenFor::Document),
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
        rename_in_order(from, to);
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
    /// Put the entry at this place in the folder's arrangement (counted
    /// in the arrangement as shown, the entry still in it).
    Reorder(PathBuf, usize),
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
    let touch = m.touch;
    let mut act = None;
    let mut leave = None;
    let mut up = None;
    // Where things were drawn this frame, for a drag's drop.
    let mut crumbs: Vec<(PathBuf, egui::Rect)> = Vec::new();
    let mut tiles: Vec<(PathBuf, egui::Rect, bool)> = Vec::new();
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(BG_CANVAS).inner_margin(12.0))
        .show(ctx, |ui| {
            let lib = &mut app.workspace.library;
            ui.horizontal_wrapped(|ui| {
                // Breadcrumbs: each folder up the path goes back there (and
                // takes what's dropped on it).
                let rel: Vec<PathBuf> = lib
                    .folder
                    .strip_prefix(root())
                    .map(|r| r.iter().map(PathBuf::from).collect())
                    .unwrap_or_default();
                let r = ui.add(egui::Button::new(RichText::new("Projects").heading()).frame(false));
                if r.clicked() {
                    up = Some(root());
                }
                crumbs.push((root(), r.rect));
                let mut at = root();
                for part in rel {
                    at.push(&part);
                    ui.label(RichText::new("/").heading().color(TEXT_DIM));
                    let target = at.clone();
                    let r = ui.add(
                        egui::Button::new(RichText::new(part.to_string_lossy()).heading())
                            .frame(false),
                    );
                    if r.clicked() {
                        up = Some(target.clone());
                    }
                    crumbs.push((target, r.rect));
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
                if !cfg!(target_os = "android")
                    && ui
                        .add(big("Open file…"))
                        .on_hover_text("Open a document from anywhere on disk")
                        .clicked()
                {
                    leave = Some(Leave::OpenFile);
                }
                if lib.has_document
                    && ui
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
            let (thumbs, drag) = (&lib.thumbs, &mut lib.drag);
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                // A lifted tile follows the finger instead.
                .drag_to_scroll(drag.item.is_none())
                .show(ui, |ui| {
                    let rows = egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true);
                    ui.with_layout(rows, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        for entry in entries {
                            let current = open_project.as_deref() == Some(entry.path.as_path());
                            let rect = tile(ui, entry, thumbs, cell, current, touch, drag, &mut act);
                            tiles.push((entry.path.clone(), rect, entry.folder));
                        }
                    });
                });
        });

    // A drag: where it would land, shown; done when the pointer lets go.
    let lib = &mut app.workspace.library;
    #[cfg(test)]
    {
        lib.drawn = tiles.clone();
    }
    if let Some(item) = lib.drag.item.clone() {
        let (pos, origin, down) = ctx.input(|i| {
            (
                i.pointer.latest_pos(),
                i.pointer.press_origin(),
                i.pointer.any_down(),
            )
        });
        if origin.zip(pos).is_some_and(|(o, p)| o.distance(p) > 8.0) {
            lib.drag.moved = true;
        }
        let target = pos.and_then(|p| drop_target(p, &item, &tiles, &crumbs));
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("library-drag"),
        ));
        if let Some((_, mark)) = &target {
            painter.rect_stroke(*mark, 4.0, egui::Stroke::new(3.0_f32, ACCENT));
        }
        if let Some(p) = pos.filter(|_| !touch || lib.drag.moved) {
            let ghost = egui::Rect::from_center_size(p, egui::vec2(72.0, 72.0));
            painter.rect_filled(ghost, 4.0, BG_RAISED.gamma_multiply(0.9));
            match lib.thumbs.get(&item).and_then(|(_, t)| t.as_ref()) {
                Some(tex) => fit_image(&painter, tex, ghost.shrink(4.0)),
                None => paint_icon(&painter, ghost.shrink(18.0), Icon::Folder, TEXT_DIM),
            }
            painter.rect_stroke(ghost, 4.0, egui::Stroke::new(2.0_f32, ACCENT));
        }
        if !down {
            let moved = lib.drag.moved;
            lib.drag.item = None;
            lib.drag.press = None;
            if touch && !moved {
                // Held and let go where it was: its menu.
                ctx.memory_mut(|mem| mem.open_popup(menu_id(&item)));
            } else if let Some((drop, _)) = target {
                act = Some(drop);
            }
        }
        ctx.request_repaint();
    }

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
            Act::Reorder(path, to) => {
                let lib = &mut app.workspace.library;
                let names: Vec<String> = lib
                    .entries
                    .iter()
                    .flatten()
                    .map(|e| file_name(&e.path))
                    .collect();
                let result = match reordered(names, &file_name(&path), to) {
                    Some(names) => write_order(&lib.folder, &names),
                    None => Ok(()),
                };
                lib.refresh();
                result
            }
        };
        if let Err(err) = result {
            app.report(err);
        }
    }

    let area = ctx.screen_rect().shrink(8.0);
    crate::app::layout::notices(app, ctx, area);
}

/// `names` with `name` moved to place `to` (counted with it still in the
/// list); `None` if it isn't there or wouldn't move.
fn reordered(mut names: Vec<String>, name: &str, to: usize) -> Option<Vec<String>> {
    let from = names.iter().position(|n| n == name)?;
    let to = if to > from { to - 1 } else { to };
    if to == from {
        return None;
    }
    let name = names.remove(from);
    names.insert(to.min(names.len()), name);
    Some(names)
}

/// What dropping `item` at `p` does, and the mark to show for it: into a
/// folder (its tile's middle, or a breadcrumb), or a new place beside a
/// tile (a bar in the gap on that side).
fn drop_target(
    p: egui::Pos2,
    item: &Path,
    tiles: &[(PathBuf, egui::Rect, bool)],
    crumbs: &[(PathBuf, egui::Rect)],
) -> Option<(Act, egui::Rect)> {
    // Never into itself or what's inside it.
    let into = |folder: &Path| folder != item && !folder.starts_with(item);
    for (i, (path, rect, folder)) in tiles.iter().enumerate() {
        if !rect.contains(p) {
            continue;
        }
        if *folder && rect.shrink2(rect.size() * 0.2).contains(p) {
            return into(path).then(|| (Act::MoveTo(item.to_path_buf(), path.clone()), *rect));
        }
        let after = p.x > rect.center().x;
        let x = if after {
            rect.right() + 5.0
        } else {
            rect.left() - 5.0
        };
        let bar = egui::Rect::from_min_max(
            egui::pos2(x - 1.0, rect.top()),
            egui::pos2(x + 1.0, rect.bottom()),
        );
        return Some((Act::Reorder(item.to_path_buf(), i + after as usize), bar));
    }
    crumbs
        .iter()
        .find(|(folder, rect)| rect.contains(p) && into(folder) && item.parent() != Some(folder))
        .map(|(folder, rect)| {
            (
                Act::MoveTo(item.to_path_buf(), folder.clone()),
                rect.expand(2.0),
            )
        })
}

/// The popup a touch tile's menu opens in.
fn menu_id(path: &Path) -> egui::Id {
    egui::Id::new(("library-menu", path))
}

/// `tex` as large as fits in `area`, centred, over a checkerboard.
fn fit_image(painter: &egui::Painter, tex: &egui::TextureHandle, area: egui::Rect) {
    let size = tex.size_vec2();
    let k = (area.width() / size.x).min(area.height() / size.y);
    let r = egui::Rect::from_center_size(area.center(), size * k);
    crate::ui::widgets::draw_checkerboard(painter, r, 8.0);
    painter.image(
        tex.id(),
        r,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// One project or folder: tap to open, menu (right-click, long-press or
/// the ... button) for the rest; dragged (on touch, once held still a
/// moment) to another place or into a folder. Returns its picture's rect.
#[allow(clippy::too_many_arguments)]
fn tile(
    ui: &mut egui::Ui,
    entry: &Entry,
    thumbs: &HashMap<PathBuf, (SystemTime, Option<egui::TextureHandle>)>,
    cell: f32,
    current: bool,
    touch: bool,
    drag: &mut DragState,
    act: &mut Option<Act>,
) -> egui::Rect {
    let thumb = |path: &Path| thumbs.get(path).and_then(|(_, t)| t.as_ref());
    let mut picture = egui::Rect::NOTHING;
    ui.allocate_ui_with_layout(
        egui::vec2(cell, cell + 44.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(cell);
            // Touch drags only once lifted (a swipe scrolls the grid).
            let sense = if touch {
                egui::Sense::click()
            } else {
                egui::Sense::click_and_drag()
            };
            let (rect, response) = ui.allocate_exact_size(egui::vec2(cell, cell), sense);
            picture = rect;
            let dragged = drag.item.as_deref() == Some(entry.path.as_path());
            let painter = ui.painter();
            painter.rect_filled(rect, 4.0, BG_RAISED);
            if entry.folder {
                // Its latest projects, two by two, with a folder badge.
                let shown: Vec<&egui::TextureHandle> = entry
                    .previews
                    .iter()
                    .filter_map(|(p, _)| thumb(p))
                    .collect();
                if shown.is_empty() {
                    paint_icon(painter, rect.shrink(cell * 0.3), Icon::Folder, TEXT_DIM);
                } else {
                    let inner = rect.shrink(8.0);
                    let half = (inner.width() - 4.0) / 2.0;
                    for (k, tex) in shown.iter().enumerate() {
                        let at = inner.min
                            + egui::vec2(
                                (k % 2) as f32 * (half + 4.0),
                                (k / 2) as f32 * (half + 4.0),
                            );
                        fit_image(
                            painter,
                            tex,
                            egui::Rect::from_min_size(at, egui::vec2(half, half)),
                        );
                    }
                    let badge = egui::Rect::from_min_size(
                        rect.right_bottom() - egui::vec2(30.0, 30.0),
                        egui::vec2(26.0, 26.0),
                    );
                    painter.rect_filled(badge, 4.0, BG_CANVAS);
                    paint_icon(painter, badge.shrink(4.0), Icon::Folder, TEXT_DIM);
                }
            } else if let Some(tex) = thumb(&entry.path) {
                fit_image(painter, tex, rect.shrink(6.0));
            }
            if dragged {
                painter.rect_filled(rect, 4.0, BG_CANVAS.gamma_multiply(0.6));
            }
            if current || response.hovered() {
                let width = if current { 3.0_f32 } else { 2.0 };
                painter.rect_stroke(rect, 4.0, egui::Stroke::new(width, ACCENT));
            }
            if !touch && response.drag_started() {
                drag.item = Some(entry.path.clone());
                drag.moved = true;
            }
            if touch && drag.item.is_none() && response.is_pointer_button_down_on() {
                // Held still long enough: lifted.
                let (now, still) = ui.input(|i| {
                    let still = i
                        .pointer
                        .press_origin()
                        .zip(i.pointer.latest_pos())
                        .is_some_and(|(o, p)| o.distance(p) < 8.0);
                    (i.time, still)
                });
                if drag.press.as_ref().is_none_or(|(p, _)| *p != entry.path) {
                    drag.press = Some((entry.path.clone(), now));
                    drag.lifted_press = false;
                }
                let since = drag.press.as_ref().map_or(0.0, |(_, t)| now - t);
                if !still {
                    drag.press = None;
                } else if since >= LIFT_SECONDS {
                    drag.item = Some(entry.path.clone());
                    drag.moved = false;
                    drag.lifted_press = true;
                } else {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(
                            LIFT_SECONDS - since,
                        ));
                }
            }
            if response.clicked() && !(touch && drag.lifted_press) {
                *act = Some(if entry.folder {
                    Act::Enter(entry.path.clone())
                } else {
                    Act::Open(entry.path.clone())
                });
            }
            if touch {
                let id = menu_id(&entry.path);
                egui::popup_below_widget(
                    ui,
                    id,
                    &response,
                    egui::PopupCloseBehavior::CloseOnClickOutside,
                    |ui| {
                        ui.set_min_width(160.0);
                        entry_menu(ui, entry, act);
                    },
                );
                if act.is_some() {
                    ui.memory_mut(|mem| mem.close_popup());
                }
            } else {
                response.context_menu(|ui| entry_menu(ui, entry, act));
            }
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
    picture
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
    // Projects' own, and those a folder's tile shows.
    let wanted: Vec<(PathBuf, SystemTime)> = (lib.entries.iter().flatten())
        .flat_map(|e| {
            if e.folder {
                e.previews.clone()
            } else {
                vec![(e.path.clone(), e.modified)]
            }
        })
        .filter(|(path, modified)| lib.thumbs.get(path).is_none_or(|(t, _)| t != modified))
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
        .fit_screen(ctx)
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

    #[test]
    fn a_folder_keeps_its_arrangement_and_new_work_shows_first() {
        let dir = std::env::temp_dir().join(format!("rp-library-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Comics")).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(dir.join(format!("{name}.rpainter")), b"x").unwrap();
        }
        let names = |dir: &Path| list(dir).iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        // Arranged: c before the folder before a; b isn't named (new) so first.
        write_order(
            &dir,
            &["c.rpainter".into(), "Comics".into(), "a.rpainter".into()],
        )
        .unwrap();
        assert_eq!(names(&dir), ["b", "c", "Comics", "a"]);
        // Dragging a in front of c.
        let shown: Vec<String> = list(&dir).iter().map(|e| file_name(&e.path)).collect();
        write_order(&dir, &reordered(shown, "a.rpainter", 1).unwrap()).unwrap();
        assert_eq!(names(&dir), ["b", "a", "c", "Comics"]);
        // Dropped where it is: nothing to do.
        let shown: Vec<String> = list(&dir).iter().map(|e| file_name(&e.path)).collect();
        assert!(reordered(shown.clone(), "a.rpainter", 1).is_none());
        assert!(reordered(shown, "a.rpainter", 2).is_none());
        // A rename keeps its place; a move into the folder leaves the list.
        let (from, to) = (dir.join("c.rpainter"), dir.join("d.rpainter"));
        std::fs::rename(&from, &to).unwrap();
        rename_in_order(&from, &to);
        assert_eq!(names(&dir), ["b", "a", "d", "Comics"]);
        let into = dir.join("Comics/a.rpainter");
        std::fs::rename(dir.join("a.rpainter"), &into).unwrap();
        rename_in_order(&dir.join("a.rpainter"), &into);
        assert_eq!(names(&dir), ["b", "d", "Comics"]);
        // The folder shows what's in it.
        let comics = list(&dir).into_iter().find(|e| e.folder).unwrap();
        assert_eq!(
            comics
                .previews
                .iter()
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>(),
            [into]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dragging_a_tile_moves_it_into_a_folder_or_to_a_new_place() {
        use crate::canvas::Canvas;
        let dir = std::env::temp_dir().join(format!("rp-library-drag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Comics")).unwrap();
        for name in ["Sketch", "Portrait", "Study"] {
            std::fs::write(dir.join(format!("{name}.rpainter")), b"x").unwrap();
        }
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(64, 64, egui::Color32::WHITE, 64));
        app.workspace.library.folder = dir.clone();
        app.workspace.library.refresh();
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut frame = |app: &mut PainterApp, events: Vec<egui::Event>| {
            time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| library_screen(app, ctx));
        };
        let rect = |app: &PainterApp, name: &str| {
            let lib = &app.workspace.library;
            lib.drawn
                .iter()
                .find(|(p, _, _)| file_name(p) == name)
                .map(|(_, r, _)| *r)
                .expect("drawn")
        };
        fn drag(
            frame: &mut dyn FnMut(&mut PainterApp, Vec<egui::Event>),
            app: &mut PainterApp,
            from: egui::Pos2,
            to: egui::Pos2,
        ) {
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            frame(
                app,
                vec![egui::Event::PointerMoved(from), button(from, true)],
            );
            for k in 1..=10 {
                let p = from + (to - from) * (k as f32 / 10.0);
                frame(app, vec![egui::Event::PointerMoved(p)]);
            }
            frame(app, vec![button(to, false)]);
            frame(app, Vec::new());
        }
        let names = |app: &PainterApp| -> Vec<String> {
            let lib = &app.workspace.library;
            lib.entries
                .iter()
                .flatten()
                .map(|e| e.name.clone())
                .collect()
        };
        frame(&mut app, Vec::new());
        frame(&mut app, Vec::new());
        assert_eq!(names(&app)[0], "Comics");
        // Portrait onto the folder's middle: moved in.
        let (from, to) = (
            rect(&app, "Portrait.rpainter").center(),
            rect(&app, "Comics").center(),
        );
        drag(&mut frame, &mut app, from, to);
        assert!(dir.join("Comics/Portrait.rpainter").exists());
        assert!(!dir.join("Portrait.rpainter").exists());
        // The last tile onto the left half of the first project: a new place.
        let shown = names(&app);
        let (last, first) = (shown[2].clone(), shown[1].clone());
        let target = rect(&app, &format!("{first}.rpainter"));
        let from = rect(&app, &format!("{last}.rpainter")).center();
        drag(
            &mut frame,
            &mut app,
            from,
            target.left_center() + egui::vec2(10.0, 0.0),
        );
        assert_eq!(names(&app), ["Comics", last.as_str(), first.as_str()]);

        // Touch: a swipe doesn't drag; held still, the tile lifts and goes.
        crate::ui::style::set_touch_metrics(&ctx, true);
        frame(&mut app, Vec::new());
        let item = rect(&app, &format!("{first}.rpainter")).center();
        let folder = rect(&app, "Comics").center();
        drag(&mut frame, &mut app, item, folder);
        assert!(
            dir.join(format!("{first}.rpainter")).exists(),
            "a swipe moved it"
        );
        frame(
            &mut app,
            vec![egui::Event::PointerMoved(item), touch_button(item, true)],
        );
        for _ in 0..40 {
            frame(&mut app, Vec::new());
        }
        assert!(app.workspace.library.drag.item.is_some(), "lifted");
        for k in 1..=10 {
            let p = item + (folder - item) * (k as f32 / 10.0);
            frame(&mut app, vec![egui::Event::PointerMoved(p)]);
        }
        frame(&mut app, vec![touch_button(folder, false)]);
        frame(&mut app, Vec::new());
        assert!(dir.join(format!("Comics/{first}.rpainter")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn touch_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
}
