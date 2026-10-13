//! Several documents open at once, in tabs. The active one lives in the
//! app's own fields, as it always has; the others wait here whole, swapped
//! in when their tab is picked. Opening or creating a document while the
//! one shown has work in it opens it in a new tab.
//!
//! A tab changes only while nothing runs in the background (no stroke
//! being painted, no file being opened or saved), so whatever finishes
//! lands in the document it started in.

use crate::PainterApp;
use crate::app::document::ColorModel;
use crate::app::state::LayerState;
use crate::canvas::Canvas;
use eframe::egui::{self, Vec2};
use std::path::PathBuf;
use std::sync::Arc;

/// A document in a tab that isn't shown: everything of it the app keeps.
pub struct Document {
    canvas: Arc<Canvas>,
    layer_state: LayerState,
    /// Zoom, offset, rotation, mirrored.
    view: (f32, Vec2, f32, bool),
    auto_fit: bool,
    fitted_to: Option<Vec2>,
    selection: Option<crate::selection::SelectionShape>,
    saved_selections: Vec<crate::app::tools::select::SavedSelection>,
    color_model: ColorModel,
    animation: crate::app::animation::AnimationState,
    rig: crate::app::rig::RigEditState,
    motion: crate::app::motion::MotionEditState,
    guides: crate::app::tools::guides::GuideState,
    wet_clock: Option<std::time::Instant>,
    wet_step: Option<(u64, usize)>,
    wet_drying: Option<crate::app::stroke_ops::WetDrying>,
    autosave: crate::app::autosave::AutosaveState,
    timelapse: crate::app::timelapse::TimelapseState,
    shader_edits: u64,
    project: Option<PathBuf>,
    title: String,
}

impl Document {
    /// An empty one, to swap with the app's (a new tab's starting point).
    fn blank(autosave_slot: usize, color_model: ColorModel) -> Self {
        Self {
            canvas: Arc::new(Canvas::new(1, 1, egui::Color32::WHITE, 64)),
            layer_state: LayerState::new(2),
            view: (1.0, Vec2::ZERO, 0.0, false),
            auto_fit: true,
            fitted_to: None,
            selection: None,
            saved_selections: Vec::new(),
            color_model,
            animation: Default::default(),
            rig: Default::default(),
            motion: Default::default(),
            guides: Default::default(),
            wet_clock: None,
            wet_step: None,
            wet_drying: None,
            autosave: crate::app::autosave::AutosaveState::tab(autosave_slot),
            timelapse: Default::default(),
            shader_edits: 0,
            project: None,
            title: UNTITLED.into(),
        }
    }

    /// Whether it has work the user hasn't saved.
    fn unsaved(&self) -> bool {
        let history = &self.layer_state.history;
        let (undo, redo) = history.stacks();
        let version = (
            history.push_count().wrapping_add(self.shader_edits),
            undo.len(),
            redo.len(),
        );
        !self.autosave.matches_saved(version)
    }
}

const UNTITLED: &str = "Untitled";

/// The tabs, in order: `None` for the shown one (its document is the
/// app's own).
pub struct Tabs {
    docs: Vec<Option<Document>>,
    active: usize,
    /// The shown document's name.
    pub title: String,
    /// A tab with unsaved work whose close is waiting for an answer.
    closing: Option<usize>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            docs: vec![None],
            active: 0,
            title: UNTITLED.into(),
            closing: None,
        }
    }
}

impl Tabs {
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// The autosave slots the tabs use.
    fn slots<'a>(&'a self, app_slot: usize) -> impl Iterator<Item = usize> + 'a {
        (self.docs.iter().flatten().map(|d| d.autosave.slot())).chain([app_slot])
    }
}

