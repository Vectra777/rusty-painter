//! Vector layers at the app level: the brush draws lines on them, the
//! eraser takes lines out (whole, or the part it touches), and Layer →
//! Vector thickens, thins or recolours every line. Each is one undo step
//! holding the lines as they were and the pixels they drew.
//!
//! A vector layer's pixels are always its lines rendered, so any other
//! pixel tool (a fill, a filter, smudge) turns it into a plain layer first,
//! as with text layers.

use crate::app::PainterApp;
use crate::app::stroke_ops::exclusive;
use crate::canvas::history::{LayerHistoryOp, TileSnapshot, UndoAction};
use crate::canvas::storage::LayerId;
use crate::canvas::vector::{self, VectorLayer, VectorStroke};
use eframe::egui::{Color32, Vec2};

/// How the eraser takes lines out of a vector layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VectorErase {
    /// Every line it touches, whole (Clip Studio's "whole line").
    #[default]
    WholeLine,
    /// Only the part of the line under it.
    Touched,
    /// The stretch of the line it touches, up to where other lines (or the
    /// line itself) cross it (Clip Studio's "up to intersection").
    ToCrossing,
}

#[derive(Default)]
pub struct VectorState {
    /// The line being drawn or the erasing going on.
    session: Option<VectorSession>,
    /// Vector layers a pixel stroke has begun on, turned into plain pixel
    /// layers, their lines waiting for that stroke's undo step.
    pub stroke_rasterised: Vec<(LayerId, Box<VectorLayer>)>,
    pub erase: VectorErase,
    /// The Line Width dialog: the layer, and its lines when it opened (the
    /// width applies to those, so dragging back and forth is exact).
    pub width_editing: Option<(LayerId, Box<VectorLayer>, f32)>,
    /// Edit Lines: the line picked, and a drag of it going on.
    pub line_edit: Option<LineEdit>,
}

/// The line Edit Lines has picked.
pub struct LineEdit {
    layer: LayerId,
    /// Which line (index in the layer's).
    line: usize,
    /// The point of the handle pressed last (Delete takes it out).
    point: Option<usize>,
    drag: Option<LineDrag>,
}

struct LineDrag {
    start: Vec2,
    /// The line as the drag found it, and its handles then.
    original: VectorStroke,
    handles: Vec<usize>,
    mode: DragMode,
}

#[derive(Clone, Copy)]
enum DragMode {
    /// Move handle `n` (an index into the handles), the line bending.
    Bend(usize),
    /// Widen or thin the line around handle `n`.
    Widen(usize),
    /// Move the whole line.
    Move,
}

/// How far (screen points) a press may be from a handle or line to take it.
const HANDLE_HIT: f32 = 12.0;
/// Handle drawn radius (screen points).
const HANDLE_RADIUS: f32 = 4.5;

struct VectorSession {
    layer: LayerId,
    /// The lines before this step, for undo.
    before: Box<VectorLayer>,
    /// Where pixels changed (canvas pixels), for undo.
    changed: Option<[i32; 4]>,
    /// Drawing: the brush's colour and opacity; `None` when erasing.
    drawing: Option<([u8; 3], f32)>,
    last: Vec2,
}

impl PainterApp {
    /// Whether layer `idx` is a vector layer.
    pub(crate) fn is_vector_layer(&self, idx: usize) -> bool {
        self.canvas
            .layers
            .get(idx)
            .is_some_and(|l| l.vector.is_some())
    }

    /// Layer → New Vector Layer: an empty vector layer above the selected
    /// one, selected.
    pub(crate) fn add_vector_layer(&mut self) {
        self.quick_mask_leave();
        let (index, parent) = self.insertion_point(true);
        let name = self.next_layer_name("Vector");
        self.insert_entry_with(
            index,
            name,
            crate::canvas::storage::LayerKind::Paint,
            parent,
            true,
            |l| l.vector = Some(Box::default()),
        );
    }

    /// The width a line has at `pressure`, as the brush's size would be.
    fn vector_width(&self, pressure: f32) -> f32 {
        let o = &self.brush_state.brush.brush_options;
        let k = if o.pressure_size {
            o.pressure_min_size + (1.0 - o.pressure_min_size) * o.pressure_curves.size(pressure)
        } else {
            1.0
        };
        (o.diameter * k).clamp(0.0, vector::MAX_WIDTH)
    }

