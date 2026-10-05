//! The app's settings, in `settings.json` next to `brushes/`: preferences,
//! each tool's options and where the last session left off. Saved when they
//! change (once the pointer is up), read at startup. The brush and eraser in
//! use are kept whole in `brushes/current.rpbrush`, colour and stabiliser
//! included.

use crate::PainterApp;
use crate::app::document::{BackgroundChoice, CanvasUnit, MAX_CANVAS_DPI};
use crate::app::input::keyboard::KeyboardLayout;
use crate::app::jobs::write_later;
use crate::app::tools::{
    Tool,
    blend::BlendToolSettings,
    fill::{FillMode, FillSource},
    gradient::GradientSettings,
    gradient_colors::{GradientColors, PRESETS},
    liquify::LiquifySettings,
    select::{ColorRangeSettings, WandSettings},
    shape::{ShapeKind, ShapeSettings},
};
use crate::brush_engine::{
    brush::{Brush, BrushPreset},
    hardness::SoftnessCurve,
    preset_file,
};
use crate::canvas::{blend_modes::BlendSpace, fill::FillSettings, text::TextStyle};
use crate::project::export::ExportFormat;
use crate::selection::SelectionType;
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct AppSettings {
    tool: Tool,
    eraser_active: bool,
    /// The presets the brush and the eraser came from.
    brush_preset: Option<String>,
    eraser_preset: Option<String>,
    secondary_color: [u8; 4],
    recent_colors: Vec<[u8; 4]>,
    thread_count: usize,
    use_masked_brush: bool,
    pressure_curve: SoftnessCurve,
    touch_mode: bool,
    finger_painting: bool,
    autohide_panels: bool,
    show_left_panel: bool,
    show_color: bool,
    show_layers: bool,
    show_presets: bool,
    quickshape: bool,
    transform_pick_layer: bool,
    select_type: SelectionType,
    wand: WandSettings,
    color_range: ColorRangeSettings,
    fill_mode: FillMode,
    fill_source: FillSource,
    fill: FillSettings,
    liquify: LiquifySettings,
    blend: BlendToolSettings,
    shape: ShapeSettings,
    shape_kind: ShapeKind,
    gradient: GradientSettings,
    text: TextStyle,
    palette_count: usize,
    palette_dither: bool,
    /// Chosen in Settings; `None` = automatic.
    keyboard_layout: Option<KeyboardLayout>,
    /// Shortcuts changed from their defaults: action → keys.
    shortcuts: std::collections::BTreeMap<String, Vec<String>>,
    /// The New Canvas dialog (its size comes from the canvas open).
    new_canvas_unit: CanvasUnit,
    new_canvas_resolution: f32,
    new_canvas_background: BackgroundChoice,
    new_canvas_custom_bg: [u8; 4],
    new_canvas_blend_space: BlendSpace,
    export_format: ExportFormat,
    export_base_name: String,
    /// The folder the file dialogs open in.
    last_dir: Option<PathBuf>,
}

/// The folder a file was last opened or saved in. A global because the
/// dialogs are opened from free functions all over the UI; it's kept in
/// `settings.json` with the rest.
static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn last_dir() -> Option<PathBuf> {
    LAST_DIR.lock().ok().and_then(|d| d.clone())
}

/// A file dialog opening in the folder used last.
#[cfg(not(target_os = "android"))]
pub fn file_dialog() -> rfd::AsyncFileDialog {
    let dialog = rfd::AsyncFileDialog::new();
    match last_dir() {
        Some(dir) if dir.is_dir() => dialog.set_directory(dir),
        _ => dialog,
    }
}

/// `path` was picked in a file dialog: the next one opens in its folder.
pub fn remember_dir(path: &Path) {
    if let (Some(dir), Ok(mut last)) = (path.parent(), LAST_DIR.lock()) {
        *last = Some(dir.to_path_buf());
    }
}

/// What was written last: the settings, and the brush and eraser.
pub struct Saved {
    settings: AppSettings,
    brushes: [Brush; 2],
}

