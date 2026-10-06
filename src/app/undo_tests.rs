//! Undo and redo across the whole document: every step taken back and
//! brought again must restore the document exactly, whichever layer it was
//! on and whatever is selected when Ctrl+Z is pressed.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::app::tools::transform;
use crate::canvas::Canvas;
use crate::selection::transform::TransformInfo;
use crate::selection::{SelectionMode, SelectionShape};
use eframe::egui::{Color32, Vec2};

/// Everything undo must restore: each layer's id, name, folder and pixels
/// (empty tiles left out, since a tile emptied and one never painted look
/// the same), in stack order.
type Snapshot = Vec<(u64, String, Option<u64>, Vec<((i32, i32), Vec<Color32>)>)>;

fn snapshot(app: &PainterApp) -> Snapshot {
    let canvas = &app.canvas;
    (0..canvas.layers.len())
        .map(|i| {
            let layer = &canvas.layers[i];
            let mut keys = canvas.layer_tile_keys(i);
            keys.sort_unstable();
            let tiles = keys
                .into_iter()
                .filter_map(|key| {
                    let data = canvas.get_layer_tile_data(i, key.0, key.1)?;
                    data.iter()
                        .any(|&p| p != Color32::TRANSPARENT)
                        .then_some((key, data))
                })
                .collect();
            (
                layer.id.0,
                layer.name.clone(),
                layer.parent.map(|p| p.0),
                tiles,
            )
        })
        .collect()
}

fn app() -> PainterApp {
    let mut app = crate::project::tests::test_app_pub(Canvas::new(256, 128, Color32::WHITE, 64));
    app.canvas_mut().active_layer_idx = 1;
    app.selection_manager.canvas_size = [256, 128];
    app.brush_state.brush.brush_options.color = Color32::from_rgb(200, 30, 30);
    app.brush_state.secondary_color = Color32::from_rgb(20, 40, 220);
    app
}

fn select_rect(app: &mut PainterApp, x0: f32, y0: f32, x1: f32, y1: f32) {
    app.selection_manager.apply_shape(
        SelectionShape::Rectangle {
            start: Vec2::new(x0, y0),
            end: Vec2::new(x1, y1),
        },
        SelectionMode::Replace,
    );
}

fn gradient(app: &mut PainterApp, from: Vec2, to: Vec2) {
    app.gradient_press(from);
    app.gradient_drag(to, false);
    app.gradient_commit();
}

fn move_selection(app: &mut PainterApp, from: Vec2, by: Vec2) {
    app.active_tool = Tool::Transform(TransformInfo::default());
    transform::transform_press(app, from);
    transform::transform_drag(app, from + by, false);
    transform::flush_transform_preview(app);
    transform::transform_release(app);
    transform::commit_floating_layer(app);
    app.active_tool = Tool::Brush;
}

#[test]
fn every_step_undoes_and_redoes_exactly() {
    let mut app = app();
    let mut states = vec![snapshot(&app)];
    let step = |app: &mut PainterApp, what: &str, states: &mut Vec<Snapshot>| {
        let now = snapshot(app);
        assert_ne!(states.last(), Some(&now), "{what} changed nothing");
        states.push(now);
    };

    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(256.0, 0.0));
    step(&mut app, "gradient on layer 1", &mut states);

    app.add_layer_and_select();
    step(&mut app, "add layer 2", &mut states);

    select_rect(&mut app, 70.0, 20.0, 150.0, 100.0);
    gradient(&mut app, Vec2::new(70.0, 0.0), Vec2::new(150.0, 0.0));
    step(&mut app, "gradient in a selection on layer 2", &mut states);

    // Moved across tiles onto layer 2's own paint.
    select_rect(&mut app, 80.0, 30.0, 110.0, 60.0);
    move_selection(&mut app, Vec2::new(95.0, 45.0), Vec2::new(40.0, 30.0));
    step(&mut app, "move a selection on layer 2", &mut states);

    app.canvas_mut().active_layer_idx = 1;
    select_rect(&mut app, 0.0, 0.0, 60.0, 128.0);
    app.delete_selection_contents();
    step(&mut app, "delete part of layer 1", &mut states);

    app.selection_manager.clear_selection();
    app.move_layer(2, 1, None);
    step(&mut app, "move layer 2 below layer 1", &mut states);

    app.remove_layer(2);
    step(&mut app, "remove layer 1", &mut states);

    let last = states.len() - 1;
    for i in (0..last).rev() {
        app.apply_history(false);
        assert_eq!(snapshot(&app), states[i], "undo back to state {i}");
    }
    for (i, state) in states.iter().enumerate().skip(1) {
        app.apply_history(true);
        assert_eq!(&snapshot(&app), state, "redo up to state {i}");
    }
}

