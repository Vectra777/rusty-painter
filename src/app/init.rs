use super::{
    PainterApp,
    gpu_canvas::GpuCanvas,
    layout,
    painter_state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
    state::{ColorModel, NewCanvasSettings, TILE_SIZE},
};
use crate::brush_engine::{
    brush::{Brush, BrushPreset},
    brush_options::BlendMode,
};
use eframe::egui::{Color32, Vec2};
use rayon::ThreadPoolBuilder;
use std::{path::PathBuf, thread};

impl PainterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let canvas_w = 4000;
        let canvas_h = 4000;
        let canvas = crate::canvas::Canvas::new(canvas_w, canvas_h, Color32::WHITE, TILE_SIZE);
        let layer_count = canvas.layers.len();
        let new_canvas = NewCanvasSettings::from_canvas(&canvas);
        let color_model = new_canvas.color_model;
        let black = Color32::from_rgba_unmultiplied(0, 0, 0, 255);

        let presets = Self::create_default_brush_presets(black);
        let workspace = Self::create_workspace(color_model);
        let render_cache = RenderCache::new(canvas_w, canvas_h);
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            let gpu = GpuCanvas::new(&render_state.device, render_state.target_format);
            render_state.renderer.write().callback_resources.insert(gpu);
        } else {
            log::error!("No wgpu render state: the canvas cannot be displayed");
        }
        let dock_left = layout::default_left_dock();
        let dock_right = layout::default_right_dock();

        let brush_state = BrushState::new(
            Brush::new(24.0, 20.0, black, 25.0),
            presets,
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
            active_tool: super::tools::Tool::Brush,
            selection_manager: crate::selection::SelectionManager::new(),
            dock_left,
            dock_right,
            tablet: crate::tablet::TabletInput::new(cc),
        };

        app.load_brush_tips(cc.egui_ctx.clone());
        app
    }

    fn create_default_brush_presets(black: Color32) -> Vec<BrushPreset> {
        vec![
            BrushPreset {
                name: "Pencil (Sketch)".to_string(),
                brush: {
                    let mut b = Brush::new(6.0, 60.0, black, 10.0);
                    b.brush_options.flow = 30.0;
                    b.brush_options.opacity = 0.8;
                    b.brush_options.pressure_min_size = 0.4;
                    b.brush_options.pressure_opacity = true;
                    b.jitter = 0.5;
                    b
                },
            },
            BrushPreset {
                name: "Ink Pen".to_string(),
                brush: {
                    let mut b = Brush::new(8.0, 100.0, black, 5.0);
                    b.stabilizer = 0.2;
                    b.brush_options.flow = 100.0;
                    b
                },
            },
            BrushPreset {
                name: "Soft Airbrush".to_string(),
                brush: {
                    let mut b = Brush::new(50.0, 0.0, black, 10.0);
                    b.brush_options.flow = 8.0;
                    b.brush_options.opacity = 0.6;
                    b.brush_options.pressure_size = false;
                    b.brush_options.pressure_flow = true;
                    b
                },
            },
            BrushPreset {
                name: "Hard Round".to_string(),
                brush: Brush::new(20.0, 100.0, black, 10.0),
            },
            BrushPreset {
                name: "Eraser (Soft)".to_string(),
                brush: {
                    let mut b = Brush::new(40.0, 20.0, black, 10.0);
                    b.brush_options.blend_mode = BlendMode::Eraser;
                    b.brush_options.opacity = 0.8;
                    b
                },
            },
            BrushPreset {
                name: "Eraser (Hard)".to_string(),
                brush: {
                    let mut b = Brush::new(20.0, 100.0, black, 5.0);
                    b.brush_options.blend_mode = BlendMode::Eraser;
                    b
                },
            },
            BrushPreset {
                name: "Chalk".to_string(),
                brush: {
                    let mut b = Brush::new(30.0, 80.0, black, 40.0);
                    b.jitter = 5.0;
                    b.brush_options.flow = 50.0;
                    b.brush_options.pressure_opacity = true;
                    b
                },
            },
            BrushPreset {
                name: "Pixel Art".to_string(),
                brush: Brush::new_pixel(1.0, black),
            },
        ]
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

    fn get_brushes_path() -> PathBuf {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("brushes")
    }
}