impl AppSettings {
    fn of(app: &PainterApp) -> Self {
        let (bs, ws) = (&app.brush_state, &app.workspace);
        let nc = &app.modal_state.new_canvas;
        let (brush_preset, eraser_preset) = if bs.eraser_active {
            (bs.stashed_preset.clone(), bs.active_preset.clone())
        } else {
            (bs.active_preset.clone(), bs.stashed_preset.clone())
        };
        Self {
            // Transforming is mid-edit, not a place to come back to.
            tool: match app.active_tool {
                Tool::Transform(_) => Tool::Brush,
                tool => tool,
            },
            eraser_active: bs.eraser_active,
            brush_preset,
            eraser_preset,
            secondary_color: bs.secondary_color.to_array(),
            recent_colors: bs.recent_colors.iter().map(|c| c.to_array()).collect(),
            thread_count: ws.thread_count,
            use_masked_brush: bs.use_masked_brush,
            pressure_curve: ws.pressure_curve.clone(),
            touch_mode: ws.touch_mode,
            finger_painting: ws.finger_painting,
            autohide_panels: ws.autohide_panels,
            show_left_panel: ws.show_left_panel,
            show_color: ws.show_color,
            show_layers: ws.show_layers,
            show_presets: bs.show_presets,
            quickshape: ws.quickshape.enabled,
            transform_pick_layer: ws.transform_pick_layer,
            select_type: ws.select_type,
            wand: ws.select.wand,
            color_range: ws.select.color,
            fill_mode: ws.fill.mode,
            fill_source: ws.fill.source,
            fill: ws.fill.settings,
            liquify: ws.liquify,
            blend: ws.blend.clone(),
            shape: ws.shapes.settings,
            shape_kind: ws.shapes.last_kind,
            gradient: ws.gradient.settings,
            text: ws.text.style,
            palette_count: ws.palette.count,
            palette_dither: ws.palette.dither,
            keyboard_layout: ws.keyboard.choice,
            shortcuts: ws.keymap.to_settings(),
            new_canvas_unit: nc.unit,
            new_canvas_resolution: nc.resolution,
            new_canvas_background: nc.background,
            new_canvas_custom_bg: nc.custom_bg.to_array(),
            new_canvas_blend_space: nc.blend_space,
            export_format: app.export_state.settings.format,
            export_base_name: app.export_state.settings.base_name.clone(),
            last_dir: last_dir(),
        }
    }

    fn apply(self, app: &mut PainterApp) {
        let color = |[r, g, b, a]: [u8; 4]| Color32::from_rgba_premultiplied(r, g, b, a);
        let preset = |name: Option<String>| {
            name.filter(|n| app.brush_state.presets.iter().any(|p| &p.name == n))
        };
        let (brush_preset, eraser_preset) = (preset(self.brush_preset), preset(self.eraser_preset));
        let bs = &mut app.brush_state;
        bs.eraser_active = self.eraser_active;
        (bs.active_preset, bs.stashed_preset) = if self.eraser_active {
            (eraser_preset, brush_preset)
        } else {
            (brush_preset, eraser_preset)
        };
        bs.secondary_color = color(self.secondary_color);
        bs.recent_colors = self.recent_colors.into_iter().map(color).collect();
        bs.use_masked_brush = self.use_masked_brush;
        bs.show_presets = self.show_presets;
        app.active_tool = self.tool;
        let ws = &mut app.workspace;
        let threads = self.thread_count.clamp(1, ws.max_threads);
        if threads != ws.thread_count
            && let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(threads).build()
        {
            ws.thread_count = threads;
            ws.pool = std::sync::Arc::new(pool);
        }
        ws.pressure_curve = self.pressure_curve;
        ws.touch_mode = self.touch_mode;
        ws.finger_painting = self.finger_painting;
        ws.autohide_panels = self.autohide_panels;
        // A tablet starts with the canvas clear: the panels stay closed.
        let restore = !ws.touch_mode;
        ws.show_left_panel = restore && self.show_left_panel;
        ws.show_color = restore && self.show_color;
        ws.show_layers = restore && self.show_layers;
        app.brush_state.show_presets &= restore;
        ws.quickshape.enabled = self.quickshape;
        ws.transform_pick_layer = self.transform_pick_layer;
        ws.select_type = self.select_type;
        ws.select.wand = self.wand;
        ws.select.color = self.color_range;
        ws.fill.mode = self.fill_mode;
        ws.fill.source = self.fill_source;
        ws.fill.settings = self.fill;
        ws.liquify = self.liquify;
        ws.blend = self.blend;
        ws.shapes.settings = self.shape;
        ws.shapes.last_kind = self.shape_kind;
        ws.gradient.settings = self.gradient;
        // A gradient that isn't there any more: back to the default.
        let colors_exist = match self.gradient.colors {
            GradientColors::Preset(i) => i < PRESETS.len(),
            GradientColors::Custom(i) => i < ws.gradient.library.custom.len(),
            _ => true,
        };
        if !colors_exist {
            ws.gradient.settings.colors = GradientSettings::default().colors;
        }
        ws.text.style = self.text;
        ws.palette.count = self.palette_count;
        ws.palette.dither = self.palette_dither;
        ws.keyboard.choice = self.keyboard_layout;
        ws.keymap = crate::app::input::keymap::Keymap::from_settings(&self.shortcuts);
        let nc = &mut app.modal_state.new_canvas;
        nc.unit = self.new_canvas_unit;
        nc.resolution = self.new_canvas_resolution.clamp(1.0, MAX_CANVAS_DPI);
        nc.background = self.new_canvas_background;
        nc.custom_bg = color(self.new_canvas_custom_bg);
        nc.blend_space = self.new_canvas_blend_space;
        nc.sync_from_canvas(&app.canvas);
        let export = &mut app.export_state.settings;
        export.format = self.export_format;
        export.base_name = self.export_base_name;
        if let Ok(mut last) = LAST_DIR.lock() {
            *last = self.last_dir;
        }
    }
}