#[test]
fn undo_takes_back_the_last_change_whichever_layer_is_selected() {
    let mut app = app();
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(256.0, 0.0));
    let after_layer_1 = snapshot(&app);
    app.add_layer_and_select();
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(0.0, 128.0));
    // Back on layer 1, whose own last change is older.
    app.canvas_mut().active_layer_idx = 1;
    app.apply_history(false);
    let layer_2 = app.canvas.layer_index_of(app.canvas.layers[2].id).unwrap();
    assert!(
        app.canvas
            .layer_tile_keys(layer_2)
            .iter()
            .all(|&(tx, ty)| app
                .canvas
                .get_layer_tile_data(layer_2, tx, ty)
                .is_none_or(|d| d.iter().all(|&p| p == Color32::TRANSPARENT))),
        "the newest change (layer 2's gradient) was undone"
    );
    app.apply_history(false);
    assert_eq!(snapshot(&app), after_layer_1, "then the layer it was on");
}

#[test]
fn a_removed_layer_comes_back_with_its_history() {
    let mut app = app();
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(256.0, 0.0));
    let painted = snapshot(&app);
    app.add_layer_and_select();
    app.remove_layer(1);
    app.apply_history(false);
    // Undoing the removal, then the add, then the gradient: all still there.
    app.apply_history(false);
    assert_eq!(snapshot(&app), painted);
    app.apply_history(false);
    assert!(
        snapshot(&app)[1].3.is_empty(),
        "the painting before the removal is still undoable"
    );
}

#[test]
fn image_menu_steps_undo_and_redo_with_the_canvas_size() {
    use crate::canvas::geometry::ImageOp;
    let mut app = app();
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(256.0, 0.0));
    let mut states = vec![(snapshot(&app), app.canvas.width(), app.canvas.height())];
    for op in [
        ImageOp::RotateCw,
        ImageOp::Reframe {
            x: 10,
            y: 20,
            w: 100,
            h: 150,
        },
        ImageOp::Resize {
            w: 50,
            h: 75,
            smooth: true,
        },
        ImageOp::FlipHorizontal,
    ] {
        app.apply_image_op(op);
        states.push((snapshot(&app), app.canvas.width(), app.canvas.height()));
        assert_eq!(app.render_cache.tiles_x, app.canvas.width().div_ceil(64));
    }
    assert_eq!((app.canvas.width(), app.canvas.height()), (50, 75));
    let last = states.len() - 1;
    for i in (0..last).rev() {
        app.apply_history(false);
        let now = (snapshot(&app), app.canvas.width(), app.canvas.height());
        assert!(now == states[i], "undo back to state {i}");
    }
    for (i, state) in states.iter().enumerate().skip(1) {
        app.apply_history(true);
        let now = (snapshot(&app), app.canvas.width(), app.canvas.height());
        assert!(now == *state, "redo to state {i}");
    }
}

#[test]
fn a_saved_file_keeps_the_steps_from_before_a_resize() {
    use crate::canvas::geometry::ImageOp;
    let state = |app: &PainterApp| (snapshot(app), app.canvas.width(), app.canvas.height());
    let mut app = app();
    let mut states = vec![state(&app)];
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(256.0, 0.0));
    states.push(state(&app));
    app.add_layer_and_select();
    states.push(state(&app));
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(0.0, 128.0));
    states.push(state(&app));
    app.apply_image_op(ImageOp::Resize {
        w: 100,
        h: 60,
        smooth: true,
    });
    states.push(state(&app));
    app.apply_image_op(ImageOp::RotateCw);
    states.push(state(&app));
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(60.0, 100.0));
    states.push(state(&app));
    // The last step undone: it's saved on the redo side.
    app.apply_history(false);
    let last = states.len() - 1;

    let bytes = crate::project::encode_project(&app).unwrap();
    let loaded = crate::project::decode_project(&bytes).unwrap();
    let mut app = self::app();
    app.replace_document(loaded.canvas, loaded.history);
    assert!(state(&app) == states[last - 1], "opens as saved");
    for i in (0..last - 1).rev() {
        app.apply_history(false);
        assert!(state(&app) == states[i], "undo back to state {i}");
        assert_eq!(app.render_cache.tiles_x, app.canvas.width().div_ceil(64));
    }
    for (i, s) in states.iter().enumerate().skip(1) {
        app.apply_history(true);
        assert!(state(&app) == *s, "redo to state {i}");
    }
}

#[test]
fn a_file_without_resize_steps_has_no_document_in_its_header() {
    // A step's `document` is left out of the file when it has none, so
    // the header reads exactly as older builds wrote it.
    let mut app = app();
    gradient(&mut app, Vec2::new(0.0, 0.0), Vec2::new(256.0, 0.0));
    let bytes = crate::project::encode_project(&app).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("\"document\""));
    let loaded = crate::project::decode_project(&bytes).unwrap();
    assert_eq!(loaded.history.stacks().0.len(), 1);
}

