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
const VERSION: u32 = 2;
pub type FolderId = u64;
pub type PresetId = u64;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BrushFolder {
    pub id: FolderId,
    pub name: String,
    pub parent: Option<FolderId>,
    pub brushes: Vec<PresetId>,
}

#[derive(Clone, Debug)]
pub enum LibraryDrag {
    Brush(PresetId),
    Folder(FolderId),
}
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
    pub preset_ids: BTreeMap<String, PresetId>,
    pub folders: Vec<BrushFolder>,
    pub next_id: u64,
    pub installed_packs: Vec<String>,
    pub pack_presets: BTreeMap<String, String>,
    pub selected_folder: Option<FolderId>,
    /// The pack's category tags were given out (once: ones the user
    /// removes stay removed).
    pub pack_tags_installed: bool,
}

impl Default for LibraryFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            tags: BTreeMap::new(),
            favourites: Vec::new(),
            recent: Vec::new(),
            defaults_installed: false,
            preset_ids: BTreeMap::new(),
            folders: Vec::new(),
            next_id: 1,
            installed_packs: Vec::new(),
            pack_presets: BTreeMap::new(),
            selected_folder: None,
            pack_tags_installed: false,
        }
    }
}

impl LibraryFile {
    pub fn folder_indices(
        &self,
        presets: &[BrushPreset],
        folder: FolderId,
        recursive: bool,
    ) -> Vec<usize> {
        // Built once per call: a lookup per brush was quadratic, every frame.
        let index: std::collections::HashMap<PresetId, usize> = presets
            .iter()
            .enumerate()
            .filter_map(|(i, p)| Some((*self.preset_ids.get(&p.name)?, i)))
            .collect();
        let mut pending = vec![folder];
        let mut visited = std::collections::BTreeSet::new();
        let mut indices = Vec::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            if let Some(folder) = self.folders.iter().find(|f| f.id == id) {
                indices.extend(
                    folder
                        .brushes
                        .iter()
                        .filter_map(|id| index.get(id).copied()),
                );
                if recursive {
                    pending.extend(
                        self.folders
                            .iter()
                            .rev()
                            .filter(|f| f.parent == Some(id))
                            .map(|f| f.id),
                    );
                }
            }
        }
        indices
    }

    /// Tag the pack's brushes by their collection, once, keeping the
    /// user's own tags.
    pub fn install_pack_tags(&mut self, manifest: &crate::brush_engine::pack::Manifest) {
        if self.pack_tags_installed {
            return;
        }
        for collection in &manifest.collections {
            for source in &collection.brushes {
                if let Some(name) = self.pack_presets.get(source).cloned() {
                    for tag in collection_tags(&collection.name) {
                        self.add_tag(&name, tag);
                    }
                }
            }
        }
        self.pack_tags_installed = true;
    }

    fn allocate_id(&mut self) -> u64 {
        self.next_id = self
            .next_id
            .max(1)
            .max(self.preset_ids.values().copied().max().unwrap_or(0) + 1)
            .max(self.folders.iter().map(|f| f.id).max().unwrap_or(0) + 1);
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn preset_id(&mut self, name: &str) -> PresetId {
        if let Some(&id) = self.preset_ids.get(name) {
            return id;
        }
        let id = self.allocate_id();
        self.preset_ids.insert(name.into(), id);
        id
    }

    pub fn create_folder(&mut self, name: &str, parent: Option<FolderId>) -> FolderId {
        let id = self.allocate_id();
        self.folders.push(BrushFolder {
            id,
            name: name.into(),
            parent,
            brushes: Vec::new(),
        });
        id
    }

    pub fn brush_folder(&self, name: &str) -> Vec<String> {
        let Some(id) = self.preset_ids.get(name) else {
            return Vec::new();
        };
        let mut current = self
            .folders
            .iter()
            .find(|f| f.brushes.contains(id))
            .map(|f| f.id);
        let mut names = Vec::new();
        for _ in 0..self.folders.len() {
            let Some(f) = current.and_then(|id| self.folders.iter().find(|f| f.id == id)) else {
                break;
            };
            names.push(f.name.clone());
            current = f.parent;
        }
        names.reverse();
        names
    }

    pub fn ensure_folder_path(&mut self, names: &[String]) -> Option<FolderId> {
        let mut parent = None;
        for name in names.iter().filter(|n| !n.trim().is_empty()) {
            parent = Some(
                self.folders
                    .iter()
                    .find(|f| f.parent == parent && f.name == *name)
                    .map(|f| f.id)
                    .unwrap_or_else(|| self.create_folder(name, parent)),
            );
        }
        parent
    }

    pub fn folder_path(&self, id: FolderId) -> String {
        let mut names = Vec::new();
        let mut current = Some(id);
        for _ in 0..self.folders.len() {
            let Some(folder) = current.and_then(|id| self.folders.iter().find(|f| f.id == id))
            else {
                break;
            };
            names.push(folder.name.as_str());
            current = folder.parent;
        }
        names.reverse();
        names.join(" / ")
    }

    pub fn move_brush(&mut self, id: PresetId, destination: FolderId, before: Option<PresetId>) {
        if !self.folders.iter().any(|f| f.id == destination) || before == Some(id) {
            return;
        }
        for folder in &mut self.folders {
            folder.brushes.retain(|&b| b != id);
        }
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == destination) {
            let at = before
                .and_then(|b| folder.brushes.iter().position(|&v| v == b))
                .unwrap_or(folder.brushes.len());
            folder.brushes.insert(at, id);
        }
    }

    pub fn move_folder(
        &mut self,
        id: FolderId,
        parent: Option<FolderId>,
        before: Option<FolderId>,
    ) -> bool {
        let mut ancestor = parent;
        for _ in 0..=self.folders.len() {
            let Some(a) = ancestor else { break };
            if a == id {
                return false;
            }
            let Some(folder) = self.folders.iter().find(|f| f.id == a) else {
                return false;
            };
            ancestor = folder.parent;
        }
        if ancestor.is_some() {
            return false;
        }
        let Some(at) = self.folders.iter().position(|f| f.id == id) else {
            return false;
        };
        let mut folder = self.folders.remove(at);
        folder.parent = parent;
        let at = before
            .and_then(|id| self.folders.iter().position(|f| f.id == id))
            .unwrap_or(self.folders.len());
        self.folders.insert(at, folder);
        true
    }

    pub fn delete_folder(&mut self, id: FolderId) {
        let Some(at) = self.folders.iter().position(|f| f.id == id) else {
            return;
        };
        let folder = self.folders.remove(at);
        for child in &mut self.folders {
            if child.parent == Some(id) {
                child.parent = folder.parent;
            }
        }
        if let Some(parent) = folder
            .parent
            .and_then(|id| self.folders.iter_mut().find(|f| f.id == id))
        {
            parent.brushes.extend(folder.brushes);
        } else if !folder.brushes.is_empty() {
            let root = self
                .folders
                .iter()
                .find(|f| f.name == "My brushes" && f.parent.is_none())
                .map(|f| f.id)
                .unwrap_or_else(|| self.create_folder("My brushes", None));
            self.folders
                .iter_mut()
                .find(|f| f.id == root)
                .unwrap()
                .brushes
                .extend(folder.brushes);
        }
    }

    pub fn reconcile(&mut self, presets: &[BrushPreset]) {
        self.version = VERSION;
        let root = self
            .folders
            .iter()
            .find(|f| f.name == "My brushes" && f.parent.is_none())
            .map(|f| f.id)
            .unwrap_or_else(|| self.create_folder("My brushes", None));
        // Runs every frame the presets window is open: one set, not a scan
        // of every folder per preset.
        let placed: std::collections::HashSet<PresetId> = self
            .folders
            .iter()
            .flat_map(|f| f.brushes.iter().copied())
            .collect();
        for preset in presets {
            let id = self.preset_id(&preset.name);
            if !placed.contains(&id) {
                self.move_brush(id, root, None);
            }
        }
    }
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
        if let Some(id) = self.preset_ids.remove(name) {
            for folder in &mut self.folders {
                folder.brushes.retain(|&b| b != id);
            }
        }
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
    Folder(FolderId),
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
    pub drag: Option<LibraryDrag>,
    pub folder_name: String,
    pub tag_filter: Option<String>,
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
        Shelf::Folder(id) => lib.folder_indices(presets, *id, !search.trim().is_empty()),
    };
    let mut shown: Vec<_> = indices.into_iter().filter(found).collect();
    if matches!(shelf, Shelf::Folder(_)) {
        shown.sort_by_key(|&i| !lib.is_favourite(&presets[i].name));
    }
    shown
}

