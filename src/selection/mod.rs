use eframe::egui::Vec2;
use eframe::egui::{Color32, Painter, Pos2, Shape, Stroke};
use std::sync::Arc;
pub mod magnetic;
pub mod mask;
pub mod transform;

pub use mask::{SelectionMask, SelectionMode};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionType {
    Rectangle,
    Circle,
    Lasso,
    /// Paint the selection with a soft round brush.
    Brush,
    /// Click an area: select it by colour (magic wand).
    Wand,
    /// Click a colour: select it everywhere.
    ColorRange,
    /// Click (or drag) along an edge: the outline snaps to it.
    Magnetic,
}

impl SelectionType {
    /// Picked with a click rather than drawn by dragging.
    pub fn is_click(self) -> bool {
        matches!(self, SelectionType::Wand | SelectionType::ColorRange)
    }
}

#[derive(Clone, Debug)]
pub enum SelectionShape {
    Rectangle {
        start: Vec2,
        end: Vec2,
    },
    Circle {
        center: Vec2,
        radius: f32,
    },
    Lasso {
        points: Vec<Vec2>,
        /// Axis-aligned bounding box of `points`, kept up to date on every mutation
        /// so `contains_coords` can cheaply reject points outside the lasso before
        /// paying for the full ray-cast polygon test (a hot path during brush
        /// stamping, which runs across rayon worker threads).
        bbox_min: Vec2,
        bbox_max: Vec2,
    },
    /// Per-pixel selection: combinations (add/subtract), painted and
    /// transformed selections.
    Mask(Arc<SelectionMask>),
}

pub(crate) fn new_lasso_shape(points: Vec<Vec2>) -> SelectionShape {
    let (bbox_min, bbox_max) = compute_lasso_bbox(&points);
    SelectionShape::Lasso {
        points,
        bbox_min,
        bbox_max,
    }
}

fn compute_lasso_bbox(points: &[Vec2]) -> (Vec2, Vec2) {
    let mut min = Vec2::new(f32::MAX, f32::MAX);
    let mut max = Vec2::new(f32::MIN, f32::MIN);
    for p in points {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }
    (min, max)
}

/// Inside spans `[a, b]` of `shape` along the horizontal line at `y`.
fn shape_row_spans(shape: &SelectionShape, y: f32, spans: &mut Vec<(f32, f32)>) {
    match shape {
        SelectionShape::Rectangle { start, end } => {
            if y >= start.y.min(end.y) && y <= start.y.max(end.y) {
                spans.push((start.x.min(end.x), start.x.max(end.x)));
            }
        }
        SelectionShape::Circle { center, radius } => {
            let dy = y - center.y;
            let h_sq = radius * radius - dy * dy;
            if h_sq >= 0.0 {
                let h = h_sq.sqrt();
                spans.push((center.x - h, center.x + h));
            }
        }
        SelectionShape::Lasso {
            points,
            bbox_min,
            bbox_max,
        } => {
            if points.len() < 3 || y < bbox_min.y || y > bbox_max.y {
                return;
            }
            let mut crossings: Vec<f32> = Vec::new();
            let mut j = points.len() - 1;
            for i in 0..points.len() {
                if (points[i].y > y) != (points[j].y > y) {
                    crossings.push(
                        (points[j].x - points[i].x) * (y - points[i].y)
                            / (points[j].y - points[i].y)
                            + points[i].x,
                    );
                }
                j = i;
            }
            crossings.sort_by(f32::total_cmp);
            // Even-odd rule: inside between each consecutive pair of crossings.
            for pair in crossings.as_chunks::<2>().0 {
                let (a, b) = (pair[0].max(bbox_min.x), pair[1].min(bbox_max.x));
                if a <= b {
                    spans.push((a, b));
                }
            }
        }
        // Coverage comes straight from the mask (see `shape_row_coverage`).
        SelectionShape::Mask(_) => {}
    }
}

/// Anti-aliased coverage (0..=1) of pixels `x0..x0 + out.len()` in row `y`
/// for `shape`: 8×8 samples per pixel for vector shapes, stored coverage
/// for masks.
pub(crate) fn shape_row_coverage(shape: &SelectionShape, y: usize, x0: usize, out: &mut [f32]) {
    if let SelectionShape::Mask(mask) = shape {
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = mask.value((x0 + i) as i32, y as i32) as f32 / 255.0;
        }
        return;
    }
    const S: usize = 8;
    let mut counts = vec![0u32; out.len()];
    let mut spans = Vec::new();
    for j in 0..S {
        let sy = y as f32 + (j as f32 + 0.5) / S as f32;
        spans.clear();
        shape_row_spans(shape, sy, &mut spans);
        for &(a, b) in &spans {
            add_span_samples(a, b, x0, S, &mut counts);
        }
    }
    let inv = 1.0 / (S * S) as f32;
    for (slot, count) in out.iter_mut().zip(counts) {
        *slot = count as f32 * inv;
    }
}

