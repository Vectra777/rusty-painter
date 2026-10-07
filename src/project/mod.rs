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
pub(crate) mod tests;

#[cfg(test)]
mod fuzz_tests;

#[cfg(test)]
mod fill_timing;

#[cfg(test)]
mod perf;

#[cfg(test)]
mod soft_brush_look;
