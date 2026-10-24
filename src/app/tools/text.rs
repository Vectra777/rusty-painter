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

/// A font on offer: shipped with the app, or a face of a font file, opened
/// when chosen.
pub struct FontEntry {
    /// Family and style, as the font names itself ("Noto Sans Bold").
    pub name: String,
    pub family: String,
    /// The file's name without its extension: what text layers saved
    /// before font names were read call it.
    stem: String,
    file: Option<PathBuf>,
    /// The face within a collection (`.ttc`).
    index: u32,
    loaded: Option<FontArc>,
}

impl FontEntry {
    fn open(&mut self) -> Option<FontArc> {
        if self.loaded.is_none()
            && let Some(path) = &self.file
        {
            self.loaded = std::fs::read(path).ok().and_then(|bytes| {
                ab_glyph::FontVec::try_from_vec_and_index(bytes, self.index)
                    .ok()
                    .map(FontArc::new)
            });
        }
        self.loaded.clone()
    }
}

/// Families that cover many scripts, tried for characters the chosen font
/// lacks (after the shipped fonts).
const FALLBACK_FAMILIES: &[&str] = &[
    "Noto Sans",
    "DejaVu Sans",
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Droid Sans Fallback",
    "Noto Sans Symbols",
    "Noto Sans Symbols 2",
    "Segoe UI",
    "Segoe UI Symbol",
    "Microsoft YaHei",
    "Arial Unicode MS",
    "Roboto",
];

#[derive(Default)]
pub struct TextToolState {
    pub style: TextStyle,
    /// Index into `fonts`.
    pub font: usize,
    /// The shipped fonts, then the system's once they're listed.
    pub fonts: Vec<FontEntry>,
    /// The system's fonts being listed on another thread.
    scan: Option<std::sync::mpsc::Receiver<Vec<FontEntry>>>,
    /// The system's fonts are listed.
    pub scanned: bool,
    /// What the font list is narrowed to.
    pub search: String,
    /// Fonts for characters the chosen one lacks, once needed.
    fallback: Option<Vec<FontArc>>,
    pub session: Option<TextSession>,
    /// Text layers a brush stroke started on, in stroke order: made plain
    /// pixels when the stroke began, their text goes into the stroke's undo
    /// step when it's filed.
    pub(crate) stroke_rasterised: Vec<(LayerId, Box<TextLayer>)>,
}

/// What the pointer is doing to the text.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TextDrag {
    /// Moving it, held at this offset from its top-left.
    Move(Vec2),
    /// Selecting from the anchor to the pointer.
    Select,
    /// Widening its box.
    Width,
}

/// What the preview was drawn from: the text, its place, style, font,
/// colour and transform.
type Shown = (String, Vec2, TextStyle, usize, Color32, Option<[f64; 9]>);

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
    drag: Option<TextDrag>,
    /// The caret, as a character index; with `anchor`, the selection.
    pub caret: usize,
    pub anchor: usize,
    /// Text an input method is composing (shown, not yet typed).
    pub preedit: String,
    /// The last click, for double and triple clicks: when, where, how many.
    last_click: Option<(std::time::Instant, Vec2, u8)>,
    /// When the caret last moved (it shows steadily for a moment).
    pub moved_at: std::time::Instant,
    /// Text and caret before each edit, for undo while typing.
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// The last edit was typing (more typing joins its undo step).
    typing: bool,
    /// What the preview shows, to repaint when anything changes.
    shown: Option<Shown>,
    /// What the canvas draws over the text, worked out each frame.
    pub view: Option<TextView>,
    /// Editing an existing text layer: how it was.
    editing: Option<EditedLayer>,
    /// The Transform tool's map of the text being edited
    /// ([`TextLayer::transform`]); the session works in the text's own
    /// untransformed space.
    pub transform: Option<[f64; 9]>,
}

impl TextSession {
    /// The layer the text shows on.
    pub(crate) fn layer_id(&self) -> LayerId {
        self.layer
    }
}

/// The text's box, caret and selection, from its top-left, for drawing.
#[derive(Clone, Debug, Default)]
pub struct TextView {
    pub size: Vec2,
    pub caret: Vec2,
    pub caret_height: f32,
    pub selection: Vec<egui::Rect>,
}

/// A text layer as it was when its editing began.
struct EditedLayer {
    text: Box<TextLayer>,
    tiles: Vec<LayerTile>,
}

/// The faces in font file `path`, named from their own tables.
fn faces_of(path: &std::path::Path) -> Vec<FontEntry> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1).min(64);
    (0..count)
        .filter_map(|index| {
            let face = ttf_parser::Face::parse(&bytes, index).ok()?;
            let name = |ids: &[u16]| {
                ids.iter().find_map(|&id| {
                    face.names()
                        .into_iter()
                        .filter(|n| n.name_id == id && n.is_unicode())
                        .find_map(|n| n.to_string())
                })
            };
            use ttf_parser::name_id;
            let family = name(&[name_id::TYPOGRAPHIC_FAMILY, name_id::FAMILY])
                .unwrap_or_else(|| stem.clone());
            let style =
                name(&[name_id::TYPOGRAPHIC_SUBFAMILY, name_id::SUBFAMILY]).unwrap_or_default();
            let full = if style.is_empty() || style.eq_ignore_ascii_case("regular") {
                family.clone()
            } else {
                format!("{family} {style}")
            };
            Some(FontEntry {
                name: full,
                family,
                stem: stem.clone(),
                file: Some(path.to_path_buf()),
                index,
                loaded: None,
            })
        })
        .collect()
}

