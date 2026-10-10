//! `PainterApp` and its frame loop: `update` runs the
//! frame as a fixed sequence of stages (setup, chrome, canvas, tools,
//! pixels, windows), each a method below it.

use crate::app::frame_stats::Stage;
use crate::app::input;
use crate::app::stroke_ops::exclusive;
use crate::app::view::render;
use crate::app::{
    layout,
    state::{
        BrushState, ExportState, LayerState, ModalState, RenderCache, ViewportState, WorkspaceState,
    },
};
use crate::brush_engine::stroke_worker::StrokeWorker;
use crate::{canvas::Canvas, tablet::TabletInput, ui};
use eframe::egui;
use eframe::egui::{Color32, Vec2};
use std::sync::Arc;

use crate::selection::SelectionManager;

/// Share of a frame the stroke worker may take to paint this frame's dabs
/// before the frame goes on without them (they show next frame).
const STROKE_FRAME_SHARE: f32 = 0.4;
/// The budget before the refresh rate is known, and its bounds (ms).
const STROKE_BUDGET_MS: (f32, std::ops::RangeInclusive<f32>) = (5.0, 1.0..=6.0);

/// Main egui application that owns the canvas, brush state, UI and rendering caches.
pub struct PainterApp {
    /// Shared with the stroke worker while a stroke is painted; use
    /// [`PainterApp::canvas_mut`] for exclusive access.
    pub(crate) canvas: Arc<Canvas>,
    pub(crate) stroke_worker: StrokeWorker,

    // Grouped state
    pub(crate) brush_state: BrushState,
    pub(crate) viewport: ViewportState,
    pub(crate) render_cache: RenderCache,
    pub(crate) layer_state: LayerState,
    pub(crate) modal_state: ModalState,
    pub(crate) export_state: ExportState,
    pub(crate) workspace: WorkspaceState,

    // Standalone components
    pub(crate) active_tool: crate::app::tools::Tool,
    pub(crate) selection_manager: SelectionManager,
    pub(crate) tablet: Option<TabletInput>,
}

