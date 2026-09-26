use crate::brush_engine::brush::Brush;
use crate::brush_engine::stroke::{StrokeContext, StrokeState, StrokeTiles};
use crate::canvas::Canvas;
use crate::canvas::history::UndoAction;
use eframe::egui::{Color32, ColorImage, Vec2};
use rayon::ThreadPool;

pub fn stroke_preview_image(
    brush: &mut Brush,
    pool: &ThreadPool,
    size: [usize; 2],
    tile_size: usize,
    color: Color32,
    max_diameter: f32,
) -> ColorImage {
    let [w, h] = size;
    let canvas = Canvas::new(w, h, Color32::TRANSPARENT, tile_size);
    let mut stroke = StrokeState::new();
    let mut undo = UndoAction {
        tiles: Vec::new(),
        selection: None,
        transform: None,
        layer_action: None,
    };
    let mut modified = StrokeTiles::default();

    let original_diameter = brush.brush_options.diameter;
    let original_opacity = brush.brush_options.opacity;
    let original_color = brush.brush_options.color;
    brush.brush_options.color = color;

    let steps = 100;
    let width = w as f32;
    let height = h as f32;
    let margin = width * 0.1;
    let effective_width = width - 2.0 * margin;

    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = margin + t * effective_width;
        let phase = t * std::f32::consts::PI * 2.0;
        let y = height * 0.5 + phase.sin() * height * 0.35;
        let pressure = (t * std::f32::consts::PI).sin();
        brush.brush_options.diameter = (max_diameter * pressure).max(1.0);
        let mut context = StrokeContext::new(pool, &canvas, None, &mut undo, &mut modified);
        // Diameter above is already the pressure-scaled value for this
        // preview point; pass 1.0 so add_point doesn't scale it again.
        stroke.add_point(brush, Vec2 { x, y }, 1.0, &mut context);
    }

    brush.brush_options.diameter = original_diameter;
    brush.brush_options.opacity = original_opacity;
    brush.brush_options.color = original_color;

    let mut image = ColorImage::new(size, Color32::TRANSPARENT);
    canvas.write_region_to_color_image(0, 0, w, h, &mut image, 1);
    image
}
