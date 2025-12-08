use eframe::egui::{self, Color32, Painter, Pos2, Stroke};
use crate::utils::vector::Vec2;

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
        // Here is where we would usually "commit" the shape to a pixel mask or path.
        // For this step, we just keep the shape to display it.
    }

    pub fn draw_overlay(&self, painter: &Painter, zoom: f32, offset: Pos2, _canvas_height: f32) {
        if let Some(shape) = &self.current_shape {
            let to_screen = |v: Vec2| -> Pos2 {
                Pos2::new(
                    offset.x + v.x * zoom,
                    offset.y + v.y * zoom,
                )
            };

            let stroke = Stroke::new(1.0, Color32::WHITE);
            let fill = Color32::from_rgba_unmultiplied(200, 200, 255, 50);

            match shape {
                SelectionShape::Rectangle { start, end } => {
                    let p1 = to_screen(*start);
                    let p2 = to_screen(*end);
                    let rect = egui::Rect::from_two_pos(p1, p2);
                    painter.rect(rect, 0.0, fill, stroke);
                    
                    // Marching ants effect could be added here with a dashed stroke
                }
                SelectionShape::Circle { center, radius } => {
                    let center_screen = to_screen(*center);
                    let radius_screen = *radius * zoom;
                    painter.circle(center_screen, radius_screen, fill, stroke);
                }
                SelectionShape::Lasso { points } => {
                    if points.len() < 2 { return; }
                    let screen_points: Vec<Pos2> = points.iter().map(|p| to_screen(*p)).collect();
                    
                    // Draw fill (requires tessellation, simpler to just draw closed line for now or use convex polygon if convex)
                    // Since lasso can be concave, fill is tricky without triangulation. 
                    // We'll just draw the line loop and a rough fill if egui supports it easily.
                    // egui::Shape::Path implies filled?
                    
                    painter.add(egui::Shape::convex_polygon(
                        screen_points.clone(),
                        fill,
                        stroke,
                    ));
                     // Note: convex_polygon is incorrect for concave lassos, but simple for now.
                     // Better: line strip.
                     painter.add(egui::Shape::line(screen_points, stroke));
                }
            }
        }
    }
}
