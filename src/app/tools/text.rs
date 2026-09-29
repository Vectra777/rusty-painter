//! The Text tool: click the canvas to start typing there. The text shows on
//! a new layer as it's typed (in the brush colour) and can be dragged into
//! place; OK (or another tool, or clicking elsewhere) keeps it as one undo
//! step, Cancel drops it.
//!
//! Kept text is a text layer: it keeps its source ([`TextLayer`]), so
//! clicking its text with the tool (or double-clicking it in the Layers
//! panel) opens it again for editing. Painting on it, or any change but a
//! move, makes it plain pixels in that same undo step, as does Layer →
//! Rasterise Text.

use crate::app::PainterApp;
use crate::canvas::history::{LayerHistoryOp, TileSnapshot, UndoAction};
use crate::canvas::storage::{LayerId, LayerKind};
use crate::canvas::text::{self, TextLayer, TextStyle};
use crate::selection::transform::TransformInfo;
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
    /// Text layers a brush stroke started on, in stroke order: made plain
    /// pixels when the stroke began, their text goes into the stroke's undo
    /// step when it's filed.
    pub(crate) stroke_rasterised: Vec<(LayerId, Box<TextLayer>)>,
}

/// Text being typed.
pub struct TextSession {
    pub text: String,
    /// Top-left of the text block, canvas pixels.
    pub pos: Vec2,
    /// The text's colour: the brush colour when typing starts, following
    /// it when the brush colour changes.
    pub color: Color32,
    /// The brush colour last seen, to notice it changing.
    brush_seen: Color32,
    /// The preview layer (not in the history until kept), or the text
    /// layer being edited.
    layer: LayerId,
    /// The layer selected before, selected again when done.
    active_before: usize,
    /// Pointer offset from `pos` while dragging the text.
    grab: Option<Vec2>,
    /// What the preview shows, to repaint when anything changes.
    shown: Option<(String, Vec2, TextStyle, usize, Color32)>,
    /// The dialog's text box should take the keyboard (just opened).
    pub focus: bool,
    /// Editing an existing text layer: how it was.
    editing: Option<EditedLayer>,
}