#[test]
fn an_adjustment_layer_undoes_redoes_and_saves_with_its_filter() {
    use crate::canvas::filters::Filter;
    let mut app = app();
    let filter = Filter::HueSaturation {
        hue: 40.0,
        saturation: 0.1,
        lightness: 0.0,
    };
    app.add_adjustment_layer(filter);
    let idx = app.canvas.active_layer_idx;
    assert_eq!(app.canvas.layers[idx].adjustment, Some(filter));
    assert_eq!(
        app.workspace.filter.editing,
        app.canvas.layer_id_at(idx),
        "settings open"
    );
    let bytes = crate::project::encode_project(&app).unwrap();
    let loaded = crate::project::decode_project(&bytes).unwrap();
    assert!(
        loaded
            .canvas
            .layers
            .iter()
            .any(|l| l.adjustment == Some(filter))
    );
    app.remove_layer(idx);
    app.apply_history(false);
    let idx = app.canvas.active_layer_idx;
    assert_eq!(
        app.canvas.layers[idx].adjustment,
        Some(filter),
        "undoing the delete"
    );
    app.apply_history(false);
    app.apply_history(true);
    assert!(
        app.canvas
            .layers
            .iter()
            .any(|l| l.adjustment == Some(filter)),
        "redoing the add"
    );
}

#[test]
fn a_fill_layer_and_a_border_undo_redo_and_save() {
    use crate::canvas::layer_style::{Border, LayerStyle};
    let mut app = app();
    app.add_gradient_fill_layer();
    let idx = app.canvas.active_layer_idx;
    let fill = app.canvas.layers[idx].style.fill.expect("a fill layer");
    assert!(app.canvas.layers[idx].locked, "not painted on");
    assert_eq!(
        app.workspace.filter.fill_editing,
        app.canvas.layer_id_at(idx)
    );
    // A border on the layer below.
    let below = idx - 1;
    let border = LayerStyle {
        fill: None,
        border: Some(Border::default()),
        impasto: None,
        lightness_map: false,
    };
    app.set_layer_style(below, border);
    let bytes = crate::project::encode_project(&app).unwrap();
    let loaded = crate::project::decode_project(&bytes).unwrap();
    assert!(
        loaded
            .canvas
            .layers
            .iter()
            .any(|l| l.style.fill == Some(fill))
    );
    assert!(loaded.canvas.layers.iter().any(|l| l.style == border));
    app.remove_layer(idx);
    app.apply_history(false);
    let idx = app.canvas.active_layer_idx;
    assert_eq!(
        app.canvas.layers[idx].style.fill,
        Some(fill),
        "undoing the delete"
    );
    app.apply_history(false);
    assert!(app.canvas.layers.iter().all(|l| l.style.fill.is_none()));
    app.apply_history(true);
    assert!(
        app.canvas.layers.iter().any(|l| l.style.fill == Some(fill)),
        "redoing the add"
    );
}

#[test]
fn painting_a_bordered_layer_redraws_the_tiles_around() {
    use crate::canvas::layer_style::{Border, LayerStyle};
    let mut app = app();
    let idx = app.canvas.active_layer_idx;
    app.set_layer_style(
        idx,
        LayerStyle {
            fill: None,
            border: Some(Border::default()),
            impasto: None,
            lightness_map: false,
        },
    );
    app.render_cache = crate::app::state::RenderCache::new(app.canvas.width(), app.canvas.height());
    for tile in app.render_cache.tiles.iter_mut() {
        tile.dirty = false;
    }
    let ts = crate::app::document::TILE_SIZE as i32;
    // A change right at a tile's edge.
    app.mark_rect_damage([ts - 2, ts + 4, ts, ts + 6]);
    let tiles_x = app.render_cache.tiles_x;
    let dirty = |tx: usize, ty: usize| app.render_cache.tiles[ty * tiles_x + tx].dirty;
    assert!(dirty(0, 1), "its own tile");
    assert!(dirty(1, 1), "the next one, where the border spills");
    assert!(!dirty(3, 0), "not far away");
}

#[test]
fn a_shader_layer_keeps_its_shader_through_undo_and_redo() {
    let mut app = app();
    let (name, source) = crate::canvas::shader::TEMPLATES[1];
    let idx = app.add_shader_layer(name, source).unwrap();
    let id = app.canvas.layers[idx].id;
    let shader = |app: &PainterApp| {
        app.canvas.layer_index_of(id).and_then(|i| {
            app.canvas.layers[i]
                .shader
                .as_deref()
                .map(|s| s.source.clone())
        })
    };
    assert_eq!(shader(&app).as_deref(), Some(source));
    assert!(
        app.canvas.layers[idx].locked,
        "no painting on a shader layer"
    );
    // Removed and brought back: still a shader layer.
    app.remove_layer(idx);
    assert_eq!(shader(&app), None);
    app.apply_history(false);
    assert_eq!(shader(&app).as_deref(), Some(source));
    // Its creation undone and redone.
    app.apply_history(false);
    assert_eq!(shader(&app), None);
    app.apply_history(true);
    assert_eq!(shader(&app).as_deref(), Some(source));
}

#[test]
fn editing_a_shader_is_unsaved_work() {
    let mut app = app();
    let (name, source) = crate::canvas::shader::TEMPLATES[0];
    let idx = app.add_shader_layer(name, source).unwrap();
    app.mark_saved();
    assert!(!app.has_unsaved_work());
    let id = app.canvas.layers[idx].id;
    app.set_shader_source(
        id,
        "void mainImage(out vec4 c, in vec2 p) { c = vec4(1.0); }",
    );
    assert!(app.has_unsaved_work());
}