/// The settings in `bytes`, field by field over `defaults`: a field that
/// doesn't read any more (renamed, changed) falls back alone.
fn read(bytes: &[u8], defaults: &AppSettings) -> AppSettings {
    let file = match serde_json::from_slice(bytes) {
        Ok(Value::Object(file)) => file,
        Ok(_) => return defaults.clone(),
        Err(err) => {
            log::warn!("Ignoring settings.json: {err}");
            return defaults.clone();
        }
    };
    let Ok(Value::Object(mut merged)) = serde_json::to_value(defaults) else {
        return defaults.clone();
    };
    for (key, value) in file {
        let mut tried = merged.clone();
        tried.insert(key.clone(), value.clone());
        if serde_json::from_value::<AppSettings>(Value::Object(tried)).is_ok() {
            merged.insert(key, value);
        } else {
            log::warn!("Ignoring setting {key} in settings.json");
        }
    }
    serde_json::from_value(Value::Object(merged)).unwrap_or_else(|_| defaults.clone())
}

impl PainterApp {
    fn settings_path(&self) -> std::path::PathBuf {
        self.brush_state
            .brushes_path
            .with_file_name("settings.json")
    }

    fn current_brushes_path(&self) -> std::path::PathBuf {
        self.brush_state.brushes_path.join("current.rpbrush")
    }

    /// The brush tool's brush and the eraser's, whichever is active.
    fn current_brushes(&self) -> [Brush; 2] {
        let bs = &self.brush_state;
        let (active, stashed) = (bs.brush.clone(), bs.stashed_brush.clone());
        if bs.eraser_active {
            [stashed, active]
        } else {
            [active, stashed]
        }
    }

    /// Where the last session left off (after the presets are loaded).
    pub(crate) fn load_settings(&mut self) {
        let defaults = AppSettings::of(self);
        let settings = match std::fs::read(self.settings_path()) {
            Ok(bytes) => read(&bytes, &defaults),
            Err(_) => defaults,
        };
        settings.apply(self);
        let brushes = std::fs::read(self.current_brushes_path())
            .map_err(|e| e.to_string())
            .and_then(|bytes| preset_file::decode(&bytes));
        if let Ok(Ok([brush, eraser])) = brushes.map(<[BrushPreset; 2]>::try_from) {
            let bs = &mut self.brush_state;
            (bs.brush, bs.stashed_brush) = if bs.eraser_active {
                (eraser.brush, brush.brush)
            } else {
                (brush.brush, eraser.brush)
            };
            bs.brush.is_changed = true;
            bs.brush_preview.dirty = true;
        }
        self.workspace.settings_saved = Some(Saved {
            settings: AppSettings::of(self),
            brushes: self.current_brushes(),
        });
    }

