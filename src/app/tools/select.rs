//! The Select tool at the app level: selection changes as undo steps, the
//! click tools (magic wand, colour range), the magnetic lasso, and the
//! Select menu's changes (grow, shrink, feather, border, a layer's paint,
//! saved selections).
//!
//! [`SelectionManager`](crate::selection::SelectionManager) owns the shapes;
//! this wraps each change (a drag, a click, select all, invert, deselect) so
//! it's recorded on the active layer's history and undo brings the previous
//! selection back.

use crate::app::PainterApp;
use crate::canvas::Canvas;
use crate::canvas::fill::{self, ColorMatch, FillSettings};
use crate::canvas::history::UndoAction;
use crate::canvas::storage::SampleLayers;
use crate::selection::magnetic::{self, LiveWire};
use crate::selection::{SelectionMask, SelectionMode, SelectionShape, SelectionType};
use eframe::egui::{self, Vec2};

/// What the click tools look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SampleSource {
    /// The active layer.
    Layer,
    /// Everything visible.
    AllVisible,
    /// The layers marked as reference.
    Reference,
}

/// Magic wand settings.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WandSettings {
    /// How different (per channel, 0..=255) a colour may be and still match.
    pub tolerance: u8,
    /// Only the connected area (otherwise the colour everywhere).
    pub contiguous: bool,
    pub source: SampleSource,
    /// Don't leak through gaps in lines up to this wide (px).
    pub gap: u8,
    /// Grow (> 0) or shrink (< 0) the result by this many pixels.
    pub grow: i32,
    /// Soft one-pixel edge.
    pub antialias: bool,
}

impl Default for WandSettings {
    fn default() -> Self {
        Self {
            tolerance: 32,
            contiguous: true,
            source: SampleSource::AllVisible,
            gap: 0,
            grow: 0,
            antialias: true,
        }
    }
}

/// Colour range settings: perceptual distance, in percent.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ColorRangeSettings {
    pub tolerance: f32,
    /// Colours a little further than the tolerance are partly selected.
    pub softness: f32,
    pub source: SampleSource,
}

impl Default for ColorRangeSettings {
    fn default() -> Self {
        Self {
            tolerance: 8.0,
            softness: 6.0,
            source: SampleSource::AllVisible,
        }
    }
}

/// The last wand or colour-range click, re-run when its settings change.
struct LastPick {
    kind: SelectionType,
    pos: Vec2,
    mode: SelectionMode,
    before: Option<SelectionShape>,
    /// What it selected, to tell whether the selection changed since.
    result: Option<SelectionShape>,
}

/// Whether `a` is still the very selection `b` (not just an equal one).
fn same_selection(a: &Option<SelectionShape>, b: &Option<SelectionShape>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(SelectionShape::Mask(a)), Some(SelectionShape::Mask(b))) => {
            std::sync::Arc::ptr_eq(a, b)
        }
        _ => false,
    }
}

/// A magnetic lasso or polygon being drawn.
pub struct MagneticSession {
    /// The outline so far (through the anchors).
    pub points: Vec<Vec2>,
    /// Indices into `points` of the anchors, starting with the first point.
    anchors: Vec<usize>,
    /// Shortest paths from the last anchor; none for a polygon, whose
    /// edges are straight.
    wire: Option<LiveWire>,
    /// From the last anchor to the cursor, following edges.
    pub preview: Vec<Vec2>,
    cursor: Vec2,
    mode: SelectionMode,
    before: Option<SelectionShape>,
}

impl MagneticSession {
    /// Anchor positions, for the overlay.
    pub fn anchor_points(&self) -> impl Iterator<Item = Vec2> + '_ {
        self.anchors.iter().map(|&i| self.points[i])
    }
}

/// Select tool settings and state that outlives a single drag.
#[derive(Default)]
pub struct SelectToolState {
    pub wand: WandSettings,
    pub color: ColorRangeSettings,
    /// The selection before the current drag, recorded when it ends.
    before: Option<Option<SelectionShape>>,
    last_pick: Option<LastPick>,
    pub magnetic: Option<MagneticSession>,
    /// Select → Modify: the last radius used for each change.
    pub modify: ModifyRadii,
    /// The Modify or Save Selection dialog, while open.
    pub dialog: Option<crate::ui::select_dialog::SelectDialog>,
    /// Selections saved with the document (Select → Save Selection).
    pub saved: Vec<SavedSelection>,
    /// Quick mask, while it's on.
    pub quick_mask: Option<crate::app::tools::quick_mask::QuickMaskSession>,
}

/// Select → Modify: what happens to the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionModify {
    /// Push the edge outwards.
    Grow,
    /// Pull the edge inwards.
    Shrink,
    /// Soften the edge.
    Feather,
    /// A band along the edge.
    Border,
}

