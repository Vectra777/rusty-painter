//! The app's state, grouped by concern: brush, viewport, render cache,
//! layers (undo history, floating layer), modals, export and workspace
//! (tool settings and panel layout).

use crate::app::document::{CanvasTile, ColorModel, NewCanvasSettings, TILE_SIZE};
use crate::app::view::gpu_canvas::TILES_PER_ATLAS;
use crate::canvas::storage::LayerId;
use crate::{
    brush_engine::{
        brush::{Brush, BrushPreset},
        brush_options::{BlendMode, PixelBrushShape},
    },
    canvas::history::History,
    ui::{brush_settings::BrushPreviewState, export_modal::ExportProgress},
};
use eframe::egui::{self, Color32, Rgba, Vec2};
use rayon::ThreadPool;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

/// Brush-related state and resources
pub struct BrushState {
    pub brush: Brush,
    pub brush_preview: BrushPreviewState,
    pub presets: Vec<BrushPreset>,
    pub preset_previews: HashMap<String, egui::TextureHandle>,
    pub loaded_brush_tips: Vec<(String, PixelBrushShape, Option<egui::TextureHandle>)>,
    /// Paper textures from `brushes/textures/` (besides the built-in ones).
    pub loaded_textures: Vec<Arc<crate::brush_engine::texture::Pattern>>,
    pub brushes_path: PathBuf,
    pub is_drawing: bool,
    pub use_masked_brush: bool,
    pub show_new_preset_modal: bool,
    pub new_preset_name: String,
    /// Background color; `X` swaps it with the brush color.
    pub secondary_color: Color32,
    /// Most recently painted colors, newest first.
    pub recent_colors: Vec<Color32>,
    /// Colours kept on purpose (the palette), in order.
    pub swatches: Vec<Color32>,
    /// The swatches changed: saved at the end of the frame.
    pub swatches_dirty: bool,
    /// Brush and eraser keep separate settings: `brush` is the active tool's,
    /// this is the other one's, swapped in when the tool changes.
    pub stashed_brush: Brush,
    pub eraser_active: bool,
    /// Name of the preset the active tool's brush came from, and the other tool's.
    pub active_preset: Option<String>,
    pub stashed_preset: Option<String>,
    /// The floating presets window is open.
    pub show_presets: bool,
    /// The smudge / blur stroke in progress.
    pub blend_stroke: Option<crate::app::tools::blend::BlendStroke>,
}

/// How many colors the recent-colors strip remembers.
pub const MAX_RECENT_COLORS: usize = 18;

impl BrushState {
    pub fn new(
        brush: Brush,
        presets: Vec<BrushPreset>,
        brushes_path: PathBuf,
        use_masked_brush: bool,
    ) -> Self {
        // The eraser starts from the first eraser preset, else from the brush.
        let eraser = presets
            .iter()
            .find(|p| p.brush.brush_options.blend_mode == BlendMode::Eraser);
        let eraser_preset = eraser.map(|p| p.name.clone());
        let mut eraser_brush = eraser.map_or_else(|| brush.clone(), |p| p.brush.clone());
        eraser_brush.brush_options.blend_mode = BlendMode::Eraser;
        eraser_brush.brush_options.color = brush.brush_options.color;
        Self {
            brush,
            brush_preview: BrushPreviewState::default(),
            presets,
            preset_previews: HashMap::new(),
            loaded_brush_tips: Vec::new(),
            loaded_textures: Vec::new(),
            brushes_path,
            is_drawing: false,
            use_masked_brush,
            show_new_preset_modal: false,
            new_preset_name: String::new(),
            secondary_color: Color32::WHITE,
            recent_colors: Vec::new(),
            swatches: vec![Color32::BLACK, Color32::WHITE],
            swatches_dirty: false,
            stashed_brush: eraser_brush,
            eraser_active: false,
            active_preset: None,
            stashed_preset: eraser_preset,
            show_presets: false,
            blend_stroke: None,
        }
    }