/// A shape's area as a mask, over its bounds clipped to the canvas.
fn rasterize_shape(shape: &SelectionShape, canvas_size: [usize; 2]) -> Option<SelectionMask> {
    if let SelectionShape::Mask(mask) = shape {
        return Some((**mask).clone());
    }
    let b = shape_bounds(shape)?;
    let (w, h) = (canvas_size[0] as i32, canvas_size[1] as i32);
    let bounds = [
        (b.min.x.floor() as i32 - 1).clamp(0, w),
        (b.min.y.floor() as i32 - 1).clamp(0, h),
        (b.max.x.ceil() as i32 + 1).clamp(0, w),
        (b.max.y.ceil() as i32 + 1).clamp(0, h),
    ];
    Some(SelectionMask::rasterize(bounds, |y, x0, out| {
        shape_row_coverage(shape, y, x0, out)
    }))
}

/// Canvas-space bounding box of a shape.
fn shape_bounds(shape: &SelectionShape) -> Option<eframe::egui::Rect> {
    use eframe::egui::{Rect, pos2, vec2};
    match shape {
        SelectionShape::Rectangle { start, end } => Some(Rect::from_two_pos(
            pos2(start.x, start.y),
            pos2(end.x, end.y),
        )),
        SelectionShape::Circle { center, radius } => Some(Rect::from_center_size(
            pos2(center.x, center.y),
            vec2(*radius * 2.0, *radius * 2.0),
        )),
        SelectionShape::Lasso {
            points,
            bbox_min,
            bbox_max,
        } => (!points.is_empty()).then(|| {
            Rect::from_min_max(pos2(bbox_min.x, bbox_min.y), pos2(bbox_max.x, bbox_max.y))
        }),
        SelectionShape::Mask(mask) => Some(Rect::from_min_size(
            pos2(mask.x0 as f32, mask.y0 as f32),
            vec2(mask.w as f32, mask.h as f32),
        )),
    }
}

/// Area of a shape, to tell a click (deselect) from a drag.
fn shape_is_tiny(shape: &SelectionShape) -> bool {
    shape_bounds(shape).is_none_or(|b| b.width() < 2.0 || b.height() < 2.0)
}

/// Count the samples of span `[a, b]` falling in each pixel of `counts`
/// (pixel `x0 + i`). Samples sit at `(k + 0.5) / s` for integer `k`.
fn add_span_samples(a: f32, b: f32, x0: usize, s: usize, counts: &mut [u32]) {
    let s_f = s as f32;
    let first = (x0 * s) as i64;
    let last = ((x0 + counts.len()) * s) as i64 - 1;
    let k_min = ((a * s_f - 0.5).ceil() as i64).max(first);
    let k_max = ((b * s_f - 0.5).floor() as i64).min(last);
    if k_min > k_max {
        return;
    }
    let s = s as i64;
    let mut k = k_min;
    while k <= k_max {
        let pixel = k / s;
        let pixel_end = (pixel + 1) * s - 1;
        let upto = pixel_end.min(k_max);
        counts[(pixel - x0 as i64) as usize] += (upto - k + 1) as u32;
        k = upto + 1;
    }
}

pub struct SelectionManager {
    pub current_shape: Option<SelectionShape>,
    pub is_dragging: bool,
    /// How new selections combine with the current one (Shift/Alt
    /// override it per drag).
    pub mode: SelectionMode,
    /// Canvas size, to clip rasterized selections.
    pub canvas_size: [usize; 2],
    /// Selection brush radius and hardness (0..=1).
    pub brush_radius: f32,
    pub brush_hardness: f32,
    /// During an add/subtract drag: the selection being combined into,
    /// and the mode for this drag.
    drag_base: Option<SelectionShape>,
    drag_mode: SelectionMode,
    /// During a selection-brush drag: the full-canvas mask being painted,
    /// the last dab position, and the path (drawn while painting).
    brush_mask: Option<SelectionMask>,
    brush_last: Option<Vec2>,
    pub brush_path: Vec<Vec2>,
    /// Smoothing for the freehand lasso and the selection brush (0 = off,
    /// 1 = strongest).
    pub smoothing: f32,
    stabilizer: crate::brush_engine::stabilizer::Stabilizer,
    /// The smoothed and the raw position of the last input.
    smooth_last: Option<Vec2>,
    raw_last: Option<Vec2>,
}

impl Default for SelectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionManager {
    pub fn new() -> Self {
        Self {
            current_shape: None,
            is_dragging: false,
            mode: SelectionMode::Replace,
            canvas_size: [usize::MAX / 4, usize::MAX / 4],
            brush_radius: 24.0,
            brush_hardness: 0.8,
            drag_base: None,
            drag_mode: SelectionMode::Replace,
            brush_mask: None,
            brush_last: None,
            brush_path: Vec::new(),
            smoothing: 0.0,
            stabilizer: Default::default(),
            smooth_last: None,
            raw_last: None,
        }
    }

    /// `raw` input smoothed along the drag.
    fn smooth(&mut self, raw: Vec2) -> Vec2 {
        let settings = crate::brush_engine::stabilizer::StabilizerSettings::simple(self.smoothing);
        let pos = self.stabilizer.step(&settings, self.smooth_last, raw);
        self.smooth_last = Some(pos);
        self.raw_last = Some(raw);
        pos
    }