    /// Add a line through `points` (a shape's outline) to vector layer
    /// `idx`, with the brush's width, colour and opacity, as one step.
    pub(crate) fn add_vector_line(&mut self, idx: usize, points: &[Vec2]) {
        if !self.is_vector_layer(idx) || points.len() < 2 || self.brush_state.eraser_active {
            return;
        }
        self.release_canvas();
        let width = self.vector_width(1.0);
        let o = &self.brush_state.brush.brush_options;
        let [r, g, b, _] = o.color.to_srgba_unmultiplied();
        let raw: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, width]).collect();
        let stroke = VectorStroke {
            // Fewer points; drawn smoothed through them, it keeps its shape.
            points: vector::simplify(&raw, 0.3),
            colour: [r, g, b],
            opacity: o.opacity.clamp(0.0, 1.0),
        };
        self.brush_state.remember_color(Color32::from_rgb(r, g, b));
        let layer = &self.canvas.layers[idx];
        let bounds = stroke.bounds();
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: Vec2::ZERO,
        });
        // What's there now is the undo step's picture: read it rather than
        // drawing the old lines again.
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let region = [
            bounds[0].max(0),
            bounds[1].max(0),
            bounds[2].min(w),
            bounds[3].min(h),
        ];
        let before = (region[0] < region[2] && region[1] < region[3])
            .then(|| read_region(&self.canvas, idx, region));
        self.edit_vector(idx, |v| v.strokes.push(stroke));
        self.redraw_vector(idx, bounds);
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step_from(session, "Vector shape", before);
        }
    }

    /// A brush stroke starts on the active layer: on a vector layer it
    /// draws (or, with the eraser, erases) lines instead. Returns whether
    /// it did.
    pub(crate) fn vector_stroke_begin(&mut self, pos: Vec2, pressure: f32) -> bool {
        let idx = self.canvas.active_layer_idx;
        if !self.is_vector_layer(idx) {
            return false;
        }
        self.release_canvas();
        self.mark_action();
        let layer = &self.canvas.layers[idx];
        let before = layer.vector.clone().unwrap_or_default();
        let erasing = self.brush_state.eraser_active;
        let drawing = (!erasing).then(|| {
            let o = &self.brush_state.brush.brush_options;
            let [r, g, b, _] = o.color.to_srgba_unmultiplied();
            ([r, g, b], o.opacity.clamp(0.0, 1.0))
        });
        if let Some(([r, g, b], _)) = drawing {
            self.brush_state.remember_color(Color32::from_rgb(r, g, b));
        }
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before,
            changed: None,
            drawing,
            last: pos,
        });
        // A stroke is on, as far as the rest of the app goes (moves extend
        // it, nothing autosaves meanwhile).
        self.brush_state.is_drawing = true;
        match drawing {
            Some((colour, opacity)) => {
                let width = self.vector_width(pressure);
                let stroke = VectorStroke {
                    points: vec![[pos.x, pos.y, width]],
                    colour,
                    opacity,
                };
                let bounds = stroke.bounds();
                self.edit_vector(idx, |v| v.strokes.push(stroke));
                self.redraw_vector(idx, bounds);
            }
            None => self.vector_erase_at(idx, pos, pos),
        }
        true
    }

    /// The pen moved on during a vector stroke. Returns whether one is on.
    pub(crate) fn vector_stroke_add(&mut self, pos: Vec2, pressure: f32) -> bool {
        let Some(session) = &self.workspace.vector.session else {
            return false;
        };
        let Some(idx) = self.canvas.layer_index_of(session.layer) else {
            return true;
        };
        let (last, drawing) = (session.last, session.drawing.is_some());
        if (pos - last).length() < 0.25 {
            return true;
        }
        if let Some(s) = self.workspace.vector.session.as_mut() {
            s.last = pos;
        }
        if drawing {
            let width = self.vector_width(pressure);
            let segment = VectorStroke {
                points: vec![[last.x, last.y, width], [pos.x, pos.y, width]],
                colour: [0; 3],
                opacity: 1.0,
            };
            self.edit_vector(idx, |v| {
                if let Some(s) = v.strokes.last_mut() {
                    s.points.push([pos.x, pos.y, width]);
                }
            });
            // The new stretch, and the last few points before it: the
            // smoothed curve through them bends to meet it.
            let bounds =
                vector::union([segment.bounds(), span_bounds(&self.canvas.layers[idx], 4)])
                    .unwrap_or_else(|| segment.bounds());
            self.redraw_vector(idx, bounds);
        } else {
            self.vector_erase_at(idx, last, pos);
        }
        true
    }

    /// The pen lifted: the line is kept (its points thinned out) or the
    /// erasing ends, as one undo step.
    pub(crate) fn vector_stroke_end(&mut self) {
        let Some(session) = self.workspace.vector.session.take() else {
            return;
        };
        self.brush_state.is_drawing = false;
        let Some(idx) = self.canvas.layer_index_of(session.layer) else {
            return;
        };
        if session.drawing.is_some() {
            // Fewer points, the same line: quicker to draw and edit.
            let old = span_bounds(&self.canvas.layers[idx], usize::MAX);
            self.edit_vector(idx, |v| {
                if let Some(s) = v.strokes.last_mut() {
                    s.points = vector::simplify(&s.points, 0.3);
                }
            });
            let new = span_bounds(&self.canvas.layers[idx], usize::MAX);
            let region = vector::union([old, new]).unwrap_or(old);
            // The redraw notes the region in the step, so it's filed after.
            self.workspace.vector.session = Some(session);
            self.redraw_vector(idx, region);
            if let Some(session) = self.workspace.vector.session.take() {
                self.push_vector_step(session, "Vector line");
            }
        } else if session.changed.is_some() {
            self.push_vector_step(session, "Vector erase");
        }
    }

    /// Erase along `from`→`to` with the eraser's size.
    fn vector_erase_at(&mut self, idx: usize, from: Vec2, to: Vec2) {
        let radius = (self.brush_state.brush.brush_options.diameter * 0.5).max(0.5);
        let mode = self.workspace.vector.erase;
        let steps = ((to - from).length() / (radius * 0.5)).ceil().max(1.0) as usize;
        let spots: Vec<Vec2> = (0..=steps)
            .map(|i| from + (to - from) * (i as f32 / steps as f32))
            .collect();
        let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
            return;
        };
        let mut changed = Vec::new();
        let mut kept = Vec::with_capacity(v.strokes.len());
        for (n, s) in v.strokes.iter().enumerate() {
            let b = s.bounds();
            let near = spots.iter().any(|c| {
                c.x + radius >= b[0] as f32
                    && c.x - radius <= b[2] as f32
                    && c.y + radius >= b[1] as f32
                    && c.y - radius <= b[3] as f32
            });
            if !near || !spots.iter().any(|&c| s.touches(c, radius)) {
                kept.push(s.clone());
                continue;
            }
            changed.push(b);
            let mut pieces = vec![s.clone()];
            match mode {
                VectorErase::WholeLine => continue,
                VectorErase::Touched => {
                    for &c in &spots {
                        pieces = pieces.iter().flat_map(|p| p.cut(c, radius)).collect();
                    }
                }
                VectorErase::ToCrossing => {
                    let others: Vec<&VectorStroke> = (v.strokes.iter().enumerate())
                        .filter(|&(m, _)| m != n)
                        .map(|(_, o)| o)
                        .collect();
                    for &c in &spots {
                        pieces = (pieces.iter())
                            .flat_map(|p| match p.touches(c, radius) {
                                true => p.cut_to_crossings(c, &others),
                                false => vec![p.clone()],
                            })
                            .collect();
                    }
                }
            }
            kept.extend(pieces);
        }
        let Some(region) = vector::union(changed) else {
            return;
        };
        self.edit_vector(idx, |v| v.strokes = kept);
        self.redraw_vector(idx, region);
    }

    /// Change layer `idx`'s lines (no redraw).
    fn edit_vector(&mut self, idx: usize, edit: impl FnOnce(&mut VectorLayer)) {
        let canvas = exclusive(&mut self.canvas);
        if let Some(v) = canvas.layers[idx].vector.as_mut() {
            edit(v);
        }
    }

    /// Draw layer `idx`'s lines again over `region` (canvas pixels), and
    /// note it as changed in the step going on.
    fn redraw_vector(&mut self, idx: usize, region: [i32; 4]) {
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let region = [
            region[0].max(0),
            region[1].max(0),
            region[2].min(w),
            region[3].min(h),
        ];
        if region[0] >= region[2] || region[1] >= region[3] {
            return;
        }
        if let Some(s) = self.workspace.vector.session.as_mut() {
            s.changed = vector::union(s.changed.into_iter().chain([region]));
        }
        let Some(v) = self.canvas.layers[idx].vector.as_ref() else {
            return;
        };
        let pixels = vector::render_region(&v.strokes, region);
        write_region(&self.canvas, idx, region, &pixels);
        self.mark_rect_damage(region);
        self.layer_state.thumbnails_dirty = true;
    }

    /// File a vector step: the lines as they were, and the pixels over what
    /// changed as they were drawn from them.
    fn push_vector_step(&mut self, session: VectorSession, label: &str) {
        self.push_vector_step_from(session, label, None);
    }

    /// [`Self::push_vector_step`], given the pixels over what changed as
    /// they were (when they were read before the change).
    fn push_vector_step_from(
        &mut self,
        session: VectorSession,
        label: &str,
        before: Option<Vec<Color32>>,
    ) {
        let Some(region) = session.changed else {
            return;
        };
        let before =
            before.unwrap_or_else(|| vector::render_region(&session.before.strokes, region));
        let tiles = region_snapshots(&self.canvas, session.layer, region, &before);
        self.layer_state.history.label_next(label);
        self.layer_state.history.push_action(UndoAction {
            tiles,
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Vector {
                layers: vec![(session.layer, Some(session.before))],
                inner: None,
            }),
        });
    }

    /// Change every line of vector layer `idx` at once (their width or
    /// colour) as one undo step named `label`.
    pub(crate) fn change_vector_lines(
        &mut self,
        idx: usize,
        label: &str,
        change: impl FnOnce(&mut VectorLayer),
    ) {
        if !self.is_vector_layer(idx) {
            return;
        }
        self.release_canvas();
        let layer = &self.canvas.layers[idx];
        let before = layer.vector.clone().unwrap_or_default();
        let old = vector::union(before.strokes.iter().map(|s| s.bounds()));
        self.edit_vector(idx, change);
        let after = self.canvas.layers[idx].vector.as_ref();
        let new = after.and_then(|v| vector::union(v.strokes.iter().map(|s| s.bounds())));
        let Some(region) = vector::union(old.into_iter().chain(new)) else {
            return;
        };
        let id = self.canvas.layers[idx].id;
        self.workspace.vector.session = Some(VectorSession {
            layer: id,
            before,
            changed: None,
            drawing: None,
            last: Vec2::ZERO,
        });
        self.redraw_vector(idx, region);
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, label);
        }
    }

    /// Layer → Vector → Line Width: open the dialog for layer `idx`.
    pub(crate) fn line_width_open(&mut self, idx: usize) {
        self.release_canvas();
        let Some(layer) = self.canvas.layers.get(idx) else {
            return;
        };
        if let Some(v) = layer.vector.clone() {
            self.workspace.vector.width_editing = Some((layer.id, v, 1.0));
        }
    }

    /// Every line of the layer in the Line Width dialog `scale` times as
    /// wide as when it opened (shown at once, filed when it's done).
    pub(crate) fn line_width_set(&mut self, scale: f32) {
        let Some((id, original, current)) = self.workspace.vector.width_editing.as_mut() else {
            return;
        };
        let (id, original, previous) = (*id, original.clone(), *current);
        *current = scale;
        let Some(idx) = self.canvas.layer_index_of(id) else {
            self.workspace.vector.width_editing = None;
            return;
        };
        self.release_canvas();
        let region = scaled_bounds(&original, previous.max(scale).max(1.0));
        let mut scaled = *original;
        scale_widths(&mut scaled, scale);
        self.edit_vector(idx, |v| *v = scaled);
        if let Some(region) = region {
            self.redraw_vector(idx, region);
        }
    }

    /// Close the Line Width dialog: keep the new width as one undo step, or
    /// put the lines back.
    pub(crate) fn line_width_done(&mut self, keep: bool) {
        let Some((id, original, scale)) = self.workspace.vector.width_editing.take() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(id) else {
            return;
        };
        if !keep || scale == 1.0 {
            let region = scaled_bounds(&original, scale.max(1.0));
            self.edit_vector(idx, |v| *v = *original);
            if let Some(region) = region {
                self.redraw_vector(idx, region);
            }
            return;
        }
        let changed = scaled_bounds(&original, scale.max(1.0)).map(|r| {
            let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
            [r[0].max(0), r[1].max(0), r[2].min(w), r[3].min(h)]
        });
        self.push_vector_step(
            VectorSession {
                layer: id,
                before: original,
                changed,
                drawing: None,
                last: Vec2::ZERO,
            },
            "Line width",
        );
    }

    /// Recolour every line of layer `idx` with the brush colour.
    pub(crate) fn recolour_vector_lines(&mut self, idx: usize) {
        let [r, g, b, _] = self
            .brush_state
            .brush
            .brush_options
            .color
            .to_srgba_unmultiplied();
        self.change_vector_lines(idx, "Recolour lines", |v| {
            for s in &mut v.strokes {
                s.colour = [r, g, b];
            }
        });
    }

    /// Layer → Vector → Rasterise: a plain pixel layer (one undo step).
    pub(crate) fn rasterise_vector_layer(&mut self, idx: usize) {
        if !self.is_vector_layer(idx) {
            return;
        }
        let canvas = self.canvas_mut();
        let id = canvas.layers[idx].id;
        let old = canvas.layers[idx].vector.take();
        self.layer_state
            .history
            .label_next("Rasterise vector layer");
        self.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Vector {
                layers: vec![(id, old)],
                inner: None,
            }),
        });
        self.layer_state.thumbnails_dirty = true;
    }

    /// Vector layers `action` paints on with a pixel tool become plain
    /// pixel layers, their lines kept in `action` for undo.
    pub(crate) fn rasterise_painted_vector(&mut self, action: &mut UndoAction) {
        if matches!(action.layer_action, Some(LayerHistoryOp::Document(_))) {
            return;
        }
        let mut covered = Vec::new();
        let mut op = action.layer_action.as_ref();
        while let Some(o) = op {
            match o {
                LayerHistoryOp::Vector { layers, inner } => {
                    covered.extend(layers.iter().map(|(id, _)| *id));
                    op = inner.as_deref();
                }
                LayerHistoryOp::Text { inner, .. } => op = inner.as_deref(),
                LayerHistoryOp::Added { id, .. } => {
                    covered.push(*id);
                    op = None;
                }
                _ => op = None,
            }
        }
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
                .is_some_and(|i| self.is_vector_layer(i))
        });
        if ids.is_empty() {
            return;
        }
        let canvas = self.canvas_mut();
        let taken: Vec<(LayerId, Option<Box<VectorLayer>>)> = ids
            .into_iter()
            .filter_map(|id| {
                let i = canvas.layer_index_of(id)?;
                Some((id, canvas.layers[i].vector.take()))
            })
            .collect();
        action.layer_action = Some(LayerHistoryOp::Vector {
            layers: taken,
            inner: action.layer_action.take().map(Box::new),
        });
        self.layer_state.thumbnails_dirty = true;
    }

    /// A pixel stroke (smudge, blur...) starts on the active layer: a vector
    /// layer becomes plain pixels, its lines going into the stroke's step.
    pub(crate) fn rasterise_vector_for_stroke(&mut self) {
        let idx = self.canvas.active_layer_idx;
        if !self.is_vector_layer(idx) {
            return;
        }
        let canvas = self.canvas_mut();
        let id = canvas.layers[idx].id;
        if let Some(v) = canvas.layers[idx].vector.take() {
            self.workspace.vector.stroke_rasterised.push((id, v));
            self.layer_state.thumbnails_dirty = true;
        }
    }

    /// File the lines of a layer the finished stroke `undo` began on into it.
    pub(crate) fn attach_vector_rasterised(&mut self, undo: &mut UndoAction) {
        let pending = &mut self.workspace.vector.stroke_rasterised;
        let Some(i) = pending.iter().position(|(id, _)| {
            undo.tiles.is_empty() || undo.tiles.iter().any(|t| t.layer_id == *id)
        }) else {
            return;
        };
        let (id, v) = pending.remove(i);
        let inner = undo.layer_action.take().map(Box::new);
        undo.layer_action = Some(LayerHistoryOp::Vector {
            layers: vec![(id, Some(v))],
            inner,
        });
    }

    /// Vector layer `id` was moved by `info` as a whole: when that's only a
    /// move, its lines move with it, and this is the undo record. Anything
    /// else (turned, scaled, bent) leaves it to become pixels.
    pub(crate) fn vector_layer_moved(
        &mut self,
        id: LayerId,
        info: &crate::selection::transform::TransformInfo,
    ) -> Option<LayerHistoryOp> {
        let only_moved = info.warp.is_none()
            && info.corners.is_none()
            && info.rotation == 0.0
            && info.scale == Vec2::new(1.0, 1.0);
        let idx = self.canvas.layer_index_of(id)?;
        if !only_moved || !self.is_vector_layer(idx) {
            return None;
        }
        let offset = info.offset;
        let layer = &mut self.canvas_mut().layers[idx];
        let old = layer.vector.clone()?;
        if let Some(v) = layer.vector.as_mut() {
            for s in &mut v.strokes {
                for p in &mut s.points {
                    p[0] += offset.x;
                    p[1] += offset.y;
                }
            }
        }
        Some(LayerHistoryOp::Vector {
            layers: vec![(id, Some(old))],
            inner: None,
        })
    }
}