impl eframe::App for PainterApp {
    /// Android: the soft keyboard's typing (NativeActivity gets none).
    #[cfg(target_os = "android")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let typed = crate::android::keyboard_input(ctx.wants_keyboard_input());
        raw_input.events.extend(typed);
    }

    fn on_exit(&mut self) {
        // Closed while starting: what's on disk wasn't read yet, so
        // nothing here may be written over it.
        if self.workspace.loading.is_some() {
            return;
        }
        self.save_active_preset(false);
        self.save_settings(false);
        self.finish_jobs_on_exit();
        self.autosave_on_exit();
        // The settings and presets just written: on disk before quitting.
        crate::app::jobs::flush_writes();
    }

    /// Handle UI, input, painting updates, and tile uploads each frame.
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Start-up: a step a frame behind the splash (none on the first
        // frame, so the window shows the splash at once).
        if self.workspace.loading.is_some() {
            if ctx.cumulative_pass_nr() > 0 {
                self.run_load_step(ctx);
            }
            if let Some(step) = self.workspace.loading {
                ui::splash::splash(ctx, step);
                ctx.request_repaint();
                return;
            }
        }
        self.workspace.frame_stats.begin(frame.info().cpu_usage);
        self.refresh_display_transform();
        self.animation_tick(ctx);
        self.workspace.refresh.tick();
        self.workspace.keyboard.observe(ctx);
        let label = self
            .active_tool
            .history_label(self.brush_state.eraser_active);
        self.layer_state.history.set_tool_label(label);
        let mut needs_repaint = false;
        // Last frame's dropped textures, now safe to free.
        self.workspace.retired_textures.clear();

        // 1. Frame setup: theme, panel sizes, shortcuts, finished background work.
        let touch = self.workspace.touch_mode;
        self.apply_touch_mode(ctx, touch);
        ui::style::set_touch_metrics(ctx, touch);
        let screen_size = ctx.screen_rect().size();
        let resized = self
            .workspace
            .screen_size
            .is_some_and(|prev| prev != screen_size);
        self.workspace.screen_size = Some(screen_size);
        self.selection_manager.canvas_size = [self.canvas.width(), self.canvas.height()];
        self.keep_guides_on_canvas();

        self.poll_file_pick();
        self.poll_export();
        self.poll_jobs(ctx);
        // The library takes the window: no canvas, bars or shortcuts.
        if self.workspace.library.open {
            ui::library::library_screen(self, ctx);
            ui::canvas_creation::canvas_creation_modal(self, ctx);
            ui::library::library_dialogs(self, ctx);
            crate::app::autosave::recovery_dialog(self, ctx);
            self.autosave_tick(ctx);
            self.save_settings(false);
            // The pen's samples are the canvas's: taps on the library (the
            // pen is the pointer there) mustn't paint once a canvas opens.
            if let Some(tablet) = &mut self.tablet {
                tablet.poll(ctx);
            }
            return;
        }

        if crate::app::input::shortcuts::handle_shortcuts(self, ctx) {
            needs_repaint = true;
        }

        ui::layers::refresh_thumbnails(self, ctx);
        self.workspace.frame_stats.mark(Stage::Setup);

        // 2. Chrome. Bars first so they span the full window width; the tool strip is
        // added before the docks so it sits at the far left.
        // Tablets have the menus in a sheet over the bottom bar, and adjust
        // size/opacity with sliders on the top bar instead of the tool options.
        ui::menus::top_bar(self, ctx);
        if touch {
            ui::menus::menu_sheet(self, ctx);
            layout::show_dropdowns(self, ctx);
        }
        ui::toolbar::toolbar(self, ctx);
        layout::right_rail(self, ctx);
        layout::show_panels(self, ctx);
        ui::timeline::timeline_panel(self, ctx);
        self.workspace.frame_stats.mark(Stage::Panels);

        let canvas_frame = egui::Frame::none().fill(ui::style::BG_CANVAS);
        egui::CentralPanel::default()
            .frame(canvas_frame)
            .show(ctx, |ui| {
                // 3. Canvas: place and draw the view, gather pen/touch input.
                self.place_view(ui, resized);

                let view = render::draw_canvas(self, ui);
                self.viewport.canvas_area = Some(view.response.rect);
                // Pen samples first: touch handling needs to know the pen is
                // down to tell a resting palm from a finger.
                let pen = self
                    .tablet
                    .as_mut()
                    .map(|t| t.poll(ctx))
                    .unwrap_or_default();
                if let Some(calibration) = &mut self.workspace.calibration {
                    calibration.record(&pen);
                }
                self.viewport.touch.pen_active =
                    !pen.is_empty() || self.tablet.as_ref().is_some_and(|t| t.pen_active());
                self.viewport.touch.pen_is_pointer =
                    self.tablet.as_ref().is_some_and(|t| t.pen_is_pointer());
                if crate::app::input::touch::handle_touch(self, ctx, &view.response) {
                    needs_repaint = true;
                }
                if !view.response.hovered() {
                    self.viewport.cursor_canvas = None;
                }
                self.draw_pointer_hints(ctx, ui, &view);

                // 4. Tools: end sessions of tools left, then route this
                // frame's input to the active tool.
                self.import_dropped_files(ctx);
                self.settle_tool_sessions(ctx, &view.response);
                // A tap that closes a floating panel paints nothing.
                let closing = layout::close_floating_panels(self, ctx, &view.response);
                // A content-aware fill in progress: the canvas waits for it.
                if self.poll_patch() {
                    ctx.set_cursor_icon(egui::CursorIcon::Progress);
                    needs_repaint = true;
                } else if self.workspace.jobs.blocking_label().is_some() {
                    // A document is being opened or saved: no painting
                    // into what's about to be replaced.
                    ctx.set_cursor_icon(egui::CursorIcon::Progress);
                } else if closing || self.workspace.filter.session.is_some() {
                    // A filter dialog is open: the layer shows its preview.
                } else {
                    input::handle_input(
                        self,
                        ctx,
                        &view.response,
                        view.origin,
                        view.canvas_center,
                        &pen,
                    );
                }
                // Overlay first: on a drag's first frame it takes over at once,
                // so the layer isn't CPU-rendered even once while dragging.
                crate::app::tools::transform::update_float_overlay(self, ctx);
                crate::app::tools::transform::flush_transform_preview(self);
                // The gradient repaints at most once a frame while dragged,
                // and only once its last repaint is all on screen.
                self.gradient_update();
                self.quickshape_tick();
                self.text_update();
                let dragging = ctx.input(|i| i.pointer.any_down());
                let screen = render::preview_view(self, &view, ui.clip_rect());
                self.filter_update(dragging.then_some(&screen));
                // Twirl / pinch / bloat keep working while the brush is held.
                if self.liquify_is_holding() && self.workspace.liquify.mode.is_continuous() {
                    let dt = ctx.input(|i| i.stable_dt).min(0.1);
                    self.liquify_hold(dt);
                    needs_repaint = true;
                }
                // This frame's liquify dabs: into the layer, or zoomed out
                // with the pen down, a screen-sized preview.
                self.liquify_frame(&screen);
                self.workspace.frame_stats.mark(Stage::Tools);
                // 5. Pixels: let the stroke worker catch up, upload dirty
                // tiles, paint the canvas, then the overlays on top.
                // Give the stroke worker a short budget to paint this frame's
                // samples so they usually show this frame; a heavy brush can't
                // stall the frame beyond it and simply shows up next frame.
                if self.brush_state.is_drawing || self.brush_state.blend_stroke.is_some() {
                    self.stroke_worker.wait_idle_for(self.stroke_frame_budget());
                }
                if self.sync_stroke_worker() {
                    needs_repaint = true;
                }
                // An airbrush paints while the pen is held still, with no
                // input to wake the frame loop.
                if self.brush_state.is_drawing && self.brush_state.brush.airbrush_rate > 0.0 {
                    needs_repaint = true;
                }
                if self.render_cache.tiles.iter().any(|t| t.dirty) {
                    self.layer_state.thumbnails_dirty = true;
                    self.workspace.view_aids.navigator.dirty = true;
                }
                self.workspace.frame_stats.mark(Stage::Stroke);
                // Shader layers: compile, advance their clocks, and split
                // the stack for the ones showing live.
                let pointer = self
                    .viewport
                    .cursor_canvas
                    .map(|p| (p, ctx.input(|i| i.pointer.primary_down())));
                if self.shader_tick(ctx, pointer) {
                    needs_repaint = true;
                }
                if self.wet_tick() {
                    needs_repaint = true;
                }
                // Composite and paint after input, so this frame's dabs and any
                // pan/zoom show up in this frame.
                let (uploads, more_tiles) =
                    render::update_dirty_textures(self, &view, ui.clip_rect());
                self.workspace.frame_stats.mark(Stage::Tiles);
                if more_tiles {
                    needs_repaint = true;
                }
                crate::app::tools::transform::float_overlay_uploaded(self, more_tiles);
                if self.gradient_uploaded(more_tiles) {
                    needs_repaint = true;
                }
                render::paint_canvas(self, ui, &view, uploads);

                if self.brush_state.is_drawing {
                    needs_repaint = true;
                }

                self.draw_overlays(ctx, ui, &view);
                ui::canvas_sliders::canvas_sliders(self, ctx, view.response.rect);
                layout::notices(self, ctx, view.response.rect);
                self.workspace.frame_stats.mark(Stage::Canvas);
            });

        // 6. Modals and floating windows.
        self.show_windows(ctx);
        self.show_job_progress(ctx);
        self.workspace.frame_stats.mark(Stage::Windows);
        if std::mem::take(&mut self.brush_state.swatches_dirty) {
            self.save_swatches();
        }
        self.save_view_settings(ctx);
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.save_active_preset(pointer_down);
        self.save_settings(pointer_down);
        if std::mem::take(&mut self.brush_state.library.dirty) {
            self.save_brush_library();
        }
        self.autosave_tick(ctx);
        self.timelapse_tick();

        // Single consolidated repaint request
        if needs_repaint {
            ctx.request_repaint();
        }
    }
}

