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
    app.active_tool =
        crate::app::tools::Tool::Transform(crate::selection::transform::TransformInfo::default());
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
    let mut shader = ShaderLayer::new("void mainImage(out vec4 c, in vec2 p) { c = vec4(1.0); }");
    shader.time = 12.5;
    shader.speed = 0.5;
    canvas.layers[1].shader = Some(Box::new(shader.clone()));
    let encoded = encode_project(&test_app(canvas, vec![History::new(), History::new()])).unwrap();
    let loaded = decode_project(&encoded).unwrap();
    assert_eq!(loaded.canvas.layers[1].shader.as_deref(), Some(&shader));
    assert_eq!(loaded.canvas.layers[0].shader, None);
}

#[test]
fn project_round_trips_blend_modes_and_blend_space() {
    let mut canvas = Canvas::new(TILE_SIZE, TILE_SIZE, Color32::WHITE, TILE_SIZE);
    canvas.layers[1].blend = LayerBlend::SoftLight;
    canvas.blend_space = BlendSpace::Gamma;
    let encoded = encode_project(&test_app(canvas, vec![History::new(), History::new()])).unwrap();
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
    let encoded = encode_project(&test_app(canvas, vec![History::new(), History::new()])).unwrap();
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