/// Every line's width times `scale`.
fn scale_widths(v: &mut VectorLayer, scale: f32) {
    for s in &mut v.strokes {
        for p in &mut s.points {
            p[2] = (p[2] * scale).clamp(0.0, vector::MAX_WIDTH);
        }
    }
}

/// Where `v`'s lines reach with their widths times `scale`.
fn scaled_bounds(v: &VectorLayer, scale: f32) -> Option<[i32; 4]> {
    let mut scaled = v.clone();
    scale_widths(&mut scaled, scale);
    vector::union(scaled.strokes.iter().map(|s| s.bounds()))
}

/// The bounds of the last `n` points of layer's newest line (all of it for
/// `usize::MAX`).
fn span_bounds(layer: &crate::canvas::storage::Layer, n: usize) -> [i32; 4] {
    layer
        .vector
        .as_ref()
        .and_then(|v| v.strokes.last())
        .map_or([0; 4], |s| {
            let start = s.points.len().saturating_sub(n);
            VectorStroke {
                points: s.points[start..].to_vec(),
                colour: s.colour,
                opacity: s.opacity,
            }
            .bounds()
        })
}

/// Layer `idx`'s pixels over `region`, row-major.
fn read_region(canvas: &crate::canvas::Canvas, idx: usize, region: [i32; 4]) -> Vec<Color32> {
    let ts = canvas.tile_size() as i32;
    let [x0, y0, x1, y1] = region;
    let w = (x1 - x0) as usize;
    let mut out = vec![Color32::TRANSPARENT; w * (y1 - y0) as usize];
    for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let Some(data) = canvas.get_layer_tile_data(idx, tx, ty) else {
                continue;
            };
            let (ox, oy) = (tx * ts, ty * ts);
            let (sx0, sx1) = (x0.max(ox), x1.min(ox + ts));
            for y in y0.max(oy)..y1.min(oy + ts) {
                let src = ((y - oy) * ts + sx0 - ox) as usize;
                let dst = (y - y0) as usize * w + (sx0 - x0) as usize;
                out[dst..dst + (sx1 - sx0) as usize]
                    .copy_from_slice(&data[src..src + (sx1 - sx0) as usize]);
            }
        }
    }
    out
}