    /// A manager holding `shape`, e.g. a copy of the selection for a stroke.
    pub fn with_shape(shape: Option<SelectionShape>) -> Self {
        Self {
            current_shape: shape,
            ..Self::new()
        }
    }

    /// Start a selection with the manager's own mode.
    pub fn start_selection(&mut self, pos: Vec2, sel_type: SelectionType) {
        self.start_selection_with_mode(pos, sel_type, self.mode);
    }

    /// Start a selection of `sel_type` at `pos`, combining with the current
    /// selection according to `mode`.
    pub fn start_selection_with_mode(
        &mut self,
        pos: Vec2,
        sel_type: SelectionType,
        mode: SelectionMode,
    ) {
        self.is_dragging = true;
        self.drag_mode = mode;
        self.stabilizer = Default::default();
        self.smooth_last = Some(pos);
        self.raw_last = Some(pos);
        self.drag_base = match mode {
            SelectionMode::Replace => None,
            _ => self.current_shape.take(),
        };
        match sel_type {
            SelectionType::Brush => {
                let [w, h] = self.canvas_size;
                let mut mask = SelectionMask::empty(0, 0, w.min(1 << 15), h.min(1 << 15));
                // Add/subtract paint onto the current selection; intersect
                // paints a new one and keeps the overlap at the end.
                if mode != SelectionMode::Intersect
                    && let Some(base) = &self.drag_base
                    && let Some(base) = rasterize_shape(base, self.canvas_size)
                {
                    mask = mask.combine(&base, SelectionMode::Add);
                }
                let subtract = mode == SelectionMode::Subtract;
                mask.stamp(pos, self.brush_radius, self.brush_hardness, !subtract);
                self.brush_mask = Some(mask);
                self.brush_last = Some(pos);
                self.brush_path = vec![pos];
                self.current_shape = None;
            }
            SelectionType::Rectangle => {
                self.current_shape = Some(SelectionShape::Rectangle {
                    start: pos,
                    end: pos,
                });
            }
            SelectionType::Circle => {
                self.current_shape = Some(SelectionShape::Circle {
                    center: pos,
                    radius: 0.0,
                });
            }
            SelectionType::Lasso => {
                self.current_shape = Some(new_lasso_shape(vec![pos]));
            }
            // Handled by the app, not dragged here.
            SelectionType::Wand | SelectionType::ColorRange | SelectionType::Magnetic => {
                self.is_dragging = false;
                self.current_shape = self.drag_base.take();
            }
        }
    }

    pub fn update_selection(&mut self, pos: Vec2) {
        if !self.is_dragging {
            return;
        }
        let freehand = self.brush_mask.is_some()
            || matches!(self.current_shape, Some(SelectionShape::Lasso { .. }));
        let pos = if freehand { self.smooth(pos) } else { pos };
        self.push_selection_point(pos);
    }

    fn push_selection_point(&mut self, pos: Vec2) {
        if let (Some(mask), Some(last)) = (&mut self.brush_mask, self.brush_last) {
            // Dabs along the segment, a quarter radius apart.
            let add = self.drag_mode != SelectionMode::Subtract;
            let step = (self.brush_radius * 0.25).max(0.5);
            let dist = (pos - last).length();
            let n = (dist / step).ceil().max(1.0) as usize;
            for i in 1..=n {
                let p = last + (pos - last) * (i as f32 / n as f32);
                mask.stamp(p, self.brush_radius, self.brush_hardness, add);
            }
            self.brush_last = Some(pos);
            self.brush_path.push(pos);
            return;
        }
        if let Some(shape) = &mut self.current_shape {
            match shape {
                SelectionShape::Rectangle { start: _, end } => {
                    *end = pos;
                }
                SelectionShape::Circle { center, radius } => {
                    *radius = (*center - pos).length();
                }
                SelectionShape::Lasso {
                    points,
                    bbox_min,
                    bbox_max,
                } => {
                    // Add point if it's far enough from the last one to avoid too many points
                    let should_push = points
                        .last()
                        .is_none_or(|last| (*last - pos).length_sq() > 4.0); // 2.0^2 = 4.0
                    if should_push {
                        points.push(pos);
                        if points.len() == 1 {
                            *bbox_min = pos;
                            *bbox_max = pos;
                        } else {
                            bbox_min.x = bbox_min.x.min(pos.x);
                            bbox_min.y = bbox_min.y.min(pos.y);
                            bbox_max.x = bbox_max.x.max(pos.x);
                            bbox_max.y = bbox_max.y.max(pos.y);
                        }
                    }
                }
                SelectionShape::Mask(_) => {}
            }
        }
    }