impl TextToolState {
    /// The shipped fonts at once; the system's listed on another thread
    /// (the list fills in when they're found).
    pub fn ensure_fonts(&mut self) {
        if self.fonts.is_empty() {
            self.fonts = text::builtin_fonts()
                .into_iter()
                .map(|(name, font)| FontEntry {
                    family: name.clone(),
                    stem: name.clone(),
                    name,
                    file: None,
                    index: 0,
                    loaded: Some(font),
                })
                .collect();
        }
        if !self.scanned && self.scan.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut found: Vec<FontEntry> = text::system_font_files()
                    .into_iter()
                    .flat_map(|(_, path)| faces_of(&path))
                    .collect();
                found.sort_by_key(|f| f.name.to_lowercase());
                found.dedup_by(|a, b| a.name == b.name);
                let _ = tx.send(found);
            });
            self.scan = Some(rx);
        }
        if let Some(rx) = &self.scan {
            match rx.try_recv() {
                Ok(found) => self.add_scanned(found),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.add_scanned(Vec::new()),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    fn add_scanned(&mut self, found: Vec<FontEntry>) {
        self.fonts.extend(found);
        self.scan = None;
        self.scanned = true;
        self.fallback = None;
    }

    /// Wait for the system's fonts (a text layer's font may be among them).
    fn wait_for_fonts(&mut self) {
        self.ensure_fonts();
        if let Some(rx) = self.scan.take() {
            self.add_scanned(rx.recv().unwrap_or_default());
        }
    }

    /// The chosen font, opened if need be (a file that won't open falls
    /// back to the first font).
    pub fn current_font(&mut self) -> Option<FontArc> {
        self.ensure_fonts();
        match self.fonts.get_mut(self.font).and_then(FontEntry::open) {
            Some(font) => Some(font),
            None => {
                self.font = 0;
                self.fonts.first().and_then(|f| f.loaded.clone())
            }
        }
    }

    /// The chosen font and, if `text` has characters it lacks, the fonts
    /// tried for them.
    pub fn fonts_for(&mut self, text: &str) -> Option<Vec<FontArc>> {
        let main = self.current_font()?;
        Some(self.with_fallback(main, text))
    }

    fn with_fallback(&mut self, main: FontArc, text: &str) -> Vec<FontArc> {
        use ab_glyph::Font;
        let lacks = text
            .chars()
            .any(|c| !c.is_whitespace() && main.glyph_id(c).0 == 0);
        if !lacks {
            return vec![main];
        }
        if self.fallback.is_none() {
            let mut fonts: Vec<FontArc> = self
                .fonts
                .iter()
                .filter_map(|f| f.loaded.clone())
                .take(2)
                .collect();
            for family in FALLBACK_FAMILIES {
                if let Some(entry) = self
                    .fonts
                    .iter_mut()
                    .find(|f| f.family.eq_ignore_ascii_case(family) && f.name == f.family)
                    && let Some(font) = entry.open()
                {
                    fonts.push(font);
                }
            }
            self.fallback = Some(fonts);
        }
        std::iter::once(main)
            .chain(self.fallback.iter().flatten().cloned())
            .collect()
    }

    /// The name of the chosen font.
    fn font_name(&self) -> String {
        self.fonts
            .get(self.font)
            .map_or_else(String::new, |f| f.name.clone())
    }

    /// Where the font called `name` is in the list (by its name, or its
    /// file's, as layers saved before named it).
    fn font_index(&mut self, name: &str) -> Option<usize> {
        self.ensure_fonts();
        let find = |fonts: &[FontEntry]| {
            fonts
                .iter()
                .position(|f| f.name == name)
                .or_else(|| fonts.iter().position(|f| f.stem == name))
        };
        if let Some(i) = find(&self.fonts) {
            return Some(i);
        }
        self.wait_for_fonts();
        find(&self.fonts)
    }

    /// The fonts a text layer uses (the first font if its own isn't here).
    pub(crate) fn fonts_of(&mut self, layer: &TextLayer) -> Option<Vec<FontArc>> {
        let index = self.font_index(&layer.font).unwrap_or(0);
        let main = self
            .fonts
            .get_mut(index)
            .and_then(FontEntry::open)
            .or_else(|| self.fonts.first().and_then(|f| f.loaded.clone()))?;
        Some(self.with_fallback(main, &layer.text))
    }
}

/// Character `index`'s byte offset in `s` (its end past the last).
fn byte_at(s: &str, index: usize) -> usize {
    s.char_indices().nth(index).map_or(s.len(), |(b, _)| b)
}

/// The word around caret `index` (letters, digits and `_`; otherwise
/// the one character there).
fn word_at(text: &str, index: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = index.min(chars.len().saturating_sub(1));
    if chars.is_empty() || !is_word(chars[at]) {
        return (index, (index + 1).min(chars.len()));
    }
    let mut a = at;
    while a > 0 && is_word(chars[a - 1]) {
        a -= 1;
    }
    let mut b = at;
    while b < chars.len() && is_word(chars[b]) {
        b += 1;
    }
    (a, b)
}

/// The caret moved a word left (`-1`) or right (`1`) from `index`.
fn word_step(text: &str, index: usize, dir: i32) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = index;
    if dir < 0 {
        while i > 0 && !is_word(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(chars[i - 1]) {
            i -= 1;
        }
    } else {
        while i < chars.len() && !is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
    }
    i
}

/// In screen points: how near a click must be to the text's handles.
const TEXT_HANDLE: f32 = 10.0;

impl PainterApp {
    /// The session's layout, from its top-left.
    /// Canvas point `pos` in the text being typed's own space (where it is
    /// before the Transform tool's map).
    fn text_local(&self, pos: Vec2) -> Vec2 {
        let transform = self
            .workspace
            .text
            .session
            .as_ref()
            .and_then(|s| s.transform);
        untransform(transform.as_ref(), pos)
    }

    fn text_layout(&mut self) -> Option<text::Layout> {
        let state = &mut self.workspace.text;
        let session = state.session.as_ref()?;
        let shown = format!("{}{}", session.text, session.preedit);
        let fonts = state.fonts_for(&shown)?;
        let session = state.session.as_ref()?;
        let mut t = session.text.clone();
        t.insert_str(byte_at(&t, session.caret), &session.preedit);
        Some(text::layout(&fonts, &t, &state.style))
    }

    /// The text's box on the canvas (at least a caret's size).
    pub(crate) fn text_box(&mut self) -> Option<egui::Rect> {
        let layout = self.text_layout()?;
        let session = self.workspace.text.session.as_ref()?;
        let size = Vec2::new(
            layout.size.x.max(1.0),
            layout.size.y.max(layout.text_height),
        );
        Some(egui::Rect::from_min_size(
            egui::pos2(session.pos.x, session.pos.y),
            egui::vec2(size.x, size.y),
        ))
    }

    /// A press with the Text tool. On the text being typed: the caret goes
    /// there (Shift: the selection grows to it; again quickly: a word,
    /// then a line), and a drag selects; on its move handle (or with Ctrl)
    /// it's moved; on a box's right edge, the box widens. Elsewhere it's
    /// kept, and a press with no text being typed starts some (on a text
    /// layer, edits it).
    pub(crate) fn text_press(&mut self, canvas_pos: Vec2, shift: bool, command: bool) {
        let pos = self.text_local(canvas_pos);
        let zoom = self.viewport.zoom.max(0.01);
        let handle = TEXT_HANDLE / zoom;
        if self.workspace.text.session.is_some()
            && let Some(rect) = self.text_box()
        {
            let session = self.workspace.text.session.as_ref().expect("a session");
            let origin = session.pos;
            let mover = egui::pos2(rect.left(), rect.top()) - egui::vec2(handle, handle);
            let near_mover = (egui::pos2(pos.x, pos.y) - mover).length() <= handle;
            let on_text = rect.expand(handle * 0.5).contains(egui::pos2(pos.x, pos.y));
            let boxed = self.workspace.text.style.width.is_some();
            let on_edge = boxed
                && (pos.x - rect.right()).abs() <= handle
                && pos.y >= rect.top()
                && pos.y <= rect.bottom();
            if near_mover || (command && on_text) {
                if let Some(s) = self.workspace.text.session.as_mut() {
                    s.drag = Some(TextDrag::Move(pos - origin));
                }
                return;
            }
            if on_edge {
                if let Some(s) = self.workspace.text.session.as_mut() {
                    s.drag = Some(TextDrag::Width);
                }
                return;
            }
            if on_text {
                let Some(layout) = self.text_layout() else {
                    return;
                };
                let at = layout.hit(pos - origin);
                let now = std::time::Instant::now();
                let s = self.workspace.text.session.as_mut().expect("a session");
                let count = match s.last_click {
                    Some((t, p, n))
                        if now.duration_since(t).as_millis() < 450
                            && (p - pos).length() * zoom < 6.0 =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                s.last_click = Some((now, pos, count));
                s.preedit.clear();
                match count {
                    2 => (s.anchor, s.caret) = word_at(&s.text, at),
                    3 => (s.anchor, s.caret) = layout.line_bounds(at),
                    _ => {
                        s.caret = at;
                        if !shift {
                            s.anchor = at;
                        }
                    }
                }
                s.drag = (count == 1).then_some(TextDrag::Select);
                s.typing = false;
                s.moved_at = now;
                return;
            }
            // Away from it: kept, and that's all.
            self.text_commit();
            return;
        }
        match self.text_layer_at(canvas_pos) {
            Some(idx) => self.text_edit_layer(idx),
            None => self.text_start(canvas_pos),
        }
    }

    /// The topmost visible, unlocked text layer whose text covers canvas
    /// point `pos`.
    fn text_layer_at(&mut self, pos: Vec2) -> Option<usize> {
        let margin = 4.0 / self.viewport.zoom.max(0.01);
        let hit = |canvas: &crate::canvas::Canvas, i: usize| -> Option<TextLayer> {
            let l = &canvas.layers[i];
            (l.visible && !l.locked)
                .then(|| l.text.as_deref().cloned())
                .flatten()
        };
        for i in (0..self.canvas.layers.len()).rev() {
            let Some(t) = hit(&self.canvas, i) else {
                continue;
            };
            let Some(fonts) = self.workspace.text.fonts_of(&t) else {
                continue;
            };
            let size = text::measure(&fonts, &t.text, &t.style);
            let pos = untransform(t.transform.as_ref(), pos);
            if egui::Rect::from_min_size(egui::pos2(t.pos.x, t.pos.y), egui::vec2(size.x, size.y))
                .expand(margin)
                .contains(egui::pos2(pos.x, pos.y))
            {
                return Some(i);
            }
        }
        None
    }

    fn new_session(
        &mut self,
        text: String,
        pos: Vec2,
        color: Color32,
        layer: LayerId,
        active_before: usize,
        editing: Option<EditedLayer>,
    ) {
        let end = text.chars().count();
        self.workspace.text.session = Some(TextSession {
            text,
            pos,
            color,
            brush_seen: self.brush_state.brush.brush_options.color,
            layer,
            active_before,
            drag: None,
            caret: end,
            anchor: if editing.is_some() { 0 } else { end },
            preedit: String::new(),
            last_click: None,
            moved_at: std::time::Instant::now(),
            undo: Vec::new(),
            redo: Vec::new(),
            typing: false,
            shown: None,
            view: None,
            editing,
            transform: None,
        });
    }

    fn text_start(&mut self, pos: Vec2) {
        let active = self.canvas.active_layer_idx;
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.puppet_commit();
        self.release_canvas();
        let (index, parent) = self.insertion_point(true);
        let id = self
            .canvas_mut()
            .insert_new_layer(index, "Text".into(), LayerKind::Paint, parent);
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        self.insert_layer_state(idx);
        // A new text starts as point text; a drag makes it a box.
        self.workspace.text.style.width = None;
        let color = self.brush_state.brush.brush_options.color;
        self.new_session(String::new(), pos, color, id, active, None);
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.drag = Some(TextDrag::Width);
        }
    }

    /// Open text layer `idx` for editing, with its text, font and settings
    /// loaded (with the Text tool), all its text selected. Does nothing for
    /// other layers.
    pub(crate) fn text_edit_layer(&mut self, idx: usize) {
        self.text_commit();
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.puppet_commit();
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
        let (text, pos, color) = (source.text.clone(), source.pos, source.color);
        let transform = source.transform;
        self.new_session(
            text,
            pos,
            color,
            id,
            idx,
            Some(EditedLayer {
                text: source,
                tiles,
            }),
        );
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.transform = transform;
        }
    }

    pub(crate) fn text_drag(&mut self, pos: Vec2) {
        let pos = self.text_local(pos);
        let Some(drag) = self.workspace.text.session.as_ref().and_then(|s| s.drag) else {
            return;
        };
        match drag {
            TextDrag::Move(grab) => {
                if let Some(s) = self.workspace.text.session.as_mut() {
                    s.pos = pos - grab;
                }
            }
            TextDrag::Select => {
                let Some(layout) = self.text_layout() else {
                    return;
                };
                if let Some(s) = self.workspace.text.session.as_mut() {
                    s.caret = layout.hit(pos - s.pos);
                    s.moved_at = std::time::Instant::now();
                }
            }
            TextDrag::Width => {
                let zoom = self.viewport.zoom.max(0.01);
                let state = &mut self.workspace.text;
                let Some(s) = state.session.as_ref() else {
                    return;
                };
                let width = pos.x - s.pos.x;
                // A new text's drag: a box once it's dragged a little.
                if state.style.width.is_some() || width * zoom > 12.0 {
                    state.style.width = Some(width.max(state.style.size));
                }
            }
        }
    }

    pub(crate) fn text_release(&mut self) {
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.drag = None;
        }
    }

    /// Keep the text and caret before an edit, for undo while typing
    /// (typing in a run is one step).
    fn text_checkpoint(s: &mut TextSession, typing: bool) {
        if !(typing && s.typing) {
            s.undo.push((s.text.clone(), s.caret));
            s.redo.clear();
        }
        s.typing = typing;
    }

    /// Put `insert` in place of the selection.
    fn text_replace(s: &mut TextSession, insert: &str) {
        let (a, b) = (s.caret.min(s.anchor), s.caret.max(s.anchor));
        let (ba, bb) = (byte_at(&s.text, a), byte_at(&s.text, b));
        s.text.replace_range(ba..bb, insert);
        s.caret = a + insert.chars().count();
        s.anchor = s.caret;
        s.moved_at = std::time::Instant::now();
    }

    /// The selected text.
    fn text_selected(s: &TextSession) -> String {
        let (a, b) = (s.caret.min(s.anchor), s.caret.max(s.anchor));
        s.text[byte_at(&s.text, a)..byte_at(&s.text, b)].to_string()
    }

    /// The keyboard while text is being typed, before any shortcut sees
    /// it: typing, the caret's keys, the clipboard, an input method, undo,
    /// and Ctrl+Enter (keep) or Esc (cancel). Every key is the text's
    /// meanwhile; returns whether anything happened.
    pub(crate) fn text_keys(&mut self, ctx: &egui::Context) -> bool {
        if self.workspace.text.session.is_none() || ctx.wants_keyboard_input() {
            return false;
        }
        let events = ctx.input_mut(|i| {
            // Kept: pointer and the rest. Taken: everything typed.
            let (keys, rest): (Vec<_>, Vec<_>) =
                std::mem::take(&mut i.events).into_iter().partition(|e| {
                    matches!(
                        e,
                        egui::Event::Text(_)
                            | egui::Event::Key { .. }
                            | egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Paste(_)
                            | egui::Event::Ime(_)
                    )
                });
            i.events = rest;
            keys
        });
        if events.is_empty() {
            return false;
        }
        let layout = self.text_layout();
        let mut keep = false;
        let mut cancel = false;
        let mut copied = None;
        let Some(s) = self.workspace.text.session.as_mut() else {
            return false;
        };
        let count = s.text.chars().count();
        for event in events {
            match event {
                egui::Event::Text(t) => {
                    let t: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !t.is_empty() {
                        Self::text_checkpoint(s, true);
                        Self::text_replace(s, &t);
                    }
                }
                egui::Event::Paste(t) => {
                    Self::text_checkpoint(s, false);
                    Self::text_replace(s, &t.replace("\r\n", "\n"));
                }
                egui::Event::Copy => copied = Some(Self::text_selected(s)),
                egui::Event::Cut => {
                    copied = Some(Self::text_selected(s));
                    Self::text_checkpoint(s, false);
                    Self::text_replace(s, "");
                }
                egui::Event::Ime(ime) => match ime {
                    egui::ImeEvent::Preedit(p) => s.preedit = p,
                    egui::ImeEvent::Commit(t) => {
                        s.preedit.clear();
                        Self::text_checkpoint(s, true);
                        Self::text_replace(s, &t);
                    }
                    egui::ImeEvent::Enabled | egui::ImeEvent::Disabled => s.preedit.clear(),
                },
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers: m,
                    ..
                } => {
                    use egui::Key;
                    let count = s
                        .text
                        .chars()
                        .count()
                        .max(count.min(s.text.chars().count()));
                    let go = |s: &mut TextSession, to: usize| {
                        s.caret = to.min(count);
                        if !m.shift {
                            s.anchor = s.caret;
                        }
                        s.typing = false;
                        s.moved_at = std::time::Instant::now();
                    };
                    let selected = s.caret != s.anchor;
                    match key {
                        Key::Escape => cancel = true,
                        Key::Enter if m.command => keep = true,
                        Key::Enter => {
                            Self::text_checkpoint(s, false);
                            Self::text_replace(s, "\n");
                        }
                        Key::Tab => {
                            Self::text_checkpoint(s, true);
                            Self::text_replace(s, "\t");
                        }
                        Key::Backspace | Key::Delete => {
                            if !selected {
                                let to = match (key, m.command || m.alt) {
                                    (Key::Backspace, true) => word_step(&s.text, s.caret, -1),
                                    (Key::Backspace, false) => s.caret.saturating_sub(1),
                                    (_, true) => word_step(&s.text, s.caret, 1),
                                    _ => (s.caret + 1).min(count),
                                };
                                s.anchor = to;
                            }
                            if s.caret != s.anchor {
                                Self::text_checkpoint(s, false);
                                Self::text_replace(s, "");
                            }
                        }
                        Key::ArrowLeft if selected && !m.shift => go(s, s.caret.min(s.anchor)),
                        Key::ArrowRight if selected && !m.shift => go(s, s.caret.max(s.anchor)),
                        Key::ArrowLeft => {
                            let to = if m.command || m.alt {
                                word_step(&s.text, s.caret, -1)
                            } else {
                                s.caret.saturating_sub(1)
                            };
                            go(s, to)
                        }
                        Key::ArrowRight => {
                            let to = if m.command || m.alt {
                                word_step(&s.text, s.caret, 1)
                            } else {
                                s.caret + 1
                            };
                            go(s, to)
                        }
                        Key::ArrowUp | Key::ArrowDown => {
                            let dir = if key == Key::ArrowUp { -1 } else { 1 };
                            let to = layout.as_ref().and_then(|l| l.vertical(s.caret, dir));
                            let edge = if dir < 0 { 0 } else { count };
                            go(s, to.unwrap_or(edge))
                        }
                        Key::Home | Key::End => {
                            let line = layout.as_ref().map(|l| l.line_bounds(s.caret));
                            let (a, b) = match (m.command, line) {
                                (false, Some(line)) => line,
                                _ => (0, count),
                            };
                            go(s, if key == Key::Home { a } else { b })
                        }
                        Key::A if m.command => {
                            s.anchor = 0;
                            s.caret = count;
                        }
                        Key::Z if m.command => {
                            let (from, to) = if m.shift {
                                (&mut s.redo, &mut s.undo)
                            } else {
                                (&mut s.undo, &mut s.redo)
                            };
                            if let Some((text, caret)) = from.pop() {
                                to.push((std::mem::replace(&mut s.text, text), s.caret));
                                s.caret = caret.min(s.text.chars().count());
                                s.anchor = s.caret;
                                s.typing = false;
                            }
                        }
                        Key::Y if m.command => {
                            if let Some((text, caret)) = s.redo.pop() {
                                s.undo.push((std::mem::replace(&mut s.text, text), s.caret));
                                s.caret = caret.min(s.text.chars().count());
                                s.anchor = s.caret;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if let Some(text) = copied.filter(|t| !t.is_empty()) {
            ctx.copy_text(text);
        }
        if keep {
            self.text_commit();
        } else if cancel {
            self.text_cancel();
        }
        true
    }

    /// Repaint the preview if the text, its place or its look changed; and
    /// work out the caret and selection to draw.
    pub(crate) fn text_update(&mut self) {
        if self.workspace.text.session.is_some()
            && let Some(layout) = self.text_layout()
            && let Some(s) = self.workspace.text.session.as_mut()
        {
            let preedit = s.preedit.chars().count();
            s.view = Some(TextView {
                size: Vec2::new(layout.size.x, layout.size.y.max(layout.text_height)),
                caret: layout.caret(s.caret + preedit),
                caret_height: layout.text_height,
                selection: if preedit > 0 {
                    // What the input method is composing, underlined.
                    layout.selection(s.caret, s.caret + preedit)
                } else {
                    layout.selection(s.anchor, s.caret)
                },
            });
        }
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
        let mut shown = session.text.clone();
        shown.insert_str(byte_at(&shown, session.caret), &session.preedit);
        let now = (
            shown,
            session.pos,
            style,
            font_index,
            session.color,
            session.transform,
        );
        if session.shown.as_ref() == Some(&now) {
            return;
        }
        let Some(idx) = self.canvas.layer_index_of(session.layer) else {
            self.workspace.text.session = None;
            return;
        };
        let Some(fonts) = self.workspace.text.fonts_for(&now.0) else {
            return;
        };
        self.release_canvas();
        self.clear_layer_tiles(idx);
        if let Some(mask) = text::render(&fonts, &now.0, &style, now.1, now.5.as_ref()) {
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
            transform: session.transform,
        }
    }

    /// A text layer's name from its text.
    fn text_layer_name(text: &str) -> String {
        let first: String = text.lines().next().unwrap_or("").chars().take(24).collect();
        format!("Text: {}", first.trim())
    }

    /// Keep the text: a new text layer, or the edited one, as one undo step
    /// (blank new text is dropped; an edited layer left blank is deleted).
    pub(crate) fn text_commit(&mut self) {
        if let Some(s) = self.workspace.text.session.as_mut() {
            s.preedit.clear();
        }
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
        self.add_layer_with_tiles(Self::text_layer_name(&session.text), tiles, |layer| {
            layer.text = Some(Box::new(source));
        });
    }

    /// Keep an edited text layer's new text and pixels as one undo step,
    /// which gives back both. A layer still named after its text is named
    /// after the new text.
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
        if self.canvas.layers[idx].name == Self::text_layer_name(&before.text.text) {
            self.canvas_mut().layers[idx].name = Self::text_layer_name(&source.text);
        }
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

    /// Text layer `id` was transformed by `info` as a whole: it stays
    /// text, the transform kept with it ([`TextLayer::transform`]) and the
    /// text drawn again through it, sharp; `tiles` (the step's) gain what
    /// that drawing changes. A warp, which no such map describes, leaves
    /// it to become pixels.
    pub(crate) fn text_layer_moved(
        &mut self,
        id: LayerId,
        info: &TransformInfo,
        tiles: &mut Vec<TileSnapshot>,
    ) -> Option<LayerHistoryOp> {
        let idx = self.canvas.layer_index_of(id)?;
        if !self.is_text_layer(idx) {
            return None;
        }
        let only_moved = info.warp.is_none()
            && info.corners.is_none()
            && info.rotation == 0.0
            && info.scale == Vec2::new(1.0, 1.0);
        let old = self.canvas.layers[idx].text.clone()?;
        let mut new = (*old).clone();
        if only_moved && new.transform.is_none() {
            new.pos += info.offset;
        } else {
            let m = info.bounds.and_then(|b| info.params().as_homography(b))?;
            new.transform = Some(match &new.transform {
                Some(h) => crate::canvas::storage::mul3(&m, h),
                None => m,
            });
        }
        self.canvas_mut().layers[idx].text = Some(Box::new(new));
        self.text_redraw_layer(idx, tiles);
        Some(LayerHistoryOp::Text {
            layers: vec![(id, Some(old))],
            inner: None,
        })
    }

    /// Draw text layer `idx` again from its text, `tiles` gaining how each
    /// tile it changes was (those it doesn't have yet: untouched till now).
    fn text_redraw_layer(&mut self, idx: usize, tiles: &mut Vec<TileSnapshot>) {
        let Some(source) = self.canvas.layers[idx].text.clone() else {
            return;
        };
        let Some(fonts) = self.workspace.text.fonts_of(&source) else {
            return;
        };
        let id = self.canvas.layers[idx].id;
        let ts = self.canvas.tile_size();
        let has = |tiles: &Vec<TileSnapshot>, key: (i32, i32)| {
            tiles
                .iter()
                .any(|t| t.layer_id == id && (t.tx, t.ty) == key)
        };
        let snapshot = |key: (i32, i32), data: Vec<Color32>| TileSnapshot {
            tx: key.0,
            ty: key.1,
            layer_id: id,
            x0: 0,
            y0: 0,
            width: ts,
            height: ts,
            data: data.into(),
        };
        self.release_canvas();
        for key in self.canvas.layer_tile_keys(idx) {
            if !has(tiles, key)
                && let Some(data) = self.canvas.get_layer_tile_data(idx, key.0, key.1)
            {
                tiles.push(snapshot(key, data));
            }
        }
        self.clear_layer_tiles(idx);
        let mask = text::render(
            &fonts,
            &source.text,
            &source.style,
            source.pos,
            source.transform.as_ref(),
        );
        if let Some(mask) = mask {
            let mut scratch = UndoAction {
                tiles: Vec::new(),
                selection: None,
                transform: None,
                layer_action: None,
            };
            if let Some(rect) = self
                .canvas
                .paint_mask(idx, &mask, source.color, &mut scratch)
            {
                self.mark_tiles_in_bounds_dirty(rect);
            }
            for t in scratch.tiles {
                if !has(tiles, (t.tx, t.ty)) {
                    tiles.push(snapshot((t.tx, t.ty), vec![Color32::TRANSPARENT; ts * ts]));
                }
            }
        }
        self.layer_state.thumbnails_dirty = true;
    }
}

/// `pos` taken back through the projective map `transform` (as it is).
fn untransform(transform: Option<&[f64; 9]>, pos: Vec2) -> Vec2 {
    match transform.and_then(crate::canvas::storage::invert3) {
        Some(inv) => crate::canvas::storage::apply_homography(&inv, pos),
        None => pos,
    }
}

/// Draw the text being typed: a frame round it, a handle to move it (and
/// a box's to widen it), the selection and a blinking caret. Tells the
/// system where the caret is, for an input method's window.
pub(crate) fn draw_text(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> egui::Pos2,
) {
    use crate::ui::style::accent;
    let Some(session) = &app.workspace.text.session else {
        return;
    };
    let Some(view) = &session.view else {
        return;
    };
    let map = |p: Vec2| match &session.transform {
        Some(h) => crate::canvas::storage::apply_homography(h, p),
        None => p,
    };
    let at = |p: Vec2| to_screen(map(session.pos + p));
    let frame = [
        at(Vec2::ZERO),
        at(Vec2::new(view.size.x, 0.0)),
        at(view.size),
        at(Vec2::new(0.0, view.size.y)),
        at(Vec2::ZERO),
    ];
    painter.add(egui::Shape::dashed_line(
        &frame,
        egui::Stroke::new(1.0_f32, accent()),
        4.0,
        3.0,
    ));
    // The move handle, up and left of the top-left corner.
    let corner = frame[0] - egui::vec2(TEXT_HANDLE, TEXT_HANDLE);
    let handle = egui::Rect::from_center_size(corner, egui::vec2(9.0, 9.0));
    painter.rect_filled(handle, 1.0, accent());
    painter.rect_stroke(handle, 1.0, egui::Stroke::new(1.0_f32, Color32::BLACK));
    if app.workspace.text.style.width.is_some() {
        // The box's width, at its right edge.
        let mid = egui::pos2(
            (frame[1].x + frame[2].x) / 2.0,
            (frame[1].y + frame[2].y) / 2.0,
        );
        let edge = egui::Rect::from_center_size(mid, egui::vec2(6.0, 14.0));
        painter.rect_filled(edge, 1.0, Color32::WHITE);
        painter.rect_stroke(edge, 1.0, egui::Stroke::new(1.0_f32, accent()));
    }
    let composing = !session.preedit.is_empty();
    for r in &view.selection {
        let (a, b) = (at(r.min.to_vec2()), at(r.max.to_vec2()));
        if composing {
            painter.line_segment(
                [at(egui::vec2(r.min.x, r.max.y)), b],
                egui::Stroke::new(2.0_f32, accent()),
            );
        } else {
            // Through the text's transform a box is any four-sided shape.
            let quad = vec![
                a,
                at(egui::vec2(r.max.x, r.min.y)),
                b,
                at(egui::vec2(r.min.x, r.max.y)),
            ];
            painter.add(egui::Shape::convex_polygon(
                quad,
                accent().gamma_multiply(0.35),
                egui::Stroke::NONE,
            ));
        }
    }
    // Blinking, steady for a moment after it moves.
    let since = session.moved_at.elapsed().as_secs_f32();
    let shown = since < 0.6 || ((since / 0.53) as u32).is_multiple_of(2);
    let top = at(view.caret);
    let bottom = at(view.caret + Vec2::new(0.0, view.caret_height));
    if shown {
        painter.line_segment(
            [top, bottom],
            egui::Stroke::new(3.0_f32, Color32::from_black_alpha(140)),
        );
        painter.line_segment([top, bottom], egui::Stroke::new(1.5_f32, Color32::WHITE));
    }
    let ctx = painter.ctx();
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    ctx.output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            rect: egui::Rect::from_two_pos(frame[0], frame[2]),
            cursor_rect: egui::Rect::from_two_pos(top, bottom + egui::vec2(1.0, 0.0)),
        });
    });
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
        app.text_press(pos, false, false);
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
        app.text_press(Vec2::new(20.0, 20.0), false, false);
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
        app.text_press(Vec2::new(20.0, 20.0), false, false);
        app.workspace.text.session.as_mut().unwrap().text = "Move".into();
        app.text_update();
        // Ctrl+press on the text: drag it rather than starting new text.
        app.text_press(Vec2::new(30.0, 30.0), false, true);
        app.text_drag(Vec2::new(130.0, 60.0));
        app.text_release();
        let s = app.workspace.text.session.as_ref().unwrap();
        assert_eq!(s.pos, Vec2::new(120.0, 50.0));
        app.text_cancel();
        assert_eq!(app.canvas.layers.len(), layers);
        assert_eq!(undo_depth(&app), 0);
    }

    /// `events` sent to the Text tool as one frame's keyboard input.
    fn keys(app: &mut crate::PainterApp, events: Vec<egui::Event>) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            app.text_keys(ctx);
            // Nothing typed is left for the shortcuts.
            assert!(ctx.input(|i| i.events.iter().all(|e| !matches!(e, egui::Event::Text(_)))));
        });
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    use eframe::egui;

    #[test]
    fn typing_on_the_canvas_edits_at_the_caret_and_undoes_in_runs() {
        let mut app = app();
        app.text_press(Vec2::new(20.0, 20.0), false, false);
        app.text_release();
        keys(&mut app, vec![egui::Event::Text("Helo".into())]);
        let none = egui::Modifiers::NONE;
        keys(&mut app, vec![key(egui::Key::ArrowLeft, none)]);
        keys(&mut app, vec![egui::Event::Text("l".into())]);
        let s = app.workspace.text.session.as_ref().unwrap();
        assert_eq!((s.text.as_str(), s.caret), ("Hello", 4));
        // Shift+Home selects to the start; typing replaces it.
        keys(&mut app, vec![key(egui::Key::Home, egui::Modifiers::SHIFT)]);
        keys(&mut app, vec![egui::Event::Text("J".into())]);
        assert_eq!(app.workspace.text.session.as_ref().unwrap().text, "Jo");
        // Undo while typing: back a step at a time.
        keys(&mut app, vec![key(egui::Key::Z, egui::Modifiers::COMMAND)]);
        assert_eq!(app.workspace.text.session.as_ref().unwrap().text, "Hello");
        keys(&mut app, vec![key(egui::Key::Z, egui::Modifiers::COMMAND)]);
        assert_eq!(app.workspace.text.session.as_ref().unwrap().text, "Helo");
        // A letter key isn't a shortcut meanwhile: B types a b.
        keys(
            &mut app,
            vec![key(egui::Key::End, none), egui::Event::Text("b".into())],
        );
        assert_eq!(app.workspace.text.session.as_ref().unwrap().text, "Helob");
        assert_eq!(
            app.active_tool,
            crate::app::tools::Tool::Brush,
            "no tool switch"
        );
        // Backspace, then Ctrl+Enter keeps it.
        keys(
            &mut app,
            vec![
                key(egui::Key::Backspace, none),
                key(egui::Key::Enter, egui::Modifiers::COMMAND),
            ],
        );
        assert!(app.workspace.text.session.is_none());
        let idx = app.canvas.active_layer_idx;
        assert_eq!(app.canvas.layers[idx].text.as_ref().unwrap().text, "Helo");
    }

    #[test]
    fn clicks_place_the_caret_and_select_words_and_a_click_away_keeps_it() {
        let mut app = app();
        app.text_press(Vec2::new(20.0, 20.0), false, false);
        app.text_release();
        keys(&mut app, vec![egui::Event::Text("one two".into())]);
        app.text_update();
        let view = app
            .workspace
            .text
            .session
            .as_ref()
            .unwrap()
            .view
            .clone()
            .unwrap();
        // A click at the far left of the text: the caret at its start.
        app.text_press(Vec2::new(21.0, 30.0), false, false);
        app.text_release();
        assert_eq!(app.workspace.text.session.as_ref().unwrap().caret, 0);
        // Twice on the second word: it's selected.
        let on_two = Vec2::new(20.0 + view.size.x - 5.0, 30.0);
        for _ in 0..2 {
            app.text_press(on_two, false, false);
            app.text_release();
        }
        let s = app.workspace.text.session.as_ref().unwrap();
        assert_eq!((s.anchor.min(s.caret), s.anchor.max(s.caret)), (4, 7));
        // Away from it: kept, and no new text started.
        let layers = app.canvas.layers.len();
        app.text_press(Vec2::new(250.0, 100.0), false, false);
        assert!(app.workspace.text.session.is_none());
        assert_eq!(
            app.canvas.layers.len(),
            layers,
            "the preview became the layer"
        );
    }

    #[test]
    fn a_dragged_out_box_wraps_its_text() {
        let mut app = app();
        app.text_press(Vec2::new(10.0, 10.0), false, false);
        app.text_drag(Vec2::new(110.0, 10.0));
        app.text_release();
        assert_eq!(app.workspace.text.style.width, Some(100.0));
        keys(
            &mut app,
            vec![egui::Event::Text("wrap these words please".into())],
        );
        app.text_update();
        let view = app
            .workspace
            .text
            .session
            .as_ref()
            .unwrap()
            .view
            .clone()
            .unwrap();
        assert_eq!(view.size.x, 100.0);
        assert!(view.size.y > 80.0, "several lines: {}", view.size.y);
        // A click without a drag is point text.
        app.text_commit();
        app.text_press(Vec2::new(250.0, 100.0), false, false);
        app.text_release();
        assert_eq!(app.workspace.text.style.width, None);
    }

    #[test]
    fn blank_text_is_not_kept() {
        let mut app = app();
        let layers = app.canvas.layers.len();
        app.text_press(Vec2::new(20.0, 20.0), false, false);
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
        app.text_press(Vec2::new(30.0, 40.0), false, false);
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
        app.workspace.text.session.as_mut().unwrap().transform =
            Some([1.2, 0.1, 5.0, -0.1, 1.2, 7.0, 0.0, 0.0, 1.0]);
        app.text_commit();
        let text = app.canvas.layers[idx].text.clone().unwrap();
        assert!(text.transform.is_some());
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
    fn painting_on_text_goes_on_a_new_layer_above_it() {
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let before = pixels(&app, idx);
        let layers = app.canvas.layers.len();
        app.brush_state.brush.brush_options.color = Color32::from_rgb(0, 0, 255);
        app.start_stroke_with_pressure(Vec2::new(10.0, 90.0), 1.0);
        app.add_stroke_point(Vec2::new(200.0, 90.0), 1.0);
        app.finish_stroke();
        app.release_canvas();
        assert_eq!(app.canvas.layers.len(), layers + 1, "a new layer");
        assert_eq!(app.canvas.active_layer_idx, idx + 1, "above the text");
        assert!(app.is_text_layer(idx), "the text stays text");
        assert_eq!(pixels(&app, idx), before, "untouched");
        assert!(
            !pixels(&app, idx + 1).is_empty(),
            "the stroke is on the new layer"
        );
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
        app.text_press(Vec2::new(30.0, 40.0), false, false);
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

    #[test]
    fn turning_and_scaling_a_text_layer_keeps_it_text_and_sharp() {
        use crate::app::tools::Tool;
        let mut app = app();
        let idx = type_text(&mut app, Vec2::new(20.0, 20.0), "Hi");
        let before = pixels(&app, idx);
        let ink_before = ink(&app, idx);
        app.active_tool = Tool::Transform(Default::default());
        crate::app::tools::transform::create_floating_layer(&mut app);
        if let Tool::Transform(ref mut info) = app.active_tool {
            info.rotation = 0.3;
            info.scale = Vec2::new(1.5, 1.5);
        }
        crate::app::tools::transform::commit_floating_layer(&mut app);
        let text = app.canvas.layers[idx].text.as_ref().expect("still text");
        assert!(text.transform.is_some(), "the transform is kept");
        // Drawn from the outlines: about 1.5² times the ink, and edges as
        // crisp as text drawn straight (few half-covered pixels).
        let ink_after = ink(&app, idx) as f32;
        let ratio = ink_after / ink_before as f32;
        assert!((1.8..2.7).contains(&ratio), "{ratio}");
        // Clicking the turned text edits it.
        let centre = {
            let t = app.canvas.layers[idx].text.as_ref().unwrap();
            let h = t.transform.unwrap();
            crate::canvas::storage::apply_homography(&h, t.pos + Vec2::new(20.0, 20.0))
        };
        app.active_tool = Tool::Text;
        app.text_press(centre, false, false);
        let s = app.workspace.text.session.as_ref().expect("editing");
        assert!(s.editing.is_some() && s.transform.is_some());
        app.text_cancel();
        app.apply_history(false);
        assert_eq!(pixels(&app, idx), before, "undo puts the pixels back");
        assert!(
            app.canvas.layers[idx]
                .text
                .as_ref()
                .unwrap()
                .transform
                .is_none()
        );
        app.apply_history(true);
        assert!(
            app.canvas.layers[idx]
                .text
                .as_ref()
                .unwrap()
                .transform
                .is_some()
        );
    }
}