/// Put `pixels` (`region`, row-major) into layer `idx`'s tiles.
fn write_region(canvas: &crate::canvas::Canvas, idx: usize, region: [i32; 4], pixels: &[Color32]) {
    let ts = canvas.tile_size() as i32;
    let [x0, y0, x1, y1] = region;
    let w = (x1 - x0) as usize;
    for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let mut data = canvas
                .get_layer_tile_data(idx, tx, ty)
                .unwrap_or_else(|| vec![Color32::TRANSPARENT; (ts * ts) as usize]);
            let (ox, oy) = (tx * ts, ty * ts);
            for y in y0.max(oy)..y1.min(oy + ts) {
                let src = (y - y0) as usize * w;
                let dst = ((y - oy) * ts) as usize;
                for x in x0.max(ox)..x1.min(ox + ts) {
                    data[dst + (x - ox) as usize] = pixels[src + (x - x0) as usize];
                }
            }
            canvas.set_layer_tile_data(idx, tx, ty, data);
        }
    }
}

/// Undo snapshots of `region` of layer `id`, holding `pixels` (the region,
/// row-major) rather than what the tiles hold now.
fn region_snapshots(
    canvas: &crate::canvas::Canvas,
    id: LayerId,
    region: [i32; 4],
    pixels: &[Color32],
) -> Vec<TileSnapshot> {
    let ts = canvas.tile_size() as i32;
    let [x0, y0, x1, y1] = region;
    let w = (x1 - x0) as usize;
    let mut out = Vec::new();
    for ty in y0.div_euclid(ts)..=(y1 - 1).div_euclid(ts) {
        for tx in x0.div_euclid(ts)..=(x1 - 1).div_euclid(ts) {
            let (ox, oy) = (tx * ts, ty * ts);
            let (sx0, sy0, sx1, sy1) = (x0.max(ox), y0.max(oy), x1.min(ox + ts), y1.min(oy + ts));
            let mut data = Vec::with_capacity(((sx1 - sx0) * (sy1 - sy0)) as usize);
            for y in sy0..sy1 {
                let row = (y - y0) as usize * w;
                data.extend_from_slice(
                    &pixels[row + (sx0 - x0) as usize..row + (sx1 - x0) as usize],
                );
            }
            out.push(TileSnapshot {
                tx,
                ty,
                layer_id: id,
                x0: (sx0 - ox) as usize,
                y0: (sy0 - oy) as usize,
                width: (sx1 - sx0) as usize,
                height: (sy1 - sy0) as usize,
                data: data.into(),
            });
        }
    }
    out
}

