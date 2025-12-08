use crate::brush_engine::brush::{Brush, BrushType};
use crate::brush_engine::stroke::StrokeState;
use crate::brush_engine::brush_options::{BlendMode, PixelBrushShape};
use crate::brush_engine::hardness::{CurvePoint, SoftnessCurve, SoftnessSelector};
use crate::canvas::canvas::Canvas;
use crate::canvas::history::UndoAction;
use crate::utils::vector::Vec2;
use eframe::egui::{self, Color32};
use rayon::ThreadPool;
use std::collections::HashSet;

pub struct BrushPreviewState {
    pub canvas: Canvas,
    pub texture: Option<egui::TextureHandle>,
    pub dirty: bool,
}

impl Default for BrushPreviewState {
    fn default() -> Self {
        Self {
            // Small canvas for preview
            canvas: Canvas::new(200, 80, Color32::TRANSPARENT, 64),
            texture: None,
            dirty: true,
        }
    }
}

/// Panel for tweaking the currently selected brush properties.
pub fn brush_settings_panel(
    ui: &mut egui::Ui,
    brush: &mut Brush,
    preview: &mut BrushPreviewState,
    pool: &ThreadPool,
    loaded_tips: &[(String, PixelBrushShape, Option<egui::TextureHandle>)],
) {
    let mut mask_dirty = false;

    ui.heading("Brush Properties");
    ui.separator();

    // --- Preview Area ---
    ui.collapsing("Preview", |ui| {
        if preview.dirty {
             render_preview(preview, brush, pool, ui.ctx());
             preview.dirty = false;
        }
        
        if let Some(texture) = &preview.texture {
            ui.image((texture.id(), texture.size_vec2()));
        }
    });
    ui.separator();
    // --------------------

    ui.horizontal(|ui| {
        ui.label("Type:");
        if ui.selectable_value(&mut brush.brush_type, BrushType::Soft, "Soft").changed() { preview.dirty = true; }
        if ui.selectable_value(&mut brush.brush_type, BrushType::Pixel, "Pixel").changed() { preview.dirty = true; }
    });

    ui.horizontal(|ui| {
        ui.label("Mode:");
        if ui.selectable_value(&mut brush.brush_options.blend_mode, BlendMode::Normal, "Normal").changed() { preview.dirty = true; }
        if ui.selectable_value(&mut brush.brush_options.blend_mode, BlendMode::Eraser, "Eraser").changed() { preview.dirty = true; }
    });

    ui.add_space(5.0);
    
    ui.label("Brush Tip:");
    egui::ScrollArea::vertical().id_salt("pixel_tip_selector").max_height(120.0).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let size = egui::vec2(32.0, 32.0);
            
            // Circle
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
            let is_selected = matches!(brush.brush_options.pixel_shape, PixelBrushShape::Circle);
            ui.painter().rect_stroke(rect, 1.0, (1.0, if is_selected { Color32::WHITE } else { Color32::GRAY }));
            ui.painter().circle_filled(rect.center(), 12.0, Color32::WHITE);
            if response.on_hover_text("Circle").clicked() {
                brush.brush_options.pixel_shape = PixelBrushShape::Circle;
                preview.dirty = true;
            }

            // Square
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
            let is_selected = matches!(brush.brush_options.pixel_shape, PixelBrushShape::Square);
            ui.painter().rect_stroke(rect, 1.0, (1.0, if is_selected { Color32::WHITE } else { Color32::GRAY }));
            ui.painter().rect_filled(rect.shrink(4.0), 0.0, Color32::WHITE);
            if response.on_hover_text("Square").clicked() {
                brush.brush_options.pixel_shape = PixelBrushShape::Square;
                preview.dirty = true;
            }

            // Custom tips
            for (name, shape, texture_opt) in loaded_tips {
                if let Some(texture) = texture_opt {
                    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                    let is_selected = &brush.brush_options.pixel_shape == shape;
                    
                    ui.painter().rect_stroke(rect, 1.0, (1.0, if is_selected { Color32::WHITE } else { Color32::GRAY }));
                    ui.painter().image(texture.id(), rect.shrink(2.0), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    
                    if response.on_hover_text(name).clicked() {
                        brush.brush_options.pixel_shape = shape.clone();
                        preview.dirty = true;
                    }
                }
            }
        });
    });
    ui.add_space(5.0);

    ui.label("Size:");
    if ui
        .add(egui::Slider::new(&mut brush.brush_options.diameter, 1.0..=3000.0).logarithmic(true))
        .changed()
    {
        mask_dirty = true;
        preview.dirty = true;
    }

    if brush.brush_type == BrushType::Soft {
        ui.horizontal(|ui| {
             ui.label("Softness:");
             if ui.selectable_value(&mut brush.brush_options.softness_selector, SoftnessSelector::Gaussian, "Gaussian").changed() {
                 mask_dirty = true;
                 preview.dirty = true;
             }
             if ui.selectable_value(&mut brush.brush_options.softness_selector, SoftnessSelector::Curve, "Curve").changed() {
                 mask_dirty = true;
                 preview.dirty = true;
             }
        });
        
        match brush.brush_options.softness_selector {
            SoftnessSelector::Gaussian => {
                ui.label("Hardness:");
                if ui
                    .add(egui::Slider::new(&mut brush.brush_options.hardness, 0.0..=100.0))
                    .changed()
                {
                    mask_dirty = true;
                    preview.dirty = true;
                }
            }
            SoftnessSelector::Curve => {
                 ui.label("Softness Curve:");
                 if curve_editor(ui, &mut brush.brush_options.softness_curve) {
                     mask_dirty = true;
                     preview.dirty = true;
                 }
                 ui.small("Double-click to add/remove points.");
            }
        }
    }

    ui.label("Opacity:");
    if ui.add(egui::Slider::new(&mut brush.brush_options.opacity, 0.0..=1.0)).changed() { preview.dirty = true; }

    ui.label("Flow:");
    if ui.add(egui::Slider::new(&mut brush.brush_options.flow, 0.0..=100.0)).changed() { preview.dirty = true; }

    ui.label("Spacing (%):");
    if ui.add(egui::Slider::new(&mut brush.brush_options.spacing, 1.0..=200.0)).changed() { preview.dirty = true; }

    ui.label("Jitter (% of size):");
    if ui.add(egui::Slider::new(&mut brush.jitter, 0.0..=50.0)).changed() { preview.dirty = true; }

    ui.label("Stabilizer:");
    if ui.add(egui::Slider::new(&mut brush.stabilizer, 0.0..=1.0)).changed() { preview.dirty = true; }

    ui.separator();
    if ui.checkbox(&mut brush.pixel_perfect, "Pixel Perfect Mode").changed() { preview.dirty = true; }
    if ui.checkbox(&mut brush.anti_aliasing, "Anti-aliasing").changed() { preview.dirty = true; }

    if mask_dirty {
        brush.is_changed = true;
    }
}

