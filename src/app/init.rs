//! Building the app at startup: default brushes and presets, workspace
//! settings, the first canvas and the tablet input.

use crate::app::{
    PainterApp,
    document::{ColorModel, NewCanvasSettings, TILE_SIZE},
    state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
    view::gpu_canvas::GpuCanvas,
};
use crate::brush_engine::brush::{Brush, BrushPreset};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;
use std::{path::PathBuf, thread};

/// Where the app keeps what it saves: `$RUSTY_PAINTER_DATA` if set, else
/// the user's data folder (`~/.local/share`, `%APPDATA%`, `~/Library/
/// Application Support`, the app's own storage on Android), else the
/// folder it was started in.
fn data_dir() -> PathBuf {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = || var("HOME");
    let base = if let Some(dir) = var("RUSTY_PAINTER_DATA") {
        return dir;
    } else if cfg!(target_os = "android") {
        return crate::ANDROID_DATA.get().cloned().unwrap_or_default();
    } else if cfg!(windows) {
        var("APPDATA")
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        var("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local/share")))
    };
    base.map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        |b| b.join("rusty-painter"),
    )
}

impl PainterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let canvas_w = 4000;
        let canvas_h = 4000;
        let mut canvas = crate::canvas::Canvas::new(canvas_w, canvas_h, Color32::WHITE, TILE_SIZE);
        // New documents blend like Krita and Photoshop; the New Canvas
        // dialog starts from this.
        canvas.blend_space = crate::canvas::blend_modes::BlendSpace::Gamma;
        let layer_count = canvas.layers.len();
        let new_canvas = NewCanvasSettings::from_canvas(&canvas);
        let color_model = new_canvas.color_model;
        let black = Color32::from_rgba_unmultiplied(0, 0, 0, 255);

        let workspace = Self::create_workspace(color_model);
        let render_cache = RenderCache::new(canvas_w, canvas_h);
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            let gpu = GpuCanvas::new(&render_state.device, render_state.target_format);
            render_state.renderer.write().callback_resources.insert(gpu);
        } else {
            log::error!("No wgpu render state: the canvas cannot be displayed");
        }

        let brush_state = BrushState::new(
            Brush::new(24.0, 20.0, black, 25.0),
            Vec::new(),
            Self::get_brushes_path(),
            true,
        );
        let viewport = ViewportState::new(1.0, Vec2 { x: 300.0, y: 100.0 });
        let layer_state = LayerState::new(layer_count);
        let modal_state = ModalState::new(new_canvas);
        let export_state = ExportState::new();

        let mut app = Self {
            canvas: std::sync::Arc::new(canvas),
            stroke_worker: Default::default(),
            brush_state,
            viewport,
            render_cache,
            layer_state,
            modal_state,
            export_state,
            workspace,
            active_tool: crate::app::tools::Tool::Brush,
            selection_manager: crate::selection::SelectionManager::new(),
            tablet: crate::tablet::TabletInput::new(cc),
        };

        app.load_brush_tips(cc.egui_ctx.clone());
        app.load_user_presets();
        app.load_brush_library();
        app.install_default_presets();
        app.brush_state.pick_eraser();
        // The user's gradients sit next to the brushes folder.
        let gradients = app
            .brush_state
            .brushes_path
            .with_file_name("gradients.json");
        app.workspace.gradient.library =
            crate::app::tools::gradient::GradientLibrary::load(gradients);
        app.load_swatches();
        app.load_view_settings();
        app.load_panel_widths(&cc.egui_ctx);
        app.load_settings();
        app.workspace.autosave = crate::app::autosave::AutosaveState::new(&app.autosave_path());
        app
    }

    /// The presets that come with the app, from `assets/default-brushes.rpbrush`
    /// (built into the program, and copied into the user's library on first
    /// start: see [`Self::install_default_presets`]).
    pub(crate) fn default_brush_presets() -> Vec<BrushPreset> {
        static FILE: &[u8] = include_bytes!("../../assets/default-brushes.rpbrush");
        crate::brush_engine::preset_file::decode(FILE).expect("the default brushes load")
    }

    fn create_workspace(color_model: ColorModel) -> WorkspaceState {
        let max_threads = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .max(1);
        let pool = ThreadPoolBuilder::new()
            .num_threads(max_threads)
            .build()
            .expect("failed to build thread pool");
        WorkspaceState::new(
            max_threads,
            max_threads,
            std::sync::Arc::new(pool),
            color_model,
        )
    }

    /// `brushes/` in the app's data folder, which also holds the settings,
    /// swatches, gradients and autosave (siblings of `brushes/`).
    fn get_brushes_path() -> PathBuf {
        let data = data_dir();
        let _ = std::fs::create_dir_all(&data);
        data.join("brushes")
    }
}