/// Search/tag filters retain folder scope and can find brushes in descendants.
pub fn filtered_presets(
    presets: &[BrushPreset],
    lib: &LibraryFile,
    shelf: &Shelf,
    search: &str,
    tag: Option<&str>,
) -> Vec<usize> {
    let mut shown = if let Shelf::Folder(id) = shelf
        && tag.is_some()
    {
        lib.folder_indices(presets, *id, true)
    } else {
        shown_presets(presets, lib, shelf, search)
    };
    shown.retain(|&i| {
        matches_search(&presets[i].name, lib.tags(&presets[i].name), search)
            && tag.is_none_or(|tag| {
                lib.tags(&presets[i].name)
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(tag))
            })
    });
    if matches!(shelf, Shelf::Folder(_)) {
        shown.sort_by_key(|&i| !lib.is_favourite(&presets[i].name));
    }
    shown
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

/// Source categories describe the imported brushes more reliably than their names.
pub fn collection_tags(category: &str) -> &'static [&'static str] {
    match category {
        "Pencils" => &["Sketching", "Pencil"],
        "Sketching" => &["Sketching"],
        "Pens" | "Inks" | "Inking" => &["Inking"],
        "Markers" => &["Painting", "Marker"],
        "Pastels" => &["Sketching", "Pastel", "Texture"],
        "Oils" => &["Painting", "Oil"],
        "Paints" | "Painting" => &["Painting"],
        "Gouache" => &["Painting", "Gouache"],
        "Watercolors" | "Water" => &["Painting", "Watercolor"],
        "Charcoals" => &["Sketching", "Charcoal", "Texture"],
        "Basics" => &["Painting", "Basic"],
        "Lettering" | "Calligraphy" => &["Inking", "Lettering"],
        "Comics" => &["Inking", "Comics"],
        "Design" => &["Design"],
        "Grunge" | "Textures" | "Materials" => &["Texture"],
        "Street Art" | "Spraypaints" => &["Texture", "Spray"],
        "Digital" => &["Painting", "Digital"],
        "Airbrushing" => &["Painting", "Airbrush"],
        "Creative" | "Artistic" => &["Painting", "Effects"],
        "Drawing" => &["Sketching", "Painting"],
        "Abstract" | "Effects" => &["Effects"],
        "Elements" => &["Nature", "Effects"],
        "Vintage" => &["Vintage", "Texture"],
        "Luminance" => &["Light", "Effects"],
        "Industrial" => &["Industrial", "Texture"],
        "Organic" | "Earth" => &["Nature", "Texture"],
        _ => &[],
    }
}

