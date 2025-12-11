use eframe::egui::{self, Color32, Painter, Pos2, Stroke, Shape};
use crate::utils::vector::Vec2;
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
    Lasso { points: Vec<Vec2> },
}

pub struct SelectionManager {
    pub current_shape: Option<SelectionShape>,
    pub is_dragging: bool,
    // For now we just visualize the creation. 
    // In a full implementation we would have a committed mask here.
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
                self.current_shape = Some(SelectionShape::Rectangle { start: pos, end: pos });
            }
            SelectionType::Circle => {
                self.current_shape = Some(SelectionShape::Circle { center: pos, radius: 0.0 });
            }
            SelectionType::Lasso => {
                self.current_shape = Some(SelectionShape::Lasso { points: vec![pos] });
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
                SelectionShape::Lasso { points } => {
                    // Add point if it's far enough from the last one to avoid too many points
                    if let Some(last) = points.last() {
                        if (*last - pos).length() > 2.0 {
                            points.push(pos);
                        }
                    } else {
                         points.push(pos);
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
        if let Some(shape) = &self.current_shape {
            match shape {
                SelectionShape::Rectangle { start, end } => {
                    let x0 = start.x.min(end.x);
                    let x1 = start.x.max(end.x);
                    let y0 = start.y.min(end.y);
                    let y1 = start.y.max(end.y);
                    p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1
                }
                SelectionShape::Circle { center, radius } => {
                    let dx = p.x - center.x;
                    let dy = p.y - center.y;
                    dx * dx + dy * dy <= radius * radius
                }
                SelectionShape::Lasso { points } => {
                    if points.len() < 3 { return false; }
                    let mut inside = false;
                    let mut j = points.len() - 1;
                    for i in 0..points.len() {
                        if (points[i].y > p.y) != (points[j].y > p.y) &&
                            p.x < (points[j].x - points[i].x) * (p.y - points[i].y) / (points[j].y - points[i].y) + points[i].x {
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

    pub fn has_selection(&self) -> bool {
        self.current_shape.is_some()
    }

    pub fn draw_overlay(&self, painter: &Painter, zoom: f32, offset: Pos2, _canvas_height: f32, transform: Option<&crate::selection::transform::TransformInfo>) {
        if let Some(shape) = &self.current_shape {
            let to_screen = |v: Vec2| -> Pos2 {
                let mut p = v;
                if let Some(info) = transform {
                    if let Some(bounds) = info.bounds {
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
                }
                Pos2::new(
                    offset.x + p.x * zoom,
                    offset.y + p.y * zoom,
                )
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
                    painter.add(Shape::line(points.clone(), stroke_black));
                    painter.add(Shape::dashed_line(&points, stroke_white, dash_len, gap_len));
                }
                SelectionShape::Circle { center, radius } => {
                    let center_screen = to_screen(*center);
                    let radius_screen = *radius * zoom;

                    let n = 64;
                    let mut points = Vec::with_capacity(n + 1);
                    for i in 0..=n {
                        let angle = (i as f32 / n as f32) * 2.0 * std::f32::consts::PI;
                        let (sin, cos) = angle.sin_cos();
                        points.push(center_screen + eframe::egui::Vec2::new(cos, sin) * radius_screen);
                    }
                    painter.add(Shape::line(points.clone(), stroke_black));
                    painter.add(Shape::dashed_line(&points, stroke_white, dash_len, gap_len));
                }
                SelectionShape::Lasso { points } => {
                    if points.len() < 2 { return; }
                    let screen_points: Vec<Pos2> = points.iter().map(|p| to_screen(*p)).collect();
                    
                    let mut outline_points = screen_points.clone();
                    if let Some(first) = screen_points.first() {
                         outline_points.push(*first);
                    }
                    painter.add(Shape::line(outline_points.clone(), stroke_black));
                    painter.add(Shape::dashed_line(&outline_points, stroke_white, dash_len, gap_len));
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
                        *shape = SelectionShape::Lasso { points };
                    }
                    SelectionShape::Circle { center: c, radius: r } => {
                        // Approximate circle with polygon
                        let n = 32;
                        let mut points = Vec::with_capacity(n);
                        for i in 0..n {
                            let angle = (i as f32 / n as f32) * 2.0 * std::f32::consts::PI;
                            let (sin, cos) = angle.sin_cos();
                            let p = Vec2::new(c.x + cos * *r, c.y + sin * *r);
                            points.push(transform_point(p));
                        }
                        *shape = SelectionShape::Lasso { points };
                    }
                    SelectionShape::Lasso { points } => {
                        for p in points {
                            *p = transform_point(*p);
                        }
                    }
                }
            } else {
                // Simple translation/uniform scale
                match shape {
                    SelectionShape::Rectangle { start, end } => {
                        *start = transform_point(*start);
                        *end = transform_point(*end);
                    }
                    SelectionShape::Circle { center: c, radius: r } => {
                        *c = transform_point(*c);
                        *r *= scale.x; // Uniform scale assumed
                    }
                    SelectionShape::Lasso { points } => {
                        for p in points {
                            *p = transform_point(*p);
                        }
                    }
                }
            }
        }
    }
}
