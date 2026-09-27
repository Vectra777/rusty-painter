//! Keyboard shortcuts, plus the tool/color actions they share with the
//! toolbar and menus.

use super::PainterApp;
use super::tools::Tool;
use crate::brush_engine::brush_options::BlendMode;
use crate::selection::SelectionType;
use crate::selection::transform::TransformInfo;
use crate::ui;
use eframe::egui::{self, Key, Modifiers};

/// Brush size step for `[` / `]`.
const SIZE_STEP: f32 = 1.2;
const MIN_BRUSH_SIZE: f32 = 1.0;
const MAX_BRUSH_SIZE: f32 = 3000.0;

impl PainterApp {
    /// Whether switching tools now would cut into an in-progress gesture.
    fn is_mid_gesture(&self) -> bool {
        self.brush_state.is_drawing || self.viewport.is_primary_down
    }

    /// Activate the brush or the eraser. Each keeps its own settings (size,
    /// tip, preset...), so switching swaps them rather than just flipping
    /// the blend mode: the eraser is ready to erase without any menu.
    pub(crate) fn set_brush_tool(&mut self, eraser: bool) {
        self.active_tool = Tool::Brush;
        let bs = &mut self.brush_state;
        if bs.eraser_active != eraser {
            let color = bs.brush.brush_options.color;
            std::mem::swap(&mut bs.brush, &mut bs.stashed_brush);
            std::mem::swap(&mut bs.active_preset, &mut bs.stashed_preset);
            bs.brush.brush_options.color = color;
            bs.eraser_active = eraser;
            bs.brush.is_changed = true;
            bs.brush_preview.dirty = true;
        }
        bs.brush.brush_options.blend_mode = if eraser {
            BlendMode::Eraser
        } else {
            BlendMode::Normal
        };
    }

    pub(crate) fn is_eraser_active(&self) -> bool {
        matches!(self.active_tool, Tool::Brush) && self.brush_state.eraser_active
    }

    /// Load a preset into the tool it belongs to (eraser presets into the
    /// eraser, the rest into the brush) and switch to that tool.
    pub(crate) fn apply_preset(&mut self, index: usize) {
        let Some(preset) = self.brush_state.presets.get(index).cloned() else {
            return;
        };
        let eraser = preset.brush.brush_options.blend_mode == BlendMode::Eraser;
        self.set_brush_tool(eraser);
        let bs = &mut self.brush_state;
        let color = bs.brush.brush_options.color;
        bs.brush = preset.brush;
        bs.brush.brush_options.color = color;
        bs.brush.is_changed = true;
        bs.brush_preview.dirty = true;
        bs.active_preset = Some(preset.name);
    }

    pub(crate) fn set_select_tool(&mut self, kind: SelectionType) {
        self.active_tool = Tool::Select(kind);
        self.workspace.select_type = kind;
    }

    pub(crate) fn set_transform_tool(&mut self) {
        if !matches!(self.active_tool, Tool::Transform(_)) {
            self.active_tool = Tool::Transform(TransformInfo::default());
        }
    }

    /// Swap the brush color with the secondary color.
    pub(crate) fn swap_colors(&mut self) {
        let brush_color = &mut self.brush_state.brush.brush_options.color;
        std::mem::swap(brush_color, &mut self.brush_state.secondary_color);
        self.brush_state.brush_preview.dirty = true;
    }

    /// Multiply the brush diameter by `factor`, within the slider's range.
    pub(crate) fn scale_brush_size(&mut self, factor: f32) {
        // Tools with their own brush size.
        match self.active_tool {
            Tool::Liquify => {
                let r = &mut self.workspace.liquify.radius;
                *r = (*r * factor).clamp(4.0, 600.0);
                return;
            }
            Tool::Select(crate::selection::SelectionType::Brush) => {
                let r = &mut self.selection_manager.brush_radius;
                *r = (*r * factor).clamp(1.0, 500.0);
                return;
            }
            _ => {}
        }
        let options = &mut self.brush_state.brush.brush_options;
        options.diameter = (options.diameter * factor).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
        self.brush_state.brush.is_changed = true;
    }
}