    /// Remember `color` at the front of the recent-colors strip.
    pub fn remember_color(&mut self, color: Color32) {
        self.recent_colors.retain(|&c| c != color);
        self.recent_colors.insert(0, color);
        self.recent_colors.truncate(MAX_RECENT_COLORS);
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
    /// Canvas-space position under the pointer, for the status bar.
    pub cursor_canvas: Option<Vec2>,
    /// Screen rect of the canvas area last frame, for zooming from the UI.
    pub canvas_area: Option<egui::Rect>,
    /// Finger tracking for touch gestures.
    pub touch: crate::app::input::touch::TouchState,
    /// Screen position of the previous pointer event, for pan/rotate deltas.
    pub last_pointer_pos: Option<egui::Pos2>,
    /// The view is mirrored left to right (the pixels are not).
    pub flip_x: bool,
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
            cursor_canvas: None,
            canvas_area: None,
            touch: Default::default(),
            last_pointer_pos: None,
            flip_x: false,
        }
    }
}

pub struct BelowCache {
    /// `Canvas::composite_below_key` for the layers this was built from.
    pub key: (Color32, Vec<(LayerId, bool, u32)>),
    pub tiles: FxHashMap<(usize, usize), Vec<Rgba>>,
}

/// Display tiles and their GPU atlas grid.
pub struct RenderCache {
    pub tiles: Vec<CanvasTile>,
    pub tiles_x: usize,
    pub tiles_y: usize,
    /// Atlas grid size; each atlas holds a `TILES_PER_ATLAS`² block of tiles.
    pub atlases_x: usize,
    pub atlases_y: usize,
    /// Composite of the layers below the active one, per tile, valid for the
    /// current stroke only (nothing but the active layer changes mid-stroke).
    pub below_cache: Option<BelowCache>,
    /// Bumped whenever the canvas is rebuilt, so the GPU recreates its atlases.
    pub texture_generation: u64,
    /// Tiles whose atlas content is only exact from this mip level up (a
    /// zoomed-out stroke preview); finer levels still need a full upload.
    pub preview_tiles: FxHashMap<(usize, usize), u32>,
}

impl RenderCache {
    /// A render cache for a `width`×`height` canvas, every tile dirty.
    pub fn new(width: usize, height: usize) -> Self {
        let tiles_x = width.div_ceil(TILE_SIZE);
        let tiles_y = height.div_ceil(TILE_SIZE);
        let tiles = (0..tiles_y)
            .flat_map(|ty| {
                (0..tiles_x).map(move |tx| CanvasTile {
                    dirty: true,
                    tx,
                    ty,
                    damage: None,
                })
            })
            .collect();
        Self {
            tiles,
            tiles_x,
            tiles_y,
            atlases_x: tiles_x.div_ceil(TILES_PER_ATLAS),
            atlases_y: tiles_y.div_ceil(TILES_PER_ATLAS),
            below_cache: None,
            texture_generation: 0,
            preview_tiles: FxHashMap::default(),
        }
    }

    /// The atlas holding tile `(tx, ty)` and the tile's pixel offset in it.
    pub fn atlas_slot(&self, tx: usize, ty: usize) -> (usize, usize, usize) {
        let atlas = (ty / TILES_PER_ATLAS) * self.atlases_x + tx / TILES_PER_ATLAS;
        let x = (tx % TILES_PER_ATLAS) * TILE_SIZE;
        let y = (ty % TILES_PER_ATLAS) * TILE_SIZE;
        (atlas, x, y)
    }
}

/// A transform session's starting point: the source layer and its tiles
/// (the ones the float lifted pixels from) as they were, plus the selection.
pub struct FloatSession {
    pub source_id: crate::canvas::storage::LayerId,
    pub source_tiles: HashMap<(i32, i32), Vec<Color32>>,
    pub selection: Option<crate::selection::SelectionShape>,
    /// Canvas area the floating layer last covered.
    pub last_rect: Option<egui::Rect>,
    /// Content bounds of the floating pixels before transforming.
    pub src_bounds: Option<egui::Rect>,
    /// The floating layer shows a quick (draft) preview right now.
    pub draft_shown: bool,
}

/// See [`LayerState::float_overlay`].
pub struct FloatOverlay {
    pub texture: egui::TextureHandle,
    /// Canvas area (pixel edges) the texture shows, before transforming.
    pub area: egui::Rect,
    /// Drawn this frame (while dragging, and until the re-rendered layer
    /// has fully reached the screen after release).
    pub showing: bool,
    /// Released: the layer is rendered and shown again; hide the overlay
    /// once every redrawn tile is on screen.
    pub revealing: bool,
}

