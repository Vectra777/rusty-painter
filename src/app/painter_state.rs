use crate::app::state::{CanvasTile, ColorModel, NewCanvasSettings, TextureAtlas};
use crate::{
    brush_engine::{
        brush::{Brush, BrushPreset},
        brush_options::PixelBrushShape,
        stroke::{StrokeState, StrokeTiles},
    },
    canvas::history::{History, UndoAction},
    ui::{brush_settings::BrushPreviewState, export_modal::ExportProgress},
};
use crate::canvas::storage::LayerId;
use eframe::egui::{self, Color32, Rgba, Vec2};
use rustc_hash::FxHashMap;
use rayon::ThreadPool;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

/// An in-progress stroke: the interpolation state plus the undo action that
/// accumulates tile snapshots for it. These previously lived as two separate
/// `Option`s (`BrushState.stroke` and `LayerState.current_undo_action`) kept
/// `Some`/`None` in sync by convention across several call sites; bundling
/// them here makes that pairing structural instead.
pub struct StrokeSession {
    pub stroke: StrokeState,
    pub undo_action: UndoAction,
}

/// Brush-related state and resources
pub struct BrushState {
    pub brush: Brush,
    pub brush_preview: BrushPreviewState,
    pub presets: Vec<BrushPreset>,
    pub preset_previews: HashMap<String, egui::TextureHandle>,
    pub loaded_brush_tips: Vec<(String, PixelBrushShape, Option<egui::TextureHandle>)>,
    pub brushes_path: PathBuf,
    pub session: Option<StrokeSession>,
    pub is_drawing: bool,
    pub use_masked_brush: bool,
    pub show_new_preset_modal: bool,
    pub new_preset_name: String,
}

impl BrushState {
    pub fn new(
        brush: Brush,
        presets: Vec<BrushPreset>,
        brushes_path: PathBuf,
        use_masked_brush: bool,
    ) -> Self {
        Self {
            brush,
            brush_preview: BrushPreviewState::default(),
            presets,
            preset_previews: HashMap::new(),
            loaded_brush_tips: Vec::new(),
            brushes_path,
            session: None,
            is_drawing: false,
            use_masked_brush,
            show_new_preset_modal: false,
            new_preset_name: String::new(),
        }
    }
}

/// Viewport/camera transformation state
pub struct ViewportState {
    pub zoom: f32,
    pub offset: Vec2,
    pub rotation: f32,
    pub is_panning: bool,
    pub is_rotating: bool,
    pub is_primary_down: bool,
}

impl ViewportState {
    pub fn new(zoom: f32, offset: Vec2) -> Self {
        Self {
            zoom,
            offset,
            rotation: 0.0,
            is_panning: false,
            is_rotating: false,
            is_primary_down: false,
        }
    }
}

pub struct BelowCache {
    /// `Canvas::composite_below_key` for the layers this was built from.
    pub key: (Color32, Vec<(LayerId, bool, u32)>),
    pub tiles: FxHashMap<(usize, usize), Vec<Rgba>>,
}

/// GPU texture atlas and rendering cache
pub struct RenderCache {
    pub tiles: Vec<CanvasTile>,
    pub atlases: Vec<TextureAtlas>,
    pub tiles_x: usize,
    pub tiles_y: usize,
    pub layer_caches: Vec<HashMap<(usize, usize), egui::ColorImage>>,
    pub layer_cache_dirty: Vec<HashSet<(usize, usize)>>,
    pub stroke_tiles: StrokeTiles,
    /// Composite of the layers below the active one, per tile, valid for the
    /// current stroke only (nothing but the active layer changes mid-stroke).
    pub below_cache: Option<BelowCache>,
    pub texture_generation: u64,
    pub disable_lod: bool,
}

impl RenderCache {
    pub fn new(
        tiles: Vec<CanvasTile>,
        atlases: Vec<TextureAtlas>,
        tiles_x: usize,
        tiles_y: usize,
        layer_count: usize,
        disable_lod: bool,
    ) -> Self {
        Self {
            tiles,
            atlases,
            tiles_x,
            tiles_y,
            layer_caches: vec![HashMap::new(); layer_count],
            layer_cache_dirty: vec![HashSet::new(); layer_count],
            stroke_tiles: StrokeTiles::default(),
            below_cache: None,
            texture_generation: 0,
            disable_lod,
        }
    }
}

/// Layer UI state and undo history
pub struct LayerState {
    pub layer_ui_colors: Vec<Color32>,
    pub layer_dragging: Option<usize>,
    pub floating_layer_idx: Option<usize>,
    pub floating_buffer: Option<HashMap<(i32, i32), Vec<Color32>>>,
    pub histories: Vec<History>,
}

impl LayerState {
    pub fn new(layer_count: usize) -> Self {
        Self {
            layer_ui_colors: vec![Color32::from_gray(40); layer_count],
            layer_dragging: None,
            floating_layer_idx: None,
            floating_buffer: None,
            histories: (0..layer_count).map(|_| History::new()).collect(),
        }
    }
}

/// Modal dialog state
pub struct ModalState {
    pub show_new_canvas_modal: bool,
    pub new_canvas: NewCanvasSettings,
    pub show_general_settings: bool,
}

impl ModalState {
    pub fn new(new_canvas: NewCanvasSettings) -> Self {
        Self {
            show_new_canvas_modal: false,
            new_canvas,
            show_general_settings: false,
        }
    }
}

/// Export operation state
pub struct ExportState {
    pub settings: crate::ui::export_modal::ExportSettings,
    pub message: Option<String>,
    pub in_progress: bool,
    pub task: Option<std::thread::JoinHandle<Result<String, String>>>,
    pub progress: f32,
    pub progress_rx: Option<mpsc::Receiver<ExportProgress>>,
    pub show_modal: bool,
}

impl ExportState {
    pub fn new() -> Self {
        Self {
            settings: crate::ui::export_modal::ExportSettings::new(),
            message: None,
            in_progress: false,
            task: None,
            progress: 0.0,
            progress_rx: None,
            show_modal: false,
        }
    }
}

impl Default for ExportState {
    fn default() -> Self {
        Self::new()
    }
}

/// Threading and workspace-level settings
pub struct WorkspaceState {
    pub thread_count: usize,
    pub max_threads: usize,
    pub pool: ThreadPool,
    pub color_model: ColorModel,
    pub first_frame: bool,
}

impl WorkspaceState {
    pub fn new(
        thread_count: usize,
        max_threads: usize,
        pool: ThreadPool,
        color_model: ColorModel,
    ) -> Self {
        Self {
            thread_count,
            max_threads,
            pool,
            color_model,
            first_frame: true,
        }
    }
}
