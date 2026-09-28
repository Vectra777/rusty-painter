//! The Text tool: click the canvas to start typing there. The text shows on
//! a new layer as it's typed (in the brush colour) and can be dragged into
//! place; OK (or another tool, or clicking elsewhere) keeps it as one undo
//! step, Cancel drops it. Kept text is pixels on its layer: it can't be
//! retyped later.

use crate::app::PainterApp;
use crate::canvas::history::UndoAction;
use crate::canvas::storage::{LayerId, LayerKind};
use crate::canvas::text::{self, TextStyle};
use ab_glyph::FontArc;
use eframe::egui::{self, Color32, Vec2};
use std::path::PathBuf;

/// A tile's position and pixels.
type LayerTile = ((i32, i32), Vec<Color32>);

/// A font on offer: shipped with the app, or a file opened when chosen.
pub struct FontEntry {
    pub name: String,
    file: Option<PathBuf>,
    loaded: Option<FontArc>,
}

#[derive(Default)]
pub struct TextToolState {
    pub style: TextStyle,
    /// Index into `fonts`.
    pub font: usize,
    /// Filled the first time the tool is used.
    pub fonts: Vec<FontEntry>,
    pub session: Option<TextSession>,
}

/// Text being typed.
pub struct TextSession {
    pub text: String,
    /// Top-left of the text block, canvas pixels.
    pub pos: Vec2,
    /// The preview layer (not in the history until kept).
    layer: LayerId,
    /// The layer selected before, selected again when done.
    active_before: usize,
    /// Pointer offset from `pos` while dragging the text.
    grab: Option<Vec2>,
    /// What the preview shows, to repaint when anything changes.
    shown: Option<(String, Vec2, TextStyle, usize, Color32)>,
    /// The dialog's text box should take the keyboard (just opened).
    pub focus: bool,
}

impl TextToolState {
    /// The shipped fonts, then the system's (listed once).
    pub fn ensure_fonts(&mut self) {
        if !self.fonts.is_empty() {
            return;
        }
        self.fonts = text::builtin_fonts()
            .into_iter()
            .map(|(name, font)| FontEntry {
                name,
                file: None,
                loaded: Some(font),
            })
            .chain(
                text::system_font_files()
                    .into_iter()
                    .map(|(name, path)| FontEntry {
                        name,
                        file: Some(path),
                        loaded: None,
                    }),
            )
            .collect();
    }

    /// The chosen font, opened if need be (a file that won't open falls
    /// back to the first font).
    pub fn current_font(&mut self) -> Option<FontArc> {
        self.ensure_fonts();
        let entry = self.fonts.get_mut(self.font)?;
        if entry.loaded.is_none()
            && let Some(path) = &entry.file
        {
            entry.loaded = std::fs::read(path)
                .ok()
                .and_then(|bytes| FontArc::try_from_vec(bytes).ok());
        }
        match &entry.loaded {
            Some(font) => Some(font.clone()),
            None => {
                self.font = 0;
                self.fonts.first().and_then(|f| f.loaded.clone())
            }
        }
    }
}

impl PainterApp {
    pub(crate) fn text_press(&mut self, pos: Vec2) {
        let font = self
            .workspace
            .text
            .session
            .is_some()
            .then(|| self.workspace.text.current_font())
            .flatten();
        if let Some(font) = font
            && let Some(session) = &self.workspace.text.session
        {
            let size = text::measure(&font, &session.text, &self.workspace.text.style);
            let r = egui::Rect::from_min_size(egui::pos2(session.pos.x, session.pos.y), size)
                .expand(8.0 / self.viewport.zoom.max(0.01));
            if r.contains(egui::pos2(pos.x, pos.y)) {
                let offset = pos - session.pos;
                if let Some(s) = self.workspace.text.session.as_mut() {
                    s.grab = Some(offset);
                }
                return;
            }
            self.text_commit();
        }
        self.text_start(pos);
    }

    fn text_start(&mut self, pos: Vec2) {
        let active = self.canvas.active_layer_idx;
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.release_canvas();
        let (index, parent) = self.insertion_point(true);
        let id = self
            .canvas_mut()
            .insert_new_layer(index, "Text".into(), LayerKind::Paint, parent);
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        self.insert_layer_state(idx);
        self.workspace.text.session = Some(TextSession {
            text: String::new(),
            pos,
            layer: id,
            active_before: active,
            grab: None,
            shown: None,
            focus: true,
        });
    }

    pub(crate) fn text_drag(&mut self, pos: Vec2) {
        if let Some(s) = self.workspace.text.session.as_mut()
            && let Some(grab) = s.grab
        {
            s.pos = pos - grab;
        }
    }