/// Handle app-wide shortcuts. Returns whether a repaint is needed.
pub(crate) fn handle_shortcuts(app: &mut PainterApp, ctx: &egui::Context) -> bool {
    // Let text fields keep their keys (typing a layer name must not
    // switch tools).
    if ctx.wants_keyboard_input() {
        return false;
    }

    let cmd = Modifiers::COMMAND;
    let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
    let none = Modifiers::NONE;
    // `consume_key` matches shift loosely, so check the shifted combos first.
    let pressed = |mods: Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(mods, key));

    let redo = pressed(cmd_shift, Key::Z) || pressed(cmd, Key::Y);
    let undo = !redo && pressed(cmd, Key::Z);
    let new_layer = pressed(cmd_shift, Key::N);
    let new_folder = pressed(cmd, Key::G);
    let new_canvas = !new_layer && pressed(cmd, Key::N);
    let import = pressed(cmd_shift, Key::O);
    let open = !import && pressed(cmd, Key::O);
    let save = pressed(cmd, Key::S);
    let export = pressed(cmd, Key::E);
    let fit = pressed(cmd, Key::Num0);
    let actual = pressed(cmd, Key::Num1);
    let zoom_in = pressed(cmd, Key::Equals) || pressed(cmd, Key::Plus);
    let zoom_out = pressed(cmd, Key::Minus);
    let invert = pressed(cmd_shift, Key::I);
    let select_all = pressed(cmd, Key::A);
    let deselect = pressed(cmd, Key::D) || pressed(none, Key::Escape);
    let brush = pressed(none, Key::B);
    let eraser = pressed(none, Key::E);
    let select_rect = pressed(none, Key::M);
    let lasso = pressed(none, Key::L);
    let wand = pressed(none, Key::Q);
    let remove_anchor = pressed(none, Key::Backspace);
    let transform = pressed(none, Key::V) || pressed(none, Key::T);
    let eyedropper = pressed(none, Key::I);
    let fill = pressed(none, Key::G);
    let alpha_lock = pressed(none, Key::Slash);
    let liquify = pressed(none, Key::W);
    let blend = pressed(none, Key::S);
    let swap = pressed(none, Key::X);
    let panels = pressed(none, Key::Tab);
    let presets = pressed(none, Key::P);
    let smaller = pressed(none, Key::OpenBracket);
    let bigger = pressed(none, Key::CloseBracket);

    let mut repaint = false;

    if undo || redo {
        app.apply_history(redo);
        repaint = true;
    }
    if new_folder {
        app.add_folder();
        repaint = true;
    }
    if new_layer {
        app.add_layer_and_select();
        repaint = true;
    }
    if new_canvas {
        ui::top_bar::open_new_canvas_dialog(app);
    }
    if import {
        crate::app::import::import_image_dialog(app);
        repaint = true;
    }
    if open {
        ui::top_bar::open_project(app);
        repaint = true;
    }
    if save {
        ui::top_bar::save_project(app);
    }
    if export {
        ui::top_bar::open_export_dialog(app);
    }
    if fit {
        app.fit_view();
        repaint = true;
    }
    if actual {
        app.set_zoom_from_center(1.0);
        repaint = true;
    }
    if zoom_in {
        app.zoom_by_from_center(1.25);
        repaint = true;
    }
    if zoom_out {
        app.zoom_by_from_center(0.8);
        repaint = true;
    }
    if deselect {
        // Esc during a transform cancels it rather than deselecting.
        if app.layer_state.floating_layer_idx.is_some() {
            crate::app::transform::cancel_floating_layer(app);
        } else if app.layer_state.liquify.is_some() {
            app.liquify_cancel();
        } else if app.selection_manager.is_dragging || app.workspace.select.magnetic.is_some() {
            app.select_cancel();
        } else {
            app.deselect();
        }
        app.modal_state.select_menu_open = false;
        repaint = true;
    }
    if remove_anchor && app.workspace.select.magnetic.is_some() {
        app.magnetic_undo_anchor();
        repaint = true;
    }
    if select_all {
        app.select_all();
        repaint = true;
    }
    if invert {
        app.invert_selection();
        repaint = true;
    }
    if alpha_lock {
        let idx = app.canvas.active_layer_idx;
        if app
            .canvas
            .layers
            .get(idx)
            .is_some_and(|l| l.kind == crate::canvas::storage::LayerKind::Paint)
        {
            let layer = &mut app.canvas_mut().layers[idx];
            layer.alpha_locked = !layer.alpha_locked;
            repaint = true;
        }
    }
    if presets {
        app.brush_state.show_presets = !app.brush_state.show_presets;
        repaint = true;
    }
    if panels {
        ui::top_bar::toggle_panels(app);
        repaint = true;
    }
    if swap {
        app.swap_colors();
        repaint = true;
    }
    if smaller {
        app.scale_brush_size(1.0 / SIZE_STEP);
        repaint = true;
    }
    if bigger {
        app.scale_brush_size(SIZE_STEP);
        repaint = true;
    }

    if !app.is_mid_gesture() {
        if brush {
            app.set_brush_tool(false);
        }
        if eraser {
            app.set_brush_tool(true);
        }
        if select_rect {
            // Pressing M again toggles between rectangle and ellipse.
            let kind = match app.active_tool {
                Tool::Select(SelectionType::Rectangle) => SelectionType::Circle,
                _ => SelectionType::Rectangle,
            };
            app.set_select_tool(kind);
        }
        if lasso {
            // Pressing L again toggles freehand / magnetic.
            let kind = match app.active_tool {
                Tool::Select(SelectionType::Lasso) => SelectionType::Magnetic,
                _ => SelectionType::Lasso,
            };
            app.set_select_tool(kind);
        }
        if wand {
            // Pressing Q again toggles magic wand / colour range.
            let kind = match app.active_tool {
                Tool::Select(SelectionType::Wand) => SelectionType::ColorRange,
                _ => SelectionType::Wand,
            };
            app.set_select_tool(kind);
        }
        if transform {
            app.set_transform_tool();
        }
        if eyedropper {
            app.active_tool = Tool::Eyedropper;
        }
        if fill {
            // Pressing G again toggles bucket / enclose.
            if matches!(app.active_tool, Tool::Fill) {
                use crate::app::fill_tool::FillMode;
                let f = &mut app.workspace.fill;
                f.mode = if f.mode == FillMode::Bucket {
                    FillMode::Enclose
                } else {
                    FillMode::Bucket
                };
            }
            app.active_tool = Tool::Fill;
        }
        if liquify {
            app.active_tool = Tool::Liquify;
        }
        if blend {
            // S again switches smudge / blur.
            let smudge = !matches!(app.active_tool, Tool::Smudge);
            app.set_blend_tool(smudge);
        }
        repaint |= brush
            || eraser
            || select_rect
            || lasso
            || transform
            || eyedropper
            || fill
            || liquify
            || blend;
    }

    repaint
}