    pub fn end_selection(&mut self) {
        // A smoothed path lags the input: finish where the input stopped.
        if self.smoothing > 0.0
            && let (Some(raw), Some(last)) = (self.raw_last, self.smooth_last)
            && raw != last
        {
            self.push_selection_point(raw);
        }
        self.smooth_last = None;
        self.raw_last = None;
        self.is_dragging = false;
        let base = self.drag_base.take();
        if let Some(mask) = self.brush_mask.take() {
            self.brush_last = None;
            self.brush_path.clear();
            let mask = match (
                self.drag_mode,
                base.and_then(|b| rasterize_shape(&b, self.canvas_size)),
            ) {
                (SelectionMode::Intersect, Some(base)) => {
                    base.combine(&mask, SelectionMode::Intersect)
                }
                (SelectionMode::Intersect, None) => return,
                _ => mask,
            };
            self.current_shape = mask.cropped().map(|m| SelectionShape::Mask(Arc::new(m)));
            return;
        }
        let Some(shape) = self.current_shape.take() else {
            self.current_shape = base;
            return;
        };
        // A click without a drag: deselect (replace) or keep (add/subtract).
        if shape_is_tiny(&shape) {
            self.current_shape = base;
            return;
        }
        self.current_shape = match base {
            None => Some(shape),
            Some(base) => {
                let combined = match (
                    rasterize_shape(&base, self.canvas_size),
                    rasterize_shape(&shape, self.canvas_size),
                ) {
                    (Some(a), Some(b)) => a.combine(&b, self.drag_mode).cropped(),
                    (None, Some(b)) if self.drag_mode == SelectionMode::Add => b.cropped(),
                    (Some(a), _) => a.cropped(),
                    _ => None,
                };
                combined.map(|m| SelectionShape::Mask(Arc::new(m)))
            }
        };
    }

    /// Combine `mask` (a new selection, e.g. from the magic wand) with the
    /// current one according to `mode`.
    pub fn apply_mask(&mut self, mask: SelectionMask, mode: SelectionMode) {
        let current = self
            .current_shape
            .take()
            .filter(|_| mode != SelectionMode::Replace)
            .and_then(|s| rasterize_shape(&s, self.canvas_size));
        let combined = match (current, mode) {
            (_, SelectionMode::Replace) | (None, SelectionMode::Add) => mask.cropped(),
            (None, _) => None,
            (Some(current), mode) => current.combine(&mask, mode).cropped(),
        };
        self.current_shape = combined.map(|m| SelectionShape::Mask(Arc::new(m)));
    }

    /// Combine `shape` with the current selection according to `mode`.
    pub fn apply_shape(&mut self, shape: SelectionShape, mode: SelectionMode) {
        if mode == SelectionMode::Replace {
            self.current_shape = Some(shape);
        } else if let Some(mask) = rasterize_shape(&shape, self.canvas_size) {
            self.apply_mask(mask, mode);
        }
    }

    /// Like [`Self::apply_mask`] when the new selection may be empty.
    pub fn apply_mask_or_nothing(&mut self, mask: Option<SelectionMask>, mode: SelectionMode) {
        match (mask, mode) {
            (Some(mask), mode) => self.apply_mask(mask, mode),
            (None, SelectionMode::Replace | SelectionMode::Intersect) => self.current_shape = None,
            (None, SelectionMode::Add | SelectionMode::Subtract) => {}
        }
    }

    pub fn clear_selection(&mut self) {
        self.current_shape = None;
        self.is_dragging = false;
        self.drag_base = None;
        self.brush_mask = None;
        self.brush_last = None;
        self.brush_path.clear();
    }

    /// Select the whole canvas.
    pub fn select_all(&mut self) {
        let [w, h] = self.canvas_size;
        self.clear_selection();
        self.current_shape = Some(SelectionShape::Rectangle {
            start: Vec2::ZERO,
            end: Vec2::new(w as f32, h as f32),
        });
    }

    /// Swap selected and unselected pixels (within the canvas).
    pub fn invert(&mut self) {
        let [w, h] = self.canvas_size;
        let all = SelectionMask::new(0, 0, w, h, vec![255; w * h]);
        self.current_shape = match self
            .current_shape
            .take()
            .and_then(|s| rasterize_shape(&s, self.canvas_size))
        {
            None => Some(SelectionShape::Rectangle {
                start: Vec2::ZERO,
                end: Vec2::new(w as f32, h as f32),
            }),
            Some(current) => all
                .combine(&current, SelectionMode::Subtract)
                .cropped()
                .map(|m| SelectionShape::Mask(Arc::new(m))),
        };
    }

    /// The selection being combined into during an add/subtract drag.
    pub fn drag_base(&self) -> Option<&SelectionShape> {
        self.drag_base.as_ref()
    }

    pub fn contains(&self, p: Vec2) -> bool {
        self.contains_coords(p.x, p.y)
    }

    /// Check if raw coordinates are in selection (avoids Vec2 allocation)
    #[inline]
    pub fn contains_coords(&self, x: f32, y: f32) -> bool {
        if let Some(shape) = &self.current_shape {
            match shape {
                SelectionShape::Rectangle { start, end } => {
                    let x0 = start.x.min(end.x);
                    let x1 = start.x.max(end.x);
                    let y0 = start.y.min(end.y);
                    let y1 = start.y.max(end.y);
                    x >= x0 && x <= x1 && y >= y0 && y <= y1
                }
                SelectionShape::Circle { center, radius } => {
                    let dx = x - center.x;
                    let dy = y - center.y;
                    dx * dx + dy * dy <= radius * radius
                }
                SelectionShape::Lasso {
                    points,
                    bbox_min,
                    bbox_max,
                } => {
                    if points.len() < 3 {
                        return false;
                    }
                    if x < bbox_min.x || x > bbox_max.x || y < bbox_min.y || y > bbox_max.y {
                        return false;
                    }
                    let mut inside = false;
                    let mut j = points.len() - 1;
                    for i in 0..points.len() {
                        if (points[i].y > y) != (points[j].y > y)
                            && x < (points[j].x - points[i].x) * (y - points[i].y)
                                / (points[j].y - points[i].y)
                                + points[i].x
                        {
                            inside = !inside;
                        }
                        j = i;
                    }
                    inside
                }
                SelectionShape::Mask(mask) => mask.contains(x, y),
            }
        } else {
            true
        }
    }