    pub(crate) fn text_release(&mut self) {
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.grab = None;
        }
    }

    /// Repaint the preview if the text, its place or its look changed.
    pub(crate) fn text_update(&mut self) {
        let color = self.brush_state.brush.brush_options.color;
        let font_index = self.workspace.text.font;
        let style = self.workspace.text.style;
        let Some(session) = &self.workspace.text.session else {
            return;
        };
        let now = (session.text.clone(), session.pos, style, font_index, color);
        if session.shown.as_ref() == Some(&now) {
            return;
        }
        let Some(idx) = self.canvas.layer_index_of(session.layer) else {
            self.workspace.text.session = None;
            return;
        };
        let Some(font) = self.workspace.text.current_font() else {
            return;
        };
        self.release_canvas();
        // Clear what was shown, then paint the text again.
        let old: Vec<(i32, i32)> = self.canvas.layer_tile_keys(idx);
        let ts = self.canvas.tile_size() as f32;
        for &(tx, ty) in &old {
            self.canvas.clear_layer_tile(idx, tx, ty);
            self.mark_tiles_in_bounds_dirty(egui::Rect::from_min_size(
                egui::pos2(tx as f32 * ts, ty as f32 * ts),
                egui::vec2(ts, ts),
            ));
        }
        if let Some(mask) = text::render(&font, &now.0, &style, now.1) {
            let mut scratch = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            if let Some(rect) = self.canvas.paint_mask(idx, &mask, color, &mut scratch) {
                self.mark_tiles_in_bounds_dirty(rect);
            }
        }
        self.layer_state.thumbnails_dirty = true;
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.shown = Some(now);
        }
    }

    /// Keep the text as a new layer, one undo step (blank text is dropped).
    pub(crate) fn text_commit(&mut self) {
        self.text_update();
        let Some(session) = self.workspace.text.session.take() else {
            return;
        };
        let Some(tiles) = self.text_remove_preview(&session) else {
            return;
        };
        if session.text.trim().is_empty() {
            return;
        }
        let name: String = session
            .text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(24)
            .collect();
        self.add_layer_with_tiles(format!("Text: {}", name.trim()), tiles, |_| {});
    }

    /// Drop the text being typed.
    pub(crate) fn text_cancel(&mut self) {
        if let Some(session) = self.workspace.text.session.take() {
            self.text_remove_preview(&session);
        }
    }

    /// Take the preview layer out (with no history), returning its tiles.
    fn text_remove_preview(&mut self, session: &TextSession) -> Option<Vec<LayerTile>> {
        self.release_canvas();
        let idx = self.canvas.layer_index_of(session.layer)?;
        let tiles: Vec<_> = self
            .canvas
            .layer_tile_keys(idx)
            .into_iter()
            .filter_map(|(tx, ty)| {
                let data = self.canvas.get_layer_tile_data(idx, tx, ty)?;
                data.iter().any(|p| p.a() > 0).then_some(((tx, ty), data))
            })
            .collect();
        self.mark_all_tiles_dirty();
        self.canvas_mut().layers.remove(idx);
        self.remove_layer_state(idx);
        let active = session
            .active_before
            .min(self.canvas.layers.len().saturating_sub(1));
        self.canvas_mut().active_layer_idx = active;
        self.layer_state.thumbnails_dirty = true;
        Some(tiles)
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use eframe::egui::{Color32, Vec2};

    fn app() -> crate::PainterApp {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(300, 120, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.brush_state.brush.brush_options.color = Color32::BLACK;
        app.workspace.text.style.size = 40.0;
        app
    }

    fn ink(app: &crate::PainterApp, idx: usize) -> usize {
        app.canvas
            .layer_tile_keys(idx)
            .into_iter()
            .filter_map(|(tx, ty)| app.canvas.get_layer_tile_data(idx, tx, ty))
            .map(|t| t.iter().filter(|p| p.a() > 128).count())
            .sum()
    }

    #[test]
    fn typed_text_becomes_a_layer_in_one_undo_step() {
        let mut app = app();
        let layers = app.canvas.layers.len();
        app.text_press(Vec2::new(20.0, 20.0));
        app.workspace.text.session.as_mut().unwrap().text = "Hello".into();
        app.text_update();
        assert_eq!(
            app.canvas.layers.len(),
            layers + 1,
            "previewed on its own layer"
        );
        assert_eq!(
            app.layer_state.history.stacks().0.len(),
            0,
            "nothing kept yet"
        );
        app.text_commit();
        assert_eq!(app.canvas.layers.len(), layers + 1);
        let idx = app.canvas.active_layer_idx;
        assert_eq!(app.canvas.layers[idx].name, "Text: Hello");
        assert!(ink(&app, idx) > 100);
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), layers, "undo takes the layer away");
    }

    #[test]
    fn dragging_the_text_moves_it_and_cancel_leaves_nothing() {
        let mut app = app();
        let layers = app.canvas.layers.len();
        app.text_press(Vec2::new(20.0, 20.0));
        app.workspace.text.session.as_mut().unwrap().text = "Move".into();
        app.text_update();
        // Press on the text: drag it rather than starting new text.
        app.text_press(Vec2::new(30.0, 30.0));
        app.text_drag(Vec2::new(130.0, 60.0));
        app.text_release();
        let s = app.workspace.text.session.as_ref().unwrap();
        assert_eq!(s.pos, Vec2::new(120.0, 50.0));
        app.text_cancel();
        assert_eq!(app.canvas.layers.len(), layers);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    #[test]
    fn blank_text_is_not_kept() {
        let mut app = app();
        let layers = app.canvas.layers.len();
        app.text_press(Vec2::new(20.0, 20.0));
        app.text_commit();
        assert_eq!(app.canvas.layers.len(), layers);
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }
}
