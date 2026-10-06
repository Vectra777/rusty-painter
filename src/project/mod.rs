//! `.rpainter` project files. The file is an OpenRaster archive (a ZIP with
//! a flattened image and a thumbnail, so file managers and other painting
//! apps can show it), holding the project data as one extra entry: a
//! versioned header (JSON) plus a blob area for tiles and undo history.
//! [`encode_project`] and [`decode_project`] are the two ends; save/open on
//! the app wrap them. Older saves are the bare project data, still read.

use crate::canvas::blend_modes::{BlendSpace, LayerBlend};
use crate::{
    PainterApp,
    app::{
        document::{ColorModel, TILE_SIZE, validate_canvas_size},
        tools::Tool,
    },
    canvas::{
        Canvas,
        history::{History, LayerHistoryOp, TileSnapshot, UndoAction},
        storage::{
            CanvasLayerSnapshot, CanvasTileSnapshot, DocumentState, Layer, LayerId, LayerSwap,
        },
    },
};
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

mod blobs;
#[cfg(feature = "sut-import")]
pub(crate) mod clip;
mod convert;
pub(crate) mod export;
pub(crate) mod kra;
mod preview;
pub(crate) mod psd;
pub(crate) mod svg;
pub(crate) mod zip;

use blobs::{StoredBlob, push_blobs, read_blob};
use convert::{
    StoredColor, StoredColorModel, StoredLayerHistoryOp, StoredSelectionShape, StoredTransformInfo,
};

const MAGIC: &[u8; 8] = b"RPNTV001";
/// Where the project data sits inside the OpenRaster archive.
const PROJECT_ENTRY: &str = "rusty-painter/project.rpnt";
const PROJECT_FORMAT: &str = "rusty-painter-project";
/// 3: one undo history for the document (2 kept one per layer).
const PROJECT_VERSION: u32 = 3;
/// The oldest version still read.
const OLDEST_PROJECT_VERSION: u32 = 2;

pub(crate) struct LoadedProject {
    pub canvas: Canvas,
    pub color_model: ColorModel,
    pub history: History,
    pub guides: Option<crate::app::tools::guides::StoredGuides>,
    pub saved_selections: Vec<crate::app::tools::select::SavedSelection>,
}

/// Everything a project file holds, copied off the app: it's encoded and
/// written on another thread while painting goes on.
pub(crate) struct ProjectSnapshot {
    canvas: Canvas,
    color_model: ColorModel,
    history: History,
    guides: crate::app::tools::guides::StoredGuides,
    saved_selections: Vec<crate::app::tools::select::SavedSelection>,
}

impl ProjectSnapshot {
    /// The document as it is now (the stroke worker must be idle). Only
    /// copies: the slow part is [`Self::encode`].
    pub(crate) fn capture(app: &PainterApp) -> Self {
        Self {
            canvas: app.canvas.detached_copy(),
            color_model: app.workspace.color_model,
            history: app.layer_state.history.clone(),
            guides: crate::app::tools::guides::StoredGuides::from_app(app),
            saved_selections: app.workspace.select.saved.clone(),
        }
    }

    /// The `.rpainter` file's bytes.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        let data = encode_project_data(self)?;
        let flat = self.canvas.flatten_final();
        let thumbnail =
            preview::encode_png(preview::thumbnail(&flat, preview::THUMBNAIL_MAX_EDGE))?;
        let [w, h] = flat.size;
        let merged = preview::encode_png(flat)?;
        // One layer: the flattened picture. Apps that read OpenRaster open
        // that; the layers, undo and settings are in the project entry.
        let stack = format!(
            "<?xml version='1.0' encoding='UTF-8'?>\n\
             <image version=\"0.0.3\" w=\"{w}\" h=\"{h}\">\n\
             <stack>\n\
             <layer name=\"{}\" src=\"mergedimage.png\" x=\"0\" y=\"0\" \
             opacity=\"1.000\" visibility=\"visible\"/>\n\
             </stack>\n\
             </image>\n",
            crate::APP_NAME
        );

        let mut zip = zip::ZipWriter::default();
        // The type check reads "mimetype" as the first entry, uncompressed.
        zip.add("mimetype", b"image/openraster")?;
        zip.add("stack.xml", stack.as_bytes())?;
        zip.add("mergedimage.png", &merged)?;
        zip.add("Thumbnails/thumbnail.png", &thumbnail)?;
        zip.add(PROJECT_ENTRY, &data)?;
        zip.finish()
    }

    /// Encode and write it to `path` (a whole file or nothing).
    pub(crate) fn write(&self, path: &Path) -> Result<(), String> {
        write_atomically(path, &self.encode()?).map_err(|err| format!("Save failed: {err}"))
    }
}

/// A document read from disk, ready to replace the open one.
pub(crate) enum OpenedDocument {
    /// Another app's (Photoshop, Krita, Clip Studio).
    Foreign(Canvas),
    Project(Box<LoadedProject>),
}

/// Read and decode the document at `path` (slow: off the UI thread).
pub(crate) fn read_document(path: &Path) -> Result<OpenedDocument, String> {
    let bytes = fs::read(path).map_err(|err| format!("Open failed: {err}"))?;
    decode_document(&path.to_string_lossy(), &bytes)
}

/// Decode the document file called `name` (its extension says which app's)
/// from its contents (slow: off the UI thread).
pub(crate) fn decode_document(name: &str, bytes: &[u8]) -> Result<OpenedDocument, String> {
    let extension = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if let Some(decode) = extension.as_deref().and_then(foreign_decoder) {
        return Ok(OpenedDocument::Foreign(decode(bytes)?.into_canvas()?));
    }
    decode_project(bytes).map(|p| OpenedDocument::Project(Box::new(p)))
}

/// Replace `path` with `bytes` so that a crash, a full disk or a power cut
/// part-way leaves the old file whole: written beside it, flushed to the
/// disk, then renamed over it.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    // Unique per write: two may be on their way at once (an autosave and a
    // save, on their own threads).
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| e.to_string())
}

/// Other apps' documents this opens (lower-case extensions).
pub(crate) const FOREIGN_EXTENSIONS: &[&str] = &[
    "psd",
    "kra",
    #[cfg(feature = "sut-import")]
    "clip",
];

type Decoder = fn(&[u8]) -> Result<psd::PsdDocument, String>;

/// The reader for another app's document, by lower-case extension.
fn foreign_decoder(extension: &str) -> Option<Decoder> {
    Some(match extension {
        "psd" => psd::decode_psd,
        "kra" => kra::decode_kra,
        #[cfg(feature = "sut-import")]
        "clip" => clip::decode_clip,
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) fn load_project(path: impl AsRef<Path>) -> Result<LoadedProject, String> {
    decode_project(&fs::read(path).map_err(|err| format!("Open failed: {err}"))?)
}

impl PainterApp {
    /// Save to `path` now (the library's saves, which other steps wait
    /// for). Saving from the menu goes through
    /// [`Self::save_project_in_background`].
    pub(crate) fn save_project_to_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let bytes = self.encode_document()?;
        write_atomically(&with_project_extension(path.as_ref()), &bytes)
            .map_err(|err| format!("Save failed: {err}"))?;
        self.saved_by_user();
        Ok(())
    }

    /// The document as a project file.
    pub(crate) fn encode_document(&mut self) -> Result<Vec<u8>, String> {
        // Saves every painted pixel and files the stroke into undo history.
        self.release_canvas();
        self.project_snapshot().encode()
    }

    /// The document as a project file can hold it: every painted pixel
    /// (strokes filed into the history), without the quick mask, shader
    /// layers at their current frame. The stroke worker must be idle.
    fn project_snapshot(&mut self) -> ProjectSnapshot {
        self.sync_stroke_worker();
        // The mask layer isn't part of the document.
        self.quick_mask_leave();
        // Shader layers are saved with their current frame.
        self.bake_shader_layers();
        ProjectSnapshot::capture(self)
    }

    /// Save, waiting for nothing: what's painted is copied once the stroke
    /// worker has painted what's queued, then encoded and written on
    /// another thread while painting goes on.
    pub(crate) fn save_project_in_background(&mut self, path: impl AsRef<Path>) {
        let path = with_project_extension(path.as_ref());
        self.when_strokes_painted(move |app| {
            let snapshot = app.project_snapshot();
            let version = app.doc_version();
            app.spawn_job(Some("Saving…"), move || {
                let result = snapshot.write(&path);
                Box::new(move |app: &mut PainterApp| match result {
                    Ok(()) => app.saved_by_user_at(version),
                    Err(err) => app.report(err),
                })
            });
        });
    }

    /// Open `path`, read and decoded on another thread (the canvas waits
    /// meanwhile); `then` runs once it's the document.
    pub(crate) fn open_project_in_background(
        &mut self,
        path: impl AsRef<Path>,
        then: impl FnOnce(&mut PainterApp) + Send + 'static,
    ) {
        let path = path.as_ref().to_path_buf();
        self.spawn_job(Some("Opening…"), move || {
            let result = read_document(&path);
            Box::new(move |app: &mut PainterApp| match result {
                // The old document goes once the stroke worker lets go of it.
                Ok(doc) => app.when_strokes_painted(move |app| {
                    app.open_document(doc);
                    then(app);
                }),
                Err(err) => app.report(err),
            })
        });
    }

    /// Open `path` now (the library's opens, which other steps wait for).
    pub(crate) fn load_project_from_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let doc = read_document(path)?;
        self.open_document(doc);
        self.workspace.library.project = crate::ui::library::is_project(path).then(|| path.into());
        Ok(())
    }

    /// Make `doc` the document.
    pub(crate) fn open_document(&mut self, doc: OpenedDocument) {
        match doc {
            OpenedDocument::Foreign(canvas) => self.replace_document(canvas, History::new()),
            OpenedDocument::Project(loaded) => {
                let loaded = *loaded;
                self.replace_document(loaded.canvas, loaded.history);
                self.workspace.color_model = loaded.color_model;
                if let Some(guides) = loaded.guides {
                    guides.apply(self);
                }
                self.workspace.select.saved = loaded.saved_selections;
            }
        }
        self.active_tool = Tool::Brush;
        self.workspace.library.has_document = true;
    }
}

/// Layer ids beyond this can only come from a damaged file (and would run
/// out of room for new ones).
const MAX_LAYER_ID: u64 = 1 << 53;

/// A loaded layer list the rest of the app can trust: each id once, and
/// folders that nest (a damaged file's ring of folders inside each other
/// made the layers in it vanish, and "merge visible" loop for ever). A
/// parent that isn't a folder, or would close a ring, is dropped: the layer
/// shows at the top level.
fn checked_layer_tree(
    mut layers: Vec<CanvasLayerSnapshot>,
) -> Result<Vec<CanvasLayerSnapshot>, String> {
    use crate::canvas::storage::LayerKind;
    use std::collections::{HashMap, HashSet};
    let mut ids = HashSet::new();
    if layers
        .iter()
        .any(|l| l.id.0 >= MAX_LAYER_ID || !ids.insert(l.id))
    {
        return Err("Damaged project: layer ids repeat".to_string());
    }
    let folders: HashSet<LayerId> = layers
        .iter()
        .filter(|l| l.kind == LayerKind::Group)
        .map(|l| l.id)
        .collect();
    for l in &mut layers {
        if l.parent.is_some_and(|p| p == l.id || !folders.contains(&p)) {
            l.parent = None;
        }
    }
    // Break rings: walking up from any layer must reach the top within as
    // many steps as there are layers.
    loop {
        let parent: HashMap<LayerId, LayerId> = layers
            .iter()
            .filter_map(|l| Some((l.id, l.parent?)))
            .collect();
        let in_ring = layers.iter().position(|l| {
            let mut at = l.id;
            for _ in 0..=layers.len() {
                match parent.get(&at) {
                    Some(&p) => at = p,
                    None => return false,
                }
            }
            true
        });
        match in_ring {
            Some(i) => layers[i].parent = None,
            None => return Ok(layers),
        }
    }
}

pub(crate) fn encode_project(app: &PainterApp) -> Result<Vec<u8>, String> {
    ProjectSnapshot::capture(app).encode()
}

/// The project itself: the header and blob area.
fn encode_project_data(snapshot: &ProjectSnapshot) -> Result<Vec<u8>, String> {
    let mut blobs = Vec::new();
    let manifest = ProjectFile::from_snapshot(snapshot, &mut blobs)?;
    let manifest =
        serde_json::to_vec(&manifest).map_err(|err| format!("Serialize failed: {err}"))?;
    let manifest_len = u64::try_from(manifest.len()).map_err(|_| "Manifest is too large")?;

    let mut out = Vec::with_capacity(MAGIC.len() + 8 + manifest.len() + blobs.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&manifest_len.to_le_bytes());
    out.extend_from_slice(&manifest);
    out.extend_from_slice(&blobs);
    Ok(out)
}

pub(crate) fn decode_project(bytes: &[u8]) -> Result<LoadedProject, String> {
    if bytes.starts_with(zip::SIGNATURE) {
        decode_project_data(zip::read_entry(bytes, PROJECT_ENTRY)?)
    } else {
        decode_project_data(bytes)
    }
}