impl SelectionModify {
    pub const ALL: [SelectionModify; 4] = [
        SelectionModify::Grow,
        SelectionModify::Shrink,
        SelectionModify::Feather,
        SelectionModify::Border,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SelectionModify::Grow => "Grow",
            SelectionModify::Shrink => "Shrink",
            SelectionModify::Feather => "Feather",
            SelectionModify::Border => "Border",
        }
    }

    /// What the radius means, for the dialog.
    pub fn radius_label(self) -> &'static str {
        match self {
            SelectionModify::Border => "Width",
            _ => "Radius",
        }
    }

    /// `mask` changed by `radius` pixels.
    pub fn apply(self, mask: &SelectionMask, radius: u32) -> Option<SelectionMask> {
        let r = radius.min(i32::MAX as u32) as i32;
        match self {
            SelectionModify::Grow => mask.grown(r),
            SelectionModify::Shrink => mask.grown(-r),
            SelectionModify::Feather => mask.feathered(radius),
            SelectionModify::Border => mask.border(radius),
        }
    }
}

/// The last radius picked for each [`SelectionModify`], in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifyRadii([u32; 4]);

impl Default for ModifyRadii {
    fn default() -> Self {
        Self([4, 4, 8, 6])
    }
}

impl ModifyRadii {
    pub fn get(&self, op: SelectionModify) -> u32 {
        self.0[op as usize]
    }

    pub fn set(&mut self, op: SelectionModify, radius: u32) {
        self.0[op as usize] = radius;
    }
}

/// A selection kept under a name (Select → Save Selection), saved in the
/// project file.
#[derive(Clone, Debug)]
pub struct SavedSelection {
    pub name: String,
    pub mask: std::sync::Arc<SelectionMask>,
}

/// Magnetic lasso distances, in screen points (divided by the zoom).
const WIRE_MARGIN: f32 = 80.0;
/// Past this length the wire fixes an anchor on its own...
const AUTO_ANCHOR: f32 = 180.0;
/// ...this far back from the cursor, where the path has settled.
const AUTO_ANCHOR_TAIL: f32 = 50.0;
const CLOSE_DISTANCE: f32 = 10.0;
/// In canvas pixels, the most the automatic anchoring lets a wire span (so
/// the path search stays fast when zoomed far out).
const MAX_WIRE_SPAN: f32 = 320.0;