/// Layer UI state and undo history
pub struct LayerState {
    pub layer_ui_colors: Vec<Color32>,
    pub layer_dragging: Option<usize>,
    pub floating_layer_idx: Option<usize>,
    pub floating_buffer: Option<HashMap<(i32, i32), Vec<Color32>>>,
    /// What the running transform session needs to undo or cancel itself.
    pub float_session: Option<FloatSession>,
    /// Transforming the selection outline alone (it had no pixels to
    /// lift): the selection as it was, applied on commit.
    pub selection_transform: Option<crate::selection::SelectionShape>,
    /// The running session's transform, kept for when the Transform tool is
    /// left (another tool picked): the session is applied as it was.
    pub transform_info: Option<crate::selection::transform::TransformInfo>,
    /// The floating pixels as a GPU texture, drawn transformed while the
    /// box is dragged (instead of re-rendering the layer every frame).
    pub float_overlay: Option<FloatOverlay>,
    /// The transform changed; the floating layer is redrawn once per frame.
    pub transform_preview_pending: bool,
    /// The running liquify session, if any.
    pub liquify: Option<crate::app::tools::liquify::LiquifySession>,
    /// The document's undo history: one stack for every layer, in the
    /// order things were done, so undo always takes back the last change
    /// wherever it was.
    pub history: History,
    /// Per-layer thumbnail textures for the layers panel, by layer index.
    pub thumbnails: Vec<Option<egui::TextureHandle>>,
    /// Set when canvas content may have changed since thumbnails were built.
    pub thumbnails_dirty: bool,
    pub thumbnails_built_at: Option<std::time::Instant>,
}

impl LayerState {
    pub fn new(layer_count: usize) -> Self {
        Self {
            layer_ui_colors: vec![Color32::from_gray(40); layer_count],
            layer_dragging: None,
            floating_layer_idx: None,
            floating_buffer: None,
            float_session: None,
            selection_transform: None,
            transform_info: None,
            float_overlay: None,
            transform_preview_pending: false,
            liquify: None,
            history: History::new(),
            thumbnails: Vec::new(),
            thumbnails_dirty: true,
            thumbnails_built_at: None,
        }
    }
}

/// Modal dialog state
pub struct ModalState {
    pub show_new_canvas_modal: bool,
    pub new_canvas: NewCanvasSettings,
    pub show_general_settings: bool,
    pub show_shortcuts: bool,
    /// The selection tool's slide-out menu is open.
    pub select_menu_open: bool,
    /// The mirror painting menu is open.
    pub symmetry_menu_open: bool,
    /// The shape menu is open.
    pub shape_menu_open: bool,
    /// Touch mode: the File / Edit / View / Help sheet is slid up.
    pub menu_sheet_open: bool,
    /// Section the menu sheet shows when it is too narrow for all of them.
    pub menu_sheet_section: crate::ui::menus::MenuSection,
}

