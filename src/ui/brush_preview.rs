//! Rendering the brush preview strip at the screen's pixel density.

use crate::brush_engine::brush::Brush;
use crate::ui::preview_worker::PreviewLook;
use eframe::egui;
use rayon::ThreadPool;
use std::sync::Arc;

use super::brush_settings::BrushPreviewState;

/// Preview dab diameter as a share of the strip height: the preview shows
/// the brush's character (hardness, softness curve, spacing, pressure, tip)
/// at a fixed size, so a huge brush doesn't overflow the strip.
const PREVIEW_DIAMETER_FRACTION: f32 = 0.55;

/// Ask for the brush's preview stroke at the screen's real pixel density
/// (`state.size` is in points), so it stays sharp on HiDPI displays. It's
/// drawn on another thread; [`collect_preview`] picks it up.
pub(crate) fn render_preview(
    state: &mut BrushPreviewState,
    brush: &Brush,
    pool: &Arc<ThreadPool>,
    ctx: &egui::Context,
) {
    let ppp = ctx.pixels_per_point();
    let size = state
        .size
        .map(|v| ((v as f32 * ppp).round() as usize).max(1));
    let diameter = size[1] as f32 * PREVIEW_DIAMETER_FRACTION;
    // Contrast with the preview background, keeping the brush's alpha.
    let ink = {
        let ink = crate::ui::style::PREVIEW_INK;
        let alpha = brush.brush_options.color.a();
        egui::Color32::from_rgba_unmultiplied(ink.r(), ink.g(), ink.b(), alpha)
    };
    let look = PreviewLook {
        size,
        diameter,
        ink,
    };
    state.worker.request("settings", brush, look, pool, ctx);
    state.pending_pixels_per_point = ppp;
}

/// The strip drawn since last frame, if any (the last one stays until then).
pub(crate) fn collect_preview(state: &mut BrushPreviewState, ctx: &egui::Context) {
    if let Some((_, texture)) = state.worker.collect(ctx).pop() {
        state.texture = Some(texture);
        state.pixels_per_point = state.pending_pixels_per_point;
    }
}