impl PainterApp {
    /// Record that the selection changed from `prev`, as an undo step.
    pub(crate) fn record_selection_change(&mut self, prev: Option<SelectionShape>) {
        self.workspace.select.last_pick = None;
        if prev.is_none() && self.selection_manager.current_shape.is_none() {
            return;
        }
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: Some(prev),
            transform: None,
            layer_action: None,
        });
    }

    /// Forget the last wand/colour click (the selection changed otherwise).
    pub(crate) fn forget_last_pick(&mut self) {
        self.workspace.select.last_pick = None;
    }

    /// Press with the Select tool: start a drag, pick by colour, or place a
    /// magnetic lasso point.
    pub(crate) fn select_press(&mut self, pos: Vec2, kind: SelectionType, mode: SelectionMode) {
        match kind {
            SelectionType::Wand | SelectionType::ColorRange => self.select_pick(pos, kind, mode),
            SelectionType::Magnetic => self.magnetic_press(pos, mode, true),
            SelectionType::Polygon => self.magnetic_press(pos, mode, false),
            _ => {
                // A smart patch fills exactly what's painted.
                let mode = if kind == SelectionType::Brush && self.workspace.patch.smart_patch {
                    SelectionMode::Replace
                } else {
                    mode
                };
                self.workspace.select.before = Some(self.selection_manager.current_shape.clone());
                self.selection_manager
                    .start_selection_with_mode(pos, kind, mode);
            }
        }
    }

    /// Movement with the Select tool, pressed or not.
    pub(crate) fn select_move(&mut self, pos: Vec2) {
        if self.workspace.select.magnetic.is_some() {
            self.magnetic_move(pos);
        } else if self.selection_manager.is_dragging {
            self.selection_manager.update_selection(pos);
        }
    }

    /// Finish the selection drag, as one undo step.
    pub(crate) fn select_release(&mut self) {
        if self.workspace.select.magnetic.is_some() {
            self.magnetic_release();
            return;
        }
        if !self.selection_manager.is_dragging {
            return;
        }
        let brush = self.selection_manager.painting_brush();
        self.selection_manager.end_selection();
        // Smart patch: what the selection brush painted is filled from its
        // surroundings, and the selection goes back to what it was.
        if brush && self.workspace.patch.smart_patch {
            let painted = self.selection_manager.current_shape.take();
            self.selection_manager.current_shape = self.workspace.select.before.take().flatten();
            if let Some(crate::selection::SelectionShape::Mask(mask)) = painted {
                self.fill_hole((*mask).clone());
            }
            return;
        }
        if let Some(prev) = self.workspace.select.before.take() {
            self.record_selection_change(prev);
        }
    }

    /// Abandon the selection drag (or magnetic outline), putting the
    /// previous selection back.
    pub(crate) fn select_cancel(&mut self) {
        self.workspace.select.magnetic = None;
        if !self.selection_manager.is_dragging {
            return;
        }
        self.selection_manager.clear_selection();
        if let Some(prev) = self.workspace.select.before.take() {
            self.selection_manager.current_shape = prev;
        }
    }

    /// Change the selection with `f`, as one undo step.
    fn change_selection(&mut self, f: impl FnOnce(&mut crate::selection::SelectionManager)) {
        self.select_cancel();
        let prev = self.selection_manager.current_shape.clone();
        f(&mut self.selection_manager);
        self.record_selection_change(prev);
    }

    pub(crate) fn select_all(&mut self) {
        self.change_selection(|s| s.select_all());
    }

    pub(crate) fn invert_selection(&mut self) {
        self.change_selection(|s| s.invert());
    }

    pub(crate) fn deselect(&mut self) {
        if self.selection_manager.has_selection() {
            self.change_selection(|s| s.clear_selection());
        }
    }

    // --- Modify, layer paint, saved selections ----------------------------

    /// Grow, shrink, feather or border the selection, as one undo step.
    pub(crate) fn modify_selection(&mut self, op: SelectionModify, radius: u32) {
        if !self.selection_manager.has_selection() {
            return;
        }
        self.workspace.select.modify.set(op, radius);
        let pool = self.workspace.pool.clone();
        self.change_selection(|s| pool.install(|| s.modify(|m| op.apply(m, radius))));
    }

    /// Select the paint of layer `idx` (its alpha as coverage; a mask's
    /// grey), combined with the selection by `mode`, as one undo step.
    pub(crate) fn select_layer_paint(&mut self, idx: usize, mode: SelectionMode) {
        if idx >= self.canvas.layers.len() {
            return;
        }
        self.release_canvas();
        let canvas = &self.canvas;
        let mask = self
            .workspace
            .pool
            .install(|| layer_paint_mask(canvas, idx));
        self.change_selection(|s| s.apply_mask_or_nothing(mask, mode));
    }
    /// Keep the selection under `name` (replacing one of the same name).
    pub(crate) fn save_selection(&mut self, name: String) {
        let Some(mask) = self.selection_manager.current_mask() else {
            return;
        };
        let saved = SavedSelection {
            name,
            mask: std::sync::Arc::new(mask),
        };
        let list = &mut self.workspace.select.saved;
        match list.iter_mut().find(|s| s.name == saved.name) {
            Some(slot) => *slot = saved,
            None => list.push(saved),
        }
    }

    /// Bring back saved selection `index`, combined by `mode`, as one undo
    /// step.
    pub(crate) fn load_selection(&mut self, index: usize, mode: SelectionMode) {
        let Some(saved) = self.workspace.select.saved.get(index) else {
            return;
        };
        let mask = (*saved.mask).clone();
        self.change_selection(|s| s.apply_mask(mask, mode));
    }

    pub(crate) fn delete_saved_selection(&mut self, index: usize) {
        if index < self.workspace.select.saved.len() {
            self.workspace.select.saved.remove(index);
        }
    }

    /// A name for the next saved selection.
    pub(crate) fn next_saved_selection_name(&self) -> String {
        let saved = &self.workspace.select.saved;
        (1..)
            .map(|n| format!("Selection {n}"))
            .find(|name| saved.iter().all(|s| &s.name != name))
            .unwrap_or_default()
    }

    // --- Magic wand and colour range ---------------------------------------

    fn select_pick(&mut self, pos: Vec2, kind: SelectionType, mode: SelectionMode) {
        self.select_cancel();
        let before = self.selection_manager.current_shape.clone();
        let mask = self.pick_mask(kind, pos);
        self.selection_manager.apply_mask_or_nothing(mask, mode);
        self.record_selection_change(before.clone());
        self.workspace.select.last_pick = Some(LastPick {
            kind,
            pos,
            mode,
            before,
            result: self.selection_manager.current_shape.clone(),
        });
    }

    /// Re-run the last wand/colour click with the current settings (they
    /// changed), replacing its result; it stays one undo step.
    pub(crate) fn rerun_last_pick(&mut self) {
        let Some(mut pick) = self.workspace.select.last_pick.take() else {
            return;
        };
        if !same_selection(&self.selection_manager.current_shape, &pick.result) {
            return;
        }
        self.selection_manager.current_shape = pick.before.clone();
        let mask = self.pick_mask(pick.kind, pick.pos);
        self.selection_manager
            .apply_mask_or_nothing(mask, pick.mode);
        pick.result = self.selection_manager.current_shape.clone();
        self.workspace.select.last_pick = Some(pick);
    }

    fn sample_layer(&self, source: SampleSource) -> SampleLayers {
        match source {
            SampleSource::Layer => SampleLayers::Layer(self.canvas.active_layer_idx),
            SampleSource::AllVisible => SampleLayers::AllVisible,
            SampleSource::Reference => SampleLayers::Reference,
        }
    }

    /// What a wand or colour-range click at `pos` selects.
    fn pick_mask(&self, kind: SelectionType, pos: Vec2) -> Option<SelectionMask> {
        let canvas = &self.canvas;
        let (w, h) = (canvas.width(), canvas.height());
        let (x, y) = (pos.x.floor() as i32, pos.y.floor() as i32);
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return None;
        }
        let pool = &self.workspace.pool;
        match kind {
            SelectionType::Wand => {
                let s = self.workspace.select.wand;
                let source = self.sample_layer(s.source);
                let reference = |x, y, w, h| canvas.render_sample(source, x, y, w, h);
                let mask = pool.install(|| {
                    if s.contiguous {
                        let settings = FillSettings {
                            tolerance: s.tolerance,
                            gap: s.gap,
                            expand: 0,
                            antialias: s.antialias,
                        };
                        fill::bucket_fill(&reference, w, h, (x, y), &settings)
                    } else {
                        let target = canvas.render_sample(source, x, y, 1, 1)[0];
                        let matching = ColorMatch::Channels {
                            tolerance: s.tolerance,
                        };
                        fill::select_color(&reference, w, h, target, matching)
                    }
                })?;
                if s.grow != 0 {
                    pool.install(|| mask.grown(s.grow))
                } else {
                    Some(mask)
                }
            }
            SelectionType::ColorRange => {
                let s = self.workspace.select.color;
                let source = self.sample_layer(s.source);
                let reference = |x, y, w, h| canvas.render_sample(source, x, y, w, h);
                let target = canvas.render_sample(source, x, y, 1, 1)[0];
                let matching = ColorMatch::Perceptual {
                    tolerance: s.tolerance,
                    softness: s.softness,
                };
                pool.install(|| fill::select_color(&reference, w, h, target, matching))
            }
            _ => None,
        }
    }

    // --- Magnetic lasso ----------------------------------------------------

    fn wire_from(&self, anchor: Vec2, cursor: Vec2) -> LiveWire {
        let canvas = &self.canvas;
        let margin = (WIRE_MARGIN / self.viewport.zoom.max(0.01)).clamp(24.0, 160.0);
        let window =
            magnetic::window_around(anchor, cursor, margin, canvas.width(), canvas.height());
        let reference = |x, y, w, h| canvas.render_reference(None, x, y, w, h);
        LiveWire::new(&reference, anchor, window)
    }

    fn magnetic_press(&mut self, pos: Vec2, mode: SelectionMode, snap: bool) {
        if self.workspace.select.magnetic.is_some() {
            // Anchors are placed on release (a click, or the end of a drag).
            self.magnetic_move(pos);
            return;
        }
        self.select_cancel();
        let wire = snap.then(|| self.wire_from(pos, pos));
        self.workspace.select.magnetic = Some(MagneticSession {
            points: vec![pos],
            anchors: vec![0],
            wire,
            preview: vec![pos],
            cursor: pos,
            mode,
            before: self.selection_manager.current_shape.clone(),
        });
    }

    fn magnetic_move(&mut self, pos: Vec2) {
        let zoom = self.viewport.zoom.max(0.01);
        let Some(anchor) = self
            .workspace
            .select
            .magnetic
            .as_ref()
            .map(|s| s.points[*s.anchors.last().unwrap_or(&0)])
        else {
            return;
        };
        let needs_wire = self
            .workspace
            .select
            .magnetic
            .as_ref()
            .is_some_and(|s| s.wire.as_ref().is_some_and(|w| !w.reaches(pos)));
        let new_wire = needs_wire.then(|| self.wire_from(anchor, pos));
        let Some(session) = self.workspace.select.magnetic.as_mut() else {
            return;
        };
        session.cursor = pos;
        let Some(wire) = session.wire.as_mut() else {
            // A polygon: a straight edge to the cursor.
            session.preview = vec![anchor, pos];
            return;
        };
        if let Some(new_wire) = new_wire {
            *wire = new_wire;
        }
        session.preview = wire.path_to(pos).unwrap_or_else(|| vec![anchor, pos]);
        // A long wire fixes an anchor where its path has settled, so the
        // outline doesn't jump about and the search stays small.
        let auto = (AUTO_ANCHOR / zoom).min(MAX_WIRE_SPAN);
        let len = magnetic::path_length(&session.preview);
        if len > auto {
            let keep = len - (AUTO_ANCHOR_TAIL / zoom).min(auto * 0.5);
            let mut walked = 0.0;
            let mut cut = 1;
            for (i, seg) in session.preview.windows(2).enumerate() {
                walked += (seg[1] - seg[0]).length();
                cut = i + 1;
                if walked >= keep {
                    break;
                }
            }
            let new_anchor = session.preview[cut];
            session.points.extend_from_slice(&session.preview[1..=cut]);
            session.anchors.push(session.points.len() - 1);
            let wire = self.wire_from(new_anchor, pos);
            if let Some(session) = self.workspace.select.magnetic.as_mut() {
                session.preview = wire.path_to(pos).unwrap_or_else(|| vec![new_anchor, pos]);
                session.wire = Some(wire);
            }
        }
    }

    fn magnetic_release(&mut self) {
        let zoom = self.viewport.zoom.max(0.01);
        let Some(session) = self.workspace.select.magnetic.as_ref() else {
            return;
        };
        let first = session.points[0];
        let cursor = session.cursor;
        let last_anchor = session.points[*session.anchors.last().unwrap_or(&0)];
        // Back at the start: close the outline.
        if session.points.len() > 2 && (cursor - first).length() <= CLOSE_DISTANCE / zoom {
            self.magnetic_close();
            return;
        }
        if (cursor - last_anchor).length() < 1.0 {
            return;
        }
        let preview = session.preview.clone();
        let wire = session
            .wire
            .is_some()
            .then(|| self.wire_from(cursor, cursor));
        if let Some(session) = self.workspace.select.magnetic.as_mut() {
            session.points.extend_from_slice(&preview[1..]);
            session.anchors.push(session.points.len() - 1);
            session.wire = wire;
            session.preview = vec![cursor];
        }
    }

    /// Remove the last anchor (and the outline back to the one before).
    pub(crate) fn magnetic_undo_anchor(&mut self) {
        let Some(session) = self.workspace.select.magnetic.as_mut() else {
            return;
        };
        if session.anchors.len() <= 1 {
            self.workspace.select.magnetic = None;
            return;
        }
        session.anchors.pop();
        let last = *session.anchors.last().unwrap_or(&0);
        session.points.truncate(last + 1);
        let (anchor, cursor) = (session.points[last], session.cursor);
        if session.wire.is_none() {
            session.preview = vec![anchor, cursor];
            return;
        }
        let wire = self.wire_from(anchor, cursor);
        if let Some(session) = self.workspace.select.magnetic.as_mut() {
            session.preview = wire.path_to(cursor).unwrap_or_else(|| vec![anchor]);
            session.wire = Some(wire);
        }
    }

    /// Close the magnetic outline and make it the selection.
    pub(crate) fn magnetic_close(&mut self) {
        let Some(mut session) = self.workspace.select.magnetic.take() else {
            return;
        };
        session
            .points
            .extend_from_slice(&session.preview[1.min(session.preview.len())..]);
        // Back to the start along the edges when it's in reach.
        let first = session.points[0];
        let last = *session.points.last().unwrap_or(&first);
        let back = (session.wire.is_some() && (last - first).length() <= MAX_WIRE_SPAN)
            .then(|| self.wire_from(last, first).path_to(first))
            .flatten();
        if let Some(back) = back {
            session.points.extend_from_slice(&back[1.min(back.len())..]);
        }
        let points = magnetic::simplify(&session.points, 0.4);
        if points.len() < 3 {
            return;
        }
        self.selection_manager
            .apply_shape(crate::selection::new_lasso_shape(points), session.mode);
        self.record_selection_change(session.before);
    }
}