    /// Selection coverage for pixel centers `(x + 0.5, y + 0.5)` with
    /// `x` in `x0..x0 + out.len()`: identical to calling `contains_coords`
    /// per pixel, but a lasso's edge crossings are computed once per row
    /// instead of once per pixel.
    pub fn row_mask(&self, y: usize, x0: usize, out: &mut [bool]) {
        let py = y as f32 + 0.5;
        let Some(SelectionShape::Lasso {
            points,
            bbox_min,
            bbox_max,
        }) = &self.current_shape
        else {
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = self.contains_coords((x0 + i) as f32 + 0.5, py);
            }
            return;
        };
        if points.len() < 3 || py < bbox_min.y || py > bbox_max.y {
            out.fill(false);
            return;
        }
        // Same per-edge crossing expression as `contains_coords`, so the
        // `x < crossing` comparisons below see bit-identical values.
        let mut crossings: Vec<f32> = Vec::new();
        let mut j = points.len() - 1;
        for i in 0..points.len() {
            if (points[i].y > py) != (points[j].y > py) {
                crossings.push(
                    (points[j].x - points[i].x) * (py - points[i].y) / (points[j].y - points[i].y)
                        + points[i].x,
                );
            }
            j = i;
        }
        crossings.sort_by(f32::total_cmp);
        // Inside iff an odd number of crossings lie strictly right of x.
        let mut passed = 0;
        for (i, slot) in out.iter_mut().enumerate() {
            let px = (x0 + i) as f32 + 0.5;
            while passed < crossings.len() && crossings[passed] <= px {
                passed += 1;
            }
            *slot = px >= bbox_min.x && px <= bbox_max.x && (crossings.len() - passed) % 2 == 1;
        }
    }

    /// Anti-aliased selection coverage (0..=1) of pixels `x0..x0 + out.len()`
    /// in row `y`, from an 8×8 grid of samples per pixel (65 levels). Each
    /// sub-row's inside spans are computed exactly, so cost is per span, not
    /// per sample.
    pub fn row_coverage(&self, y: usize, x0: usize, out: &mut [f32]) {
        match &self.current_shape {
            Some(shape) => shape_row_coverage(shape, y, x0, out),
            None => out.fill(1.0),
        }
    }

    pub fn has_selection(&self) -> bool {
        self.current_shape.is_some()
    }

    /// Get the bounding rectangle of the current selection in canvas coordinates.
    pub fn get_bounds(&self) -> Option<eframe::egui::Rect> {
        self.current_shape.as_ref().and_then(shape_bounds)
    }

    /// Draw the selection outline. `to_screen` maps canvas points to the
    /// screen (zoom, pan and canvas rotation); `zoom` sets curve detail.
    pub fn draw_overlay(&self, painter: &Painter, zoom: f32, to_screen: &dyn Fn(Vec2) -> Pos2) {
        // While add/subtract-dragging, the selection being combined into.
        if let Some(base) = &self.drag_base {
            draw_shape_outline(painter, base, to_screen, zoom);
        }
        if self.brush_mask.is_some() {
            // The painted path, until the mask is traced on release.
            let color = if self.drag_mode == SelectionMode::Subtract {
                Color32::from_rgba_unmultiplied(255, 80, 80, 70)
            } else {
                Color32::from_rgba_unmultiplied(80, 160, 255, 70)
            };
            for p in &self.brush_path {
                painter.circle_filled(to_screen(*p), self.brush_radius * zoom, color);
            }
            return;
        }
        if let Some(shape) = &self.current_shape {
            draw_shape_outline(painter, shape, to_screen, zoom);
        }
    }

    pub fn apply_transform(&mut self, offset: Vec2, rotation: f32, scale: Vec2, center: Vec2) {
        if let Some(shape) = &mut self.current_shape {
            let (sin_r, cos_r) = rotation.sin_cos();

            let transform_point = |p: Vec2| -> Vec2 {
                let dx = p.x - center.x;
                let dy = p.y - center.y;

                let sx = dx * scale.x;
                let sy = dy * scale.y;

                let rx = sx * cos_r - sy * sin_r;
                let ry = sx * sin_r + sy * cos_r;

                Vec2::new(rx + center.x + offset.x, ry + center.y + offset.y)
            };

            // Convert to Lasso if rotation or non-uniform scale
            let needs_conversion = rotation != 0.0 || (scale.x - scale.y).abs() > 0.001;

            if needs_conversion {
                match shape {
                    SelectionShape::Rectangle { start, end } => {
                        let p0 = *start;
                        let p1 = Vec2::new(end.x, start.y);
                        let p2 = *end;
                        let p3 = Vec2::new(start.x, end.y);

                        let points = vec![
                            transform_point(p0),
                            transform_point(p1),
                            transform_point(p2),
                            transform_point(p3),
                        ];
                        *shape = new_lasso_shape(points);
                    }
                    SelectionShape::Circle {
                        center: c,
                        radius: r,
                    } => {
                        // Approximate circle with polygon
                        let n = 32;
                        let mut points = Vec::with_capacity(n);
                        for i in 0..n {
                            let angle = (i as f32 / n as f32) * 2.0 * std::f32::consts::PI;
                            let (sin, cos) = angle.sin_cos();
                            let p = Vec2::new(c.x + cos * *r, c.y + sin * *r);
                            points.push(transform_point(p));
                        }
                        *shape = new_lasso_shape(points);
                    }
                    SelectionShape::Lasso {
                        points,
                        bbox_min,
                        bbox_max,
                    } => {
                        for p in points.iter_mut() {
                            *p = transform_point(*p);
                        }
                        (*bbox_min, *bbox_max) = compute_lasso_bbox(points);
                    }
                    SelectionShape::Mask(mask) => {
                        *mask = Arc::new(transform_mask(mask, offset, rotation, scale, center));
                    }
                }
            } else {
                // Simple translation/uniform scale
                match shape {
                    SelectionShape::Rectangle { start, end } => {
                        *start = transform_point(*start);
                        *end = transform_point(*end);
                    }
                    SelectionShape::Circle {
                        center: c,
                        radius: r,
                    } => {
                        *c = transform_point(*c);
                        *r *= scale.x; // Uniform scale assumed
                    }
                    SelectionShape::Lasso {
                        points,
                        bbox_min,
                        bbox_max,
                    } => {
                        for p in points.iter_mut() {
                            *p = transform_point(*p);
                        }
                        (*bbox_min, *bbox_max) = compute_lasso_bbox(points);
                    }
                    SelectionShape::Mask(mask) => {
                        *mask = Arc::new(transform_mask(mask, offset, rotation, scale, center));
                    }
                }
            }
        }
    }
}