impl PainterApp {
    /// Swap the shown document with `doc`, field by field.
    fn swap_document(&mut self, doc: &mut Document) {
        std::mem::swap(&mut self.canvas, &mut doc.canvas);
        std::mem::swap(&mut self.layer_state, &mut doc.layer_state);
        let v = &mut self.viewport;
        let mut view = (v.zoom, v.offset, v.rotation, v.flip_x);
        std::mem::swap(&mut view, &mut doc.view);
        (v.zoom, v.offset, v.rotation, v.flip_x) = view;
        std::mem::swap(
            &mut self.selection_manager.current_shape,
            &mut doc.selection,
        );
        self.selection_manager.canvas_size = [self.canvas.width(), self.canvas.height()];
        let ws = &mut self.workspace;
        std::mem::swap(&mut ws.auto_fit, &mut doc.auto_fit);
        std::mem::swap(&mut ws.fitted_to, &mut doc.fitted_to);
        std::mem::swap(&mut ws.select.saved, &mut doc.saved_selections);
        std::mem::swap(&mut ws.color_model, &mut doc.color_model);
        std::mem::swap(&mut ws.animation, &mut doc.animation);
        std::mem::swap(&mut ws.rig, &mut doc.rig);
        std::mem::swap(&mut ws.motion, &mut doc.motion);
        std::mem::swap(&mut ws.guides, &mut doc.guides);
        std::mem::swap(&mut ws.wet_clock, &mut doc.wet_clock);
        std::mem::swap(&mut ws.wet_step, &mut doc.wet_step);
        std::mem::swap(&mut ws.wet_drying, &mut doc.wet_drying);
        std::mem::swap(&mut ws.autosave, &mut doc.autosave);
        std::mem::swap(&mut ws.timelapse, &mut doc.timelapse);
        std::mem::swap(&mut ws.shaders.edits, &mut doc.shader_edits);
        std::mem::swap(&mut ws.library.project, &mut doc.project);
        std::mem::swap(&mut ws.tabs.title, &mut doc.title);
        // Compiled shaders and baked frames are per layer id, which the
        // other document reuses.
        ws.shaders.forget_document();
        // ponytail: one set of GPU atlases, uploaded again for the tab
        // shown; per-document atlases if switching big documents drags.
        let (w, h) = (self.canvas.width(), self.canvas.height());
        self.recreate_render_cache(w, h);
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// What can't wait in a tab is applied or dropped: a transform,
    /// liquify, gradient, text or shape is applied; a filter preview, a
    /// selection being drawn and the quick mask end.
    fn settle_for_tab_change(&mut self) {
        if crate::app::tools::transform::transform_running(self) {
            crate::app::tools::transform::commit_floating_layer(self);
        }
        self.liquify_commit();
        self.gradient_commit();
        self.text_commit();
        self.shape_commit();
        if self.workspace.select.quick_mask.is_some() {
            self.quick_mask_leave();
        }
        self.select_cancel();
        self.forget_last_pick();
        self.patch_abandon();
        let ws = &mut self.workspace;
        ws.filter.session = None;
        ws.filter.editing = None;
        ws.fill.path.clear();
        ws.guides.end_drag();
        ws.animation.playing = false;
        self.release_canvas();
    }

    /// Whether the tab shown can change now (nothing is being painted,
    /// opened or saved).
    pub(crate) fn can_change_tab(&self) -> bool {
        !self.brush_state.is_drawing
            && !self.viewport.is_primary_down
            && !self.strokes_settling()
            && self.workspace.jobs.is_idle()
    }

    /// Whether the shown document is untouched: a new document can take
    /// its place rather than a tab of its own.
    fn pristine(&self) -> bool {
        self.layer_state.history.push_count() == 0
            && self.workspace.library.project.is_none()
            && !self.has_unsaved_work()
    }

    /// A document is about to replace the shown one: unless that one is
    /// untouched, it moves to a tab of its own first (the new one opens in
    /// a tab after it).
    pub(crate) fn keep_document_in_tab(&mut self) {
        if self.pristine() || !self.workspace.library.has_document {
            return;
        }
        self.settle_for_tab_change();
        let slot = self.free_autosave_slot();
        let mut doc = Document::blank(slot, self.workspace.color_model);
        self.swap_document(&mut doc);
        let tabs = &mut self.workspace.tabs;
        tabs.docs.insert(tabs.active, Some(doc));
        tabs.active += 1;
    }

    /// An autosave slot no tab uses and no file left over holds.
    fn free_autosave_slot(&self) -> usize {
        let used: Vec<usize> = (self.workspace.tabs)
            .slots(self.workspace.autosave.slot())
            .collect();
        (1..)
            .find(|&n| !used.contains(&n) && !self.autosave_path_for(n).exists())
            .unwrap_or(usize::MAX)
    }

    /// Show tab `i`.
    pub(crate) fn switch_tab(&mut self, i: usize) {
        let tabs = &self.workspace.tabs;
        if i == tabs.active || i >= tabs.docs.len() || !self.can_change_tab() {
            return;
        }
        self.settle_for_tab_change();
        let Some(mut doc) = self.workspace.tabs.docs[i].take() else {
            return;
        };
        self.swap_document(&mut doc);
        let tabs = &mut self.workspace.tabs;
        tabs.docs[tabs.active] = Some(doc);
        tabs.active = i;
    }

    /// Close tab `i` (asking first when it has unsaved work, unless
    /// `discard`).
    pub(crate) fn close_tab(&mut self, i: usize, discard: bool) {
        let tabs = &self.workspace.tabs;
        if tabs.docs.len() < 2 || i >= tabs.docs.len() || !self.can_change_tab() {
            return;
        }
        let unsaved = match &tabs.docs[i] {
            Some(doc) => doc.unsaved(),
            None => self.has_unsaved_work(),
        };
        if unsaved && !discard {
            self.workspace.tabs.closing = Some(i);
            return;
        }
        if i == self.workspace.tabs.active {
            self.switch_tab(if i == 0 { 1 } else { i - 1 });
        }
        let tabs = &mut self.workspace.tabs;
        if let Some(doc) = tabs.docs.remove(i) {
            // Closed on purpose: nothing to recover next time.
            let path = self.autosave_path_for(doc.autosave.slot());
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("source"));
        }
        let tabs = &mut self.workspace.tabs;
        if tabs.active > i {
            tabs.active -= 1;
        }
    }

