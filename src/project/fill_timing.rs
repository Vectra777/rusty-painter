use super::*;
use crate::app::state::LayerState;
use eframe::egui::Vec2;

#[test]
#[ignore = "timing; run with --release --ignored"]
fn fill_on_a_4000px_canvas() {
    use crate::canvas::fill::{FillSettings, bucket_fill};
    let canvas = Canvas::new(4000, 4000, Color32::WHITE, TILE_SIZE);
    // Line art: a grid of 200 px cells on layer 1.
    for ty in 0..63 {
        for tx in 0..63 {
            let mut t = vec![Color32::TRANSPARENT; TILE_SIZE * TILE_SIZE];
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    let (gx, gy) = (tx * 64 + x, ty * 64 + y);
                    if gx % 200 < 3 || gy % 200 < 3 {
                        t[y * TILE_SIZE + x] = Color32::BLACK;
                    }
                }
            }
            canvas.set_layer_tile_data(1, tx as i32, ty as i32, t);
        }
    }
    let _ = LayerState::new(2);
    let t = std::time::Instant::now();
    for i in 0..100 {
        let _ = canvas.render_reference(None, (i % 60) * 64, 640, 64, 64);
    }
    eprintln!("render one tile (all visible): {:?}", t.elapsed() / 100);
    let t = std::time::Instant::now();
    let px = canvas.render_reference(None, 0, 0, 4000, 64);
    eprintln!(
        "render 64 rows (all visible): {:?} ({} px)",
        t.elapsed(),
        px.len()
    );
    for (label, source) in [("all visible", None), ("layer", Some(1))] {
        let r = |x, y, w, h| canvas.render_reference(source, x, y, w, h);
        for gap in [0u8, 6] {
            let t = std::time::Instant::now();
            let s = FillSettings {
                gap,
                ..FillSettings::default()
            };
            let m = bucket_fill(&r, 4000, 4000, (100, 100), &s).unwrap();
            eprintln!(
                "{label} gap {gap}: small cell {:?} ({}x{})",
                t.elapsed(),
                m.w,
                m.h
            );
        }
    }
    {
        let r = |x, y, w, h| canvas.render_reference(None, x, y, w, h);
        let s = FillSettings {
            tolerance: 255,
            ..FillSettings::default()
        };
        let t = std::time::Instant::now();
        let m = bucket_fill(&r, 4000, 4000, (100, 100), &s).unwrap();
        eprintln!("mask only, whole canvas: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let mut a = crate::canvas::history::UndoAction {
            tiles: vec![],
            selection: None,
            transform: None,
            layer_action: None,
        };
        canvas.paint_mask(1, &m, Color32::RED, &mut a);
        eprintln!("paint_mask whole canvas: {:?}", t.elapsed());
    }
    // Whole app path (mask + paint + undo), small cell and a whole-canvas fill.
    let mut app = tests::test_app_pub(canvas);
    app.canvas_mut().active_layer_idx = 1;
    app.active_tool = crate::app::tools::Tool::Fill;
    for (i, (label, pos, tol)) in [("app whole canvas", (100.0, 100.0), 255); 4]
        .into_iter()
        .enumerate()
    {
        app.workspace.fill.settings.tolerance = tol;
        app.brush_state.brush.brush_options.color = [Color32::BLUE, Color32::GREEN][i % 2];
        let t = std::time::Instant::now();
        app.fill_press(Vec2::new(pos.0, pos.1));
        eprintln!("{label}: {:?}", t.elapsed());
    }
}