/// A text layer as it was when its editing began.
struct EditedLayer {
    text: Box<TextLayer>,
    tiles: Vec<LayerTile>,
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
        match self.open_font(self.font) {
            Some(font) => Some(font),
            None => {
                self.font = 0;
                self.fonts.first().and_then(|f| f.loaded.clone())
            }
        }
    }

    /// Font `index`, opened if need be.
    fn open_font(&mut self, index: usize) -> Option<FontArc> {
        let entry = self.fonts.get_mut(index)?;
        if entry.loaded.is_none()
            && let Some(path) = &entry.file
        {
            entry.loaded = std::fs::read(path)
                .ok()
                .and_then(|bytes| FontArc::try_from_vec(bytes).ok());
        }
        entry.loaded.clone()
    }

    /// The name of the chosen font.
    fn font_name(&self) -> String {
        self.fonts
            .get(self.font)
            .map_or_else(String::new, |f| f.name.clone())
    }

    /// Where the font called `name` is in the list.
    fn font_index(&mut self, name: &str) -> Option<usize> {
        self.ensure_fonts();
        self.fonts.iter().position(|f| f.name == name)
    }

    /// The font a text layer uses (the first font if it isn't here).
    fn font_for(&mut self, name: &str) -> Option<FontArc> {
        let index = self.font_index(name).unwrap_or(0);
        self.open_font(index)
            .or_else(|| self.fonts.first().and_then(|f| f.loaded.clone()))
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
        match self.text_layer_at(pos) {
            Some(idx) => self.text_edit_layer(idx),
            None => self.text_start(pos),
        }
    }

    /// The topmost visible, unlocked text layer whose text covers canvas
    /// point `pos`.
    fn text_layer_at(&mut self, pos: Vec2) -> Option<usize> {
        let margin = 4.0 / self.viewport.zoom.max(0.01);
        let candidates: Vec<(usize, TextLayer)> = self
            .canvas
            .layers
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, l)| l.visible && !l.locked)
            .filter_map(|(i, l)| Some((i, (**l.text.as_ref()?).clone())))
            .collect();
        candidates.into_iter().find_map(|(i, t)| {
            let font = self.workspace.text.font_for(&t.font)?;
            let size = text::measure(&font, &t.text, &t.style);
            egui::Rect::from_min_size(egui::pos2(t.pos.x, t.pos.y), size)
                .expand(margin)
                .contains(egui::pos2(pos.x, pos.y))
                .then_some(i)
        })
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
        let color = self.brush_state.brush.brush_options.color;
        self.workspace.text.session = Some(TextSession {
            text: String::new(),
            pos,
            color,
            brush_seen: color,
            layer: id,
            active_before: active,
            grab: None,
            shown: None,
            focus: true,
            editing: None,
        });
    }

    /// Open text layer `idx` for editing, with its text, font and settings
    /// loaded (with the Text tool). Does nothing for other layers.
    pub(crate) fn text_edit_layer(&mut self, idx: usize) {
        self.text_commit();
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.release_canvas();
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        let Some(source) = layer.text.clone().filter(|_| !layer.locked) else {
            return;
        };
        let id = layer.id;
        let tiles: Vec<LayerTile> = self
            .canvas
            .layer_tile_keys(idx)
            .into_iter()
            .filter_map(|(tx, ty)| Some(((tx, ty), self.canvas.get_layer_tile_data(idx, tx, ty)?)))
            .collect();
        let state = &mut self.workspace.text;
        if let Some(font) = state.font_index(&source.font) {
            state.font = font;
        }
        state.style = source.style;
        self.active_tool = crate::app::tools::Tool::Text;
        self.canvas_mut().active_layer_idx = idx;
        self.workspace.text.session = Some(TextSession {
            text: source.text.clone(),
            pos: source.pos,
            color: source.color,
            brush_seen: self.brush_state.brush.brush_options.color,
            layer: id,
            active_before: idx,
            grab: None,
            shown: None,
            focus: true,
            editing: Some(EditedLayer {
                text: source,
                tiles,
            }),
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
        let brush_color = self.brush_state.brush.brush_options.color;
        let font_index = self.workspace.text.font;
        let style = self.workspace.text.style;
        let Some(session) = self.workspace.text.session.as_mut() else {
            return;
        };
        // Picking a colour while typing colours the text.
        if session.brush_seen != brush_color {
            session.brush_seen = brush_color;
            session.color = brush_color;
        }
        let now = (
            session.text.clone(),
            session.pos,
            style,
            font_index,
            session.color,
        );
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
        self.clear_layer_tiles(idx);
        if let Some(mask) = text::render(&font, &now.0, &style, now.1) {
            let mut scratch = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            if let Some(rect) = self.canvas.paint_mask(idx, &mask, now.4, &mut scratch) {
                self.mark_tiles_in_bounds_dirty(rect);
            }
        }
        self.layer_state.thumbnails_dirty = true;
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.shown = Some(now);
        }
    }

    /// Drop every tile of layer `idx` (no history), marking them for redraw.
    fn clear_layer_tiles(&mut self, idx: usize) {
        let ts = self.canvas.tile_size() as f32;
        for (tx, ty) in self.canvas.layer_tile_keys(idx) {
            self.canvas.clear_layer_tile(idx, tx, ty);
            self.mark_tiles_in_bounds_dirty(egui::Rect::from_min_size(
                egui::pos2(tx as f32 * ts, ty as f32 * ts),
                egui::vec2(ts, ts),
            ));
        }
    }

    /// The session's text as a text layer's source.
    fn text_source(&mut self, session: &TextSession) -> TextLayer {
        self.workspace.text.current_font();
        TextLayer {
            text: session.text.clone(),
            font: self.workspace.text.font_name(),
            style: self.workspace.text.style,
            color: session.color,
            pos: session.pos,
        }
    }

    /// Keep the text: a new text layer, or the edited one, as one undo step
    /// (blank new text is dropped; an edited layer left blank is deleted).
    pub(crate) fn text_commit(&mut self) {
        self.text_update();
        let Some(session) = self.workspace.text.session.take() else {
            return;
        };
        if session.editing.is_some() {
            self.text_commit_edit(session);
            return;
        }
        let source = self.text_source(&session);
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
        self.add_layer_with_tiles(format!("Text: {}", name.trim()), tiles, |layer| {
            layer.text = Some(Box::new(source));
        });
    }

    /// Keep an edited text layer's new text and pixels as one undo step,
    /// which gives back both.
    fn text_commit_edit(&mut self, mut session: TextSession) {
        let source = self.text_source(&session);
        let Some(before) = session.editing.take() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(session.layer) else {
            return;
        };
        if *before.text == source {
            self.text_restore(idx, before);
            return;
        }
        if session.text.trim().is_empty() {
            self.text_restore(idx, before);
            self.remove_layer(idx);
            return;
        }
        self.release_canvas();
        let ts = self.canvas.tile_size();
        let mut keys = self.canvas.layer_tile_keys(idx);
        keys.extend(before.tiles.iter().map(|(k, _)| *k));
        keys.sort_unstable();
        keys.dedup();
        let mut old: std::collections::HashMap<(i32, i32), Vec<Color32>> =
            before.tiles.into_iter().collect();
        let tiles = keys
            .into_iter()
            .map(|(tx, ty)| TileSnapshot {
                tx,
                ty,
                layer_id: session.layer,
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: old
                    .remove(&(tx, ty))
                    .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts])
                    .into(),
            })
            .collect();
        self.canvas_mut().layers[idx].text = Some(Box::new(source));
        self.push_undo(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Text {
                layers: vec![(session.layer, Some(before.text))],
                inner: None,
            }),
        });
        self.layer_state.thumbnails_dirty = true;
    }

    /// Put an edited text layer's pixels back as they were.
    fn text_restore(&mut self, idx: usize, before: EditedLayer) {
        self.release_canvas();
        self.clear_layer_tiles(idx);
        for ((tx, ty), data) in before.tiles {
            self.canvas.set_layer_tile_data(idx, tx, ty, data);
        }
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
    }

    /// Drop the text being typed (an edited layer goes back as it was).
    pub(crate) fn text_cancel(&mut self) {
        if let Some(mut session) = self.workspace.text.session.take() {
            match session.editing.take() {
                Some(before) => {
                    if let Some(idx) = self.canvas.layer_index_of(session.layer) {
                        self.text_restore(idx, before);
                    }
                }
                None => {
                    self.text_remove_preview(&session);
                }
            }
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

    /// Whether layer `idx` is a text layer.
    pub(crate) fn is_text_layer(&self, idx: usize) -> bool {
        self.canvas
            .layers
            .get(idx)
            .is_some_and(|l| l.text.is_some())
    }

    /// Make text layer `idx` a plain pixel layer (Layer → Rasterise Text),
    /// as one undo step.
    pub(crate) fn rasterise_text_layer(&mut self, idx: usize) {
        if !self.is_text_layer(idx) {
            return;
        }
        self.text_commit();
        let canvas = self.canvas_mut();
        let id = canvas.layers[idx].id;
        let old = canvas.layers[idx].text.take();
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Text {
                layers: vec![(id, old)],
                inner: None,
            }),
        });
        self.layer_state.thumbnails_dirty = true;
    }

    /// Text layers `action` paints on become plain pixels, their text kept
    /// in `action` for undo (not a layer it adds, nor text it already sets).
    pub(crate) fn rasterise_painted_text(&mut self, action: &mut UndoAction) {
        let covered: Vec<LayerId> = match &action.layer_action {
            Some(LayerHistoryOp::Document(_)) => return,
            Some(LayerHistoryOp::Text { layers, .. }) => layers.iter().map(|(id, _)| *id).collect(),
            Some(LayerHistoryOp::Added { id, .. }) => vec![*id],
            _ => Vec::new(),
        };
        let mut ids: Vec<LayerId> = action
            .tiles
            .iter()
            .map(|t| t.layer_id)
            .filter(|id| !covered.contains(id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|&id| {
            self.canvas
                .layer_index_of(id)
                .is_some_and(|i| self.is_text_layer(i))
        });
        if ids.is_empty() {
            return;
        }
        let canvas = self.canvas_mut();
        let taken: Vec<(LayerId, Option<Box<TextLayer>>)> = ids
            .into_iter()
            .filter_map(|id| {
                let i = canvas.layer_index_of(id)?;
                Some((id, canvas.layers[i].text.take()))
            })
            .collect();
        action.layer_action = Some(match action.layer_action.take() {
            Some(LayerHistoryOp::Text { mut layers, inner }) => {
                layers.extend(taken);
                LayerHistoryOp::Text { layers, inner }
            }
            other => LayerHistoryOp::Text {
                layers: taken,
                inner: other.map(Box::new),
            },
        });
        self.layer_state.thumbnails_dirty = true;
    }

    /// A brush stroke is starting on the active layer: a text layer becomes
    /// plain pixels, its text going into the stroke's undo step.
    pub(crate) fn rasterise_text_for_stroke(&mut self) {
        let idx = self.canvas.active_layer_idx;
        if !self.is_text_layer(idx) {
            return;
        }
        let canvas = self.canvas_mut();
        let id = canvas.layers[idx].id;
        if let Some(text) = canvas.layers[idx].text.take() {
            self.workspace.text.stroke_rasterised.push((id, text));
            self.layer_state.thumbnails_dirty = true;
        }
    }

    /// File the text of a layer the finished stroke `undo` began on into it.
    pub(crate) fn attach_stroke_rasterised(&mut self, undo: &mut UndoAction) {
        let pending = &mut self.workspace.text.stroke_rasterised;
        let Some(i) = pending.iter().position(|(id, _)| {
            undo.tiles.is_empty() || undo.tiles.iter().any(|t| t.layer_id == *id)
        }) else {
            return;
        };
        let (id, text) = pending.remove(i);
        let inner = undo.layer_action.take().map(Box::new);
        undo.layer_action = Some(LayerHistoryOp::Text {
            layers: vec![(id, Some(text))],
            inner,
        });
    }

    /// Text layer `id` was moved by `info` as a whole: when that's only a
    /// move, it stays text at its new place, and this is the undo record.
    /// Anything else leaves it to become pixels.
    pub(crate) fn text_layer_moved(
        &mut self,
        id: LayerId,
        info: &TransformInfo,
    ) -> Option<LayerHistoryOp> {
        let only_moved = info.warp.is_none()
            && info.corners.is_none()
            && info.rotation == 0.0
            && info.scale == Vec2::new(1.0, 1.0);
        let idx = self.canvas.layer_index_of(id)?;
        if !only_moved || !self.is_text_layer(idx) {
            return None;
        }
        let layer = &mut self.canvas_mut().layers[idx];
        let old = layer.text.clone()?;
        if let Some(t) = layer.text.as_mut() {
            t.pos += info.offset;
        }
        Some(LayerHistoryOp::Text {
            layers: vec![(id, Some(old))],
            inner: None,
        })
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

    /// Layer `idx`'s pixels, by tile, without empty tiles.
    fn pixels(app: &crate::PainterApp, idx: usize) -> Vec<((i32, i32), Vec<Color32>)> {
        let mut tiles: Vec<_> = app
            .canvas
            .layer_tile_keys(idx)
            .into_iter()
            .filter_map(|(tx, ty)| {
                let data = app.canvas.get_layer_tile_data(idx, tx, ty)?;
                data.iter().any(|p| p.a() > 0).then_some(((tx, ty), data))
            })
            .collect();
        tiles.sort_by_key(|(k, _)| *k);
        tiles
    }

    fn type_text(app: &mut crate::PainterApp, pos: Vec2, text: &str) -> usize {
        app.text_press(pos);
        app.workspace.text.session.as_mut().unwrap().text = text.into();
        app.text_update();
        app.text_commit();
        app.canvas.active_layer_idx
    }

    fn undo_depth(app: &crate::PainterApp) -> usize {
        app.layer_state.history.stacks().0.len()
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
        assert_eq!(undo_depth(&app), 0, "nothing kept yet");
        app.text_commit();
        assert_eq!(app.canvas.layers.len(), layers + 1);
        let idx = app.canvas.active_layer_idx;
        assert_eq!(app.canvas.layers[idx].name, "Text: Hello");
        assert!(ink(&app, idx) > 100);
        let text = app.canvas.layers[idx].text.as_ref().expect("a text layer");
        assert_eq!(text.text, "Hello");
        assert_eq!(text.pos, Vec2::new(20.0, 20.0));
        assert_eq!(text.color, Color32::BLACK);
        assert_eq!(text.style.size, 40.0);
        assert_eq!(undo_depth(&app), 1);
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), layers, "undo takes the layer away");
        app.apply_history(true);
        let idx = app.canvas.layers.len() - 1;
        assert_eq!(
            app.canvas.layers[idx]
                .text
                .as_ref()
                .map(|t| t.text.as_str()),
            Some("Hello"),
            "redo brings the text layer back"
        );
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
        assert_eq!(undo_depth(&app), 0);
    }

    #[test]
    fn blank_text_is_not_kept() {
        let mut app = app();
        let layers = app.canvas.layers.len();
        app.text_press(Vec2::new(20.0, 20.0));
        app.text_commit();
        assert_eq!(app.canvas.layers.len(), layers);
        assert_eq!(undo_depth(&app), 0);
    }

    #[test]
    fn clicking_kept_text_edits_it_in_one_undo_step() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let layers = app.canvas.layers.len();
        let before = pixels(&app, idx);
        let depth = undo_depth(&app);
        // Click on the letters: the session opens on that layer.
        app.canvas_mut().active_layer_idx = 0;
        app.text_press(Vec2::new(30.0, 40.0));
        let s = app.workspace.text.session.as_ref().expect("editing");
        assert_eq!(s.text, "Hi");
        assert_eq!(app.canvas.active_layer_idx, idx);
        app.workspace.text.session.as_mut().unwrap().text = "Hello there".into();
        app.workspace.text.style.size = 30.0;
        app.brush_state.brush.brush_options.color = Color32::from_rgb(200, 0, 0);
        app.text_update();
        app.text_commit();
        assert_eq!(app.canvas.layers.len(), layers, "the same layer");
        assert_eq!(undo_depth(&app), depth + 1, "one step");
        let text = app.canvas.layers[idx].text.as_ref().unwrap();
        assert_eq!(text.text, "Hello there");
        assert_eq!(text.style.size, 30.0);
        assert_eq!(text.color, Color32::from_rgb(200, 0, 0));
        let after = pixels(&app, idx);
        assert_ne!(after, before, "re-rendered");
        app.apply_history(false);
        assert_eq!(pixels(&app, idx), before, "undo restores the pixels");
        let text = app.canvas.layers[idx].text.as_ref().unwrap();
        assert_eq!(text.text, "Hi", "and the text");
        assert_eq!(text.style.size, 40.0);
        assert_eq!(text.color, Color32::BLACK);
        app.apply_history(true);
        assert_eq!(pixels(&app, idx), after);
        assert_eq!(
            app.canvas.layers[idx].text.as_ref().unwrap().text,
            "Hello there"
        );
    }

    #[test]
    fn text_layers_and_their_undo_steps_save_and_load() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let first = pixels(&app, idx);
        app.text_edit_layer(idx);
        app.workspace.text.session.as_mut().unwrap().text = "Hey\nyou".into();
        app.workspace.text.style.align = crate::canvas::text::TextAlign::Center;
        app.workspace.text.style.letter_spacing = 3.0;
        app.text_commit();
        let text = app.canvas.layers[idx].text.clone().unwrap();
        let edited = pixels(&app, idx);

        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let mut reopened = crate::project::tests::test_app_pub(loaded.canvas);
        reopened.layer_state.history = loaded.history;
        assert_eq!(reopened.canvas.layers[idx].text.as_deref(), Some(&*text));
        assert_eq!(pixels(&reopened, idx), edited);
        // The edit's undo step came along, text and all.
        reopened.apply_history(false);
        assert_eq!(pixels(&reopened, idx), first);
        assert_eq!(
            reopened.canvas.layers[idx].text.as_ref().unwrap().text,
            "Hi"
        );
        // Other layers load as they were.
        assert!(reopened.canvas.layers[0].text.is_none());
    }

    #[test]
    fn cancelling_an_edit_leaves_the_layer_as_it_was() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let before = pixels(&app, idx);
        let depth = undo_depth(&app);
        app.text_edit_layer(idx);
        app.workspace.text.session.as_mut().unwrap().text = "Changed".into();
        app.text_update();
        app.text_cancel();
        assert_eq!(pixels(&app, idx), before);
        assert_eq!(app.canvas.layers[idx].text.as_ref().unwrap().text, "Hi");
        assert_eq!(undo_depth(&app), depth);
    }

    #[test]
    fn painting_on_text_makes_it_pixels_in_the_same_undo_step() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let depth = undo_depth(&app);
        let before = pixels(&app, idx);
        app.brush_state.brush.brush_options.color = Color32::from_rgb(0, 0, 255);
        app.start_stroke_with_pressure(Vec2::new(10.0, 90.0), 1.0);
        app.add_stroke_point(Vec2::new(200.0, 90.0), 1.0);
        app.finish_stroke();
        app.release_canvas();
        assert!(app.canvas.layers[idx].text.is_none(), "rasterised");
        assert_eq!(undo_depth(&app), depth + 1, "one step");
        app.apply_history(false);
        assert_eq!(pixels(&app, idx), before);
        assert_eq!(
            app.canvas.layers[idx]
                .text
                .as_ref()
                .map(|t| t.text.as_str()),
            Some("Hi"),
            "undo brings the text layer back"
        );
        app.apply_history(true);
        assert!(app.canvas.layers[idx].text.is_none());
    }

    #[test]
    fn erasing_text_makes_it_pixels_in_the_same_undo_step() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let before = pixels(&app, idx);
        let depth = undo_depth(&app);
        let mask = crate::selection::SelectionMask::new(0, 0, 300, 50, vec![255; 300 * 50]);
        app.erase_under(mask, false);
        assert!(app.canvas.layers[idx].text.is_none());
        assert_eq!(undo_depth(&app), depth + 1);
        app.apply_history(false);
        assert_eq!(pixels(&app, idx), before);
        assert!(app.canvas.layers[idx].text.is_some());
    }

    #[test]
    fn rasterise_text_is_one_undo_step() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let before = pixels(&app, idx);
        let depth = undo_depth(&app);
        app.rasterise_text_layer(idx);
        assert!(!app.is_text_layer(idx));
        assert_eq!(pixels(&app, idx), before, "the pixels stay");
        assert_eq!(undo_depth(&app), depth + 1);
        // No longer text: clicking it starts new text.
        app.text_press(Vec2::new(30.0, 40.0));
        assert!(
            app.workspace
                .text
                .session
                .as_ref()
                .unwrap()
                .editing
                .is_none()
        );
        app.text_cancel();
        app.apply_history(false);
        assert!(app.is_text_layer(idx));
    }

    #[test]
    fn moving_a_text_layer_keeps_it_text() {
        use crate::app::tools::Tool;
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        app.active_tool = Tool::Transform(Default::default());
        crate::app::tools::transform::create_floating_layer(&mut app);
        if let Tool::Transform(ref mut info) = app.active_tool {
            info.offset = Vec2::new(50.0, 10.0);
        }
        crate::app::tools::transform::commit_floating_layer(&mut app);
        let text = app.canvas.layers[idx].text.as_ref().expect("still text");
        assert_eq!(text.pos, Vec2::new(70.0, 30.0));
        app.apply_history(false);
        assert_eq!(
            app.canvas.layers[idx].text.as_ref().unwrap().pos,
            Vec2::new(20.0, 20.0)
        );
    }
}
