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
        let mut canvas = crate::canvas::Canvas::new(canvas_w, canvas_h, Color32::WHITE, TILE_SIZE);
        // New documents blend like Krita and Photoshop; the New Canvas
        // dialog starts from this.
        canvas.blend_space = crate::canvas::blend_modes::BlendSpace::Gamma;
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
            active_tool: crate::app::tools::Tool::Brush,
            selection_manager: crate::selection::SelectionManager::new(),
            tablet: crate::tablet::TabletInput::new(cc),
        };

        app.load_brush_tips(cc.egui_ctx.clone());
        app.load_user_presets();
        // The user's gradients sit next to the brushes folder.
        let gradients = app
            .brush_state
            .brushes_path
            .with_file_name("gradients.json");
        app.workspace.gradient.library =
            crate::app::tools::gradient::GradientLibrary::load(gradients);
        app.load_swatches();
        app.workspace.autosave = crate::app::autosave::AutosaveState::new(&app.autosave_path());
        app
    }

    pub(crate) fn create_default_brush_presets(black: Color32) -> Vec<BrushPreset> {
        use crate::brush_engine::brush_options::PixelBrushShape;
        use crate::brush_engine::dynamics::{Randomness, SpeedDynamics, Taper, TiltDynamics};
        use crate::brush_engine::texture::{self, BrushTexture, TextureMode};
        use crate::canvas::blend_modes::LayerBlend;
        let tip = |name: &str| {
            crate::brush_engine::tip::builtin()
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, t)| PixelBrushShape::Custom(t.clone()))
                .unwrap_or(PixelBrushShape::Circle)
        };
        let paper = |name: &str, mode: TextureMode, strength: f32| {
            texture::builtin()
                .iter()
                .find(|p| p.name == name)
                .map(|p| BrushTexture {
                    mode,
                    strength,
                    ..BrushTexture::new(p.clone())
                })
        };
        let preset = |name: &str, brush: Brush| BrushPreset {
            name: name.to_string(),
            brush,
            file: None,
        };
        vec![
            preset("Pencil (Sketch)", {
                // Graphite on paper: the grain shows under light pressure,
                // the tip widens as the pen leans.
                let mut b = Brush::new(6.0, 80.0, Color32::from_rgb(40, 40, 45), 8.0);
                b.brush_options.flow = 60.0;
                b.brush_options.opacity = 0.9;
                b.brush_options.pressure_min_size = 0.5;
                b.brush_options.pressure_opacity = true;
                b.jitter = 0.5;
                b.texture = paper("Fine grain", TextureMode::Subtract, 0.5);
                b.dynamics.taper = Taper {
                    start: 20.0,
                    end: 30.0,
                    ..Default::default()
                };
                b.dynamics.tilt = TiltDynamics {
                    size: 0.8,
                    opacity: -0.3,
                };
                b
            }),
            preset("Ink Pen", {
                // Thins at both ends and when drawn fast, like a dip pen.
                let mut b = Brush::new(8.0, 100.0, black, 5.0);
                b.brush_options.flow = 100.0;
                b.dynamics.taper = Taper {
                    start: 25.0,
                    end: 60.0,
                    min: 0.05,
                    ..Default::default()
                };
                b.dynamics.speed = SpeedDynamics {
                    size: -0.35,
                    opacity: 0.0,
                };
                b
            }),
            preset("Calligraphy", {
                // A flat nib held at 45°: thick and thin by direction.
                let mut b = Brush::new(16.0, 100.0, black, 4.0);
                b.brush_options.pressure_min_size = 0.5;
                b.dynamics.tip.angle = 45.0;
                b.dynamics.tip.ratio = 0.18;
                b.dynamics.taper = Taper {
                    start: 10.0,
                    end: 20.0,
                    min: 0.3,
                    ..Default::default()
                };
                b
            }),
            preset("Soft Airbrush", {
                let mut b = Brush::new(50.0, 0.0, black, 10.0);
                b.brush_options.flow = 8.0;
                b.brush_options.opacity = 0.6;
                b.brush_options.pressure_size = false;
                b.brush_options.pressure_flow = true;
                // Keeps spraying where the pen rests.
                b.airbrush_rate = 20.0;
                b
            }),
            preset("Hard Round", Brush::new(20.0, 100.0, black, 10.0)),
            preset("Multiply Marker", {
                // Layers darken like marker ink.
                let mut b = Brush::new(24.0, 90.0, Color32::from_rgb(120, 110, 150), 8.0);
                b.brush_options.flow = 70.0;
                b.brush_options.opacity = 0.7;
                b.brush_options.painting_mode =
                    crate::brush_engine::brush_options::PaintingMode::Wash;
                b.paint_blend = LayerBlend::Multiply;
                b
            }),
            preset("Glow", {
                // Adds light: bright over dark, for highlights and lights.
                let mut b = Brush::new(60.0, 0.0, Color32::from_rgb(255, 170, 60), 10.0);
                b.brush_options.flow = 15.0;
                b.brush_options.pressure_size = false;
                b.brush_options.pressure_flow = true;
                b.paint_blend = LayerBlend::LinearDodge;
                b
            }),
            preset("Eraser (Soft)", {
                let mut b = Brush::new(40.0, 20.0, black, 10.0);
                b.brush_options.blend_mode = BlendMode::Eraser;
                b.brush_options.opacity = 0.8;
                b
            }),
            preset("Eraser (Hard)", {
                let mut b = Brush::new(20.0, 100.0, black, 5.0);
                b.brush_options.blend_mode = BlendMode::Eraser;
                b
            }),
            preset("Chalk", {
                // A rough disc on rough paper: gaps where the paper is low.
                let mut b = Brush::new(30.0, 80.0, black, 8.0);
                b.brush_options.pixel_shape = tip("Rough disc");
                b.jitter = 3.0;
                b.brush_options.flow = 85.0;
                b.brush_options.pressure_opacity = true;
                b.brush_options.pressure_min_size = 0.6;
                b.dynamics.tip.random_angle = 180.0;
                b.texture = paper("Rough paper", TextureMode::Height, 0.85).map(|mut t| {
                    t.scale = 0.7;
                    t
                });
                b
            }),
            preset("Charcoal", {
                let mut b = Brush::new(22.0, 70.0, black, 12.0);
                b.brush_options.pixel_shape = tip("Rough disc");
                b.brush_options.flow = 55.0;
                b.brush_options.pressure_opacity = true;
                b.dynamics.tip.ratio = 0.5;
                b.dynamics.tip.follow_stroke = true;
                b.dynamics.random.opacity = 0.3;
                b.dynamics.tilt = TiltDynamics {
                    size: 1.0,
                    opacity: 0.0,
                };
                b.texture = paper("Charcoal", TextureMode::Height, 0.9);
                b
            }),
            preset("Dry Bristles", {
                // Streaks from the bristles dragged along the stroke.
                let mut b = Brush::new(36.0, 80.0, black, 3.0);
                b.brush_options.pixel_shape = tip("Bristles");
                b.brush_options.flow = 35.0;
                b.brush_options.pressure_min_size = 0.6;
                b.brush_options.pressure_opacity = true;
                b.dynamics.tip.follow_stroke = true;
                b.dynamics.speed = SpeedDynamics {
                    size: 0.0,
                    opacity: -0.4,
                };
                b
            }),
            preset("Spray", {
                let mut b = Brush::new(5.0, 40.0, black, 120.0);
                b.brush_options.pressure_size = false;
                b.brush_options.pressure_flow = true;
                b.jitter = 250.0;
                b.dynamics.random = Randomness {
                    size: 0.7,
                    opacity: 0.5,
                    count: 10,
                    ..Default::default()
                };
                b
            }),
            preset("Foliage", {
                let mut b = Brush::new(26.0, 90.0, Color32::from_rgb(60, 120, 50), 60.0);
                b.brush_options.pixel_shape = tip("Leaf");
                b.brush_options.pressure_size = false;
                b.jitter = 90.0;
                b.dynamics.tip.random_angle = 180.0;
                b.dynamics.random = Randomness {
                    size: 0.5,
                    count: 3,
                    hue: 12.0,
                    value: 0.15,
                    ..Default::default()
                };
                b
            }),
            preset("Mixed Leaves", {
                // Three leaf shapes, a different one at each dab.
                let mut b = Brush::new(30.0, 90.0, Color32::from_rgb(70, 125, 45), 70.0);
                if let Some((_, tips)) = crate::brush_engine::tip::builtin_sets()
                    .into_iter()
                    .find(|(n, _)| *n == "Mixed leaves")
                    && let Some((first, rest)) = tips.split_first()
                {
                    b.brush_options.pixel_shape = PixelBrushShape::Custom(first.clone());
                    b.brush_options.extra_tips = rest.to_vec();
                    b.brush_options.tip_order =
                        crate::brush_engine::brush_options::TipOrder::Random;
                }
                b.brush_options.pressure_size = false;
                b.jitter = 80.0;
                b.dynamics.tip.random_angle = 180.0;
                b.dynamics.random = Randomness {
                    size: 0.4,
                    count: 2,
                    hue: 15.0,
                    value: 0.12,
                    ..Default::default()
                };
                b
            }),
            preset("Dry Media", {
                // A soft round brush broken up by a spatter tip: the grain
                // of dry pastel or a worn-out brush.
                let mut b = Brush::new(36.0, 70.0, Color32::from_rgb(90, 60, 50), 10.0);
                b.brush_options.pressure_min_size = 0.4;
                b.dual = Some(crate::brush_engine::dual::DualTip {
                    shape: tip("Spatter"),
                    size: 0.5,
                    spacing: 30.0,
                    scatter: 60.0,
                    count: 2,
                    random_angle: true,
                    mode: crate::brush_engine::dual::DualMode::Multiply,
                    ..Default::default()
                });
                b
            }),
            preset("Watercolour", {
                // A wash on rough paper that dries with a darker rim.
                let mut b = Brush::new(40.0, 30.0, Color32::from_rgb(40, 90, 170), 8.0);
                b.brush_options.flow = 40.0;
                b.brush_options.opacity = 0.55;
                b.brush_options.painting_mode =
                    crate::brush_engine::brush_options::PaintingMode::Wash;
                b.brush_options.pressure_min_size = 0.5;
                b.texture = paper("Rough paper", TextureMode::Height, 0.5);
                b.wet_edge = 0.55;
                b.wet_edge_width = 7.0;
                b
            }),
            preset("Oil Bristle", {
                // A loaded flat brush: streaks that fan out with pressure.
                let mut b = Brush::new(40.0, 60.0, Color32::from_rgb(150, 70, 40), 10.0);
                b.brush_type = crate::brush_engine::brush::BrushType::Bristle;
                b.brush_options.pressure_min_size = 0.35;
                b.brush_options.flow = 70.0;
                b.bristles = crate::brush_engine::bristle::Bristles {
                    count: 30,
                    thickness: 2.5,
                    spread: 1.0,
                    ink: 0.0,
                    variation: 0.6,
                };
                b
            }),
            preset("Dry Brush", {
                // Few thin hairs that run dry along the stroke.
                let mut b = Brush::new(34.0, 80.0, Color32::from_rgb(45, 40, 38), 10.0);
                b.brush_type = crate::brush_engine::brush::BrushType::Bristle;
                b.brush_options.pressure_min_size = 0.5;
                b.brush_options.pressure_opacity = true;
                b.bristles = crate::brush_engine::bristle::Bristles {
                    count: 18,
                    thickness: 1.5,
                    spread: 1.0,
                    ink: 700.0,
                    variation: 0.8,
                };
                b.dynamics.taper.start = 20.0;
                b
            }),
            preset("Stitches", {
                // Running stitches along the stroke.
                let mut b = Brush::new(14.0, 90.0, Color32::from_rgb(60, 60, 70), 170.0);
                b.brush_options.pixel_shape = tip("Stitch");
                b.brush_options.pressure_size = false;
                b.dynamics.tip.follow_stroke = true;
                b
            }),
            preset("Chain", {
                // Links face on and side on, in turn, along the stroke.
                let mut b = Brush::new(22.0, 90.0, Color32::from_rgb(120, 120, 130), 62.0);
                if let Some((_, tips)) = crate::brush_engine::tip::builtin_sets()
                    .into_iter()
                    .find(|(n, _)| *n == "Chain")
                    && let Some((first, rest)) = tips.split_first()
                {
                    b.brush_options.pixel_shape = PixelBrushShape::Custom(first.clone());
                    b.brush_options.extra_tips = rest.to_vec();
                }
                b.brush_options.pressure_size = false;
                b.dynamics.tip.follow_stroke = true;
                b
            }),
            preset("Lace Ribbon", {
                let mut b = Brush::new(30.0, 90.0, Color32::from_rgb(245, 245, 240), 10.0);
                b.brush_options.pixel_shape = tip("Lace");
                b.brush_options.placement = crate::brush_engine::brush_options::Placement::Ribbon;
                b.brush_options.pressure_size = false;
                b
            }),
            preset("Striped Ribbon", {
                let mut b = Brush::new(24.0, 90.0, black, 10.0);
                b.brush_options.pixel_shape = tip("Striped ribbon");
                b.brush_options.placement = crate::brush_engine::brush_options::Placement::Ribbon;
                b.brush_options.tip_colors = true;
                b.brush_options.pressure_min_size = 0.6;
                b
            }),
            preset("Flowers", {
                // Scattered flowers in their own colours.
                let mut b = Brush::new(26.0, 90.0, black, 120.0);
                b.brush_options.pixel_shape = tip("Flower");
                b.brush_options.tip_colors = true;
                b.brush_options.pressure_size = false;
                b.jitter = 70.0;
                b.dynamics.tip.random_angle = 180.0;
                b.dynamics.random.size = 0.4;
                b
            }),
            preset("Sketchy Pencil", {
                // A fine line that webs itself into shading.
                let mut b = Brush::new(2.0, 80.0, Color32::from_rgb(45, 45, 50), 20.0);
                b.brush_type = crate::brush_engine::brush::BrushType::Sketch;
                b.brush_options.pressure_min_size = 0.5;
                b.brush_options.opacity = 0.8;
                b.sketch = crate::brush_engine::sketch::Sketch {
                    reach: 45.0,
                    density: 0.12,
                    opacity: 0.3,
                    thickness: 1.0,
                };
                b
            }),
            preset("Cross Hatch", {
                // Ink hatching; press harder to cross it.
                let mut b = Brush::new(40.0, 90.0, Color32::from_rgb(25, 25, 30), 10.0);
                b.brush_type = crate::brush_engine::brush::BrushType::Hatching;
                b.brush_options.pressure_size = false;
                b.hatching = crate::brush_engine::hatching::Hatching {
                    angle: 45.0,
                    separation: 5.0,
                    thickness: 1.2,
                    crosshatch: true,
                };
                b
            }),
            preset("Spatter", {
                let mut b = Brush::new(40.0, 90.0, black, 90.0);
                b.brush_options.pixel_shape = tip("Spatter");
                b.brush_options.pressure_size = false;
                b.jitter = 60.0;
                b.dynamics.tip.random_angle = 180.0;
                b.dynamics.random.size = 0.6;
                b
            }),
            preset("Pixel Art", Brush::new_pixel(1.0, black)),
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