/// Stages of `update`, in the order it runs them.
impl PainterApp {
    /// Rebuild the style when touch mode or the accent changes (and on
    /// the first frame).
    fn apply_touch_mode(&mut self, ctx: &egui::Context, touch: bool) {
        let wanted = (touch, self.workspace.accent);
        if self.workspace.applied_style != Some(wanted) {
            crate::ui::style::set_accent(wanted.1);
            crate::ui::theme::apply_style(ctx, touch);
            self.workspace.applied_style = Some(wanted);
        }
    }

    /// Finish a background export: report its result and progress.
    fn poll_export(&mut self) {
        // Progress first: the last update carries what to share.
        if let Some(rx) = &self.export_state.progress_rx {
            for update in rx.try_iter() {
                self.export_state.progress = update.progress;
                if update.share.is_some() {
                    self.export_state.share = update.share;
                }
                if let Some(msg) = update.message {
                    self.export_state.message = Some(msg);
                }
            }
        }
        if let Some(handle) = self.export_state.task.as_ref()
            && handle.is_finished()
        {
            let result = self
                .export_state
                .task
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("Export thread panicked".to_string()));
            self.export_state.in_progress = false;
            match result {
                Ok(msg) => {
                    self.export_state.message = Some(msg);
                    // Android keeps it open, to share what was saved.
                    if self.export_state.share.is_none() {
                        self.export_state.show_modal = false;
                    }
                }
                Err(err) => {
                    self.export_state.message = Some(err);
                }
            }
        }
    }

    /// Keep the canvas fitted while the window settles, and still on screen
    /// while panels slide or the window resizes.
    fn place_view(&mut self, ui: &egui::Ui, resized: bool) {
        // Keep the canvas fitted while the window settles (the window
        // manager may resize it after the first frame), until the user
        // moves the view themselves.
        let available = ui.available_size();
        if self.workspace.auto_fit && self.workspace.fitted_to != Some(available) {
            let canvas_w = self.canvas.width() as f32;
            let canvas_h = self.canvas.height() as f32;

            let zoom_x = available.x / canvas_w;
            let zoom_y = available.y / canvas_h;
            self.viewport.zoom = zoom_x.min(zoom_y) * 0.9; // 90% fit
            let canvas_size = egui::vec2(canvas_w, canvas_h) * self.viewport.zoom;
            let offset = (available - canvas_size) * 0.5;
            self.viewport.offset = Vec2 {
                x: offset.x,
                y: offset.y,
            };
            self.workspace.fitted_to = Some(available);
        }

        // Keep the canvas still on screen while a side panel slides
        // (the view offset is relative to the canvas area's corner),
        // and keep the view centered when the window is resized or
        // the screen rotates.
        let area = ui.max_rect();
        if let Some(prev) = self.viewport.canvas_area
            && !self.workspace.auto_fit
        {
            if resized {
                self.viewport.offset += (area.size() - prev.size()) * 0.5;
            } else {
                self.viewport.offset -= area.min - prev.min;
            }
        }
    }

    /// Cursor hints: a crosshair when picking colours, the tip's outline
    /// for the brush and eraser, a ring for the selection brush, liquify,
    /// smudge and blur.
    fn draw_pointer_hints(&self, ctx: &egui::Context, ui: &egui::Ui, view: &render::CanvasView) {
        let picking = matches!(
            self.active_tool,
            crate::app::tools::Tool::Eyedropper | crate::app::tools::Tool::Fill
        ) || (matches!(self.active_tool, crate::app::tools::Tool::Brush)
            && ctx.input(|i| i.modifiers.alt));
        if picking && view.response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        // Selection and liquify brushes: show their size under the pointer.
        let ring = match self.active_tool {
            crate::app::tools::Tool::Select(crate::selection::SelectionType::Brush) => {
                Some(self.selection_manager.brush_radius)
            }
            crate::app::tools::Tool::Liquify => Some(self.workspace.liquify.radius),
            crate::app::tools::Tool::Smudge | crate::app::tools::Tool::Blur => {
                Some(self.brush_state.brush.brush_options.diameter * 0.5)
            }
            _ => None,
        };
        // The brush and eraser: the tip's own outline, at its size.
        if matches!(self.active_tool, crate::app::tools::Tool::Brush)
            && !picking
            && let Some(pos) = view.response.hover_pos()
        {
            let map = render::screen_map(self, view);
            let brush = &self.brush_state.brush;
            if crate::app::view::brush_cursor::draw(ctx, ui.painter(), map, brush, pos) {
                ctx.set_cursor_icon(egui::CursorIcon::None);
            }
        }
        if let Some(radius) = ring
            && let Some(pos) = view.response.hover_pos()
        {
            let r = radius * self.viewport.zoom;
            let painter = ui.painter();
            painter.circle_stroke(pos, r, egui::Stroke::new(2.0_f32, egui::Color32::BLACK));
            painter.circle_stroke(pos, r, egui::Stroke::new(1.0_f32, egui::Color32::WHITE));
        }
    }

    /// Sessions end when their tool is left: a transform, gradient, shape
    /// or liquify is applied, a magnetic outline dropped. A double-click
    /// finishes a polygon or closes a magnetic outline.
    fn settle_tool_sessions(&mut self, ctx: &egui::Context, response: &egui::Response) {
        // Leaving the Transform tool applies the running transform.
        if crate::app::tools::transform::transform_running(self)
            && !matches!(self.active_tool, crate::app::tools::Tool::Transform(_))
        {
            crate::app::tools::transform::commit_floating_layer(self);
        }
        // Leaving the Gradient tool keeps the gradient; the Text tool, the text.
        if !matches!(self.active_tool, crate::app::tools::Tool::Gradient) {
            self.gradient_commit();
        }
        if !matches!(self.active_tool, crate::app::tools::Tool::Text) {
            self.text_commit();
        }
        // Leaving the Shape tool applies the shape; a double-click
        // finishes a polygon.
        if !matches!(self.active_tool, crate::app::tools::Tool::Shape(_)) {
            self.shape_commit();
        } else if response.hovered()
            && ctx.input(|i| {
                i.pointer
                    .button_double_clicked(egui::PointerButton::Primary)
            })
        {
            self.shape_finish_polygon();
        }
        // A magnetic outline in progress belongs to its tool; a
        // double-click closes it.
        let magnetic = matches!(
            self.active_tool,
            crate::app::tools::Tool::Select(
                crate::selection::SelectionType::Magnetic
                    | crate::selection::SelectionType::Polygon
            )
        );
        if !magnetic {
            self.workspace.select.magnetic = None;
        } else if response.hovered()
            && ctx.input(|i| {
                i.pointer
                    .button_double_clicked(egui::PointerButton::Primary)
            })
        {
            self.magnetic_close();
        }
        self.quick_mask_settle();
        // Likewise for liquify.
        if self.layer_state.liquify.is_some()
            && !matches!(self.active_tool, crate::app::tools::Tool::Liquify)
        {
            self.liquify_commit();
        }
    }

    /// Overlays drawn over the canvas; they follow it exactly (zoom, pan and
    /// rotation).
    fn draw_overlays(&mut self, ctx: &egui::Context, ui: &egui::Ui, view: &render::CanvasView) {
        // Overlays follow the canvas exactly (zoom, pan and rotation).
        let map = render::screen_map(self, view);
        let panel = view.response.rect;
        crate::app::playback::draw_cached_frame(self, ui.painter(), &|p| map.to_screen(p));
        crate::app::view::grid::draw_grid(self, ui.painter(), &map, panel);
        crate::app::view::guide_lines::draw_guide_lines(self, ctx, ui.painter(), &map, panel);
        // With Transform, the outline shows where the box puts it.
        match self.active_tool {
            crate::app::tools::Tool::Transform(info) => {
                let params = info.params();
                self.selection_manager
                    .draw_overlay(ui.painter(), map.zoom(), &|p| {
                        map.to_screen(params.forward(p))
                    });
            }
            _ => self
                .selection_manager
                .draw_overlay(ui.painter(), map.zoom(), &|p| map.to_screen(p)),
        }
        crate::app::tools::select::draw_magnetic(self, ui.painter(), &|p| map.to_screen(p));
        crate::app::tools::guides::draw_guides(self, ui.painter(), &map);
        crate::app::stroke_ops::draw_string(self, ui.painter(), &map);
        // (On a moved layer, shapes and gradients are on its own pixels.)
        crate::app::tools::shape::draw_shape(self, ui.painter(), &|p| {
            map.to_screen(self.to_canvas_space(p))
        });
        crate::app::tools::gradient::draw_gradient(self, ui.painter(), &|p| {
            map.to_screen(self.to_canvas_space(p))
        });
        crate::app::tools::animate::draw_animate(self, ui.painter(), &|p| map.to_screen(p));
        if matches!(self.active_tool, crate::app::tools::Tool::Animate)
            && let Some(canvas) = self.viewport.cursor_canvas
        {
            ctx.set_cursor_icon(self.animate_cursor(canvas, ctx.input(|i| i.modifiers.alt)));
        }
        crate::app::tools::vector::draw_line_edit(self, ui.painter(), &map);
        crate::app::tools::blend::draw_clone_source(self, ui.painter(), &|p| map.to_screen(p));
        // A hand over the guide handles: they can be dragged.
        if let Some(canvas) = self.viewport.cursor_canvas
            && (self.guides_dragging() || self.over_guide_handle(canvas))
        {
            ctx.set_cursor_icon(if self.guides_dragging() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }

        crate::app::tools::transform::draw_float_overlay(self, ui.painter(), &map);
        self.draw_transform_overlay(ui.painter(), &map);
        // Enclose-and-fill or lasso-delete lasso in progress (red when it
        // erases).
        if matches!(self.active_tool, crate::app::tools::Tool::Fill)
            && self.workspace.fill.path.len() > 1
        {
            let ink = if self.workspace.fill.mode == crate::app::tools::fill::FillMode::LassoDelete
            {
                egui::Color32::from_rgb(255, 110, 110)
            } else {
                egui::Color32::WHITE
            };
            let pts: Vec<egui::Pos2> = self
                .workspace
                .fill
                .path
                .iter()
                .map(|&p| map.to_screen(self.to_canvas_space(p)))
                .collect();
            let painter = ui.painter();
            painter.add(egui::Shape::line(
                pts.clone(),
                egui::Stroke::new(3.0_f32, egui::Color32::BLACK),
            ));
            painter.add(egui::Shape::line(
                pts.clone(),
                egui::Stroke::new(1.0_f32, ink),
            ));
            if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
                painter.line_segment(
                    [*a, *b],
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(110)),
                );
            }
        }
    }

    /// Modal dialogs and floating windows, over everything else.
    fn show_windows(&mut self, ctx: &egui::Context) {
        ui::canvas_creation::canvas_creation_modal(self, ctx);
        ui::general_settings::general_settings_modal(self, ctx);
        ui::palette_window::palette_window(self, ctx);
        ui::image_gallery::image_gallery(self, ctx);
        ui::general_settings::shortcuts_window(self, ctx);
        ui::history_panel::history_window(self, ctx);
        ui::brush_list::presets_window(self, ctx);
        ui::radial_palette::radial_palette(self, ctx);
        ui::export_modal::export_modal(self, ctx);
        ui::frame_times::frame_times_window(self, ctx);
        ui::gradient_editor::gradient_editor_window(self, ctx);
        ui::filter_dialog::filter_dialog(self, ctx);
        ui::filter_dialog::adjustment_dialog(self, ctx);
        ui::layer_style_dialog::fill_dialog(self, ctx);
        ui::layer_style_dialog::border_dialog(self, ctx);
        ui::layer_style_dialog::line_width_dialog(self, ctx);
        crate::app::autosave::recovery_dialog(self, ctx);
        ui::library::library_dialogs(self, ctx);
        ui::image_menu::size_dialog(self, ctx);
        ui::select_dialog::select_dialog(self, ctx);
        ui::text_dialog::text_dialog(self, ctx);
        ui::view_aids_menu::guides_window(self, ctx);
        ui::reference_window::reference_window(self, ctx);
        ui::navigator::navigator_window(self, ctx);
        ui::shader_editor::shader_editors(self, ctx);
    }
}

