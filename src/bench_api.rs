//! Entry points for the benchmarks (`benches/app_bench.rs`), behind the
//! `bench` feature: the app's tools, run headless on a real `PainterApp`
//! (stroke worker, undo, the same code paths as the UI). Not a stable API.

use crate::PainterApp;
use crate::app::document::{ColorModel, NewCanvasSettings, TILE_SIZE};
use crate::app::state::{
    BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
};
use crate::app::tools::Tool;
use crate::brush_engine::brush::Brush;
use crate::canvas::Canvas;
use crate::canvas::history::History;
use crate::selection::{SelectionMode, SelectionShape, SelectionType};
use eframe::egui::{Color32, Vec2};
use std::sync::Arc;

pub use crate::app::tools::gradient::GradientColors;
pub use crate::app::tools::shape::{ShapeKind, ShapeMods, ShapeStyle};
pub use crate::brush_engine::symmetry::SymmetryMode;
pub use crate::canvas::gradient::GradientShape;
pub use crate::canvas::liquify::LiquifyMode;

/// A `size`×`size` app whose layer 1 is painted (a smooth colour field with
/// a grid of black lines every 250 px) and active; brushes run on a pool
/// of all cores.
pub fn painted_app(size: usize) -> PainterApp {
    let canvas = Canvas::new(size, size, Color32::WHITE, TILE_SIZE);
    let tiles = size.div_ceil(TILE_SIZE);
    for ty in 0..tiles {
        for tx in 0..tiles {
            let tile: Vec<Color32> = (0..TILE_SIZE * TILE_SIZE)
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
            canvas.set_layer_tile_data(1, tx as i32, ty as i32, tile);
        }
    }
    let mut app = headless(canvas);
    app.canvas_mut().active_layer_idx = 1;
    app
}

fn headless(canvas: Canvas) -> PainterApp {
    let layers = canvas.layers.len();
    let (w, h) = (canvas.width(), canvas.height());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap(),
    );
    let mut app = PainterApp {
        canvas: Arc::new(canvas),
        stroke_worker: Default::default(),
        brush_state: BrushState::new(
            Brush::new(40.0, 50.0, Color32::from_rgb(30, 60, 200), 15.0),
            Vec::new(),
            ".".into(),
            true,
        ),
        viewport: ViewportState::new(1.0, Vec2::ZERO),
        render_cache: RenderCache::new(1, 1),
        layer_state: {
            let mut state = LayerState::new(layers);
            state.histories = (0..layers).map(|_| History::new()).collect();
            state
        },
        modal_state: ModalState::new(NewCanvasSettings::from_canvas(&Canvas::new(
            1,
            1,
            Color32::WHITE,
            TILE_SIZE,
        ))),
        export_state: ExportState::new(),
        workspace: WorkspaceState::new(threads, threads, pool, ColorModel::Rgba),
        active_tool: Tool::Brush,
        selection_manager: crate::selection::SelectionManager::new(),
        dock_left: egui_dock::DockState::new(Vec::new()),
        dock_right: egui_dock::DockState::new(Vec::new()),
        tablet: None,
    };
    app.selection_manager.canvas_size = [w, h];
    app
}

/// A wavy path of `n` samples across the middle of a `size` canvas, with
/// rising pressure.
pub fn wavy_path(size: f32, n: usize) -> Vec<(Vec2, f32)> {
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1).max(1) as f32;
            let p = Vec2::new(
                size * (0.2 + 0.6 * t),
                size * (0.5 + 0.15 * (t * 9.0).sin()),
            );
            (p, 0.3 + 0.7 * t)
        })
        .collect()
}

pub fn set_brush(app: &mut PainterApp, diameter: f32) {
    app.brush_state.brush.brush_options.diameter = diameter;
    app.brush_state.brush.is_changed = true;
}

/// A brush (or eraser) stroke through the stroke worker, waited for.
pub fn stroke(app: &mut PainterApp, path: &[(Vec2, f32)], eraser: bool) {
    app.set_brush_tool(eraser);
    let Some(&(first, pressure)) = path.first() else {
        return;
    };
    app.start_stroke_with_pressure(first, pressure);
    for &(p, pressure) in &path[1..] {
        app.add_stroke_point(p, pressure);
    }
    app.release_canvas();
}

/// A smudge (or blur) stroke.
pub fn blend_stroke(app: &mut PainterApp, path: &[(Vec2, f32)], smudge: bool) {
    app.set_blend_tool(smudge);
    let Some(&(first, pressure)) = path.first() else {
        return;
    };
    app.blend_press(first, pressure);
    for &(p, pressure) in &path[1..] {
        app.blend_drag(p, pressure);
    }
    app.blend_release();
}

pub fn set_symmetry(app: &mut PainterApp, mode: SymmetryMode, count: u32, mirrored: bool) {
    app.workspace.symmetry.mode = mode;
    app.workspace.symmetry.count = count;
    app.workspace.symmetry.mirrored = mirrored;
    app.centre_symmetry();
}

/// A selection drag of `kind` through `points` (one undo step).
pub fn select_drag(app: &mut PainterApp, kind: SelectionType, points: &[Vec2]) {
    app.active_tool = Tool::Select(kind);
    let Some(&first) = points.first() else {
        return;
    };
    app.select_press(first, kind, SelectionMode::Replace);
    for &p in &points[1..] {
        app.select_move(p);
    }
    app.select_release();
}

