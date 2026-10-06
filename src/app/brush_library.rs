//! The brush library's own data: each preset's tags, the favourites and the
//! brushes used last. It is kept for every preset, the built-in ones too, by
//! name, in `brushes/presets/library.json` (the `.rpbrush` files hold only
//! brushes, so they stay as they were). Also the pure functions the presets
//! window and the pop-up palette filter with.

use crate::app::PainterApp;
use crate::brush_engine::brush::BrushPreset;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How many brushes the recent list remembers.
pub const MAX_RECENT: usize = 8;
/// Most brushes on the pop-up palette.
pub const MAX_PALETTE: usize = 12;
/// The library file, in `brushes/presets/`.
pub const LIBRARY_FILE: &str = "library.json";
const VERSION: u32 = 1;
/// A right press that moves less than this (in points) before its release is
/// a click (the pop-up palette), not a pan.
const CLICK_SLOP: f32 = 5.0;

/// What `library.json` holds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct LibraryFile {
    pub version: u32,
    /// Each preset's tags, by preset name.
    pub tags: BTreeMap<String, Vec<String>>,
    /// Starred presets, in the order they were starred.
    pub favourites: Vec<String>,
    /// The presets picked last, newest first.
    pub recent: Vec<String>,
    /// The presets that come with the app were copied into the library
    /// (once: the ones deleted since stay deleted).
    pub defaults_installed: bool,
}

impl Default for LibraryFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            tags: BTreeMap::new(),
            favourites: Vec::new(),
            recent: Vec::new(),
            defaults_installed: false,
        }
    }
}

impl LibraryFile {
    pub fn tags(&self, name: &str) -> &[String] {
        self.tags.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn is_favourite(&self, name: &str) -> bool {
        self.favourites.iter().any(|f| f == name)
    }

    pub fn toggle_favourite(&mut self, name: &str) {
        if self.is_favourite(name) {
            self.favourites.retain(|f| f != name);
        } else {
            self.favourites.push(name.to_string());
        }
    }

    /// Put `name` at the front of the recent list.
    pub fn remember(&mut self, name: &str) {
        self.recent.retain(|r| r != name);
        self.recent.insert(0, name.to_string());
        self.recent.truncate(MAX_RECENT);
    }

    /// Tag `name` with `tag` (trimmed); a tag it has in another case isn't
    /// added twice. Returns whether it changed.
    pub fn add_tag(&mut self, name: &str, tag: &str) -> bool {
        let tag = tag.trim();
        if tag.is_empty() {
            return false;
        }
        let tags = self.tags.entry(name.to_string()).or_default();
        if tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            return false;
        }
        tags.push(tag.to_string());
        true
    }

    pub fn remove_tag(&mut self, name: &str, tag: &str) {
        if let Some(tags) = self.tags.get_mut(name) {
            tags.retain(|t| t != tag);
        }
    }

    /// A preset was deleted: drop what was kept for it.
    pub fn forget(&mut self, name: &str) {
        self.tags.remove(name);
        self.favourites.retain(|f| f != name);
        self.recent.retain(|r| r != name);
    }

    /// Every tag in use, sorted, each once.
    pub fn all_tags(&self) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        for tag in self.tags.values().flatten() {
            if !all.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                all.push(tag.clone());
            }
        }
        all.sort_by_key(|t| t.to_lowercase());
        all
    }
}

/// Which presets the presets window lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Shelf {
    #[default]
    All,
    Favourites,
    /// The recent list, newest first.
    Recent,
    Tag(String),
}

/// The library, plus the presets window's filters and the pop-up palette.
#[derive(Default)]
pub struct BrushLibrary {
    pub file: LibraryFile,
    /// Changed: saved at the end of the frame.
    pub dirty: bool,
    pub search: String,
    pub shelf: Shelf,
    /// The tag being typed in a preset's menu.
    pub new_tag: String,
    pub radial: Option<RadialPalette>,
    /// Where the right button went down on the canvas.
    pub secondary_press: Option<egui::Pos2>,
}