impl PainterApp {
    /// How long a frame waits for the stroke worker before drawing anyway:
    /// a share of one refresh, so a 240 Hz screen keeps its frame rate while
    /// a heavy brush paints.
    fn stroke_frame_budget(&self) -> std::time::Duration {
        let (default, bounds) = STROKE_BUDGET_MS;
        let ms = self.workspace.refresh.period_ms().map_or(default, |p| {
            (p * STROKE_FRAME_SHARE).clamp(*bounds.start(), *bounds.end())
        });
        std::time::Duration::from_secs_f32(ms / 1000.0)
    }

    /// Clear every layer to white. Not undoable.
    pub(crate) fn clear_canvas(&mut self) {
        self.canvas_mut().clear(Color32::WHITE);
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Undo (or redo) the last action, once the strokes queued are painted
    /// (at once if there are none; else on a later frame, in order).
    /// Mirror a structural change undo or redo applied (layer added,
    /// removed or moved) onto the per-layer side-car state (UI colors):
    /// `History` only has `&mut Canvas`, so it can't reach those.
    fn mirror_layer_op(&mut self, op: &crate::canvas::history::LayerHistoryOp, redo: bool) {
        use crate::canvas::history::LayerHistoryOp;
        match op {
            LayerHistoryOp::Added { index, .. } => {
                if redo {
                    self.insert_layer_state(*index);
                } else {
                    self.remove_layer_state(*index);
                }
            }
            op @ LayerHistoryOp::Removed { .. } => {
                // The layer plus whatever went with it (mask, folder
                // contents), at the positions actually touched.
                let indices = op.removed_indices();
                if redo {
                    self.remove_layer_states(&indices);
                } else {
                    self.insert_layer_states(&indices);
                }
            }
            LayerHistoryOp::Moved { from, to, .. } => {
                self.reorder_layer_state(*from, *to);
            }
            LayerHistoryOp::Document(_) => self.after_document_swap(),
            LayerHistoryOp::Replaced(swap) => {
                let (out, put) = swap
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .applied
                    .clone();
                self.replace_layer_states(&out, &put);
            }
            // Adds and moves, in the order they were applied (the canvas has
            // had them all, so it only matches after the last).
            LayerHistoryOp::Batch(ops) => {
                let colors = &mut self.layer_state.layer_ui_colors;
                for op in ops {
                    match *op {
                        LayerHistoryOp::Added { index, .. } if redo => {
                            colors.insert(index.min(colors.len()), Color32::from_gray(40));
                        }
                        LayerHistoryOp::Added { index, .. } if index < colors.len() => {
                            colors.remove(index);
                        }
                        LayerHistoryOp::Moved { from, to, .. } if from < colors.len() => {
                            let color = colors.remove(from);
                            colors.insert(to.min(colors.len()), color);
                        }
                        _ => {}
                    }
                }
            }
            LayerHistoryOp::Text { .. }
            | LayerHistoryOp::Vector { .. }
            | LayerHistoryOp::Height { .. }
            | LayerHistoryOp::Wet { .. } => {}
        }
    }

    pub(crate) fn apply_history(&mut self, redo: bool) {
        self.when_strokes_painted(move |app| app.apply_history_now(redo));
    }

    /// Undo (or redo) the last action on the active layer, now (waiting
    /// for the stroke worker if it's painting).
    pub(crate) fn apply_history_now(&mut self, redo: bool) {
        // Undo inside a magnetic outline takes back its last anchor.
        if self.workspace.select.magnetic.is_some() {
            if !redo {
                self.magnetic_undo_anchor();
            }
            return;
        }
        self.forget_last_pick();
        self.filter_cancel();
        self.text_commit();
        // A running liquify or transform session becomes a normal step first,
        // so undo takes it back and redo brings it again (cancelling it
        // outright left nothing to redo).
        if self.layer_state.liquify.is_some() {
            self.liquify_commit();
        }
        if crate::app::tools::transform::transform_running(self) {
            crate::app::tools::transform::commit_floating_layer(self);
        }
        // Finish any stroke first so it is in the history (and undoable).
        self.release_canvas();
        {
            let history = &mut self.layer_state.history;
            let (affected, layer_action) = if redo {
                history.redo(
                    exclusive(&mut self.canvas),
                    &mut self.selection_manager,
                    &mut self.active_tool,
                )
            } else {
                history.undo(
                    exclusive(&mut self.canvas),
                    &mut self.selection_manager,
                    &mut self.active_tool,
                )
            };

            // A structural change (layer added/removed/moved) also needs the
            // per-layer side-car state (UI colors) mirrored to match:
            // `History` only has `&mut Canvas`, so it can't reach those.
            if let Some(op) = &layer_action {
                self.mirror_layer_op(op, redo);
            }

            if layer_action.is_some() {
                self.mark_all_tiles_dirty();
            }
            // Moved layers show their restored pixels moved.
            if self.canvas.pose_motions() {
                self.mark_all_tiles_dirty();
            }
            for (tx, ty) in affected {
                // Off-canvas tiles (negative coordinates) aren't drawn.
                if tx < 0 || ty < 0 {
                    continue;
                }
                // (With the tiles a border spills into.)
                self.mark_tile_dirty(tx as usize, ty as usize);
            }

            // Reset transform tool state if active so it recalculates bounds
            // Only reset if the undo action didn't restore a transform state
            if let crate::app::tools::Tool::Transform(ref mut info) = self.active_tool
                && info.bounds.is_none()
                && info.rotation == 0.0
                && info.offset.x == 0.0
                && info.offset.y == 0.0
            {
                *info = crate::selection::transform::TransformInfo::default();
            }

            self.layer_state.thumbnails_dirty = true;
        }
    }
}

#[cfg(test)]
mod window_size_tests {
    use crate::canvas::Canvas;
    use eframe::egui::{self, Color32};

