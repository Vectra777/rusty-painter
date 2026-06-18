use crate::{
    PainterApp,
    app::{
        painter_state::LayerState,
        state::{ColorModel, TILE_SIZE, validate_canvas_size},
        tools::Tool,
    },
    canvas::{
        Canvas,
        history::{History, TileSnapshot, UndoAction},
        storage::{CanvasLayerSnapshot, CanvasTileSnapshot},
    },
};
use eframe::egui::{Color32, Vec2};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

mod blobs;
mod convert;
mod preview;

use blobs::{StoredBlob, push_blob, read_blob};
use convert::{StoredColor, StoredColorModel, StoredSelectionShape, StoredTransformInfo};
use preview::{StoredPreview, preview_png_blob};

const MAGIC: &[u8; 8] = b"RPNTV001";
const PROJECT_FORMAT: &str = "rusty-painter-project";
const PROJECT_VERSION: u32 = 2;

pub(crate) struct LoadedProject {
    pub canvas: Canvas,
    pub color_model: ColorModel,
    pub histories: Vec<History>,
}

pub(crate) fn save_project(app: &PainterApp, path: impl AsRef<Path>) -> Result<(), String> {
    fs::write(path, encode_project(app)?).map_err(|err| format!("Save failed: {err}"))
}

pub(crate) fn load_project(path: impl AsRef<Path>) -> Result<LoadedProject, String> {
    decode_project(&fs::read(path).map_err(|err| format!("Open failed: {err}"))?)
}

impl PainterApp {
    pub(crate) fn save_project_to_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        if self.brush_state.is_drawing {
            self.finish_stroke();
        }
        save_project(self, with_project_extension(path.as_ref()))
    }

    pub(crate) fn load_project_from_path(
        &mut self,
        ctx: &eframe::egui::Context,
        path: impl AsRef<Path>,
    ) -> Result<(), String> {
        let loaded = load_project(path)?;
        let width = loaded.canvas.width();
        let height = loaded.canvas.height();
        let layer_count = loaded.canvas.layers.len();

        self.canvas = loaded.canvas;
        self.workspace.color_model = loaded.color_model;
        self.layer_state = LayerState::new(layer_count);
        self.layer_state.histories = loaded.histories;
        self.render_cache = Self::initialize_render_cache(ctx, width, height, layer_count);
        self.brush_state.stroke = None;
        self.brush_state.is_drawing = false;
        self.active_tool = Tool::Brush;
        self.selection_manager.clear_selection();
        self.viewport.offset = Vec2::ZERO;
        self.viewport.zoom = 1.0;
        self.viewport.rotation = 0.0;
        self.workspace.first_frame = true;
        Ok(())
    }
}