fn decode_project_data(bytes: &[u8]) -> Result<LoadedProject, String> {
    if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
        return Err("Unsupported project file".to_string());
    }
    let manifest_start = MAGIC.len() + 8;
    let manifest_len = u64::from_le_bytes(
        bytes[MAGIC.len()..manifest_start]
            .try_into()
            .map_err(|_| "Invalid project header")?,
    ) as usize;
    let manifest_end = manifest_start
        .checked_add(manifest_len)
        .ok_or_else(|| "Invalid project manifest size".to_string())?;
    if manifest_end > bytes.len() {
        return Err("Truncated project file".to_string());
    }

    let manifest: ProjectFile = serde_json::from_slice(&bytes[manifest_start..manifest_end])
        .map_err(|err| format!("Project parse failed: {err}"))?;
    manifest.into_loaded_project(&bytes[manifest_end..])
}

pub(crate) fn with_project_extension(path: &Path) -> std::path::PathBuf {
    if path.extension().and_then(|s| s.to_str()) == Some("rpainter") {
        path.to_path_buf()
    } else {
        path.with_extension("rpainter")
    }
}

fn colors_to_bytes(colors: &[Color32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(colors.len() * 4);
    for color in colors {
        out.extend_from_slice(&color.to_array());
    }
    out
}

fn bytes_to_colors(bytes: Vec<u8>) -> Result<Vec<Color32>, String> {
    if !bytes.len().is_multiple_of(4) {
        return Err("Invalid RGBA byte count".to_string());
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
        .collect())
}

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: String,
    version: u32,
    width: usize,
    height: usize,
    tile_size: usize,
    clear_color: StoredColor,
    color_model: StoredColorModel,
    /// "gamma" for gamma-space blending; absent (older files) = linear.
    #[serde(default)]
    blend_space: Option<String>,
    active_layer_idx: usize,
    layers: Vec<StoredLayer>,
    /// Version 3: the one document history. Version 2: one per layer.
    histories: Vec<StoredHistory>,
    /// The ruler, assistants and mirror painting; absent in older files.
    #[serde(default)]
    guides: Option<crate::app::tools::guides::StoredGuides>,
    /// Select → Save Selection; absent in older files.
    #[serde(default)]
    saved_selections: Vec<StoredSavedSelection>,
}

impl ProjectFile {
    fn from_snapshot(snapshot: &ProjectSnapshot, blobs: &mut Vec<u8>) -> Result<Self, String> {
        let canvas = &snapshot.canvas;
        Ok(Self {
            format: PROJECT_FORMAT.to_string(),
            version: PROJECT_VERSION,
            width: canvas.width(),
            height: canvas.height(),
            tile_size: canvas.tile_size(),
            clear_color: StoredColor::from_color(canvas.clear_color()),
            color_model: StoredColorModel::from(snapshot.color_model),
            blend_space: match canvas.blend_space {
                BlendSpace::Linear => None,
                BlendSpace::Gamma => Some("gamma".to_string()),
            },
            active_layer_idx: canvas.active_layer_idx,
            layers: canvas
                .layer_snapshots()
                .into_iter()
                .map(|layer| StoredLayer::from_snapshot(layer, blobs))
                .collect::<Result<_, _>>()?,
            histories: vec![StoredHistory::from_history(&snapshot.history, blobs)?],
            guides: Some(snapshot.guides.clone()),
            saved_selections: StoredSavedSelection::from_saved(&snapshot.saved_selections, blobs)?,
        })
    }

    fn into_loaded_project(self, blobs: &[u8]) -> Result<LoadedProject, String> {
        if self.format != PROJECT_FORMAT
            || !(OLDEST_PROJECT_VERSION..=PROJECT_VERSION).contains(&self.version)
        {
            return Err("Unsupported project file".to_string());
        }
        validate_canvas_size(self.width, self.height)?;
        if self.tile_size == 0 || self.tile_size != TILE_SIZE {
            return Err("Unsupported tile size".to_string());
        }
        if self.layers.is_empty() {
            return Err("Project has no layers".to_string());
        }

        let layers: Vec<_> = self
            .layers
            .into_iter()
            .enumerate()
            .map(|(idx, layer)| layer.into_snapshot(idx, self.tile_size, blobs))
            .collect::<Result<_, _>>()?;
        let layers = checked_layer_tree(layers)?;
        let mut canvas = Canvas::new(
            self.width,
            self.height,
            self.clear_color.to_color(),
            self.tile_size,
        );
        canvas.replace_layers_from_snapshots(layers, self.active_layer_idx);
        canvas.blend_space = match self.blend_space.as_deref() {
            Some("gamma") => BlendSpace::Gamma,
            _ => BlendSpace::Linear,
        };

        let histories: Vec<History> = self
            .histories
            .into_iter()
            .map(|history| history.into_history(self.tile_size, blobs))
            .collect::<Result<_, _>>()?;
        // Version 2's per-layer histories become one.
        let history = History::merged(histories);

        let saved_selections = self
            .saved_selections
            .into_iter()
            .map(|s| s.into_saved(blobs))
            .collect::<Result<_, _>>()?;

        Ok(LoadedProject {
            canvas,
            color_model: self.color_model.into(),
            history,
            guides: self.guides,
            saved_selections,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredLayer {
    name: String,
    visible: bool,
    opacity: f32,
    locked: bool,
    #[serde(default)]
    alpha_locked: bool,
    /// Stable layer id. Absent in files saved before LayerId existed; such
    /// files fall back to assigning ids by position on load (see
    /// `into_snapshot`), which reproduces the old (position-based) behavior
    /// exactly rather than fixing it retroactively.
    #[serde(default)]
    id: Option<u64>,
    /// Folder/mask data; absent in files saved before those existed.
    #[serde(default)]
    kind: convert::StoredLayerKind,
    #[serde(default)]
    parent: Option<u64>,
    #[serde(default = "default_expanded")]
    expanded: bool,
    /// Blend mode key (`LayerBlend::key`); absent in older files = Normal.
    #[serde(default)]
    blend: Option<String>,
    /// Clipping mask; absent in older files.
    #[serde(default)]
    clipped: bool,
    /// Adjustment layer's filter; absent in older files.
    #[serde(default)]
    adjustment: Option<crate::canvas::filters::Filter>,
    /// Fill layer and border; absent in older files.
    #[serde(
        default,
        skip_serializing_if = "crate::canvas::layer_style::LayerStyle::is_plain"
    )]
    style: crate::canvas::layer_style::LayerStyle,
    /// A text layer's source; absent in older files (and on other layers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<convert::StoredText>,
    /// A vector layer's lines; absent in older files (and on other layers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vector: Option<crate::canvas::vector::VectorLayer>,
    /// A shader layer's shader; absent in older files (and on other layers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shader: Option<crate::canvas::shader::ShaderLayer>,
    /// Impasto heights, as pixels (see `canvas::impasto::to_pixels`);
    /// absent in older files (and on layers without).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    heights: Option<Vec<StoredTile>>,
    /// Layer flags; absent in older files.
    #[serde(default)]
    position_locked: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    reference: bool,
    tiles: Vec<StoredTile>,
}

fn default_expanded() -> bool {
    true
}

impl StoredLayer {
    fn from_snapshot(layer: CanvasLayerSnapshot, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            name: layer.name,
            visible: layer.visible,
            opacity: layer.opacity,
            locked: layer.locked,
            alpha_locked: layer.alpha_locked,
            id: Some(layer.id.0),
            kind: layer.kind.into(),
            parent: layer.parent.map(|p| p.0),
            expanded: layer.expanded,
            blend: Some(layer.blend.key().to_string()),
            clipped: layer.clipped,
            adjustment: layer.adjustment,
            style: layer.style,
            text: convert::StoredText::from_layer(layer.text.as_deref()),
            vector: layer.vector.as_deref().cloned(),
            shader: layer.shader.map(|s| *s),
            heights: match &layer.height {
                Some(map) => {
                    let tiles = map.tiles();
                    let raws: Vec<Vec<u8>> = (tiles.iter())
                        .map(|(_, h)| colors_to_bytes(&crate::canvas::impasto::to_pixels(h)))
                        .collect();
                    let stored = push_blobs(blobs, &raws)?;
                    Some(
                        (tiles.iter().zip(stored))
                            .map(|(((tx, ty), _), rgba_zstd)| StoredTile {
                                tx: *tx,
                                ty: *ty,
                                rgba_zstd,
                            })
                            .collect(),
                    )
                }
                None => None,
            },
            position_locked: layer.position_locked,
            draft: layer.draft,
            reference: layer.reference,
            tiles: {
                use rayon::prelude::*;
                let raws: Vec<Vec<u8>> = layer
                    .tiles
                    .par_iter()
                    .map(|t| colors_to_bytes(&t.data))
                    .collect();
                let stored = push_blobs(blobs, &raws)?;
                layer
                    .tiles
                    .iter()
                    .zip(stored)
                    .map(|(t, rgba_zstd)| StoredTile {
                        tx: t.tx,
                        ty: t.ty,
                        rgba_zstd,
                    })
                    .collect()
            },
        })
    }

    fn into_snapshot(
        self,
        fallback_idx: usize,
        tile_size: usize,
        blobs: &[u8],
    ) -> Result<CanvasLayerSnapshot, String> {
        Ok(CanvasLayerSnapshot {
            id: LayerId(self.id.unwrap_or(fallback_idx as u64)),
            name: self.name,
            visible: self.visible,
            opacity: self.opacity,
            locked: self.locked,
            alpha_locked: self.alpha_locked,
            kind: self.kind.into(),
            parent: self.parent.map(LayerId),
            expanded: self.expanded,
            blend: self
                .blend
                .as_deref()
                .and_then(LayerBlend::from_key)
                .unwrap_or_default(),
            clipped: self.clipped,
            adjustment: self.adjustment,
            style: self.style,
            text: convert::StoredText::into_layer(self.text),
            vector: self.vector.map(Box::new),
            height: match self.heights {
                Some(tiles) => {
                    let map = crate::canvas::impasto::HeightMap::default();
                    for tile in tiles {
                        let t = tile.into_snapshot(tile_size, blobs)?;
                        map.set_tile(
                            (t.tx, t.ty),
                            Some(crate::canvas::impasto::from_pixels(&t.data)),
                        );
                    }
                    Some(Box::new(map))
                }
                None => None,
            },
            shader: self.shader.map(Box::new),
            position_locked: self.position_locked,
            draft: self.draft,
            reference: self.reference,
            tiles: self
                .tiles
                .into_iter()
                .map(|tile| tile.into_snapshot(tile_size, blobs))
                .collect::<Result<_, _>>()?,
        })
    }
}

/// A saved selection: its name, box and coverage bytes.
#[derive(Serialize, Deserialize)]
struct StoredSavedSelection {
    name: String,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    coverage: StoredBlob,
}

impl StoredSavedSelection {
    fn from_saved(
        saved: &[crate::app::tools::select::SavedSelection],
        blobs: &mut Vec<u8>,
    ) -> Result<Vec<Self>, String> {
        let raws: Vec<Vec<u8>> = saved.iter().map(|s| s.mask.data.clone()).collect();
        let stored = push_blobs(blobs, &raws)?;
        Ok(saved
            .iter()
            .zip(stored)
            .map(|(s, coverage)| Self {
                name: s.name.clone(),
                x0: s.mask.x0,
                y0: s.mask.y0,
                w: s.mask.w,
                h: s.mask.h,
                coverage,
            })
            .collect())
    }

