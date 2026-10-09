//! Shared theme tokens for everything under `src/ui/`, so the same visual
//! value (a panel shade, the accent, a swatch size) doesn't drift
//! independently in each panel that happens to need it.
//!
//! The palette is deliberately neutral gray: a tinted UI biases how colors
//! on the canvas are perceived. The accent is the only saturated color.

use eframe::egui::Color32;
use std::sync::atomic::{AtomicU32, Ordering};

/// Area around the canvas; darker than the panels so the canvas reads as the focus.
pub(crate) const BG_CANVAS: Color32 = Color32::from_gray(24);
/// Panel background (docks, bars).
pub(crate) const BG_PANEL: Color32 = Color32::from_gray(36);
/// Slightly raised surface: tab bars, section headers, list rows.
pub(crate) const BG_RAISED: Color32 = Color32::from_gray(44);
/// Recessed surface: text fields, slider rails, preview wells.
pub(crate) const BG_INSET: Color32 = Color32::from_gray(28);

/// Widget fills.
pub(crate) const WIDGET: Color32 = Color32::from_gray(52);
pub(crate) const WIDGET_HOVER: Color32 = Color32::from_gray(64);
pub(crate) const WIDGET_ACTIVE: Color32 = Color32::from_gray(76);

/// Hairline separating panels and outlining widgets.
pub(crate) const BORDER: Color32 = Color32::from_gray(20);
pub(crate) const BORDER_LIGHT: Color32 = Color32::from_gray(60);

pub(crate) const TEXT: Color32 = Color32::from_gray(212);
pub(crate) const TEXT_DIM: Color32 = Color32::from_gray(140);
pub(crate) const TEXT_STRONG: Color32 = Color32::from_gray(245);

/// The accent the app starts with, until one is chosen in Settings.
pub(crate) const DEFAULT_ACCENT: Color32 = Color32::from_rgb(56, 132, 232);

/// The accents offered in Settings, besides any colour picked.
pub(crate) const ACCENT_PRESETS: [(&str, Color32); 8] = [
    ("Blue", DEFAULT_ACCENT),
    ("Purple", Color32::from_rgb(140, 98, 230)),
    ("Pink", Color32::from_rgb(226, 86, 160)),
    ("Red", Color32::from_rgb(222, 72, 72)),
    ("Orange", Color32::from_rgb(234, 128, 40)),
    ("Yellow", Color32::from_rgb(226, 186, 40)),
    ("Green", Color32::from_rgb(64, 176, 96)),
    ("Teal", Color32::from_rgb(32, 170, 170)),
];

/// The accent as `0x00RRGGBB`: one value read by every panel, so it's a
/// global rather than threaded through each of them.
static ACCENT_RGB: AtomicU32 = AtomicU32::new(pack(DEFAULT_ACCENT));

const fn pack(c: Color32) -> u32 {
    (c.r() as u32) << 16 | (c.g() as u32) << 8 | c.b() as u32
}