/// Layer `idx`'s coverage over the canvas: alpha for paint, grey for a
/// mask. `None` if it has none (or is a folder).
pub(crate) fn layer_paint_mask(canvas: &Canvas, idx: usize) -> Option<SelectionMask> {
    use crate::canvas::storage::LayerKind;
    use rayon::prelude::*;
    let layer = canvas.layers.get(idx)?;
    let is_mask = match layer.kind {
        LayerKind::Group => return None,
        LayerKind::Mask { .. } => true,
        _ => false,
    };
    let (w, h, ts) = (canvas.width(), canvas.height(), canvas.tile_size());
    let (tiles_x, tiles_y) = (w.div_ceil(ts), h.div_ceil(ts));
    // Unpainted tiles read as the background colour on the bottom
    // layer, white on a mask, and nothing elsewhere.
    let blank = if idx == 0 {
        canvas.clear_color().a()
    } else if is_mask {
        255
    } else {
        0
    };
    // Only the tiles it holds, unless blank tiles count.
    let keys = canvas.layer_tile_keys(idx);
    let [tx0, ty0, tx1, ty1] = if blank > 0 {
        [0, 0, tiles_x as i32, tiles_y as i32]
    } else {
        let in_canvas = keys
            .iter()
            .filter(|&&(tx, ty)| {
                tx >= 0 && ty >= 0 && (tx as usize) < tiles_x && (ty as usize) < tiles_y
            })
            .copied();
        in_canvas.fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |b, (tx, ty)| {
            [
                b[0].min(tx),
                b[1].min(ty),
                b[2].max(tx + 1),
                b[3].max(ty + 1),
            ]
        })
    };
    if tx1 <= tx0 || ty1 <= ty0 {
        return None;
    }
    let (x0, y0) = (tx0 as usize * ts, ty0 as usize * ts);
    let x1 = (tx1 as usize * ts).min(w);
    let y1 = (ty1 as usize * ts).min(h);
    let mw = x1 - x0;
    let mut data = vec![blank; mw * (y1 - y0)];
    // One band of tile rows per task.
    data.par_chunks_mut(mw * ts)
        .enumerate()
        .for_each(|(band, rows)| {
            let ty = ty0 + band as i32;
            for tx in tx0..tx1 {
                let Some(tile) = canvas.get_layer_tile_data(idx, tx, ty) else {
                    continue;
                };
                let ox = tx as usize * ts - x0;
                let cols = ts.min(mw - ox);
                for (ly, row) in rows.chunks_mut(mw).enumerate() {
                    let src = &tile[ly * ts..ly * ts + cols];
                    for (dst, p) in row[ox..ox + cols].iter_mut().zip(src) {
                        *dst = if is_mask { p.r() } else { p.a() };
                    }
                }
            }
        });
    SelectionMask::new(x0 as i32, y0 as i32, mw, y1 - y0, data).cropped()
}

