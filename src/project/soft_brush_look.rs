use super::*;
use eframe::egui::Vec2;

/// Paints soft strokes like the screenshots and writes them to
/// `$SOFT_OUT` (a PNG) to inspect by eye.
#[test]
#[ignore = "writes an image for inspection"]
fn paint_soft_strokes() {
    let canvas = Canvas::new(512, 512, Color32::WHITE, TILE_SIZE);
    let mut app = tests::test_app_pub(canvas);
    app.canvas_mut().active_layer_idx = 1;
    let o = &mut app.brush_state.brush.brush_options;
    o.diameter = 120.0;
    o.hardness = 20.0;
    o.spacing = 25.0;
    o.flow = 100.0;
    o.pressure_size = true;
    // A curved stroke at full pressure.
    app.start_stroke_with_pressure(Vec2::new(20.0, 60.0), 1.0);
    for i in 1..=60 {
        let t = i as f32 / 60.0;
        app.add_stroke_point(Vec2::new(20.0 + t * 470.0, 60.0 + t * t * 120.0), 1.0);
    }
    app.finish_stroke();
    // A stroke growing from light to full pressure.
    app.start_stroke_with_pressure(Vec2::new(60.0, 380.0), 0.05);
    for i in 1..=40 {
        let t = i as f32 / 40.0;
        app.add_stroke_point(
            Vec2::new(60.0 + t * 250.0, 380.0 - t * 60.0),
            0.05 + 0.95 * t,
        );
    }
    app.finish_stroke();
    app.stroke_worker.wait_idle();
    let img = app.canvas.flatten();
    let out = std::env::var("SOFT_OUT").unwrap_or_else(|_| "soft.png".into());
    crate::project::export::save_color_image(img, out, crate::project::export::ExportFormat::Png)
        .unwrap();
}