/// A mask moved/rotated/scaled like the pixels under it (nearest-neighbour).
fn transform_mask(
    mask: &SelectionMask,
    offset: Vec2,
    rotation: f32,
    scale: Vec2,
    center: Vec2,
) -> SelectionMask {
    let (sin_r, cos_r) = rotation.sin_cos();
    let forward = |p: Vec2| {
        let (dx, dy) = ((p.x - center.x) * scale.x, (p.y - center.y) * scale.y);
        Vec2::new(
            dx * cos_r - dy * sin_r + center.x + offset.x,
            dx * sin_r + dy * cos_r + center.y + offset.y,
        )
    };
    let (x0, y0) = (mask.x0 as f32, mask.y0 as f32);
    let (x1, y1) = (x0 + mask.w as f32, y0 + mask.h as f32);
    let corners = [
        Vec2::new(x0, y0),
        Vec2::new(x1, y0),
        Vec2::new(x1, y1),
        Vec2::new(x0, y1),
    ]
    .map(forward);
    let min = corners.iter().fold(Vec2::splat(f32::MAX), |a, c| {
        Vec2::new(a.x.min(c.x), a.y.min(c.y))
    });
    let max = corners.iter().fold(Vec2::splat(f32::MIN), |a, c| {
        Vec2::new(a.x.max(c.x), a.y.max(c.y))
    });
    let bounds = [
        min.x.floor() as i32,
        min.y.floor() as i32,
        max.x.ceil() as i32,
        max.y.ceil() as i32,
    ];
    let (sx, sy) = (
        if scale.x.abs() < 1e-6 { 1e-6 } else { scale.x },
        if scale.y.abs() < 1e-6 { 1e-6 } else { scale.y },
    );
    mask.resample(bounds, |p| {
        let (dx, dy) = (p.x - center.x - offset.x, p.y - center.y - offset.y);
        let (rx, ry) = (dx * cos_r + dy * sin_r, -dx * sin_r + dy * cos_r);
        Vec2::new(rx / sx + center.x, ry / sy + center.y)
    })
}

/// Marching-ants outline: a solid black line under white dashes, so it
/// shows on any colour.
fn draw_path(painter: &Painter, points: &[Pos2]) {
    if points.len() < 2 {
        return;
    }
    painter.add(Shape::line(
        points.to_vec(),
        Stroke::new(1.0_f32, Color32::BLACK),
    ));
    painter.extend(Shape::dashed_line(
        points,
        Stroke::new(1.0_f32, Color32::WHITE),
        4.0,
        4.0,
    ));
}