fn encode_project(app: &PainterApp) -> Result<Vec<u8>, String> {
    let mut blobs = Vec::new();
    let manifest = ProjectFile::from_app(app, &mut blobs)?;
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

fn decode_project(bytes: &[u8]) -> Result<LoadedProject, String> {
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

fn with_project_extension(path: &Path) -> std::path::PathBuf {
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
        .chunks_exact(4)
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
    active_layer_idx: usize,
    preview: Option<StoredPreview>,
    layers: Vec<StoredLayer>,
    histories: Vec<StoredHistory>,
}

impl ProjectFile {
    fn from_app(app: &PainterApp, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            format: PROJECT_FORMAT.to_string(),
            version: PROJECT_VERSION,
            width: app.canvas.width(),
            height: app.canvas.height(),
            tile_size: app.canvas.tile_size(),
            clear_color: StoredColor::from_color(app.canvas.clear_color()),
            color_model: StoredColorModel::from(app.workspace.color_model),
            active_layer_idx: app.canvas.active_layer_idx,
            preview: preview_png_blob(&app.canvas, blobs)?,
            layers: app
                .canvas
                .layer_snapshots()
                .into_iter()
                .map(|layer| StoredLayer::from_snapshot(layer, blobs))
                .collect::<Result<_, _>>()?,
            histories: app
                .layer_state
                .histories
                .iter()
                .map(|history| StoredHistory::from_history(history, blobs))
                .collect::<Result<_, _>>()?,
        })
    }

    fn into_loaded_project(self, blobs: &[u8]) -> Result<LoadedProject, String> {
        if self.format != PROJECT_FORMAT || self.version != PROJECT_VERSION {
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
            .map(|layer| layer.into_snapshot(self.tile_size, blobs))
            .collect::<Result<_, _>>()?;
        let mut canvas = Canvas::new(
            self.width,
            self.height,
            self.clear_color.to_color(),
            self.tile_size,
        );
        canvas.replace_layers_from_snapshots(layers, self.active_layer_idx);

        let mut histories: Vec<_> = self
            .histories
            .into_iter()
            .map(|history| history.into_history(self.tile_size, blobs))
            .collect::<Result<_, _>>()?;
        histories.resize_with(canvas.layers.len(), History::new);
        histories.truncate(canvas.layers.len());

        Ok(LoadedProject {
            canvas,
            color_model: self.color_model.into(),
            histories,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredLayer {
    name: String,
    visible: bool,
    opacity: f32,
    locked: bool,
    tiles: Vec<StoredTile>,
}

impl StoredLayer {
    fn from_snapshot(layer: CanvasLayerSnapshot, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            name: layer.name,
            visible: layer.visible,
            opacity: layer.opacity,
            locked: layer.locked,
            tiles: layer
                .tiles
                .into_iter()
                .map(|tile| StoredTile::from_snapshot(tile, blobs))
                .collect::<Result<_, _>>()?,
        })
    }

    fn into_snapshot(self, tile_size: usize, blobs: &[u8]) -> Result<CanvasLayerSnapshot, String> {
        Ok(CanvasLayerSnapshot {
            name: self.name,
            visible: self.visible,
            opacity: self.opacity,
            locked: self.locked,
            tiles: self
                .tiles
                .into_iter()
                .map(|tile| tile.into_snapshot(tile_size, blobs))
                .collect::<Result<_, _>>()?,
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
    fn from_snapshot(tile: CanvasTileSnapshot, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            tx: tile.tx,
            ty: tile.ty,
            rgba_zstd: push_blob(blobs, &colors_to_bytes(&tile.data))?,
        })
    }

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
}

impl StoredUndoAction {
    fn from_action(action: &UndoAction, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            tiles: action
                .tiles
                .iter()
                .map(|tile| StoredTileSnapshot::from_snapshot(tile, blobs))
                .collect::<Result<_, _>>()?,
            selection: action
                .selection
                .as_ref()
                .map(|shape| shape.as_ref().map(StoredSelectionShape::from)),
            transform: action.transform.as_ref().map(StoredTransformInfo::from),
        })
    }

    fn into_action(self, tile_size: usize, blobs: &[u8]) -> Result<UndoAction, String> {
        Ok(UndoAction {
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
    layer_idx: usize,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
    rgba_zstd: StoredBlob,
}

impl StoredTileSnapshot {
    fn from_snapshot(snapshot: &TileSnapshot, blobs: &mut Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            tx: snapshot.tx,
            ty: snapshot.ty,
            layer_idx: snapshot.layer_idx,
            x0: snapshot.x0,
            y0: snapshot.y0,
            width: snapshot.width,
            height: snapshot.height,
            rgba_zstd: push_blob(blobs, &colors_to_bytes(&snapshot.data))?,
        })
    }

    fn into_snapshot(self, tile_size: usize, blobs: &[u8]) -> Result<TileSnapshot, String> {
        let data = bytes_to_colors(read_blob(blobs, &self.rgba_zstd)?)?;
        if self.width == 0
            || self.height == 0
            || self.x0 + self.width > tile_size
            || self.y0 + self.height > tile_size
            || data.len() != self.width * self.height
        {
            return Err("Invalid undo tile snapshot".to_string());
        }
        Ok(TileSnapshot {
            tx: self.tx,
            ty: self.ty,
            layer_idx: self.layer_idx,
            x0: self.x0,
            y0: self.y0,
            width: self.width,
            height: self.height,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::painter_state::LayerState, brush_engine::brush::Brush};
    use rayon::ThreadPoolBuilder;

    fn test_app(canvas: Canvas, histories: Vec<History>) -> PainterApp {
        let layer_count = canvas.layers.len();
        PainterApp {
            canvas,
            brush_state: crate::app::painter_state::BrushState::new(
                Brush::new(1.0, 100.0, Color32::BLACK, 10.0),
                Vec::new(),
                ".".into(),
                true,
            ),
            viewport: crate::app::painter_state::ViewportState::new(1.0, Vec2::ZERO),
            render_cache: crate::app::painter_state::RenderCache::new(
                Vec::new(),
                Vec::new(),
                1,
                1,
                layer_count,
                true,
            ),
            layer_state: {
                let mut state = LayerState::new(layer_count);
                state.histories = histories;
                state
            },
            modal_state: crate::app::painter_state::ModalState::new(
                crate::app::state::NewCanvasSettings::from_canvas(&Canvas::new(
                    1,
                    1,
                    Color32::WHITE,
                    TILE_SIZE,
                )),
            ),
            export_state: crate::app::painter_state::ExportState::new(),
            workspace: crate::app::painter_state::WorkspaceState::new(
                1,
                1,
                ThreadPoolBuilder::new().num_threads(1).build().unwrap(),
                ColorModel::Rgba,
            ),
            active_tool: Tool::Brush,
            selection_manager: crate::selection::SelectionManager::new(),
            dock_left: egui_dock::DockState::new(Vec::new()),
            dock_right: egui_dock::DockState::new(Vec::new()),
            tablet: None,
        }
    }

    #[test]
    fn project_round_trips_compressed_tiles_history_and_preview() {
        let canvas = Canvas::new(TILE_SIZE, TILE_SIZE, Color32::WHITE, TILE_SIZE);
        let pixels = vec![Color32::from_rgb(255, 0, 0); TILE_SIZE * TILE_SIZE];
        canvas.set_layer_tile_data(1, 0, 0, pixels.clone());

        let mut history = History::new();
        history.push_action(UndoAction {
            tiles: vec![TileSnapshot {
                tx: 0,
                ty: 0,
                layer_idx: 1,
                x0: 0,
                y0: 0,
                width: TILE_SIZE,
                height: TILE_SIZE,
                data: pixels,
            }],
            selection: Some(None),
            transform: None,
        });

        let encoded = encode_project(&test_app(canvas, vec![History::new(), history])).unwrap();
        assert!(encoded.starts_with(MAGIC));
        assert!(encoded.len() < TILE_SIZE * TILE_SIZE * 4);

        let loaded = decode_project(&encoded).unwrap();
        assert_eq!(
            loaded.canvas.get_layer_tile_data(1, 0, 0).unwrap()[0],
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(loaded.histories[1].stacks().0.len(), 1);
    }
}