/// A wand or colour-range click.
pub fn select_click(app: &mut PainterApp, kind: SelectionType, pos: Vec2) {
    app.select_press(pos, kind, SelectionMode::Replace);
}

pub fn select_all(app: &mut PainterApp) {
    app.select_all();
}

pub fn invert_selection(app: &mut PainterApp) {
    app.invert_selection();
}

pub fn deselect(app: &mut PainterApp) {
    app.selection_manager.clear_selection();
}

/// A rectangle selection, without recording it.
pub fn select_rect(app: &mut PainterApp, min: Vec2, max: Vec2) {
    app.selection_manager.current_shape = Some(SelectionShape::Rectangle {
        start: min,
        end: max,
    });
}

pub fn bucket_fill(app: &mut PainterApp, pos: Vec2, tolerance: u8) {
    app.workspace.fill.mode = crate::app::tools::fill::FillMode::Bucket;
    app.workspace.fill.settings.tolerance = tolerance;
    app.fill_press(pos);
}

pub fn enclose_fill(app: &mut PainterApp, lasso: &[Vec2]) {
    app.workspace.fill.mode = crate::app::tools::fill::FillMode::Enclose;
    app.workspace.fill.path = lasso.to_vec();
    app.fill_release();
}

pub fn eyedropper(app: &mut PainterApp, pos: Vec2) {
    app.pick_color(pos);
}

/// Float the layer (or selection), preview a rotation and apply it.
pub fn transform_rotate(app: &mut PainterApp, rotation: f32) {
    use crate::app::tools::transform;
    app.active_tool = Tool::Transform(Default::default());
    transform::transform_press(app, Vec2::new(10.0, 10.0));
    transform::transform_release(app);
    if let Tool::Transform(ref mut info) = app.active_tool {
        info.rotation = rotation;
    }
    app.layer_state.transform_preview_pending = true;
    transform::flush_transform_preview(app);
    transform::commit_floating_layer(app);
}

/// A liquify drag of `mode` along `path`, applied.
pub fn liquify(app: &mut PainterApp, mode: LiquifyMode, radius: f32, path: &[Vec2]) {
    app.active_tool = Tool::Liquify;
    app.workspace.liquify.mode = mode;
    app.workspace.liquify.radius = radius;
    let Some(&first) = path.first() else {
        return;
    };
    app.liquify_press(first);
    for &p in &path[1..] {
        app.liquify_drag(p);
    }
    app.liquify_release();
    app.liquify_commit();
}

pub fn extract_palette(app: &mut PainterApp, count: usize) -> Vec<Color32> {
    app.workspace.palette.count = count;
    app.extract_palette();
    app.workspace.palette.extracted.clone()
}

/// Recolour the layer to the palette last extracted.
pub fn recolor_extracted(app: &mut PainterApp, dither: bool) {
    app.workspace.palette.dither = dither;
    let palette = app.workspace.palette.extracted.clone();
    app.recolor_layer(&palette);
}

pub fn set_selection_brush_radius(app: &mut PainterApp, radius: f32) {
    app.selection_manager.brush_radius = radius;
}

/// Draw and apply a shape.
pub fn shape(app: &mut PainterApp, kind: ShapeKind, style: ShapeStyle, a: Vec2, b: Vec2) {
    app.workspace.shapes.settings.style = style;
    app.set_shape_tool(kind);
    app.shape_press(kind, a);
    app.shape_move(b, ShapeMods::default());
    app.shape_release();
    app.shape_commit();
}

/// Place a gradient, preview it once more after a move, and apply it.
pub fn gradient(
    app: &mut PainterApp,
    shape: GradientShape,
    colors: GradientColors,
    a: Vec2,
    b: Vec2,
) {
    app.workspace.gradient.settings.shape = shape;
    app.workspace.gradient.settings.colors = colors;
    app.active_tool = Tool::Gradient;
    app.gradient_press(a);
    app.gradient_drag(b, false);
    app.gradient_update();
    app.gradient_drag(b + Vec2::new(40.0, 25.0), false);
    app.gradient_update();
    app.gradient_release();
    app.gradient_commit();
}

/// Fill the selection from its surroundings, waiting for the result.
pub fn content_aware_fill(app: &mut PainterApp) {
    app.content_aware_fill();
    while app.patch_running() {
        if !app.poll_patch() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
}

pub fn undo(app: &mut PainterApp) {
    app.apply_history(false);
}

pub fn redo(app: &mut PainterApp) {
    app.apply_history(true);
}

/// The project file bytes (as Save writes them).
pub fn save_project(app: &mut PainterApp) -> Vec<u8> {
    app.release_canvas();
    crate::project::encode_project(app).expect("encode")
}

/// Decode a project file (as Open reads it).
pub fn load_project(bytes: &[u8]) -> usize {
    let loaded = crate::project::decode_project(bytes).expect("decode");
    loaded.canvas.layers.len()
}

/// Flatten and encode as PNG (as Export does).
pub fn export_png(app: &mut PainterApp) -> Vec<u8> {
    let img = app.canvas.flatten();
    crate::project::export::encode_color_image(img, crate::project::export::ExportFormat::Png)
        .expect("encode")
}

/// Import an image file as a new layer.
pub fn import_image(app: &mut PainterApp, bytes: &[u8]) {
    app.import_image_bytes("bench", bytes).expect("import");
}

/// Undo steps on the active layer.
pub fn undo_depth(app: &PainterApp) -> usize {
    app.layer_state.histories[app.canvas.active_layer_idx]
        .stacks()
        .0
        .len()
}