/// The one accent color: selection, active tool, focused outlines.
pub(crate) fn accent() -> Color32 {
    let v = ACCENT_RGB.load(Ordering::Relaxed);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// The accent sunk toward the panels: fills that shouldn't shout.
pub(crate) fn accent_dim() -> Color32 {
    mix(BG_PANEL, accent(), 0.55)
}

/// Text on an accent fill: dark on a light accent (yellow), light otherwise.
pub(crate) fn accent_text() -> Color32 {
    let c = accent();
    let luma = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    if luma > 160.0 {
        Color32::from_gray(16)
    } else {
        TEXT_STRONG
    }
}

/// Choose the accent (the theme picks it up when rebuilt).
pub(crate) fn set_accent(c: Color32) {
    ACCENT_RGB.store(pack(c), Ordering::Relaxed);
}

/// `a` to `b` by `t` (0..=1), channel by channel.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Corner radii, Blender's way: the regions (docks, bars, the canvas) stay
/// square; what sits in them is rounded, more so the bigger it is.
/// Swatches and chips.
pub(crate) const RADIUS_SMALL: f32 = 3.0;
/// Buttons, text fields, sliders, combo boxes.
pub(crate) const RADIUS_WIDGET: f32 = 4.0;
/// Tool buttons, list rows, library cards.
pub(crate) const RADIUS_CARD: f32 = 6.0;
/// Windows, menus, popups, tooltips.
pub(crate) const RADIUS_WINDOW: f32 = 8.0;

/// Height of the top bar's controls.
pub(crate) const BAR_HEIGHT: f32 = 30.0;
/// Width of the vertical tool strip.
pub(crate) const TOOLBAR_WIDTH: f32 = 44.0;
/// Side length of a tool button.
pub(crate) const TOOL_BUTTON_SIZE: f32 = 34.0;
/// Width reserved for parameter labels in property rows.
pub(crate) const LABEL_WIDTH: f32 = 72.0;

/// Side length (in points) of a brush-tip selector swatch (`brush_settings`).
pub(crate) const TIP_SWATCH_SIZE: f32 = 32.0;

/// Ink for brush previews: the inverse of their [`BG_INSET`] background, so
/// strokes read on the dark theme whatever the brush color is.
pub(crate) const PREVIEW_INK: Color32 = Color32::from_gray(255 - BG_INSET.r());

/// Checkerboard pattern grays used to indicate transparency in color swatches.
pub(crate) const CHECKERBOARD_LIGHT: Color32 = Color32::from_gray(200);
pub(crate) const CHECKERBOARD_DARK: Color32 = Color32::from_gray(150);

/// Sizes that grow in touch mode so controls are comfortable finger targets.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub touch: bool,
    /// The one bar above the canvas (menus and tool options).
    pub menu_height: f32,
    pub toolbar_width: f32,
    pub tool_button: f32,
    /// Icon buttons in panel headers (add/delete).
    pub header_button: f32,
    /// Small icon toggles in list rows (visibility, lock).
    pub row_toggle: f32,
    pub layer_row_height: f32,
    pub recent_swatch: f32,
    pub label_width: f32,
    pub value_box_width: f32,
    pub tip_swatch: f32,
    pub wheel_max: f32,
}

const DESKTOP_METRICS: Metrics = Metrics {
    touch: false,
    // Tall enough for the tool options' sliders.
    menu_height: BAR_HEIGHT + 2.0,
    toolbar_width: TOOLBAR_WIDTH,
    tool_button: TOOL_BUTTON_SIZE,
    header_button: 24.0,
    row_toggle: 20.0,
    layer_row_height: 46.0,
    recent_swatch: 18.0,
    label_width: LABEL_WIDTH,
    value_box_width: 52.0,
    tip_swatch: TIP_SWATCH_SIZE,
    wheel_max: 210.0,
};

const TOUCH_METRICS: Metrics = Metrics {
    touch: true,
    menu_height: 42.0,
    toolbar_width: 60.0,
    tool_button: 48.0,
    header_button: 38.0,
    row_toggle: 32.0,
    layer_row_height: 62.0,
    recent_swatch: 30.0,
    label_width: 84.0,
    value_box_width: 64.0,
    tip_swatch: 44.0,
    wheel_max: 280.0,
};

fn metrics_id() -> eframe::egui::Id {
    eframe::egui::Id::new("rusty_painter_touch_metrics")
}

/// Record whether the UI is in touch mode for [`metrics`] this frame.
pub(crate) fn set_touch_metrics(ctx: &eframe::egui::Context, touch: bool) {
    ctx.data_mut(|d| d.insert_temp(metrics_id(), touch));
}

/// Current UI sizes (desktop or touch).
pub(crate) fn metrics(ctx: &eframe::egui::Context) -> Metrics {
    if ctx
        .data(|d| d.get_temp::<bool>(metrics_id()))
        .unwrap_or(false)
    {
        TOUCH_METRICS
    } else {
        DESKTOP_METRICS
    }
}