fn draw_shape_outline(
    painter: &Painter,
    shape: &SelectionShape,
    to_screen: &dyn Fn(Vec2) -> Pos2,
    zoom: f32,
) {
    match shape {
        SelectionShape::Rectangle { start, end } => {
            let corners = [
                *start,
                Vec2::new(end.x, start.y),
                *end,
                Vec2::new(start.x, end.y),
                *start,
            ];
            draw_path(painter, &corners.map(to_screen));
        }
        SelectionShape::Circle { center, radius } => {
            let n = ((radius * zoom * 0.5) as usize).clamp(24, 256);
            let points: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    to_screen(*center + Vec2::new(a.cos(), a.sin()) * *radius)
                })
                .collect();
            draw_path(painter, &points);
        }
        SelectionShape::Lasso { points, .. } => {
            let mut pts: Vec<Pos2> = points.iter().map(|p| to_screen(*p)).collect();
            if let Some(&first) = pts.first() {
                pts.push(first);
            }
            draw_path(painter, &pts);
        }
        SelectionShape::Mask(mask) => {
            for outline in mask.outline() {
                let mut pts: Vec<Pos2> = outline.iter().map(|p| to_screen(*p)).collect();
                if let Some(&first) = pts.first() {
                    pts.push(first);
                }
                draw_path(painter, &pts);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(
        sel: &mut SelectionManager,
        t: SelectionType,
        mode: SelectionMode,
        a: (f32, f32),
        b: (f32, f32),
    ) {
        sel.start_selection_with_mode(Vec2::new(a.0, a.1), t, mode);
        sel.update_selection(Vec2::new(b.0, b.1));
        sel.end_selection();
    }

    #[test]
    fn shift_adds_and_alt_subtracts_selections() {
        let mut sel = SelectionManager::new();
        sel.canvas_size = [200, 200];
        drag(
            &mut sel,
            SelectionType::Rectangle,
            SelectionMode::Replace,
            (10.0, 10.0),
            (50.0, 50.0),
        );
        drag(
            &mut sel,
            SelectionType::Rectangle,
            SelectionMode::Add,
            (100.0, 100.0),
            (140.0, 140.0),
        );
        assert!(matches!(sel.current_shape, Some(SelectionShape::Mask(_))));
        assert!(sel.contains_coords(20.5, 20.5) && sel.contains_coords(120.5, 120.5));
        assert!(!sel.contains_coords(75.5, 75.5));
        drag(
            &mut sel,
            SelectionType::Circle,
            SelectionMode::Subtract,
            (30.0, 30.0),
            (38.0, 30.0),
        );
        assert!(!sel.contains_coords(30.5, 30.5), "hole punched");
        assert!(sel.contains_coords(12.5, 12.5));
    }

    #[test]
    fn a_click_deselects_but_keeps_the_selection_when_adding() {
        let mut sel = SelectionManager::new();
        drag(
            &mut sel,
            SelectionType::Rectangle,
            SelectionMode::Replace,
            (10.0, 10.0),
            (50.0, 50.0),
        );
        drag(
            &mut sel,
            SelectionType::Rectangle,
            SelectionMode::Add,
            (70.0, 70.0),
            (70.5, 70.5),
        );
        assert!(sel.has_selection(), "a click in add mode changes nothing");
        drag(
            &mut sel,
            SelectionType::Rectangle,
            SelectionMode::Replace,
            (70.0, 70.0),
            (70.5, 70.5),
        );
        assert!(!sel.has_selection(), "a plain click deselects");
    }

    #[test]
    fn the_selection_brush_paints_and_erases() {
        let mut sel = SelectionManager::new();
        sel.canvas_size = [120, 120];
        sel.brush_radius = 6.0;
        drag(
            &mut sel,
            SelectionType::Brush,
            SelectionMode::Replace,
            (20.0, 60.0),
            (100.0, 60.0),
        );
        assert!(sel.contains_coords(60.5, 60.5) && !sel.contains_coords(60.5, 80.5));
        drag(
            &mut sel,
            SelectionType::Brush,
            SelectionMode::Subtract,
            (60.0, 50.0),
            (60.0, 70.0),
        );
        assert!(!sel.contains_coords(60.5, 60.5) && sel.contains_coords(30.5, 60.5));
    }

    #[test]
    fn rotating_a_rectangle_keeps_it_rotated() {
        let mut sel = SelectionManager::with_shape(Some(SelectionShape::Rectangle {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(100.0, 10.0),
        }));
        sel.apply_transform(
            Vec2::ZERO,
            std::f32::consts::FRAC_PI_2,
            Vec2::new(1.0, 1.0),
            Vec2::new(50.0, 5.0),
        );
        // A quarter turn about the centre: now tall and thin, not a bigger box.
        assert!(sel.contains_coords(50.0, 40.0));
        assert!(!sel.contains_coords(20.0, 5.0));
    }

    #[test]
    fn rectangle_selection_contains_edges() {
        let mut selection = SelectionManager::new();
        selection.start_selection(Vec2::new(10.0, 20.0), SelectionType::Rectangle);
        selection.update_selection(Vec2::new(30.0, 40.0));

        assert!(selection.contains_coords(10.0, 20.0));
        assert!(selection.contains_coords(30.0, 40.0));
        assert!(!selection.contains_coords(30.1, 40.0));
    }

    #[test]
    fn row_mask_matches_contains_coords() {
        // A star-shaped lasso (concave, many crossings per row) plus the
        // other shapes and "no selection".
        let star: Vec<Vec2> = (0..23)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 23.0;
                let r = if i % 2 == 0 { 40.0 } else { 13.7 };
                Vec2::new(50.3 + a.cos() * r, 48.9 + a.sin() * r)
            })
            .collect();
        let shapes = [
            None,
            Some(new_lasso_shape(star)),
            Some(SelectionShape::Rectangle {
                start: Vec2::new(10.2, 30.5),
                end: Vec2::new(70.0, 12.0),
            }),
            Some(SelectionShape::Circle {
                center: Vec2::new(40.0, 40.0),
                radius: 22.4,
            }),
        ];
        for shape in shapes {
            let selection = SelectionManager::with_shape(shape);
            let mut row = [false; 100];
            for y in 0..100 {
                selection.row_mask(y, 3, &mut row);
                for (i, &inside) in row.iter().enumerate() {
                    let expected = selection.contains_coords((3 + i) as f32 + 0.5, y as f32 + 0.5);
                    assert_eq!(inside, expected, "x={} y={y}", 3 + i);
                }
            }
        }
    }

    fn coverage_grid(selection: &SelectionManager, size: usize) -> Vec<f32> {
        let mut grid = vec![0.0; size * size];
        for y in 0..size {
            selection.row_coverage(y, 0, &mut grid[y * size..(y + 1) * size]);
        }
        grid
    }

    #[test]
    fn row_coverage_is_antialiased_and_area_correct() {
        let pixel_aligned = SelectionManager::with_shape(Some(SelectionShape::Rectangle {
            start: Vec2::new(2.0, 3.0),
            end: Vec2::new(7.0, 9.0),
        }));
        let grid = coverage_grid(&pixel_aligned, 12);
        for y in 0..12 {
            for x in 0..12 {
                let inside = (2..7).contains(&x) && (3..9).contains(&y);
                assert_eq!(
                    grid[y * 12 + x],
                    if inside { 1.0 } else { 0.0 },
                    "x={x} y={y}"
                );
            }
        }

        let half_pixel_edge = SelectionManager::with_shape(Some(SelectionShape::Rectangle {
            start: Vec2::new(2.5, 0.0),
            end: Vec2::new(8.0, 12.0),
        }));
        assert_eq!(coverage_grid(&half_pixel_edge, 12)[5 * 12 + 2], 0.5);

        let polygon_area = 0.5 * 40.0 * 13.0 * 13.0 * (std::f32::consts::TAU / 40.0).sin();
        for (shape, expected) in [
            (
                SelectionShape::Circle {
                    center: Vec2::new(20.3, 19.7),
                    radius: 13.2,
                },
                std::f32::consts::PI * 13.2 * 13.2,
            ),
            (
                new_lasso_shape(
                    (0..40)
                        .map(|i| {
                            let a = i as f32 * std::f32::consts::TAU / 40.0;
                            Vec2::new(20.0 + a.cos() * 13.0, 20.0 + a.sin() * 13.0)
                        })
                        .collect(),
                ),
                polygon_area,
            ),
        ] {
            let selection = SelectionManager::with_shape(Some(shape));
            let grid = coverage_grid(&selection, 40);
            let area: f32 = grid.iter().sum();
            assert!(
                (area - expected).abs() / expected < 0.005,
                "area {area} vs {expected}"
            );
            assert!(
                grid.iter().any(|&c| c > 0.0 && c < 1.0),
                "edges are anti-aliased"
            );
            assert_eq!(grid[20 * 40 + 20], 1.0);
            assert_eq!(grid[0], 0.0);
        }
    }

    #[test]
    fn intersect_keeps_only_the_overlap() {
        let a = SelectionMask::new(0, 0, 4, 1, vec![255, 255, 255, 0]);
        let b = SelectionMask::new(2, 0, 4, 1, vec![128, 255, 255, 255]);
        let both = a.combine(&b, SelectionMode::Intersect);
        assert_eq!((both.x0, both.w), (2, 2));
        assert_eq!(both.data, vec![128, 0]);
        let apart = SelectionMask::new(10, 0, 2, 1, vec![255, 255]);
        assert!(
            a.combine(&apart, SelectionMode::Intersect)
                .cropped()
                .is_none()
        );
    }

    #[test]
    fn apply_mask_combines_with_the_current_selection() {
        let mut m = SelectionManager::new();
        m.canvas_size = [16, 16];
        let left = SelectionMask::new(0, 0, 8, 16, vec![255; 8 * 16]);
        let top = SelectionMask::new(0, 0, 16, 8, vec![255; 16 * 8]);
        m.apply_mask(top.clone(), SelectionMode::Subtract);
        assert!(
            !m.has_selection(),
            "subtracting from nothing selects nothing"
        );
        m.apply_mask(left.clone(), SelectionMode::Replace);
        m.apply_mask(top.clone(), SelectionMode::Intersect);
        assert!(m.contains(Vec2::new(2.0, 2.0)));
        assert!(!m.contains(Vec2::new(2.0, 12.0)));
        assert!(!m.contains(Vec2::new(12.0, 2.0)));
        m.apply_mask(left, SelectionMode::Add);
        assert!(m.contains(Vec2::new(2.0, 12.0)));
        m.apply_mask(top, SelectionMode::Subtract);
        assert!(!m.contains(Vec2::new(2.0, 2.0)));
        assert!(m.contains(Vec2::new(2.0, 12.0)));
    }
}
