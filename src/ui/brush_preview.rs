use crate::brush_engine::{brush::Brush, preview::stroke_preview_image};
use eframe::egui;
use rayon::ThreadPool;

use super::brush_settings::BrushPreviewState;

pub(crate) fn render_preview(
    state: &mut BrushPreviewState,
    brush: &mut Brush,
    pool: &ThreadPool,
    ctx: &egui::Context,
) {
    let image = stroke_preview_image(
        brush,
        pool,
        state.size,
        64,
        brush.brush_options.color,
        brush.brush_options.diameter,
    );
    state.texture = Some(ctx.load_texture("brush_preview", image, egui::TextureOptions::NEAREST));
}