impl ModalState {
    pub fn new(new_canvas: NewCanvasSettings) -> Self {
        Self {
            show_new_canvas_modal: false,
            new_canvas,
            show_general_settings: false,
            show_shortcuts: false,
            select_menu_open: false,
            symmetry_menu_open: false,
            shape_menu_open: false,
            menu_sheet_open: false,
            menu_sheet_section: Default::default(),
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
    pub pool: Arc<ThreadPool>,
    pub color_model: ColorModel,
    /// Fit the canvas to the panel whenever the panel size changes, until the
    /// user pans, zooms or rotates the view.
    pub auto_fit: bool,
    /// Panel size the canvas was last auto-fitted to.
    pub fitted_to: Option<egui::Vec2>,
    /// Larger controls and gesture hints for touch screens.
    pub touch_mode: bool,
    /// The touch mode the egui style was last built for.
    pub applied_touch_mode: Option<bool>,
    /// Let a single finger paint; when off, one finger pans and only a
    /// stylus paints.
    pub finger_painting: bool,
    /// How the pen's raw pressure maps to the pressure every brush sees (a
    /// soft or firm pen, a heavy or light hand). What pressure drives (size,
    /// opacity, flow) is set per brush, with its own curves on top.
    pub pressure_curve: crate::brush_engine::hardness::SoftnessCurve,
    /// The pen's last raw pressure and what the curve made of it, for the
    /// live readout in Settings.
    pub last_pressure: Option<(f32, f32)>,
    pub show_left_panel: bool,
    pub show_right_panel: bool,
    /// Panel visibility last frame, to tell which one was just opened.
    pub panels_last_frame: (bool, bool),
    /// Window size last frame, to notice resizes and screen rotations.
    pub screen_size: Option<egui::Vec2>,
    /// Selection type the Select tool uses (last one picked).
    pub select_type: crate::selection::SelectionType,
    /// Fill tool mode and settings.
    pub fill: crate::app::tools::fill::FillToolState,
    /// Liquify brush mode and settings.
    pub liquify: crate::app::tools::liquify::LiquifySettings,
    /// Palette window state.
    pub palette: crate::app::tools::palette::PaletteToolState,
    /// Smudge and blur settings (the rest comes from the brush).
    pub blend: crate::app::tools::blend::BlendToolSettings,
    /// Transform tool: clicking another layer's pixels selects that layer.
    pub transform_pick_layer: bool,
    /// Select tool settings and drag state.
    pub select: crate::app::tools::select::SelectToolState,
    /// Mirror painting.
    pub symmetry: crate::brush_engine::symmetry::Symmetry,
    /// On-canvas guides and their handles.
    pub guides: crate::app::tools::guides::GuideState,
    /// Shape tool settings and the shape being edited.
    pub shapes: crate::app::tools::shape::ShapeToolState,
    /// Gradient tool settings and the gradient being placed.
    pub gradient: crate::app::tools::gradient::GradientToolState,
    /// Content-aware fill (smart patch).
    pub patch: crate::app::tools::patch::PatchState,
    /// Android's image picker (the photo library).
    pub gallery: crate::ui::image_gallery::GalleryState,
    /// Textures dropped this frame, kept until the next one. egui-wgpu frees
    /// a texture before submitting the frame's uploads, so one updated and
    /// freed in the same frame fails the submit.
    pub retired_textures: Vec<egui::TextureHandle>,
    /// Frame timing for the readout (View → Frame Times).
    pub frame_stats: crate::app::frame_stats::FrameStats,
    /// Copied pixels (Ctrl+C / Ctrl+V).
    pub clipboard: crate::app::clipboard::ClipboardState,
    /// The display's refresh rate, measured from frame pacing.
    pub refresh: crate::app::frame_stats::RefreshRate,
    /// Keyboard layout, for where shortcuts are and how they're labelled.
    pub keyboard: crate::app::input::keyboard::KeyboardState,
}

impl WorkspaceState {
    pub fn new(
        thread_count: usize,
        max_threads: usize,
        pool: Arc<ThreadPool>,
        color_model: ColorModel,
    ) -> Self {
        Self {
            thread_count,
            max_threads,
            pool,
            color_model,
            auto_fit: true,
            fitted_to: None,
            // Android, or a desktop touchscreen via RUSTY_PAINTER_TOUCH=1.
            touch_mode: cfg!(target_os = "android")
                || std::env::var_os("RUSTY_PAINTER_TOUCH").is_some(),
            applied_touch_mode: None,
            finger_painting: true,
            pressure_curve: crate::brush_engine::hardness::SoftnessCurve {
                points: vec![
                    crate::brush_engine::hardness::CurvePoint::new(0.0, 0.0),
                    crate::brush_engine::hardness::CurvePoint::new(1.0, 1.0),
                ],
            },
            last_pressure: None,
            show_left_panel: true,
            show_right_panel: true,
            panels_last_frame: (true, true),
            screen_size: None,
            select_type: crate::selection::SelectionType::Rectangle,
            fill: Default::default(),
            liquify: Default::default(),
            palette: Default::default(),
            blend: Default::default(),
            gallery: Default::default(),
            transform_pick_layer: true,
            select: Default::default(),
            symmetry: Default::default(),
            guides: Default::default(),
            shapes: Default::default(),
            gradient: Default::default(),
            patch: Default::default(),
            retired_textures: Vec::new(),
            frame_stats: Default::default(),
            clipboard: Default::default(),
            refresh: Default::default(),
            keyboard: crate::app::input::keyboard::KeyboardState::new(),
        }
    }
}