    fn into_saved(self, blobs: &[u8]) -> Result<crate::app::tools::select::SavedSelection, String> {
        let data = read_blob(blobs, &self.coverage)?;
        if Some(data.len()) != self.w.checked_mul(self.h) {
            return Err("Invalid saved selection size".to_string());
        }
        Ok(crate::app::tools::select::SavedSelection {
            name: self.name,
            mask: std::sync::Arc::new(crate::selection::SelectionMask::new(
                self.x0, self.y0, self.w, self.h, data,
            )),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredTile {
    tx: i32,
    ty: i32,
    rgba_zstd: StoredBlob,
}

impl StoredTile {
    fn into_snapshot(self, tile_size: usize, blobs: &[u8]) -> Result<CanvasTileSnapshot, String> {
        let data = bytes_to_colors(read_blob(blobs, &self.rgba_zstd)?)?;
        if data.len() != tile_size * tile_size {
            return Err("Invalid tile pixel count".to_string());
        }
        Ok(CanvasTileSnapshot {
            tx: self.tx,
            ty: self.ty,
            data,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredHistory {
    undo: Vec<StoredUndoAction>,
    redo: Vec<StoredUndoAction>,
}

impl StoredHistory {
    fn from_history(history: &History, blobs: &mut Vec<u8>) -> Result<Self, String> {
        let (undo, redo) = history.stacks();
        Ok(Self {
            undo: undo
                .iter()
                .map(|action| StoredUndoAction::from_action(action, blobs))
                .collect::<Result<_, _>>()?,
            redo: redo
                .iter()
                .map(|action| StoredUndoAction::from_action(action, blobs))
                .collect::<Result<_, _>>()?,
        })
    }

    fn into_history(self, tile_size: usize, blobs: &[u8]) -> Result<History, String> {
        Ok(History::from_stacks(
            self.undo
                .into_iter()
                .map(|action| action.into_action(tile_size, blobs))
                .collect::<Result<_, _>>()?,
            self.redo
                .into_iter()
                .map(|action| action.into_action(tile_size, blobs))
                .collect::<Result<_, _>>()?,
        ))
    }
}

#[derive(Serialize, Deserialize)]
struct StoredUndoAction {
    tiles: Vec<StoredTileSnapshot>,
    selection: Option<Option<StoredSelectionShape>>,
    transform: Option<StoredTransformInfo>,
    /// Absent in files saved before layer-structural undo existed; such
    /// files simply have no structural entries to recover (there weren't
    /// any to begin with), same reasoning as the LayerId migration above.
    #[serde(default)]
    layer_action: Option<StoredLayerHistoryOp>,
    /// A canvas resize, crop or rotation: the whole document on the other
    /// side of the step. Absent in older files, which didn't save steps
    /// from before a resize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    document: Option<StoredDocument>,
    /// A merge: the layers it puts in and the ids it takes out. Absent in
    /// older files, which didn't save steps from before a merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merge: Option<StoredMerge>,
}

/// The layers a merge step swaps: see [`crate::canvas::storage::LayerSwap`].
#[derive(Serialize, Deserialize)]
struct StoredMerge {
    /// Entries to put in, at these final positions (ascending).
    layers: Vec<(usize, StoredLayer)>,
    remove: Vec<u64>,
    active_layer_idx: usize,
    applied: (Vec<usize>, Vec<usize>),
}

impl StoredMerge {
    fn from_swap(swap: &LayerSwap, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            layers: swap
                .layers
                .iter()
                .map(|(idx, layer)| {
                    Ok((*idx, StoredLayer::from_snapshot(layer.snapshot(), blobs)?))
                })
                .collect::<Result<_, String>>()?,
            remove: swap.remove.iter().map(|id| id.0).collect(),
            active_layer_idx: swap.active_layer_idx,
            applied: swap.applied.clone(),
        })
    }

    fn into_op(self, tile_size: usize, blobs: &[u8]) -> Result<LayerHistoryOp, String> {
        let layers = self
            .layers
            .into_iter()
            .map(|(idx, layer)| {
                layer
                    .into_snapshot(idx, tile_size, blobs)
                    .map(|snapshot| (idx, Layer::from_snapshot(snapshot)))
            })
            .collect::<Result<_, _>>()?;
        Ok(LayerHistoryOp::Replaced(std::sync::Arc::new(
            std::sync::Mutex::new(LayerSwap {
                layers,
                remove: self.remove.into_iter().map(LayerId).collect(),
                active_layer_idx: self.active_layer_idx,
                applied: self.applied,
            }),
        )))
    }
}

/// The document a resize step swaps in: its size and every layer.
#[derive(Serialize, Deserialize)]
struct StoredDocument {
    width: usize,
    height: usize,
    active_layer_idx: usize,
    layers: Vec<StoredLayer>,
}

impl StoredDocument {
    fn from_state(doc: &DocumentState, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            width: doc.width,
            height: doc.height,
            active_layer_idx: doc.active_layer_idx,
            layers: doc
                .layers
                .iter()
                .map(|layer| StoredLayer::from_snapshot(layer.snapshot(), blobs))
                .collect::<Result<_, _>>()?,
        })
    }

    fn into_op(self, tile_size: usize, blobs: &[u8]) -> Result<LayerHistoryOp, String> {
        validate_canvas_size(self.width, self.height)?;
        if self.layers.is_empty() {
            return Err("Invalid resize step in undo history".to_string());
        }
        let layers: Vec<Layer> = self
            .layers
            .into_iter()
            .enumerate()
            .map(|(idx, layer)| {
                layer
                    .into_snapshot(idx, tile_size, blobs)
                    .map(Layer::from_snapshot)
            })
            .collect::<Result<_, _>>()?;
        let active_layer_idx = self.active_layer_idx.min(layers.len() - 1);
        Ok(LayerHistoryOp::Document(std::sync::Arc::new(
            std::sync::Mutex::new(DocumentState {
                width: self.width,
                height: self.height,
                layers,
                active_layer_idx,
            }),
        )))
    }
}

impl StoredUndoAction {
    fn from_action(action: &UndoAction, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            tiles: {
                use rayon::prelude::*;
                let raws: Vec<Vec<u8>> = action
                    .tiles
                    .par_iter()
                    .map(|t| colors_to_bytes(&t.data.to_vec()))
                    .collect();
                let stored = push_blobs(blobs, &raws)?;
                action
                    .tiles
                    .iter()
                    .zip(stored)
                    .map(|(s, rgba_zstd)| StoredTileSnapshot {
                        tx: s.tx,
                        ty: s.ty,
                        layer_idx: 0,
                        layer_id: Some(s.layer_id.0),
                        x0: s.x0,
                        y0: s.y0,
                        width: s.width,
                        height: s.height,
                        rgba_zstd,
                    })
                    .collect()
            },
            selection: action
                .selection
                .as_ref()
                .map(|shape| shape.as_ref().map(StoredSelectionShape::from)),
            transform: action.transform.as_ref().map(StoredTransformInfo::from),
            layer_action: match &action.layer_action {
                Some(LayerHistoryOp::Document(_) | LayerHistoryOp::Replaced(_)) | None => None,
                Some(op) => Some(StoredLayerHistoryOp::from(op)),
            },
            document: match &action.layer_action {
                Some(LayerHistoryOp::Document(doc)) => Some(StoredDocument::from_state(
                    &doc.lock().unwrap_or_else(|e| e.into_inner()),
                    blobs,
                )?),
                _ => None,
            },
            merge: match &action.layer_action {
                Some(LayerHistoryOp::Replaced(swap)) => Some(StoredMerge::from_swap(
                    &swap.lock().unwrap_or_else(|e| e.into_inner()),
                    blobs,
                )?),
                _ => None,
            },
        })
    }

    fn into_action(self, tile_size: usize, blobs: &[u8]) -> Result<UndoAction, String> {
        let layer_action = match (self.document, self.merge) {
            (Some(document), _) => Some(document.into_op(tile_size, blobs)?),
            (None, Some(merge)) => Some(merge.into_op(tile_size, blobs)?),
            (None, None) => self.layer_action.map(StoredLayerHistoryOp::into_op),
        };
        Ok(UndoAction {
            layer_action,
            tiles: self
                .tiles
                .into_iter()
                .map(|tile| tile.into_snapshot(tile_size, blobs))
                .collect::<Result<_, _>>()?,
            selection: self
                .selection
                .map(|shape| shape.map(StoredSelectionShape::into_shape)),
            transform: self.transform.map(StoredTransformInfo::into_info),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredTileSnapshot {
    tx: i32,
    ty: i32,
    /// Legacy position-based layer reference, kept only so files saved
    /// before LayerId existed can still resolve a layer on load. Ignored
    /// whenever `layer_id` is present.
    #[serde(default)]
    layer_idx: usize,
    #[serde(default)]
    layer_id: Option<u64>,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
    rgba_zstd: StoredBlob,
}

impl StoredTileSnapshot {
    fn into_snapshot(self, tile_size: usize, blobs: &[u8]) -> Result<TileSnapshot, String> {
        let data = bytes_to_colors(read_blob(blobs, &self.rgba_zstd)?)?;
        let fits = |at: usize, len: usize| at.checked_add(len).is_some_and(|end| end <= tile_size);
        if self.width == 0
            || self.height == 0
            || !fits(self.x0, self.width)
            || !fits(self.y0, self.height)
            || Some(data.len()) != self.width.checked_mul(self.height)
        {
            return Err("Invalid undo tile snapshot".to_string());
        }
        let layer_id = LayerId(self.layer_id.unwrap_or(self.layer_idx as u64));
        Ok(TileSnapshot {
            tx: self.tx,
            ty: self.ty,
            layer_id,
            x0: self.x0,
            y0: self.y0,
            width: self.width,
            height: self.height,
            data: data.into(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{app::state::LayerState, brush_engine::brush::Brush};
    use eframe::egui::Vec2;
    use rayon::ThreadPoolBuilder;

    pub(crate) fn test_app_pub(canvas: Canvas) -> PainterApp {
        test_app(canvas, vec![History::new(), History::new()])
    }

    fn test_app(canvas: Canvas, histories: Vec<History>) -> PainterApp {
        let layer_count = canvas.layers.len();
        PainterApp {
            canvas: std::sync::Arc::new(canvas),
            stroke_worker: Default::default(),
            brush_state: crate::app::state::BrushState::new(
                Brush::new(1.0, 100.0, Color32::BLACK, 10.0),
                Vec::new(),
                ".".into(),
                true,
            ),
            viewport: crate::app::state::ViewportState::new(1.0, Vec2::ZERO),
            render_cache: crate::app::state::RenderCache::new(1, 1),
            layer_state: {
                let mut state = LayerState::new(layer_count);
                state.history = History::merged(histories);
                state
            },
            modal_state: crate::app::state::ModalState::new(
                crate::app::document::NewCanvasSettings::from_canvas(&Canvas::new(
                    1,
                    1,
                    Color32::WHITE,
                    TILE_SIZE,
                )),
            ),
            export_state: crate::app::state::ExportState::new(),
            workspace: crate::app::state::WorkspaceState::new(
                1,
                1,
                std::sync::Arc::new(ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
                ColorModel::Rgba,
            ),
            active_tool: Tool::Brush,
            selection_manager: crate::selection::SelectionManager::new(),

            tablet: None,
        }
    }

    #[test]
    fn deleting_a_masked_layer_takes_the_mask_and_undo_restores_both() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, TILE_SIZE);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        app.add_mask_to_active();
        let owner = app.canvas.layers[1].id;
        assert!(app.canvas.mask_index_of(owner).is_some());
        assert_eq!(app.canvas.layers.len(), 3);

        app.remove_layer(1);
        assert_eq!(
            app.canvas.layers.len(),
            1,
            "layer and its mask are both gone"
        );
        assert_eq!(app.layer_state.layer_ui_colors.len(), 1);

        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), 3);
        assert_eq!(app.layer_state.layer_ui_colors.len(), 3);
        let restored_owner = app.canvas.layers[1].id;
        assert_eq!(restored_owner, owner);
        assert!(
            app.canvas.mask_index_of(owner).is_some(),
            "mask is linked again"
        );
    }

    #[test]
    fn moving_into_a_folder_and_undo_restores_the_parent() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, TILE_SIZE);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        app.add_folder();
        let folder = app.canvas.layers[2].id;
        let layer = app.canvas.layers[1].id;
        app.move_layer(1, 2, Some(folder));
        let moved = app.canvas.layer_index_of(layer).unwrap();
        assert_eq!(app.canvas.layers[moved].parent, Some(folder));

        app.apply_history(false);
        let back = app.canvas.layer_index_of(layer).unwrap();
        assert_eq!(app.canvas.layers[back].parent, None);
        assert_eq!(back, 1);
    }

    #[test]
    fn canvas_mut_finishes_the_stroke_and_files_its_undo_step() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, TILE_SIZE);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.start_stroke_with_pressure(Vec2::new(10.0, 10.0), 1.0);
        app.add_stroke_point(Vec2::new(30.0, 20.0), 1.0);
        assert!(app.brush_state.is_drawing);

        let _ = app.canvas_mut();

        assert!(!app.brush_state.is_drawing);
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        assert_ne!(
            app.canvas.get_layer_tile_data(1, 0, 0),
            Some(vec![Color32::TRANSPARENT; TILE_SIZE * TILE_SIZE]),
            "the stroke was painted"
        );
    }

    fn layer_pixels(app: &PainterApp, layer: usize) -> Vec<Option<Vec<Color32>>> {
        (0..4)
            .flat_map(|ty| (0..4).map(move |tx| (tx, ty)))
            .map(|(tx, ty)| {
                app.canvas
                    .get_layer_tile_data(layer, tx, ty)
                    .filter(|d| d.iter().any(|&p| p != Color32::TRANSPARENT))
            })
            .collect()
    }

    fn app_with_red_square() -> PainterApp {
        let canvas = Canvas::new(256, 256, Color32::WHITE, TILE_SIZE);
        canvas.set_layer_tile_data(
            1,
            1,
            1,
            vec![Color32::from_rgb(255, 0, 0); TILE_SIZE * TILE_SIZE],
        );
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        app.active_tool = crate::app::tools::Tool::Transform(
            crate::selection::transform::TransformInfo::default(),
        );
        app
    }

    #[test]
    fn whole_layer_transform_is_live_and_one_undo_step() {
        use crate::app::tools::Tool;
        use crate::app::tools::transform;
        let mut app = app_with_red_square();
        let original = layer_pixels(&app, 1);

        // No selection: pressing floats the whole layer, with a box around it.
        transform::transform_press(&mut app, Vec2::new(96.0, 96.0));
        assert_eq!(app.layer_state.floating_layer_idx, Some(2));
        transform::transform_drag(&mut app, Vec2::new(160.0, 96.0), false);
        transform::flush_transform_preview(&mut app);
        transform::transform_release(&mut app);
        // Live: the floating layer already shows it moved; the box moved too.
        assert!(
            app.canvas
                .get_layer_tile_data(2, 2, 1)
                .is_some_and(|d| d[0].r() == 255)
        );
        let Tool::Transform(info) = app.active_tool else {
            panic!()
        };
        assert_eq!(info.offset, Vec2::new(64.0, 0.0));
        assert!(info.bounds.is_some());

        transform::commit_floating_layer(&mut app);
        assert_eq!(app.canvas.layers.len(), 2);
        assert_eq!(app.canvas.active_layer_idx, 1);
        assert!(
            layer_pixels(&app, 1)[4 + 1].is_none(),
            "moved out of tile (1,1)"
        );
        assert!(layer_pixels(&app, 1)[4 + 2].is_some(), "into tile (2,1)");
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);

        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original);
        assert_eq!(app.canvas.layers.len(), 2);
        app.apply_history(true);
        assert!(layer_pixels(&app, 1)[4 + 2].is_some());
    }

    #[test]
    fn cancelling_a_transform_restores_the_layer() {
        use crate::app::tools::transform;
        let mut app = app_with_red_square();
        let original = layer_pixels(&app, 1);
        transform::transform_press(&mut app, Vec2::new(96.0, 96.0));
        transform::rotate_quarter(&mut app, true);
        transform::flip(&mut app, true);
        transform::transform_drag(&mut app, Vec2::new(20.0, 30.0), false);
        transform::flush_transform_preview(&mut app);
        transform::cancel_floating_layer(&mut app);
        assert_eq!(app.canvas.layers.len(), 2);
        assert_eq!(app.layer_state.layer_ui_colors.len(), 2);
        assert_eq!(layer_pixels(&app, 1), original);
        assert!(app.layer_state.history.stacks().0.is_empty());
    }

    #[test]
    fn distort_moves_one_corner() {
        use crate::app::tools::Tool;
        use crate::app::tools::transform;
        let mut app = app_with_red_square();
        transform::transform_press(&mut app, Vec2::new(96.0, 96.0));
        transform::transform_release(&mut app);
        transform::set_corner_mode(
            &mut app,
            Some(crate::canvas::storage::DistortKind::Perspective),
        );
        // Drag the bottom-right corner (128,128) out to (192,192).
        transform::transform_press(&mut app, Vec2::new(128.0, 128.0));
        let Tool::Transform(info) = app.active_tool else {
            panic!()
        };
        assert_eq!(
            info.state,
            crate::selection::transform::TransformState::Corner(2)
        );
        transform::transform_drag(&mut app, Vec2::new(192.0, 192.0), false);
        transform::transform_release(&mut app);
        transform::commit_floating_layer(&mut app);
        // The far corner now reaches into tile (2,2); the top-left stays.
        let px = app.canvas.get_layer_tile_data(1, 2, 2).unwrap();
        assert!(px[10 * TILE_SIZE + 10].a() > 0);
        let px = app.canvas.get_layer_tile_data(1, 1, 1).unwrap();
        assert_eq!(px[TILE_SIZE + 1].r(), 255);
    }

    #[test]
    fn bucket_fill_paints_one_area_and_undoes() {
        use crate::app::tools::Tool;
        use crate::app::tools::fill::FillSource;
        // Layer 1 holds a closed black square outline on transparency.
        let canvas = Canvas::new(128, 128, Color32::WHITE, TILE_SIZE);
        let mut tile = vec![Color32::TRANSPARENT; TILE_SIZE * TILE_SIZE];
        for i in 10..50 {
            for (x, y) in [(i, 10), (i, 49), (10, i), (49, i)] {
                tile[y * TILE_SIZE + x] = Color32::BLACK;
            }
        }
        canvas.set_layer_tile_data(1, 0, 0, tile);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        app.active_tool = Tool::Fill;
        app.workspace.fill.source = FillSource::CurrentLayer;
        app.workspace.fill.settings.antialias = false;
        app.workspace.fill.settings.expand = 0;
        app.brush_state.brush.brush_options.color = Color32::from_rgb(0, 0, 255);
        let before = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();

        app.fill_press(Vec2::new(30.0, 30.0));
        let after = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(after[30 * TILE_SIZE + 30], Color32::from_rgb(0, 0, 255));
        assert_eq!(
            after[5 * TILE_SIZE + 5],
            Color32::TRANSPARENT,
            "outside untouched"
        );
        assert_eq!(after[10 * TILE_SIZE + 30], Color32::BLACK, "line untouched");
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);

        app.apply_history(false);
        assert_eq!(app.canvas.get_layer_tile_data(1, 0, 0).unwrap(), before);
    }