/// The pop-up palette: brushes in a ring around where it opened.
pub struct RadialPalette {
    pub centre: egui::Pos2,
    /// The presets on it, clockwise from the top, by name.
    pub names: Vec<String>,
    /// Opened with its key, which is still held: letting go over a slice
    /// picks it.
    pub key_held: bool,
    /// A button went down since it opened (not the click that opened it):
    /// its release may pick.
    pub armed: bool,
    /// Not drawn yet.
    pub fresh: bool,
}

/// Whether `name` with `tags` matches `query`: every word of it is part of
/// the name or of a tag, ignoring case.
pub fn matches_search(name: &str, tags: &[String], query: &str) -> bool {
    let name = name.to_lowercase();
    let tags: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| name.contains(word) || tags.iter().any(|t| t.contains(word)))
}

/// The presets `shelf` and `search` show, as indices into `presets`: in
/// list order, or newest first for the recent shelf.
pub fn shown_presets(
    presets: &[BrushPreset],
    lib: &LibraryFile,
    shelf: &Shelf,
    search: &str,
) -> Vec<usize> {
    let found = |i: &usize| {
        let name = &presets[*i].name;
        matches_search(name, lib.tags(name), search)
    };
    let indices: Vec<usize> = match shelf {
        Shelf::All => (0..presets.len()).collect(),
        Shelf::Favourites => (0..presets.len())
            .filter(|&i| lib.is_favourite(&presets[i].name))
            .collect(),
        Shelf::Recent => by_name(presets, &lib.recent),
        Shelf::Tag(tag) => (0..presets.len())
            .filter(|&i| (lib.tags(&presets[i].name).iter()).any(|t| t.eq_ignore_ascii_case(tag)))
            .collect(),
    };
    indices.into_iter().filter(found).collect()
}

/// The indices of the presets called `names`, in that order (names no
/// preset has any more are left out).
fn by_name(presets: &[BrushPreset], names: &[String]) -> Vec<usize> {
    names
        .iter()
        .filter_map(|n| presets.iter().position(|p| &p.name == n))
        .collect()
}

/// The presets on the pop-up palette: the favourites, else the recent ones,
/// else the first few.
pub fn palette_presets(presets: &[BrushPreset], lib: &LibraryFile) -> Vec<usize> {
    let mut shown = by_name(presets, &lib.favourites);
    if shown.is_empty() {
        shown = by_name(presets, &lib.recent);
    }
    if shown.is_empty() {
        shown = (0..presets.len()).collect();
    }
    shown.truncate(MAX_PALETTE);
    shown
}

/// Where a point falls on the pop-up palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadialHit {
    /// On the middle: closes it.
    Centre,
    /// Toward slice `n` (counted clockwise from the one at the top), at any
    /// distance past the middle.
    Slice(usize),
}

/// Which of `count` slices around `centre` `pointer` points to; within
/// `inner_radius` of the centre it's the centre.
pub fn radial_hit(
    centre: egui::Pos2,
    pointer: egui::Pos2,
    count: usize,
    inner_radius: f32,
) -> RadialHit {
    let d = pointer - centre;
    if count == 0 || d.length() < inner_radius {
        return RadialHit::Centre;
    }
    let tau = std::f32::consts::TAU;
    let slice = tau / count as f32;
    // Clockwise from straight up (y points down), shifted half a slice so
    // slice 0 is centred at the top.
    let angle = (d.y.atan2(d.x) + std::f32::consts::FRAC_PI_2 + slice * 0.5).rem_euclid(tau);
    RadialHit::Slice(((angle / slice) as usize).min(count - 1))
}

/// The direction of the middle of slice `n` of `count` (clockwise from up).
pub fn slice_direction(n: usize, count: usize) -> egui::Vec2 {
    let angle =
        n as f32 * std::f32::consts::TAU / count.max(1) as f32 - std::f32::consts::FRAC_PI_2;
    egui::vec2(angle.cos(), angle.sin())
}