fn render_preview(state: &mut BrushPreviewState, brush: &mut Brush, pool: &ThreadPool, ctx: &egui::Context) {
    // Clear canvas
    state.canvas.clear(Color32::TRANSPARENT);
    
    let width = state.canvas.width() as f32;
    let height = state.canvas.height() as f32;
    
    // Create a temporary stroke state
    let mut stroke = StrokeState::new();
    let mut undo_action = UndoAction { tiles: Vec::new() };
    let mut modified = HashSet::new();
    
    // Draw an S curve with pressure
    // S curve: two cubic beziers or just a sine wave.
    // Let's use a sine wave for simplicity and "S" shape.
    // x from 10% to 90% width
    // y around center with amplitude
    
    let steps = 100;
    let margin = width * 0.1;
    let effective_width = width - 2.0 * margin;
    
    let original_diameter = brush.brush_options.diameter;
    let original_opacity = brush.brush_options.opacity;
    
    for i in 0..=steps {
        let t = i as f32 / steps as f32; // 0..1
        
        // S-curve shape
        // x = linear
        // y = sine
        let x = margin + t * effective_width;
        let phase = t * std::f32::consts::PI * 2.0;
        let y = height * 0.5 + (phase.sin() * height * 0.35);
        
        let pos = Vec2 { x, y };
        
        // Pressure simulation: Taper ends
        // Pressure is 0 at t=0, 1 at t=0.5, 0 at t=1 ?
        // Or maybe start low, high middle, low end.
        // sin(t * PI) -> 0 at 0, 1 at 0.5, 0 at 1.
        let pressure = (t * std::f32::consts::PI).sin();
        
        // Apply pressure to size
        brush.brush_options.diameter = (original_diameter * pressure).max(1.0);
        // Optional: apply to opacity
        // brush.brush_options.opacity = original_opacity * pressure;
        
        stroke.add_point(pool, &state.canvas, brush, None, pos, &mut undo_action, &mut modified);
    }
    
    brush.brush_options.diameter = original_diameter;
    brush.brush_options.opacity = original_opacity;
    
    // Convert canvas to image
    let mut image = egui::ColorImage::new([state.canvas.width(), state.canvas.height()], Color32::TRANSPARENT);
    // We reuse write_region_to_color_image with step=1 for full quality
    state.canvas.write_region_to_color_image(0, 0, state.canvas.width(), state.canvas.height(), &mut image, 1);
    
    // Upload texture
    state.texture = Some(ctx.load_texture("brush_preview", image, egui::TextureOptions::NEAREST));
}