    #[test]
    fn alpha_lock_and_selection_limit_a_fill() {
        use crate::app::tools::fill::FillSource;
        use crate::selection::SelectionType;
        let canvas = Canvas::new(128, 128, Color32::WHITE, TILE_SIZE);
        // Left half of tile (0,0) opaque red, right half transparent.
        let mut tile = vec![Color32::TRANSPARENT; TILE_SIZE * TILE_SIZE];
        for y in 0..TILE_SIZE {
            for x in 0..32 {
                tile[y * TILE_SIZE + x] = Color32::from_rgb(255, 0, 0);
            }
        }
        canvas.set_layer_tile_data(1, 0, 0, tile);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        app.canvas_mut().layers[1].alpha_locked = true;
        app.workspace.fill.source = FillSource::AllVisible;
        app.workspace.fill.settings.tolerance = 255;
        app.workspace.fill.settings.antialias = false;
        app.brush_state.brush.brush_options.color = Color32::from_rgb(0, 255, 0);
        // Only the top half of the canvas is selected.
        app.selection_manager
            .start_selection(Vec2::new(0.0, 0.0), SelectionType::Rectangle);
        app.selection_manager
            .update_selection(Vec2::new(128.0, 20.0));
        app.selection_manager.end_selection();

        app.fill_press(Vec2::new(5.0, 5.0));
        let px = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert_eq!(
            px[5 * TILE_SIZE + 5],
            Color32::from_rgb(0, 255, 0),
            "recoloured"
        );
        assert_eq!(
            px[5 * TILE_SIZE + 40],
            Color32::TRANSPARENT,
            "alpha lock keeps it empty"
        );
        assert_eq!(
            px[40 * TILE_SIZE + 5],
            Color32::from_rgb(255, 0, 0),
            "outside the selection"
        );
    }

    #[test]
    fn liquify_session_applies_as_one_step_and_cancels_cleanly() {
        use crate::app::tools::Tool;
        let mut app = app_with_red_square();
        app.active_tool = Tool::Liquify;
        app.workspace.liquify.radius = 30.0;
        app.workspace.liquify.strength = 1.0;
        let original = layer_pixels(&app, 1);

        // Two strokes pushing the square's left edge.
        for y in [80.0, 100.0] {
            app.liquify_press(Vec2::new(60.0, y));
            app.liquify_drag(Vec2::new(90.0, y));
            app.liquify_release();
        }
        assert_ne!(layer_pixels(&app, 1), original, "pixels moved");
        assert!(
            app.layer_state.history.stacks().0.is_empty(),
            "no step until applied"
        );
        app.liquify_commit();
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original);