/// The tags a built-in preset starts with.
pub fn default_tags(name: &str) -> &'static [&'static str] {
    match name {
        "Pencil (Sketch)" | "Sketchy Pencil" => &["Sketching"],
        "Ink Pen" | "Calligraphy" => &["Inking"],
        "Cross Hatch" => &["Inking", "Sketching"],
        "Hard Round" => &["Painting", "Inking"],
        "Soft Airbrush" | "Multiply Marker" | "Watercolour" | "Oil Bristle" => &["Painting"],
        "Dry Bristles" | "Dry Media" | "Dry Brush" => &["Painting", "Texture"],
        "Chalk" | "Charcoal" | "Two-Tone Chalk" => &["Sketching", "Texture"],
        "Spray" | "Spatter" => &["Texture", "Effects"],
        "Foliage" | "Mixed Leaves" | "Flowers" => &["Texture", "Nature"],
        "Glow" | "Stitches" | "Chain" | "Lace Ribbon" | "Striped Ribbon" => &["Effects"],
        "Eraser (Soft)" | "Eraser (Hard)" => &["Erasers"],
        "Pixel Art" => &["Pixel"],
        "Spray Cloud" | "Particle Swarm" => &["Texture", "Effects"],
        "Rough Chalk" => &["Sketching", "Texture"],
        "Swirl Curves" => &["Sketching", "Effects"],
        "Mosaic Grid" => &["Effects"],
        "Normal Map" => &["3D"],
        _ => &[],
    }
}

/// The app a brush file comes from, by its extension.
pub fn source_app(file_name: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(file_name)
        .extension()?
        .to_string_lossy()
        .to_lowercase();
    Some(match ext.as_str() {
        "abr" => "Photoshop",
        "kpp" | "bundle" => "Krita",
        "gbr" | "gih" => "GIMP",
        "myb" => "MyPaint",
        "sut" => "Clip Studio",
        _ => return None,
    })
}

impl PainterApp {
    fn library_path(&self) -> std::path::PathBuf {
        self.brush_state
            .brushes_path
            .join("presets")
            .join(LIBRARY_FILE)
    }

