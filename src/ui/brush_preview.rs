use crate::brush_engine::{brush::Brush, preview::stroke_preview_image};
use eframe::egui;
use rayon::ThreadPool;

use super::brush_settings::BrushPreviewState;

/// Preview dab diameter as a share of the strip height: the preview shows
/// the brush's character (hardness, softness curve, spacing, pressure, tip)
/// at a fixed size, so a huge brush doesn't overflow the strip.
const PREVIEW_DIAMETER_FRACTION: f32 = 0.55;

/// Render the brush's preview stroke at the screen's real pixel density
/// (`state.size` is in points), so it stays sharp on HiDPI displays.
pub(crate) fn render_preview(
    state: &mut BrushPreviewState,
    brush: &Brush,
    pool: &ThreadPool,
    ctx: &egui::Context,
) {
    let ppp = ctx.pixels_per_point();
    let size = state
        .size
        .map(|v| ((v as f32 * ppp).round() as usize).max(1));
    let diameter = size[1] as f32 * PREVIEW_DIAMETER_FRACTION;
    // A copy: the preview renders at its own size, not the brush's.
    let mut brush = brush.clone();
    // Contrast with the preview background, keeping the brush's alpha.
    let ink = {
        let ink = crate::ui::style::PREVIEW_INK;
        let alpha = brush.brush_options.color.a();
        egui::Color32::from_rgba_unmultiplied(ink.r(), ink.g(), ink.b(), alpha)
    };
    let image = stroke_preview_image(&mut brush, pool, size, 64, ink, diameter);
    state.texture = Some(ctx.load_texture("brush_preview", image, egui::TextureOptions::LINEAR));
    state.pixels_per_point = ppp;
}