    fn run(ppp: f32) -> Vec<Vec<(egui::LayerId, egui::Vec2)>> {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.workspace.palette.open = true;
        app.modal_state.show_history = true;
        app.brush_state.show_presets = true;
        app.workspace.view_aids.guides.new_guide.open = true;
        app.workspace.view_aids.navigator.open = true;
        app.workspace.frame_stats.enabled = true;
        app.workspace.frame_stats.window_open = true;
        app.filter_open(crate::canvas::filters::Filter::Exposure { stops: 0.0 });
        let ctx = egui::Context::default();
        crate::ui::theme::apply_global_style(&ctx);
        let mut sizes: Vec<Vec<(egui::LayerId, egui::Vec2)>> = Vec::new();
        let mut targets: Vec<egui::Pos2> = Vec::new();
        for i in 0..120 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0 / ppp, 1000.0 / ppp),
                )),
                ..Default::default()
            };
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(ppp);
            // Wander over every window's rows.
            let p = targets
                .get(i % targets.len().max(1))
                .copied()
                .unwrap_or(egui::pos2(300.0, 300.0));
            input.events.push(egui::Event::PointerMoved(p));
            let _ = ctx.run(input, |ctx| app.show_windows(ctx));
            let frame = ctx.memory(|m| {
                let mut v: Vec<_> = m
                    .areas()
                    .visible_layer_ids()
                    .into_iter()
                    .filter_map(|l| m.area_rect(l.id).map(|r| (l, r)))
                    .collect();
                v.sort_by_key(|(l, _)| l.id.value());
                v
            });
            if i == 5 {
                for (_, r) in &frame {
                    for k in 0..12 {
                        let t = (k as f32 + 0.5) / 12.0;
                        targets.push(egui::pos2(
                            r.left() + r.width() * ((k * 7 % 12) as f32 / 12.0),
                            r.top() + r.height() * t,
                        ));
                    }
                }
            }
            sizes.push(frame.into_iter().map(|(l, r)| (l, r.size())).collect());
        }
        sizes
    }

    /// Every floating window keeps its size while the pointer moves over it
    /// (a row a pixel too wide used to widen its window every frame).
    #[test]
    fn floating_windows_keep_their_size() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            let sizes = run(ppp);
            let base = &sizes[20];
            assert!(base.len() >= 6, "windows shown: {base:?}");
            for (i, s) in sizes.iter().enumerate().skip(20) {
                assert_eq!(base, s, "at {ppp}x, frame {i}: windows changed size");
            }
        }
    }

    /// On a small screen, or after the window shrinks, every floating
    /// window stays whole inside it (scrolling what doesn't fit).
    #[test]
    fn floating_windows_fit_a_small_or_shrunk_screen() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(256, 256, Color32::WHITE, 64));
        app.workspace.palette.open = true;
        app.modal_state.show_history = true;
        app.brush_state.show_presets = true;
        app.workspace.view_aids.guides.new_guide.open = true;
        app.workspace.view_aids.navigator.open = true;
        app.workspace.frame_stats.enabled = true;
        app.workspace.frame_stats.window_open = true;
        app.modal_state.show_general_settings = true;
        app.modal_state.show_new_canvas_modal = true;
        app.export_state.show_modal = true;
        app.filter_open(crate::canvas::filters::Filter::Exposure { stops: 0.0 });
        let ctx = egui::Context::default();
        crate::ui::theme::apply_global_style(&ctx);
        let screen = |w: f32, h: f32| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h));
        let frame = |app: &mut crate::PainterApp, rect: egui::Rect| {
            let input = egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.show_windows(ctx));
            let titles = [
                "Palette",
                "History",
                "Brush Presets",
                "Guides",
                "Navigator",
                "Frame Times",
                "Settings",
                "New Canvas",
                "Export Canvas",
                "Exposure",
            ];
            ctx.memory(|m| {
                m.areas()
                    .visible_layer_ids()
                    .into_iter()
                    .filter_map(|l| {
                        let name = titles.iter().find(|t| egui::Id::new(**t) == l.id)?;
                        Some((*name, m.area_rect(l.id)?))
                    })
                    .collect::<Vec<_>>()
            })
        };
        for _ in 0..5 {
            frame(&mut app, screen(1600.0, 1000.0));
        }
        let small = screen(640.0, 420.0);
        let mut rects = Vec::new();
        for _ in 0..5 {
            rects = frame(&mut app, small);
        }
        assert!(rects.len() >= 6, "windows shown: {rects:?}");
        for (name, r) in rects {
            assert!(
                small.expand(1.0).contains_rect(r),
                "{name}: {r:?} outside {small:?}"
            );
        }
    }
}