    /// Quitting: every tab's unsaved work kept for the next start, as the
    /// shown one's is.
    pub(crate) fn autosave_tabs_on_exit(&mut self) {
        let shown = self.workspace.tabs.active;
        for i in 0..self.workspace.tabs.len() {
            if i == shown {
                continue;
            }
            let Some(mut doc) = self.workspace.tabs.docs[i].take() else {
                continue;
            };
            self.swap_document(&mut doc);
            self.autosave_on_exit();
            self.swap_document(&mut doc);
            self.workspace.tabs.docs[i] = Some(doc);
        }
    }

    /// The tab name of a document's file.
    pub(crate) fn set_title_from(&mut self, name: &str) {
        let stem = std::path::Path::new(name).file_stem();
        self.workspace.tabs.title =
            stem.map_or_else(|| UNTITLED.into(), |s| s.to_string_lossy().into_owned());
    }
}

/// The strip of tabs over the canvas (when there's more than one), with
/// Ctrl+Tab, Ctrl+Shift+Tab and Ctrl+W.
pub(crate) fn tab_strip(app: &mut PainterApp, ctx: &egui::Context) {
    let count = app.workspace.tabs.len();
    if count < 2 {
        return;
    }
    let ready = app.can_change_tab();
    let active = app.workspace.tabs.active;
    let (next, prev, close) = ctx.input_mut(|i| {
        let tab = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Tab);
        let back = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::Tab,
        );
        let w = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::W);
        // (The longer shortcut first: Ctrl+Tab would take Ctrl+Shift+Tab.)
        let prev = i.consume_shortcut(&back);
        (i.consume_shortcut(&tab), prev, i.consume_shortcut(&w))
    });
    let mut switch = None;
    let mut close_tab = close.then_some(active);
    if next {
        switch = Some((active + 1) % count);
    } else if prev {
        switch = Some((active + count - 1) % count);
    }
    egui::TopBottomPanel::top("doc_tabs").show(ctx, |ui| {
        ui.add_enabled_ui(ready, |ui| {
            ui.horizontal(|ui| {
                for i in 0..count {
                    let (title, unsaved) = match &app.workspace.tabs.docs[i] {
                        Some(doc) => (doc.title.clone(), doc.unsaved()),
                        None => (app.workspace.tabs.title.clone(), app.has_unsaved_work()),
                    };
                    let (picked, close) = tab(ui, &title, i == active, unsaved);
                    if close {
                        close_tab = Some(i);
                    } else if picked {
                        switch = Some(i);
                    }
                }
            });
        });
    });
    if let Some(i) = switch {
        app.switch_tab(i);
    }
    if let Some(i) = close_tab {
        app.close_tab(i, false);
    }
    close_dialog(app, ctx);
}

/// One tab: its name and, in the same block, a cross that closes it (a
/// dot while it has unsaved work, the cross when hovered). Whether it was
/// clicked, and whether to close it (the cross, or a middle click).
fn tab(ui: &mut egui::Ui, title: &str, active: bool, unsaved: bool) -> (bool, bool) {
    use crate::ui::style::{accent, accent_text};
    const PAD: f32 = 10.0;
    const CROSS: f32 = 16.0;
    let visuals = ui.visuals().clone();
    let text = if active {
        accent_text()
    } else {
        visuals.text_color()
    };
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(title.to_owned(), font, text);
    let size = egui::vec2(
        PAD + galley.size().x + 6.0 + CROSS + PAD * 0.5,
        galley.size().y.max(CROSS) + 8.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let cross = egui::Rect::from_center_size(
        egui::pos2(rect.right() - PAD * 0.5 - CROSS * 0.5, rect.center().y),
        egui::Vec2::splat(CROSS),
    );
    let on_cross = response.hover_pos().is_some_and(|p| cross.contains(p));
    let rounding = visuals.widgets.inactive.rounding;
    let fill = match (active, response.hovered()) {
        (true, _) => accent(),
        (false, true) => visuals.widgets.hovered.weak_bg_fill,
        (false, false) => visuals.widgets.inactive.weak_bg_fill,
    };
    let painter = ui.painter();
    painter.rect_filled(rect, rounding, fill);
    painter.galley(
        egui::pos2(rect.left() + PAD, rect.center().y - galley.size().y * 0.5),
        galley,
        text,
    );
    if on_cross {
        painter.rect_filled(cross, rounding, text.gamma_multiply(0.2));
    }
    if unsaved && !response.hovered() {
        painter.circle_filled(cross.center(), 3.5, text);
    } else if active || response.hovered() {
        let r = CROSS * 0.22;
        let stroke = egui::Stroke::new(1.5_f32, text);
        let c = cross.center();
        painter.line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke);
        painter.line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], stroke);
    }
    let response = response.on_hover_text(if on_cross { "Close" } else { title });
    let close = (response.clicked() && on_cross) || response.middle_clicked();
    (response.clicked(), close)
}