    /// Read `library.json` (after the presets are loaded); presets it
    /// doesn't know get their default tags.
    pub(crate) fn load_brush_library(&mut self) {
        let path = self.library_path();
        let mut file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<LibraryFile>(&bytes).unwrap_or_else(|err| {
                log::warn!("Ignoring {}: {err}", path.display());
                LibraryFile::default()
            }),
            Err(_) => LibraryFile::default(),
        };
        for preset in &self.brush_state.presets {
            if !file.tags.contains_key(&preset.name) {
                let tags = default_tags(&preset.name).iter().map(|t| t.to_string());
                file.tags.insert(preset.name.clone(), tags.collect());
            }
        }
        self.brush_state.library.file = file;
    }

    /// Keep the library for next time. Errors are logged.
    pub(crate) fn save_brush_library(&self) {
        match serde_json::to_vec_pretty(&self.brush_state.library.file) {
            Ok(bytes) => {
                crate::app::jobs::write_later(self.library_path(), "brush library", move || {
                    Ok(bytes)
                })
            }
            Err(err) => log::warn!("Couldn't save the brush library: {err}"),
        }
    }

    /// Change the library (saved at the end of the frame).
    pub(crate) fn edit_library(&mut self, edit: impl FnOnce(&mut LibraryFile)) {
        edit(&mut self.brush_state.library.file);
        self.brush_state.library.dirty = true;
    }

    /// Open the pop-up palette around `pos` (unless mid-stroke or there
    /// are no presets). `key_held`: opened with its key, still down.
    pub(crate) fn open_radial_palette(&mut self, pos: egui::Pos2, key_held: bool) {
        if self.brush_state.is_drawing || self.viewport.is_primary_down {
            return;
        }
        let bs = &self.brush_state;
        let names: Vec<String> = palette_presets(&bs.presets, &bs.library.file)
            .into_iter()
            .map(|i| bs.presets[i].name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        self.brush_state.library.radial = Some(RadialPalette {
            centre: pos,
            names,
            key_held,
            armed: false,
            fresh: true,
        });
    }

    /// Pick the palette's slice `n` and close it.
    pub(crate) fn pick_radial_slice(&mut self, n: usize) {
        let Some(radial) = self.brush_state.library.radial.take() else {
            return;
        };
        let index = (radial.names.get(n)).and_then(|name| {
            self.brush_state
                .presets
                .iter()
                .position(|p| &p.name == name)
        });
        if let Some(index) = index {
            self.apply_preset(index);
        }
    }

    /// The right button on the canvas: a click without a drag opens the
    /// pop-up palette where it was (a drag still pans). A pen's side button
    /// usually sends a right click, so it opens it too.
    pub(crate) fn radial_right_button(&mut self, pos: egui::Pos2, pressed: bool, over: bool) {
        let library = &mut self.brush_state.library;
        if pressed {
            library.secondary_press = over.then_some(pos);
        } else if let Some(start) = library.secondary_press.take()
            && start.distance(pos) < CLICK_SLOP
        {
            self.open_radial_palette(pos, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush_engine::brush::Brush;
    use eframe::egui::{Color32, pos2};

    fn presets(names: &[&str]) -> Vec<BrushPreset> {
        names
            .iter()
            .map(|n| BrushPreset {
                name: n.to_string(),
                brush: Brush::new(10.0, 50.0, Color32::BLACK, 10.0),
                file: None,
            })
            .collect()
    }

    fn library() -> LibraryFile {
        let mut lib = LibraryFile::default();
        lib.add_tag("Ink Pen", "Inking");
        lib.add_tag("Chalk", "Sketching");
        lib.add_tag("Chalk", "Texture");
        lib.add_tag("Leaf", "Imported");
        lib.add_tag("Leaf", "Krita");
        lib
    }

    #[test]
    fn search_matches_name_and_tags_ignoring_case() {
        let ps = presets(&["Ink Pen", "Chalk", "Leaf"]);
        let lib = library();
        let search = |q: &str| shown_presets(&ps, &lib, &Shelf::All, q);
        assert_eq!(search(""), vec![0, 1, 2]);
        assert_eq!(search("  "), vec![0, 1, 2]);
        assert_eq!(search("PEN"), vec![0]);
        assert_eq!(search("krita"), vec![2], "by tag");
        assert_eq!(search("tex"), vec![1], "part of a tag");
        assert_eq!(search("chalk sketch"), vec![1], "every word");
        assert_eq!(search("chalk inking"), Vec::<usize>::new());
    }

    #[test]
    fn shelves_filter_by_tag_favourite_and_recent() {
        let ps = presets(&["Ink Pen", "Chalk", "Leaf"]);
        let mut lib = library();
        let tag = |lib: &LibraryFile, t: &str| shown_presets(&ps, lib, &Shelf::Tag(t.into()), "");
        assert_eq!(tag(&lib, "texture"), vec![1], "tags ignore case");
        assert_eq!(tag(&lib, "Nothing"), Vec::<usize>::new());
        lib.toggle_favourite("Leaf");
        lib.toggle_favourite("Ink Pen");
        assert_eq!(
            shown_presets(&ps, &lib, &Shelf::Favourites, ""),
            vec![0, 2],
            "in list order"
        );
        lib.toggle_favourite("Leaf");
        assert_eq!(shown_presets(&ps, &lib, &Shelf::Favourites, ""), vec![0]);
        lib.remember("Leaf");
        lib.remember("Ink Pen");
        lib.remember("Gone");
        assert_eq!(
            shown_presets(&ps, &lib, &Shelf::Recent, ""),
            vec![0, 2],
            "newest first, missing presets left out"
        );
        assert_eq!(shown_presets(&ps, &lib, &Shelf::Recent, "leaf"), vec![2]);
    }

    #[test]
    fn recent_list_is_newest_first_without_repeats_and_capped() {
        let mut lib = LibraryFile::default();
        for i in 0..MAX_RECENT + 3 {
            lib.remember(&format!("B{i}"));
        }
        assert_eq!(lib.recent.len(), MAX_RECENT);
        assert_eq!(lib.recent[0], format!("B{}", MAX_RECENT + 2));
        lib.remember("B5");
        assert_eq!(lib.recent[0], "B5");
        assert_eq!(lib.recent.iter().filter(|r| *r == "B5").count(), 1);
        assert_eq!(lib.recent.len(), MAX_RECENT);
    }

    #[test]
    fn tags_are_trimmed_and_not_doubled() {
        let mut lib = LibraryFile::default();
        assert!(lib.add_tag("A", " Inking "));
        assert!(!lib.add_tag("A", "inking"));
        assert!(!lib.add_tag("A", "  "));
        assert!(lib.add_tag("B", "Effects"));
        assert!(lib.add_tag("B", "INKING"));
        assert_eq!(lib.tags("A"), ["Inking"]);
        assert_eq!(lib.all_tags(), ["Effects", "Inking"]);
        lib.remove_tag("A", "Inking");
        assert!(lib.tags("A").is_empty());
        lib.toggle_favourite("B");
        lib.remember("B");
        lib.forget("B");
        assert!(lib.tags("B").is_empty() && lib.favourites.is_empty() && lib.recent.is_empty());
    }

    #[test]
    fn the_palette_shows_favourites_else_recent() {
        let ps = presets(&["A", "B", "C"]);
        let mut lib = LibraryFile::default();
        assert_eq!(palette_presets(&ps, &lib), vec![0, 1, 2]);
        lib.remember("B");
        lib.remember("C");
        assert_eq!(palette_presets(&ps, &lib), vec![2, 1]);
        lib.toggle_favourite("A");
        assert_eq!(palette_presets(&ps, &lib), vec![0]);
    }

    #[test]
    fn radial_slices_are_hit_by_direction() {
        let c = pos2(100.0, 100.0);
        let hit = |x: f32, y: f32, n: usize| radial_hit(c, pos2(100.0 + x, 100.0 + y), n, 20.0);
        assert_eq!(hit(0.0, 0.0, 4), RadialHit::Centre);
        assert_eq!(hit(10.0, 10.0, 4), RadialHit::Centre);
        assert_eq!(hit(5.0, 3.0, 0), RadialHit::Centre, "no slices");
        // Four slices: top, right, bottom, left.
        assert_eq!(hit(0.0, -50.0, 4), RadialHit::Slice(0));
        assert_eq!(hit(50.0, 0.0, 4), RadialHit::Slice(1));
        assert_eq!(hit(0.0, 50.0, 4), RadialHit::Slice(2));
        assert_eq!(hit(-50.0, 0.0, 4), RadialHit::Slice(3));
        // Just left of the top is still the top slice; far away counts.
        assert_eq!(hit(-10.0, -50.0, 4), RadialHit::Slice(0));
        assert_eq!(hit(-500.0, -20.0, 4), RadialHit::Slice(3));
        // Eight slices: the top right diagonal is slice 1.
        assert_eq!(hit(40.0, -40.0, 8), RadialHit::Slice(1));
        assert_eq!(hit(0.0, -40.0, 1), RadialHit::Slice(0));
        assert_eq!(hit(0.0, 40.0, 1), RadialHit::Slice(0));
        // Each slice's own direction hits it.
        for n in 1..=MAX_PALETTE {
            for i in 0..n {
                let d = slice_direction(i, n) * 60.0;
                assert_eq!(hit(d.x, d.y, n), RadialHit::Slice(i), "{i} of {n}");
            }
        }
    }

    #[test]
    fn built_in_presets_all_have_tags() {
        for preset in PainterApp::default_brush_presets() {
            assert!(!default_tags(&preset.name).is_empty(), "{}", preset.name);
        }
    }

    #[test]
    fn default_presets_are_files_that_keep_changes() {
        let dir = std::env::temp_dir().join(format!("rp-defaults-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let start = || {
            let canvas = crate::canvas::Canvas::new(64, 64, Color32::WHITE, 64);
            let mut app = crate::project::tests::test_app_pub(canvas);
            app.brush_state.brushes_path = dir.join("brushes");
            app.load_user_presets();
            app.load_brush_library();
            app.install_default_presets();
            app
        };
        let defaults = PainterApp::default_brush_presets();
        let names = |app: &PainterApp| -> Vec<String> {
            app.brush_state
                .presets
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        let mut app = start();
        let default_names: Vec<String> = defaults.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names(&app), default_names, "installed, in their order");
        assert!(app.brush_state.presets.iter().all(|p| p.file.is_some()));
        assert_eq!(app.brush_state.library.file.tags("Ink Pen"), ["Inking"]);
        app.save_brush_library();

        // A change to the brush lands in its preset's file.
        let ink = app
            .brush_state
            .presets
            .iter()
            .position(|p| p.name == "Ink Pen")
            .unwrap();
        app.apply_preset(ink);
        app.brush_state.brush.brush_options.diameter = 99.0;
        app.save_active_preset(true);
        assert_ne!(
            app.brush_state.presets[ink].brush.brush_options.diameter,
            99.0
        );
        app.save_active_preset(false);
        let chalk = app
            .brush_state
            .presets
            .iter()
            .position(|p| p.name == "Chalk")
            .unwrap();
        app.delete_user_preset(chalk);
        app.save_brush_library();

        let mut app = start();
        let ink = app
            .brush_state
            .presets
            .iter()
            .position(|p| p.name == "Ink Pen")
            .unwrap();
        assert_eq!(
            app.brush_state.presets[ink].brush.brush_options.diameter,
            99.0
        );
        assert!(
            !names(&app).contains(&"Chalk".to_string()),
            "deleted stays deleted"
        );
        app.restore_default_presets();
        assert_eq!(names(&app), default_names, "restored, in their order");
        let ink = app
            .brush_state
            .presets
            .iter()
            .position(|p| p.name == "Ink Pen")
            .unwrap();
        app.reset_preset(ink);
        let default_ink = defaults.iter().find(|p| p.name == "Ink Pen").unwrap();
        assert_eq!(
            app.brush_state.presets[ink].brush.brush_options.diameter,
            default_ink.brush.brush_options.diameter
        );
        let app = start();
        assert_eq!(names(&app), default_names);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_library_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("rp-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let canvas = crate::canvas::Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.brush_state.brushes_path = dir.join("brushes");
        app.brush_state.presets = PainterApp::default_brush_presets();
        // No file yet: the built-in tags.
        app.load_brush_library();
        assert_eq!(app.brush_state.library.file.tags("Ink Pen"), ["Inking"]);
        app.edit_library(|lib| {
            lib.remove_tag("Ink Pen", "Inking");
            lib.add_tag("Ink Pen", "Comics");
            lib.toggle_favourite("Chalk");
        });
        let chalk = app
            .brush_state
            .presets
            .iter()
            .position(|p| p.name == "Chalk");
        app.apply_preset(chalk.unwrap());
        assert!(app.brush_state.library.dirty);
        app.save_brush_library();
        let saved = app.brush_state.library.file.clone();
        app.brush_state.library.file = LibraryFile::default();
        app.load_brush_library();
        assert_eq!(app.brush_state.library.file, saved);
        assert_eq!(app.brush_state.library.file.tags("Ink Pen"), ["Comics"]);
        assert_eq!(app.brush_state.library.file.recent, ["Chalk"]);
        // An older or partial file loads with the rest at defaults.
        std::fs::write(app.library_path(), br#"{"favourites":["Glow"]}"#).unwrap();
        app.load_brush_library();
        let lib = &app.brush_state.library.file;
        assert_eq!(lib.favourites, ["Glow"]);
        assert!(lib.recent.is_empty());
        assert_eq!(lib.tags("Glow"), ["Effects"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imported_brushes_are_tagged_with_their_app() {
        assert_eq!(source_app("set.ABR"), Some("Photoshop"));
        assert_eq!(source_app("x.bundle"), Some("Krita"));
        assert_eq!(source_app("x.rpbrush"), None);
        let dir = std::env::temp_dir().join(format!("rp-import-tags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let canvas = crate::canvas::Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.brush_state.brushes_path = dir.join("brushes");
        app.brush_state.presets.clear();
        let bytes = crate::brush_engine::preset_file::encode(&presets(&["Mine"])).unwrap();
        app.import_brushes_bytes("mine.rpbrush", &bytes).unwrap();
        assert_eq!(app.brush_state.library.file.tags("Mine"), ["Imported"]);
        // The same name again gets a new one, tagged too.
        app.import_brushes_bytes("mine.rpbrush", &bytes).unwrap();
        assert_eq!(app.brush_state.library.file.tags("Mine 2"), ["Imported"]);
        // Another app's brush: its app too. A version 2 GIMP brush, 4×4 grey.
        let mut gbr = Vec::new();
        for v in [28 + 5u32, 2, 4, 4, 1] {
            gbr.extend_from_slice(&v.to_be_bytes());
        }
        gbr.extend_from_slice(b"GIMP");
        gbr.extend_from_slice(&25u32.to_be_bytes());
        gbr.extend_from_slice(b"Dot\0\0");
        gbr.extend_from_slice(&[255; 16]);
        app.import_brushes_bytes("dot.gbr", &gbr).unwrap();
        let dot = &app.brush_state.presets[2].name;
        assert_eq!(app.brush_state.library.file.tags(dot), ["Imported", "GIMP"]);
        // Deleting forgets its tags.
        app.delete_user_preset(1);
        assert!(app.brush_state.library.file.tags("Mine 2").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exported_tags_and_stars_arrive_with_the_brushes() {
        use crate::brush_engine::preset_file::{PresetMeta, decode_with_meta, encode_with_meta};
        let dir = std::env::temp_dir().join(format!("rp-import-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let canvas = crate::canvas::Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.brush_state.brushes_path = dir.join("brushes");
        app.brush_state.presets.clear();
        let meta = PresetMeta {
            tags: vec!["Comics".into()],
            favourite: true,
        };
        let bytes = encode_with_meta(&presets(&["Mine", "Other"]), &[meta]).unwrap();
        app.import_brushes_bytes("set.rpbrush", &bytes).unwrap();
        let lib = &app.brush_state.library.file;
        assert_eq!(lib.tags("Mine"), ["Comics", "Imported"]);
        assert!(lib.is_favourite("Mine"));
        assert_eq!(lib.tags("Other"), ["Imported"]);
        assert!(!lib.is_favourite("Other"));
        // Exporting writes them back, without "Imported".
        let bytes = crate::app::brush_io::export_presets_bytes(&app, &[0]).unwrap();
        let back = decode_with_meta(&bytes.unwrap()).unwrap();
        assert_eq!(back[0].1.tags, ["Comics"]);
        assert!(back[0].1.favourite);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_right_click_opens_the_palette_and_a_drag_does_not() {
        let canvas = crate::canvas::Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.brush_state.presets = presets(&["A", "B"]);
        app.radial_right_button(pos2(10.0, 10.0), true, true);
        app.radial_right_button(pos2(60.0, 10.0), false, true);
        assert!(app.brush_state.library.radial.is_none(), "a drag pans");
        app.radial_right_button(pos2(10.0, 10.0), true, false);
        app.radial_right_button(pos2(10.0, 10.0), false, false);
        assert!(app.brush_state.library.radial.is_none(), "off the canvas");
        app.radial_right_button(pos2(10.0, 10.0), true, true);
        app.radial_right_button(pos2(11.0, 11.0), false, true);
        let radial = app.brush_state.library.radial.as_ref().unwrap();
        assert_eq!(radial.names, ["A", "B"]);
        app.pick_radial_slice(1);
        assert!(app.brush_state.library.radial.is_none());
        assert_eq!(app.brush_state.active_preset.as_deref(), Some("B"));
        assert_eq!(app.brush_state.library.file.recent, ["B"]);
    }
}