fn curve_editor(ui: &mut egui::Ui, curve: &mut SoftnessCurve) -> bool {
    let mut changed = false;
    let size = egui::Vec2::new(ui.available_width(), 150.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
    let rect = response.rect;

    // Background
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(30));
    
    // Helper to convert normalized coordinates to screen coordinates
    let to_screen = |p: &CurvePoint| -> egui::Pos2 {
        let x = rect.min.x + p.x * rect.width();
        let y = rect.max.y - p.y * rect.height(); // y=1 is top (min y in screen coords), y=0 is bottom
        egui::Pos2::new(x, y)
    };
    
    // Helper to convert screen coordinates to normalized coordinates
    let from_screen = |pos: egui::Pos2| -> CurvePoint {
        let x = (pos.x - rect.min.x) / rect.width();
        let y = (rect.max.y - pos.y) / rect.height();
        CurvePoint {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
        }
    };

    // Draw grid lines
    painter.line_segment([to_screen(&CurvePoint{x:0.0, y:0.0}), to_screen(&CurvePoint{x:1.0, y:0.0})], egui::Stroke::new(1.0, egui::Color32::GRAY));
    painter.line_segment([to_screen(&CurvePoint{x:0.0, y:1.0}), to_screen(&CurvePoint{x:1.0, y:1.0})], egui::Stroke::new(1.0, egui::Color32::GRAY));

    // Draw curve lines
    if curve.points.len() >= 2 {
        let num_segments = 100;
        let mut points = Vec::with_capacity(num_segments + 1);
        for i in 0..=num_segments {
            let t = i as f32 / num_segments as f32;
            let val = curve.eval(t);
            points.push(to_screen(&CurvePoint { x: t, y: val }));
        }
        painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE)));
    }

    // Interaction logic
    let dragged_point_id = ui.make_persistent_id("curve_dragged_point");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(dragged_point_id));

    // Start drag
    if dragging.is_none() && response.drag_started() {
        if let Some(pointer_pos) = response.interact_pointer_pos().or(response.hover_pos()) {
             // Find closest point
             let mut best_dist = f32::MAX;
             let mut best_idx = None;
             
             for (i, p) in curve.points.iter().enumerate() {
                 let screen_pos = to_screen(p);
                 let dist = screen_pos.distance(pointer_pos);
                 if dist < 15.0 && dist < best_dist {
                     best_dist = dist;
                     best_idx = Some(i);
                 }
             }
             
             if let Some(idx) = best_idx {
                 dragging = Some(idx);
                 ui.data_mut(|d| d.insert_temp(dragged_point_id, dragging));
             }
        }
    }

    // Continue drag
    if let Some(idx) = dragging {
        if ui.input(|i| i.pointer.primary_down()) {
             // Update position using global pointer to avoid sticking
             if let Some(pointer_pos) = ui.input(|i| i.pointer.interact_pos()) {
                 let new_p = from_screen(pointer_pos);
                 let len = curve.points.len();
                 
                 // Safety check
                 if idx < len {
                     if idx == 0 {
                         curve.points[idx].y = new_p.y; // Lock x at 0
                     } else if idx == len - 1 {
                         curve.points[idx].y = new_p.y; // Lock x at 1
                     } else {
                         // Constrain x between neighbors
                         let prev_x = curve.points[idx-1].x;
                         let next_x = curve.points[idx+1].x;
                         
                         let p = &mut curve.points[idx];
                         p.x = new_p.x.clamp(prev_x + 0.01, next_x - 0.01);
                         p.y = new_p.y;
                     }
                     changed = true;
                 }
             }
             // Keep dragging state alive
             ui.data_mut(|d| d.insert_temp(dragged_point_id, dragging));
             ui.ctx().request_repaint(); // Smooth updates
        } else {
             // Stop drag
             dragging = None;
             ui.data_mut(|d| d.remove_temp::<Option<usize>>(dragged_point_id));
        }
    }
    
    // Double click to add point
    if response.double_clicked() {
         if let Some(pointer_pos) = response.interact_pointer_pos().or(response.hover_pos()) {
             let new_p = from_screen(pointer_pos);
             // Insert sorted
             let mut insert_idx = 0;
             let mut found = false;
             for (i, p) in curve.points.iter().enumerate() {
                 if new_p.x < p.x {
                     insert_idx = i;
                     found = true;
                     break;
                 }
             }
             
             // Check distance to existing points
             let mut clicked_point_idx = None;
             for (i, p) in curve.points.iter().enumerate() {
                  if to_screen(p).distance(pointer_pos) < 10.0 {
                      clicked_point_idx = Some(i);
                      break;
                  }
             }
             
             if let Some(idx) = clicked_point_idx {
                 // Remove if not start/end
                 if idx > 0 && idx < curve.points.len() - 1 {
                     curve.points.remove(idx);
                     changed = true;
                 }
             } else if found && insert_idx > 0 {
                 curve.points.insert(insert_idx, new_p);
                 changed = true;
             }
         }
    }

    // Draw handles
    for (i, p) in curve.points.iter().enumerate() {
        let center = to_screen(p);
        let is_being_dragged = Some(i) == dragging;
        let radius = if is_being_dragged { 6.0 } else { 4.0 };
        let color = if is_being_dragged { egui::Color32::WHITE } else { egui::Color32::YELLOW };
        painter.circle_filled(center, radius, color);
        painter.circle_stroke(center, radius, egui::Stroke::new(1.0, egui::Color32::BLACK));
    }

    changed
}