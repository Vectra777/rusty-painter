use eframe::egui::Vec2;
use eframe::egui::{self, Color32, Painter, Pos2, Shape, Stroke};
pub mod transform;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionType {
    Rectangle,
    Circle,
    Lasso,
}

#[derive(Clone, Debug)]
pub enum SelectionShape {
    Rectangle { start: Vec2, end: Vec2 },
    Circle { center: Vec2, radius: f32 },
    Lasso {
        points: Vec<Vec2>,
        /// Axis-aligned bounding box of `points`, kept up to date on every mutation
        /// so `contains_coords` can cheaply reject points outside the lasso before
        /// paying for the full ray-cast polygon test (a hot path during brush
        /// stamping, which runs across rayon worker threads).
        bbox_min: Vec2,
        bbox_max: Vec2,
    },
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
                        (points[j].x - points[i].x) * (y - points[i].y) / (points[j].y - points[i].y)
                            + points[i].x,
                    );
                }
                j = i;
            }
            crossings.sort_by(f32::total_cmp);
            // Even-odd rule: inside between each consecutive pair of crossings.
            for pair in crossings.chunks_exact(2) {
                let (a, b) = (pair[0].max(bbox_min.x), pair[1].min(bbox_max.x));
                if a <= b {
                    spans.push((a, b));
                }
            }
        }
    }
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
    // For now we just visualize the creation.
    // In a full implementation we would have a committed mask here.
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
        }
    }

    pub fn start_selection(&mut self, pos: Vec2, sel_type: SelectionType) {
        self.is_dragging = true;
        match sel_type {
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
        }
    }

    pub fn update_selection(&mut self, pos: Vec2) {
        if !self.is_dragging {
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
            }
        }
    }

    pub fn end_selection(&mut self) {
        self.is_dragging = false;
    }

    pub fn clear_selection(&mut self) {
        self.current_shape = None;
        self.is_dragging = false;
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
        const S: usize = 8;
        let Some(shape) = &self.current_shape else {
            out.fill(1.0);
            return;
        };
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

    pub fn has_selection(&self) -> bool {
        self.current_shape.is_some()
    }

    /// Get the bounding rectangle of the current selection in canvas coordinates.
    pub fn get_bounds(&self) -> Option<eframe::egui::Rect> {
        if let Some(shape) = &self.current_shape {
            match shape {
                SelectionShape::Rectangle { start, end } => {
                    let min_x = start.x.min(end.x);
                    let max_x = start.x.max(end.x);
                    let min_y = start.y.min(end.y);
                    let max_y = start.y.max(end.y);
                    Some(eframe::egui::Rect::from_min_max(
                        eframe::egui::pos2(min_x, min_y),
                        eframe::egui::pos2(max_x, max_y),
                    ))
                }
                SelectionShape::Circle { center, radius } => {
                    Some(eframe::egui::Rect::from_center_size(
                        eframe::egui::pos2(center.x, center.y),
                        eframe::egui::vec2(*radius * 2.0, *radius * 2.0),
                    ))
                }
                SelectionShape::Lasso {
                    points,
                    bbox_min,
                    bbox_max,
                } => {
                    if points.is_empty() {
                        return None;
                    }
                    Some(eframe::egui::Rect::from_min_max(
                        eframe::egui::pos2(bbox_min.x, bbox_min.y),
                        eframe::egui::pos2(bbox_max.x, bbox_max.y),
                    ))
                }
            }
        } else {
            None
        }
    }

    pub fn draw_overlay(
        &self,
        painter: &Painter,
        zoom: f32,
        offset: Pos2,
        _canvas_height: f32,
        transform: Option<&crate::selection::transform::TransformInfo>,
    ) {
        if let Some(shape) = &self.current_shape {
            let to_screen = |v: Vec2| -> Pos2 {
                let mut p = v;
                if let Some(info) = transform
                    && let Some(bounds) = info.bounds
                {
                    let center = Vec2::new(bounds.center().x, bounds.center().y);
                    let (sin_r, cos_r) = info.rotation.sin_cos();

                    let dx = p.x - center.x;
                    let dy = p.y - center.y;

                    let sx = dx * info.scale.x;
                    let sy = dy * info.scale.y;

                    let rx = sx * cos_r - sy * sin_r;
                    let ry = sx * sin_r + sy * cos_r;

                    p.x = rx + center.x + info.offset.x;
                    p.y = ry + center.y + info.offset.y;
                }
                Pos2::new(offset.x + p.x * zoom, offset.y + p.y * zoom)
            };

            let stroke_white = Stroke::new(1.0, Color32::WHITE);
            let stroke_black = Stroke::new(1.0, Color32::BLACK);
            let dash_len = 5.0;
            let gap_len = 5.0;

            match shape {
                SelectionShape::Rectangle { start, end } => {
                    let p1 = to_screen(*start);
                    let p2 = to_screen(*end);
                    let rect = egui::Rect::from_two_pos(p1, p2);

                    let points = vec![
                        rect.min,
                        Pos2::new(rect.max.x, rect.min.y),
                        rect.max,
                        Pos2::new(rect.min.x, rect.max.y),
                        rect.min,
                    ];
                    painter.add(Shape::dashed_line(&points, stroke_white, dash_len, gap_len));
                    painter.add(Shape::line(points, stroke_black));
                }
                SelectionShape::Circle { center, radius } => {
                    let center_screen = to_screen(*center);
                    let radius_screen = *radius * zoom;

                    let n = 64;
                    let mut points = Vec::with_capacity(n + 1);
                    for i in 0..=n {
                        let angle = (i as f32 / n as f32) * 2.0 * std::f32::consts::PI;
                        let (sin, cos) = angle.sin_cos();
                        points.push(
                            center_screen + eframe::egui::Vec2::new(cos, sin) * radius_screen,
                        );
                    }
                    painter.add(Shape::dashed_line(&points, stroke_white, dash_len, gap_len));
                    painter.add(Shape::line(points, stroke_black));
                }
                SelectionShape::Lasso { points, .. } => {
                    if points.len() < 2 {
                        return;
                    }
                    let mut outline_points: Vec<Pos2> =
                        points.iter().map(|p| to_screen(*p)).collect();
                    if let Some(&first) = outline_points.first() {
                        outline_points.push(first);
                    }
                    painter.add(Shape::dashed_line(
                        &outline_points,
                        stroke_white,
                        dash_len,
                        gap_len,
                    ));
                    painter.add(Shape::line(outline_points, stroke_black));
                }
            }
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
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let selection = SelectionManager {
                current_shape: shape,
                is_dragging: false,
            };
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
        let pixel_aligned = SelectionManager {
            current_shape: Some(SelectionShape::Rectangle {
                start: Vec2::new(2.0, 3.0),
                end: Vec2::new(7.0, 9.0),
            }),
            is_dragging: false,
        };
        let grid = coverage_grid(&pixel_aligned, 12);
        for y in 0..12 {
            for x in 0..12 {
                let inside = (2..7).contains(&x) && (3..9).contains(&y);
                assert_eq!(grid[y * 12 + x], if inside { 1.0 } else { 0.0 }, "x={x} y={y}");
            }
        }

        let half_pixel_edge = SelectionManager {
            current_shape: Some(SelectionShape::Rectangle {
                start: Vec2::new(2.5, 0.0),
                end: Vec2::new(8.0, 12.0),
            }),
            is_dragging: false,
        };
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
            let selection = SelectionManager {
                current_shape: Some(shape),
                is_dragging: false,
            };
            let grid = coverage_grid(&selection, 40);
            let area: f32 = grid.iter().sum();
            assert!((area - expected).abs() / expected < 0.005, "area {area} vs {expected}");
            assert!(grid.iter().any(|&c| c > 0.0 && c < 1.0), "edges are anti-aliased");
            assert_eq!(grid[20 * 40 + 20], 1.0);
            assert_eq!(grid[0], 0.0);
        }
    }
}