    /// Write the settings and the brushes in use when they've changed
    /// (not while the pointer is down: not every frame of a slider drag).
    /// Errors are logged.
    pub(crate) fn save_settings(&mut self, pointer_down: bool) {
        let Some(saved) = &self.workspace.settings_saved else {
            return;
        };
        if pointer_down || self.brush_state.is_drawing {
            return;
        }
        let settings = AppSettings::of(self);
        let brushes = self.current_brushes();
        let settings_changed = settings != saved.settings;
        let brushes_changed =
            (brushes.iter().zip(&saved.brushes)).any(|(a, b)| !preset_file::same_settings(a, b));
        // Written on the file writer's thread (a disk flush can be slow).
        if settings_changed {
            match serde_json::to_vec_pretty(&settings) {
                Ok(bytes) => write_later(self.settings_path(), "settings", move || Ok(bytes)),
                Err(err) => log::warn!("Couldn't save the settings: {err}"),
            }
        }
        if brushes_changed {
            let presets = brushes.clone().map(|brush| BrushPreset {
                name: "Current".into(),
                brush,
                file: None,
            });
            write_later(self.current_brushes_path(), "brushes in use", move || {
                preset_file::encode(&presets)
            });
        }
        if settings_changed || brushes_changed {
            self.workspace.settings_saved = Some(Saved { settings, brushes });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn app_in(dir: &std::path::Path) -> PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        app.brush_state.presets = PainterApp::default_brush_presets();
        app
    }

    #[test]
    fn settings_and_brushes_come_back_and_a_bad_field_falls_back_alone() {
        let dir = std::env::temp_dir().join(format!("rp-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = app_in(&dir);
        app.load_settings();
        app.workspace.fill.settings.tolerance = 77;
        app.workspace.show_layers = true;
        app.active_tool = Tool::Fill;
        app.brush_state.brush.brush_options.diameter = 123.0;
        app.brush_state.brush.brush_options.color = Color32::from_rgb(10, 20, 30);
        app.workspace.keyboard.choice = Some(KeyboardLayout::default());
        app.modal_state.new_canvas.unit = CanvasUnit::Inches;
        app.modal_state.new_canvas.resolution = 32.0;
        app.export_state.settings.format = ExportFormat::Jpeg;
        remember_dir(&dir.join("picture.png"));
        app.save_settings(true);
        assert!(
            !dir.join("settings.json").exists(),
            "not while the pointer is down"
        );
        app.save_settings(false);

        let mut back = app_in(&dir);
        back.load_settings();
        assert_eq!(back.workspace.fill.settings.tolerance, 77);
        assert!(back.workspace.show_layers);
        assert_eq!(back.active_tool, Tool::Fill);
        assert_eq!(back.brush_state.brush.brush_options.diameter, 123.0);
        assert_eq!(
            back.brush_state.brush.brush_options.color,
            Color32::from_rgb(10, 20, 30)
        );
        assert_eq!(
            back.workspace.keyboard.choice,
            Some(KeyboardLayout::default())
        );
        assert_eq!(back.export_state.settings.format, ExportFormat::Jpeg);
        assert_eq!(last_dir().as_deref(), Some(dir.as_path()));
        // The New Canvas size is the canvas's, in the unit kept: 64 px at 32 dpi.
        let nc = &back.modal_state.new_canvas;
        assert_eq!((nc.unit, nc.width), (CanvasUnit::Inches, 2.0));
        assert_eq!(nc.validated_dimensions(), Ok((64, 64)));

        // A field that no longer reads: only it goes back to the default.
        let path = dir.join("settings.json");
        let mut json: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json["tool"] = "NoSuchTool".into();
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let mut back = app_in(&dir);
        back.load_settings();
        assert_eq!(back.active_tool, Tool::Brush);
        assert_eq!(back.workspace.fill.settings.tolerance, 77);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
