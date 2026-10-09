//! Keyboard shortcuts, plus the tool/color actions they share with the
//! toolbar and menus.

use crate::app::PainterApp;
use crate::app::tools::Tool;
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
            // The stabiliser is the artist's: it stays with the hand, not
            // with the brush or eraser.
            let b = &bs.brush;
            let stabilizer = (
                b.stabilizer_algorithm,
                b.stabilizer,
                b.stabilizer_mass,
                b.stabilizer_drag,
                b.stabilizer_modes,
            );
            std::mem::swap(&mut bs.brush, &mut bs.stashed_brush);
            std::mem::swap(&mut bs.active_preset, &mut bs.stashed_preset);
            bs.brush.brush_options.color = color;
            let b = &mut bs.brush;
            (
                b.stabilizer_algorithm,
                b.stabilizer,
                b.stabilizer_mass,
                b.stabilizer_drag,
                b.stabilizer_modes,
            ) = stabilizer;
            crate::ui::widgets::new_slider_defaults();
            bs.eraser_active = eraser;
            bs.brush.is_changed = true;
            bs.brush_preview.dirty = true;
        }
        // (A brush painting behind keeps doing so.)
        let mode = &mut bs.brush.brush_options.blend_mode;
        if eraser {
            *mode = BlendMode::Eraser;
        } else if *mode == BlendMode::Eraser {
            *mode = BlendMode::Normal;
        }
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
        // Erasing, any brush erases: picking one keeps the eraser on, with
        // that brush's tip, texture and dynamics. An eraser preset picked
        // while painting switches to erasing.
        let eraser = self.brush_state.eraser_active
            || preset.brush.brush_options.blend_mode == BlendMode::Eraser;
        // The stabiliser is the artist's, not the brush's: a preset (or the
        // switch between brush and eraser) keeps it.
        let b = &self.brush_state.brush;
        let stabilizer = (
            b.stabilizer_algorithm,
            b.stabilizer,
            b.stabilizer_mass,
            b.stabilizer_drag,
            b.stabilizer_modes,
        );
        self.set_brush_tool(eraser);
        let bs = &mut self.brush_state;
        let color = bs.brush.brush_options.color;
        bs.brush = preset.brush;
        bs.brush.brush_options.color = color;
        if eraser {
            bs.brush.brush_options.blend_mode = BlendMode::Eraser;
        }
        let b = &mut bs.brush;
        (
            b.stabilizer_algorithm,
            b.stabilizer,
            b.stabilizer_mass,
            b.stabilizer_drag,
            b.stabilizer_modes,
        ) = stabilizer;
        bs.brush.is_changed = true;
        bs.brush_preview.dirty = true;
        bs.library.file.remember(&preset.name);
        bs.library.dirty = true;
        bs.active_preset = Some(preset.name);
        // Double-clicking a slider now returns it to this preset's value.
        crate::ui::widgets::new_slider_defaults();
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
    // A filter dialog takes Enter and Esc; nothing else may change the
    // layer under its preview.
    if app.workspace.filter.session.is_some() {
        return false;
    }
    // The shortcuts window is waiting for a new shortcut's keys.
    if app.workspace.recording_shortcut.is_some() {
        return false;
    }
    // The pop-up palette takes Esc and its own key while it's open.
    if app.brush_state.library.radial.is_some() {
        return ui::radial_palette::palette_keys(app, ctx);
    }
    use crate::app::input::keymap::Action;
    // Copying and pasting frames, over the timeline.
    if ui::timeline::timeline_keys(app, ctx) {
        return true;
    }
    // Before the keys below consume V (the Transform tool).
    let clipboard = app.clipboard_keys(ctx);

    let mut actions = std::mem::take(&mut app.workspace.jobs.deferred_actions);
    actions.extend(app.workspace.keymap.take_actions(ctx));
    // Strokes still being painted: the shortcuts wait for them (a frame or
    // a few) instead of the frame waiting.
    if !actions.is_empty() && app.strokes_settling() {
        app.workspace.jobs.deferred_actions = actions;
        ctx.request_repaint();
        return clipboard;
    }
    let on = |a: Action| actions.contains(&a);
    // Esc isn't a shortcut to change: it cancels what's in progress.
    // (An open right-click menu takes Esc to close.)
    let escape = !ctx.is_context_menu_open()
        && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));

    let redo = on(Action::Redo);
    let undo = !redo && on(Action::Undo);
    let history = on(Action::History);
    let new_layer = on(Action::NewLayer);
    let clip = on(Action::ClipToBelow);
    let new_folder = on(Action::NewFolder);
    let duplicate = on(Action::DuplicateLayer);
    let new_canvas = on(Action::NewCanvas);
    let import = on(Action::Import);
    let open = on(Action::Open);
    let save = on(Action::Save);
    let merge_visible = on(Action::MergeVisible);
    let merge_down = on(Action::MergeDown);
    let export = on(Action::Export);
    let fit = on(Action::FitView);
    let previous_frame = on(Action::PreviousFrame);
    let next_frame = on(Action::NextFrame);
    let play = on(Action::PlayAnimation);
    let animate_tool = on(Action::Animate);
    let animation: Vec<Action> = [
        Action::FirstFrame,
        Action::LastFrame,
        Action::PreviousDrawing,
        Action::NextDrawing,
        Action::NewDrawing,
        Action::CopyDrawing,
        Action::RemoveDrawing,
        Action::HoldLonger,
        Action::HoldShorter,
        Action::ToggleOnion,
        Action::KeyMotion,
        Action::NewAnimationLayer,
    ]
    .into_iter()
    .filter(|&a| on(a))
    .collect();
    let actual = on(Action::ActualPixels);
    let zoom_in = on(Action::ZoomIn);
    let zoom_out = on(Action::ZoomOut);
    let invert = on(Action::InvertSelection);
    let select_all = on(Action::SelectAll);
    let deselect = on(Action::Deselect) || escape;
    let brush = on(Action::Brush);
    let eraser = on(Action::Eraser);
    let select_rect = on(Action::RectSelect);
    let lasso = on(Action::Lasso);
    let quick_mask = on(Action::QuickMask);
    let wand = on(Action::Wand);
    let flip = on(Action::FlipView);
    let content_fill = on(Action::ContentFill);
    let gradient = on(Action::Gradient);
    let shapes = on(Action::Shapes);
    let ruler = on(Action::Ruler);
    let delete = on(Action::DeletePixels);
    let transform = on(Action::Transform);
    let eyedropper = on(Action::Eyedropper);
    let fill = on(Action::Fill);
    let alpha_lock = on(Action::AlphaLock);
    let liquify = on(Action::Liquify);
    let blend = on(Action::Blend);
    let swap = on(Action::SwapColors);
    let panels = on(Action::TogglePanels);
    let presets = on(Action::Presets);
    let palette = on(Action::Palette);
    let smaller = on(Action::SmallerBrush);
    let bigger = on(Action::BiggerBrush);
    let grid = on(Action::Grid);
    let guides = on(Action::Guides);

    let mut repaint = clipboard;
    if duplicate {
        app.duplicate_layer();
        repaint = true;
    }

    if undo || redo {
        app.apply_history(redo);
        repaint = true;
    }
    if history {
        let show = &mut app.modal_state.show_history;
        *show = !*show;
        repaint = true;
    }
    if new_folder {
        app.add_folder();
        repaint = true;
    }
    if clip {
        app.toggle_clip_active();
        repaint = true;
    }
    if new_layer {
        app.add_layer_and_select();
        repaint = true;
    }
    if merge_down {
        app.merge_down();
        repaint = true;
    }
    if merge_visible {
        app.merge_visible();
        repaint = true;
    }
    if new_canvas {
        ui::menus::open_new_canvas_dialog(app);
    }
    if import {
        crate::app::import::import_image_dialog(app);
        repaint = true;
    }
    if open {
        ui::menus::open_project(app);
        repaint = true;
    }
    if save {
        ui::menus::save_project(app);
    }
    if export {
        ui::menus::open_export_dialog(app);
    }
    if fit {
        app.fit_view();
        repaint = true;
    }
    if previous_frame || next_frame {
        let (t, timeline) = (app.canvas.time, app.canvas.timeline);
        app.go_to_frame(if next_frame {
            timeline.next(t)
        } else {
            timeline.previous(t)
        });
        repaint = true;
    }
    if play {
        app.workspace.animation.playing = !app.workspace.animation.playing;
        repaint = true;
    }
    if animate_tool {
        app.active_tool = Tool::Animate;
        app.workspace.animation.show_timeline = true;
        repaint = true;
    }
    for action in animation {
        app.animation_shortcut(action);
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
        if crate::app::tools::transform::transform_running(app) {
            crate::app::tools::transform::cancel_floating_layer(app);
        } else if app.layer_state.liquify.is_some() {
            app.liquify_cancel();
        } else if app.workspace.shapes.session.is_some() {
            app.shape_cancel();
        } else if app.workspace.gradient.session.is_some() {
            app.gradient_cancel();
        } else if app.selection_manager.is_dragging || app.workspace.select.magnetic.is_some() {
            app.select_cancel();
        } else {
            app.deselect();
        }
        app.modal_state.select_menu_open = false;
        app.modal_state.symmetry_menu_open = false;
        repaint = true;
    }
    if content_fill {
        app.content_aware_fill();
        repaint = true;
    }
    if quick_mask {
        app.toggle_quick_mask();
        repaint = true;
    }
    if flip {
        app.viewport.flip_x = !app.viewport.flip_x;
        repaint = true;
    }
    if grid {
        let grid = &mut app.workspace.view_aids.grid;
        grid.show = !grid.show;
        repaint = true;
    }
    if guides {
        let guides = &mut app.workspace.view_aids.guides;
        guides.show = !guides.show;
        repaint = true;
    }
    if delete && app.workspace.select.magnetic.is_some() {
        app.magnetic_undo_anchor();
        repaint = true;
    } else if delete
        && matches!(app.active_tool, crate::app::tools::Tool::VectorEdit)
        && app.line_edit_delete()
    {
        repaint = true;
    } else if delete && app.workspace.shapes.session.is_some() {
        app.shape_undo_point();
        repaint = true;
    } else if delete
        && (app.layer_state.floating_layer_idx.is_none() || app.selection_manager.has_selection())
    {
        // Delete (or Backspace, the Mac delete key) erases the selected
        // pixels (mid-transform too: it ends it), or with nothing selected
        // deletes the selected layer; a whole layer being transformed is
        // left alone.
        if app.selection_manager.has_selection() {
            app.delete_selection_contents();
        } else {
            app.delete_selected_layer();
        }
        repaint = true;
    }
    if ruler {
        let on = !app.workspace.guides.ruler.enabled;
        app.set_ruler(on);
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
    if palette {
        let pos = ctx.input(|i| i.pointer.latest_pos());
        app.open_radial_palette(pos.unwrap_or(ctx.screen_rect().center()), true);
        repaint = true;
    }
    if panels {
        ui::menus::toggle_panels(app);
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
            // Pressing L again steps freehand → polygon → magnetic.
            let kind = match app.active_tool {
                Tool::Select(SelectionType::Lasso) => SelectionType::Polygon,
                Tool::Select(SelectionType::Polygon) => SelectionType::Magnetic,
                _ => SelectionType::Lasso,
            };
            app.set_select_tool(kind);
        }
        if gradient {
            app.active_tool = Tool::Gradient;
        }
        if shapes {
            // Pressing U again goes to the next shape.
            let kind = match app.active_tool {
                Tool::Shape(kind) => kind.next(),
                _ => app.workspace.shapes.last_kind,
            };
            app.set_shape_tool(kind);
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
            // Pressing G again goes to the next mode (bucket, enclose,
            // lasso delete).
            if matches!(app.active_tool, Tool::Fill) {
                let f = &mut app.workspace.fill;
                f.mode = f.mode.next();
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

#[cfg(test)]
mod preset_tests {
    use crate::brush_engine::brush::StabilizerAlgorithm;
    use crate::brush_engine::brush_options::BlendMode;
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    #[test]
    fn any_brush_erases_while_erasing() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.canvas_mut()
            .set_layer_tile_data(1, 0, 0, vec![Color32::RED; 64 * 64]);
        app.brush_state.presets = crate::PainterApp::default_brush_presets();
        let index = |app: &crate::PainterApp, name: &str| {
            app.brush_state
                .presets
                .iter()
                .position(|p| p.name == name)
                .unwrap()
        };
        app.set_brush_tool(true);
        let chalk = index(&app, "Chalk");
        app.apply_preset(chalk);
        assert!(app.is_eraser_active(), "picking a brush keeps the eraser");
        let b = &app.brush_state.brush;
        assert_eq!(b.brush_options.blend_mode, BlendMode::Eraser);
        assert!(b.texture.is_some(), "with the chalk's texture");
        assert_eq!(app.brush_state.active_preset.as_deref(), Some("Chalk"));
        // It erases (with the grain: not every pixel fully).
        app.brush_state.brush.brush_options.diameter = 20.0;
        app.start_stroke_with_pressure(Vec2::new(10.0, 32.0), 1.0);
        for x in 11..54 {
            app.add_stroke_point(Vec2::new(x as f32, 32.0), 1.0);
        }
        app.finish_stroke();
        app.release_canvas();
        let tile = app.canvas.get_layer_tile_data(1, 0, 0).unwrap();
        assert!(tile[32 * 64 + 32].a() < 255, "erased");
        assert_eq!(tile[2 * 64 + 32], Color32::RED, "away from the stroke");
        // Back to painting: the brush's own brush, painting again.
        app.set_brush_tool(false);
        assert!(!app.is_eraser_active());
        assert_eq!(
            app.brush_state.brush.brush_options.blend_mode,
            BlendMode::Normal
        );
        // An eraser preset picked while painting switches to erasing.
        let soft = index(&app, "Eraser (Soft)");
        app.apply_preset(soft);
        assert!(app.is_eraser_active());
    }

    #[test]
    fn a_preset_keeps_the_stabiliser() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.brush_state.presets = crate::PainterApp::default_brush_presets();
        let b = &mut app.brush_state.brush;
        b.stabilizer_algorithm = StabilizerAlgorithm::Dynamic;
        b.stabilizer = 0.7;
        b.stabilizer_mass = 0.3;
        b.stabilizer_drag = 0.4;
        b.stabilizer_modes.string_length = 90.0;
        for i in 0..app.brush_state.presets.len() {
            app.apply_preset(i);
            let b = &app.brush_state.brush;
            let name = &app.brush_state.presets[i].name;
            assert_eq!(
                b.stabilizer_algorithm,
                StabilizerAlgorithm::Dynamic,
                "{name}"
            );
            assert_eq!(
                (b.stabilizer, b.stabilizer_mass, b.stabilizer_drag),
                (0.7, 0.3, 0.4),
                "{name}"
            );
            assert_eq!(b.stabilizer_modes.string_length, 90.0, "{name}");
        }
    }
}