impl PainterApp {
    /// The line Edit Lines has picked, if it's still there: its layer index
    /// and the line.
    fn picked_line(&self) -> Option<(usize, &VectorStroke)> {
        let edit = self.workspace.vector.line_edit.as_ref()?;
        let idx = self.canvas.layer_index_of(edit.layer)?;
        let line = self.canvas.layers[idx]
            .vector
            .as_ref()?
            .strokes
            .get(edit.line)?;
        Some((idx, line))
    }

    /// Where a line's handles are at this zoom: its points that keep its
    /// shape to within a pixel and a half on screen.
    fn line_handles(&self, line: &VectorStroke) -> Vec<usize> {
        vector::simplify_indices(&line.points, 1.5 / self.viewport.zoom.max(0.01))
    }

    /// Edit Lines pressed at `pos`: a handle of the picked line starts
    /// bending it (Shift: widening it), Alt on the line moves it all, and
    /// anywhere else picks the line there (or none).
    pub(crate) fn line_edit_press(&mut self, pos: Vec2, shift: bool, alt: bool) {
        let idx = self.canvas.active_layer_idx;
        if !self.is_vector_layer(idx) {
            self.workspace.vector.line_edit = None;
            return;
        }
        let hit = HANDLE_HIT / self.viewport.zoom.max(0.01);
        let picked = self
            .picked_line()
            .filter(|(i, _)| *i == idx)
            .map(|(_, l)| l.clone());
        if let Some(line) = picked {
            let handles = self.line_handles(&line);
            let near = (handles.iter().enumerate())
                .map(|(n, &k)| {
                    (
                        n,
                        (Vec2::new(line.points[k][0], line.points[k][1]) - pos).length(),
                    )
                })
                .filter(|&(_, d)| d <= hit)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            let mode = match near {
                Some((n, _)) if shift => Some(DragMode::Widen(n)),
                Some((n, _)) => Some(DragMode::Bend(n)),
                None if alt && line.touches(pos, hit) => Some(DragMode::Move),
                None => None,
            };
            if let Some(mode) = mode {
                // (Before the session: finishing a stroke would take it.)
                self.release_canvas();
                self.mark_action();
                let layer = &self.canvas.layers[idx];
                self.workspace.vector.session = Some(VectorSession {
                    layer: layer.id,
                    before: layer.vector.clone().unwrap_or_default(),
                    changed: None,
                    drawing: None,
                    last: pos,
                });
                if let Some(edit) = self.workspace.vector.line_edit.as_mut() {
                    if let DragMode::Bend(n) | DragMode::Widen(n) = mode {
                        edit.point = Some(handles[n]);
                    }
                    edit.drag = Some(LineDrag {
                        start: pos,
                        original: line,
                        handles,
                        mode,
                    });
                }
                return;
            }
        }
        let layer = &self.canvas.layers[idx];
        let strokes = layer.vector.as_ref().map_or(&[][..], |v| &v.strokes[..]);
        self.workspace.vector.line_edit = vector::pick(strokes, pos, hit).map(|line| LineEdit {
            layer: layer.id,
            line,
            point: None,
            drag: None,
        });
    }

    /// The Edit Lines drag went on to `pos`.
    pub(crate) fn line_edit_drag(&mut self, pos: Vec2) {
        let Some(edit) = self.workspace.vector.line_edit.as_ref() else {
            return;
        };
        let Some(drag) = edit.drag.as_ref() else {
            return;
        };
        let Some(idx) = self.canvas.layer_index_of(edit.layer) else {
            return;
        };
        let delta = pos - drag.start;
        let original = &drag.original;
        let points = match drag.mode {
            DragMode::Bend(n) => vector::bend(&original.points, &drag.handles, n, delta, 0.0),
            // Up widens, down thins.
            DragMode::Widen(n) => {
                vector::bend(&original.points, &drag.handles, n, Vec2::ZERO, -delta.y)
            }
            DragMode::Move => (original.points.iter())
                .map(|&[x, y, w]| [x + delta.x, y + delta.y, w])
                .collect(),
        };
        let line = edit.line;
        let Some(old) = self.picked_line().map(|(_, l)| l.bounds()) else {
            return;
        };
        let mut new = None;
        self.edit_vector(idx, |v| {
            if let Some(s) = v.strokes.get_mut(line) {
                s.points = points;
                new = Some(s.bounds());
            }
        });
        if let Some(region) = vector::union([old].into_iter().chain(new)) {
            self.redraw_vector(idx, region);
        }
    }