        app.liquify_press(Vec2::new(60.0, 90.0));
        app.liquify_drag(Vec2::new(90.0, 90.0));
        app.liquify_release();
        app.liquify_cancel();
        assert_eq!(layer_pixels(&app, 1), original);
        assert!(app.layer_state.liquify.is_none());
    }

    #[test]
    fn imported_image_becomes_a_fitted_centred_layer() {
        use crate::app::tools::Tool;
        let canvas = Canvas::new(256, 256, Color32::WHITE, TILE_SIZE);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        // Wider than the canvas: scaled to 256×128, centred vertically.
        let img = image::RgbaImage::from_pixel(512, 256, image::Rgba([0, 200, 0, 255]));
        app.import_rgba("Photo", img);
        assert_eq!(app.canvas.layers.len(), 3);
        let idx = app.canvas.active_layer_idx;
        assert_eq!(app.canvas.layers[idx].name, "Photo");
        assert!(matches!(app.active_tool, Tool::Transform(_)));
        let top = app.canvas.get_layer_tile_data(idx, 1, 0);
        assert!(
            top.is_none_or(|t| t[0].a() == 0),
            "nothing above the picture"
        );
        let middle = app.canvas.get_layer_tile_data(idx, 1, 1).unwrap();
        assert_eq!(middle[0], Color32::from_rgb(0, 200, 0));

        // Undo removes the pixels, then the layer; redo brings both back.
        app.apply_history(false);
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), 2);
    }

    #[test]
    fn palette_extracts_and_recolours_a_layer() {
        let mut app = app_with_red_square();
        app.workspace.palette.from_layer = true;
        app.workspace.palette.count = 4;
        app.extract_palette();
        let pal = app.workspace.palette.extracted.clone();
        assert_eq!(
            pal,
            vec![Color32::from_rgb(255, 0, 0)],
            "one colour on the layer"
        );

        let before = layer_pixels(&app, 1);
        app.recolor_layer(&[Color32::from_rgb(0, 0, 250), Color32::BLACK]);
        let px = app.canvas.get_layer_tile_data(1, 1, 1).unwrap();
        assert_eq!(
            px[0],
            Color32::from_rgb(0, 0, 250),
            "red is nearest to blue here"
        );
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), before);
        app.add_swatches(&pal);
        assert!(
            app.brush_state
                .swatches
                .contains(&Color32::from_rgb(255, 0, 0))
        );
    }

    #[test]
    fn parallel_flatten_matches_the_reference_compositor() {
        use crate::canvas::blend_modes::LayerBlend;
        use crate::canvas::storage::LayerKind;
        let mut canvas = Canvas::new(300, 200, Color32::WHITE, TILE_SIZE);
        let paint = |c: &Canvas, layer: usize, f: &dyn Fn(usize, usize) -> Color32| {
            for ty in 0..4 {
                for tx in 0..5 {
                    let t = (0..TILE_SIZE * TILE_SIZE)
                        .map(|i| {
                            f(
                                tx * TILE_SIZE + i % TILE_SIZE,
                                ty * TILE_SIZE + i / TILE_SIZE,
                            )
                        })
                        .collect();
                    c.set_layer_tile_data(layer, tx as i32, ty as i32, t);
                }
            }
        };
        paint(&canvas, 1, &|x, y| {
            Color32::from_rgba_unmultiplied(x as u8, y as u8, 200, 180)
        });
        let folder = canvas.insert_new_layer(2, "F".into(), LayerKind::Group, None);
        canvas.insert_new_layer(3, "M".into(), LayerKind::Paint, Some(folder));
        paint(&canvas, 3, &|x, y| {
            Color32::from_rgba_unmultiplied(250, (x + y) as u8, 40, (x % 256) as u8)
        });
        canvas.layers[3].blend = LayerBlend::Multiply;
        canvas.layers[3].opacity = 0.6;
        canvas.layers[2].opacity = 0.8;
        for plain in [false, true] {
            if plain {
                canvas.layers.truncate(2);
            }
            let mut reference = eframe::egui::ColorImage::new([300, 200], Color32::TRANSPARENT);
            canvas.write_region_to_color_image(0, 0, 300, 200, &mut reference, 1);
            assert!(
                canvas.flatten().pixels == reference.pixels,
                "plain stack: {plain}"
            );
        }
    }

    #[test]
    fn liquify_twirl_undo_redo_restores_every_tile() {
        use crate::app::tools::Tool;
        use crate::canvas::liquify::LiquifyMode;
        let mut app = app_with_red_square();
        app.active_tool = Tool::Liquify;
        app.workspace.liquify.radius = 90.0;
        app.workspace.liquify.strength = 1.0;
        let original = layer_pixels(&app, 1);
        for mode in [LiquifyMode::TwirlCw, LiquifyMode::Bloat] {
            app.workspace.liquify.mode = mode;
            app.liquify_press(Vec2::new(96.0, 96.0));
            for _ in 0..20 {
                app.liquify_hold(1.0 / 30.0);
            }
            app.liquify_drag(Vec2::new(130.0, 110.0));
            app.liquify_release();
        }
        app.liquify_commit();
        let after = layer_pixels(&app, 1);
        assert_ne!(after, original);
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original, "undo");
        app.apply_history(true);
        assert_eq!(layer_pixels(&app, 1), after, "redo");
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original, "undo again");
    }

    #[test]
    fn smudge_drags_paint_and_blur_softens_edges() {
        use crate::app::tools::Tool;
        let mut app = app_with_red_square(); // red on tile (1,1): 64..128
        app.brush_state.brush.brush_options.diameter = 30.0;
        app.brush_state.brush.brush_options.hardness = 60.0;
        app.brush_state.brush.brush_options.flow = 100.0;
        app.brush_state.brush.brush_options.spacing = 10.0;
        let original = layer_pixels(&app, 1);

        // Smudge from inside the square out to the right: red lands outside.
        app.set_blend_tool(true);
        assert!(matches!(app.active_tool, Tool::Smudge));
        app.blend_press(Vec2::new(110.0, 96.0), 1.0);
        app.blend_drag(Vec2::new(150.0, 96.0), 1.0);
        app.blend_release();
        app.settle_strokes();
        let px = app.canvas.get_layer_tile_data(1, 2, 1).unwrap();
        assert!(px[32 * TILE_SIZE + 5].a() > 60, "red carried past the edge");
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original);

        // Blur across the bottom edge: the hard edge becomes a ramp.
        app.set_blend_tool(false);
        app.blend_press(Vec2::new(80.0, 128.0), 1.0);
        app.blend_drag(Vec2::new(110.0, 128.0), 1.0);
        app.blend_release();
        app.settle_strokes();
        let inside = app.canvas.get_layer_tile_data(1, 1, 1).unwrap()[62 * TILE_SIZE + 30];
        let outside = app.canvas.get_layer_tile_data(1, 1, 2).unwrap()[TILE_SIZE + 30];
        assert!(
            inside.a() < 255 && outside.a() > 0,
            "{inside:?} {outside:?}"
        );
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original);
    }

    #[test]
    fn blur_and_smudge_keep_the_layer_colour_at_soft_edges() {
        // A light yellow square on a transparent layer, over a white
        // background: softening its edge must fade it (alpha) without
        // turning it darker or picking up another colour.
        let canvas = Canvas::new(128, 128, Color32::WHITE, TILE_SIZE);
        let yellow = Color32::from_rgb(255, 235, 120);
        canvas.set_layer_tile_data(
            1,
            0,
            0,
            (0..TILE_SIZE * TILE_SIZE)
                .map(|i| {
                    if i % TILE_SIZE < 32 {
                        yellow
                    } else {
                        Color32::TRANSPARENT
                    }
                })
                .collect(),
        );
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        let o = &mut app.brush_state.brush.brush_options;
        o.diameter = 24.0;
        o.hardness = 50.0;
        o.flow = 100.0;
        o.spacing = 10.0;
        for smudge in [false, true] {
            app.set_blend_tool(smudge);
            app.blend_press(Vec2::new(if smudge { 20.0 } else { 32.0 }, 10.0), 1.0);
            app.blend_drag(Vec2::new(if smudge { 50.0 } else { 32.0 }, 50.0), 1.0);
            app.blend_release();
            app.settle_strokes();
            let px = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
            let mut soft = 0;
            for &p in &px {
                if p.a() > 20 && p.a() < 235 {
                    soft += 1;
                    let [r, g, b, _] = crate::canvas::blend::unmultiply(p);
                    assert!(
                        (r as i32 - 255).abs() <= 6
                            && (g as i32 - 235).abs() <= 6
                            && (b as i32 - 120).abs() <= 8,
                        "smudge {smudge}: {p:?} unmultiplies to {:?}",
                        [r, g, b]
                    );
                }
            }
            assert!(soft > 20, "the edge got soft ({soft} px)");
            app.apply_history(false);
        }
    }

    fn whole_layer(app: &PainterApp, layer: usize) -> Vec<((i32, i32), Vec<Color32>)> {
        let mut v: Vec<_> = app
            .canvas
            .capture_layer_pixels(layer)
            .into_iter()
            .filter(|(_, d)| d.iter().any(|p| p.a() > 0))
            .collect();
        v.sort_by_key(|(k, _)| *k);
        v
    }

    fn brush_stroke(app: &mut PainterApp, color: Color32, y: f32) {
        app.brush_state.brush.brush_options.color = color;
        app.brush_state.brush.brush_options.diameter = 70.0;
        app.start_stroke_with_pressure(Vec2::new(30.0, y), 1.0);
        for i in 1..=30 {
            app.add_stroke_point(
                Vec2::new(30.0 + i as f32 * 14.0, y + (i as f32 * 0.3).sin() * 20.0),
                1.0,
            );
        }
        app.finish_stroke();
        app.stroke_worker.wait_idle();
        app.sync_stroke_worker();
    }

    #[test]
    fn undo_redo_after_liquify_restores_the_whole_layer() {
        use crate::app::tools::Tool;
        use crate::canvas::liquify::LiquifyMode;
        let canvas = Canvas::new(512, 384, Color32::WHITE, TILE_SIZE);
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        brush_stroke(&mut app, Color32::from_rgb(210, 40, 40), 150.0);
        let red_only = whole_layer(&app, 1);
        brush_stroke(&mut app, Color32::from_rgb(210, 100, 40), 200.0);
        let before_liquify = whole_layer(&app, 1);

        app.active_tool = Tool::Liquify;
        app.workspace.liquify.radius = 80.0;
        app.workspace.liquify.strength = 0.8;
        for (mode, y) in [
            (LiquifyMode::Push, 240.0),
            (LiquifyMode::Push, 230.0),
            (LiquifyMode::Bloat, 200.0),
        ] {
            app.workspace.liquify.mode = mode;
            app.liquify_press(Vec2::new(150.0, y));
            for i in 1..=12 {
                app.liquify_drag(Vec2::new(150.0 + i as f32 * 9.0, y - i as f32 * 3.0));
                app.liquify_hold(1.0 / 60.0);
            }
            app.liquify_release();
        }
        // Ctrl+Z while the liquify session is still open.
        app.apply_history(false);
        assert!(whole_layer(&app, 1) == before_liquify, "undo liquify");
        app.apply_history(true);
        let liquified = whole_layer(&app, 1);
        assert!(liquified != before_liquify, "redo brings it back");
        app.apply_history(false);
        assert!(whole_layer(&app, 1) == before_liquify, "undo again");
        app.apply_history(false);
        assert!(whole_layer(&app, 1) == red_only, "then the orange stroke");
        app.apply_history(true);
        app.apply_history(true);
        assert!(whole_layer(&app, 1) == liquified, "redo both");
    }

    #[test]
    fn undo_into_an_emptied_tile_makes_it_visible_again() {
        // Push every red pixel out of tile (1,1) with liquify: the tile
        // becomes empty. Undo must bring it back *and* show it; undo used to
        // restore the pixels but leave the tile flagged empty, so it drew
        // (and was read by every tool) as a transparent hole.
        use crate::app::tools::Tool;
        use crate::canvas::liquify::LiquifyMode;
        let canvas = Canvas::new(256, 256, Color32::WHITE, TILE_SIZE);
        canvas.set_layer_tile_data(
            1,
            1,
            1,
            vec![Color32::from_rgb(255, 0, 0); TILE_SIZE * TILE_SIZE],
        );
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        let shown_red = |app: &PainterApp| {
            app.canvas.flatten().pixels[96 * 256 + 96] == Color32::from_rgb(255, 0, 0)
        };
        assert!(shown_red(&app));
        app.active_tool = Tool::Liquify;
        app.workspace.liquify.mode = LiquifyMode::Reconstruct;
        // Blow the tile's content away by sampling far outside it.
        app.liquify_press(Vec2::new(96.0, 96.0));
        // Field tile (0, 0) covers canvas 0..128 at half resolution.
        app.layer_state
            .liquify
            .as_mut()
            .unwrap()
            .field_for_test()
            .fill_offset([0, 0], Vec2::new(200.0, 0.0));
        app.liquify_redraw_for_test([64, 64, 128, 128]);
        app.liquify_release();
        assert!(!shown_red(&app), "tile emptied");
        app.apply_history(false);
        assert!(shown_red(&app), "undo shows the tile again");
        app.apply_history(true);
        assert!(!shown_red(&app));
        app.apply_history(false);
        assert!(shown_red(&app), "and again");
    }

    #[test]
    fn moving_off_the_canvas_and_back_keeps_the_whole_image() {
        use crate::app::tools::transform;
        let mut app = app_with_red_square(); // red on 64..128
        let original = layer_pixels(&app, 1);
        for (from, to) in [((96.0, 96.0), (-4.0, 96.0)), ((-4.0, 96.0), (96.0, 96.0))] {
            transform::transform_press(&mut app, Vec2::new(from.0, from.1));
            transform::transform_drag(&mut app, Vec2::new(to.0, to.1), false);
            transform::transform_release(&mut app);
            transform::commit_floating_layer(&mut app);
        }
        assert_eq!(layer_pixels(&app, 1), original, "came back whole");
        // And undo walks back through both moves.
        app.apply_history(false);
        let half_off = layer_pixels(&app, 1);
        assert_ne!(half_off, original);
        app.apply_history(false);
        assert_eq!(layer_pixels(&app, 1), original);
    }

    #[test]
    fn transform_click_picks_the_image_under_the_pointer() {
        use crate::app::tools::Tool;
        use crate::app::tools::transform;
        let mut app = app_with_red_square(); // layer 1: red on 64..128
        app.add_layer_and_select(); // layer 2: blue on 192..256
        let top = app.canvas.active_layer_idx;
        app.canvas.set_layer_tile_data(
            top,
            3,
            3,
            vec![Color32::from_rgb(0, 0, 255); TILE_SIZE * TILE_SIZE],
        );
        app.canvas_mut().active_layer_idx = 1;
        app.active_tool = Tool::Transform(crate::selection::transform::TransformInfo::default());
        // Click the blue image: its layer becomes the one transformed.
        transform::transform_press(&mut app, Vec2::new(220.0, 220.0));
        let float = app.layer_state.floating_layer_idx.expect("floating");
        assert!(
            app.canvas
                .get_layer_tile_data(float, 3, 3)
                .is_some_and(|t| t[0].b() == 255)
        );
        transform::transform_release(&mut app);
        transform::commit_floating_layer(&mut app);
        assert_eq!(app.canvas.active_layer_idx, top);
        // A locked layer can't be picked.
        app.canvas_mut().layers[1].locked = true;
        let top_id = app.canvas.layer_id_at(top);
        transform::transform_press(&mut app, Vec2::new(96.0, 96.0));
        assert_eq!(
            app.layer_state.float_session.as_ref().map(|s| s.source_id),
            top_id,
            "still transforming the blue layer"
        );
    }

    #[test]
    fn dragging_uses_the_gpu_overlay_and_renders_once_on_release() {
        use crate::app::tools::transform;
        let mut app = app_with_red_square();
        let ctx = eframe::egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        transform::transform_press(&mut app, Vec2::new(96.0, 96.0));
        let float = app.layer_state.floating_layer_idx.unwrap();
        transform::transform_drag(&mut app, Vec2::new(130.0, 96.0), false);
        transform::flush_transform_preview(&mut app);
        transform::update_float_overlay(&mut app, &ctx);
        let overlay = app
            .layer_state
            .float_overlay
            .as_ref()
            .expect("overlay built");
        assert!(overlay.showing);
        assert!(
            !app.canvas.layers[float].visible,
            "layer hidden while the overlay shows it"
        );
        // While dragging, the layer isn't re-rendered every frame.
        transform::transform_drag(&mut app, Vec2::new(160.0, 96.0), false);
        transform::flush_transform_preview(&mut app);
        assert!(
            app.layer_state.transform_preview_pending,
            "render waits for release"
        );
        // (x = 180 is only red once the +64 px move is rendered.)
        assert!(
            app.canvas
                .get_layer_tile_data(float, 2, 1)
                .is_none_or(|t| t[52].a() == 0)
        );
        // Release: one full render, the layer shows again.
        transform::transform_release(&mut app);
        transform::flush_transform_preview(&mut app);
        transform::update_float_overlay(&mut app, &ctx);
        assert!(app.canvas.layers[float].visible);
        assert!(
            app.canvas
                .get_layer_tile_data(float, 2, 1)
                .is_some_and(|t| t[52].r() == 255)
        );
        transform::float_overlay_uploaded(&mut app, false);
        assert!(!app.layer_state.float_overlay.as_ref().unwrap().showing);
        transform::commit_floating_layer(&mut app);
        assert!(app.layer_state.float_overlay.is_none());
        assert!(
            app.canvas
                .get_layer_tile_data(1, 2, 1)
                .is_some_and(|t| t[0].r() == 255)
        );
    }

    #[test]
    fn transform_of_a_selection_previews_commits_and_undoes() {
        use crate::app::tools::Tool;
        use crate::app::tools::transform;
        use crate::selection::SelectionType;
        use crate::selection::transform::TransformInfo;
        let canvas = Canvas::new(256, 256, Color32::WHITE, TILE_SIZE);
        for tx in 0..4 {
            for ty in 0..4 {
                canvas.set_layer_tile_data(
                    1,
                    tx,
                    ty,
                    vec![Color32::from_rgb(255, 0, 0); TILE_SIZE * TILE_SIZE],
                );
            }
        }
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        app.selection_manager
            .start_selection(Vec2::new(10.0, 10.0), SelectionType::Rectangle);
        app.selection_manager
            .update_selection(Vec2::new(120.0, 90.0));
        app.selection_manager.end_selection();
        app.active_tool = Tool::Transform(TransformInfo::default());

        transform::create_floating_layer(&mut app);
        assert!(app.layer_state.floating_layer_idx.is_some());
        let floating = app.layer_state.floating_layer_idx.unwrap();
        let mut info = TransformInfo {
            bounds: app.canvas.get_content_bounds(floating, None),
            ..TransformInfo::default()
        };
        for step in 1..=5 {
            info.offset = Vec2::new(step as f32 * 7.0, step as f32 * 3.0);
            transform::apply_live_transform_preview(&mut app, &info);
        }
        transform::commit_floating_layer(&mut app);
        assert!(app.layer_state.floating_layer_idx.is_none());
        assert_eq!(app.canvas.layers.len(), 2);
        app.apply_history(false);
        assert_eq!(
            app.canvas.layers.len(),
            app.layer_state.layer_ui_colors.len()
        );
    }

    #[test]
    fn strokes_wandering_off_canvas_never_panic_the_worker() {
        // Random walks in and out of the canvas at many brush sizes, through
        // the real stroke worker; a panic there would leave it busy forever
        // and freeze the next `release_canvas` (e.g. starting a transform).
        let canvas = Canvas::new(300, 200, Color32::WHITE, TILE_SIZE);
        let mut app = test_app(canvas, vec![History::new(), History::new()]);
        app.canvas_mut().active_layer_idx = 1;
        let mut seed = 0xdead_beef_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for &diameter in &[1.0_f32, 3.0, 17.0, 64.0, 150.0, 700.0] {
            for pixel in [false, true] {
                app.brush_state.brush.brush_options.diameter = diameter;
                app.brush_state.brush.brush_type = if pixel {
                    crate::brush_engine::brush::BrushType::Pixel
                } else {
                    crate::brush_engine::brush::BrushType::Soft
                };
                app.brush_state.brush.is_changed = true;
                let mut pos = Vec2::new(-50.0, 100.0);
                app.start_stroke_with_pressure(pos, 1.0);
                for _ in 0..60 {
                    pos.x += (next() % 160) as f32 - 70.0;
                    pos.y += (next() % 160) as f32 - 80.0;
                    let pressure = (next() % 100) as f32 / 100.0;
                    app.add_stroke_point(pos, pressure);
                }
                app.finish_stroke();
                // Would hang forever if the worker died mid-stroke.
                app.release_canvas();
            }
        }
    }

    #[test]
    fn project_round_trips_shader_layers() {
        use crate::canvas::shader::ShaderLayer;
        let mut canvas = Canvas::new(TILE_SIZE, TILE_SIZE, Color32::WHITE, TILE_SIZE);
        let mut shader =
            ShaderLayer::new("void mainImage(out vec4 c, in vec2 p) { c = vec4(1.0); }");
        shader.time = 12.5;
        shader.speed = 0.5;
        canvas.layers[1].shader = Some(Box::new(shader.clone()));
        let encoded =
            encode_project(&test_app(canvas, vec![History::new(), History::new()])).unwrap();
        let loaded = decode_project(&encoded).unwrap();
        assert_eq!(loaded.canvas.layers[1].shader.as_deref(), Some(&shader));
        assert_eq!(loaded.canvas.layers[0].shader, None);
    }

    #[test]
    fn project_round_trips_blend_modes_and_blend_space() {
        let mut canvas = Canvas::new(TILE_SIZE, TILE_SIZE, Color32::WHITE, TILE_SIZE);
        canvas.layers[1].blend = LayerBlend::SoftLight;
        canvas.blend_space = BlendSpace::Gamma;
        let encoded =
            encode_project(&test_app(canvas, vec![History::new(), History::new()])).unwrap();
        let loaded = decode_project(&encoded).unwrap();
        assert_eq!(loaded.canvas.layers[1].blend, LayerBlend::SoftLight);
        assert_eq!(loaded.canvas.layers[0].blend, LayerBlend::Normal);
        assert_eq!(loaded.canvas.blend_space, BlendSpace::Gamma);
    }

    #[test]
    fn project_round_trips_tiles_and_history_old_and_new_format() {
        let canvas = Canvas::new(TILE_SIZE, TILE_SIZE, Color32::WHITE, TILE_SIZE);
        let pixels = vec![Color32::from_rgb(255, 0, 0); TILE_SIZE * TILE_SIZE];
        canvas.set_layer_tile_data(1, 0, 0, pixels.clone());

        let mut history = History::new();
        history.push_action(UndoAction {
            tiles: vec![TileSnapshot {
                tx: 0,
                ty: 0,
                layer_id: LayerId(1),
                x0: 0,
                y0: 0,
                width: TILE_SIZE,
                height: TILE_SIZE,
                data: pixels.into(),
            }],
            selection: Some(None),
            transform: None,
            layer_action: None,
        });

        let app = test_app(canvas, vec![History::new(), history]);
        let encoded = encode_project(&app).unwrap();
        assert!(encoded.len() < TILE_SIZE * TILE_SIZE * 4);

        // Files saved before the OpenRaster container: the bare data.
        let bare = encode_project_data(&ProjectSnapshot::capture(&app)).unwrap();
        assert!(bare.starts_with(MAGIC));
        for bytes in [&encoded, &bare] {
            let loaded = decode_project(bytes).unwrap();
            assert_eq!(
                loaded.canvas.get_layer_tile_data(1, 0, 0).unwrap()[0],
                Color32::from_rgb(255, 0, 0)
            );
            assert_eq!(loaded.history.stacks().0.len(), 1);
        }
    }

    #[test]
    fn project_is_an_openraster_file() {
        let canvas = Canvas::new(600, 300, Color32::WHITE, TILE_SIZE);
        let encoded =
            encode_project(&test_app(canvas, vec![History::new(), History::new()])).unwrap();
        // What shared-mime-info matches to call a file image/openraster.
        assert_eq!(&encoded[..4], b"PK\x03\x04");
        assert_eq!(&encoded[30..38], b"mimetype");
        assert_eq!(&encoded[38..54], b"image/openraster");

        let stack = std::str::from_utf8(zip::read_entry(&encoded, "stack.xml").unwrap()).unwrap();
        assert!(stack.contains(r#"w="600" h="300""#));
        let png = |name| image::load_from_memory(zip::read_entry(&encoded, name).unwrap()).unwrap();
        let merged = png("mergedimage.png");
        assert_eq!((merged.width(), merged.height()), (600, 300));
        assert_eq!(merged.to_rgba8().get_pixel(10, 10).0, [255, 255, 255, 255]);
        let thumb = png("Thumbnails/thumbnail.png");
        assert_eq!((thumb.width(), thumb.height()), (256, 128));
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;
    use eframe::egui::Vec2;

    /// A small document with one of most things: paint, a folder, a mask,
    /// vector lines, a fill layer, an adjustment layer and undo steps.
    fn rich_app() -> PainterApp {
        let canvas = Canvas::new(96, 64, Color32::WHITE, TILE_SIZE);
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.filter_open(crate::canvas::filters::Filter::Invert);
        app.add_folder();
        app.add_vector_layer();
        let idx = app.canvas.active_layer_idx;
        app.add_vector_line(idx, &[Vec2::new(5.0, 5.0), Vec2::new(80.0, 50.0)]);
        app.add_fill_layer(crate::canvas::layer_style::LayerFill::Colour([10, 200, 90]));
        app.add_adjustment_layer(crate::canvas::filters::Filter::Levels {
            black: 0.1,
            white: 0.9,
            gamma: 1.2,
        });
        app.canvas_mut().active_layer_idx = 1;
        app.add_mask_to_active();
        // Impasto paint, and a stroke of it in the history.
        app.brush_state.brush.impasto = Some(Default::default());
        app.start_stroke_with_pressure(Vec2::new(10.0, 10.0), 1.0);
        app.add_stroke_point(Vec2::new(60.0, 40.0), 1.0);
        app.finish_stroke();
        app.release_canvas();
        app.brush_state.brush.impasto = None;
        app
    }

    fn open(bytes: &[u8]) {
        let Ok(loaded) = decode_project(bytes) else {
            return;
        };
        let mut canvas = loaded.canvas;
        let mut history = loaded.history;
        canvas.flatten();
        // Every step back and forward again.
        let mut selection = crate::selection::SelectionManager::new();
        let mut tool = crate::app::tools::Tool::Brush;
        for _ in 0..64 {
            history.undo(&mut canvas, &mut selection, &mut tool);
        }
        canvas.flatten();
        for _ in 0..64 {
            history.redo(&mut canvas, &mut selection, &mut tool);
        }
        canvas.flatten();
    }

    /// Every value in `v`, depth first (for picking one to change).
    fn count(v: &serde_json::Value) -> usize {
        1 + match v {
            serde_json::Value::Array(a) => a.iter().map(count).sum(),
            serde_json::Value::Object(o) => o.values().map(count).sum(),
            _ => 0,
        }
    }

    /// Replace value number `n` (depth first) with `with`.
    fn replace(v: &mut serde_json::Value, n: &mut usize, with: &serde_json::Value) -> bool {
        if *n == 0 {
            *v = with.clone();
            return true;
        }
        *n -= 1;
        match v {
            serde_json::Value::Array(a) => a.iter_mut().any(|x| replace(x, n, with)),
            serde_json::Value::Object(o) => o.values_mut().any(|x| replace(x, n, with)),
            _ => false,
        }
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_project_values() {
        // Well-formed files with nonsense in them: sizes, indices, ids and
        // counts at extremes, wrong types, missing parts.
        let bare = encode_project_data(&ProjectSnapshot::capture(&rich_app())).unwrap();
        let start = MAGIC.len() + 8;
        let len = u64::from_le_bytes(bare[MAGIC.len()..start].try_into().unwrap()) as usize;
        let manifest: serde_json::Value =
            serde_json::from_slice(&bare[start..start + len]).unwrap();
        let blobs = &bare[start + len..];
        let extremes: Vec<serde_json::Value> = serde_json::from_str(
            r#"[0, 1, -1, 2, 7, 64, 65, 4294967295, 18446744073709551615, -9223372036854775808,
                1e30, -1e30, 0.5, null, "", "x", [], {}, true, [0], [0, 0, 0, 0]]"#,
        )
        .unwrap();
        let total = count(&manifest);
        let rounds: usize = std::env::var("RP_FUZZ_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000);
        // One value changed per input, walking every value with every
        // extreme first, then random pairs.
        let mut inputs = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for i in 0..rounds {
            let mut m = manifest.clone();
            for _ in 0..1 + i % 2 {
                let mut n = rnd(total);
                replace(&mut m, &mut n, &extremes[rnd(extremes.len())]);
            }
            let json = serde_json::to_vec(&m).unwrap();
            let mut bytes = MAGIC.to_vec();
            bytes.extend((json.len() as u64).to_le_bytes());
            bytes.extend(json);
            bytes.extend(blobs);
            inputs.push(bytes);
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let mut failures = Vec::new();
        let mut slowest = std::time::Duration::ZERO;
        for (i, input) in inputs.iter().enumerate() {
            let t = std::time::Instant::now();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open(input)));
            slowest = slowest.max(t.elapsed());
            if let Err(e) = r {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                let path = std::env::temp_dir().join(format!("rp-fuzz-values-{i}.bin"));
                std::fs::write(&path, input).unwrap();
                failures.push(format!("{msg} ({})", path.display()));
            } else if t.elapsed() > std::time::Duration::from_secs(2) {
                failures.push(format!("round {i} took {:?}", t.elapsed()));
            }
        }
        std::panic::set_hook(hook);
        eprintln!(
            "fuzz project values: {} rounds, slowest {slowest:?}, {} failures",
            inputs.len(),
            failures.len()
        );
        failures.sort();
        failures.dedup_by(|a, b| a.split(" (").next() == b.split(" (").next());
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "replays a saved fuzz input: RP_REPLAY=path"]
    fn replay() {
        open(&std::fs::read(std::env::var("RP_REPLAY").unwrap()).unwrap());
    }

    #[test]
    fn folders_inside_each_other_in_a_damaged_file_are_undone() {
        let mut app = rich_app();
        app.add_folder();
        let a = app.canvas.layers[app.canvas.active_layer_idx].id;
        app.add_folder();
        let bi = app.canvas.active_layer_idx;
        let b = app.canvas.layers[bi].id;
        let ai = app.canvas.layer_index_of(a).unwrap();
        app.canvas_mut().layers[ai].parent = Some(b);
        app.canvas_mut().layers[bi].parent = Some(a);
        let loaded =
            decode_project(&encode_project_data(&ProjectSnapshot::capture(&app)).unwrap()).unwrap();
        let parent_of = |id| {
            let c = &loaded.canvas;
            c.layers[c.layer_index_of(id).unwrap()].parent
        };
        // One of the two lets go; the other stays inside it.
        assert!(parent_of(a).is_none() != parent_of(b).is_none());
        let mut canvas = loaded.canvas;
        canvas.merge_visible().unwrap();
    }

    #[test]
    fn a_damaged_file_giving_two_layers_one_id_is_refused() {
        let mut app = rich_app();
        let id = app.canvas.layers[1].id;
        app.canvas_mut().layers[2].id = id;
        let err = decode_project(&encode_project_data(&ProjectSnapshot::capture(&app)).unwrap())
            .err()
            .unwrap();
        assert!(err.contains("layer ids"), "{err}");
    }

    #[test]
    #[ignore = "fuzzing"]
    fn fuzz_project() {
        let app = rich_app();
        open(&encode_project(&app).unwrap());
        let bare = encode_project_data(&ProjectSnapshot::capture(&app)).unwrap();
        crate::fuzz::fuzz("rpainter", &bare, std::time::Duration::from_secs(2), open);
    }
}

#[cfg(test)]
mod fill_timing {
    use super::*;
    use crate::app::state::LayerState;
    use eframe::egui::Vec2;

    #[test]
    #[ignore = "timing; run with --release --ignored"]
    fn fill_on_a_4000px_canvas() {
        use crate::canvas::fill::{FillSettings, bucket_fill};
        let canvas = Canvas::new(4000, 4000, Color32::WHITE, TILE_SIZE);
        // Line art: a grid of 200 px cells on layer 1.
        for ty in 0..63 {
            for tx in 0..63 {
                let mut t = vec![Color32::TRANSPARENT; TILE_SIZE * TILE_SIZE];
                for y in 0..TILE_SIZE {
                    for x in 0..TILE_SIZE {
                        let (gx, gy) = (tx * 64 + x, ty * 64 + y);
                        if gx % 200 < 3 || gy % 200 < 3 {
                            t[y * TILE_SIZE + x] = Color32::BLACK;
                        }
                    }
                }
                canvas.set_layer_tile_data(1, tx as i32, ty as i32, t);
            }
        }
        let _ = LayerState::new(2);
        let t = std::time::Instant::now();
        for i in 0..100 {
            let _ = canvas.render_reference(None, (i % 60) * 64, 640, 64, 64);
        }
        eprintln!("render one tile (all visible): {:?}", t.elapsed() / 100);
        let t = std::time::Instant::now();
        let px = canvas.render_reference(None, 0, 0, 4000, 64);
        eprintln!(
            "render 64 rows (all visible): {:?} ({} px)",
            t.elapsed(),
            px.len()
        );
        for (label, source) in [("all visible", None), ("layer", Some(1))] {
            let r = |x, y, w, h| canvas.render_reference(source, x, y, w, h);
            for gap in [0u8, 6] {
                let t = std::time::Instant::now();
                let s = FillSettings {
                    gap,
                    ..FillSettings::default()
                };
                let m = bucket_fill(&r, 4000, 4000, (100, 100), &s).unwrap();
                eprintln!(
                    "{label} gap {gap}: small cell {:?} ({}x{})",
                    t.elapsed(),
                    m.w,
                    m.h
                );
            }
        }
        {
            let r = |x, y, w, h| canvas.render_reference(None, x, y, w, h);
            let s = FillSettings {
                tolerance: 255,
                ..FillSettings::default()
            };
            let t = std::time::Instant::now();
            let m = bucket_fill(&r, 4000, 4000, (100, 100), &s).unwrap();
            eprintln!("mask only, whole canvas: {:?}", t.elapsed());
            let t = std::time::Instant::now();
            let mut a = crate::canvas::history::UndoAction {
                tiles: vec![],
                selection: None,
                transform: None,
                layer_action: None,
            };
            canvas.paint_mask(1, &m, Color32::RED, &mut a);
            eprintln!("paint_mask whole canvas: {:?}", t.elapsed());
        }
        // Whole app path (mask + paint + undo), small cell and a whole-canvas fill.
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.active_tool = crate::app::tools::Tool::Fill;
        for (i, (label, pos, tol)) in [("app whole canvas", (100.0, 100.0), 255); 4]
            .into_iter()
            .enumerate()
        {
            app.workspace.fill.settings.tolerance = tol;
            app.brush_state.brush.brush_options.color = [Color32::BLUE, Color32::GREEN][i % 2];
            let t = std::time::Instant::now();
            app.fill_press(Vec2::new(pos.0, pos.1));
            eprintln!("{label}: {:?}", t.elapsed());
        }
    }
}

#[cfg(test)]
mod perf {
    //! Timings for the heavier features on a 4000 px canvas:
    //! `cargo test --release --lib perf:: -- --ignored --nocapture --test-threads 1`
    use super::*;
    use crate::app::tools::Tool;
    use crate::selection::SelectionType;
    use crate::selection::transform::TransformInfo;
    use eframe::egui::Vec2;
    use std::time::Instant;

    const N: usize = 4000;

    /// Layer 1 fully painted with a smooth gradient plus some lines.
    fn big_app() -> PainterApp {
        let canvas = Canvas::new(N, N, Color32::WHITE, TILE_SIZE);
        let tiles = N.div_ceil(TILE_SIZE);
        for ty in 0..tiles {
            for tx in 0..tiles {
                let t: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
                    .map(|i| {
                        let (x, y) = (
                            tx * TILE_SIZE + i % TILE_SIZE,
                            ty * TILE_SIZE + i / TILE_SIZE,
                        );
                        if x % 250 < 3 || y % 250 < 3 {
                            Color32::BLACK
                        } else {
                            Color32::from_rgb((x / 16) as u8, (y / 16) as u8, ((x + y) / 32) as u8)
                        }
                    })
                    .collect();
                canvas.set_layer_tile_data(1, tx as i32, ty as i32, t);
            }
        }
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        // Every core, as the app has (the test app has one thread).
        app.workspace.pool = std::sync::Arc::new(rayon::ThreadPoolBuilder::new().build().unwrap());
        app
    }

    fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let r = f();
        eprintln!("{label:<40} {:>10.1?}", t.elapsed());
        r
    }

    /// A smudge-tool or mixing-brush stroke across the canvas, painted on
    /// the stroke worker and waited for.
    #[test]
    #[ignore = "timing"]
    fn smudge() {
        let path: Vec<(Vec2, f32)> = (0..240)
            .map(|i| {
                let t = i as f32 / 239.0;
                let p = Vec2::new(800.0 + 2400.0 * t, 2000.0 + 600.0 * (t * 9.0).sin());
                (p, 0.3 + 0.7 * t)
            })
            .collect();
        for diameter in [40.0, 80.0, 200.0] {
            for (what, smudge, mixing) in [
                ("smudge", true, false),
                ("blur", false, false),
                ("mixing brush", false, true),
            ] {
                let mut app = big_app();
                app.brush_state.brush.brush_options.diameter = diameter;
                if mixing {
                    app.brush_state.brush.mixing = Some(Default::default());
                }
                let started = Instant::now();
                if mixing {
                    app.set_brush_tool(false);
                    app.start_stroke_with_pressure(path[0].0, path[0].1);
                    for &(p, pressure) in &path[1..] {
                        app.add_stroke_point(p, pressure);
                    }
                    app.finish_stroke();
                } else {
                    app.set_blend_tool(smudge);
                    app.blend_press(path[0].0, path[0].1);
                    for &(p, pressure) in &path[1..] {
                        app.blend_drag(p, pressure);
                    }
                    app.blend_release();
                }
                app.settle_strokes();
                eprintln!(
                    "{:<40} {:>10.1?}",
                    format!("{what}: {diameter} px, 240 samples"),
                    started.elapsed()
                );
            }
        }
        // Krita's colour smudge (imported presets), the default one and
        // the ones in the bundles at hand.
        let stroke_with = |label: &str, brush: crate::brush_engine::brush::Brush| {
            let mut app = big_app();
            app.brush_state.brush = brush;
            app.set_brush_tool(false);
            let started = Instant::now();
            app.start_stroke_with_pressure(path[0].0, path[0].1);
            for &(p, pressure) in &path[1..] {
                app.add_stroke_point(p, pressure);
            }
            app.finish_stroke();
            app.settle_strokes();
            eprintln!("{label:<60} {:>10.1?}", started.elapsed());
        };
        for diameter in [80.0, 200.0] {
            let mut brush = big_app().brush_state.brush.clone();
            brush.brush_options.diameter = diameter;
            brush.mixing = Some(crate::brush_engine::brush_options::Mixing {
                krita: Some(Default::default()),
                ..Default::default()
            });
            stroke_with(&format!("krita smudge: {diameter} px"), brush);
        }
        let home = std::env::var("HOME").unwrap_or_default();
        for bundle in [
            "Peaches_Painting_Brushes.bundle",
            "Rakurri_Brush_Set_V2.0.bundle",
        ] {
            let Ok(bytes) = std::fs::read(format!("{home}/Downloads/{bundle}")) else {
                continue;
            };
            let Ok(imported) = crate::brush_engine::import::import(bundle, &bytes) else {
                continue;
            };
            let smudges = imported
                .presets
                .into_iter()
                .filter(|p| p.brush.mixing.is_some_and(|m| m.krita.is_some()));
            for preset in smudges.take(4) {
                stroke_with(
                    &format!(
                        "{} ({} px)",
                        preset.name, preset.brush.brush_options.diameter
                    ),
                    preset.brush,
                );
            }
        }
    }

    #[test]
    #[ignore = "timing"]
    fn export() {
        let app = big_app();
        let img = time("export: flatten", || app.canvas.flatten());
        {
            let mut reference = eframe::egui::ColorImage::new([N, N], Color32::TRANSPARENT);
            app.canvas
                .write_region_to_color_image(0, 0, N, N, &mut reference, 1);
            assert!(reference.pixels == img.pixels, "parallel flatten matches");
        }
        let rgba = time("export: to rgba", || {
            crate::project::export::to_rgba_image(img.clone()).unwrap()
        });
        time("export: encode png", || {
            let mut out = Vec::new();
            rgba.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            eprintln!("  png size {} KB", out.len() / 1024);
        });
        // A noisy, photo-like picture compresses much harder.
        let noisy = image::RgbaImage::from_fn(N as u32, N as u32, |x, y| {
            let n = (x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) >> 27;
            image::Rgba([
                (x / 16) as u8 ^ n as u8,
                (y / 16) as u8,
                ((x + y) / 32) as u8,
                255,
            ])
        });
        use image::ImageEncoder;
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        for (label, c, f) in [
            (
                "png default",
                CompressionType::Default,
                FilterType::Adaptive,
            ),
            ("png fast", CompressionType::Fast, FilterType::Adaptive),
            ("png best", CompressionType::Best, FilterType::Adaptive),
        ] {
            time(&format!("export: noisy {label}"), || {
                let mut out = Vec::new();
                PngEncoder::new_with_quality(&mut out, c, f)
                    .write_image(&noisy, N as u32, N as u32, image::ExtendedColorType::Rgba8)
                    .unwrap();
                eprintln!("  {label}: {} KB", out.len() / 1024);
            });
        }
        for f in [
            crate::project::export::ExportFormat::Jpeg,
            crate::project::export::ExportFormat::Tiff,
        ] {
            time(&format!("export: {}", f.label()), || {
                let r = crate::project::export::encode_color_image(img.clone(), f);
                eprintln!(
                    "  {:?}",
                    r.as_ref().map(|b| b.len() / 1024).map_err(|e| e.clone())
                );
            });
        }
        time("export: whole (encode_color_image png)", || {
            crate::project::export::encode_color_image(
                img,
                crate::project::export::ExportFormat::Png,
            )
            .unwrap()
        });
    }

    #[test]
    #[ignore = "timing"]
    fn transform() {
        let mut app = big_app();
        app.active_tool = Tool::Transform(TransformInfo::default());
        time("transform: float whole layer", || {
            transform_press_at(&mut app)
        });
        for (label, dragging, rot) in [
            ("transform: 1st preview (bounds scan)", false, 0.1),
            ("transform: preview rotate, full quality", false, 0.17),
            ("transform: preview rotate, while dragging", true, 0.2),
        ] {
            time(label, || {
                if let Tool::Transform(ref mut i) = app.active_tool {
                    i.rotation = rot;
                    i.start_pos = dragging.then_some(Vec2::ZERO);
                }
                app.layer_state.transform_preview_pending = true;
                crate::app::tools::transform::flush_transform_preview(&mut app);
            });
        }
        time("transform: preview (move 7px)", || {
            if let Tool::Transform(ref mut i) = app.active_tool {
                i.rotation = 0.0;
                i.offset = Vec2::new(7.0, 3.0);
            }
            app.layer_state.transform_preview_pending = true;
            crate::app::tools::transform::flush_transform_preview(&mut app);
        });
        time("transform: commit", || {
            crate::app::tools::transform::commit_floating_layer(&mut app)
        });
        // A selection float.
        app.selection_manager
            .start_selection(Vec2::new(500.0, 500.0), SelectionType::Rectangle);
        app.selection_manager
            .update_selection(Vec2::new(1500.0, 1500.0));
        app.selection_manager.end_selection();
        time("transform: float 1000px selection", || {
            transform_press_at(&mut app)
        });
        time("transform: cancel", || {
            crate::app::tools::transform::cancel_floating_layer(&mut app)
        });
    }

    fn transform_press_at(app: &mut PainterApp) {
        crate::app::tools::transform::transform_press(app, Vec2::new(1000.0, 1000.0));
        crate::app::tools::transform::transform_release(app);
    }

    #[test]
    #[ignore = "timing"]
    fn liquify() {
        let mut app = big_app();
        app.active_tool = Tool::Liquify;
        app.workspace.liquify.radius = 150.0;
        time("liquify: begin + 1st dab", || {
            app.liquify_press(Vec2::new(2000.0, 2000.0))
        });
        // Each move as a frame: the field, then the layer drawn.
        time("liquify: 300 px drag", || {
            for i in 1..=30 {
                app.liquify_drag(Vec2::new(2000.0 + i as f32 * 10.0, 2000.0));
                app.liquify_flush();
            }
        });
        app.liquify_release();
        time("liquify: commit", || app.liquify_commit());
        use crate::canvas::liquify::LiquifyMode;
        for mode in [
            LiquifyMode::TwirlCw,
            LiquifyMode::Pinch,
            LiquifyMode::Bloat,
            LiquifyMode::Smooth,
        ] {
            app.workspace.liquify.mode = mode;
            app.liquify_press(Vec2::new(2000.0, 2000.0));
            time(&format!("liquify: {mode:?} hold, 30 frames"), || {
                for _ in 0..30 {
                    app.liquify_hold(1.0 / 60.0);
                    app.liquify_flush();
                }
            });
            time(&format!("liquify: {mode:?} 300 px drag"), || {
                for i in 1..=30 {
                    app.liquify_drag(Vec2::new(2000.0 + i as f32 * 10.0, 2000.0));
                    app.liquify_flush();
                }
            });
            app.liquify_release();
            app.liquify_commit();
        }
    }

    #[test]
    #[ignore = "timing"]
    fn palette() {
        let mut app = big_app();
        app.workspace.palette.count = 16;
        time("palette: extract 16 (all visible)", || {
            app.extract_palette()
        });
        app.workspace.palette.from_layer = true;
        time("palette: extract 16 (layer)", || app.extract_palette());
        let pal = app.workspace.palette.extracted.clone();
        time("palette: recolour layer", || app.recolor_layer(&pal));
        app.workspace.palette.dither = true;
        time("palette: recolour layer (dither)", || {
            app.recolor_layer(&pal)
        });
    }

    #[test]
    #[ignore = "timing"]
    fn import() {
        let mut app = big_app();
        let img =
            image::RgbaImage::from_fn(3000, 2000, |x, y| image::Rgba([x as u8, y as u8, 90, 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        time("import: 3000x2000 png (decode+place)", || {
            app.import_image_bytes("p", &png).unwrap()
        });
        let big = image::RgbaImage::from_pixel(6000, 6000, image::Rgba([10, 20, 30, 255]));
        time("import: 6000x6000 raw (resize+place)", || {
            app.import_rgba("big", big)
        });
    }

    #[test]
    #[ignore = "timing"]
    fn selection_and_enclose() {
        let mut app = big_app();
        app.selection_manager.canvas_size = [N, N];
        app.selection_manager.brush_radius = 100.0;
        time("selection brush: 60 px stroke x 40", || {
            app.selection_manager
                .start_selection(Vec2::new(500.0, 500.0), SelectionType::Brush);
            for i in 0..40 {
                app.selection_manager
                    .update_selection(Vec2::new(500.0 + i as f32 * 60.0, 500.0 + i as f32 * 30.0));
            }
            app.selection_manager.end_selection();
        });
        time("selection: invert", || app.selection_manager.invert());
        time("selection: outline", || {
            if let Some(crate::selection::SelectionShape::Mask(m)) =
                &app.selection_manager.current_shape
            {
                m.outline().len()
            } else {
                0
            }
        });
        app.selection_manager.clear_selection();
        let lasso: Vec<Vec2> = (0..200)
            .map(|i| {
                let a = i as f32 / 200.0 * std::f32::consts::TAU;
                Vec2::new(2000.0 + a.cos() * 1500.0, 2000.0 + a.sin() * 1500.0)
            })
            .collect();
        app.workspace.fill.mode = crate::app::tools::fill::FillMode::Enclose;
        app.workspace.fill.path = lasso.clone();
        time("enclose fill: 3000 px lasso", || app.fill_release());
    }
    #[test]
    #[ignore = "timing"]
    fn every_filter() {
        use crate::canvas::filters::Filter;
        let mut app = big_app();
        // As the app runs them: the UI thread queues the filter, the
        // stroke worker runs it.
        app.workspace.jobs.defer = true;
        for f in Filter::MENU.iter().flat_map(|g| g.iter()) {
            let started = std::time::Instant::now();
            app.filter_open(*f);
            app.filter_commit();
            let ui = started.elapsed();
            app.run_jobs();
            app.release_canvas();
            eprintln!(
                "filter: {:<28} UI thread {:>7.1?}   done {:>7.1?}",
                f.name(),
                ui,
                started.elapsed()
            );
            app.apply_history_now(false);
        }
    }

    #[test]
    #[ignore = "timing"]
    fn filter_stages() {
        // Where the time of applying a cheap filter to a whole layer goes.
        use crate::canvas::filters::Filter;
        let mut app = big_app();
        let pool = std::sync::Arc::clone(&app.workspace.pool);
        let canvas = &app.canvas;
        let bounds = [0, 0, N as i32, N as i32];
        let original = time("filter stage: capture region", || {
            pool.install(|| canvas.capture_region(1, bounds))
        });
        let src = time("filter stage: region to pixels", || {
            pool.install(|| original.pixels(canvas.tile_size()))
        });
        let out = time("filter stage: apply (invert)", || {
            pool.install(|| Filter::Invert.apply(&src, N, N, (0, 0)))
        });
        time("filter stage: write back", || {
            pool.install(|| canvas.replace_region(1, &original, &out, None))
        });
        let tiles = time("filter stage: undo snapshots", || {
            pool.install(|| canvas.region_snapshots(1, &original))
        });
        time("filter stage: push undo", || {
            app.push_undo(crate::canvas::history::UndoAction {
                tiles,
                selection: None,
                transform: None,
                layer_action: None,
            })
        });
    }

    #[test]
    #[ignore = "timing"]
    fn layer_kinds_and_styles() {
        use crate::canvas::filters::Filter;
        use crate::canvas::layer_style::{Border, LayerFill, LayerStyle};
        let mut app = big_app();
        time("composite: two paint layers", || app.canvas.flatten());
        for f in Filter::ADJUSTMENTS {
            app.canvas_mut().active_layer_idx = 1;
            app.add_adjustment_layer(f);
            let idx = app.canvas.active_layer_idx;
            time(&format!("composite: + {} layer", f.name()), || {
                app.canvas.flatten()
            });
            app.remove_layer(idx);
        }
        for (label, fill) in [
            ("colour", LayerFill::Colour([30, 90, 200])),
            (
                "gradient",
                LayerFill::Gradient {
                    colours: Default::default(),
                    shape: crate::canvas::gradient::GradientShape::Linear,
                    start: [0.0, 0.0],
                    end: [N as f32, N as f32],
                },
            ),
        ] {
            app.canvas_mut().active_layer_idx = 1;
            app.add_fill_layer(fill);
            let idx = app.canvas.active_layer_idx;
            time(&format!("composite: + {label} fill layer"), || {
                app.canvas.flatten()
            });
            app.remove_layer(idx);
        }
        for width in [4.0, 16.0, 40.0] {
            app.set_layer_style(
                1,
                LayerStyle {
                    fill: None,
                    border: Some(Border {
                        width,
                        ..Border::default()
                    }),
                    impasto: None,
                },
            );
            time(
                &format!("composite: {width} px border on the layer"),
                || app.canvas.flatten(),
            );
        }
        app.set_layer_style(1, LayerStyle::default());

        app.add_vector_layer();
        let idx = app.canvas.active_layer_idx;
        let lines: Vec<Vec<Vec2>> = (0..500)
            .map(|i| {
                let y = 100.0 + i as f32 * 7.5;
                (0..60)
                    .map(|k| Vec2::new(100.0 + k as f32 * 60.0, y + (k as f32 * 0.7).sin() * 40.0))
                    .collect()
            })
            .collect();
        time("vector: add 500 lines of 60 points", || {
            for l in &lines {
                app.add_vector_line(idx, l);
            }
        });
        time("composite: + 500-line vector layer", || {
            app.canvas.flatten()
        });
        time("vector: rasterise the layer", || {
            app.rasterise_vector_layer(idx)
        });
    }

    #[test]
    #[ignore = "profiling: a small tree composite to run under callgrind"]
    fn tree_composite_profile() {
        let canvas = Canvas::new(1024, 1024, Color32::WHITE, TILE_SIZE);
        for ty in 0..16 {
            for tx in 0..16 {
                let t: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
                    .map(|i| Color32::from_rgb((i % 251) as u8, (tx * 16) as u8, (ty * 16) as u8))
                    .collect();
                canvas.set_layer_tile_data(1, tx, ty, t);
            }
        }
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        app.add_folder();
        for _ in 0..3 {
            std::hint::black_box(app.canvas.flatten());
        }
    }

    #[test]
    #[ignore = "timing"]
    fn every_export_format() {
        use crate::project::export::{ExportFormat, encode_color_image};
        let app = big_app();
        let img = app.canvas.flatten();
        for f in ExportFormat::ALL {
            time(&format!("export: {}", f.label()), || {
                let size = match f {
                    ExportFormat::Psd => {
                        psd::encode_psd(&psd::PsdDocument::from_canvas(&app.canvas))
                            .unwrap()
                            .len()
                    }
                    ExportFormat::Svg => svg::document_svg(&app.canvas).unwrap().len(),
                    _ => encode_color_image(img.clone(), f).unwrap().len(),
                };
                eprintln!("  {} KB", size / 1024);
            });
        }
    }

    #[test]
    #[ignore = "timing"]
    fn open_other_apps_documents() {
        // A 4000 px PSD of the test picture; set `RP_OPEN` to other files
        // (.kra, .clip) to time those too.
        let app = big_app();
        let bytes = psd::encode_psd(&psd::PsdDocument::from_canvas(&app.canvas)).unwrap();
        time("open: 4000 px psd (decode)", || {
            psd::decode_psd(&bytes).unwrap()
        });
        let doc = psd::decode_psd(&bytes).unwrap();
        time("open: 4000 px psd (to layers)", || {
            doc.into_canvas().unwrap()
        });
        for path in std::env::var("RP_OPEN").unwrap_or_default().split(':') {
            let path = std::path::Path::new(path);
            let Some(decode) = path
                .extension()
                .and_then(|e| foreign_decoder(&e.to_string_lossy().to_ascii_lowercase()))
            else {
                continue;
            };
            let bytes = fs::read(path).unwrap();
            let doc = time(&format!("open: {} (decode)", path.display()), || {
                decode(&bytes).unwrap()
            });
            time(&format!("open: {} (to layers)", path.display()), || {
                doc.into_canvas().unwrap()
            });
        }
    }
}

#[cfg(test)]
mod soft_brush_look {
    use super::*;
    use eframe::egui::Vec2;

    /// Paints soft strokes like the screenshots and writes them to
    /// `$SOFT_OUT` (a PNG) to inspect by eye.
    #[test]
    #[ignore = "writes an image for inspection"]
    fn paint_soft_strokes() {
        let canvas = Canvas::new(512, 512, Color32::WHITE, TILE_SIZE);
        let mut app = tests::test_app_pub(canvas);
        app.canvas_mut().active_layer_idx = 1;
        let o = &mut app.brush_state.brush.brush_options;
        o.diameter = 120.0;
        o.hardness = 20.0;
        o.spacing = 25.0;
        o.flow = 100.0;
        o.pressure_size = true;
        // A curved stroke at full pressure.
        app.start_stroke_with_pressure(Vec2::new(20.0, 60.0), 1.0);
        for i in 1..=60 {
            let t = i as f32 / 60.0;
            app.add_stroke_point(Vec2::new(20.0 + t * 470.0, 60.0 + t * t * 120.0), 1.0);
        }
        app.finish_stroke();
        // A stroke growing from light to full pressure.
        app.start_stroke_with_pressure(Vec2::new(60.0, 380.0), 0.05);
        for i in 1..=40 {
            let t = i as f32 / 40.0;
            app.add_stroke_point(
                Vec2::new(60.0 + t * 250.0, 380.0 - t * 60.0),
                0.05 + 0.95 * t,
            );
        }
        app.finish_stroke();
        app.stroke_worker.wait_idle();
        let img = app.canvas.flatten();
        let out = std::env::var("SOFT_OUT").unwrap_or_else(|_| "soft.png".into());
        crate::project::export::save_color_image(
            img,
            out,
            crate::project::export::ExportFormat::Png,
        )
        .unwrap();
    }
}