/// Draw the magnetic outline in progress.
pub(crate) fn draw_magnetic(
    app: &PainterApp,
    painter: &egui::Painter,
    to_screen: &dyn Fn(Vec2) -> egui::Pos2,
) {
    let Some(session) = &app.workspace.select.magnetic else {
        return;
    };
    let line = |points: &[Vec2], color: egui::Color32| {
        let pts: Vec<egui::Pos2> = points.iter().map(|&p| to_screen(p)).collect();
        painter.add(egui::Shape::line(
            pts.clone(),
            egui::Stroke::new(3.0_f32, egui::Color32::BLACK),
        ));
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0_f32, color)));
    };
    line(&session.points, egui::Color32::WHITE);
    line(&session.preview, crate::ui::style::ACCENT);
    for p in session.anchor_points() {
        let r = egui::Rect::from_center_size(to_screen(p), egui::vec2(6.0, 6.0));
        painter.rect_filled(r.expand(1.0), 0.0, egui::Color32::BLACK);
        painter.rect_filled(r, 0.0, egui::Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::project::tests::test_app_pub;
    use crate::selection::{SelectionMode, SelectionType};
    use eframe::egui::{Color32, Vec2};

    fn app() -> crate::PainterApp {
        let mut app = test_app_pub(Canvas::new(128, 128, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.selection_manager.canvas_size = [128, 128];
        app
    }

    fn drag(app: &mut crate::PainterApp, from: Vec2, to: Vec2) {
        app.select_press(from, SelectionType::Rectangle, SelectionMode::Replace);
        app.selection_manager.update_selection(to);
        app.select_release();
    }

    #[test]
    fn selection_changes_undo_and_redo() {
        let mut app = app();
        drag(&mut app, Vec2::new(10.0, 10.0), Vec2::new(50.0, 50.0));
        app.invert_selection();
        assert!(app.selection_manager.contains(Vec2::new(100.0, 100.0)));

        app.apply_history(false);
        assert!(!app.selection_manager.contains(Vec2::new(100.0, 100.0)));
        assert!(app.selection_manager.contains(Vec2::new(20.0, 20.0)));
        app.apply_history(false);
        assert!(!app.selection_manager.has_selection());

        app.apply_history(true);
        assert!(app.selection_manager.contains(Vec2::new(20.0, 20.0)));
    }

    #[test]
    fn a_cancelled_drag_restores_the_previous_selection() {
        let mut app = app();
        drag(&mut app, Vec2::new(10.0, 10.0), Vec2::new(50.0, 50.0));
        app.select_press(
            Vec2::new(60.0, 60.0),
            SelectionType::Rectangle,
            SelectionMode::Replace,
        );
        app.selection_manager
            .update_selection(Vec2::new(90.0, 90.0));
        app.select_cancel();
        assert!(app.selection_manager.contains(Vec2::new(20.0, 20.0)));
        assert!(!app.selection_manager.contains(Vec2::new(80.0, 80.0)));
    }

    #[test]
    fn deselecting_nothing_records_nothing() {
        let mut app = app();
        app.deselect();
        assert_eq!(app.layer_state.history.stacks().0.len(), 0);
    }

    /// Layer 1 of a 128×128 canvas with red squares at the given corners
    /// (each 20 px wide), on the white background.
    fn app_with_squares(corners: &[(usize, usize)]) -> crate::PainterApp {
        let mut app = app();
        let mut tiles = vec![vec![Color32::TRANSPARENT; 64 * 64]; 4];
        let red = Color32::from_rgb(220, 20, 20);
        for &(x0, y0) in corners {
            for y in y0..y0 + 20 {
                for x in x0..x0 + 20 {
                    tiles[(y / 64) * 2 + x / 64][(y % 64) * 64 + x % 64] = red;
                }
            }
        }
        for (i, tile) in tiles.into_iter().enumerate() {
            app.canvas_mut()
                .set_layer_tile_data(1, (i % 2) as i32, (i / 2) as i32, tile);
        }
        app
    }

    #[test]
    fn the_wand_selects_the_clicked_area_and_reruns_in_place() {
        let mut app = app_with_squares(&[(10, 10), (90, 90)]);
        app.select_press(
            Vec2::new(15.0, 15.0),
            SelectionType::Wand,
            SelectionMode::Replace,
        );
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(20.0, 20.0)));
        assert!(
            !sel.contains(Vec2::new(50.0, 50.0)),
            "not the white around it"
        );
        assert!(!sel.contains(Vec2::new(95.0, 95.0)), "not the other square");

        // Not contiguous: every red pixel. Still one undo step.
        app.workspace.select.wand.contiguous = false;
        app.rerun_last_pick();
        assert!(app.selection_manager.contains(Vec2::new(95.0, 95.0)));
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        app.apply_history(false);
        assert!(!app.selection_manager.has_selection());
    }

    #[test]
    fn colour_range_selects_the_colour_everywhere() {
        let mut app = app_with_squares(&[(10, 10), (90, 90)]);
        app.select_press(
            Vec2::new(15.0, 15.0),
            SelectionType::ColorRange,
            SelectionMode::Replace,
        );
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(95.0, 95.0)));
        assert!(!sel.contains(Vec2::new(50.0, 50.0)));
    }

    #[test]
    fn a_magnetic_outline_snaps_to_the_square() {
        let mut app = app_with_squares(&[(40, 40)]);
        let press = |app: &mut crate::PainterApp, p: Vec2| {
            app.select_press(p, SelectionType::Magnetic, SelectionMode::Replace);
            app.select_move(p);
            app.select_release();
        };
        // Clicks a few pixels off each corner of the square (40..60).
        press(&mut app, Vec2::new(37.5, 37.5));
        press(&mut app, Vec2::new(62.5, 37.5));
        press(&mut app, Vec2::new(62.5, 62.5));
        press(&mut app, Vec2::new(37.5, 62.5));
        app.magnetic_close();
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(50.0, 50.0)), "the square is inside");
        assert!(!sel.contains(Vec2::new(30.0, 50.0)), "the outside isn't");
        // Inside the clicked corners but outside the square: the outline
        // hugs the square's edge rather than joining the clicks.
        for p in [(38.0, 50.0), (50.0, 38.0), (62.0, 50.0), (50.0, 62.0)] {
            assert!(
                !sel.contains(Vec2::new(p.0, p.1)),
                "{p:?} should be outside"
            );
        }
        assert!(app.workspace.select.magnetic.is_none());
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
    }

    #[test]
    fn a_polygon_joins_the_clicks_with_straight_edges() {
        let mut app = app_with_squares(&[(40, 40)]);
        let press = |app: &mut crate::PainterApp, p: Vec2| {
            app.select_press(p, SelectionType::Polygon, SelectionMode::Replace);
            app.select_move(p);
            app.select_release();
        };
        // A triangle, the same clicks the magnetic lasso would snap.
        press(&mut app, Vec2::new(10.0, 10.0));
        press(&mut app, Vec2::new(110.0, 10.0));
        let session = app.workspace.select.magnetic.as_ref().unwrap();
        assert_eq!(session.preview.len(), 1, "no path search between clicks");
        app.select_move(Vec2::new(10.0, 110.0));
        let session = app.workspace.select.magnetic.as_ref().unwrap();
        assert_eq!(
            session.preview,
            vec![Vec2::new(110.0, 10.0), Vec2::new(10.0, 110.0)]
        );
        press(&mut app, Vec2::new(10.0, 110.0));
        // Clicking the first point closes it.
        press(&mut app, Vec2::new(10.5, 10.5));
        assert!(app.workspace.select.magnetic.is_none());
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(30.0, 30.0)));
        assert!(
            sel.contains(Vec2::new(50.0, 50.0)),
            "the square doesn't pull it in"
        );
        assert!(!sel.contains(Vec2::new(90.0, 90.0)), "past the long edge");
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);
        // Undo points work the same.
        press(&mut app, Vec2::new(5.0, 5.0));
        press(&mut app, Vec2::new(50.0, 5.0));
        app.magnetic_undo_anchor();
        let session = app.workspace.select.magnetic.as_ref().unwrap();
        assert_eq!(session.anchor_points().count(), 1);
        assert_eq!(session.preview.len(), 2);
    }

    #[test]
    fn removing_magnetic_points_and_cancelling() {
        let mut app = app_with_squares(&[(40, 40)]);
        for p in [Vec2::new(37.5, 37.5), Vec2::new(62.5, 37.5)] {
            app.select_press(p, SelectionType::Magnetic, SelectionMode::Replace);
            app.select_move(p);
            app.select_release();
        }
        let points = |app: &crate::PainterApp| {
            app.workspace
                .select
                .magnetic
                .as_ref()
                .map(|s| s.anchor_points().count())
        };
        assert_eq!(points(&app), Some(2));
        app.magnetic_undo_anchor();
        assert_eq!(points(&app), Some(1));
        app.select_cancel();
        assert_eq!(points(&app), None);
        assert!(!app.selection_manager.has_selection());
    }

    #[test]
    fn modifying_the_selection_is_one_undo_step_each() {
        use crate::app::tools::select::SelectionModify;
        let mut app = app();
        drag(&mut app, Vec2::new(40.0, 40.0), Vec2::new(80.0, 80.0));
        let depth = app.layer_state.history.stacks().0.len();
        app.modify_selection(SelectionModify::Grow, 5);
        assert!(app.selection_manager.contains(Vec2::new(36.5, 60.5)));
        app.modify_selection(SelectionModify::Shrink, 10);
        assert!(!app.selection_manager.contains(Vec2::new(42.5, 60.5)));
        assert!(app.selection_manager.contains(Vec2::new(46.5, 60.5)));
        app.modify_selection(SelectionModify::Border, 4);
        assert!(
            !app.selection_manager.contains(Vec2::new(60.5, 60.5)),
            "hollow"
        );
        app.modify_selection(SelectionModify::Feather, 3);
        assert_eq!(app.layer_state.history.stacks().0.len(), depth + 4);
        assert_eq!(app.workspace.select.modify.get(SelectionModify::Shrink), 10);
        // Back one by one to the rectangle.
        for _ in 0..4 {
            app.apply_history(false);
        }
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(41.5, 60.5)) && sel.contains(Vec2::new(60.5, 60.5)));
        assert!(!sel.contains(Vec2::new(38.5, 60.5)));
        // Nothing selected: nothing to do, nothing recorded.
        app.deselect();
        let depth = app.layer_state.history.stacks().0.len();
        app.modify_selection(SelectionModify::Grow, 5);
        assert!(!app.selection_manager.has_selection());
        assert_eq!(app.layer_state.history.stacks().0.len(), depth);
    }

    #[test]
    fn a_rectangle_selection_can_be_grown_past_the_canvas_edge() {
        use crate::app::tools::select::SelectionModify;
        let mut app = app();
        app.select_all();
        app.modify_selection(SelectionModify::Shrink, 8);
        assert!(!app.selection_manager.contains(Vec2::new(4.5, 60.5)));
        app.modify_selection(SelectionModify::Grow, 20);
        let b = app.selection_manager.get_bounds().unwrap();
        assert_eq!((b.min.x, b.max.x), (0.0, 128.0), "kept on the canvas");
    }

    #[test]
    fn a_layers_paint_becomes_the_selection() {
        let mut app = app_with_squares(&[(10, 10), (90, 90)]);
        // Half-transparent paint selects partly.
        app.canvas_mut().set_layer_tile_data(
            1,
            1,
            0,
            vec![Color32::from_rgba_premultiplied(40, 0, 0, 100); 64 * 64],
        );
        app.select_layer_paint(1, SelectionMode::Replace);
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(15.5, 15.5)));
        assert!(sel.contains(Vec2::new(95.5, 95.5)));
        assert!(!sel.contains(Vec2::new(50.5, 50.5)), "transparent");
        let mut cov = [0.0];
        sel.row_coverage(20, 100, &mut cov);
        assert!((cov[0] - 100.0 / 255.0).abs() < 0.01, "alpha as coverage");
        assert_eq!(app.layer_state.history.stacks().0.len(), 1);

        // Subtract, then add back, then intersect.
        app.select_all();
        app.select_layer_paint(1, SelectionMode::Subtract);
        assert!(!app.selection_manager.contains(Vec2::new(15.5, 15.5)));
        assert!(app.selection_manager.contains(Vec2::new(50.5, 50.5)));
        app.select_layer_paint(1, SelectionMode::Add);
        assert!(app.selection_manager.contains(Vec2::new(15.5, 15.5)));
        drag(&mut app, Vec2::new(0.0, 0.0), Vec2::new(40.0, 40.0));
        app.select_layer_paint(1, SelectionMode::Intersect);
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(15.5, 15.5)));
        assert!(!sel.contains(Vec2::new(35.5, 35.5)) && !sel.contains(Vec2::new(95.5, 95.5)));
        // The background is paint everywhere.
        app.select_layer_paint(0, SelectionMode::Replace);
        assert!(app.selection_manager.contains(Vec2::new(127.5, 127.5)));
        // An empty layer selects nothing.
        app.add_layer_and_select();
        let empty = app.canvas.active_layer_idx;
        app.select_layer_paint(empty, SelectionMode::Replace);
        assert!(!app.selection_manager.has_selection());
        app.apply_history(false);
        assert!(app.selection_manager.contains(Vec2::new(127.5, 127.5)));
    }

    #[test]
    fn saved_selections_load_with_each_mode_and_round_trip_through_the_file() {
        let mut app = app();
        drag(&mut app, Vec2::new(10.0, 10.0), Vec2::new(40.0, 40.0));
        let name = app.next_saved_selection_name();
        assert_eq!(name, "Selection 1");
        app.save_selection(name);
        drag(&mut app, Vec2::new(30.0, 30.0), Vec2::new(60.0, 60.0));
        app.save_selection("Second".into());
        assert_eq!(app.next_saved_selection_name(), "Selection 2");

        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let names: Vec<&str> = loaded
            .saved_selections
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["Selection 1", "Second"]);
        for (a, b) in loaded
            .saved_selections
            .iter()
            .zip(&app.workspace.select.saved)
        {
            assert_eq!(
                (a.mask.x0, a.mask.y0, a.mask.w, a.mask.h),
                (b.mask.x0, b.mask.y0, b.mask.w, b.mask.h)
            );
            assert_eq!(a.mask.data, b.mask.data);
        }

        app.deselect();
        let depth = app.layer_state.history.stacks().0.len();
        app.load_selection(0, SelectionMode::Replace);
        assert!(app.selection_manager.contains(Vec2::new(15.5, 15.5)));
        app.load_selection(1, SelectionMode::Add);
        assert!(app.selection_manager.contains(Vec2::new(55.5, 55.5)));
        app.load_selection(0, SelectionMode::Subtract);
        assert!(!app.selection_manager.contains(Vec2::new(15.5, 15.5)));
        assert!(app.selection_manager.contains(Vec2::new(55.5, 55.5)));
        app.load_selection(0, SelectionMode::Replace);
        app.load_selection(1, SelectionMode::Intersect);
        let sel = &app.selection_manager;
        assert!(sel.contains(Vec2::new(35.5, 35.5)));
        assert!(!sel.contains(Vec2::new(15.5, 15.5)) && !sel.contains(Vec2::new(55.5, 55.5)));
        assert_eq!(app.layer_state.history.stacks().0.len(), depth + 5);
        app.apply_history(false);
        assert!(app.selection_manager.contains(Vec2::new(15.5, 15.5)));

        // Saving under a used name replaces; deleting removes.
        app.save_selection("Second".into());
        assert_eq!(app.workspace.select.saved.len(), 2);
        app.delete_saved_selection(0);
        assert_eq!(app.workspace.select.saved[0].name, "Second");

        // Opening the file brings them back; a new canvas has none.
        let path =
            std::env::temp_dir().join(format!("rp-saved-sel-{}.rpainter", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        app.load_project_from_path(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(app.workspace.select.saved.len(), 2);
        app.modal_state.new_canvas.width = 64.0;
        app.modal_state.new_canvas.height = 64.0;
        app.modal_state.new_canvas.unit = crate::app::document::CanvasUnit::Pixels;
        app.apply_new_canvas();
        assert!(app.workspace.select.saved.is_empty());
    }
}