    /// The Edit Lines drag ended: one undo step.
    pub(crate) fn line_edit_release(&mut self) {
        if let Some(edit) = self.workspace.vector.line_edit.as_mut() {
            edit.drag = None;
        }
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, "Edit line");
        }
    }

    /// Delete with Edit Lines: the handle pressed last goes, the line
    /// running straight past it. Returns whether there was one.
    pub(crate) fn line_edit_delete(&mut self) -> bool {
        let Some((idx, line)) = self.picked_line() else {
            return false;
        };
        let Some(point) = self
            .workspace
            .vector
            .line_edit
            .as_ref()
            .and_then(|e| e.point)
        else {
            return false;
        };
        let handles = self.line_handles(line);
        let Some(h) = handles.iter().position(|&k| k == point) else {
            return false;
        };
        let points = vector::remove_handle(&line.points, &handles, h);
        let old = line.bounds();
        self.release_canvas();
        self.mark_action();
        let layer = &self.canvas.layers[idx];
        self.workspace.vector.session = Some(VectorSession {
            layer: layer.id,
            before: layer.vector.clone().unwrap_or_default(),
            changed: None,
            drawing: None,
            last: Vec2::ZERO,
        });
        let Some(edit) = self.workspace.vector.line_edit.as_mut() else {
            return false;
        };
        edit.point = None;
        let n = edit.line;
        let mut new = None;
        self.edit_vector(idx, |v| {
            // Too few points left to be a line: it goes.
            if points.len() < 2 {
                v.strokes.remove(n);
            } else {
                v.strokes[n].points = points;
                new = Some(v.strokes[n].bounds());
            }
        });
        if new.is_none() {
            self.workspace.vector.line_edit = None;
        }
        if let Some(region) = vector::union([old].into_iter().chain(new)) {
            self.redraw_vector(idx, region);
        }
        if let Some(session) = self.workspace.vector.session.take() {
            self.push_vector_step(session, "Edit line");
        }
        true
    }
}