/// The folder under the default brushes a built-in preset goes in, by
/// its first tag (with the pack's brushes of that kind).
pub fn default_folder(name: &str) -> &'static str {
    let tags = default_tags(name);
    let has = |tag: &str| tags.contains(&tag);
    if has("Erasers") {
        "Erasers"
    } else if has("Pixel") {
        "Pixel"
    } else if has("3D") || (has("Effects") && !has("Texture") && !has("Sketching")) {
        "Effects"
    } else if has("Sketching") {
        "Sketching"
    } else if has("Inking") {
        "Inking"
    } else if has("Texture") {
        "Textures"
    } else {
        "Painting"
    }
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
        "Watercolour Wash" | "Wet Round" | "Water (Re-wet)" | "Drippy Ink" => &["Painting", "Wet"],
        _ => krita_default_tags(name),
    }
}

/// The app's tags, for the presets that came from Krita's defaults (whose
/// tags are kept in the default brushes file, by their names here).
const TAGS: &[&str] = &[
    "Sketching",
    "Inking",
    "Painting",
    "Texture",
    "Effects",
    "Erasers",
    "Pixel",
    "3D",
    "Krita",
];

/// The tags the default brushes file gives the preset called `name`.
fn krita_default_tags(name: &str) -> &'static [&'static str] {
    static TAGGED: std::sync::OnceLock<std::collections::HashMap<String, Vec<&'static str>>> =
        std::sync::OnceLock::new();
    let tagged = TAGGED.get_or_init(|| {
        let file = PainterApp::default_brushes_file();
        let presets = crate::brush_engine::preset_file::decode_with_meta(file).unwrap_or_default();
        (presets.into_iter())
            .map(|(p, meta)| {
                let tags = TAGS
                    .iter()
                    .copied()
                    .filter(|t| meta.tags.iter().any(|m| m == t));
                (p.name, tags.collect())
            })
            .collect()
    });
    tagged.get(name).map_or(&[], Vec::as_slice)
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
        let loaded = file.clone();
        for preset in &self.brush_state.presets {
            if !file.tags.contains_key(&preset.name) {
                let tags = default_tags(&preset.name).iter().map(|t| t.to_string());
                file.tags.insert(preset.name.clone(), tags.collect());
            }
        }
        file.reconcile(&self.brush_state.presets);
        if let Some(id) = file
            .selected_folder
            .filter(|id| file.folders.iter().any(|f| f.id == *id))
        {
            self.brush_state.library.shelf = Shelf::Folder(id);
        }
        // Written back only when a migration or new preset changed it.
        self.brush_state.library.dirty = file != loaded;
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
    fn favorites_pin_in_their_folder_and_reference_the_original_brush() {
        let ps = presets(&["A", "B", "C"]);
        let mut lib = LibraryFile::default();
        lib.reconcile(&ps);
        let folder = lib.folders[0].id;
        let original = lib.folders[0].brushes.clone();
        lib.toggle_favourite("B");
        assert_eq!(
            shown_presets(&ps, &lib, &Shelf::Folder(folder), ""),
            [1, 0, 2]
        );
        assert_eq!(shown_presets(&ps, &lib, &Shelf::Favourites, ""), [1]);
        assert_eq!(
            lib.folders[0].brushes, original,
            "pinning doesn't move or duplicate the brush"
        );
        let mut loaded: LibraryFile =
            serde_json::from_slice(&serde_json::to_vec(&lib).unwrap()).unwrap();
        assert_eq!(shown_presets(&ps, &loaded, &Shelf::Favourites, ""), [1]);
        loaded.toggle_favourite("B");
        assert_eq!(
            shown_presets(&ps, &loaded, &Shelf::Folder(folder), ""),
            [0, 1, 2]
        );
        assert!(shown_presets(&ps, &loaded, &Shelf::Favourites, "").is_empty());
    }

    #[test]
    fn category_tags_seed_once_and_filter_descendants_without_leaving_the_folder() {
        use crate::brush_engine::pack::{Collection, Manifest, PACK_ID};
        let ps = presets(&["New Pencil", "New Ink", "Unrelated"]);
        let mut lib = LibraryFile::default();
        lib.reconcile(&ps);
        let root = lib.create_folder("Default brushes", None);
        let pencils = lib.create_folder("Pencils", Some(root));
        let inks = lib.create_folder("Inking", Some(root));
        let a = lib.preset_id("New Pencil");
        let b = lib.preset_id("New Ink");
        lib.move_brush(a, pencils, None);
        lib.move_brush(b, inks, None);
        lib.pack_presets
            .insert("source pencil".into(), "New Pencil".into());
        lib.pack_presets
            .insert("source ink".into(), "New Ink".into());
        lib.add_tag("New Pencil", "Personal");
        let manifest = Manifest {
            version: 2,
            id: PACK_ID.into(),
            file: "pack.rpbrush".into(),
            collections: vec![
                Collection {
                    name: "Pencils".into(),
                    brushes: vec!["source pencil".into()],
                },
                Collection {
                    name: "Inking".into(),
                    brushes: vec!["source ink".into()],
                },
            ],
        };
        lib.install_pack_tags(&manifest);
        assert_eq!(lib.tags("New Pencil"), ["Personal", "Sketching", "Pencil"]);
        assert_eq!(
            filtered_presets(&ps, &lib, &Shelf::Folder(root), "", Some("Pencil")),
            [0]
        );
        assert_eq!(shown_presets(&ps, &lib, &Shelf::Folder(root), "ink"), [1]);
        assert!(
            filtered_presets(&ps, &lib, &Shelf::Folder(pencils), "", Some("Inking")).is_empty()
        );
        lib.remove_tag("New Pencil", "Pencil");
        lib.install_pack_tags(&manifest);
        assert_eq!(
            lib.tags("New Pencil"),
            ["Personal", "Sketching"],
            "removed default tags stay removed"
        );
    }

    #[test]
    fn folder_moves_preserve_identity_order_and_reject_cycles() {
        let mut lib = LibraryFile::default();
        let ps = presets(&["A", "B", "C"]);
        lib.reconcile(&ps);
        let a = lib.preset_id("A");
        let b = lib.preset_id("B");
        let root = lib.create_folder("Library", None);
        let child = lib.create_folder("Inks", Some(root));
        lib.move_brush(a, child, None);
        lib.move_brush(b, child, Some(a));
        assert_eq!(shown_presets(&ps, &lib, &Shelf::Folder(child), ""), [1, 0]);
        assert!(!lib.move_folder(root, Some(child), None));
        assert_eq!(lib.folder_path(child), "Library / Inks");
        lib.delete_folder(child);
        assert_eq!(shown_presets(&ps, &lib, &Shelf::Folder(root), ""), [1, 0]);
        let saved = serde_json::to_vec(&lib).unwrap();
        let mut restored: LibraryFile = serde_json::from_slice(&saved).unwrap();
        restored.reconcile(&ps);
        assert_eq!(restored, lib);
        assert_eq!(restored.preset_id("A"), a);
    }

    #[test]
    fn old_library_migration_keeps_tags_stars_and_recent() {
        let mut lib: LibraryFile = serde_json::from_str(
            r#"{"version":1,"tags":{"A":["Ink"]},"favourites":["A"],"recent":["A"]}"#,
        )
        .unwrap();
        lib.reconcile(&presets(&["A", "B"]));
        assert_eq!(lib.tags("A"), ["Ink"]);
        assert!(lib.is_favourite("A"));
        assert_eq!(lib.recent, ["A"]);
        assert_eq!(lib.preset_ids.len(), 2);
        assert_eq!(lib.folders[0].brushes.len(), 2);
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
        let tag = |lib: &LibraryFile, t: &str| filtered_presets(&ps, lib, &Shelf::All, "", Some(t));
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

    /// Rebuild `assets/default-brushes.rpbrush`: the app's own presets,
    /// then Krita's defaults (from an installed Krita) as this app imports
    /// them, less those it can't paint like Krita.
    #[test]
    #[ignore = "rewrites the default brushes from an installed Krita"]
    fn regenerate_default_brushes_from_krita() {
        const BUNDLE: &str = "/usr/share/krita/bundles/Krita_4_Default_Resources.bundle";
        // Engines with no counterpart (experiment, clone, deform, filter).
        const SKIP: &[&str] = &[
            "t)_Shapes_Fill",
            "v)_Clone_Tool",
            "v)_Distort_Grow",
            "v)_Distort_Move",
            "v)_Distort_Shrink",
            "x)_Filter_Blur",
            "x)_Filter_Sharpen",
        ];
        let bytes = std::fs::read(BUNDLE).expect("Krita installed");
        let entries = crate::project::zip::read_all(&bytes).unwrap();
        let manifest = entries
            .iter()
            .find(|(n, _)| n == "META-INF/manifest.xml")
            .unwrap();
        let manifest = String::from_utf8_lossy(&manifest.1);
        // Krita's tags for the preset in `file`, as this app's.
        let krita_tags = |file: &str| -> Vec<String> {
            let entry = format!("full-path=\"paintoppresets/{file}.kpp\"");
            let Some(at) = manifest.find(&entry) else {
                return vec!["Krita".into()];
            };
            let rest = &manifest[at..];
            let rest = &rest[..rest.find("</manifest:file-entry>").unwrap_or(rest.len())];
            let mut tags = vec!["Krita".to_string()];
            for tag in rest.split("<manifest:tag>").skip(1) {
                let ours = match tag.split('<').next().unwrap_or("") {
                    "Ink" => "Inking",
                    "Sketch" => "Sketching",
                    "Paint" | "Digital" => "Painting",
                    "Textures" => "Texture",
                    "FX" => "Effects",
                    "Erasers" => "Erasers",
                    "Pixel Art" => "Pixel",
                    _ => continue,
                };
                if !tags.iter().any(|t| t == ours) {
                    tags.push(ours.into());
                }
            }
            if file.contains("Normal_Map") {
                tags.push("3D".into());
            }
            tags
        };
        let own: Vec<_> =
            crate::brush_engine::preset_file::decode_with_meta(PainterApp::default_brushes_file())
                .unwrap()
                .into_iter()
                .filter(|(_, meta)| !meta.tags.iter().any(|t| t == "Krita"))
                .collect();
        let imported = crate::brush_engine::import::import("krita.bundle", &bytes).unwrap();
        let (mut presets, mut meta): (Vec<_>, Vec<_>) = own
            .into_iter()
            .map(|(p, _)| (p, crate::brush_engine::preset_file::PresetMeta::default()))
            .unzip();
        for mut preset in imported.presets {
            let file = preset.name.clone();
            if SKIP.contains(&file.as_str()) {
                continue;
            }
            // "b)_Basic-2_Opacity": "Basic-2 Opacity".
            let name = file.split_once(")_").map_or(file.as_str(), |(_, n)| n);
            preset.name = name.replace('_', " ");
            if presets.iter().any(|p| p.name == preset.name) {
                preset.name += " (Krita)";
            }
            meta.push(crate::brush_engine::preset_file::PresetMeta {
                tags: krita_tags(&file),
                favourite: false,
                folder: Vec::new(),
            });
            presets.push(preset);
        }
        let out = crate::brush_engine::preset_file::encode_with_meta(&presets, &meta).unwrap();
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/default-brushes.rpbrush"
        );
        std::fs::write(path, out).unwrap();
        for note in imported.notes {
            println!("{note}");
        }
        println!("{} presets", presets.len());
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
            folder: Vec::new(),
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