/// Closing a tab with unsaved work: discard it or keep it open.
fn close_dialog(app: &mut PainterApp, ctx: &egui::Context) {
    use crate::ui::widgets::FitScreen;
    let Some(i) = app.workspace.tabs.closing else {
        return;
    };
    let (mut discard, mut keep) = (false, false);
    egui::Window::new("Close without saving?")
        .fit_screen(ctx)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("This tab has changes that aren't saved.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                discard = ui.button("Close without saving").clicked();
                keep = ui.button("Cancel").clicked();
            });
        });
    if discard || keep {
        app.workspace.tabs.closing = None;
    }
    if discard {
        app.close_tab(i, true);
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::history::{History, UndoAction};
    use eframe::egui::Color32;

    fn change(app: &mut crate::PainterApp, color: Color32) {
        app.canvas_mut()
            .set_layer_tile_data(1, 0, 0, vec![color; 64 * 64]);
        app.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: None,
        });
    }

    fn top_left(app: &crate::PainterApp) -> Color32 {
        app.canvas.get_layer_tile_data(1, 0, 0).unwrap()[0]
    }

    #[test]
    fn a_document_with_work_moves_to_a_tab_and_each_tab_keeps_its_own() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.workspace.library.has_document = true;
        app.workspace.autosave.recovery = None;
        app.mark_saved();
        change(&mut app, Color32::RED);
        app.viewport.zoom = 3.0;
        // A new document: the red one stays, in the first tab.
        app.replace_document(Canvas::new(128, 64, Color32::WHITE, 64), History::new());
        assert_eq!(app.workspace.tabs.len(), 2);
        assert_eq!(app.canvas.width(), 128);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
        change(&mut app, Color32::BLUE);
        change(&mut app, Color32::GREEN);
        app.switch_tab(0);
        assert_eq!(app.canvas.width(), 64);
        assert_eq!(top_left(&app), Color32::RED);
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        assert_eq!(app.viewport.zoom, 3.0, "its own view");
        app.switch_tab(1);
        assert_eq!(top_left(&app), Color32::GREEN);
        assert_eq!(app.layer_state.history.stacks().0.len(), 2);
        // Closing one with unsaved work asks first.
        app.close_tab(0, false);
        assert_eq!(app.workspace.tabs.len(), 2);
        app.close_tab(0, true);
        assert_eq!(app.workspace.tabs.len(), 1);
        assert_eq!(top_left(&app), Color32::GREEN, "the shown one stays");
    }

    #[test]
    fn an_untouched_document_is_simply_replaced() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.workspace.library.has_document = true;
        app.mark_saved();
        app.replace_document(Canvas::new(128, 64, Color32::WHITE, 64), History::new());
        assert_eq!(app.workspace.tabs.len(), 1);
    }

    #[test]
    fn quitting_keeps_every_tabs_unsaved_work() {
        let dir = std::env::temp_dir().join(format!("rp-tabs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.brush_state.brushes_path = dir.join("brushes");
        app.workspace.library.has_document = true;
        app.workspace.autosave.recovery = None;
        app.mark_saved();
        change(&mut app, Color32::RED);
        app.replace_document(Canvas::new(64, 64, Color32::WHITE, 64), History::new());
        change(&mut app, Color32::BLUE);
        app.autosave_on_exit();
        app.autosave_tabs_on_exit();
        assert!(dir.join("autosave.rpainter").exists(), "the first tab's");
        assert!(dir.join("autosave-1.rpainter").exists(), "the second's");
        // Both offered next time.
        let next = crate::app::autosave::AutosaveState::new(&dir.join("autosave.rpainter"));
        assert!(next.recovery.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