/// Edit Lines over the canvas: the picked line's path and its handles.
pub(crate) fn draw_line_edit(
    app: &PainterApp,
    painter: &eframe::egui::Painter,
    map: &crate::app::view::render::ScreenMap,
) {
    use eframe::egui::Stroke;
    if !matches!(app.active_tool, crate::app::tools::Tool::VectorEdit) {
        return;
    }
    let Some((_, line)) = app.picked_line() else {
        return;
    };
    let accent = crate::ui::style::ACCENT;
    let path: Vec<_> = (line.smoothed().iter())
        .map(|&[x, y, _]| map.to_screen(Vec2::new(x, y)))
        .collect();
    painter.add(eframe::egui::Shape::line(
        path.clone(),
        Stroke::new(3.0_f32, Color32::from_black_alpha(120)),
    ));
    painter.add(eframe::egui::Shape::line(path, Stroke::new(1.0_f32, accent)));
    let point = app
        .workspace
        .vector
        .line_edit
        .as_ref()
        .and_then(|e| e.point);
    for k in app.line_handles(line) {
        let [x, y, _] = line.points[k];
        let p = map.to_screen(Vec2::new(x, y));
        painter.circle_filled(p, HANDLE_RADIUS + 1.5, Color32::BLACK);
        let fill = if point == Some(k) {
            accent
        } else {
            Color32::WHITE
        };
        painter.circle_filled(p, HANDLE_RADIUS, fill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn app() -> PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 96, Color32::WHITE, 64));
        app.selection_manager.canvas_size = [128, 96];
        app.canvas_mut().active_layer_idx = 1;
        app.add_vector_layer();
        let b = &mut app.brush_state.brush.brush_options;
        b.color = Color32::BLACK;
        b.diameter = 6.0;
        b.pressure_size = false;
        app
    }

    fn line(app: &mut PainterApp, from: Vec2, to: Vec2) {
        app.start_stroke_with_pressure(from, 1.0);
        for i in 1..=20 {
            app.add_stroke_point(from + (to - from) * (i as f32 / 20.0), 1.0);
        }
        app.finish_stroke();
    }

    fn px(app: &PainterApp, x: i32, y: i32) -> Color32 {
        let idx = app.canvas.active_layer_idx;
        let ts = app.canvas.tile_size() as i32;
        app.canvas
            .get_layer_tile_data(idx, x / ts, y / ts)
            .map_or(Color32::TRANSPARENT, |t| {
                t[((y % ts) * ts + x % ts) as usize]
            })
    }

    fn strokes(app: &PainterApp) -> usize {
        app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .map_or(0, |v| v.strokes.len())
    }

    #[test]
    fn the_brush_draws_a_line_that_undoes_and_redoes() {
        let mut app = app();
        let pushes = app.layer_state.history.push_count();
        line(&mut app, Vec2::new(10.0, 20.0), Vec2::new(110.0, 20.0));
        assert_eq!(strokes(&app), 1);
        assert_eq!(px(&app, 70, 20), Color32::BLACK, "across the tile edge too");
        assert_eq!(px(&app, 70, 30), Color32::TRANSPARENT);
        assert_eq!(app.layer_state.history.push_count(), pushes + 1, "one step");
        assert_eq!(
            app.layer_state
                .history
                .labels()
                .0
                .last()
                .map(String::as_str),
            Some("Vector line")
        );
        // Thinned out: a straight line needs few points.
        let points = app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap()
            .strokes[0]
            .points
            .len();
        assert!(points <= 3, "{points} points");
        app.apply_history(false);
        assert_eq!(strokes(&app), 0);
        assert_eq!(px(&app, 70, 20), Color32::TRANSPARENT, "undone");
        app.apply_history(true);
        assert_eq!(strokes(&app), 1);
        assert_eq!(px(&app, 70, 20), Color32::BLACK, "redone");
    }

    #[test]
    fn a_shape_line_over_other_lines_undoes_to_exactly_what_was_there() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 40.0), Vec2::new(120.0, 40.0));
        let idx = app.canvas.active_layer_idx;
        let snapshot = |app: &PainterApp| -> Vec<Color32> {
            (0..96)
                .flat_map(|y| (0..128).map(move |x| (x, y)))
                .map(|(x, y)| px(app, x, y))
                .collect()
        };
        let before = snapshot(&app);
        app.brush_state.brush.brush_options.color = Color32::RED;
        app.add_vector_line(idx, &[Vec2::new(60.0, 10.0), Vec2::new(70.0, 90.0)]);
        let after = snapshot(&app);
        assert_ne!(before, after);
        assert_eq!(strokes(&app), 2);
        app.apply_history(false);
        assert_eq!(snapshot(&app), before, "undone exactly");
        assert_eq!(strokes(&app), 1);
        app.apply_history(true);
        assert_eq!(snapshot(&app), after, "redone exactly");
    }

    /// A mouse drag, frame by frame, through the app's input handling.
    #[test]
    fn a_mouse_drag_draws_one_line() {
        use eframe::egui;
        let mut app = app();
        app.viewport.zoom = 1.0;
        let ctx = egui::Context::default();
        let frame = |app: &mut PainterApp, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response =
                        ui.allocate_response(ui.available_size(), egui::Sense::click_and_drag());
                    let rect = response.rect;
                    crate::app::input::handle_input(
                        app,
                        ctx,
                        &response,
                        rect.min,
                        rect.center(),
                        &[],
                    );
                });
            });
        };
        let button = |pos: egui::Pos2, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let start = egui::pos2(150.0, 150.0);
        frame(&mut app, vec![egui::Event::PointerMoved(start)]);
        frame(&mut app, vec![button(start, true)]);
        for i in 1..=10 {
            let p = start + egui::vec2(i as f32 * 8.0, 0.0);
            frame(&mut app, vec![egui::Event::PointerMoved(p)]);
        }
        frame(&mut app, vec![button(start + egui::vec2(80.0, 0.0), false)]);
        assert_eq!(strokes(&app), 1, "one line, not a dot per move");
        let points = &app.canvas.layers[app.canvas.active_layer_idx]
            .vector
            .as_ref()
            .unwrap()
            .strokes[0]
            .points;
        let length = points.last().unwrap()[0] - points[0][0];
        assert!(length > 60.0, "the whole drag: {length}");
        assert!(!app.brush_state.is_drawing);
    }

    #[test]
    fn the_eraser_takes_out_whole_lines_or_the_part_it_touches() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 20.0), Vec2::new(110.0, 20.0));
        line(&mut app, Vec2::new(10.0, 60.0), Vec2::new(110.0, 60.0));
        app.set_brush_tool(true);
        app.brush_state.brush.brush_options.diameter = 8.0;
        // Whole line: a dab on the first line takes all of it.
        line(&mut app, Vec2::new(60.0, 17.0), Vec2::new(60.0, 23.0));
        assert_eq!(strokes(&app), 1);
        assert_eq!(px(&app, 20, 20), Color32::TRANSPARENT);
        assert_eq!(px(&app, 20, 60), Color32::BLACK, "the other stays");
        assert_eq!(
            app.layer_state
                .history
                .labels()
                .0
                .last()
                .map(String::as_str),
            Some("Vector erase")
        );
        // Touched part: the second line splits in two.
        app.workspace.vector.erase = VectorErase::Touched;
        line(&mut app, Vec2::new(60.0, 55.0), Vec2::new(60.0, 65.0));
        assert_eq!(strokes(&app), 2);
        assert_eq!(px(&app, 60, 60), Color32::TRANSPARENT);
        assert_eq!(px(&app, 20, 60), Color32::BLACK);
        assert_eq!(px(&app, 100, 60), Color32::BLACK);
        // Undo brings both back.
        app.apply_history(false);
        app.apply_history(false);
        assert_eq!(strokes(&app), 2);
        assert_eq!(px(&app, 60, 20), Color32::BLACK);
        assert_eq!(px(&app, 60, 60), Color32::BLACK);
    }

    #[test]
    fn the_eraser_takes_a_line_out_up_to_where_others_cross_it() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 50.0), Vec2::new(118.0, 50.0));
        line(&mut app, Vec2::new(40.0, 10.0), Vec2::new(40.0, 90.0));
        line(&mut app, Vec2::new(80.0, 10.0), Vec2::new(80.0, 90.0));
        let all = |app: &PainterApp| -> Vec<Color32> {
            (0..96)
                .flat_map(|y| (0..128).map(move |x| (x, y)))
                .map(|(x, y)| px(app, x, y))
                .collect()
        };
        let before = all(&app);
        app.set_brush_tool(true);
        app.brush_state.brush.brush_options.diameter = 6.0;
        app.workspace.vector.erase = VectorErase::ToCrossing;
        line(&mut app, Vec2::new(60.0, 48.0), Vec2::new(60.0, 52.0));
        assert_eq!(strokes(&app), 4, "the line in two, and both crossing ones");
        assert_eq!(
            px(&app, 60, 50),
            Color32::TRANSPARENT,
            "between the crossings"
        );
        assert_eq!(px(&app, 50, 50), Color32::TRANSPARENT);
        assert_eq!(
            px(&app, 20, 50),
            Color32::BLACK,
            "before the first crossing"
        );
        assert_eq!(px(&app, 100, 50), Color32::BLACK, "after the second");
        assert_eq!(px(&app, 40, 50), Color32::BLACK, "the crossing lines stay");
        assert_eq!(px(&app, 80, 30), Color32::BLACK);
        // One undo step brings back exactly what was there.
        app.apply_history(false);
        assert_eq!(strokes(&app), 3);
        assert!(all(&app) == before);
    }

    #[test]
    fn edit_lines_bends_widens_and_moves_a_line_as_one_step_each() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 30.0), Vec2::new(110.0, 30.0));
        line(&mut app, Vec2::new(60.0, 70.0), Vec2::new(60.0, 90.0));
        // A straight line keeps just its ends as points: add one to bend at.
        let idx = app.canvas.active_layer_idx;
        app.edit_vector(idx, |v| {
            v.strokes[0].points = (0..=10)
                .map(|i| [10.0 + i as f32 * 10.0, 30.0, 6.0])
                .collect();
        });
        app.redraw_vector(idx, [0, 0, 128, 96]);
        let all = |app: &PainterApp| -> Vec<Color32> {
            (0..96)
                .flat_map(|y| (0..128).map(move |x| (x, y)))
                .map(|(x, y)| px(app, x, y))
                .collect()
        };
        let before = all(&app);
        let pushes = app.layer_state.history.push_count();
        app.active_tool = crate::app::tools::Tool::VectorEdit;
        app.viewport.zoom = 1.0;
        // Picking: a press on the line, nothing else changes.
        app.line_edit_press(Vec2::new(30.0, 31.0), false, false);
        assert_eq!(app.picked_line().map(|(_, l)| l.points.len()), Some(11));
        // Its middle stays a point at this zoom only when it's a handle:
        // bend from the handle nearest the middle.
        let handles = app.line_handles(app.picked_line().unwrap().1);
        let mid = handles[handles.len() / 2];
        let at = app.picked_line().unwrap().1.points[mid];
        let at = Vec2::new(at[0], at[1]);
        app.line_edit_press(at, false, false);
        app.line_edit_drag(at + Vec2::new(0.0, 20.0));
        app.line_edit_release();
        assert_eq!(
            px(&app, at.x as i32, 50),
            Color32::BLACK,
            "bent down to there"
        );
        assert_eq!(px(&app, at.x as i32, 30), Color32::TRANSPARENT);
        assert_eq!(px(&app, 60, 80), Color32::BLACK, "the other line stays");
        assert_eq!(app.layer_state.history.push_count(), pushes + 1);
        // Alt-drag moves it all; undo twice is exactly as it was.
        app.line_edit_press(Vec2::new(12.0, 30.0), false, true);
        app.line_edit_drag(Vec2::new(12.0, 40.0));
        app.line_edit_release();
        assert_eq!(px(&app, 12, 40), Color32::BLACK);
        app.apply_history(false);
        app.apply_history(false);
        assert!(all(&app) == before);
    }

    #[test]
    fn edit_lines_widens_near_the_handle_and_delete_takes_it_out() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 30.0), Vec2::new(110.0, 30.0));
        let idx = app.canvas.active_layer_idx;
        app.edit_vector(idx, |v| {
            v.strokes[0].points = (0..=10)
                .map(|i| [10.0 + i as f32 * 10.0, 30.0 + (i % 2) as f32 * 8.0, 4.0])
                .collect();
        });
        app.redraw_vector(idx, [0, 0, 128, 96]);
        app.active_tool = crate::app::tools::Tool::VectorEdit;
        app.viewport.zoom = 1.0;
        app.line_edit_press(Vec2::new(10.0, 30.0), false, false);
        let first = app.picked_line().unwrap().1.points[0];
        // Shift on the first handle widens there only.
        app.line_edit_press(Vec2::new(first[0], first[1]), true, false);
        app.line_edit_drag(Vec2::new(first[0], first[1] - 10.0));
        app.line_edit_release();
        let widths: Vec<f32> = app
            .picked_line()
            .unwrap()
            .1
            .points
            .iter()
            .map(|p| p[2])
            .collect();
        assert_eq!(widths[0], 14.0);
        assert_eq!(widths[10], 4.0, "far from it, as it was");
        // The handle pressed last goes with Delete.
        let n = widths.len();
        assert!(app.line_edit_delete());
        assert!(app.picked_line().unwrap().1.points.len() < n);
    }

    #[test]
    fn line_width_scales_every_line_as_one_step() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 40.0), Vec2::new(110.0, 40.0));
        assert_eq!(px(&app, 60, 45), Color32::TRANSPARENT);
        let idx = app.canvas.active_layer_idx;
        app.line_width_open(idx);
        app.line_width_set(4.0);
        app.line_width_set(3.0);
        assert_eq!(
            px(&app, 60, 45),
            Color32::BLACK,
            "9 px from the middle of an 18 px line"
        );
        assert_eq!(px(&app, 60, 52), Color32::TRANSPARENT);
        let pushes = app.layer_state.history.push_count();
        app.line_width_done(true);
        assert_eq!(app.layer_state.history.push_count(), pushes + 1);
        app.apply_history(false);
        assert_eq!(px(&app, 60, 45), Color32::TRANSPARENT, "undone");
        assert_eq!(px(&app, 60, 40), Color32::BLACK);
        // Cancelled, nothing changes.
        app.line_width_open(idx);
        app.line_width_set(0.2);
        app.line_width_done(false);
        assert_eq!(px(&app, 60, 42), Color32::BLACK);
    }

    #[test]
    fn a_pixel_tool_makes_it_a_plain_layer_and_undo_brings_the_lines_back() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 40.0), Vec2::new(110.0, 40.0));
        app.filter_open(crate::canvas::filters::Filter::Invert);
        app.filter_commit();
        let idx = app.canvas.active_layer_idx;
        assert!(!app.is_vector_layer(idx));
        assert_eq!(px(&app, 60, 40), Color32::WHITE, "inverted");
        app.apply_history(false);
        assert!(app.is_vector_layer(idx));
        assert_eq!(px(&app, 60, 40), Color32::BLACK);
        // Its lines still erase.
        app.set_brush_tool(true);
        line(&mut app, Vec2::new(60.0, 37.0), Vec2::new(60.0, 43.0));
        assert_eq!(strokes(&app), 0);
    }

    #[test]
    fn lines_and_their_history_are_saved_with_the_project() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 40.0), Vec2::new(110.0, 40.0));
        line(&mut app, Vec2::new(10.0, 50.0), Vec2::new(110.0, 80.0));
        app.apply_history(false);
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let layer = loaded
            .canvas
            .layers
            .iter()
            .find(|l| l.vector.is_some())
            .expect("a vector layer");
        assert_eq!(layer.vector.as_ref().unwrap().strokes.len(), 1);
        let (undo, redo) = loaded.history.stacks();
        assert!(
            !undo.is_empty() && redo.len() == 1,
            "the undone line can come back"
        );
    }

    #[test]
    fn cropping_moves_the_lines_with_the_picture() {
        let mut app = app();
        line(&mut app, Vec2::new(10.0, 40.0), Vec2::new(110.0, 40.0));
        app.apply_image_op(crate::canvas::geometry::ImageOp::Reframe {
            x: 5,
            y: 10,
            w: 100,
            h: 60,
        });
        let idx = app.canvas.active_layer_idx;
        let v = app.canvas.layers[idx].vector.as_ref().expect("still lines");
        assert_eq!(v.strokes[0].points[0][1], 30.0);
        assert_eq!(px(&app, 50, 30), Color32::BLACK);
    }
}
