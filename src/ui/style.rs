//! Shared sizing/color constants for panels under `src/ui/`, so the same
//! visual value (a swatch size, a panel background shade) doesn't drift
//! independently in each panel that happens to need it.

use eframe::egui::Color32;

/// Side length (in points) of a brush-tip selector swatch (`brush_settings`).
pub(crate) const TIP_SWATCH_SIZE: f32 = 32.0;

/// Side length (in points) of a brush preset thumbnail (`brush_list`).
pub(crate) const PRESET_PREVIEW_SIZE: f32 = 64.0;

/// Background fill for a preset/preview panel tile.
pub(crate) const PANEL_BG: Color32 = Color32::from_gray(30);

/// Checkerboard pattern grays used to indicate transparency in color swatches.
pub(crate) const CHECKERBOARD_LIGHT: Color32 = Color32::from_gray(240);
pub(crate) const CHECKERBOARD_DARK: Color32 = Color32::from_gray(200);
